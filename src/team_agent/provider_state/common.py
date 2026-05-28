"""Shared, provider-neutral plumbing for the turn-state readers.

The per-provider readers (claude.py, codex.py) only translate their own record
shapes into a normalized list of lifecycle facts; everything else — JSONL
tail parsing, metadata filtering wiring, the verdict decision, and the
process-identity liveness guard — lives here so it is written once.
"""

from __future__ import annotations

import json
from typing import Any, Callable

# Normalized lifecycle fact kinds emitted by every reader.
TURN_OPEN = "turn_open"
TURN_COMPLETE = "turn_complete"
INTERRUPTED = "interrupted"
FAILED = "failed"
APPROVAL = "approval"
ERROR = "error"  # non-closing structured error (e.g. transient api retry / tool is_error)

_CLOSING = {TURN_COMPLETE, INTERRUPTED, FAILED}


def parse_jsonl(text: str) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Parse JSONL text into (records, parse_diagnostics).

    Lines that are blank are skipped. Lines that are not valid JSON objects are
    collected as diagnostics rather than raising — the caller decides whether a
    populated diagnostics list with zero usable records means ``unknown``.
    """
    records: list[dict[str, Any]] = []
    diagnostics: list[dict[str, Any]] = []
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except (ValueError, TypeError):
            diagnostics.append({"kind": "json_decode_error", "line": lineno})
            continue
        if not isinstance(obj, dict):
            diagnostics.append({"kind": "non_object_record", "line": lineno})
            continue
        records.append(obj)
    return records, diagnostics


def decide_state(
    facts: list[dict[str, Any]],
    *,
    process: Any = None,
    parse_diagnostics: list[dict[str, Any]] | None = None,
    had_records: bool,
    extra_diagnostics: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Turn a normalized fact stream into the public classify result.

    Verdict = the LAST lifecycle fact, not the last physical record. An open
    turn (a ``turn_open`` not yet closed) is a positive "still working" fact
    that survives arbitrary file silence (Gap 32 C14); the only thing that can
    demote it is a failed process-identity guard (Gap 32 C4).
    """
    diagnostics = list(parse_diagnostics or []) + list(extra_diagnostics or [])

    lifecycle = [f for f in facts if f.get("kind") in (_CLOSING | {TURN_OPEN, APPROVAL})]
    if not lifecycle:
        # No turn-lifecycle fact at all. If the input was unreadable/empty or a
        # changed format with no recognizable records, fail safe to unknown (C5).
        reason = "no_turn_lifecycle_fact"
        if not had_records:
            reason = "unreadable_or_empty"
        elif diagnostics:
            reason = "unrecognized_format"
        return _result("unknown", None, reason, "session_file", [], diagnostics)

    last = lifecycle[-1]
    kind = last.get("kind")
    turn_id = last.get("turn_id")
    reason = str(last.get("reason") or kind)

    if kind == TURN_COMPLETE:
        return _result("idle", turn_id, reason or "turn_complete", "session_file", [], diagnostics)
    if kind == INTERRUPTED:
        return _result("idle_interrupted", turn_id, reason or "interrupted", "session_file", ["interrupted"], diagnostics)
    if kind == FAILED:
        return _result("abnormal", turn_id, reason or "turn_failed", "session_file", ["turn_failed"], diagnostics)
    if kind == APPROVAL:
        return _result("blocked_on_human", turn_id, reason or "approval_required", "session_file", ["awaiting_approval"], diagnostics)

    # kind == TURN_OPEN with no later close → open turn. To declare "working" we
    # must POSITIVELY confirm the recorded process is still alive (C4 fail-safe);
    # missing/partial identity cannot be optimistically read as working.
    verdict, live_reason, live_diag = process_liveness(process)
    if live_diag:
        diagnostics = diagnostics + [live_diag]
    if verdict == "alive":
        return _result("working", turn_id, "open_turn", "session_file", [], diagnostics)
    if verdict == "dead":
        return _result("abnormal", turn_id, "crashed_mid_turn", "process_guard", ["crashed_mid_turn", live_reason], diagnostics)
    # unverifiable: cannot confirm alive → fail safe to unknown, never working.
    return _result("unknown", turn_id, "process_identity_unverified", "process_guard", ["process_identity_unverified", live_reason], diagnostics)


_STRONG_IDENTITY_FIELDS = ("start_time", "cmdline", "create_time")


def process_liveness(process: Any) -> tuple[str, str, dict[str, Any] | None]:
    """Process-identity liveness guard (Gap 32 C4) — three-valued.

    Returns (verdict, reason, diagnostic) where verdict is one of:
      - ``"alive"``        — positively confirmed the same process is running
      - ``"dead"``         — confirmed replaced/exited (identity mismatch or flag)
      - ``"unverifiable"`` — identity missing/partial; CANNOT be read as working

    Identity, not bare PID: aliveness must be affirmatively confirmed by a strong
    identity field (start_time / cmdline / create_time) present and equal in BOTH
    the recorded and the current snapshot. Missing/partial info is fail-safe
    unverifiable, never optimistically "alive".

    Accepted ``process`` shapes (any one):
      - None / non-dict                      → unverifiable
      - {"alive"|"running": bool}            → explicit
      - {"identity_match": bool}             → explicit identity verdict
      - {"expected"|"recorded": {...}, "current"|"observed": {...}}
    """
    if process is None or not isinstance(process, dict):
        return "unverifiable", "process_identity_missing", {"kind": "process_identity_unverified"}
    if process.get("alive") is False or process.get("running") is False:
        return "dead", "process_not_running", {"kind": "process_dead", "detail": "not_running"}
    if process.get("identity_match") is False:
        return "dead", "process_identity_mismatch", {"kind": "process_identity_mismatch"}
    if process.get("alive") is True or process.get("running") is True or process.get("identity_match") is True:
        return "alive", "process_alive", None
    recorded = process.get("recorded") if isinstance(process.get("recorded"), dict) else process.get("expected")
    current = process.get("current") if isinstance(process.get("current"), dict) else process.get("observed")
    if not (isinstance(recorded, dict) and isinstance(current, dict)):
        return "unverifiable", "process_identity_partial", {"kind": "process_identity_unverified"}
    if current.get("alive") is False or current.get("running") is False:
        return "dead", "process_not_running", {"kind": "process_dead", "detail": "current_not_running"}
    # Any shared strong identity field that DIFFERS = confirmed replacement.
    for key in _STRONG_IDENTITY_FIELDS:
        if key in recorded and key in current and recorded.get(key) != current.get(key):
            return "dead", f"process_identity_mismatch:{key}", {
                "kind": "process_identity_mismatch",
                "field": key,
                "recorded": recorded.get(key),
                "current": current.get(key),
            }
    # Require at least one strong identity field present+equal in BOTH, with no
    # recorded strong field missing from current (else we cannot confirm).
    recorded_strong = [k for k in _STRONG_IDENTITY_FIELDS if k in recorded]
    confirmed = [k for k in recorded_strong if k in current and recorded.get(k) == current.get(k)]
    missing = [k for k in recorded_strong if k not in current]
    if confirmed and not missing:
        return "alive", "process_identity_match", None
    return "unverifiable", "process_identity_partial", {
        "kind": "process_identity_unverified",
        "recorded_strong": recorded_strong,
        "confirmed": confirmed,
        "missing": missing,
    }


def process_is_live(process: Any) -> tuple[bool, str, dict[str, Any] | None]:
    """Boolean wrapper used by conservative callers (e.g. whole-team-gone): a
    process is treated as live unless it is CONFIRMED dead. Unverifiable counts
    as live here so we never falsely declare the team gone."""
    verdict, reason, diag = process_liveness(process)
    return (verdict != "dead"), reason, diag


def _result(
    state: str,
    turn_id: str | None,
    reason: str,
    source: str,
    annotations: list[str],
    diagnostics: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "state": state,
        "turn_id": turn_id,
        "reason": reason,
        "source": source,
        "annotations": list(annotations),
        "diagnostics": list(diagnostics),
    }


def classify_with_reader(
    extract_facts: Callable[[list[dict[str, Any]]], tuple[list[dict[str, Any]], list[dict[str, Any]]]],
    session_log_text: str,
    *,
    process: Any = None,
) -> dict[str, Any]:
    """Run a provider reader's fact extractor through the shared pipeline."""
    records, parse_diag = parse_jsonl(session_log_text or "")
    facts, extra_diag = extract_facts(records)
    return decide_state(
        facts,
        process=process,
        parse_diagnostics=parse_diag,
        had_records=bool(records),
        extra_diagnostics=extra_diag,
    )
