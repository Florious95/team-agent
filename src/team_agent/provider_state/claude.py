"""Claude transcript reader — the ONLY Claude-specific turn-state knowledge.

Translates Claude transcript JSONL records into normalized lifecycle facts.
Real markers (see turn-state-markers-evidence.md):
  - assistant message.stop_reason == "tool_use"   -> open turn (working)
  - assistant message.stop_reason == "end_turn"    -> turn complete (idle)
  - user text == "[Request interrupted by user]"   -> interrupted
  - user tool_result is_error == true              -> structured tool error
  - system subtype == "api_error" and level=="error" -> provider api error
Trailing metadata records (stop_hook_summary / turn_duration / last-prompt /
ai-title / permission-mode / ...) are ignored for the turn verdict.
"""

from __future__ import annotations

from typing import Any

from team_agent.provider_state import common

_INTERRUPT_TEXT = "[Request interrupted by user]"


def extract_facts(records: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    facts: list[dict[str, Any]] = []
    diagnostics: list[dict[str, Any]] = []
    for record in records:
        rtype = record.get("type")
        message = record.get("message")
        if rtype == "assistant" and isinstance(message, dict):
            stop_reason = message.get("stop_reason")
            turn_id = record.get("requestId") or record.get("uuid")
            if stop_reason == "end_turn":
                facts.append({"kind": common.TURN_COMPLETE, "turn_id": turn_id, "reason": "end_turn"})
            elif stop_reason == "tool_use":
                facts.append({"kind": common.TURN_OPEN, "turn_id": turn_id, "reason": "tool_use"})
            elif stop_reason == "stop_sequence":
                facts.append({"kind": common.TURN_COMPLETE, "turn_id": turn_id, "reason": "stop_sequence"})
            # other/missing stop_reason on assistant is treated as an open turn fragment
            elif stop_reason is None and isinstance(message.get("content"), list):
                facts.append({"kind": common.TURN_OPEN, "turn_id": turn_id, "reason": "assistant_in_flight"})
        elif rtype == "user" and isinstance(message, dict):
            content = message.get("content")
            if _content_has_interrupt(content):
                facts.append({"kind": common.INTERRUPTED, "turn_id": record.get("uuid"), "reason": "user_interrupt"})
            elif _content_has_tool_error(content):
                facts.append({
                    "kind": common.ERROR,
                    # the turn being retried/affected, stable across records (C8 dedup)
                    "turn_id": record.get("parentUuid") or record.get("uuid"),
                    "reason": "tool_result_is_error",
                    "signature": "tool_result_is_error",
                    "raw": record,
                })
        elif rtype == "system" and record.get("subtype") == "api_error" and record.get("level") == "error":
            facts.append({
                "kind": common.ERROR,
                # api_error retries within a session dedup on (signature, session) (C8)
                "turn_id": record.get("sessionId") or record.get("parentUuid") or record.get("uuid"),
                "reason": "api_error",
                "signature": "api_error",
                "raw": record,
            })
        # everything else (metadata, snapshots, titles) is ignored for the verdict
    return facts, diagnostics


def classify(session_log_text: str, *, process: Any = None) -> dict[str, Any]:
    return common.classify_with_reader(extract_facts, session_log_text, process=process)


def _content_has_interrupt(content: Any) -> bool:
    if not isinstance(content, list):
        return False
    for item in content:
        if isinstance(item, dict) and item.get("type") == "text" and item.get("text") == _INTERRUPT_TEXT:
            return True
    return False


def _content_has_tool_error(content: Any) -> bool:
    if not isinstance(content, list):
        return False
    for item in content:
        if isinstance(item, dict) and item.get("type") == "tool_result" and item.get("is_error") is True:
            return True
    return False
