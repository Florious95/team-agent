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

from team_agent.events import EventLog
from team_agent.messaging import idle_alerts
from team_agent.state import save_runtime_state


_TEAM = "team-in-flight"


def _setup_team_no_active_task(workspace: Path) -> tuple[dict, MessageStore, EventLog]:
    spec = _fake_spec(workspace)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    state = {
        "spec_path": str(spec_path),
        "team_dir": str(workspace / ".team" / _TEAM),
        "session_name": _TEAM,
        "leader": spec["leader"],
        "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%leader"},
        "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
        "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "done"}],
    }
    save_runtime_state(workspace, state)
    store = MessageStore(workspace)
    store.upsert_agent_health(
        "fake_impl",
        "idle",
        last_output_at=(datetime.now(timezone.utc) - timedelta(minutes=30)).isoformat(),
        owner_team_id=_TEAM,
    )
    return state, store, EventLog(workspace)


class IdleFallbackInFlightObligationTests(unittest.TestCase):
    def test_fresh_pending_message_under_min_age_is_not_an_obligation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-in-flight-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team_no_active_task(workspace)
            # In-flight pending message just created (well under OBLIGATION_PENDING_MIN_AGE_SECONDS).
            store.create_message("task_impl", "leader", "fake_impl", "fresh ping", owner_team_id=_TEAM)
            delivered: list[dict] = []

            def fake_deliver(_workspace, leader_id, content, *_a, **_kw):
                delivered.append({"leader_id": leader_id, "content": content})
                return {"ok": True, "status": "submitted", "message_id": "msg_x"}

            with patch("team_agent.messaging.idle_alerts.deliver_stored_message", side_effect=fake_deliver):
                alerts = idle_alerts.detect_idle_fallbacks(
                    workspace, state, store, event_log,
                    now=datetime.now(timezone.utc),
                )

            self.assertEqual(alerts, [], "in-flight (fresh) pending message must not count as obligation")
            self.assertEqual(delivered, [])

    def test_aged_pending_message_does_count_as_obligation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-in-flight-aged-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team_no_active_task(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "old ping", owner_team_id=_TEAM)
            future_now = datetime.now(timezone.utc) + timedelta(seconds=idle_alerts.OBLIGATION_PENDING_MIN_AGE_SECONDS + 5)
            delivered: list[dict] = []

            def fake_deliver(_workspace, leader_id, content, *_a, **_kw):
                delivered.append({"leader_id": leader_id, "content": content})
                return {"ok": True, "status": "submitted", "message_id": "msg_y"}

            with patch("team_agent.messaging.idle_alerts.deliver_stored_message", side_effect=fake_deliver):
                alerts = idle_alerts.detect_idle_fallbacks(
                    workspace, state, store, event_log, now=future_now,
                )

            self.assertEqual([a["alert_type"] for a in alerts], ["idle_fallback"])
            self.assertEqual(len(delivered), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
