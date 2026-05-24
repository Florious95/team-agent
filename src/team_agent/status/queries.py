from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import load_runtime_state, save_runtime_state
from team_agent.status.compact import compact_status
from team_agent.status.constants import PENDING_DELIVERY_STATUSES


def status(workspace: Path, as_json: bool = False, *, compact: bool = False) -> dict[str, Any]:
    from team_agent.runtime import (
        _capture_missing_sessions,
        _handle_provider_startup_prompts,
        _refresh_agent_runtime_statuses,
        _sync_agent_health,
        _tmux_session_exists,
        coordinator_health,
    )
    _ = as_json
    state = load_runtime_state(workspace)
    store = MessageStore(workspace)
    event_log = EventLog(workspace)
    _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)
    _refresh_agent_runtime_statuses(workspace, state, event_log)
    _handle_provider_startup_prompts(workspace, state, event_log)
    _sync_agent_health(workspace, state, store)
    save_runtime_state(workspace, state)
    session_name = state.get("session_name")
    tmux_exists = _tmux_session_exists(session_name) if session_name else False
    result = {
        "team": state.get("leader", {}).get("id", "leader"),
        "session_name": session_name,
        "tmux_session_present": tmux_exists,
        "leader_receiver": state.get("leader_receiver", {}),
        "agents": state.get("agents", {}),
        "agent_health": store.agent_health(),
        "tasks": state.get("tasks", []),
        "messages": store.message_counts(),
        "queued_messages": queued_message_statuses(store.messages()),
        "results": store.result_counts(),
        "latest_results": latest_result_summaries(store),
        "coordinator": coordinator_health(workspace),
        "last_events": EventLog(workspace).tail(10),
    }
    return compact_status(result) if compact else result


def latest_result_summaries(store: MessageStore, limit: int = 5) -> list[dict[str, Any]]:
    summaries: list[dict[str, Any]] = []
    for row in store.latest_results(limit=limit):
        summary = result_summary_from_row(row)
        if summary:
            summaries.append(summary)
    return summaries


def result_summary_from_row(row: dict[str, Any]) -> dict[str, Any] | None:
    try:
        envelope = json.loads(row["envelope"]) if isinstance(row.get("envelope"), str) else row.get("envelope")
    except (TypeError, json.JSONDecodeError):
        return None
    if not isinstance(envelope, dict):
        return None
    return {
        "result_id": row.get("result_id"),
        "task_id": envelope.get("task_id") or row.get("task_id"),
        "agent_id": envelope.get("agent_id") or row.get("agent_id"),
        "status": envelope.get("status") or row.get("status"),
        "summary": envelope.get("summary"),
        "created_at": row.get("created_at"),
    }


def queued_message_statuses(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    from team_agent.runtime import _age_text
    visible_statuses = PENDING_DELIVERY_STATUSES | {"target_resolved", "delivery_blocked", "injected_unverified"}
    queued: list[dict[str, Any]] = []
    for row in messages:
        if row.get("status") not in visible_statuses:
            continue
        queued.append(
            {
                "message_id": row.get("message_id"),
                "recipient": row.get("recipient"),
                "sender": row.get("sender"),
                "status": row.get("status"),
                "reason": row.get("error"),
                "age": _age_text(row.get("created_at")),
                "attempts": row.get("delivery_attempts") or 0,
            }
        )
    return queued


def format_status(workspace: Path, agent_id: str | None = None) -> str:
    from team_agent.runtime import RuntimeError, _agent_health_status, _age_text, _current_task_for_agent
    data = status(workspace, as_json=True)
    health = data.get("agent_health", {})
    tasks = data.get("tasks", [])
    if agent_id:
        if agent_id not in data.get("agents", {}) and agent_id not in health:
            raise RuntimeError(f"unknown agent id: {agent_id}")
        agent = data.get("agents", {}).get(agent_id, {})
        row = health.get(agent_id, {})
        task_id = _current_task_for_agent(tasks, agent_id) or "-"
        inbox_rows = MessageStore(workspace).inbox(agent_id, limit=3)
        lines = [
            f"{agent_id}  {row.get('status', _agent_health_status(agent))}",
            f"  provider: {agent.get('provider', '-')}",
            f"  model: {agent.get('model', '-')}",
            f"  profile: {agent.get('profile', '-')}",
            f"  session_id: {agent.get('session_id') or '-'}",
            f"  captured_via: {agent.get('captured_via') or '-'}",
            f"  attribution_confidence: {agent.get('attribution_confidence') or '-'}",
            f"  task: {task_id}",
            f"  handoff: {agent.get('handoff_path', '-')}",
            "  recent messages:",
        ]
        if inbox_rows:
            for item in inbox_rows:
                lines.append(
                    f"    {item['created_at']} {item['sender']} -> {item['recipient']} "
                    f"{item['status']}: {item['content'][:120]}"
                )
        else:
            lines.append("    none")
        return "\n".join(lines)

    agents = data.get("agents", {})
    state_name = "up" if data.get("tmux_session_present") else "down"
    results = data.get("results", {})
    lines = [
        f"team {data.get('session_name') or '-'} ({state_name})",
        (
            "results "
            f"total {results.get('total', 0)} "
            f"uncollected {results.get('uncollected', 0)} "
            f"collected {results.get('collected', 0)} "
            f"invalid {results.get('invalid', 0)}"
        ),
    ]
    if results.get("uncollected", 0):
        lines.append("  final result pending in result store; run team-agent collect")
    queued_messages = data.get("queued_messages") or []
    if queued_messages:
        lines.append("queued messages")
        for item in queued_messages[:8]:
            reason = item.get("reason") or "-"
            lines.append(
                f"  {item.get('message_id')} -> {item.get('recipient')} "
                f"{item.get('status')} age {item.get('age')} attempts {item.get('attempts')} reason {reason}"
            )
    for aid in sorted(agents):
        agent = agents[aid]
        row = health.get(aid, {})
        status_value = row.get("status") or _agent_health_status(agent)
        task_id = _current_task_for_agent(tasks, aid) or "-"
        context = row.get("context_usage_pct")
        context_text = f"ctx {context}%" if context is not None else "ctx -"
        last = _age_text(row.get("last_output_at"))
        session_text = f"sid {agent.get('session_id') or '-'}"
        capture_text = f"via {agent.get('captured_via') or '-'} {agent.get('attribution_confidence') or '-'}"
        lines.append(f"  {aid}  {status_value}  {task_id}  {context_text}  last {last}  {session_text}  {capture_text}")
    return "\n".join(lines)
