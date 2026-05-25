from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


class LeaderReceiverBufferTests(unittest.TestCase):
    def _state(self, provider: str = "codex") -> dict:
        return {
            "leader": {"id": "leader"},
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": provider,
                "pane_id": "%1",
                "session_name": "leader-session",
            },
            "agents": {"worker": {"status": "running", "provider": "fake"}},
            "tasks": [{"id": "task_1", "title": "Task", "assignee": "worker", "status": "running"}],
            "session_name": None,
        }

    def _pane(self, provider: str = "codex") -> dict[str, str]:
        return {
            "pane_id": "%1",
            "session_name": "leader-session",
            "window_index": "0",
            "window_name": "leader",
            "pane_index": "0",
            "pane_tty": "/dev/ttys001",
            "pane_current_command": provider,
            "pane_current_path": "/tmp",
            "pane_active": "1",
            "window_active": "1",
        }

    def _write_spec(self, workspace: Path) -> None:
        spec = _fake_spec(workspace)
        spec["leader"]["id"] = "leader"
        spec["agents"][0]["id"] = "worker"
        spec["routing"]["rules"][0]["assign_to"] = "worker"
        spec["tasks"][0]["id"] = "task_1"
        spec["tasks"][0]["assignee"] = "worker"
        (workspace / "team.spec.yaml").write_text(dumps(spec), encoding="utf-8")

    def _run_cmd_fake(self, before_capture: str, after_capture: str, calls: list[list[str]]):
        pasted = {"seen": False}

        def fake(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                pasted["seen"] = True
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = after_capture if pasted["seen"] else before_capture
            return proc

        return fake

    def test_idle_leader_receiver_uses_per_message_buffer_and_deletes_it(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-buffer-idle-") as tmp:
            workspace = Path(tmp)
            self._write_spec(workspace)
            state = self._state(provider="codex")
            save_runtime_state(workspace, state)
            calls: list[list[str]] = []
            after = "❯ Team Agent message from worker for task_1:\nidle leader receiver payload arrived"
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.runtime.run_cmd", side_effect=self._run_cmd_fake("", after, calls)),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                result = runtime._send_to_leader_receiver(
                    workspace,
                    state,
                    "leader",
                    "idle leader receiver payload arrived",
                    "task_1",
                    "worker",
                    False,
                    EventLog(workspace),
                )
        self.assertTrue(result["ok"])
        self.assertTrue(result["visible"])
        self.assertEqual(result["turn_verification"], "leader_new_turn_boundary_verified")
        set_call = next(call for call in calls if call[:2] == ["tmux", "set-buffer"])
        buffer_name = set_call[3]
        self.assertTrue(buffer_name.startswith("team-agent-leader-receiver-msg_"))
        self.assertIn(["tmux", "paste-buffer", "-t", "%1", "-b", buffer_name, "-p"], calls)
        self.assertIn(["tmux", "delete-buffer", "-b", buffer_name], calls)

    def test_prompt_with_prior_content_still_uses_set_buffer_not_preexisting_prompt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-buffer-prior-") as tmp:
            workspace = Path(tmp)
            self._write_spec(workspace)
            state = self._state(provider="codex")
            save_runtime_state(workspace, state)
            calls: list[list[str]] = []
            before = "❯ unfinished prompt text from a prior leader turn"
            after = "❯ Team Agent message from worker for task_1:\nprior prompt payload now has a turn boundary"
            event_log = EventLog(workspace)
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.runtime.run_cmd", side_effect=self._run_cmd_fake(before, after, calls)),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                result = runtime._send_to_leader_receiver(
                    workspace,
                    state,
                    "leader",
                    "prior prompt payload now has a turn boundary",
                    "task_1",
                    "worker",
                    False,
                    event_log,
                )
            events = [json.loads(line) for line in (workspace / ".team" / "logs" / "events.jsonl").read_text().splitlines()]
        self.assertTrue(result["ok"])
        submitted = next(event for event in events if event["event"] == "leader_receiver.submitted")
        self.assertEqual(submitted["attempts"][0]["buffer_method"], "set_buffer_arg")
        self.assertNotEqual(submitted["attempts"][0]["buffer_method"], "preexisting_prompt")
        self.assertTrue(any(call[:2] == ["tmux", "paste-buffer"] for call in calls))

    def test_missing_new_turn_marker_requeues_scheduled_notification_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-buffer-retry-") as tmp:
            workspace = Path(tmp)
            self._write_spec(workspace)
            state = self._state(provider="codex")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            store.add_scheduled_event(
                "1970-01-01T00:00:00+00:00",
                "leader",
                "send",
                {
                    "content": "payload visible without a leader turn boundary",
                    "task_id": "task_1",
                    "sender": "worker",
                    "requires_ack": False,
                    "wait_visible": True,
                    "timeout": 30.0,
                    "max_attempts": 2,
                },
            )
            calls: list[list[str]] = []
            after = "payload visible without a leader turn boundary"
            event_log = EventLog(workspace)
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.runtime.run_cmd", side_effect=self._run_cmd_fake("", after, calls)),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                fired = runtime._fire_due_scheduled_events(workspace, store, event_log)
            events = [json.loads(line) for line in (workspace / ".team" / "logs" / "events.jsonl").read_text().splitlines()]
            scheduled_rows = store.due_scheduled_events("9999-12-31T00:00:00+00:00")
        self.assertEqual(fired, [1])
        self.assertTrue(any(event["event"] == "coordinator.scheduled_retry" for event in events))
        failed = next(event for event in events if event["event"] == "leader_receiver.delivery_failed")
        self.assertEqual(failed["stage"], "turn-boundary-verification")
        self.assertEqual(scheduled_rows[0]["status"], "pending")


if __name__ == "__main__":
    unittest.main()
