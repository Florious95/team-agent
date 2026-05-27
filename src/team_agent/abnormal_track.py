"""Provider-neutral abnormal-state track (Gap 32 §4).

Reads structured fault records + process identity; never reads a screen and
never names a provider. Catch-bias for structured error/failed-class records
(C9), dedup by (signature, turn) (C8), and coordinator-independent whole-team
disappearance with clean-shutdown vs unexpected distinction (C10).
"""

from __future__ import annotations

from typing import Any


def process_abnormal_records(
    records: list[dict[str, Any]],
    *,
    registry: Any,
    notification_state: dict[str, Any] | None,
    event_sink: Any = None,
) -> dict[str, Any]:
    """Classify raw provider session records that may carry faults.

    ``registry`` carries the provider whose records these are (``{"provider":
    name}``) or a full registry mapping. Records are turned into structured
    fault facts by the provider reader (so this module names no provider), then
    catch-biased + deduped by (signature, turn).
    """
    from team_agent.provider_state import read_fault_facts
    from team_agent.provider_state.registry import get_provider_registry

    state = dict(notification_state or {})
    seen = set(state.get("seen") or [])
    notifications: list[dict[str, Any]] = []
    discovery_log: list[dict[str, Any]] = []
    diagnostics: list[dict[str, Any]] = []

    provider = _provider_of(registry)
    white, black = _lists_for(provider, registry, get_provider_registry)

    faults = read_fault_facts(provider, records or []) if provider else []
    if not faults and records:
        # Records that produced no structured fault fact are not default-notify
        # candidates (C9): arbitrary unrecognized lines become diagnostics only.
        diagnostics.append({"kind": "no_structured_fault", "count": len(records)})

    for fact in faults:
        signature = str(fact.get("signature") or fact.get("reason") or "fault")
        turn_id = fact.get("turn_id")
        text = " ".join(str(x) for x in (signature, fact.get("reason"), _raw_message(fact)) if x).lower()
        decision = _classify(text, signature, white, black)
        discovery_log.append({
            "signature": signature,
            "turn_id": turn_id,
            "decision": decision,
            "kind": fact.get("kind"),
            "provider": provider,
        })
        if decision == "skip":
            continue
        key = f"{signature}\x00{turn_id}"
        if key in seen:
            continue  # C8: one notify per (signature, turn)
        seen.add(key)
        notifications.append({
            "signature": signature,
            "turn_id": turn_id,
            "dedupe_key": (signature, turn_id),
            "state": "blocked_on_human" if fact.get("kind") == "approval" else "abnormal",
            "decision": decision,
            "provider": provider,
            "raw": fact.get("raw", fact),
            "raw_record": fact.get("raw", fact),
        })
        _emit(event_sink, "abnormal.notify", signature=signature, turn_id=turn_id, decision=decision)

    state["seen"] = sorted(seen)
    return {
        "notifications": notifications,
        "discovery_log": discovery_log,
        "diagnostics": diagnostics,
        "notification_state": state,
    }


def detect_whole_team_gone(
    snapshot: dict[str, Any],
    *,
    marker_store: Any,
    event_sink: Any = None,
) -> dict[str, Any]:
    """Coordinator-independent whole-team-gone detection (C10/C13).

    Does not require the coordinator to be alive. The whole team is gone when the
    coordinator, the leader, every provider process, and every session are all
    absent. Clean shutdown / restart-in-progress (flagged in the snapshot) are
    silent; an unexpected disappearance records a durable marker and defers user
    escalation to the next leader command.
    """
    coordinator = snapshot.get("coordinator") or {}
    leader = snapshot.get("leader") or {}
    provider_processes = snapshot.get("provider_processes")
    if provider_processes is None:
        provider_processes = snapshot.get("nodes") or snapshot.get("agents") or []
    tmux_sessions = snapshot.get("tmux_sessions") or []

    coord_alive = _alive(coordinator)
    leader_alive = _alive(leader)
    any_worker_alive = any(_alive(p) for p in provider_processes)
    sessions_present = bool(tmux_sessions)

    whole_gone = not (coord_alive or leader_alive or any_worker_alive or sessions_present)

    if not whole_gone:
        return {
            "state": "alive",
            "whole_team_gone": False,
            "classification": "alive",
            "notify": False,
            "escalate_user_on_next_leader_command": False,
            "marker_written": False,
        }

    if snapshot.get("clean_shutdown"):
        return _silent_gone("clean_shutdown")
    if snapshot.get("restart_in_progress"):
        return _silent_gone("restart_in_progress")

    # Unexpected disappearance (闪退): durable marker + deferred escalation.
    marker_written = _marker_set(marker_store, "whole_team_gone", {
        "classification": "unexpected_exit",
        "provider_processes": len(provider_processes),
    })
    _emit(event_sink, "abnormal.whole_team_gone", classification="unexpected_exit")
    return {
        "state": "whole_team_gone",
        "whole_team_gone": True,
        "classification": "unexpected_exit",
        "notify": True,
        "escalate_user_on_next_leader_command": True,
        "marker_written": bool(marker_written),
    }


def _silent_gone(classification: str) -> dict[str, Any]:
    return {
        "state": classification,
        "whole_team_gone": True,
        "classification": classification,
        "notify": False,
        "escalate_user_on_next_leader_command": False,
        "marker_written": False,
    }


def _alive(entry: Any) -> bool:
    from team_agent.provider_state.common import process_is_live

    if isinstance(entry, dict):
        if "alive" in entry:
            return entry.get("alive") is True
        if "process" in entry:
            ok, _r, _d = process_is_live(entry.get("process"))
            return ok
        ok, _r, _d = process_is_live(entry)
        return ok
    return bool(entry)


def _provider_of(registry: Any) -> str | None:
    if isinstance(registry, dict):
        if isinstance(registry.get("provider"), str):
            return registry.get("provider")
        if isinstance(registry.get("kind"), str):
            return registry.get("kind")
    return None


def _lists_for(provider: str | None, registry: Any, get_provider_registry: Any) -> tuple[list[str], list[str]]:
    entry: Any = None
    if isinstance(registry, dict) and ("error_whitelist" in registry or "error_blacklist" in registry):
        entry = registry
    elif provider is not None:
        entry = get_provider_registry(provider)
    if not isinstance(entry, dict):
        return [], []
    lists = entry.get("error_lists") if isinstance(entry.get("error_lists"), dict) else {}
    white = [str(x).lower() for x in (lists.get("whitelist") or entry.get("error_whitelist") or [])]
    black = [str(x).lower() for x in (lists.get("blacklist") or entry.get("error_blacklist") or [])]
    return white, black


def _classify(text: str, signature: str, white: list[str], black: list[str]) -> str:
    sig = signature.lower()
    if any(w and (w in text or w in sig) for w in white):
        return "skip"  # whitelist > blacklist > default
    if any(b and (b in text or b in sig) for b in black):
        return "notify_blacklist"
    return "notify_default"  # C9 catch-bias for structured faults


def _raw_message(fact: dict[str, Any]) -> str:
    raw = fact.get("raw")
    if isinstance(raw, dict):
        return str(raw.get("message") or "")
    return ""


def _marker_set(marker_store: Any, name: str, value: Any) -> bool:
    if marker_store is None:
        return False
    if isinstance(marker_store, dict):
        marker_store[name] = value
        return True
    setter = getattr(marker_store, "set", None) or getattr(marker_store, "write", None)
    if callable(setter):
        try:
            setter(name, value)
            return True
        except Exception:
            return False
    return False


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
