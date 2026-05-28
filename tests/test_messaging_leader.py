from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})

class MessagingLeaderTests(unittest.TestCase):
    def test_launch_auto_attaches_codex_leader_receiver_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-attach-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["leader"]["provider"] = "codex"
            spec["runtime"]["session_name"] = "team-agent-launch-attach-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")

            def fake_attach(workspace_arg, state, pane, provider, event_log, source, require_current=False):
                self.assertEqual(workspace_arg, workspace)
                self.assertIsNone(pane)
                self.assertEqual(provider, "codex")
                self.assertEqual(source, "launch")
                self.assertFalse(require_current)
                receiver = {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "provider": "codex",
                    "pane_id": "%9",
                    "session_name": "leader-session",
                }
                state["leader_receiver"] = receiver
                event_log.write("leader_receiver.attached", target="%9", provider="codex", source="launch")
                return receiver, {"ok": True}

            with (
                patch("team_agent.runtime._attach_leader_to_state", side_effect=fake_attach) as attached,
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="", stderr="")),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)

            self.assertTrue(launched["ok"])
            self.assertEqual(attached.call_count, 1)
            self.assertEqual(launched["leader_receiver"]["pane_id"], "%9")
            self.assertEqual(load_runtime_state(workspace)["leader_receiver"]["pane_id"], "%9")
            self.assertTrue(any(e["event"] == "leader_receiver.attached" and e["source"] == "launch" for e in _events(workspace)))

    def test_aaa_busy_agent_pending_message_delivers_after_idle_detection(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-busy-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-busy-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            try:
                self.assertTrue(launched["ok"])
                import time

                time.sleep(0.2)
                state = load_runtime_state(workspace)
                state["agents"]["fake_impl"]["status"] = "busy"
                save_runtime_state(workspace, state)

                sent = runtime.send_message(workspace, None, "queued while busy", task_id="task_impl", requires_ack=True)
                self.assertTrue(sent["ok"])
                self.assertEqual(sent["status"], "delivered")
                counts = MessageStore(workspace).message_counts()
                self.assertEqual(counts.get("accepted"), None)
                self.assertEqual(counts.get("submitted", 0) + counts.get("acknowledged", 0), 1)

                pumped = runtime.collect(workspace)
                self.assertNotIn(sent["message_id"], pumped["delivered_messages"])

                collected = {"collected": []}
                for _ in range(30):
                    time.sleep(0.5)
                    collected = runtime.collect(workspace)
                    if collected["collected"]:
                        break
                status = runtime.status(workspace, as_json=True)
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_impl")
                else:
                    self.assertIn(status["tasks"][0]["status"], {"done", "pending"})
                    if status["tasks"][0]["status"] == "done":
                        self.assertTrue(status["tasks"][0].get("accepted_result_id"))
                events = _events(workspace)
                self.assertTrue(any(e["event"] == "runtime.status_detected" and e["status"] == "running" for e in events))
                self.assertTrue(any(e["event"] == "send.deliver_attempt" and e.get("recipient_busy") for e in events))
                self.assertFalse(any(e["event"] == "send.queued_busy" for e in events))
            finally:
                runtime.shutdown(workspace)

    def test_attach_leader_registers_existing_tmux_pane(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-attach-leader-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            session, pane_id, _ = _start_fake_leader_pane(workspace)
            try:
                attached = runtime.attach_leader(workspace, pane=pane_id, provider="fake")
                self.assertTrue(attached["ok"])
                receiver = load_runtime_state(workspace)["leader_receiver"]
                self.assertEqual(receiver["mode"], "direct_tmux")
                self.assertEqual(receiver["pane_id"], pane_id)
                self.assertEqual(receiver["provider"], "fake")
                self.assertTrue(any(e["event"] == "leader_receiver.attached" for e in _events(workspace)))
            finally:
                runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

    def test_attach_leader_without_pane_infers_active_tmux_pane(self) -> None:
        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.returncode = 1
                proc.stderr = "no current client"
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "%9\tsession\t1\twin\t0\t/dev/ttys001\tnode\t1\n"
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            pane, discovery = runtime._resolve_leader_pane(None, "codex")
        self.assertEqual(discovery, "active_pane_scan")
        self.assertEqual(pane["pane_id"], "%9")
        self.assertEqual(pane["pane_current_command"], "node")

    def test_resolve_leader_scans_workspace_when_tool_shell_has_wrong_tmux_client(self) -> None:
        workspace = Path("/tmp/team-agent-workspace-scan")

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.stdout = "%1\tother\t1\twin\t0\t/dev/ttys001\tyazi\t1\t/tmp\t1\n"
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "\n".join(
                    [
                        "%1\tother\t1\twin\t0\t/dev/ttys001\tyazi\t1\t/tmp\t1",
                        "%2\tcodexwork\t1\twin\t1\t/dev/ttys002\tnode\t1\t/tmp/team-agent-workspace-scan\t1",
                        "%3\tcodexwork\t1\twin\t2\t/dev/ttys003\tnode\t0\t/tmp/team-agent-workspace-scan\t1",
                    ]
                )
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            pane, discovery = runtime._resolve_leader_pane(None, "codex", workspace=workspace, require_current=True)

        self.assertEqual(discovery, "workspace_pane_scan")
        self.assertEqual(pane["pane_id"], "%2")
        self.assertEqual(pane["pane_current_path"], str(workspace))

    def test_resolve_leader_reports_ambiguous_workspace_panes(self) -> None:
        workspace = Path("/tmp/team-agent-ambiguous")

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.returncode = 1
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "\n".join(
                    [
                        "%2\tcodexwork-a\t1\twin\t1\t/dev/ttys002\tnode\t1\t/tmp/team-agent-ambiguous\t1",
                        "%3\tcodexwork-b\t1\twin\t1\t/dev/ttys003\tnode\t1\t/tmp/team-agent-ambiguous\t1",
                    ]
                )
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            with self.assertRaises(TeamAgentRuntimeError) as ctx:
                runtime._resolve_leader_pane(None, "codex", workspace=workspace, require_current=True)

        self.assertIn("multiple tmux leader panes match this workspace", str(ctx.exception))
        self.assertIn("%2", str(ctx.exception))
        self.assertIn("%3", str(ctx.exception))

    def test_launch_requires_current_tmux_leader_for_real_workers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-required-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["leader"]["provider"] = "codex"
            spec["agents"][0]["provider"] = "codex"
            spec["runtime"]["session_name"] = "team-agent-leader-required-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime._tmux_current_client_pane_info", return_value=None),
                patch("team_agent.runtime._tmux_list_panes", return_value=[]),
                self.assertRaises(TeamAgentRuntimeError) as ctx,
            ):
                runtime.launch(spec_path, auto_approve=True)
            self.assertIn("could not locate a tmux-managed leader pane", str(ctx.exception))

    def test_worker_to_leader_direct_injection_uses_standard_payload_and_does_not_route_task(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-direct-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": [task],
                },
            )
            session, pane_id, capture_path = _start_fake_leader_pane(workspace)
            runtime.attach_leader(workspace, pane=pane_id, provider="fake")
            unique = "direct leader payload " + workspace.name[-6:]
            try:
                result = runtime.send_message(
                    workspace,
                    "leader",
                    unique,
                    task_id="task_impl",
                    sender="fake_impl",
                    requires_ack=True,
                )
                self.assertTrue(result["ok"])
                self.assertEqual(result["channel"], "direct_tmux")
                self.assertEqual(result["submit_key"], "Enter")
                _wait_for_file_line(capture_path, unique)
                text = capture_path.read_text(encoding="utf-8")
                self.assertIn("Team Agent message from fake_impl for task_impl:", text)
                self.assertIn(f"[team-agent-token:{result['message_id']}]", text)
                self.assertNotIn("TEAM_AGENT_MESSAGE", text)
                events = _events(workspace)
                attempt = next(e for e in events if e["event"] == "leader_receiver.deliver_attempt")
                payload = attempt["payload"]
                self.assertEqual(payload["content"], unique)
                self.assertEqual(payload["from"], "fake_impl")
                self.assertEqual(payload["to"], "leader")
                self.assertEqual(payload["task_id"], "task_impl")
                self.assertFalse(payload["requires_ack"])
                self.assertTrue(payload["message_id"].startswith("msg_"))
                self.assertEqual(result["status"], "submitted")
                self.assertTrue(result["visible"])
                self.assertTrue(result["submitted"])
                state = load_runtime_state(workspace)
                self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
                self.assertEqual(state["tasks"][0]["status"], "pending")
                self.assertFalse((workspace / ".team" / "runtime" / "leader-inbox.log").exists())
                delivered = next(e for e in events if e["event"] == "leader_receiver.submitted")
                self.assertEqual(delivered["target"], pane_id)
                self.assertEqual(delivered["submit_key"], "Enter")
                self.assertTrue(delivered["submitted"])
                self.assertEqual(delivered["submit_verification"], "Enter_sent_after_visible_token")
            finally:
                runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

    def test_worker_to_leader_missing_pane_falls_back_and_diagnose_explains(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-missing-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": [task],
                },
            )
            session, pane_id, _ = _start_fake_leader_pane(workspace)
            runtime.attach_leader(workspace, pane=pane_id, provider="fake")
            runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

            result = runtime.send_message(
                workspace,
                "leader",
                "this should use fallback audit",
                task_id="task_impl",
                sender="fake_impl",
            )
            self.assertTrue(result["ok"])
            self.assertEqual(result["status"], "fallback_log")
            self.assertEqual(result["message_status"], "failed")
            self.assertEqual(result["reason"], "leader_pane_missing")
            state = load_runtime_state(workspace)
            self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
            self.assertEqual(state["tasks"][0]["status"], "pending")
            fallback = workspace / ".team" / "runtime" / "leader-inbox.log"
            self.assertIn("Team Agent message from fake_impl for task_impl:", fallback.read_text(encoding="utf-8"))
            diagnosed = runtime.diagnose(workspace)
            self.assertIn("leader_pane_missing", {issue["kind"] for issue in diagnosed["issues"]})

    def test_stale_leader_target_rediscovery_unique_and_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rediscover-") as tmp:
            workspace = Path(tmp)
            event_log = runtime.EventLog(workspace)
            receiver = {"provider": "codex", "pane_id": "%old"}
            one = {
                "ok": True,
                "targets": [
                    {
                        "pane_id": "%1",
                        "session_name": "s",
                        "window_index": "1",
                        "window_name": "w",
                        "pane_index": "0",
                        "pane_tty": "/dev/ttys001",
                        "pane_current_command": "node",
                        "pane_active": True,
                        "fingerprint": "s|1|0|/dev/ttys001",
                    }
                ],
            }
            with patch("team_agent.runtime.core_list_targets", return_value=one):
                result = runtime._rediscover_leader_receiver(receiver, event_log)
            self.assertEqual(result["status"], "updated")
            self.assertEqual(result["receiver"]["pane_id"], "%1")
            many = {"ok": True, "targets": [one["targets"][0], {**one["targets"][0], "pane_id": "%2"}]}
            with patch("team_agent.runtime.core_list_targets", return_value=many):
                result = runtime._rediscover_leader_receiver(receiver, event_log)
            self.assertEqual(result["status"], "ambiguous")


if __name__ == "__main__":
    unittest.main(verbosity=2)
