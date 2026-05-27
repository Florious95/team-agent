"""Codex rollout reader — the ONLY Codex-specific turn-state knowledge.

Translates Codex rollout JSONL (and app-server jsonrpc) records into normalized
lifecycle facts. Real markers (see turn-state-markers-evidence.md):
  - event_msg payload.type == "task_started"   -> open turn (working)
  - event_msg payload.type == "task_complete"  -> turn complete (idle)
  - event_msg payload.type == "turn_aborted" reason=="interrupted" -> interrupted
App-server schema-derived markers:
  - method "turn/completed" params.turn.status == "failed" -> failed/error
  - method ".../requestApproval"                            -> approval block
Telemetry (token_count, agent_message, patch_apply_end, ...) is not a close.
"""

from __future__ import annotations

from typing import Any

from team_agent.provider_state import common


def extract_facts(records: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    facts: list[dict[str, Any]] = []
    diagnostics: list[dict[str, Any]] = []
    for record in records:
        rtype = record.get("type")
        payload = record.get("payload") if isinstance(record.get("payload"), dict) else None
        if rtype == "event_msg" and payload is not None:
            ptype = payload.get("type")
            turn_id = payload.get("turn_id")
            if ptype == "task_started":
                facts.append({"kind": common.TURN_OPEN, "turn_id": turn_id, "reason": "task_started"})
            elif ptype == "task_complete":
                facts.append({"kind": common.TURN_COMPLETE, "turn_id": turn_id, "reason": "task_complete"})
            elif ptype == "turn_aborted" and payload.get("reason") == "interrupted":
                facts.append({"kind": common.INTERRUPTED, "turn_id": turn_id, "reason": "interrupted"})
            elif ptype == "turn_aborted":
                facts.append({"kind": common.INTERRUPTED, "turn_id": turn_id, "reason": str(payload.get("reason") or "aborted")})
        elif _is_app_server(record):
            fact = _app_server_fact(record)
            if fact is not None:
                facts.append(fact)
        # response_item (assistant/user messages), token_count, etc. are not verdicts
    return facts, diagnostics


def classify(session_log_text: str, *, process: Any = None) -> dict[str, Any]:
    return common.classify_with_reader(extract_facts, session_log_text, process=process)


def _is_app_server(record: dict[str, Any]) -> bool:
    return record.get("jsonrpc") == "2.0" and isinstance(record.get("method"), str)


def _app_server_fact(record: dict[str, Any]) -> dict[str, Any] | None:
    method = str(record.get("method") or "")
    params = record.get("params") if isinstance(record.get("params"), dict) else {}
    if method == "turn/completed":
        turn = params.get("turn") if isinstance(params.get("turn"), dict) else {}
        status = turn.get("status")
        turn_id = turn.get("id")
        if status == "failed":
            return {
                "kind": common.FAILED,
                "turn_id": turn_id,
                "reason": "turn_failed",
                "signature": "turn_failed",
                "raw": record,
            }
        if status == "completed":
            return {"kind": common.TURN_COMPLETE, "turn_id": turn_id, "reason": "completed"}
        if status == "interrupted":
            return {"kind": common.INTERRUPTED, "turn_id": turn_id, "reason": "interrupted"}
        if status == "inProgress":
            return {"kind": common.TURN_OPEN, "turn_id": turn_id, "reason": "in_progress"}
        return None
    if method.endswith("requestApproval"):
        return {
            "kind": common.APPROVAL,
            "turn_id": params.get("turnId") or params.get("turn_id"),
            "reason": "approval_required",
            "signature": "approval_required",
            "raw": record,
        }
    return None
