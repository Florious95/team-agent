from __future__ import annotations

from typing import Any


ADAPTIVE_DISPLAY_BACKEND = "adaptive"
GHOSTTY_DISPLAY_BACKENDS = {"ghostty", "ghostty_window", "ghostty_workspace"}
DISPLAY_BACKENDS_WITH_WORKER_VIEWS = GHOSTTY_DISPLAY_BACKENDS | {ADAPTIVE_DISPLAY_BACKEND}
VALID_DISPLAY_BACKENDS = {"none", "tmux_attach", "iterm"} | DISPLAY_BACKENDS_WITH_WORKER_VIEWS


def resolve_display_backend(
    requested: str | None,
    *,
    recorded: str | None = None,
    event_log: Any | None = None,
    source: str,
) -> str:
    resolved = requested or recorded or ADAPTIVE_DISPLAY_BACKEND
    reason = "explicit" if requested else ("recorded" if recorded else "default")
    if event_log is not None and reason == "default":
        event_log.write(
            "display.backend_resolved",
            requested=None,
            resolved=resolved,
            reason=reason,
            source=source,
        )
    return resolved


def resolve_restart_display_backend(spec: dict[str, Any], state: dict[str, Any], event_log: Any) -> str:
    return resolve_display_backend(
        spec.get("runtime", {}).get("display_backend"),
        recorded=state.get("display_backend"),
        event_log=event_log,
        source="restart",
    )


def display_backend_has_worker_views(display_backend: str) -> bool:
    return display_backend in DISPLAY_BACKENDS_WITH_WORKER_VIEWS


def display_backend_opens_before_leader_rebind(display_backend: str) -> bool:
    return display_backend_has_worker_views(display_backend) and display_backend != ADAPTIVE_DISPLAY_BACKEND
