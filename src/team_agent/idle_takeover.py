"""Gap 32 idle/take-over public facade.

Thin surface that the runtime + acceptance contract import. Provider dispatch
lives in ``provider_state``; the predicate / abnormal / wake logic lives in the
provider-neutral modules. This module only wires them together.
"""

from __future__ import annotations

from typing import Any

from team_agent.abnormal_track import detect_whole_team_gone, process_abnormal_records
from team_agent.idle_predicate import evaluate_takeover_reminder, record_turn_open_after_delivery
from team_agent.provider_state import read_turn_state
from team_agent.provider_state.registry import get_provider_registry


def classify_provider_turn_state(
    provider: str,
    session_log_text: str,
    *,
    process: Any = None,
    file_silence_seconds: float = 0,
    registry: Any = None,
    event_sink: Any = None,
) -> dict[str, Any]:
    """Classify one node's turn state from its provider session-log text."""
    result = read_turn_state(
        provider,
        session_log_text,
        process=process,
        file_silence_seconds=file_silence_seconds,
        registry=registry,
    )
    if event_sink is not None and result.get("state") in {"unknown", "abnormal"}:
        _emit(event_sink, "idle_takeover.classify", provider=provider, state=result.get("state"), reason=result.get("reason"))
    return result


__all__ = [
    "classify_provider_turn_state",
    "evaluate_takeover_reminder",
    "record_turn_open_after_delivery",
    "process_abnormal_records",
    "detect_whole_team_gone",
    "get_provider_registry",
]


def _emit(event_sink: Any, name: str, **fields: Any) -> None:
    try:
        event_sink(name, fields)
    except TypeError:
        try:
            event_sink({"event": name, **fields})
        except Exception:
            pass
    except Exception:
        pass
