"""Provider-neutral take-over reminder predicate (Gap 32 §3).

Consumes already-classified node states only. Contains no provider knowledge.
Rules: arm only after a worker has opened a turn (C1); fire one neutral ping when
every node is idle for a monotonic debounce window (C2/C11); idle_interrupted
counts as idle but is annotated (C12); re-arm on a real turn-open edge (C3).
"""

from __future__ import annotations

from typing import Any

_IDLE_STATES = {"idle", "idle_interrupted"}
_DELEGATED_STATES = {"working", "blocked_on_human", "idle_interrupted", "abnormal"}

_ARM_KEY = "opened_worker_turn_since_ack"
_SUPPRESS_KEY = "suppressed"


def evaluate_takeover_reminder(
    nodes: list[dict[str, Any]],
    *,
    monitor_state: dict[str, Any] | None,
    now_monotonic: float,
    debounce_seconds: float,
    suspend_intervals: list[tuple[float, float]] | None = None,
    event_sink: Any = None,
) -> dict[str, Any]:
    state = dict(monitor_state or {})
    state.setdefault(_ARM_KEY, False)
    state.setdefault(_SUPPRESS_KEY, False)
    state.setdefault("all_idle_since", None)
    state.setdefault("pinged_for_episode", None)

    # C1: a WORKER turn-open (working / blocked / interrupted / faulted) is the
    # only thing that arms the watch. Leader-only activity never arms it.
    for node in nodes:
        if _role(node) == "leader":
            continue
        if node.get("state") in _DELEGATED_STATES:
            state[_ARM_KEY] = True

    # Any non-idle node blocks the ping; report which kind (C5 unknown / C14 working).
    for node in nodes:
        node_state = node.get("state")
        if node_state not in _IDLE_STATES:
            state["all_idle_since"] = None
            state["pinged_for_episode"] = None
            return _result(False, None, f"node_{node_state or 'unknown'}", _interrupted(nodes), state)

    if not nodes:
        return _result(False, None, "no_nodes", [], state)

    if state.get("all_idle_since") is None:
        state["all_idle_since"] = now_monotonic
        state["pinged_for_episode"] = None
    elapsed = _active_elapsed(state["all_idle_since"], now_monotonic, suspend_intervals)
    interrupted = _interrupted(nodes)

    if not state.get(_ARM_KEY):
        return _result(False, None, "not_armed_no_worker_turn", interrupted, state)
    if state.get(_SUPPRESS_KEY):
        return _result(False, None, "acknowledged", interrupted, state)
    if elapsed < debounce_seconds:
        return _result(False, None, "debounce_active", interrupted, state)
    if state.get("pinged_for_episode") == state.get("all_idle_since"):
        return _result(False, None, "already_pinged_this_episode", interrupted, state)

    state["pinged_for_episode"] = state["all_idle_since"]
    message = _neutral_message(len(nodes), elapsed, interrupted)
    _emit(event_sink, "idle_takeover.ping", nodes=len(nodes), elapsed_seconds=int(elapsed), interrupted=[i["node_id"] for i in interrupted])
    return _result(True, message, "all_idle_debounce_elapsed", interrupted, state)


def record_turn_open_after_delivery(
    monitor_state: dict[str, Any] | None,
    *,
    node_id: str,
    turn_id: str | None,
    delivered_message_id: str | None,
    now_monotonic: float,
    event_sink: Any = None,
) -> dict[str, Any]:
    """A delivered inbound message produced a real turn-open edge (C3).

    Re-arms a previously acknowledged watch so delivered-but-unprocessed work
    can never leave it permanently suppressed. Returns the updated monitor_state
    directly (with the re-arm flags set).
    """
    state = dict(monitor_state or {})
    state[_ARM_KEY] = True
    state[_SUPPRESS_KEY] = False
    state["all_idle_since"] = None
    state["pinged_for_episode"] = None
    state["last_turn_open"] = {
        "node_id": node_id,
        "turn_id": turn_id,
        "delivered_message_id": delivered_message_id,
        "at": now_monotonic,
    }
    state["ok"] = True
    state["rearmed"] = True
    _emit(event_sink, "idle_takeover.turn_open_rearmed", node_id=node_id, turn_id=turn_id, delivered_message_id=delivered_message_id)
    return state


def _role(node: dict[str, Any]) -> str:
    return str(node.get("role") or ("leader" if node.get("is_leader") else "worker"))


def _interrupted(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {
            "node_id": n.get("node_id"),
            "node": n.get("node_id"),
            "state": "idle_interrupted",
            "reason": "interrupted",
            "interrupted": True,
            "kind": "interrupted",
            "type": "interrupted",
            "annotation": "interrupted",
        }
        for n in nodes
        if n.get("state") == "idle_interrupted"
    ]


def _active_elapsed(start: float, now: float, suspend_intervals: list[tuple[float, float]] | None) -> float:
    elapsed = max(0.0, now - start)
    if not suspend_intervals:
        return elapsed
    # C11: clip each window to [start, now], then MERGE overlapping/duplicate
    # windows before subtracting, so an overlap is never counted twice.
    clipped: list[tuple[float, float]] = []
    for interval in suspend_intervals:
        try:
            s, e = float(interval[0]), float(interval[1])
        except (TypeError, ValueError, IndexError):
            continue
        lo = max(s, start)
        hi = min(e, now)
        if hi > lo:
            clipped.append((lo, hi))
    suspended = 0.0
    for lo, hi in _merge_intervals(clipped):
        suspended += hi - lo
    return max(0.0, elapsed - suspended)


def _merge_intervals(intervals: list[tuple[float, float]]) -> list[tuple[float, float]]:
    if not intervals:
        return []
    ordered = sorted(intervals)
    merged: list[tuple[float, float]] = [ordered[0]]
    for lo, hi in ordered[1:]:
        last_lo, last_hi = merged[-1]
        if lo <= last_hi:  # overlapping or touching → merge
            merged[-1] = (last_lo, max(last_hi, hi))
        else:
            merged.append((lo, hi))
    return merged


def _neutral_message(node_count: int, elapsed: float, interrupted: list[dict[str, Any]]) -> str:
    minutes = max(1, int(round(elapsed / 60.0)))
    base = (
        f"All nodes idle: {node_count} team nodes have had every turn closed for "
        f"about {minutes} min. If this idle state is intentional, run "
        f"team-agent acknowledge-idle to confirm it."
    )
    if interrupted:
        ids = ", ".join(str(i["node_id"]) for i in interrupted)
        base += f" Interrupted nodes: {ids}."
    return base


def _result(should_ping: bool, message: str | None, reason: str, annotations: list[dict[str, Any]], state: dict[str, Any]) -> dict[str, Any]:
    return {
        "should_ping": should_ping,
        "message": message,
        "reason": reason,
        "annotations": list(annotations),
        "interrupted_nodes": [a.get("node_id") for a in annotations],
        "interrupted": [a.get("node_id") for a in annotations],
        "monitor_state": state,
    }


def _emit(event_sink: Any, name: str, **fields: Any) -> None:
    if event_sink is None:
        return
    try:
        event_sink(name, fields)
    except TypeError:
        try:
            event_sink({"event": name, **fields})
        except Exception:
            pass
    except Exception:
        pass
