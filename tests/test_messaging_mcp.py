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

class MessagingMcpTests(unittest.TestCase):
    def test_status_peek_inbox_and_agent_health(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-") as tmp:
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
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )
            MessageStore(workspace).create_message("task_impl", "leader", "fake_impl", "hello")

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
                text = runtime.format_status(workspace)
                detail = runtime.format_status(workspace, "fake_impl")
                peeked = runtime.peek(workspace, "fake_impl", tail=20)
                searched = runtime.peek(workspace, "fake_impl", search="fake_ready", context=0)
            self.assertIn("fake_impl  IDLE  task_impl", text)
            self.assertIn("results total 0 uncollected 0 collected 0 invalid 0", text)
            self.assertIn("sid session-123", text)
            self.assertIn("via fs_watch high", text)
            self.assertIn("recent messages", detail)
            self.assertIn("session_id: session-123", detail)
            self.assertIn("TEAM_AGENT_FAKE_READY", peeked["text"])
            self.assertIn("TEAM_AGENT_FAKE_READY", searched["text"])
            with self.assertRaises(TeamAgentRuntimeError):
                runtime.peek(workspace, "fake_impl")
            inbox = runtime.format_inbox(workspace, "fake_impl")
            self.assertIn("leader -> fake_impl", inbox)
            self.assertIn("final results are not in inbox", inbox)

    def test_approvals_returns_structured_prompt_without_terminal_page(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-approvals-") as tmp:
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

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "\n".join(
                        [
                            "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                            "envelope: {\"schema_version\":\"result_envelope_v1\",\"summary\":\"large private payload\"}",
                            "› 1. Allow        Run the tool and continue.",
                            "  2. Allow for this session  Run the tool and remember this choice for this session.",
                            "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                            "  4. Cancel       Cancel this tool call.",
                            "enter to submit | esc to cancel",
                        ]
                    )
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.approvals(workspace)
                text = runtime.format_approvals(workspace)
            self.assertTrue(result["waiting"])
            item = result["approvals"][0]
            self.assertEqual(item["agent_id"], "fake_impl")
            self.assertEqual(item["kind"], "mcp_tool")
            self.assertEqual(item["tool"], "report_result")
            self.assertIn("Allow", item["choices"])
            self.assertNotIn("large private payload", json.dumps(result, ensure_ascii=False))
            self.assertIn("report_result", text)
            self.assertNotIn("large private payload", text)

    def test_stale_approval_prompt_in_scrollback_is_not_current_approval(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stale-approval-") as tmp:
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
            stale_prompt = "\n".join(
                [
                    "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                    "› 1. Allow        Run the tool and continue.",
                    "  2. Allow for this session  Run the tool and remember this choice for this session.",
                    "enter to submit | esc to cancel",
                    "• Called",
                    "  └ team_orchestrator.report_result({\"summary\":\"done\"})",
                    "    {\"ok\": true}",
                    "› Implement {feature}",
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
                    proc.stdout = stale_prompt
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                approvals = runtime.approvals(workspace)
                result = runtime.coordinator_tick(workspace)
            self.assertFalse(approvals["waiting"])
            self.assertTrue(result["ok"])
            self.assertEqual(send_calls, [])
            self.assertNotEqual(MessageStore(workspace).agent_health()["fake_impl"]["status"], "AWAITING_APPROVAL")

    def test_mcp_send_message_accepts_thin_args_and_returns_compact_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-thin-send-") as tmp:
            workspace = Path(tmp)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {
                        "ok": True,
                        "status": "submitted",
                        "message_id": "msg_123",
                        "to": "leader",
                        "visible": True,
                        "submitted": True,
                        "channel": "direct_tmux",
                        "leader_receiver": {"pane_id": "%1"},
                        "verification": {"token": "msg_123"},
                    }
                    result = TeamOrchestratorTools(workspace).send_message(to="leader", content="hello")
            send.assert_called_once()
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "leader", "hello"))
            self.assertEqual(kwargs["sender"], "fake_impl")
            self.assertIsNone(kwargs["task_id"])
            self.assertFalse(kwargs["requires_ack"])
            self.assertEqual(
                result,
                {
                    "ok": True,
                    "status": "submitted",
                    "message_id": "msg_123",
                    "to": "leader",
                    "submitted": True,
                    "visible": True,
                },
            )

    def test_mcp_send_message_accepts_broadcast_target(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-broadcast-") as tmp:
            workspace = Path(tmp)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {
                        "ok": True,
                        "status": "broadcast_delivered",
                        "to": "*",
                        "targets": ["leader", "fake_peer"],
                        "delivered_count": 2,
                        "failed_count": 0,
                        "deliveries": [{"ok": True, "to": "leader"}, {"ok": True, "to": "fake_peer"}],
                    }
                    result = TeamOrchestratorTools(workspace).send_message(to="*", content="hello all")
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "*", "hello all"))
            self.assertEqual(kwargs["sender"], "fake_impl")
            self.assertTrue(kwargs["requires_ack"])
            self.assertEqual(
                result,
                {
                    "ok": True,
                    "status": "broadcast_delivered",
                    "to": "*",
                    "targets": ["leader", "fake_peer"],
                    "delivered_count": 2,
                    "failed_count": 0,
                },
            )

    def test_mcp_send_message_without_env_uses_unknown_sender_without_candidate_scan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-missing-id-send-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            old_agent = os.environ.pop("TEAM_AGENT_ID", None)
            try:
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {"ok": True, "status": "submitted", "message_id": "msg_123", "to": "leader"}
                    result = TeamOrchestratorTools(workspace).send_message(to="leader", content="hello")
            finally:
                if old_agent is not None:
                    os.environ["TEAM_AGENT_ID"] = old_agent
            self.assertTrue(result["ok"])
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "leader", "hello"))
            self.assertEqual(kwargs["sender"], "unknown")
            self.assertFalse(kwargs["requires_ack"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
