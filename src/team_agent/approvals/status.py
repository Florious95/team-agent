from __future__ import annotations

import hashlib
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.approvals.parsing import capture_has_approval_prompt
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import team_state_key


def refresh_agent_runtime_statuses(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    from team_agent.runtime import _tmux_session_exists, _tmux_window_exists
    _ = workspace
    session_name = state.get("session_name")
    tmux_exists = _tmux_session_exists(session_name) if session_name else False
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped"}:
            continue
        old_status = agent_state.get("status")
        window = agent_state.get("window", agent_id)
        window_present = _tmux_window_exists(session_name, window) if tmux_exists else False
        agent_state["tmux_window_present"] = window_present
        if not window_present:
            if session_name:
                agent_state["status"] = "missing"
        else:
            detected = detect_provider_status(agent_state["provider"], session_name, window)
            if detected:
                agent_state["status"] = detected
            else:
                agent_state.setdefault("status", "running")
        if old_status != agent_state.get("status"):
            event_log.write(
                "runtime.status_detected",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                old_status=old_status,
                status=agent_state.get("status"),
            )


def sync_agent_health(workspace: Path, state: dict[str, Any], store: MessageStore | None = None) -> dict[str, dict[str, Any]]:
    from team_agent.runtime import _tmux_window_exists, _tmux_pane_info, run_cmd
    from team_agent.messaging.activity_detector import classify_agent_activity
    store = store or MessageStore(workspace)
    session_name = state.get("session_name")
    owner_team_id = team_state_key(state)
    captures: dict[str, dict[str, Any]] = {}
    for agent_id, agent_state in state.get("agents", {}).items():
        health_status = agent_health_status(agent_state)
        last_output_at = agent_state.get("last_output_at")
        window = agent_state.get("window", agent_id)
        scrollback = ""
        pane_info: dict[str, Any] | None = None
        if session_name and _tmux_window_exists(session_name, window):
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-40", "-t", f"{session_name}:{window}"], timeout=5)
            if proc.returncode == 0:
                scrollback = proc.stdout
                digest = hashlib.sha256(proc.stdout.encode("utf-8", errors="ignore")).hexdigest()
                if digest != agent_state.get("last_output_hash"):
                    last_output_at = datetime.now(timezone.utc).isoformat()
                    agent_state["last_output_hash"] = digest
                    agent_state["last_output_at"] = last_output_at
                if capture_has_approval_prompt(proc.stdout):
                    health_status = "AWAITING_APPROVAL"
            try:
                pane_info = _tmux_pane_info(f"{session_name}:{window}")
            except Exception:
                pane_info = None
            if scrollback and health_status != "AWAITING_APPROVAL":
                activity = classify_agent_activity(
                    agent_id,
                    agent_state.get("provider") or "",
                    last_output_at,
                    pane_info,
                    scrollback,
                )
                if activity.get("confidence", 0) >= 0.85:
                    raw = str(activity.get("status") or "")
                    mapping = {"idle": "IDLE", "working": "RUNNING", "stuck": "STUCK", "uncertain": "UNCERTAIN"}
                    mapped = mapping.get(raw)
                    if mapped:
                        health_status = mapped
        current_task = current_task_for_agent(state.get("tasks", []), agent_id)
        store.upsert_agent_health(
            agent_id,
            health_status,
            last_output_at=last_output_at,
            context_usage_pct=agent_state.get("context_usage_pct"),
            current_task_id=current_task,
            owner_team_id=owner_team_id,
        )
        captures[agent_id] = {"scrollback": scrollback, "pane_info": pane_info, "health_status": health_status}
    return captures


def agent_health_status(agent_state: dict[str, Any]) -> str:
    raw = str(agent_state.get("status") or "").lower()
    if raw in {"busy", "running"}:
        return "RUNNING" if raw == "busy" else "IDLE"
    if raw in {"paused", "blocked"}:
        return "BLOCKED"
    if raw in {"error", "missing", "interrupted"}:
        return "ERROR"
    if raw in {"stopped", "done"}:
        return "DONE"
    return "IDLE"


def current_task_for_agent(tasks: list[dict[str, Any]], agent_id: str) -> str | None:
    active = {"pending", "ready", "running", "blocked", "needs_retry"}
    for task in reversed(tasks):
        if task.get("assignee") == agent_id and task.get("status", "pending") in active:
            return task.get("id")
    return None


def age_text(iso_text: str | None) -> str:
    if not iso_text:
        return "-"
    try:
        dt = datetime.fromisoformat(iso_text)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        seconds = max(0, int((datetime.now(timezone.utc) - dt).total_seconds()))
    except ValueError:
        return "-"
    if seconds < 60:
        return f"{seconds}s ago"
    minutes = seconds // 60
    if minutes < 60:
        return f"{minutes}m ago"
    return f"{minutes // 60}h ago"


def detect_provider_status(provider: str, session_name: str, window: str) -> str | None:
    from team_agent.runtime import get_adapter, run_cmd
    proc = run_cmd(["tmux", "capture-pane", "-p", "-t", f"{session_name}:{window}"], timeout=5)
    if proc.returncode != 0:
        return None
    patterns = get_adapter(provider).status_patterns()
    positions: dict[str, int] = {}
    for status_name, pattern in patterns.items():
        if not pattern:
            continue
        try:
            matches = list(re.finditer(pattern, proc.stdout, re.MULTILINE))
        except re.error:
            continue
        if matches:
            positions[status_name] = matches[-1].start()
    if not positions:
        return None
    latest = max(positions, key=positions.get)
    return {"idle": "running", "processing": "busy", "error": "error"}.get(latest)
