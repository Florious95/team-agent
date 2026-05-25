from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.spec import load_spec
from team_agent.state import load_runtime_state


def sessions_overview(workspace: Path) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path) if spec_path.exists() else {}
    tasks = state.get("tasks", [])
    rows = []
    for agent in spec.get("agents", []):
        agent_state = state.get("agents", {}).get(agent["id"], {})
        last_task = next((task.get("id") for task in reversed(tasks) if task.get("assignee") == agent["id"]), None)
        rows.append(
            {
                "agent_id": agent["id"],
                "provider": agent.get("provider"),
                "model": agent.get("model"),
                "profile": agent.get("profile"),
                "session_id": agent_state.get("session_id"),
                "resume_id": agent_state.get("resume_id"),
                "rollout_path": agent_state.get("rollout_path"),
                "captured_at": agent_state.get("captured_at"),
                "captured_via": agent_state.get("captured_via"),
                "attribution_confidence": agent_state.get("attribution_confidence"),
                "spawn_cwd": agent_state.get("spawn_cwd"),
                "context_usage": agent_state.get("context_usage"),
                "status": agent_state.get("status", "unknown"),
                "last_task": last_task,
                "handoff_path": agent_state.get("handoff_path"),
                "display_target": agent_state.get("display"),
                "terminal_target": {
                    "session": state.get("session_name"),
                    "window": agent_state.get("window", agent["id"]),
                    "pane": agent_state.get("pane_id"),
                },
            }
        )
    return {"ok": True, "sessions": rows, "workspace": str(workspace)}
