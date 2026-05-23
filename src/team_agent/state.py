from __future__ import annotations

import json
import os
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import runtime_dir
from team_agent.simple_yaml import dumps


SESSION_CAPTURE_FIELDS = [
    "session_id",
    "rollout_path",
    "captured_at",
    "captured_via",
    "attribution_confidence",
]
SESSION_STATE_FIELDS = [
    *SESSION_CAPTURE_FIELDS,
    "spawn_cwd",
]


def runtime_state_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "state.json"


def normalize_agent_session_state(state: dict[str, Any]) -> None:
    agents = state.get("agents", {})
    if not isinstance(agents, dict):
        return
    for agent_state in agents.values():
        if isinstance(agent_state, dict):
            for field in SESSION_STATE_FIELDS:
                agent_state.setdefault(field, None)


def load_runtime_state(workspace: Path) -> dict[str, Any]:
    path = runtime_state_path(workspace)
    if not path.exists():
        return {"agents": {}, "tasks": [], "session_name": None}
    state = json.loads(path.read_text(encoding="utf-8"))
    normalize_agent_session_state(state)
    return state


def save_runtime_state(workspace: Path, state: dict[str, Any]) -> None:
    path = runtime_state_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        tmp_path.write_text(json.dumps(state, indent=2, ensure_ascii=False), encoding="utf-8")
        os.replace(tmp_path, path)
    finally:
        tmp_path.unlink(missing_ok=True)


def write_team_state(workspace: Path, spec: dict[str, Any], runtime: dict[str, Any], results: list[dict[str, Any]] | None = None) -> Path:
    path = workspace / spec.get("context", {}).get("state_file", "team_state.md")
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Team State",
        "",
        f"Updated: {datetime.now(timezone.utc).isoformat()}",
        "",
        "## Objective",
        "",
        spec.get("team", {}).get("objective", ""),
        "",
        "## Team",
        "",
        f"- Name: {spec.get('team', {}).get('name')}",
        f"- Runtime session: {runtime.get('session_name')}",
    ]
    receiver = runtime.get("leader_receiver") or {}
    if receiver:
        if receiver.get("mode") == "direct_tmux":
            lines.append(
                f"- Leader receiver: direct tmux {receiver.get('pane_id')} "
                f"({receiver.get('provider')}, {receiver.get('status')})"
            )
        else:
            lines.append(f"- Leader inbox fallback: {receiver.get('session')}:{receiver.get('window')} ({receiver.get('status')})")
            lines.append(f"- Leader inbox log: {receiver.get('path')}")
    lines.extend(["", "## Agents", ""])
    for agent in spec.get("agents", []):
        status = runtime.get("agents", {}).get(agent["id"], {}).get("status", "unknown")
        lines.append(f"- {agent['id']}: {agent['role']} on {agent['provider']} ({status})")
    lines.extend(["", "## Task Graph", ""])
    for task in runtime.get("tasks", spec.get("tasks", [])):
        deps = ", ".join(task.get("deps", [])) or "none"
        assignee = task.get("assignee") or "unassigned"
        lines.append(f"- {task['id']} [{task.get('status', 'pending')}], assignee={assignee}, deps={deps}: {task['title']}")
        if task.get("last_result_summary"):
            lines.append(f"  Summary: {task['last_result_summary']}")
        if task.get("artifact_refs"):
            for ref in task["artifact_refs"]:
                if isinstance(ref, dict):
                    lines.append(f"  Artifact: {ref.get('path')} - {ref.get('description', '')}")
                else:
                    lines.append(f"  Artifact: INVALID artifact ref {ref!r}")
    lines.extend(["", "## Latest Results", ""])
    for result in results or []:
        envelope = json.loads(result["envelope"]) if isinstance(result.get("envelope"), str) else result
        lines.append(f"- {envelope.get('task_id')} from {envelope.get('agent_id')}: {envelope.get('status')} - {envelope.get('summary')}")
    lines.extend(["", "## Blockers", ""])
    blockers = [
        task
        for task in runtime.get("tasks", spec.get("tasks", []))
        if task.get("status") in {"blocked", "failed", "needs_retry"}
    ]
    if blockers:
        for task in blockers:
            lines.append(f"- {task['id']}: {task.get('last_result_summary', task.get('title'))}")
    else:
        lines.append("- None")
    lines.extend(["", "## Next Step", "", "- Continue routing ready tasks and collect result envelopes."])
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def write_spec(path: Path, spec: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    tmp_path.write_text(dumps(spec), encoding="utf-8")
    os.replace(tmp_path, path)
