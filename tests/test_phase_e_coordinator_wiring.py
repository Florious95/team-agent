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

            def fake_idle(nodes, *, monitor_state=None, now_monotonic=0.0, debounce_seconds=60.0, suspend_intervals=None, event_sink=None):
                # Gap 32: coordinator_tick now drives the file-fact idle/takeover
                # predicate, not the retired screen-scrape detect_idle_fallbacks.
                calls["idle"] += 1
                return {"should_ping": False, "message": None, "reason": "debounce_active",
                        "annotations": [], "interrupted_nodes": [], "monitor_state": dict(monitor_state or {})}

            def fake_deadlock(_workspace, _state, _store, _event_log, now=None):
                calls["deadlock"] += 1
                return [{"alert_type": "cross_worker_deadlock", "agent_id": "fake_impl"}]

            def fake_compaction(_workspace, _state, _event_log, *, agent_id, provider, scrollback, stuck_loop=False):
                calls["compaction"] += 1
                return {"ok": True, "event": "compaction_threshold_crossed.below_threshold", "agent_id": agent_id, "compaction_count": 0}

            with (
                patch("team_agent.idle_predicate.evaluate_takeover_reminder", side_effect=fake_idle),
                patch("team_agent.messaging.idle_alerts.detect_cross_worker_deadlocks", side_effect=fake_deadlock),
                patch("team_agent.messaging.activity_detector.detect_compaction_degradation", side_effect=fake_compaction),
            ):
                result = runtime.coordinator_tick(workspace)

            self.assertTrue(result.get("ok"))
            self.assertEqual(calls["idle"], 1, "the idle/takeover predicate must be invoked from coordinator_tick")
            self.assertEqual(calls["deadlock"], 1, "detect_cross_worker_deadlocks must be invoked from coordinator_tick")
            self.assertEqual(result.get("idle_alerts"), [])
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


    def test_codex_working_pane_surfaces_working_status_via_status_json(self) -> None:
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory(prefix="team-agent-status-working-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            session_name = "team-status-working"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": session_name,
                    "leader": spec["leader"],
                    "agents": {
                        "worker_a": {
                            "status": "busy",
                            "provider": "codex",
                            "window": "worker_a",
                            "tmux_window_present": True,
                        },
                    },
                    "tasks": [{**spec["tasks"][0], "assignee": "worker_a", "status": "running"}],
                },
            )
            codex_working_scrollback = (
                "[worker_a] doing useful things\n"
                "✱ Working (12s) ⠋\n"
                "  ↳ esc to interrupt\n"
            )

            def fake_run_cmd(args, timeout=5):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "capture-pane"]:
                    proc.stdout = codex_working_scrollback
                return proc

            fake_pane_info = {
                "pane_id": "%42",
                "session_name": session_name,
                "window_index": "0",
                "window_name": "worker_a",
                "pane_index": "0",
                "pane_tty": "/dev/ttys001",
                "pane_current_command": "node",
                "pane_active": "1",
            }

            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._tmux_pane_info", return_value=fake_pane_info),
                patch("team_agent.runtime._detect_provider_status", return_value=None),
                patch("team_agent.runtime.coordinator_health", return_value={"ok": True, "schema_ok": True, "status": "running", "pid": 1}),
                patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
                patch("team_agent.runtime._handle_provider_startup_prompts", return_value=None),
            ):
                payload = runtime.status(workspace, as_json=True)

            agent_health = payload["agent_health"].get("worker_a") or {}
            self.assertEqual(
                agent_health.get("status"),
                "WORKING",
                f"Codex 'Working (Xs)' scrollback must surface as WORKING in agent_health (got {agent_health.get('status')!r}); full row: {agent_health!r}",
            )
            agent = payload["agents"].get("worker_a") or {}
            activity = agent.get("activity") or {}
            self.assertEqual(activity.get("status"), "working", f"agent_state.activity must expose lowercase classifier enum: {activity!r}")
            self.assertGreaterEqual(activity.get("confidence", 0), 0.85, f"Codex 'Working (Xs)' should produce high-confidence classification: {activity!r}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
