from __future__ import annotations

import copy
import json
import os
import re
import subprocess
import time
from datetime import datetime, timedelta, timezone
from typing import Any

from team_agent import runtime as _runtime
from team_agent.errors import RuntimeError, ValidationError
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.paths import runtime_dir
from team_agent.permissions import missing_tools
from team_agent.routing import route_task
from team_agent.spec import load_spec, validate_result_envelope
from team_agent.state import load_runtime_state, save_runtime_state, write_team_state
from team_agent.task_graph import update_task_status

# Explicit runtime dependency surface for messaging extraction. Wrappers keep
# runtime monkeypatch points stable while avoiding module-wide globals sync.
DELIVERY_CAPTURE_LINES = _runtime.DELIVERY_CAPTURE_LINES
PASTED_CONTENT_PROMPT_RE = _runtime.PASTED_CONTENT_PROMPT_RE
TMUX_PANE_FORMAT = _runtime.TMUX_PANE_FORMAT
TMUX_PASTE_BYTES_PER_SECOND = _runtime.TMUX_PASTE_BYTES_PER_SECOND
TMUX_PASTE_MAX_READY_TIMEOUT = _runtime.TMUX_PASTE_MAX_READY_TIMEOUT
TMUX_PASTE_MIN_READY_TIMEOUT = _runtime.TMUX_PASTE_MIN_READY_TIMEOUT
TMUX_STDIN_BUFFER_THRESHOLD = _runtime.TMUX_STDIN_BUFFER_THRESHOLD
TMUX_SUBMIT_BYTES_PER_SECOND = _runtime.TMUX_SUBMIT_BYTES_PER_SECOND
TMUX_SUBMIT_MAX_SETTLE_TIMEOUT = _runtime.TMUX_SUBMIT_MAX_SETTLE_TIMEOUT
TMUX_SUBMIT_MIN_SETTLE_TIMEOUT = _runtime.TMUX_SUBMIT_MIN_SETTLE_TIMEOUT

def _capture_has_pasted_content_prompt(*args: Any, **kwargs: Any) -> Any:
    return _runtime._capture_has_pasted_content_prompt(*args, **kwargs)

def _capture_missing_sessions(*args: Any, **kwargs: Any) -> Any:
    return _runtime._capture_missing_sessions(*args, **kwargs)

def _capture_tmux_pane_text(*args: Any, **kwargs: Any) -> Any:
    return _runtime._capture_tmux_pane_text(*args, **kwargs)

def _choose_leader_submit_key(*args: Any, **kwargs: Any) -> Any:
    return _runtime._choose_leader_submit_key(*args, **kwargs)

def _current_task_for_agent(*args: Any, **kwargs: Any) -> Any:
    return _runtime._current_task_for_agent(*args, **kwargs)

def _deliver_pending_message(*args: Any, **kwargs: Any) -> Any:
    return _runtime._deliver_pending_message(*args, **kwargs)

def _deliver_pending_messages(*args: Any, **kwargs: Any) -> Any:
    return _runtime._deliver_pending_messages(*args, **kwargs)

def _find_agent(*args: Any, **kwargs: Any) -> Any:
    return _runtime._find_agent(*args, **kwargs)

def _find_task(*args: Any, **kwargs: Any) -> Any:
    return _runtime._find_task(*args, **kwargs)

def _find_task_or_none(*args: Any, **kwargs: Any) -> Any:
    return _runtime._find_task_or_none(*args, **kwargs)

def _format_team_agent_message(*args: Any, **kwargs: Any) -> Any:
    return _runtime._format_team_agent_message(*args, **kwargs)

def _handle_provider_runtime_prompts(*args: Any, **kwargs: Any) -> Any:
    return _runtime._handle_provider_runtime_prompts(*args, **kwargs)

def _handle_provider_startup_prompts(*args: Any, **kwargs: Any) -> Any:
    return _runtime._handle_provider_startup_prompts(*args, **kwargs)

def _is_leader_sender(*args: Any, **kwargs: Any) -> Any:
    return _runtime._is_leader_sender(*args, **kwargs)

def _is_leader_target(*args: Any, **kwargs: Any) -> Any:
    return _runtime._is_leader_target(*args, **kwargs)

def _is_message_scoped_result(*args: Any, **kwargs: Any) -> Any:
    return _runtime._is_message_scoped_result(*args, **kwargs)

def _is_runtime_team_agent(*args: Any, **kwargs: Any) -> Any:
    return _runtime._is_runtime_team_agent(*args, **kwargs)

def _leader_id(*args: Any, **kwargs: Any) -> Any:
    return _runtime._leader_id(*args, **kwargs)

def _leader_receiver_is_direct(*args: Any, **kwargs: Any) -> Any:
    return _runtime._leader_receiver_is_direct(*args, **kwargs)

def _message_by_id(*args: Any, **kwargs: Any) -> Any:
    return _runtime._message_by_id(*args, **kwargs)

def _message_payload(*args: Any, **kwargs: Any) -> Any:
    return _runtime._message_payload(*args, **kwargs)

def _mirror_peer_message_to_leader(*args: Any, **kwargs: Any) -> Any:
    return _runtime._mirror_peer_message_to_leader(*args, **kwargs)

def _notify_leader_of_report_result(*args: Any, **kwargs: Any) -> Any:
    return _runtime._notify_leader_of_report_result(*args, **kwargs)

def _rediscover_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    return _runtime._rediscover_leader_receiver(*args, **kwargs)

def _refresh_agent_runtime_statuses(*args: Any, **kwargs: Any) -> Any:
    return _runtime._refresh_agent_runtime_statuses(*args, **kwargs)

def _result_status_to_task_status(*args: Any, **kwargs: Any) -> Any:
    return _runtime._result_status_to_task_status(*args, **kwargs)

def _runtime_lock(*args: Any, **kwargs: Any) -> Any:
    return _runtime._runtime_lock(*args, **kwargs)

def _runtime_team_agent_ids(*args: Any, **kwargs: Any) -> Any:
    return _runtime._runtime_team_agent_ids(*args, **kwargs)

def _send_to_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    return _runtime._send_to_leader_receiver(*args, **kwargs)

def _submit_worker_prompt(*args: Any, **kwargs: Any) -> Any:
    return _runtime._submit_worker_prompt(*args, **kwargs)

def _tmux_inject_text(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_inject_text(*args, **kwargs)

def _tmux_current_client_pane_info(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_current_client_pane_info(*args, **kwargs)

def _tmux_list_panes(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_list_panes(*args, **kwargs)

def _infer_active_tmux_pane(*args: Any, **kwargs: Any) -> Any:
    return _runtime._infer_active_tmux_pane(*args, **kwargs)

def _tmux_pane_info(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_pane_info(*args, **kwargs)

def _infer_workspace_tmux_pane(*args: Any, **kwargs: Any) -> Any:
    return _runtime._infer_workspace_tmux_pane(*args, **kwargs)

def _tmux_load_buffer_stdin(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_load_buffer_stdin(*args, **kwargs)

def _tmux_paste_ready_timeout(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_paste_ready_timeout(*args, **kwargs)

def _tmux_set_buffer_text(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_set_buffer_text(*args, **kwargs)

def _tmux_submit_settle_timeout(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_submit_settle_timeout(*args, **kwargs)

def _tmux_window_exists(*args: Any, **kwargs: Any) -> Any:
    return _runtime._tmux_window_exists(*args, **kwargs)

def _validate_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    return _runtime._validate_leader_receiver(*args, **kwargs)

def _wait_for_message_ready(*args: Any, **kwargs: Any) -> Any:
    return _runtime._wait_for_message_ready(*args, **kwargs)

def _wait_for_worker_message_ready(*args: Any, **kwargs: Any) -> Any:
    return _runtime._wait_for_worker_message_ready(*args, **kwargs)

def run_cmd(*args: Any, **kwargs: Any) -> Any:
    return _runtime.run_cmd(*args, **kwargs)

def core_list_targets(*args: Any, **kwargs: Any) -> Any:
    return _runtime.core_list_targets(*args, **kwargs)

def core_render_message(*args: Any, **kwargs: Any) -> Any:
    return _runtime.core_render_message(*args, **kwargs)

def send_message(*args: Any, **kwargs: Any) -> Any:
    return _runtime.send_message(*args, **kwargs)

def start_coordinator(*args: Any, **kwargs: Any) -> Any:
    return _runtime.start_coordinator(*args, **kwargs)

__all__ = ['DELIVERY_CAPTURE_LINES', 'EventLog', 'MessageStore', 'PASTED_CONTENT_PROMPT_RE', 'RuntimeError', 'TMUX_PANE_FORMAT', 'TMUX_PASTE_BYTES_PER_SECOND', 'TMUX_PASTE_MAX_READY_TIMEOUT', 'TMUX_PASTE_MIN_READY_TIMEOUT', 'TMUX_STDIN_BUFFER_THRESHOLD', 'TMUX_SUBMIT_BYTES_PER_SECOND', 'TMUX_SUBMIT_MAX_SETTLE_TIMEOUT', 'TMUX_SUBMIT_MIN_SETTLE_TIMEOUT', 'ValidationError', '_capture_has_pasted_content_prompt', '_capture_missing_sessions', '_capture_tmux_pane_text', '_choose_leader_submit_key', '_current_task_for_agent', '_deliver_pending_message', '_deliver_pending_messages', '_find_agent', '_find_task', '_find_task_or_none', '_format_team_agent_message', '_handle_provider_runtime_prompts', '_handle_provider_startup_prompts', '_infer_active_tmux_pane', '_infer_workspace_tmux_pane', '_is_leader_sender', '_is_leader_target', '_is_message_scoped_result', '_is_runtime_team_agent', '_leader_id', '_leader_receiver_is_direct', '_message_by_id', '_message_payload', '_mirror_peer_message_to_leader', '_notify_leader_of_report_result', '_rediscover_leader_receiver', '_refresh_agent_runtime_statuses', '_result_status_to_task_status', '_runtime_lock', '_runtime_team_agent_ids', '_send_to_leader_receiver', '_submit_worker_prompt', '_tmux_current_client_pane_info', '_tmux_inject_text', '_tmux_list_panes', '_tmux_load_buffer_stdin', '_tmux_pane_info', '_tmux_paste_ready_timeout', '_tmux_set_buffer_text', '_tmux_submit_settle_timeout', '_tmux_window_exists', '_validate_leader_receiver', '_wait_for_message_ready', '_wait_for_worker_message_ready', 'copy', 'core_list_targets', 'core_render_message', 'datetime', 'json', 'load_runtime_state', 'load_spec', 'missing_tools', 'os', 're', 'route_task', 'run_cmd', 'runtime_dir', 'save_runtime_state', 'send_message', 'start_coordinator', 'subprocess', 'time', 'timedelta', 'timezone', 'update_task_status', 'validate_result_envelope', 'write_team_state']
