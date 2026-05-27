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
