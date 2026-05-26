"""Gap 28 (Slice 2 Stage 2): leader.api_error structured-event emission.

Mocks the leader-pane scrollback capture and asserts:
  - the right error_class is emitted for each pattern
  - schema fields are populated (leader_session_uuid, provider, partial flag,
    worker_dispatch_just_before, retry_count=0, matched_pattern_snippet)
  - no event is emitted for clean scrollback
  - dedupe: re-running the detector on identical scrollback emits at most one event
    until the scrollback goes clean
  - worker_dispatch_just_before reflects leader→worker sends inside the lookback
    window and excludes older ones
"""
from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging.leader_api_errors import detect_leader_api_errors


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base_gap28", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


_LEADER_UUID = "leadersession_uuid_for_gap28_tests"


def _state_with_attached_leader() -> dict[str, Any]:
    return {
        "team_owner": {
            "leader_session_uuid": _LEADER_UUID,
            "pane_id": "%leader",
            "provider": "claude",
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "pane_id": "%leader",
            "provider": "claude",
            "leader_session_uuid": _LEADER_UUID,
        },
    }


def _make_capture(text: str):
    def _cap(target: str) -> dict[str, Any]:
        return {"ok": True, "capture": text}
    return _cap


def _ts(now: datetime, *, seconds_ago: float) -> str:
    return (now - timedelta(seconds=seconds_ago)).isoformat()


class Gap28DetectionTests(unittest.TestCase):

    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="gap28-")
        self.workspace = Path(self._tmp_ctx.name)
        (self.workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
        self.store = MessageStore(self.workspace)
        self.event_log = EventLog(self.workspace)

    def tearDown(self) -> None:
        self._tmp_ctx.cleanup()

    def _emitted_events(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def _emitted_api_errors(self) -> list[dict[str, Any]]:
        return [ev for ev in self._emitted_events() if ev.get("event") == "leader.api_error"]

    def test_no_event_when_scrollback_is_clean(self) -> None:
        state = _state_with_attached_leader()
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture("> idle prompt\n\n● Assistant: ready\n"),
        )
        self.assertEqual(events, [])
        self.assertEqual(self._emitted_api_errors(), [])

    def test_overloaded_pattern_emits_overloaded_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "● Thinking…\nAPI Error: Overloaded — please try again later\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        emitted = self._emitted_api_errors()
        self.assertEqual(len(emitted), 1)
        ev = emitted[0]
        self.assertEqual(ev["error_class"], "Overloaded")
        self.assertEqual(ev["leader_session_uuid"], _LEADER_UUID)
        self.assertEqual(ev["provider"], "claude")
        self.assertEqual(ev["retry_count"], 0)
        self.assertIn("Overloaded", ev["matched_pattern_snippet"])
        self.assertIsInstance(ev["worker_dispatch_just_before"], list)
        self.assertIsInstance(ev["partial_response_streamed"], bool)

    def test_rate_limit_pattern_emits_rate_limit_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "HTTPError: 429 Too Many Requests\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "RateLimit")

    def test_5xx_server_error_pattern_emits_network_error_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "claude: API Error: 503 Service Unavailable\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "NetworkError")

    def test_fetch_failed_pattern_emits_network_error_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "TypeError: fetch failed\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "NetworkError")

    def test_timeout_pattern_emits_timeout_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "claude: request timed out after 60s\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "Timeout")

    def test_etimedout_token_alone_emits_timeout_class(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "Error: connect ETIMEDOUT 10.0.0.1:443\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "Timeout")

    # ----------------------------------------------------------------
    # Negative tests (spark MEDIUM #3): benign user text containing 5xx /
    # 'fetch failed' / 'timed out' WITHOUT an API/provider context marker
    # on the same line must NOT trigger a leader.api_error event.
    # ----------------------------------------------------------------

    def test_negative_bare_503_in_user_text_does_not_emit(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: I saw a 503 error in my browser yesterday\n"
            "● Assistant: That can mean the upstream is overloaded.\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(events, [])
        self.assertEqual(self._emitted_api_errors(), [])

    def test_negative_bare_fetch_failed_in_user_text_does_not_emit(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: the unit test description says 'fetch failed' which is misleading\n"
            "● Assistant: Let me look at the spec.\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(events, [])
        self.assertEqual(self._emitted_api_errors(), [])

    def test_negative_bare_timed_out_in_user_text_does_not_emit(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: my CI build timed out after 30 minutes\n"
            "● Assistant: A long build can hint at runaway tests.\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(events, [])
        self.assertEqual(self._emitted_api_errors(), [])

    def test_negative_503_in_a_url_path_does_not_emit(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: please open https://example.com/issues/503/comments\n"
            "● Assistant: Reading the issue thread...\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(events, [])

    def test_negative_fetch_failed_inside_doc_string_unrelated_to_api(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: when fetch failed in the legacy frontend module we logged at warn level\n"
            "● Assistant: That's an unrelated wrapper.\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(events, [])

    def test_positive_5xx_with_codex_prefix_still_emits(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "codex returned 502 Bad Gateway from upstream\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "NetworkError")

    def test_positive_fetch_failed_with_anthropic_prefix_still_emits(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "Anthropic SDK: fetch failed (ECONNRESET)\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(self._emitted_api_errors()[0]["error_class"], "NetworkError")

    def test_dedupe_does_not_double_emit_for_same_scrollback(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "API Error: Overloaded\n"
        first = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        second = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertEqual(len(first), 1)
        self.assertEqual(second, [])
        self.assertEqual(len(self._emitted_api_errors()), 1)

    def test_clean_scrollback_clears_dedupe_so_next_error_re_emits(self) -> None:
        state = _state_with_attached_leader()
        dirty = "API Error: Overloaded\n"
        clean = "> ready\n"
        detect_leader_api_errors(self.workspace, state, self.store, self.event_log, capture_fn=_make_capture(dirty))
        detect_leader_api_errors(self.workspace, state, self.store, self.event_log, capture_fn=_make_capture(clean))
        again = detect_leader_api_errors(self.workspace, state, self.store, self.event_log, capture_fn=_make_capture(dirty))
        self.assertEqual(len(again), 1)
        self.assertEqual(len(self._emitted_api_errors()), 2)

    def test_no_event_when_no_leader_receiver_attached(self) -> None:
        state = {"leader_receiver": {"mode": "fallback_inbox"}}
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture("API Error: Overloaded\n"),
        )
        self.assertEqual(events, [])
        self.assertEqual(self._emitted_api_errors(), [])

    def test_worker_dispatch_just_before_includes_recent_leader_sends(self) -> None:
        state = _state_with_attached_leader()
        now = datetime.now(timezone.utc)
        # Create three leader→worker messages: two within 60s, one well outside.
        recent_a = self.store.create_message(None, "leader", "developer", "task A", requires_ack=False)
        recent_b = self.store.create_message(None, "leader", "developer", "task B", requires_ack=False)
        old = self.store.create_message(None, "leader", "developer", "task old", requires_ack=False)
        # Backdate the old one to 5 minutes ago.
        with self.store.connect() as conn:
            conn.execute(
                "update messages set created_at = ? where message_id = ?",
                (_ts(now, seconds_ago=300), old),
            )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture("API Error: Overloaded\n"),
            now_fn=lambda: now + timedelta(seconds=1),
        )
        self.assertEqual(len(events), 1)
        dispatches = events[0]["worker_dispatch_just_before"]
        self.assertIn(recent_a, dispatches)
        self.assertIn(recent_b, dispatches)
        self.assertNotIn(old, dispatches)

    def test_partial_response_streamed_true_when_assistant_text_precedes_error(self) -> None:
        state = _state_with_attached_leader()
        scrollback = (
            "User: do the thing\n"
            "● Assistant: I'll start by reading the file...\n"
            "Let me check the imports first.\n"
            "API Error: Overloaded\n"
        )
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertTrue(events[0]["partial_response_streamed"])

    def test_partial_response_streamed_false_when_error_appears_alone(self) -> None:
        state = _state_with_attached_leader()
        scrollback = "API Error: Overloaded\n"
        events = detect_leader_api_errors(
            self.workspace, state, self.store, self.event_log,
            capture_fn=_make_capture(scrollback),
        )
        self.assertFalse(events[0]["partial_response_streamed"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
