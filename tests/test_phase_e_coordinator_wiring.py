from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

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

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging import activity_detector


class CoordinatorTickWiresPhaseEDetectorsTests(unittest.TestCase):
    def test_coordinator_tick_invokes_idle_fallback_and_cross_worker_deadlock_detectors(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-phase-e-wiring-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}],
                },
            )
            calls: dict[str, int] = {"idle": 0, "deadlock": 0, "compaction": 0}

            def fake_idle(_workspace, _state, _store, _event_log, now=None):
                calls["idle"] += 1
                return [{"alert_type": "idle_fallback", "agent_id": "fake_impl"}]

            def fake_deadlock(_workspace, _state, _store, _event_log, now=None):
                calls["deadlock"] += 1
                return [{"alert_type": "cross_worker_deadlock", "agent_id": "fake_impl"}]

            def fake_compaction(_workspace, _state, _event_log, *, agent_id, provider, scrollback, stuck_loop=False):
                calls["compaction"] += 1
                return {"ok": True, "event": "compaction_threshold_crossed.below_threshold", "agent_id": agent_id, "compaction_count": 0}

            with (
                patch("team_agent.messaging.idle_alerts.detect_idle_fallbacks", side_effect=fake_idle),
                patch("team_agent.messaging.idle_alerts.detect_cross_worker_deadlocks", side_effect=fake_deadlock),
                patch("team_agent.messaging.activity_detector.detect_compaction_degradation", side_effect=fake_compaction),
            ):
                result = runtime.coordinator_tick(workspace)

            self.assertTrue(result.get("ok"))
            self.assertEqual(calls["idle"], 1, "detect_idle_fallbacks must be invoked from coordinator_tick")
            self.assertEqual(calls["deadlock"], 1, "detect_cross_worker_deadlocks must be invoked from coordinator_tick")
            self.assertEqual(result.get("idle_alerts"), [{"alert_type": "idle_fallback", "agent_id": "fake_impl"}])
            self.assertEqual(result.get("deadlock_alerts"), [{"alert_type": "cross_worker_deadlock", "agent_id": "fake_impl"}])

    def test_compaction_counter_resets_to_zero_after_successful_auto_reset(self) -> None:
        from team_agent.state import team_state_key
        with tempfile.TemporaryDirectory(prefix="team-agent-compaction-reset-counter-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            state = {
                "spec_path": str(spec_path),
                "session_name": "team-agent-test",
                "leader": spec["leader"],
                "agents": {"codex_worker": {"status": "running", "provider": "codex", "window": "codex_worker"}},
                "tasks": [],
            }
            owner_team_id = team_state_key(state)
            state["coordinator"] = {"compaction_counts": {owner_team_id: {"codex_worker": 2}}}
            save_runtime_state(workspace, state)
            event_log = EventLog(workspace)
            scrollback = "context compacted\ncontext compacted\ncontext compacted\n"
            with patch("team_agent.runtime.reset_agent", return_value={"ok": True, "agent_id": "codex_worker", "status": "running"}):
                result = activity_detector.detect_compaction_degradation(
                    workspace,
                    state,
                    event_log,
                    agent_id="codex_worker",
                    provider="codex",
                    scrollback=scrollback,
                    stuck_loop=False,
                )

            self.assertEqual(result["event"], "compaction_threshold_crossed.auto_reset", result)
            persisted = load_runtime_state(workspace)
            counts = persisted.get("coordinator", {}).get("compaction_counts", {}).get(owner_team_id, {})
            self.assertEqual(counts.get("codex_worker"), 0, "compaction count must reset after successful auto-reset to avoid loop")


if __name__ == "__main__":
    unittest.main(verbosity=2)
