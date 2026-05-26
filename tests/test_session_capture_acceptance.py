from __future__ import annotations

import json
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any, Callable
from unittest.mock import patch

from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.events import EventLog
from team_agent.sessions.capture import capture_agent_session, capture_missing_sessions


AGENT_IDS = ["worker_a", "worker_b", "worker_c", "worker_d", "worker_e", "worker_f"]


class SessionCaptureAcceptanceTests(unittest.TestCase):
    def test_1_immediate_session_id_captures_and_emits_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-capture-immediate-") as tmp:
            workspace = Path(tmp)
            sessions = workspace / "sessions"
            _write_session_fixture(sessions, "worker_a")
            adapter = FileBackedSessionAdapter(sessions)
            agent_state = _agent_state(workspace, "worker_a")

            with patch("team_agent.sessions.capture.get_adapter", return_value=adapter):
                result = capture_agent_session(
                    workspace,
                    "worker_a",
                    agent_state,
                    EventLog(workspace),
                    timeout_s=1.0,
                )

            self.assertIsNotNone(result)
            self.assertEqual(result["session_id"], "session-worker_a")
            self.assertEqual(agent_state["session_id"], "session-worker_a")
            events = _events(workspace)
            self.assertEqual([event["event"] for event in events], ["session.captured"])
            self.assertEqual(events[0]["agent_id"], "worker_a")
            self.assertEqual(events[0]["session_id"], "session-worker_a")

    def test_2_slow_startup_session_id_is_retried_and_captured(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-capture-slow-") as tmp:
            workspace = Path(tmp)
            sessions = workspace / "sessions"
            writer = _write_session_fixture_later(sessions, "worker_a", delay_s=0.5)
            adapter = FileBackedSessionAdapter(sessions)
            agent_state = _agent_state(workspace, "worker_a")

            try:
                with patch("team_agent.sessions.capture.get_adapter", return_value=adapter):
                    result = capture_agent_session(
                        workspace,
                        "worker_a",
                        agent_state,
                        EventLog(workspace),
                        timeout_s=2.0,
                    )
            finally:
                writer.join(timeout=2.0)

            self.assertIsNotNone(result, _diagnostic(workspace, adapter, agent_state))
            self.assertEqual(result["session_id"], "session-worker_a")
            self.assertEqual(agent_state["session_id"], "session-worker_a")
            self.assertGreaterEqual(adapter.calls["worker_a"], 2)
            self.assertEqual(_event_names(workspace), ["session.captured"])

    def test_3_missing_session_id_fails_loudly_instead_of_silent_null(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-capture-missing-") as tmp:
            workspace = Path(tmp)
            adapter = FileBackedSessionAdapter(workspace / "empty-sessions")
            agent_state = _agent_state(workspace, "worker_a", status="running")

            with patch("team_agent.sessions.capture.get_adapter", return_value=adapter):
                _assert_loud_capture_failure(
                    self,
                    workspace,
                    "worker_a",
                    lambda: capture_agent_session(
                        workspace,
                        "worker_a",
                        agent_state,
                        EventLog(workspace),
                        timeout_s=0.1,
                    ),
                )

    def test_4_six_workers_with_two_slow_startups_all_eventually_capture(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-capture-fanout-") as tmp:
            workspace = Path(tmp)
            sessions = workspace / "sessions"
            for agent_id in AGENT_IDS[:4]:
                _write_session_fixture(sessions, agent_id)
            writers = [
                _write_session_fixture_later(sessions, "worker_e", delay_s=0.5),
                _write_session_fixture_later(sessions, "worker_f", delay_s=0.5),
            ]
            adapter = FileBackedSessionAdapter(sessions)
            state = {"agents": {agent_id: _agent_state(workspace, agent_id) for agent_id in AGENT_IDS}}

            try:
                with patch("team_agent.sessions.capture.get_adapter", return_value=adapter):
                    captured = capture_missing_sessions(
                        workspace,
                        state,
                        EventLog(workspace),
                        timeout_s=2.0,
                    )
            finally:
                for writer in writers:
                    writer.join(timeout=2.0)

            self.assertCountEqual(captured, AGENT_IDS)
            self.assertEqual(
                {agent_id: state["agents"][agent_id]["session_id"] for agent_id in AGENT_IDS},
                {agent_id: f"session-{agent_id}" for agent_id in AGENT_IDS},
            )
            events = _events(workspace)
            self.assertEqual([event["event"] for event in events], ["session.captured"] * 6)
            self.assertCountEqual([event["agent_id"] for event in events], AGENT_IDS)

    def test_5_capture_timeout_cannot_leave_running_worker_with_null_session(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-capture-timeout-") as tmp:
            workspace = Path(tmp)
            adapter = FileBackedSessionAdapter(workspace / "empty-sessions")
            agent_state = _agent_state(workspace, "worker_a", status="running")

            with patch("team_agent.sessions.capture.get_adapter", return_value=adapter):
                raised = _assert_loud_capture_failure(
                    self,
                    workspace,
                    "worker_a",
                    lambda: capture_agent_session(
                        workspace,
                        "worker_a",
                        agent_state,
                        EventLog(workspace),
                        timeout_s=0.1,
                    ),
                )

            if not raised:
                self.assertFalse(
                    agent_state.get("status") == "running" and not agent_state.get("session_id"),
                    _diagnostic(workspace, adapter, agent_state),
                )


class FileBackedSessionAdapter:
    def __init__(self, sessions_dir: Path):
        self.sessions_dir = sessions_dir
        self.calls: dict[str, int] = {}

    def capture_session_id(
        self,
        agent_id: str,
        spawn_context: dict[str, Any],
        timeout_s: float = 3.0,
    ) -> dict[str, Any] | None:
        _ = spawn_context, timeout_s
        self.calls[agent_id] = self.calls.get(agent_id, 0) + 1
        session_file = self.sessions_dir / f"{agent_id}.json"
        if not session_file.exists():
            return None
        return json.loads(session_file.read_text(encoding="utf-8"))


def _agent_state(workspace: Path, agent_id: str, status: str | None = None) -> dict[str, Any]:
    state = {
        "provider": "codex",
        "session_name": "team-session-capture",
        "window": agent_id,
        "spawn_cwd": str(workspace),
        "spawned_at": "2026-05-26T00:00:00+00:00",
        "session_id": None,
        "rollout_path": None,
        "captured_at": None,
        "captured_via": None,
        "attribution_confidence": None,
    }
    if status is not None:
        state["status"] = status
    return state


def _write_session_fixture(sessions_dir: Path, agent_id: str) -> None:
    sessions_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "session_id": f"session-{agent_id}",
        "rollout_path": str(sessions_dir / f"{agent_id}.jsonl"),
        "captured_at": "2026-05-26T00:00:01+00:00",
        "captured_via": "fs_watch",
        "attribution_confidence": "high",
        "spawn_cwd": str(sessions_dir.parent),
    }
    (sessions_dir / f"{agent_id}.json").write_text(
        json.dumps(payload, sort_keys=True),
        encoding="utf-8",
    )


def _write_session_fixture_later(sessions_dir: Path, agent_id: str, delay_s: float) -> threading.Thread:
    def worker() -> None:
        time.sleep(delay_s)
        _write_session_fixture(sessions_dir, agent_id)

    thread = threading.Thread(target=worker, daemon=True)
    thread.start()
    return thread


def _assert_loud_capture_failure(
    case: unittest.TestCase,
    workspace: Path,
    agent_id: str,
    action: Callable[[], dict[str, Any] | None],
) -> bool:
    try:
        result = action()
    except TeamAgentRuntimeError as exc:
        case.assertIn(agent_id, str(exc))
        return True

    events = _events(workspace)
    attention_events = [
        event
        for event in events
        if event.get("event") == "session.capture_required_attention" and event.get("agent_id") == agent_id
    ]
    case.assertTrue(
        attention_events,
        f"session capture miss must raise TeamAgentRuntimeError or emit "
        f"session.capture_required_attention; result={result!r}; events={events!r}",
    )
    case.assertIsInstance(result, dict)
    case.assertEqual(result.get("ok"), False)
    case.assertIn(agent_id, json.dumps(result, sort_keys=True))
    return False


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def _event_names(workspace: Path) -> list[str]:
    return [event["event"] for event in _events(workspace)]


def _diagnostic(workspace: Path, adapter: FileBackedSessionAdapter, agent_state: dict[str, Any]) -> str:
    return (
        f"calls={adapter.calls!r} "
        f"agent_state={agent_state!r} "
        f"events={_events(workspace)!r}"
    )


if __name__ == "__main__":
    unittest.main(verbosity=2)
