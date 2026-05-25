from __future__ import annotations

import inspect
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import restart, runtime
from team_agent.events import EventLog
from team_agent.state import save_runtime_state


class RestartBoundaryTests(unittest.TestCase):
    """Pin runtime.py <-> restart/ contract by exercising actual behavior.

    Identity-only aliases let the 0a36ad9-style half-extraction slip past
    review, so every public restart surface here also gets a behavioral
    probe."""

    def test_runtime_aliases_resolve_to_restart_module(self) -> None:
        pairs = [
            (runtime.restart, restart.restart),
            (runtime._rollback_restart_session, restart.rollback_restart_session),
            (runtime._save_team_runtime_snapshot, restart.save_team_runtime_snapshot),
            (runtime._team_runtime_snapshot_dir, restart.team_runtime_snapshot_dir),
            (runtime._safe_snapshot_name, restart.safe_snapshot_name),
            (runtime._state_team_name, restart.state_team_name),
            (runtime._load_snapshot_state, restart.load_snapshot_state),
            (runtime._restart_candidates, restart.restart_candidates),
            (runtime._restart_candidate_from_state, restart.restart_candidate_from_state),
            (runtime._state_has_restart_context, restart.state_has_restart_context),
            (runtime._select_restart_state, restart.select_restart_state),
            (runtime._format_restart_candidates, restart.format_restart_candidates),
            (runtime._quick_start_existing_context, restart.quick_start_existing_context),
        ]
        for rt_attr, restart_attr in pairs:
            self.assertIs(rt_attr, restart_attr, f"{rt_attr.__name__} alias drift")

    def test_restart_helpers_have_explicit_signatures(self) -> None:
        for fn in (
            restart.restart,
            restart.rollback_restart_session,
            restart.save_team_runtime_snapshot,
            restart.team_runtime_snapshot_dir,
            restart.safe_snapshot_name,
            restart.state_team_name,
            restart.load_snapshot_state,
            restart.restart_candidates,
            restart.restart_candidate_from_state,
            restart.state_has_restart_context,
            restart.select_restart_state,
            restart.format_restart_candidates,
            restart.quick_start_existing_context,
        ):
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{fn.__name__} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{fn.__name__} uses **kwargs")

    def test_restart_modules_do_not_top_level_import_runtime(self) -> None:
        for module_name in (
            "team_agent.restart.snapshot",
            "team_agent.restart.selection",
            "team_agent.restart.orchestration",
            "team_agent.restart",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class SafeSnapshotNameProbeTests(unittest.TestCase):
    def test_keeps_safe_characters(self) -> None:
        self.assertEqual(restart.safe_snapshot_name("team-agent.alpha"), "team-agent.alpha")

    def test_replaces_path_separators_and_unsafe_chars(self) -> None:
        self.assertEqual(restart.safe_snapshot_name("team/with bad:chars"), "team_with_bad_chars")

    def test_strips_leading_and_trailing_punctuation(self) -> None:
        self.assertEqual(restart.safe_snapshot_name("..team-x.."), "team-x")

    def test_falls_back_to_team_when_nothing_safe(self) -> None:
        self.assertEqual(restart.safe_snapshot_name("///"), "team")


class TeamRuntimeSnapshotDirProbeTests(unittest.TestCase):
    def test_snapshot_dir_under_runtime_teams(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-dir-") as tmp:
            workspace = Path(tmp)
            path = restart.team_runtime_snapshot_dir(workspace, "team-foo")
        self.assertTrue(str(path).endswith("/runtime/teams/team-foo"))


class StateHasRestartContextProbeTests(unittest.TestCase):
    def test_empty_agents_returns_false(self) -> None:
        self.assertFalse(restart.state_has_restart_context({"agents": {}}))

    def test_session_id_marks_restartable(self) -> None:
        state = {"agents": {"alpha": {"session_id": "sess-1"}}}
        self.assertTrue(restart.state_has_restart_context(state))

    def test_rollout_path_marks_restartable(self) -> None:
        state = {"agents": {"alpha": {"rollout_path": "/tmp/x.jsonl"}}}
        self.assertTrue(restart.state_has_restart_context(state))

    def test_any_agent_entry_marks_restartable_even_without_session_metadata(self) -> None:
        state = {"agents": {"alpha": {"status": "stopped"}}}
        self.assertTrue(restart.state_has_restart_context(state))


class RestartCandidateFromStateProbeTests(unittest.TestCase):
    def test_summary_contains_session_agents_and_state_pointer(self) -> None:
        state = {
            "session_name": "team-alpha",
            "spec_path": "/tmp/missing.yaml",
            "agents": {"a": {"session_id": "s1"}, "b": {}},
        }
        out = restart.restart_candidate_from_state(state, Path("/tmp/state.json"))
        self.assertEqual(out["session_name"], "team-alpha")
        self.assertEqual(out["state_path"], "/tmp/state.json")
        self.assertEqual(out["agents"], ["a", "b"])
        self.assertTrue(out["has_context"])
        self.assertIs(out["state"], state)


class FormatRestartCandidatesProbeTests(unittest.TestCase):
    def test_empty_list_returns_helpful_message(self) -> None:
        self.assertIn("No restartable", restart.format_restart_candidates([]))

    def test_list_includes_session_and_agents(self) -> None:
        text = restart.format_restart_candidates([
            {"session_name": "team-a", "team_name": "alpha", "agents": ["worker_a"]},
        ])
        self.assertIn("team-a", text)
        self.assertIn("alpha", text)
        self.assertIn("worker_a", text)


class LoadSnapshotStateProbeTests(unittest.TestCase):
    def test_returns_none_for_missing_file(self) -> None:
        self.assertIsNone(restart.load_snapshot_state(Path("/nonexistent/state.json")))

    def test_returns_none_for_invalid_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-bad-json-") as tmp:
            bad = Path(tmp) / "state.json"
            bad.write_text("{bad", encoding="utf-8")
            self.assertIsNone(restart.load_snapshot_state(bad))

    def test_normalizes_session_metadata_on_load(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-good-json-") as tmp:
            good = Path(tmp) / "state.json"
            good.write_text(json.dumps({"session_name": "team-x", "agents": {}}), encoding="utf-8")
            state = restart.load_snapshot_state(good)
        self.assertEqual(state.get("session_name"), "team-x")


class SaveTeamRuntimeSnapshotProbeTests(unittest.TestCase):
    def test_skips_when_no_session_name(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-no-session-") as tmp:
            self.assertIsNone(restart.save_team_runtime_snapshot(Path(tmp), {"agents": {}}))

    def test_writes_snapshot_dir_with_state_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-save-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-save",
                "spec_path": "",
                "agents": {"alpha": {"status": "running"}},
            }
            out = restart.save_team_runtime_snapshot(workspace, state)
            self.assertIsNotNone(out)
            self.assertTrue(out.exists())
            on_disk = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual(on_disk["session_name"], "team-save")
            self.assertIn("team_snapshot", on_disk)
            self.assertEqual(on_disk["team_snapshot"]["session_name"], "team-save")


class RestartCandidatesProbeTests(unittest.TestCase):
    def test_returns_empty_for_fresh_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-cand-empty-") as tmp:
            self.assertEqual(restart.restart_candidates(Path(tmp)), [])

    def test_active_state_is_included_when_session_name_set(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-cand-active-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"session_name": "team-current", "agents": {"alpha": {"session_id": "s"}}})
            cands = restart.restart_candidates(workspace)
        self.assertEqual(len(cands), 1)
        self.assertEqual(cands[0]["session_name"], "team-current")
        self.assertTrue(cands[0]["has_context"])


class SelectRestartStateProbeTests(unittest.TestCase):
    def test_returns_load_runtime_state_when_no_candidates(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-select-empty-") as tmp:
            state = restart.select_restart_state(Path(tmp))
        self.assertEqual(state.get("agents", {}), {})

    def test_unknown_team_filter_raises_with_helpful_message(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-select-unknown-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"session_name": "team-current", "agents": {"a": {"session_id": "s"}}})
            with self.assertRaises(runtime.RuntimeError) as ctx:
                restart.select_restart_state(workspace, team="nonexistent")
        self.assertIn("not found", str(ctx.exception))

    def test_matching_team_returns_state_copy(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-select-match-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"session_name": "team-x", "agents": {"a": {"session_id": "s"}}})
            state = restart.select_restart_state(workspace, team="team-x")
        self.assertEqual(state["session_name"], "team-x")


class QuickStartExistingContextProbeTests(unittest.TestCase):
    def test_returns_none_for_unknown_session(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-qs-none-") as tmp:
            self.assertIsNone(restart.quick_start_existing_context(Path(tmp), "team-missing"))

    def test_returns_match_for_known_session_with_context(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-qs-found-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"session_name": "team-x", "agents": {"a": {"session_id": "s"}}})
            out = restart.quick_start_existing_context(workspace, "team-x")
        self.assertIsNotNone(out)
        self.assertEqual(out["session_name"], "team-x")


class RollbackRestartSessionProbeTests(unittest.TestCase):
    def test_reports_kill_session_outcome_and_writes_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-rollback-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            proc = Mock(returncode=0, stdout="", stderr="")
            with patch("team_agent.runtime.run_cmd", return_value=proc):
                result = restart.rollback_restart_session("team-rb", event_log)
            self.assertTrue(result["ok"])
            self.assertEqual(result["session"], "team-rb")
            log_path = workspace / ".team" / "logs" / "events.jsonl"
            self.assertTrue(log_path.exists(), "EventLog should have produced events.jsonl")
            lines = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines() if line]
            rollback_events = [evt for evt in lines if evt.get("event") == "restart.rollback_session"]
            self.assertEqual(len(rollback_events), 1)
            self.assertEqual(rollback_events[0]["session"], "team-rb")


if __name__ == "__main__":
    unittest.main()
