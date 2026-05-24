from __future__ import annotations

import unittest
from pathlib import Path


class McpServerBoundaryTests(unittest.TestCase):
    def test_package_reexports_legacy_mcp_server_surface(self) -> None:
        from team_agent import mcp_server
        from team_agent.mcp_server import contracts, normalize, server, tools

        self.assertIs(mcp_server.TeamOrchestratorTools, tools.TeamOrchestratorTools)
        self.assertIs(mcp_server.TOOLS, contracts.TOOLS)
        self.assertIs(mcp_server.dispatch, server.dispatch)
        self.assertIs(mcp_server.handle_mcp, server.handle_mcp)
        self.assertIs(mcp_server.main, server.main)
        self.assertIs(mcp_server._compact_tool_result, normalize._compact_tool_result)
        self.assertIs(mcp_server._normalize_report_envelope, normalize._normalize_report_envelope)
        self.assertIn("TeamOrchestratorTools", mcp_server.__all__)
        self.assertIn("_normalize_report_envelope", mcp_server.__all__)

    def test_json_rpc_tools_list_uses_contracts_module(self) -> None:
        from team_agent.mcp_server import TOOLS, TeamOrchestratorTools, handle_mcp

        tools = TeamOrchestratorTools(Path("."))
        response = handle_mcp(tools, {"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        self.assertEqual(response["result"]["tools"], TOOLS)


if __name__ == "__main__":
    unittest.main()
