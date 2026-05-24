from __future__ import annotations

from team_agent.display.close import close_ghostty_display, close_ghostty_workspace
from team_agent.display.ghostty import (
    ghostty_app_exists,
    ghostty_attach_args,
    ghostty_command,
    ghostty_display_session_name,
    ghostty_pids_by_title,
    prepare_ghostty_display_session,
)
from team_agent.display.worker_window import (
    open_ghostty_worker_window,
    open_worker_displays,
)
from team_agent.display.workspace import (
    GHOSTTY_WORKSPACE_PANES_PER_WINDOW,
    ghostty_workspace_aggregator_name,
    ghostty_workspace_blocked,
    ghostty_workspace_pane_command,
    ghostty_workspace_pane_title,
    ghostty_workspace_partial_update_display,
    ghostty_workspace_window_name,
    kill_ghostty_workspace_linked_sessions,
    open_ghostty_workspace,
    open_ghostty_workspace_agent_display,
    prepare_ghostty_workspace_aggregator,
    prepare_ghostty_workspace_linked_sessions,
    set_ghostty_workspace_pane_title,
)

__all__ = [
    "GHOSTTY_WORKSPACE_PANES_PER_WINDOW",
    "close_ghostty_display",
    "close_ghostty_workspace",
    "ghostty_app_exists",
    "ghostty_attach_args",
    "ghostty_command",
    "ghostty_display_session_name",
    "ghostty_pids_by_title",
    "ghostty_workspace_aggregator_name",
    "ghostty_workspace_blocked",
    "ghostty_workspace_pane_command",
    "ghostty_workspace_pane_title",
    "ghostty_workspace_partial_update_display",
    "ghostty_workspace_window_name",
    "kill_ghostty_workspace_linked_sessions",
    "open_ghostty_worker_window",
    "open_ghostty_workspace",
    "open_ghostty_workspace_agent_display",
    "open_worker_displays",
    "prepare_ghostty_display_session",
    "prepare_ghostty_workspace_aggregator",
    "prepare_ghostty_workspace_linked_sessions",
    "set_ghostty_workspace_pane_title",
]
