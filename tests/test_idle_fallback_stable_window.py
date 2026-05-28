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


_TEAM = "team-stable-window"


def _setup_team(workspace: Path, *, last_output_ago_seconds: float) -> tuple[dict, MessageStore, EventLog]:
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
        last_output_at=(datetime.now(timezone.utc) - timedelta(seconds=last_output_ago_seconds)).isoformat(),
        owner_team_id=_TEAM,
    )
    return state, store, EventLog(workspace)


class IdleFallbackStableWindowTests(unittest.TestCase):
    def test_no_fire_while_within_stable_idle_window_then_fire_once_after_window(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stable-window-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace, last_output_ago_seconds=5.0)
            now = datetime.now(timezone.utc)
            delivered: list[dict] = []

            def fake_deliver(_workspace, leader_id, content, *_a, **_kw):
                delivered.append({"leader_id": leader_id, "content": content})
                return {"ok": True, "status": "submitted", "message_id": f"msg_{len(delivered)}"}

            with patch("team_agent.messaging.idle_alerts.deliver_stored_message", side_effect=fake_deliver):
                for offset in (0.0, 5.0, 10.0, 15.0, 20.0):
                    alerts = idle_alerts.detect_idle_fallbacks(
                        workspace, state, store, event_log, now=now + timedelta(seconds=offset)
                    )
                    self.assertEqual(alerts, [], f"expected no fire at offset={offset}s, got {alerts}")
                self.assertEqual(delivered, [], "no leader deliveries should happen inside stable-idle window")

                past_window = now + timedelta(seconds=idle_alerts.STABLE_IDLE_SECONDS + 1)
                alerts = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=past_window)

            self.assertEqual([a["alert_type"] for a in alerts], ["idle_fallback"])
            self.assertEqual(len(delivered), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
