from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

try:
    from .contracts.positive_source_rebind_fixtures import (
        multi_team_state,
        prepare_team_runtime_dir,
        read_events,
        read_runtime_state,
        shutdown_state,
        write_runtime_state,
    )
except ImportError:
    from contracts.positive_source_rebind_fixtures import (
        multi_team_state,
        prepare_team_runtime_dir,
        read_events,
        read_runtime_state,
        shutdown_state,
        write_runtime_state,
    )


class PositiveSourceStateAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="ta-pos-state-")
        self.workspace = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_c6_legacy_state_migrates_once_to_active_team_key(self) -> None:
        from team_agent.state import load_runtime_state

        state = multi_team_state(active=None, include_ghost_top_level=False)
        state.pop("active_team_key", None)
        write_runtime_state(self.workspace, state)

        loaded = load_runtime_state(self.workspace)

        self.assertIn("active_team_key", loaded)
        self.assertEqual(loaded["active_team_key"], "team-b")
        persisted = read_runtime_state(self.workspace)
        self.assertEqual(persisted["active_team_key"], "team-b")

    def test_c7_team_state_candidates_reads_only_alive_teams_and_never_top_level_ghost(self) -> None:
        from team_agent.state import team_state_candidates

        candidates = team_state_candidates(multi_team_state(active="team-b", include_ghost_top_level=True))

        self.assertEqual(set(candidates), {"team-a", "team-b"})

    def test_c8_top_level_state_is_derived_from_active_team_key(self) -> None:
        from team_agent.state import select_runtime_state

        write_runtime_state(self.workspace, multi_team_state(active="team-b", include_ghost_top_level=True))

        try:
            selected = select_runtime_state(self.workspace)
        except Exception as exc:  # noqa: BLE001 - current implementation refuses instead of deriving active view.
            self.fail(f"select_runtime_state must derive from active_team_key, got refusal/error: {exc}")

        self.assertEqual(selected["session_name"], "team-b-session")
        self.assertEqual(selected["team_dir"], ".team/team-b")

    def test_c9_unqualified_send_uses_active_team_and_ambiguous_candidates_are_alive_only(self) -> None:
        from team_agent.state import ambiguous_team_target_result, resolve_team_scoped_state

        state = multi_team_state(active="team-b", include_ghost_top_level=True)
        write_runtime_state(self.workspace, state)

        selected, refusal = resolve_team_scoped_state(self.workspace, None)
        self.assertIsNone(refusal)
        self.assertEqual(selected["session_name"], "team-b-session")

        state["active_team_key"] = None
        ambiguous = ambiguous_team_target_result(state)
        self.assertEqual(ambiguous["reason"], "team_target_ambiguous")
        self.assertEqual(set(ambiguous["candidates"]), {"team-a", "team-b"})

    def test_c10_shutdown_unregisters_team_clears_active_and_archives_runtime_dir_atomically(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state("current"))
        prepare_team_runtime_dir(self.workspace, "team-current-session")

        with patch("team_agent.runtime.check_team_owner", return_value=None):
            result = runtime.shutdown(self.workspace, keep_logs=True, team="current")

        self.assertTrue(result["ok"])
        state = read_runtime_state(self.workspace)
        self.assertNotIn("current", state.get("teams", {}))
        self.assertIsNone(state.get("active_team_key"))
        self.assertFalse((self.workspace / ".team" / "runtime" / "teams" / "team-current-session").exists())
        archives = list((self.workspace / ".team" / "runtime" / "teams").glob(".archived-team-current-session-*"))
        self.assertEqual(len(archives), 1)

    def test_c11_shutdown_keep_logs_keeps_logs_but_not_team_registration(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state("current"))
        prepare_team_runtime_dir(self.workspace, "team-current-session", log_name="worker.log")

        with patch("team_agent.runtime.check_team_owner", return_value=None):
            result = runtime.shutdown(self.workspace, keep_logs=True, team="current")

        self.assertTrue(result["ok"])
        state = read_runtime_state(self.workspace)
        self.assertNotIn("current", state.get("teams", {}))
        archives = list((self.workspace / ".team" / "runtime" / "teams").glob(".archived-team-current-session-*"))
        self.assertEqual(len(archives), 1)
        self.assertTrue((archives[0] / "logs" / "worker.log").exists())

    def test_c12_shutdown_cleanup_is_synchronous_not_background_async(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state("current"))
        prepare_team_runtime_dir(self.workspace, "team-current-session")

        with patch("team_agent.runtime.check_team_owner", return_value=None):
            result = runtime.shutdown(self.workspace, keep_logs=True, team="current")

        self.assertNotEqual(result.get("cleanup_mode"), "async")
        self.assertEqual(result.get("cleanup_mode"), "synchronous_committed")
        self.assertFalse((self.workspace / ".team" / "runtime" / "teams" / "team-current-session").exists())
        events = read_events(self.workspace)
        self.assertFalse(any(event.get("event") == "team.shutdown_cleanup_scheduled" for event in events))


if __name__ == "__main__":
    unittest.main(verbosity=2)
