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

    # kind == TURN_OPEN with no later close → open turn.
    alive, live_reason, live_diag = process_is_live(process)
    if live_diag:
        diagnostics = diagnostics + [live_diag]
    if alive:
        return _result("working", turn_id, "open_turn", "session_file", [], diagnostics)
    return _result("abnormal", turn_id, "crashed_mid_turn", "process_guard", ["crashed_mid_turn", live_reason], diagnostics)


def process_is_live(process: Any) -> tuple[bool, str, dict[str, Any] | None]:
    """Process-identity liveness guard (Gap 32 C4).

    Returns (alive, reason, diagnostic). Identity, not bare PID existence: if a
    recorded process has been replaced by a different start-time / command, the
    original process is gone even though the PID may be reused.

    Accepted ``process`` shapes (any one):
      - None                         → assumed alive (cannot disprove)
      - {"alive": bool}              → explicit
      - {"identity_match": bool}     → explicit identity verdict
      - {"expected"|"recorded": {...}, "current"|"observed": {...}}
            → compared on pid/start_time/cmdline (identity, not bare PID)
    """
    if process is None:
        return True, "process_unknown_assumed_alive", None
    if not isinstance(process, dict):
        return True, "process_opaque_assumed_alive", None
    if process.get("alive") is False or process.get("running") is False:
        return False, "process_not_running", {"kind": "process_dead", "detail": "not_running"}
    if process.get("identity_match") is False:
        return False, "process_identity_mismatch", {"kind": "process_identity_mismatch"}
    recorded = process.get("recorded") if isinstance(process.get("recorded"), dict) else process.get("expected")
    current = process.get("current") if isinstance(process.get("current"), dict) else process.get("observed")
    if isinstance(recorded, dict) and isinstance(current, dict):
        if current.get("alive") is False or current.get("running") is False:
            return False, "process_not_running", {"kind": "process_dead", "detail": "current_not_running"}
        for key in ("start_time", "cmdline", "create_time"):
            if key in recorded and key in current and recorded.get(key) != current.get(key):
                return False, f"process_identity_mismatch:{key}", {
                    "kind": "process_identity_mismatch",
                    "field": key,
                    "recorded": recorded.get(key),
                    "current": current.get(key),
                }
        # pid reuse: same pid, different identity already handled above; if pid
        # differs and no identity fields given, treat as replaced.
        if "pid" in recorded and "pid" in current and recorded.get("pid") != current.get("pid"):
            return False, "process_pid_replaced", {"kind": "process_pid_replaced"}
    if process.get("alive") is True or process.get("running") is True or process.get("identity_match") is True:
        return True, "process_alive", None
    return True, "process_assumed_alive", None


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
