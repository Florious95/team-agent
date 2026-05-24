from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


LEGACY_MCP_SERVER_EXPORTS = (
    "TeamOrchestratorTools",
    "TOOLS",
    "dispatch",
    "_compact_tool_result",
    "_normalize_report_envelope",
    "_items",
    "_text",
    "_first_text",
    "_normalize_result_status",
    "_normalize_changes",
    "_normalize_change_kind",
    "_normalize_tests",
    "_normalize_test_status",
    "_normalize_risks",
    "_normalize_artifacts",
    "_normalize_next_actions",
    "handle_mcp",
    "main",
)


class McpServerBoundaryTests(unittest.TestCase):
    def _registered_tool_names(self) -> set[str]:
        from team_agent import mcp_server

        return {tool["name"] for tool in mcp_server.TOOLS}

    def _call_tool(self, tools: object, name: str, arguments: dict[str, object], request_id: int = 1) -> tuple[dict[str, object], dict[str, object]]:
        from team_agent import mcp_server

        self.assertIn(name, self._registered_tool_names())
        response = mcp_server.handle_mcp(
            tools,
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            },
        )
        self.assertIsNotNone(response)
        self.assertEqual(response["jsonrpc"], "2.0")
        self.assertEqual(response["id"], request_id)
        content = response["result"]["content"]
        self.assertEqual(content[0]["type"], "text")
        payload = json.loads(content[0]["text"])
        return response, payload

    def test_package_reexports_legacy_mcp_server_surface(self) -> None:
        from team_agent import mcp_server
        from team_agent.mcp_server import contracts, normalize, server, tools

        for name in LEGACY_MCP_SERVER_EXPORTS:
            self.assertIn(name, mcp_server.__all__)
            self.assertTrue(hasattr(mcp_server, name), name)
        self.assertTrue(hasattr(mcp_server, "runtime"))
        self.assertIs(mcp_server.TeamOrchestratorTools, tools.TeamOrchestratorTools)
        self.assertIs(mcp_server.TOOLS, contracts.TOOLS)
        self.assertIs(mcp_server.dispatch, server.dispatch)
        self.assertIs(mcp_server.handle_mcp, server.handle_mcp)
        self.assertIs(mcp_server.main, server.main)
        self.assertIs(mcp_server._compact_tool_result, normalize._compact_tool_result)
        self.assertIs(mcp_server._normalize_report_envelope, normalize._normalize_report_envelope)

    def test_json_rpc_tools_list_uses_contracts_module(self) -> None:
        from team_agent.mcp_server import TOOLS, TeamOrchestratorTools, handle_mcp

        tools = TeamOrchestratorTools(Path("."))
        response = handle_mcp(tools, {"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        self.assertEqual(response["result"]["tools"], TOOLS)

    def test_report_result_tools_call_roundtrip_normalizes_and_compacts_result(self) -> None:
        from team_agent import mcp_server

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-report-") as tmp:
            workspace = Path(tmp)
            with patch.dict("os.environ", {"TEAM_AGENT_ID": "fake_impl"}):
                tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result") as report:
                report.return_value = {
                    "ok": True,
                    "status": "stored",
                    "result_id": "res_123",
                    "task_id": "manual",
                    "agent_id": "fake_impl",
                    "leader_notified": True,
                    "ignored_private_detail": "not in compact result",
                }
                response, payload = self._call_tool(
                    tools,
                    "report_result",
                    {
                        "summary": "done",
                        "status": "ok",
                        "changes": [{"file": "src/example.py", "summary": "edited"}],
                        "tests": [{"cmd": "unit", "status": "ok"}],
                    },
                )
        self.assertFalse(response["result"]["isError"])
        self.assertEqual(
            payload,
            {
                "ok": True,
                "status": "stored",
                "result_id": "res_123",
                "task_id": "manual",
                "agent_id": "fake_impl",
                "leader_notified": True,
            },
        )
        report.assert_called_once()
        envelope = report.call_args.args[1]
        self.assertEqual(envelope["schema_version"], "result_envelope_v1")
        self.assertEqual(envelope["agent_id"], "fake_impl")
        self.assertEqual(envelope["task_id"], "manual")
        self.assertEqual(envelope["status"], "success")
        self.assertEqual(envelope["changes"][0]["path"], "src/example.py")
        self.assertEqual(envelope["tests"][0]["status"], "passed")

    def test_assign_task_tools_call_roundtrip_persists_task_and_compacts_delivery(self) -> None:
        from team_agent import mcp_server
        from team_agent.state import load_runtime_state, save_runtime_state

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-assign-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"agents": {"fake_impl": {}}, "tasks": [], "session_name": None})
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.send_message") as send:
                send.return_value = {
                    "ok": True,
                    "status": "submitted",
                    "message_id": "msg_assign",
                    "to": "fake_impl",
                    "submitted": True,
                    "visible": True,
                    "leader_receiver": {"pane_id": "%1"},
                }
                response, payload = self._call_tool(
                    tools,
                    "assign_task",
                    {
                        "task": {
                            "id": "task_new",
                            "title": "Build feature",
                            "description": "Detailed task",
                            "assignee": "fake_impl",
                            "status": "pending",
                        },
                        "message": "Do the task",
                    },
                )
            state = load_runtime_state(workspace)
        self.assertFalse(response["result"]["isError"])
        self.assertEqual(payload["message_id"], "msg_assign")
        self.assertEqual(payload["to"], "fake_impl")
        self.assertEqual(state["tasks"][0]["id"], "task_new")
        send.assert_called_once_with(workspace.resolve(), "fake_impl", "Do the task", task_id="task_new")

    def test_send_message_tools_call_roundtrip_covers_leader_and_worker_paths(self) -> None:
        from team_agent import mcp_server

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-send-") as tmp:
            workspace = Path(tmp)
            leader_tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.send_message") as send:
                send.return_value = {"ok": True, "status": "submitted", "message_id": "msg_worker", "to": "fake_impl", "submitted": True}
                response, payload = self._call_tool(
                    leader_tools,
                    "send_message",
                    {"to": "fake_impl", "content": "hello worker", "sender": "leader", "requires_ack": True},
                )
            self.assertFalse(response["result"]["isError"])
            self.assertEqual(payload, {"ok": True, "status": "submitted", "message_id": "msg_worker", "to": "fake_impl", "submitted": True})
            send.assert_called_once_with(
                workspace.resolve(),
                "fake_impl",
                "hello worker",
                task_id=None,
                sender="leader",
                requires_ack=True,
            )

            with patch.dict("os.environ", {"TEAM_AGENT_ID": "fake_impl"}):
                worker_tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.send_message") as send:
                send.return_value = {"ok": True, "status": "submitted", "message_id": "msg_leader", "to": "leader", "visible": True}
                response, payload = self._call_tool(worker_tools, "send_message", {"to": "leader", "content": "done"})
            self.assertFalse(response["result"]["isError"])
            self.assertEqual(payload, {"ok": True, "status": "submitted", "message_id": "msg_leader", "to": "leader", "visible": True})
            send.assert_called_once_with(
                workspace.resolve(),
                "leader",
                "done",
                task_id=None,
                sender="fake_impl",
                requires_ack=False,
            )

    def test_tools_call_rejects_malformed_payloads_with_structured_error(self) -> None:
        from team_agent import mcp_server

        tools = mcp_server.TeamOrchestratorTools(Path("."))
        response, payload = self._call_tool(tools, "send_message", {"to": "fake_impl"})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["error_code"], "invalid_tool_arguments")
        self.assertEqual(payload["exc_type"], "TypeError")
        self.assertIn("content", payload["message"])
        self.assertEqual(payload["error"], payload["message"])

        response, payload = self._call_tool(tools, "assign_task", {})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["error_code"], "invalid_tool_arguments")
        self.assertEqual(payload["exc_type"], "TypeError")
        self.assertIn("task", payload["message"])
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_unknown_tool_returns_structured_error(self) -> None:
        from team_agent import mcp_server

        tools = mcp_server.TeamOrchestratorTools(Path("."))
        response = mcp_server.handle_mcp(
            tools,
            {
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {"name": "missing_tool", "arguments": {}},
            },
        )
        payload = json.loads(response["result"]["content"][0]["text"])
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "unknown_tool")
        self.assertEqual(payload["error_code"], "unknown_tool")
        self.assertEqual(payload["exc_type"], "UnknownTool")
        self.assertEqual(payload["message"], "unknown tool 'missing_tool'")
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_validation_subclass_is_reported_as_bad_arguments(self) -> None:
        from team_agent import mcp_server

        class ProfileValidationError(ValueError):
            pass

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-validation-error-") as tmp:
            workspace = Path(tmp)
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result", side_effect=ProfileValidationError("bad envelope")):
                response, payload = self._call_tool(tools, "report_result", {"summary": "done"})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["error_code"], "invalid_tool_arguments")
        self.assertEqual(payload["exc_type"], "ProfileValidationError")
        self.assertEqual(payload["message"], "bad envelope")
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_key_error_is_reported_as_bad_arguments(self) -> None:
        from team_agent import mcp_server

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-key-error-") as tmp:
            workspace = Path(tmp)
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result", side_effect=KeyError("missing_field")):
                response, payload = self._call_tool(tools, "report_result", {"summary": "done"})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["error_code"], "invalid_tool_arguments")
        self.assertEqual(payload["exc_type"], "KeyError")
        self.assertIn("missing_field", payload["message"])
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_attribute_error_is_reported_as_bad_arguments(self) -> None:
        from team_agent import mcp_server

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-attribute-error-") as tmp:
            workspace = Path(tmp)
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result", side_effect=AttributeError("missing attribute")):
                response, payload = self._call_tool(tools, "report_result", {"summary": "done"})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["error_code"], "invalid_tool_arguments")
        self.assertEqual(payload["exc_type"], "AttributeError")
        self.assertEqual(payload["message"], "missing attribute")
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_runtime_failure_is_not_reported_as_bad_arguments(self) -> None:
        from team_agent import mcp_server

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-runtime-error-") as tmp:
            workspace = Path(tmp)
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result", side_effect=RuntimeError("storage unavailable")):
                response, payload = self._call_tool(tools, "report_result", {"summary": "done"})
        self.assertTrue(response["result"]["isError"])
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["reason"], "internal_runtime_error")
        self.assertEqual(payload["error_code"], "internal_runtime_error")
        self.assertEqual(payload["exc_type"], "RuntimeError")
        self.assertNotEqual(payload["reason"], "invalid_tool_arguments")
        self.assertEqual(payload["message"], "storage unavailable")
        self.assertEqual(payload["error"], payload["message"])

    def test_tools_call_internal_error_message_is_public_safe_and_bounded(self) -> None:
        from team_agent import mcp_server

        noisy_message = "secret-ish context\n" + ("x" * 300)
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-runtime-error-") as tmp:
            workspace = Path(tmp)
            tools = mcp_server.TeamOrchestratorTools(workspace)
            with patch("team_agent.mcp_server.runtime.report_result", side_effect=RuntimeError(noisy_message)):
                _, payload = self._call_tool(tools, "report_result", {"summary": "done"})
        self.assertEqual(payload["reason"], "internal_runtime_error")
        self.assertEqual(payload["error_code"], "internal_runtime_error")
        self.assertEqual(payload["exc_type"], "RuntimeError")
        self.assertNotIn("\n", payload["message"])
        self.assertLessEqual(len(payload["message"]), 200)
        self.assertEqual(payload["error"], payload["message"])

    def test_compact_and_normalize_helpers_preserve_documented_shapes(self) -> None:
        from team_agent import mcp_server

        compact = mcp_server._compact_tool_result(
            {
                "ok": True,
                "status": "submitted",
                "message_id": "msg_123",
                "to": "leader",
                "acknowledged_messages": [{"message_id": "msg_123"}],
                "private": "drop",
            }
        )
        self.assertEqual(compact, {"ok": True, "status": "submitted", "message_id": "msg_123", "to": "leader", "acknowledged_count": 1})

        envelope = mcp_server._normalize_report_envelope(
            {
                "task_id": "",
                "agent_id": "",
                "status": "done",
                "summary": "finished",
                "changes": [{"filepath": "a.py", "action": "added", "detail": "new file"}],
                "tests": ["unit"],
                "risks": ["none"],
                "artifacts": ["artifact.txt"],
                "next_actions": ["ship"],
            }
        )
        self.assertEqual(envelope["task_id"], "manual")
        self.assertEqual(envelope["agent_id"], "unknown")
        self.assertEqual(envelope["status"], "success")
        self.assertEqual(envelope["changes"], [{"path": "a.py", "kind": "created", "description": "new file"}])
        self.assertEqual(envelope["tests"], [{"command": "unit", "status": "not_run"}])
        self.assertEqual(envelope["risks"], [{"severity": "low", "description": "none"}])
        self.assertEqual(envelope["artifacts"], [{"path": "artifact.txt", "description": "artifact.txt"}])
        self.assertEqual(envelope["next_actions"], [{"description": "ship"}])


if __name__ == "__main__":
    unittest.main()
