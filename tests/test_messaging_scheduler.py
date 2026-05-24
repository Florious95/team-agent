from __future__ import annotations

import io
import importlib.util
import unittest
from contextlib import redirect_stdout
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
from team_agent import cli
from team_agent import runtime
from team_agent.mcp_server.tools import TeamOrchestratorTools
from team_agent.state import load_runtime_state, save_runtime_state


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

    def test_delivered_message_history_does_not_count_as_inbound_work(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-delivered-history-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            for index, status in enumerate(("submitted", "visible", "delivered")):
                message_id = store.create_message(None, f"sender-{index}", "fake_impl", f"cycle probe {index}")
                store.mark(message_id, status)

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, [])
            send.assert_not_called()
            events = _events(workspace)
            self.assertFalse(any(e["event"] == "coordinator.agent_stuck" for e in events))
            self.assertTrue(any(e["event"] == "coordinator.agent_stuck_suppressed" and e["reason"] == "idle_no_work" for e in events))

    def test_accepted_message_still_counts_as_pre_delivery_work(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-accepted-message-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            store.create_message(None, "leader", "fake_impl", "not delivered yet")

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

    def test_manual_suppression_blocks_stuck_event_and_leader_push(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-cancel-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="in_progress")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            runtime.stuck_cancel(workspace, "fake_impl", suppressed_by="leader")
            state = load_runtime_state(workspace)

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, [])
            send.assert_not_called()
            self.assertFalse(any(e["event"] == "coordinator.agent_stuck" for e in _events(workspace)))

    def test_suppression_auto_clears_when_new_task_is_assigned(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-clear-task-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            runtime.stuck_cancel(workspace, "fake_impl", suppressed_by="leader")
            state = load_runtime_state(workspace)
            state["tasks"].append({"id": "task-new", "title": "new", "assignee": "fake_impl", "status": "in_progress"})

            with patch("team_agent.runtime.send_message", return_value={"ok": True}) as send:
                stuck = _detect_stuck_agents(workspace, state, store, EventLog(workspace))

            self.assertEqual(stuck, ["fake_impl"])
            send.assert_called_once()
            self.assertNotIn("fake_impl", state["coordinator"].get("suppressed_idle_alerts", {}))

    def test_suppression_auto_clears_on_outbound_progress_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-clear-progress-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="in_progress")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            runtime.stuck_cancel(workspace, "fake_impl", suppressed_by="leader")
            event_log.write("send.submitted", target="fake_impl")
            state = load_runtime_state(workspace)

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, [])
            send.assert_not_called()
            self.assertNotIn("fake_impl", state["coordinator"].get("suppressed_idle_alerts", {}))

    def test_suppression_auto_clears_when_inbound_message_becomes_delivered(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-clear-inbound-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="done")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            message_id = store.create_message(None, "leader", "fake_impl", "probe")
            runtime.stuck_cancel(workspace, "fake_impl", suppressed_by="leader")
            store.mark(message_id, "submitted")
            state = load_runtime_state(workspace)

            with patch("team_agent.runtime.send_message") as send:
                stuck = _detect_stuck_agents(workspace, state, store, EventLog(workspace))

            self.assertEqual(stuck, [])
            send.assert_not_called()
            self.assertNotIn("fake_impl", state["coordinator"].get("suppressed_idle_alerts", {}))

    def test_suppression_auto_clears_on_restart_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-clear-restart-") as tmp:
            workspace = Path(tmp)
            state = self._workspace_state(workspace, task_status="in_progress")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            event_log = EventLog(workspace)
            old = (datetime.now(timezone.utc) - timedelta(seconds=600)).isoformat()
            store.upsert_agent_health("fake_impl", "RUNNING", last_output_at=old)
            runtime.stuck_cancel(workspace, "fake_impl", suppressed_by="leader")
            event_log.write("reset_agent.complete", agent_id="fake_impl")
            state = load_runtime_state(workspace)

            with patch("team_agent.runtime.send_message", return_value={"ok": True}) as send:
                stuck = _detect_stuck_agents(workspace, state, store, event_log)

            self.assertEqual(stuck, ["fake_impl"])
            send.assert_called_once()
            self.assertNotIn("fake_impl", state["coordinator"].get("suppressed_idle_alerts", {}))

    def test_suppression_is_workspace_scoped_and_persists_in_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-team-a-") as a, tempfile.TemporaryDirectory(prefix="team-agent-stuck-team-b-") as b:
            workspace_a = Path(a)
            workspace_b = Path(b)
            save_runtime_state(workspace_a, self._workspace_state(workspace_a, task_status="done"))
            save_runtime_state(workspace_b, self._workspace_state(workspace_b, task_status="done"))
            runtime.stuck_cancel(workspace_a, "fake_impl", suppressed_by="leader")

            self.assertIn("fake_impl", runtime.stuck_list(workspace_a)["suppressed_idle_alerts"])
            self.assertNotIn("fake_impl", runtime.stuck_list(workspace_b)["suppressed_idle_alerts"])
            self.assertIn("fake_impl", load_runtime_state(workspace_a)["coordinator"]["suppressed_idle_alerts"])

    def test_cli_stuck_cancel_and_list_roundtrip(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-cli-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, self._workspace_state(workspace, task_status="done"))
            with redirect_stdout(io.StringIO()):
                cli.main(["stuck-cancel", "fake_impl", "--workspace", str(workspace), "--json"])

            stdout = io.StringIO()
            with redirect_stdout(stdout):
                cli.main(["stuck-list", "--workspace", str(workspace), "--json"])

            payload = json.loads(stdout.getvalue())
            self.assertIn("fake_impl", payload["suppressed_idle_alerts"])
            self.assertIn("stuck", payload["suppressed_idle_alerts"]["fake_impl"])

    def test_mcp_stuck_tools_roundtrip(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stuck-mcp-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, self._workspace_state(workspace, task_status="done"))
            result = TeamOrchestratorTools(workspace).stuck_cancel("fake_impl")

            self.assertTrue(result["ok"])
            listed = TeamOrchestratorTools(workspace).stuck_list()
            self.assertIn("fake_impl", listed["suppressed_idle_alerts"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
