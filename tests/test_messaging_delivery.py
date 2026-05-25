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

class MessagingDeliveryTests(unittest.TestCase):
    def test_message_store_creates_blackbox_runtime_tables(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-schema-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            conn = store.connect()
            try:
                tables = {
                    row[0]
                    for row in conn.execute("select name from sqlite_master where type='table'").fetchall()
                }
            finally:
                conn.close()
            self.assertIn("scheduled_events", tables)
            self.assertIn("delivery_tokens", tables)
            self.assertIn("agent_health", tables)
            self.assertIn("peer_allowlist", tables)
            self.assertIn("result_watchers", tables)

    def test_message_store_claim_for_delivery_is_single_consumer(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            message_id = store.create_message(None, "leader", "fake_impl", "hello")
            self.assertTrue(store.claim_for_delivery(message_id))
            self.assertFalse(store.claim_for_delivery(message_id))
            row = next(row for row in store.messages() if row["message_id"] == message_id)
            self.assertEqual(row["status"], "target_resolved")

    def test_send_default_timeout_reports_submitted_unverified(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-visible-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
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
                    "tasks": spec["tasks"],
                },
            )

            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "no visible token here")
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["message_status"], "submitted")
            self.assertTrue(sent["visible"])
            self.assertTrue(sent["submitted"])
            self.assertEqual(sent["paste_attempts"][0]["buffer_name"], f"team-agent-send-{sent['message_id']}")
            self.assertTrue(sent["paste_attempts"][0]["buffer_deleted"])
            self.assertEqual(MessageStore(workspace).delivery_tokens()[0]["message_id"], sent["message_id"])

    def test_worker_delivery_retries_paste_until_message_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-paste-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    if len(paste_calls) == 1:
                        proc.stdout = "prompt without delivered message"
                    elif send_calls:
                        proc.stdout = "codex>"
                    else:
                        proc.stdout = "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["message_status"], "submitted")
            self.assertEqual(len(paste_calls), 2)
            self.assertEqual(len(send_calls), 1)
            self.assertEqual(sent["paste_attempts"][0]["verification"], "capture_missing_token")
            self.assertEqual(sent["paste_attempts"][1]["verification"], "capture_contains_new_pasted_content_prompt")

    def test_worker_delivery_submits_visible_message_fragment_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-fragment-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            content = "这是一段足够长的评审正文片段，用来模拟长文本已经进入输入框但 token 不在可见区。"
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = content
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", content, timeout=30)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["message_status"], "submitted")
            self.assertEqual(len(paste_calls), 1)
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            self.assertEqual(sent["verification"], "capture_contains_message_fragment")

    def test_send_no_wait_keeps_injected_semantics(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-no-wait-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "still hidden")
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", wait_visible=False)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["message_status"], "submitted")

    def test_send_watch_result_registers_result_watcher(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )

            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "still hidden")
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", wait_visible=False, watch_result=True)
            self.assertTrue(sent["ok"])
            self.assertTrue(sent["watch_result"])
            self.assertEqual(sent["watch"]["task_id"], "task_impl")
            self.assertIn("notify the leader", sent["watch"]["notice"])
            watchers = MessageStore(workspace).result_watchers()
            self.assertEqual(len(watchers), 1)
            self.assertEqual(watchers[0]["status"], "pending")
            self.assertEqual(watchers[0]["task_id"], "task_impl")
            self.assertEqual(watchers[0]["agent_id"], "fake_impl")
            self.assertEqual(watchers[0]["message_id"], sent["message_id"])

    def test_send_watch_result_registers_after_submitted_unverified(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-submitted-unverified-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )

            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "streaming output without token")
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01, watch_result=True)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["message_status"], "submitted")
            self.assertTrue(sent["submitted"])
            self.assertTrue(sent["watch_result"])
            watchers = MessageStore(workspace).result_watchers()
            self.assertEqual(len(watchers), 1)
            self.assertEqual(watchers[0]["message_id"], sent["message_id"])
            self.assertTrue(any(e["event"] == "send.submitted" for e in _events(workspace)))

    def test_broadcast_sends_only_to_current_team_and_excludes_sender(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-broadcast-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            qa = copy.deepcopy(spec["agents"][0])
            qa["id"] = "fake_qa"
            spec["agents"].extend([peer, qa])
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_peer": {"status": "running", "provider": "fake", "window": "fake_peer"},
                        "fake_qa": {"status": "running", "provider": "fake", "window": "fake_qa"},
                        "stale_outside_team": {"status": "running", "provider": "fake", "window": "stale_outside_team"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            worker_targets: list[str] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\nfake_peer\nfake_qa\nstale_outside_team\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    worker_targets.append(args[3])
                    self.assertNotIn("fake_impl", args[3])
                    self.assertNotIn("stale_outside_team", args[3])
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = (
                        "fake>"
                        if worker_targets and len(send_calls) >= len(worker_targets)
                        else ("› [Pasted Content 1093 chars]" if worker_targets else "prompt")
                    )
                return proc

            def fake_leader(*args, **kwargs):
                return {"ok": True, "message_id": "msg_leader", "status": "submitted", "to": "leader", "channel": "direct_tmux"}

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader),
            ):
                result = runtime.send_message(workspace, "*", "broadcast hello", sender="fake_impl", wait_visible=False)

            self.assertTrue(result["ok"])
            self.assertEqual(result["status"], "broadcast_delivered")
            self.assertEqual(result["targets"], ["leader", "fake_peer", "fake_qa"])
            self.assertEqual(result["delivered_count"], 3)
            self.assertEqual(result["failed_count"], 0)
            self.assertEqual(worker_targets, ["session:fake_peer", "session:fake_qa"])
            events = _events(workspace)
            complete = next(e for e in events if e["event"] == "send.broadcast_complete")
            self.assertEqual(complete["targets"], ["leader", "fake_peer", "fake_qa"])
            rejected = runtime.send_message(workspace, "stale_outside_team", "nope", sender="fake_impl", wait_visible=False)
            self.assertFalse(rejected["ok"])
            self.assertEqual(rejected["status"], "refused")
            self.assertEqual(rejected["reason"], "target_not_in_team")

    def test_send_lock_reports_busy_instead_of_parallel_write(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-lock-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            lock_path = workspace / ".team" / "runtime" / "send.lock"
            lock_path.parent.mkdir(parents=True, exist_ok=True)
            with lock_path.open("w", encoding="utf-8") as lock_file:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    runtime.send_message(workspace, None, "blocked by lock", task_id="task_impl", lock_timeout=0.01)
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            self.assertIn("locked by another team-agent process", str(ctx.exception))
            self.assertTrue(any(e["event"] == "runtime.lock_busy" for e in _events(workspace)))

    def test_human_confirmation_blocks_send_until_confirmed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-human-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["human_confirmation"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})

            blocked = runtime.send_message(workspace, None, "needs approval", task_id="task_impl")
            self.assertFalse(blocked["ok"])
            self.assertEqual(blocked["reason"], "human_confirmation_required")
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "blocked")
            self.assertTrue(any(e["event"] == "send.human_confirmation_required" for e in _events(workspace)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
