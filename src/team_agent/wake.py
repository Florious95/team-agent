"""Provider-neutral wake layer (Gap 32 §2): decide WHEN to re-read a session
file, never HOW to parse it. No polling loop, no screen, no provider names.

Two cheap, passive sources:
  - a file-change watch (the caller wires FSEvents/inotify/kqueue and calls
    ``on_file_changed``) — naturally per-session (one file per node);
  - an mtime gate — only re-read the tail when the file changed since the last
    classification, or when it has been quiet past the debounce window.
"""

from __future__ import annotations

from typing import Any


def should_reread(
    *,
    last_mtime: float | None,
    current_mtime: float | None,
    last_classified_mtime: float | None,
    now: float,
    debounce_seconds: float,
) -> dict[str, Any]:
    """Return whether a fresh tail-read is warranted and why.

    A read is warranted when the file changed since we last classified it, or
    when it has been silent longer than the debounce window and we have not yet
    classified at this mtime (so an idle-close that we already saw is not
    re-read forever).
    """
    if current_mtime is None:
        return {"reread": False, "reason": "no_file"}
    if last_classified_mtime is None:
        return {"reread": True, "reason": "never_classified"}
    if current_mtime != last_classified_mtime:
        return {"reread": True, "reason": "file_changed"}
    silent_for = max(0.0, now - current_mtime)
    if silent_for >= debounce_seconds:
        return {"reread": False, "reason": "quiescent_already_classified"}
    return {"reread": False, "reason": "unchanged"}


def on_file_changed(watch_state: dict[str, Any] | None, *, node_id: str, mtime: float) -> dict[str, Any]:
    """Record a file-change wake for a node (the push path)."""
    state = dict(watch_state or {})
    pending = set(state.get("pending") or [])
    pending.add(node_id)
    state["pending"] = sorted(pending)
    state.setdefault("mtimes", {})[node_id] = mtime
    return state


def take_pending(watch_state: dict[str, Any] | None) -> tuple[list[str], dict[str, Any]]:
    """Drain the set of nodes whose files changed since the last drain."""
    state = dict(watch_state or {})
    pending = sorted(state.get("pending") or [])
    state["pending"] = []
    return pending, state
