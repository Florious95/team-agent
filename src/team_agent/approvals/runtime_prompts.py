from __future__ import annotations

import time
from pathlib import Path
from typing import Any

from team_agent.approvals.constants import (
    INTERNAL_MCP_AUTO_APPROVE_TOOLS,
    STARTUP_PROMPT_RUNTIME_CHECK_LIMIT,
)
from team_agent.approvals.parsing import (
    approval_choice_keys,
    approval_prompt_fingerprint,
    choose_internal_mcp_approval_choice,
    extract_approval_prompt,
)
from team_agent.events import EventLog
from team_agent.status import APPROVAL_SCAN_LINES


def handle_provider_runtime_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    from team_agent.runtime import _tmux_session_exists, _tmux_window_exists, get_adapter
    _ = workspace
    session_name = state.get("session_name")
    if not session_name or not _tmux_session_exists(session_name):
        return
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped", "missing"}:
            continue
        window = agent_state.get("window", agent_id)
        if not _tmux_window_exists(session_name, window):
            continue
        internal_mcp = handle_internal_mcp_approval_prompt(agent_id, session_name, window, event_log)
        if internal_mcp is not None:
            continue
        adapter = get_adapter(agent_state["provider"])
        for prompt_event in adapter.handle_runtime_prompts(session_name, window):
            event_log.write(
                "runtime.prompt_handled",
                agent_id=agent_id,
                provider=agent_state["provider"],
                **prompt_event,
            )


def handle_provider_startup_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    from team_agent.runtime import _tmux_session_exists, _tmux_window_exists, get_adapter
    _ = workspace
    session_name = state.get("session_name")
    if not session_name or not _tmux_session_exists(session_name):
        return
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped", "missing"}:
            continue
        window = agent_state.get("window", agent_id)
        if not _tmux_window_exists(session_name, window):
            continue
        spawned_at = str(agent_state.get("spawned_at") or "")
        if agent_state.get("startup_prompt_check_spawned_at") != spawned_at:
            agent_state["startup_prompt_check_spawned_at"] = spawned_at
            agent_state["startup_prompt_check_count"] = 0
        check_count = int(agent_state.get("startup_prompt_check_count") or 0)
        if check_count >= STARTUP_PROMPT_RUNTIME_CHECK_LIMIT:
            continue
        agent_state["startup_prompt_check_count"] = check_count + 1
        adapter = get_adapter(agent_state["provider"])
        for prompt_event in adapter.handle_startup_prompts(session_name, window, checks=20, sleep_s=0.5):
            event_log.write(
                "runtime.startup_prompt_handled",
                agent_id=agent_id,
                provider=agent_state["provider"],
                **prompt_event,
            )


def handle_internal_mcp_approval_prompt(
    agent_id: str,
    session_name: str,
    window: str,
    event_log: EventLog,
) -> dict[str, Any] | None:
    from team_agent.runtime import run_cmd
    target = f"{session_name}:{window}"
    proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", target], timeout=5)
    if proc.returncode != 0:
        return None
    prompt = extract_approval_prompt(agent_id, proc.stdout)
    if not prompt or prompt.get("kind") != "mcp_tool":
        return None
    tool = str(prompt.get("tool") or "")
    fingerprint = approval_prompt_fingerprint(prompt)
    if tool not in INTERNAL_MCP_AUTO_APPROVE_TOOLS:
        result = {
            "ok": False,
            "action": "skipped",
            "reason": "tool_not_allowlisted",
            "tool": tool,
            "fingerprint": fingerprint,
        }
        event_log.write("runtime.internal_mcp_approval.skipped", agent_id=agent_id, **result)
        return result
    result = submit_internal_mcp_approval(agent_id, target, tool, prompt, proc.stdout)
    event_log.write("runtime.internal_mcp_approval.auto", agent_id=agent_id, **result)
    return result


def submit_internal_mcp_approval(
    agent_id: str,
    target: str,
    tool: str,
    prompt: dict[str, Any],
    capture_text: str,
    attempts: int = 3,
) -> dict[str, Any]:
    from team_agent.runtime import run_cmd
    choice = choose_internal_mcp_approval_choice(prompt)
    fingerprint = approval_prompt_fingerprint(prompt)
    attempt_log: list[dict[str, Any]] = []
    current_prompt = prompt
    current_capture = capture_text
    for attempt in range(1, attempts + 1):
        keys = approval_choice_keys(current_prompt, current_capture, choice)
        proc = run_cmd(["tmux", "send-keys", "-t", target, *keys], timeout=10)
        if proc.returncode != 0:
            return {
                "ok": False,
                "action": "auto_approve",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": False, "error": proc.stderr.strip()}],
                "verification": "send_keys_failed",
            }
        time.sleep(0.35)
        verify = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", target], timeout=5)
        if verify.returncode != 0:
            attempt_log.append({"attempt": attempt, "submitted": True, "keys": keys, "verification": "capture_failed"})
            continue
        after_prompt = extract_approval_prompt(agent_id, verify.stdout)
        if not after_prompt:
            return {
                "ok": True,
                "action": "auto_approved",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": True, "keys": keys, "verification": "prompt_absent"}],
                "verification": "prompt_absent_after_submit",
            }
        if after_prompt.get("kind") != "mcp_tool" or after_prompt.get("tool") != tool:
            return {
                "ok": True,
                "action": "auto_approved",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": True, "keys": keys, "verification": "different_prompt_present"}],
                "verification": "original_prompt_replaced",
            }
        attempt_log.append({"attempt": attempt, "submitted": True, "keys": keys, "verification": "prompt_still_present"})
        current_prompt = after_prompt
        current_capture = verify.stdout
    return {
        "ok": False,
        "action": "auto_approve",
        "tool": tool,
        "choice": choice,
        "fingerprint": fingerprint,
        "attempts": attempt_log,
        "verification": "prompt_still_present_after_retries",
    }
