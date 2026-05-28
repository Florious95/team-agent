"""Gap 32 runtime wiring: drive the file-fact idle/takeover reminder from the
coordinator tick. This is the glue that replaces the legacy screen-scrape
`detect_idle_fallbacks` path — it classifies each node from its provider
session-log file (never the pane) and runs the provider-neutral predicate.
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any

_TAIL_BYTES = 131072
_DEBOUNCE_SECONDS = 60.0


IDLE_DEBOUNCE_SECONDS = _DEBOUNCE_SECONDS


def build_idle_nodes(state: dict[str, Any]) -> list[dict[str, Any]]:
    """Classify every live node from its provider session-log file fact (never
    the pane screen, never message-row status). The leader is read via its own
    transcript when its path is tracked (C13), else omitted rather than guessed.
    """
    from team_agent.provider_state import read_turn_state

    nodes: list[dict[str, Any]] = []
    for agent_id, agent_state in (state.get("agents") or {}).items():
        if str(agent_state.get("status") or "") in {"stopped", "paused"}:
            continue
        provider = str(agent_state.get("provider") or "")
        classification = read_turn_state(provider, _read_session_tail(agent_state.get("rollout_path")))
        nodes.append({
            "node_id": agent_id,
            "role": "worker",
            "state": classification.get("state"),
            "turn_id": classification.get("turn_id"),
            "annotations": classification.get("annotations"),
        })
    leader_node = _leader_node(state)
    if leader_node is not None:
        nodes.append(leader_node)
    return nodes


def push_idle_reminder(workspace: Path, state: dict[str, Any], event_log: Any, result: dict[str, Any]) -> None:
    """Deliver the one neutral take-over reminder to the leader when the
    predicate fired. No-op otherwise."""
    if not result.get("should_ping"):
        return
    from team_agent.messaging.internal_delivery import deliver_stored_message
    from team_agent.state import team_state_key

    leader_id = (state.get("leader") or {}).get("id") or "leader"
    try:
        deliver_stored_message(
            workspace,
            leader_id,
            result["message"],
            sender="coordinator",
            requires_ack=False,
            wait_visible=False,
            team=team_state_key(state),
        )
    except Exception as exc:
        event_log.write("idle_takeover.push_failed", error=str(exc))
    event_log.write(
        "idle_takeover.reminder",
        interrupted=result.get("interrupted_nodes"),
        reason=result.get("reason"),
    )


def _leader_node(state: dict[str, Any]) -> dict[str, Any] | None:
    """Best-effort leader node from its own provider transcript (C13). If the
    leader's session file path is not tracked, the leader is omitted rather than
    guessed — the predicate then evaluates the workers (the leader is the ping
    recipient and acts on the reminder regardless)."""
    from team_agent.provider_state import read_turn_state

    leader = state.get("leader") if isinstance(state.get("leader"), dict) else {}
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    path = leader.get("rollout_path") or receiver.get("rollout_path")
    provider = str(leader.get("provider") or receiver.get("provider") or "")
    if not path or not provider:
        return None
    classification = read_turn_state(provider, _read_session_tail(path))
    return {
        "node_id": leader.get("id") or "leader",
        "role": "leader",
        "state": classification.get("state"),
        "turn_id": classification.get("turn_id"),
        "annotations": classification.get("annotations"),
    }


def _read_session_tail(path: Any, max_bytes: int = _TAIL_BYTES) -> str:
    if not path:
        return ""
    try:
        p = Path(str(path))
        size = p.stat().st_size
        with p.open("rb") as handle:
            if size > max_bytes:
                handle.seek(size - max_bytes)
                # drop a possibly-partial first line after seeking mid-file
                handle.readline()
            data = handle.read()
        return data.decode("utf-8", errors="ignore")
    except (OSError, ValueError):
        return ""
