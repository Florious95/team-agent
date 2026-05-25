from __future__ import annotations

from typing import Any

from team_agent.status.constants import STATUS_EVENT_LIMIT, STATUS_TEXT_LIMIT


def compact_status(data: dict[str, Any]) -> dict[str, Any]:
    return {
        "team": data.get("team"),
        "session_name": data.get("session_name"),
        "tmux_session_present": data.get("tmux_session_present"),
        "leader_receiver": compact_mapping(
            data.get("leader_receiver", {}),
            {
                "status",
                "provider",
                "mode",
                "session_name",
                "window_name",
                "pane_id",
                "pane_current_command",
            },
        ),
        "agents": {
            agent_id: compact_agent_state(agent_id, agent)
            for agent_id, agent in (data.get("agents") or {}).items()
        },
        "agent_health": data.get("agent_health", {}),
        "tasks": [compact_task(task) for task in data.get("tasks", [])],
        "messages": data.get("messages", {}),
        "queued_messages": data.get("queued_messages", [])[:8],
        "results": data.get("results", {}),
        "latest_results": data.get("latest_results", [])[:5],
        "coordinator": compact_mapping(data.get("coordinator", {}), {"status", "pid", "metadata_ok", "schema_ok"}),
        "last_events": [compact_event(event) for event in data.get("last_events", [])[-STATUS_EVENT_LIMIT:]],
    }


def compact_agent_state(agent_id: str, agent: dict[str, Any]) -> dict[str, Any]:
    display = agent.get("display") or {}
    result = compact_mapping(
        agent,
        {
            "agent_id",
            "status",
            "provider",
            "model",
            "tmux_window_present",
            "session_id",
            "captured_via",
            "attribution_confidence",
        },
    )
    result.setdefault("agent_id", agent_id)
    if display:
        result["display"] = compact_mapping(
            display,
            {
                "backend",
                "status",
                "workspace_window",
                "pane_id",
                "pid",
                "pids",
                "reason",
            },
        )
    return result


def compact_task(task: dict[str, Any]) -> dict[str, Any]:
    return compact_mapping(
        task,
        {
            "id",
            "title",
            "status",
            "assignee",
            "type",
            "risk",
            "accepted_result_id",
            "last_result_summary",
        },
    )


def compact_event(event: dict[str, Any]) -> dict[str, Any]:
    skipped = {"command", "payload", "launch_args", "content", "prompt", "developer_instructions"}
    kept = {
        "event",
        "ts",
        "agent_id",
        "task_id",
        "message_id",
        "result_id",
        "status",
        "ok",
        "reason",
        "error",
        "session",
        "window",
        "target",
        "backend",
        "workspace_window",
        "pane_id",
        "restart_mode",
        "provider",
        "delivery_status",
        "warning",
        "collected",
        "notified",
        "lock",
        "waited_sec",
        "once",
        "pid",
    }
    result: dict[str, Any] = {}
    for key, value in event.items():
        if key in skipped or key not in kept | {"agents", "coordinator"}:
            continue
        if key == "agents" and isinstance(value, list):
            result["agent_count"] = len(value)
            result["agents"] = [
                compact_mapping(item, {"agent_id", "restart_mode", "session_id"})
                for item in value[:8]
                if isinstance(item, dict)
            ]
            continue
        result[key] = compact_value(value)
    return result


def compact_mapping(source: Any, keys: set[str]) -> dict[str, Any]:
    if not isinstance(source, dict):
        return {}
    return {key: compact_value(source[key]) for key in keys if key in source}


def compact_value(value: Any) -> Any:
    if isinstance(value, str):
        return value if len(value) <= STATUS_TEXT_LIMIT else value[: STATUS_TEXT_LIMIT - 1] + "…"
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    if isinstance(value, list):
        if all(isinstance(item, (str, int, float, bool)) or item is None for item in value):
            compact = [compact_value(item) for item in value[:8]]
            if len(value) > 8:
                compact.append(f"... {len(value) - 8} more")
            return compact
        return f"{len(value)} item(s)"
    if isinstance(value, dict):
        return {
            key: compact_value(item)
            for key, item in value.items()
            if key not in {"command", "payload", "launch_args", "content", "prompt", "developer_instructions"}
        }
    return str(value)
