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

class RuntimeTests01(unittest.TestCase):
    def test_fake_e2e(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-test-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-test-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            try:
                self.assertTrue(launched["ok"])
                import time

                sent = runtime.send_message(workspace, None, "do fake work", task_id="task_impl", requires_ack=True)
                self.assertTrue(sent["ok"])
                collected = {"collected": []}
                for _ in range(10):
                    time.sleep(0.5)
                    collected = runtime.collect(workspace)
                    if collected["collected"]:
                        break
                status = runtime.status(workspace, as_json=True)
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_impl")
                else:
                    self.assertEqual(status["tasks"][0]["status"], "done")
                    self.assertTrue(status["tasks"][0].get("accepted_result_id"))
                self.assertEqual(status["messages"].get("acknowledged"), 1)
                state_text = (workspace / "team_state.md").read_text(encoding="utf-8")
                self.assertIn("Fake worker handled", state_text)
                events = _events(workspace)
                routing_events = [e for e in events if e["event"] == "routing.decision"]
                self.assertTrue(any(e["source"] == "launch" and e["reason"] == "matched routing rule implementation-to-fake" for e in routing_events))
                self.assertTrue(any(e["source"] == "send" and e["selected_agent"] == "fake_impl" for e in routing_events))
            finally:
                runtime.shutdown(workspace)

    def test_launch_fast_runtime_sends_codex_fast_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-fast-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec["agents"][0]["model"] = "gpt-5.4-mini"
            spec["leader"]["provider"] = "fake"
            spec["runtime"]["fast"] = True
            spec["runtime"]["session_name"] = "team-agent-launch-fast-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.is_installed.return_value = True
            adapter.mcp_config.return_value = {}
            adapter.install_mcp.return_value = workspace / ".team/runtime/mcp/codebase.json"
            adapter.handle_startup_prompts.return_value = []

            sent_fast = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "send-keys", "-t"] and "/fast" in args:
                    sent_fast.append(args)
                return proc

            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shell_command_for_agent", return_value="true"),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)
            self.assertTrue(launched["ok"])
            self.assertEqual(sent_fast[0][-2:], ["/fast", "Enter"])

    def test_launch_opens_displays_after_all_worker_windows_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-display-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            spec["leader"]["provider"] = "fake"
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            spec["agents"].append(peer)
            spec_path.write_text(dumps(spec), encoding="utf-8")
            started_windows: set[str] = set()
            display_snapshots: list[set[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "new-session", "-d"]:
                    started_windows.add(args[6])
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                return proc

            def fake_open_display(workspace_arg, session_name, window_name, agent, event_log):
                display_snapshots.append(set(started_windows))
                return {"status": "opened", "target": f"{session_name}:{window_name}"}

            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._open_ghostty_worker_window", side_effect=fake_open_display),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)

            self.assertTrue(launched["ok"])
            self.assertEqual(started_windows, {"fake_impl", "fake_peer"})
            self.assertEqual(len(display_snapshots), 2)
            self.assertTrue(all(snapshot == {"fake_impl", "fake_peer"} for snapshot in display_snapshots))

    def test_launch_opens_single_ghostty_workspace_with_three_panes_per_tmux_window(self) -> None:
        for worker_count in [2, 3, 4, 8]:
            with self.subTest(worker_count=worker_count):
                with tempfile.TemporaryDirectory(prefix="team-agent-launch-workspace-") as tmp:
                    workspace = Path(tmp)
                    spec_path = workspace / "team.spec.yaml"
                    spec = _fake_spec_with_agents(workspace, worker_count)
                    spec["runtime"]["display_backend"] = "ghostty_workspace"
                    spec["leader"]["provider"] = "fake"
                    spec_path.write_text(dumps(spec), encoding="utf-8")
                    started_windows: set[str] = set()
                    calls: list[list[str]] = []
                    fake_run_cmd = _make_fake_ghostty_workspace_run_cmd(started_windows, calls)

                    with (
                        patch("team_agent.runtime._ghostty_app_exists", return_value=True),
                        patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                        patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                    ):
                        launched = runtime.launch(spec_path, auto_approve=True)

                    self.assertTrue(launched["ok"])
                    agent_ids = {agent["id"] for agent in spec["agents"]}
                    self.assertEqual(started_windows, agent_ids)
                    workspace_sessions = [
                        call
                        for call in calls
                        if call[:3] == ["tmux", "new-session", "-d"] and "-P" in call
                    ]
                    workspace_windows = [
                        call
                        for call in calls
                        if call[:2] == ["tmux", "new-window"] and "-P" in call
                    ]
                    linked_sessions = [
                        call
                        for call in calls
                        if call[:3] == ["tmux", "new-session", "-d"] and "-t" in call and "-P" not in call
                    ]
                    workspace_window_count = (worker_count + runtime.GHOSTTY_WORKSPACE_PANES_PER_WINDOW - 1) // runtime.GHOSTTY_WORKSPACE_PANES_PER_WINDOW
                    self.assertEqual(len(workspace_sessions), 1)
                    self.assertEqual(len(workspace_windows), workspace_window_count - 1)
                    self.assertEqual(len(linked_sessions), worker_count)
                    self.assertEqual(len([call for call in calls if call[:2] == ["tmux", "split-window"]]), worker_count - workspace_window_count)
                    self.assertEqual(
                        len([call for call in calls if call[:3] == ["tmux", "select-layout", "-t"] and call[-1] == "even-horizontal"]),
                        workspace_window_count,
                    )
                    self.assertEqual(len([call for call in calls if call[:3] == ["open", "-na", "Ghostty.app"]]), 1)
                    self.assertTrue(any(call[:2] == ["tmux", "set-window-option"] and call[-2:] == ["remain-on-exit", "on"] for call in calls))
                    self.assertTrue(any(call[:2] == ["tmux", "set-option"] and call[-2:] == ["mouse", "on"] for call in calls))
                    for call in calls:
                        if call[:2] in (["tmux", "send-keys"], ["tmux", "capture-pane"]) or call[:3] == ["tmux", "list-windows", "-t"]:
                            self.assertFalse(any(f"{spec['runtime']['session_name']}:" in arg and "." in arg for arg in call))
                    state = load_runtime_state(workspace)
                    displays = [state["agents"][agent_id]["display"] for agent_id in agent_ids]
                    self.assertTrue(all(display["backend"] == "ghostty_workspace" for display in displays))
                    self.assertEqual(len({display["aggregator_session"] for display in displays}), 1)
                    self.assertEqual(len({display["linked_session"] for display in displays}), worker_count)
                    self.assertEqual(len({display["pane_id"] for display in displays}), worker_count)
                    self.assertEqual(len({display["pane_title"] for display in displays}), worker_count)
                    window_sizes = sorted(
                        len([display for display in displays if display.get("workspace_window") == window])
                        for window in {display.get("workspace_window") for display in displays}
                    )
                    self.assertLessEqual(max(window_sizes), runtime.GHOSTTY_WORKSPACE_PANES_PER_WINDOW)
                    if worker_count == 4:
                        self.assertEqual(window_sizes, [1, 3])
                    if worker_count == 8:
                        self.assertEqual(window_sizes, [2, 3, 3])
                    self.assertEqual({tuple(display["pids"]) for display in displays}, {(4321,)})
                    pane_commands = [
                        call[-1]
                        for call in workspace_sessions
                        + workspace_windows
                        + [call for call in calls if call[:2] == ["tmux", "split-window"]]
                    ]
                    self.assertEqual(len(set(pane_commands)), worker_count)
                    for display in displays:
                        self.assertIn(display["linked_session"], " ".join(pane_commands))

    def test_launch_ghostty_workspace_skips_worker_when_linked_session_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-workspace-partial-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec_with_agents(workspace, 3)
            spec["runtime"]["display_backend"] = "ghostty_workspace"
            spec["leader"]["provider"] = "fake"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            started_windows: set[str] = set()
            calls: list[list[str]] = []
            failed_agent = "fake_peer"
            failed_linked = runtime._ghostty_display_session_name(spec["runtime"]["session_name"], failed_agent)
            fake_run_cmd = _make_fake_ghostty_workspace_run_cmd(
                started_windows,
                calls,
                fail_select_windows={failed_agent},
            )

            with (
                patch("team_agent.runtime._ghostty_app_exists", return_value=True),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)

            self.assertTrue(launched["ok"])
            state = load_runtime_state(workspace)
            self.assertEqual(state["agents"][failed_agent]["display"]["status"], "blocked")
            self.assertEqual(state["agents"][failed_agent]["display"]["reason"], "display_session_select_window_failed")
            opened = [agent_id for agent_id, agent_state in state["agents"].items() if agent_state["display"]["status"] == "opened"]
            self.assertEqual(len(opened), 2)
            workspace_sessions = [call for call in calls if call[:3] == ["tmux", "new-session", "-d"] and "-P" in call]
            splits = [call for call in calls if call[:2] == ["tmux", "split-window"]]
            self.assertEqual(len(workspace_sessions), 1)
            self.assertEqual(len(splits), 1)
            self.assertNotIn(failed_linked, " ".join(call[-1] for call in workspace_sessions + splits))
            self.assertIn(["tmux", "kill-session", "-t", failed_linked], calls)
            self.assertTrue(any(e["event"] == "display.ghostty_workspace_blocked" and e["agent_id"] == failed_agent for e in _events(workspace)))

    def test_incremental_task_keeps_existing_team_state(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-incremental-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-incremental-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            tools = TeamOrchestratorTools(workspace)
            try:
                self.assertTrue(launched["ok"])
                first = runtime.send_message(workspace, None, "initial task", task_id="task_impl", requires_ack=True)
                self.assertTrue(first["ok"])
                import time

                time.sleep(1.0)
                runtime.collect(workspace)
                state = runtime.status(workspace, as_json=True)
                self.assertEqual(state["tasks"][0]["status"], "done")
                original_session = state["session_name"]

                followup_task = {
                    "id": "task_followup",
                    "title": "Incremental follow-up",
                    "type": "implementation",
                    "assignee": None,
                    "deps": ["task_impl"],
                    "acceptance": ["follow-up result collected"],
                    "status": "pending",
                    "requires_tools": ["fs_write", "execute_bash"],
                    "files": ["src/followup.py"],
                    "risk": "low",
                }
                assigned = tools.assign_task(followup_task, message="handle follow-up")
                self.assertTrue(assigned["ok"])
                time.sleep(1.0)
                collected = runtime.collect(workspace)
                state = runtime.status(workspace, as_json=True)
                by_id = {task["id"]: task for task in state["tasks"]}
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_followup")
                else:
                    self.assertEqual(by_id["task_followup"]["status"], "done")
                    self.assertTrue(by_id["task_followup"].get("accepted_result_id"))
                self.assertEqual(state["session_name"], original_session)
                self.assertEqual(by_id["task_impl"]["status"], "done")
                self.assertEqual(by_id["task_followup"]["status"], "done")
            finally:
                runtime.shutdown(workspace)

    def test_manual_route_override_is_logged(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            alt = dict(spec["agents"][0])
            alt["id"] = "fake_alt"
            spec["agents"].append(alt)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "missing-route-test",
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_alt": {"status": "running", "provider": "fake", "window": "fake_alt"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.send_message(workspace, "fake_alt", "manual override", task_id="task_impl")
            self.assertFalse(result["ok"])
            routing_events = [e for e in _events(workspace) if e["event"] == "routing.decision"]
            override = routing_events[-1]
            self.assertEqual(override["source"], "send")
            self.assertEqual(override["route_agent"], "fake_impl")
            self.assertEqual(override["selected_agent"], "fake_alt")
            self.assertTrue(override["manual_override"])

    def test_leader_start_plan_creates_tmux_session_outside_tmux_and_passes_args(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-") as tmp:
            workspace = Path(tmp)
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            with (
                patch.dict(os.environ, {"TMUX": ""}, clear=False),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
            ):
                plan = runtime.leader_start_plan("codex", ["--model", "gpt-5.5"], workspace)
            self.assertEqual(plan["mode"], "new_tmux_session")
            self.assertEqual(plan["argv"][:3], ["tmux", "new-session", "-s"])
            self.assertIn("team-agent-leader-codex", plan["session_name"])
            self.assertIn("exec codex --model gpt-5.5", plan["argv"][-1])

    def test_leader_start_plan_inside_tmux_execs_provider_in_current_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-current-") as tmp:
            adapter = Mock()
            adapter.command_name = "claude"
            adapter.is_installed.return_value = True
            with (
                patch.dict(os.environ, {"TMUX": "/tmp/tmux.sock,1,0"}, clear=False),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
            ):
                plan = runtime.leader_start_plan("claude_code", ["--dangerously-skip-permissions"], Path(tmp))
            self.assertEqual(plan["mode"], "exec_provider")
            self.assertEqual(plan["argv"], ["claude", "--dangerously-skip-permissions"])

    def test_launch_blocks_missing_provider_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-provider-missing-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec["runtime"]["session_name"] = "team-agent-provider-missing-" + workspace.name[-6:]
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.providers.shutil.which", return_value=None),
                self.assertRaises(TeamAgentRuntimeError) as ctx,
            ):
                runtime.launch(spec_path, auto_approve=True)
            self.assertIn("Provider codex command 'codex' not found", str(ctx.exception))

    def test_codex_working_submit_key_uses_enter_for_followup_submission(self) -> None:
        submit_key, reason = runtime._choose_leader_submit_key("codex", "• running command esc to interrupt")
        self.assertEqual(submit_key, "Enter")
        self.assertEqual(reason, "codex_busy_submit_followup")

    def test_pasted_content_prompt_recognizes_new_claude_text_placeholder(self) -> None:
        self.assertTrue(runtime._capture_has_pasted_content_prompt("› [Pasted text #1 +67 lines]"))
        self.assertTrue(
            runtime._capture_has_pasted_content_prompt(
                "› [Pasted text #1 +67\n"
                "lines][Pasted text #2 +66 lines]\n"
            )
        )

    def test_rust_core_renderer_integration(self) -> None:
        payload = {"message_id": "msg_test", "from": "worker", "task_id": "task_x", "content": "hello"}
        rendered = core_render_message(payload)
        self.assertTrue(rendered["ok"])
        self.assertIn("Team Agent message from worker for task_x:", rendered["text"])
        self.assertIn("[team-agent-token:msg_test]", rendered["text"])
        self.assertIn(rendered["engine"], {"rust", "python_fallback"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
