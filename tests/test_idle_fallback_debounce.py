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


_TEAM = "team-debounce"


def _setup_team(workspace: Path) -> tuple[dict, MessageStore, EventLog]:
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
        "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
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


class IdleFallbackDebounceTests(unittest.TestCase):
    def test_second_fire_blocked_within_debounce_then_allowed_after(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-debounce-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace)
            base_now = datetime.now(timezone.utc)
            delivered: list[dict] = []

            def fake_deliver(_workspace, leader_id, content, *_a, **_kw):
                delivered.append({"leader_id": leader_id, "content": content, "ts": "fake"})
                return {"ok": True, "status": "submitted", "message_id": f"msg_{len(delivered)}"}

            with patch("team_agent.messaging.idle_alerts.deliver_stored_message", side_effect=fake_deliver):
                first = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=base_now)
                self.assertEqual([a["alert_type"] for a in first], ["idle_fallback"])
                self.assertEqual(len(delivered), 1)

                # Simulate suppression auto-clear via stuck_cancel-equivalent state mutation:
                coordinator = state.setdefault("coordinator", {})
                coordinator.setdefault("suppressed_idle_alerts", {}).pop(_TEAM, None)
                save_runtime_state(workspace, state)
                state = load_runtime_state(workspace)

                # Within debounce window — must NOT re-fire.
                for offset_seconds in (5, 30, 120, idle_alerts.FIRE_DEBOUNCE_SECONDS - 1):
                    alerts = idle_alerts.detect_idle_fallbacks(
                        workspace, state, store, event_log,
                        now=base_now + timedelta(seconds=offset_seconds),
                    )
                    self.assertEqual(alerts, [], f"unexpected fire at offset={offset_seconds}s")
                self.assertEqual(len(delivered), 1, "exactly one delivery inside debounce window")

                past_debounce = base_now + timedelta(seconds=idle_alerts.FIRE_DEBOUNCE_SECONDS + 1)
                second = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=past_debounce)

            self.assertEqual([a["alert_type"] for a in second], ["idle_fallback"])
            self.assertEqual(len(delivered), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
