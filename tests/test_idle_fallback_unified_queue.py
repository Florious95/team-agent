from __future__ import annotations

import importlib.util
import unittest
from datetime import datetime, timedelta, timezone
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
from team_agent.messaging import scheduler
from team_agent.state import load_runtime_state, save_runtime_state


class IdleFallbackUnifiedQueueTests(unittest.TestCase):
    def test_undelivered_obligation_with_idle_workers_reminds_leader_via_receiver_path(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-idle-fallback-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace)
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "please continue")
            store.upsert_agent_health(
                "fake_impl",
                "idle",
                last_output_at=(datetime.now(timezone.utc) - timedelta(minutes=6)).isoformat(),
                owner_team_id="team-idle",
            )
            deliveries: list[dict] = []

            def fake_leader_receiver(_workspace, _state, leader_id, content, *_args, **_kwargs):
                deliveries.append({"leader_id": leader_id, "content": content})
                return {"ok": True, "status": "submitted", "message_id": "msg_idle_reminder"}

            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader_receiver):
                alerts = _detect_idle_fallbacks(workspace, state, store, event_log)

            self.assertEqual([alert["alert_type"] for alert in alerts], ["idle_fallback"])
            self.assertEqual(len(deliveries), 1)
            self.assertEqual(deliveries[0]["leader_id"], "leader")
            self.assertIn("unfinished work", deliveries[0]["content"])
            self.assertFalse(any(event.get("event") == "user-warning" for event in _events(workspace)))

    def test_failed_send_to_idle_worker_registers_cross_worker_deadlock_and_can_be_suppressed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-cross-worker-deadlock-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace)
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(minutes=6)).isoformat()
            store.upsert_agent_health("worker_a", "idle", last_output_at=old, owner_team_id="team-idle")
            message_id = store.create_message("task_impl", "leader", "worker_a", "do the thing", owner_team_id="team-idle")
            store.defer_delivery(message_id, "failed", "pane_mode_cancel_failed")

            alerts = _detect_cross_worker_deadlocks(workspace, state, store, event_log)

            self.assertEqual([alert["agent_id"] for alert in alerts], ["worker_a"])
            self.assertEqual(alerts[0]["alert_type"], "cross_worker_deadlock")
            listed = runtime.stuck_list(workspace)
            self.assertIn("team-idle", listed["suppressed_idle_alerts"])
            self.assertIn("worker_a", listed["suppressed_idle_alerts"]["team-idle"])
            self.assertIn("cross_worker_deadlock", listed["suppressed_idle_alerts"]["team-idle"]["worker_a"])

            suppressed = runtime.stuck_cancel(workspace, "worker_a", alert_type="cross_worker_deadlock")
            self.assertTrue(suppressed["ok"], suppressed)

    def test_unified_queue_preserves_independent_alert_type_subscriptions(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-unified-alert-queue-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace)
            save_runtime_state(workspace, state)

            stuck = runtime.stuck_cancel(workspace, "fake_impl", alert_type="stuck")
            deadlock = runtime.stuck_cancel(workspace, "fake_impl", alert_type="cross_worker_deadlock")
            self.assertTrue(stuck["ok"], stuck)
            self.assertTrue(deadlock["ok"], deadlock)
            listed = runtime.stuck_list(workspace)["suppressed_idle_alerts"]["fake_impl"]
            self.assertEqual(set(listed), {"stuck", "cross_worker_deadlock"})

            runtime.stuck_cancel(workspace, "fake_impl", alert_type="cross_worker_deadlock")
            listed_after = runtime.stuck_list(workspace)["suppressed_idle_alerts"]["fake_impl"]
            self.assertIn("stuck", listed_after)
            self.assertIn("cross_worker_deadlock", listed_after)


def _detect_idle_fallbacks(workspace: Path, state: dict, store: MessageStore, event_log: EventLog) -> list[dict]:
    try:
        detect = scheduler.detect_idle_fallbacks
    except AttributeError as exc:
        raise AssertionError(
            "Gap 8 requires scheduler.detect_idle_fallbacks(...) to remind the leader "
            "when all workers are idle but obligations remain undelivered"
        ) from exc
    return detect(workspace, state, store, event_log, now=datetime.now(timezone.utc))


def _detect_cross_worker_deadlocks(workspace: Path, state: dict, store: MessageStore, event_log: EventLog) -> list[dict]:
    try:
        detect = scheduler.detect_cross_worker_deadlocks
    except AttributeError as exc:
        raise AssertionError(
            "Gap 8b requires scheduler.detect_cross_worker_deadlocks(...) and a unified alert queue "
            "with alert_type='cross_worker_deadlock'"
        ) from exc
    return detect(workspace, state, store, event_log, now=datetime.now(timezone.utc))


def _state(workspace: Path) -> dict:
    spec = _fake_spec(workspace)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return {
        "spec_path": str(spec_path),
        "team_dir": str(workspace / ".team" / "team-idle"),
        "session_name": "team-idle",
        "leader": spec["leader"],
        "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%leader"},
        "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
        "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
