from __future__ import annotations

import importlib.util
import io
import json
import unittest
from contextlib import redirect_stdout
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

from team_agent import cli
from team_agent.coordinator.lifecycle import coordinator_tick
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


class AcknowledgeIdleCliSuppressesReminderTests(unittest.TestCase):
    def test_acknowledge_idle_survives_progress_events_and_blocks_next_tick_reminder(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-ack-idle-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace)
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            old = (datetime.now(timezone.utc) - timedelta(minutes=6)).isoformat()
            store.upsert_agent_health("fake_impl", "idle", last_output_at=old, owner_team_id="team-idle")
            store.create_message("task_impl", "leader", "fake_impl", "please continue", owner_team_id="team-idle")

            stdout = io.StringIO()
            with redirect_stdout(stdout):
                cli.main(["acknowledge-idle", "--workspace", str(workspace), "--json"])
            payload = json.loads(stdout.getvalue())
            self.assertTrue(payload["ok"], payload)
            self.assertEqual(payload["team"], "team-idle")

            event_log = EventLog(workspace)
            for _ in range(3):
                event_log.write("send.submitted", target="fake_impl")

            deliveries: list[dict] = []

            def fake_leader_receiver(_workspace, _state, leader_id, content, *_args, **_kwargs):
                deliveries.append({"leader_id": leader_id, "content": content})
                return {"ok": True, "status": "submitted", "message_id": "msg_idle_reminder"}

            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
                patch("team_agent.runtime._capture_missing_sessions", return_value=None),
                patch("team_agent.runtime._refresh_agent_runtime_statuses", return_value=None),
                patch("team_agent.runtime._handle_provider_startup_prompts", return_value=None),
                patch("team_agent.runtime._handle_provider_runtime_prompts", return_value=None),
                patch("team_agent.runtime._sync_agent_health", return_value={}),
                patch("team_agent.runtime._deliver_pending_messages", return_value=[]),
                patch("team_agent.runtime._fire_due_scheduled_events", return_value=[]),
                patch("team_agent.runtime._detect_stuck_agents", return_value=[]),
                patch("team_agent.runtime._collect_results_and_notify_watchers", return_value={"collected": 0, "notified": 0}),
                patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader_receiver),
            ):
                result = coordinator_tick(workspace)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["idle_alerts"], [])
            self.assertEqual(deliveries, [])


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
