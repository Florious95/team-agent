"""Gap 28 (Slice 2 Stage 2): observe-only detection of leader-pane API errors.

The coordinator tick captures the leader pane scrollback once per cycle, scans it for
known upstream-API error patterns (Claude/Codex CLI errors that occur mid-turn), and
emits a structured `leader.api_error` audit event. The intent is observability — auto-
retry belongs to the upstream CLI; this module never touches the pane.

Event schema (logged via EventLog.write):

  event:                          'leader.api_error'
  ts:                             ISO-8601 UTC (added by EventLog)
  leader_session_uuid:            str | None
  error_class:                    'Overloaded' | 'RateLimit' | 'Timeout' |
                                  'NetworkError' | 'Unknown'
  provider:                       'claude' | 'codex' | 'claude_code' | str | None
  partial_response_streamed:      bool  (heuristic: assistant text before the error)
  worker_dispatch_just_before:    list[str]  (leader→worker msg_ids in the prior 60s)
  retry_count:                    int   (always 0 — the framework does not retry today)
  matched_pattern_snippet:        str   (the captured error line, ≤160 chars)

Detection dedupes within the coordinator state via a (error_class, snippet-tail)
fingerprint stored under `state['coordinator']['last_api_error_fingerprint']`. A
clean tick (no error pattern present) clears the fingerprint so the next genuine
error re-emits. This keeps event volume bounded while still catching distinct
errors as they occur.
"""
from __future__ import annotations

import re
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable

from team_agent.events import EventLog
from team_agent.message_store import MessageStore


# Spark MEDIUM sweeps (2026-05-26):
# (#3) Require an API/provider context marker near the error keyword. Bare '503' /
#      'fetch failed' / 'timed out' in user text used to false-fire.
# (#7) Match across short sliding windows of 1-3 adjacent lines so wrapped tmux
#      output ("claude:\n  request timed out") still resolves to a single
#      detection. Window joined with a single space; capped at _WINDOW_MAX_CHARS
#      so the scan stays bounded.
_API_CONTEXT = (
    r"(?:API\s+Error|HTTP\s*Error|HTTPError|request\s+failed|"
    r"codex|claude|Anthropic|OpenAI|TypeError)"
)

# Patterns operate against a sliding window of up to 3 joined lines. The window
# never contains '\n' (lines are joined with a single space), so `[^\n]` and `.`
# behave the same; we use `[^\n]` for self-documentation.
_ERROR_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    # Overloaded — keyword itself already includes the "API Error:" prefix.
    (re.compile(r"API\s+Error:\s*Overloaded", re.IGNORECASE), "Overloaded"),
    # RateLimit — 429 with "Too Many Requests" is sufficiently specific; require it
    # appear AFTER an API context marker OR before "Too Many Requests" tightly.
    (re.compile(rf"(?:{_API_CONTEXT}[^\n]*\b429\b|\b429\s+Too\s+Many\s+Requests)", re.IGNORECASE), "RateLimit"),
    # 5xx — must share a window with an API-context marker on either side.
    (re.compile(rf"{_API_CONTEXT}[^\n]{{0,200}}\b5(?:00|02|03|04)\b", re.IGNORECASE), "NetworkError"),
    (re.compile(rf"\b5(?:00|02|03|04)\b[^\n]{{0,200}}{_API_CONTEXT}", re.IGNORECASE), "NetworkError"),
    # fetch failed — needs an API-context marker in the same window. The TypeError
    # marker on its own counts (Node fetch frames the error this way).
    (re.compile(rf"{_API_CONTEXT}[^\n]{{0,200}}fetch\s+failed", re.IGNORECASE), "NetworkError"),
    (re.compile(rf"fetch\s+failed[^\n]{{0,200}}{_API_CONTEXT}", re.IGNORECASE), "NetworkError"),
    # Timeout — likewise requires an API-context marker in the window, except for
    # the unambiguous syscall token ETIMEDOUT.
    (re.compile(rf"{_API_CONTEXT}[^\n]{{0,200}}(?:request|connection)\s+(?:timed\s+out|timeout)", re.IGNORECASE), "Timeout"),
    (re.compile(rf"(?:request|connection)\s+(?:timed\s+out|timeout)[^\n]{{0,200}}{_API_CONTEXT}", re.IGNORECASE), "Timeout"),
    (re.compile(r"\bETIMEDOUT\b", re.IGNORECASE), "Timeout"),
]

_RECENT_LINE_WINDOW = 100        # scan only the most recent N lines
_SLIDING_WINDOW_LINES = 3        # join up to 3 adjacent lines per scan window
_WINDOW_MAX_CHARS = 400          # discard windows beyond this length to bound work
_DISPATCH_WINDOW_SECONDS = 60    # leader→worker sends counted within this lookback
_PARTIAL_RESPONSE_HEAD_BYTES = 4000

_PARTIAL_RESPONSE_HINT = re.compile(
    r"(?:^|\n)\s*(?:Assistant|⏺|●|> |I'll |I will |I'm |I am |Let me )",
    re.IGNORECASE,
)


def detect_leader_api_errors(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
    *,
    capture_fn: Callable[[str], dict[str, Any]] | None = None,
    now_fn: Callable[[], datetime] | None = None,
) -> list[dict[str, Any]]:
    """Coordinator-tick entry point. Returns a list of emitted events (0 or 1)."""
    receiver = state.get("leader_receiver") or {}
    pane = receiver.get("pane_id") if receiver.get("mode") == "direct_tmux" else None
    if not pane:
        return []
    capture_fn = capture_fn or _default_capture_fn()
    capture = capture_fn(str(pane))
    if not capture.get("ok"):
        return []
    scrollback = str(capture.get("capture") or "")
    coordinator_state = state.setdefault("coordinator", {})
    found = _match_first_error(scrollback)
    if not found:
        if coordinator_state.get("last_api_error_fingerprint"):
            coordinator_state["last_api_error_fingerprint"] = None
        return []
    error_class, snippet = found
    fingerprint = f"{error_class}::{snippet[-120:]}"
    if coordinator_state.get("last_api_error_fingerprint") == fingerprint:
        return []
    coordinator_state["last_api_error_fingerprint"] = fingerprint
    now = (now_fn() if now_fn else datetime.now(timezone.utc))
    cutoff_iso = (now - timedelta(seconds=_DISPATCH_WINDOW_SECONDS)).isoformat()
    leader_uuid = (
        str((state.get("team_owner") or {}).get("leader_session_uuid") or "")
        or str(receiver.get("leader_session_uuid") or "")
        or None
    )
    provider = str(receiver.get("provider") or "") or None
    event = event_log.write(
        "leader.api_error",
        leader_session_uuid=leader_uuid,
        error_class=error_class,
        provider=provider,
        partial_response_streamed=_scrollback_has_partial_response(scrollback, snippet),
        worker_dispatch_just_before=_recent_leader_dispatches(store, cutoff_iso),
        retry_count=0,
        matched_pattern_snippet=snippet[:160],
    )
    return [event]


def _default_capture_fn() -> Callable[[str], dict[str, Any]]:
    from team_agent.messaging.deps import _capture_tmux_pane_text
    return _capture_tmux_pane_text


def _match_first_error(scrollback: str) -> tuple[str, str] | None:
    """Spark MEDIUM #7: sliding window of 1..N adjacent lines. Lines inside a
    window are joined with a single space so a wrapped pair such as
        claude:
          request timed out
    is detected as one event without permitting unbounded cross-line matches.
    Latest window wins so the freshest error is reported."""
    if not scrollback:
        return None
    lines = [line.strip() for line in scrollback.splitlines()[-_RECENT_LINE_WINDOW:]]
    if not lines:
        return None
    best: tuple[int, str, str] | None = None
    for start in range(len(lines)):
        for size in range(1, _SLIDING_WINDOW_LINES + 1):
            end = start + size
            if end > len(lines):
                break
            window = " ".join(line for line in lines[start:end] if line)
            if not window or len(window) > _WINDOW_MAX_CHARS:
                continue
            for pattern, error_class in _ERROR_PATTERNS:
                match = pattern.search(window)
                if not match:
                    continue
                snippet = window[:240]
                if best is None or start > best[0]:
                    best = (start, error_class, snippet)
                # First match per window is enough; later windows may override.
                break
    if best is None:
        return None
    return best[1], best[2]


def _scrollback_has_partial_response(scrollback: str, error_snippet: str) -> bool:
    idx = scrollback.rfind(error_snippet)
    if idx == -1:
        return False
    head = scrollback[max(0, idx - _PARTIAL_RESPONSE_HEAD_BYTES): idx]
    return bool(_PARTIAL_RESPONSE_HINT.search(head))


def _recent_leader_dispatches(store: MessageStore, cutoff_iso: str) -> list[str]:
    out: list[str] = []
    try:
        rows = store.messages()
    except Exception:
        return out
    for row in rows:
        sender = str(row.get("sender") or "")
        if sender not in {"leader", "Leader"} and not _looks_like_leader_sender(sender):
            continue
        created = str(row.get("created_at") or "")
        if not created or created < cutoff_iso:
            continue
        msg_id = str(row.get("message_id") or "")
        if msg_id:
            out.append(msg_id)
    return out


def _looks_like_leader_sender(sender: str) -> bool:
    return sender.startswith("leader") or sender.lower() == "leader"


__all__ = ["detect_leader_api_errors"]
