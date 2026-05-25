from __future__ import annotations

from team_agent import runtime
from team_agent.mcp_server.contracts import TOOLS
from team_agent.mcp_server.normalize import (
    _compact_tool_result,
    _normalize_report_envelope,
    _items,
    _text,
    _first_text,
    _normalize_result_status,
    _normalize_changes,
    _normalize_change_kind,
    _normalize_tests,
    _normalize_test_status,
    _normalize_risks,
    _normalize_artifacts,
    _normalize_next_actions,
)
from team_agent.mcp_server.server import dispatch, handle_mcp, main
from team_agent.mcp_server.tools import TeamOrchestratorTools

__all__ = [
    'TeamOrchestratorTools',
    'TOOLS',
    'dispatch',
    'handle_mcp',
    'main',
    '_compact_tool_result',
    '_normalize_report_envelope',
    '_items',
    '_text',
    '_first_text',
    '_normalize_result_status',
    '_normalize_changes',
    '_normalize_change_kind',
    '_normalize_tests',
    '_normalize_test_status',
    '_normalize_risks',
    '_normalize_artifacts',
    '_normalize_next_actions',
]
