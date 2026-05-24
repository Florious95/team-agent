from __future__ import annotations

import importlib.util
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

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

from team_agent.messaging.scheduler import _detect_stuck_agents
from team_agent.events import EventLog


class MessagingSchedulerTests(unittest.TestCase):
    def _workspace_state(self, workspace: Path, task_status: str = "done") -> dict:
        spec = _fake_spec(workspace)
        spec["runtime"]["stuck_timeout_sec"] = 300
        spec_path = workspace / "team.spec.yaml"
        spec_path.write_text(dumps(spec), encoding="utf-8")
        task = {**spec["tasks"][0], "assignee": "fake_impl", "status": task_status}
        return {"spec_path": str(spec_path), "session_name": "session", "agents": {}, "tasks": [task]}

    def test_completed_idle_agent_does_not_emit_stuck_alert(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-idle-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            event_log.write("report_result.accepted", agent_id="fake_impl")

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, [])
            send.assert_not_called()
            events = _events(workspace)
            self.assertFalse(any(e["event"] == "coordinator.agent_stuck" for e in events))
            self.assertTrue(any(e["event"] == "coordinator.agent_stuck_suppressed" and e["reason"] == "idle_no_work" for e in events))

    def test_in_progress_task_without_recent_progress_emits_stuck_alert(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-task-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="in_progress")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)

            with patch("team_agent.runtime.send_message", return_value={"ok": True}) as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, ["fake_impl"])
            send.assert_called_once()
            stuck_event = next(e for e in _events(workspace) if e["event"] == "coordinator.agent_stuck")
            self.assertEqual(stuck_event["work_reason"], "active_task")

    def test_inbound_unconsumed_message_counts_as_stuck_relevant_work(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-inbound-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            store.create_message(None, "leader", "fake_impl", "unconsumed")

            with patch("team_agent.runtime.send_message", return_value={"ok": True}) as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, ["fake_impl"])
            send.assert_called_once()
            stuck_event = next(e for e in _events(workspace) if e["event"] == "coordinator.agent_stuck")
            self.assertEqual(stuck_event["work_reason"], "inbound_message")

    def test_recent_progress_suppresses_stuck_alert_even_with_active_task(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-progress-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="in_progress")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            event_log.write("send.submitted", target="fake_impl")

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, [])
            send.assert_not_called()
            self.assertTrue(any(e["event"] == "coordinator.agent_stuck_suppressed" and e["reason"] == "recent_progress_event" for e in _events(workspace)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
