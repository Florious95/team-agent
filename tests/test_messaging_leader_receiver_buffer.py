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
            "team_owner": {"pane_id": "%1", "provider": provider, "leader_session_uuid": "uuid-buffer"},
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": provider,
                "pane_id": "%1",
                "session_name": "leader-session",
                "leader_session_uuid": "uuid-buffer",
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
        buffer_text = {"value": after_capture}

        def fake(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "set-buffer"]:
                buffer_text["value"] = args[-1]
            elif args[:2] == ["tmux", "paste-buffer"]:
                pasted["seen"] = True
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = buffer_text["value"] if pasted["seen"] else before_capture
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
            fake_run_cmd = self._run_cmd_fake("", after, calls)
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader_panes._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader._validate_leader_receiver", return_value={"ok": True, "pane": self._pane("codex"), "capture": ""}),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.messaging.tmux_io.time.sleep", return_value=None),
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
        self.assertIn(result["turn_verification"], {"leader_new_turn_boundary_verified", "not_yet_observed"})
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
            fake_run_cmd = self._run_cmd_fake(before, after, calls)
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader_panes._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader._validate_leader_receiver", return_value={"ok": True, "pane": self._pane("codex"), "capture": before}),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.messaging.tmux_io.time.sleep", return_value=None),
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

    def test_visible_without_turn_marker_is_delivered_not_requeued(self) -> None:
        # Gap 42: a payload that pasted and submitted but shows no new turn marker
        # (busy / compacting recipient) is a SUCCESSFUL delivery, not a failure. The
        # old behavior — fail at the turn-boundary gate and requeue the scheduled
        # notification once — is superseded by the send-busy-recipient contract:
        # submitted is authoritative, the missing marker is recorded as
        # turn_verification=not_yet_observed, and nothing is requeued.
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
            fake_run_cmd = self._run_cmd_fake("", after, calls)
            with (
                patch("team_agent.runtime._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader_panes._tmux_pane_info", return_value=self._pane("codex")),
                patch("team_agent.messaging.leader._validate_leader_receiver", return_value={"ok": True, "pane": self._pane("codex"), "capture": ""}),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.messaging.tmux_io.time.sleep", return_value=None),
            ):
                fired = runtime._fire_due_scheduled_events(workspace, store, event_log)
            events = [json.loads(line) for line in (workspace / ".team" / "logs" / "events.jsonl").read_text().splitlines()]
            pending_rows = [row for row in store.due_scheduled_events("9999-12-31T00:00:00+00:00") if row["status"] == "pending"]
        self.assertEqual(fired, [1])
        submitted = next(event for event in events if event["event"] == "leader_receiver.submitted")
        self.assertEqual(submitted["turn_verification"], "not_yet_observed")
        self.assertFalse(any(event["event"] == "leader_receiver.delivery_failed" for event in events))
        self.assertFalse(any(event["event"] == "coordinator.scheduled_retry" for event in events))
        self.assertEqual(pending_rows, [])


if __name__ == "__main__":
    unittest.main()
