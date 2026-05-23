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

class RuntimeTests04(unittest.TestCase):
    def test_ghostty_display_session_is_linked_and_window_selected(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            return Mock(returncode=0, stdout="", stderr="")

        with (
            patch("team_agent.runtime._tmux_window_exists", return_value=True),
            patch("team_agent.runtime._tmux_session_exists", side_effect=[True]),
            patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
        ):
            result = runtime._prepare_ghostty_display_session("team-chat0", "alice", "team-chat0__display__alice__12345678")

        self.assertTrue(result["ok"])
        self.assertEqual(
            calls,
            [
                ["tmux", "kill-session", "-t", "team-chat0__display__alice__12345678"],
                ["tmux", "new-session", "-d", "-t", "team-chat0", "-s", "team-chat0__display__alice__12345678"],
                ["tmux", "select-window", "-t", "team-chat0__display__alice__12345678:alice"],
            ],
        )

    def test_ghostty_workspace_pane_command_mirror_attaches_without_base_pane_addressing(self) -> None:
        linked_session = runtime._ghostty_display_session_name("team-chat0", "alice")
        command = runtime._ghostty_workspace_pane_command(linked_session)
        self.assertEqual(command, f"TMUX= tmux attach-session -t {linked_session}")
        self.assertNotIn(".0", command)
        self.assertNotIn("select-window", command)
        self.assertNotIn("send-keys", command)
        self.assertNotIn("capture-pane", command)

    def test_ghostty_pid_lookup_retries_until_open_process_is_visible(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            if len(calls) < 3:
                return Mock(returncode=1, stdout="", stderr="")
            return Mock(returncode=0, stdout="123\n456\n", stderr="")

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep") as sleep_mock:
            pids = runtime._ghostty_pids_by_title("team-agent:demo", wait_s=3.0)

        self.assertEqual(pids, [123, 456])
        self.assertEqual(sleep_mock.call_count, 2)
        self.assertEqual(calls, [["pgrep", "-f", "--title=team-agent:demo"]] * 3)

    def test_ghostty_workspace_blocked_when_app_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-workspace-blocked-") as tmp:
            workspace = Path(tmp)
            runtime.ensure_workspace_dirs(workspace)
            event_log = runtime.EventLog(workspace)
            jobs = [
                ("fake_impl", {"id": "fake_impl", "role": "Implementation Worker"}),
                ("fake_peer", {"id": "fake_peer", "role": "Peer Worker"}),
            ]
            with patch("team_agent.runtime._ghostty_app_exists", return_value=False), patch("team_agent.runtime.run_cmd") as run_cmd_mock:
                displays = runtime._open_worker_displays(
                    workspace,
                    "team-workspace-blocked",
                    jobs,
                    event_log,
                    "ghostty_workspace",
                )

            run_cmd_mock.assert_not_called()
            self.assertEqual(set(displays), {"fake_impl", "fake_peer"})
            self.assertTrue(all(display["status"] == "blocked" for display in displays.values()))
            self.assertTrue(all(display["reason"] == "ghostty_app_missing" for display in displays.values()))
            self.assertTrue(any(e["event"] == "display.ghostty_workspace_blocked" for e in _events(workspace)))

    def test_status_json_defaults_to_compact_context_safe_shape(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-compact-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(
                workspace,
                {
                    "session_name": None,
                    "leader": {"id": "leader"},
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "model": "fake-model",
                            "profile": "fake-profile",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "permissions": {"resolved_tools": [{"tool": "fs_read", "enforcement": "prompt_only"}]},
                            "display": {
                                "backend": "ghostty_workspace",
                                "status": "opened",
                                "workspace_window": "overview",
                                "pane_id": "%1",
                                "launch_args": ["open", "-na", "Ghostty.app", "--args", "--title=long"],
                            },
                        }
                    },
                    "tasks": [
                        {
                            "id": "task_impl",
                            "title": "Implement",
                            "assignee": "fake_impl",
                            "status": "done",
                            "last_result_summary": "x" * 1000,
                        }
                    ],
                },
            )
            runtime.EventLog(workspace).write(
                "restart.agent_start",
                agent_id="fake_impl",
                command="codex resume " + "x" * 2000,
                payload={"content": "y" * 2000},
                agents=[{"agent_id": "fake_impl", "restart_mode": "resumed", "session_id": "session-123", "display_target": {"launch_args": ["x"]}}],
            )

            compact = runtime.status(workspace, as_json=True, compact=True)
            full = runtime.status(workspace, as_json=True)
            cli_compact = cli.cmd_status(Mock(workspace=str(workspace), json=True, detail=False))
            cli_detail = cli.cmd_status(Mock(workspace=str(workspace), json=True, detail=True))

            compact_text = json.dumps(compact, ensure_ascii=False, indent=2)
            full_text = json.dumps(full, ensure_ascii=False)
            self.assertLess(len(compact_text), len(full_text))
            self.assertLess(len(compact_text.splitlines()), 80)
            self.assertNotIn("codex resume", compact_text)
            self.assertNotIn('"payload"', compact_text)
            self.assertNotIn('"launch_args"', compact_text)
            self.assertNotIn('"permissions"', compact_text)
            self.assertEqual(compact["agents"]["fake_impl"]["display"]["workspace_window"], "overview")
            self.assertIn("command", full["last_events"][-1])
            cli_compact_text = json.dumps(cli_compact, ensure_ascii=False, indent=2)
            self.assertNotIn("codex resume", cli_compact_text)
            self.assertNotIn('"payload"', cli_compact_text)
            self.assertNotIn('"launch_args"', cli_compact_text)
            self.assertIn("command", cli_detail["last_events"][-1])

    def test_coordinator_auto_approves_internal_mcp_prompt_with_retry_verification(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-auto-approval-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def prompt_text() -> str:
                return "\n".join(
                    [
                        "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                        "› 1. Allow        Run the tool and continue.",
                        "  2. Allow for this session  Run the tool and remember this choice for this session.",
                        "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                        "  4. Cancel       Cancel this tool call.",
                        "enter to submit | esc to cancel",
                    ]
                )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    if f"-{runtime.APPROVAL_SCAN_LINES}" in args:
                        proc.stdout = prompt_text() if len(send_calls) < 2 else "codex>"
                    else:
                        proc.stdout = "codex>" if len(send_calls) >= 2 else "›"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual([call[-2:] for call in send_calls], [["Down", "Enter"], ["Down", "Enter"]])
            events = _events(workspace)
            handled = [e for e in events if e["event"] == "runtime.internal_mcp_approval.auto"]
            self.assertEqual(len(handled), 1)
            self.assertTrue(handled[0]["ok"])
            self.assertEqual(handled[0]["tool"], "report_result")
            self.assertEqual(handled[0]["choice"], "Allow for this session")
            self.assertEqual(handled[0]["verification"], "prompt_absent_after_submit")
            self.assertEqual(len(handled[0]["attempts"]), 2)

    def test_coordinator_auto_approves_claude_internal_mcp_prompt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-auto-approval-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "claude_code"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "claude_code", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def prompt_text() -> str:
                return "\n".join(
                    [
                        "Tool use",
                        'team_orchestrator - send_message(to: "gpt", content: "private payload omitted") (MCP)',
                        "Send a message to a teammate, the leader, or '*' for all other team members.",
                        "",
                        "Do you want to proceed?",
                        "❯ 1. Yes",
                        "  2. Yes, and don't ask again for team_orchestrator - send_message commands in /Users/alauda/Documents/code/agent前沿探索/11",
                        "  3. No",
                        "",
                        "Esc to cancel · Tab to amend",
                    ]
                )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = prompt_text() if not send_calls else "Claude Code\n>"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                approvals = runtime.approvals(workspace)
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(approvals["waiting"])
            self.assertEqual(approvals["approvals"][0]["tool"], "send_message")
            self.assertNotIn("private payload", json.dumps(approvals, ensure_ascii=False))
            self.assertTrue(result["ok"])
            self.assertEqual([call[-2:] for call in send_calls], [["Down", "Enter"]])
            events = _events(workspace)
            handled = [e for e in events if e["event"] == "runtime.internal_mcp_approval.auto"]
            self.assertEqual(len(handled), 1)
            self.assertTrue(handled[0]["ok"])
            self.assertEqual(handled[0]["tool"], "send_message")
            self.assertIn("don't ask again", handled[0]["choice"])
            self.assertEqual(handled[0]["verification"], "prompt_absent_after_submit")

    def test_coordinator_does_not_auto_approve_non_allowlisted_mcp_prompt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-auto-approval-skip-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []
            prompt = "\n".join(
                [
                    "Allow the team_orchestrator MCP server to run tool \"assign_task\"?",
                    "› 1. Allow        Run the tool and continue.",
                    "  2. Allow for this session  Run the tool and remember this choice for this session.",
                    "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                    "  4. Cancel       Cancel this tool call.",
                    "enter to submit | esc to cancel",
                ]
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = prompt
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual(send_calls, [])
            self.assertEqual(MessageStore(workspace).agent_health()["fake_impl"]["status"], "AWAITING_APPROVAL")
            events = _events(workspace)
            skipped = [e for e in events if e["event"] == "runtime.internal_mcp_approval.skipped"]
            self.assertEqual(len(skipped), 1)
            self.assertEqual(skipped[0]["tool"], "assign_task")
            self.assertEqual(skipped[0]["reason"], "tool_not_allowlisted")

    def test_coordinator_tick_updates_health_and_scheduled_events(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["tick_interval_sec"] = 1
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
            store = MessageStore(workspace)
            store.add_scheduled_event("2000-01-01T00:00:00+00:00", "leader", "health_ping", {"note": "test"})

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "TEAM_AGENT_FAKE_READY agent=fake_impl"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual(store.agent_health()["fake_impl"]["status"], "IDLE")
            self.assertEqual(result["scheduled"], [1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
