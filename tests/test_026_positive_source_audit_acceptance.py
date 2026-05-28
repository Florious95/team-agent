from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

try:
    from .contracts.positive_source_rebind_fixtures import (
        MultiPaneTmuxFixture,
        prepare_team_runtime_dir,
        multi_team_state,
        read_events,
        shutdown_state,
        tmux_available,
        write_runtime_state,
    )
except ImportError:
    from contracts.positive_source_rebind_fixtures import (
        MultiPaneTmuxFixture,
        prepare_team_runtime_dir,
        multi_team_state,
        read_events,
        shutdown_state,
        tmux_available,
        write_runtime_state,
    )


class PositiveSourceAuditAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="ta-pos-audit-")
        self.workspace = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    @unittest.skipUnless(tmux_available(), "tmux required for real caller-pane fixture")
    def test_c20_successful_bind_emits_owner_bound_from_caller_pane_schema(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state("current"))
        with MultiPaneTmuxFixture() as fixture:
            pane = fixture.panes["claude_active"]
            with patch.dict(
                os.environ,
                {
                    "TMUX_PANE": pane,
                    "TEAM_AGENT_LEADER_PANE_ID": pane,
                    "TEAM_AGENT_LEADER_PROVIDER": "claude",
                    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": "uuid-new-caller",
                    "TEAM_AGENT_MACHINE_FINGERPRINT": "machine-new",
                },
                clear=False,
            ):
                runtime.takeover(self.workspace, team="current", confirm=True)

        events = read_events(self.workspace)
        matches = [event for event in events if event.get("event") == "owner.bound_from_caller_pane"]
        self.assertEqual(len(matches), 1)
        event = matches[0]
        for key in ("caller_pane_id", "caller_current_command", "derived_uuid_prefix", "team_id", "old_uuid_prefix"):
            self.assertIn(key, event)
        self.assertEqual(event["caller_pane_id"], pane)

    def test_c21_shutdown_completed_and_blocked_events_have_required_schema(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state("current"))
        prepare_team_runtime_dir(self.workspace, "team-current-session")

        with patch("team_agent.runtime.check_team_owner", return_value=None):
            runtime.shutdown(self.workspace, keep_logs=True, team="current")

        events = read_events(self.workspace)
        matches = [event for event in events if event.get("event") == "team.shutdown_completed"]
        self.assertEqual(len(matches), 1)
        completed = matches[0]
        for key in ("team_key", "archive_path", "teams_remaining", "new_active_team_key"):
            self.assertIn(key, completed)

    def test_c22_mcp_scope_resolved_event_has_required_schema(self) -> None:
        from team_agent.mcp_server.tools import TeamOrchestratorTools

        write_runtime_state(self.workspace, multi_team_state(active="team-a", include_ghost_top_level=True))

        with patch.dict(os.environ, {"TEAM_AGENT_ID": "a_worker", "TEAM_AGENT_OWNER_TEAM_ID": "team-a"}, clear=False), \
             patch("team_agent.mcp_server.tools.runtime.send_message", return_value={"ok": True, "status": "accepted", "message_id": "msg_scope"}):
            TeamOrchestratorTools(self.workspace).send_message(to="a_peer", content="hello")

        events = read_events(self.workspace)
        matches = [event for event in events if event.get("event") == "mcp.scope_resolved"]
        self.assertEqual(len(matches), 1)
        event = matches[0]
        self.assertEqual(event["sender_team_id"], "team-a")
        self.assertEqual(event["requested_to"], "a_peer")
        self.assertEqual(event["resolved_agent"], "a_peer")
        self.assertIn(event["scope"], {"team", "workspace"})

    def test_c23_rejection_paths_emit_closed_reason_and_user_hint_without_silent_fallback(self) -> None:
        from team_agent.mcp_server.tools import TeamOrchestratorTools

        write_runtime_state(self.workspace, multi_team_state(active="team-a", include_ghost_top_level=True))

        with patch.dict(os.environ, {"TEAM_AGENT_ID": "a_worker", "TEAM_AGENT_OWNER_TEAM_ID": "team-a"}, clear=False):
            result = TeamOrchestratorTools(self.workspace).send_message(to="b_peer", content="cross-team")

        self.assertFalse(result["ok"])
        self.assertEqual(result["reason"], "peer_not_in_scope")
        self.assertNotIn("b_peer", result.get("hint", ""))
        events = read_events(self.workspace)
        matches = [event for event in events if event.get("event", "").endswith("_refused")]
        self.assertGreaterEqual(len(matches), 1)
        refusal = matches[0]
        self.assertIn(refusal["reason"], {"caller_pane_missing", "caller_not_leader_shaped", "team_target_ambiguous", "peer_not_in_scope", "peer_not_found"})
        self.assertIn("hint", refusal)


if __name__ == "__main__":
    unittest.main(verbosity=2)
