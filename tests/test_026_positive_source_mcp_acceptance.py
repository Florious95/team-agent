from __future__ import annotations

import inspect
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

try:
    from .contracts.positive_source_rebind_fixtures import multi_team_state, read_events, write_runtime_state
except ImportError:
    from contracts.positive_source_rebind_fixtures import multi_team_state, read_events, write_runtime_state


class PositiveSourceMcpScopeAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="ta-pos-mcp-")
        self.workspace = Path(self.tmp.name)
        write_runtime_state(self.workspace, multi_team_state(active="team-b", include_ghost_top_level=True))

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_c13_worker_spawn_env_includes_team_agent_owner_team_id(self) -> None:
        provider_sources = "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted((Path.cwd() / "src" / "team_agent" / "provider_cli").glob("*.py"))
        )
        self.assertTrue("TEAM_AGENT_OWNER_TEAM_ID" in provider_sources, "worker spawn env must include TEAM_AGENT_OWNER_TEAM_ID")
        self.assertRegex(provider_sources, r"TEAM_AGENT_OWNER_TEAM_ID.*team")

    def test_c14_mcp_send_message_uses_sender_env_team_scope_without_worker_team_argument(self) -> None:
        from team_agent.mcp_server.tools import TeamOrchestratorTools

        captured: dict[str, object] = {}

        def fake_send_message(*args, **kwargs):
            captured["args"] = args
            captured["kwargs"] = kwargs
            return {"ok": True, "status": "accepted", "message_id": "msg_scope"}

        with patch.dict(os.environ, {"TEAM_AGENT_ID": "a_worker", "TEAM_AGENT_OWNER_TEAM_ID": "team-a"}, clear=False), \
             patch("team_agent.mcp_server.tools.runtime.send_message", side_effect=fake_send_message):
            result = TeamOrchestratorTools(self.workspace).send_message(to="a_peer", content="hello")

        self.assertTrue(result["ok"])
        self.assertIn("team", captured["kwargs"])
        self.assertEqual(captured["kwargs"]["team"], "team-a")
        self.assertEqual(captured["kwargs"]["sender"], "a_worker")
        self.assertEqual(captured["args"][1], "a_peer")

    def test_c15_star_broadcast_defaults_to_team_scope_unless_workspace_scope_is_explicit(self) -> None:
        from team_agent.mcp_server.tools import TeamOrchestratorTools

        captured: dict[str, object] = {}

        def fake_send_message(*args, **kwargs):
            captured["args"] = args
            captured["kwargs"] = kwargs
            return {"ok": True, "status": "accepted", "message_id": "msg_star"}

        with patch.dict(os.environ, {"TEAM_AGENT_ID": "a_worker", "TEAM_AGENT_OWNER_TEAM_ID": "team-a"}, clear=False), \
             patch("team_agent.mcp_server.tools.runtime.send_message", side_effect=fake_send_message):
            result = TeamOrchestratorTools(self.workspace).send_message(to="*", content="team broadcast")

        self.assertTrue(result["ok"])
        self.assertIn("team", captured["kwargs"])
        self.assertEqual(captured["kwargs"]["team"], "team-a")
        self.assertNotEqual(captured["kwargs"].get("scope"), "workspace")

    def test_c16_worker_visible_peer_list_is_filtered_to_sender_team_alive_agents(self) -> None:
        from team_agent.mcp_server.tools import TeamOrchestratorTools

        with patch.dict(os.environ, {"TEAM_AGENT_ID": "a_worker", "TEAM_AGENT_OWNER_TEAM_ID": "team-a"}, clear=False):
            tools = TeamOrchestratorTools(self.workspace)
            self.assertTrue(hasattr(tools, "get_visible_peers"), "MCP server must expose filtered visible peers")
            result = tools.get_visible_peers()

        self.assertEqual(set(result["peers"]), {"a_worker", "a_peer"})
        self.assertNotIn("b_peer", str(result))
        self.assertNotIn("c_dead", str(result))

    def test_c17_mcp_scope_resolution_source_is_sender_env_not_candidate_scan(self) -> None:
        from team_agent.mcp_server import tools as tools_mod

        source = inspect.getsource(tools_mod.TeamOrchestratorTools)
        self.assertTrue("TEAM_AGENT_OWNER_TEAM_ID" in source, "MCP scope source must be TEAM_AGENT_OWNER_TEAM_ID")
        forbidden = [
            "MessageStore(self.workspace).messages()",
            "active_assignees",
            "len(runtime_agents) == 1",
            "for row in reversed(messages)",
        ]
        for snippet in forbidden:
            self.assertNotIn(snippet, source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
