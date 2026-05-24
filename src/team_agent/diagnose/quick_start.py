from __future__ import annotations

import json
import shutil
import time
from pathlib import Path
from typing import Any

from team_agent.diagnose.preflight import ensure_profiles_for_roles, preflight
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.paths import logs_dir, team_workspace
from team_agent.spec import load_spec
from team_agent.state import load_runtime_state, save_runtime_state, write_team_state
from team_agent.task_graph import TASK_STATUSES


def quick_start(
    agents_dir: Path,
    name: str | None = None,
    yes: bool = False,
    fresh: bool = False,
    team_id: str | None = None,
) -> dict[str, Any]:
    from team_agent.runtime import (
        RuntimeError,
        _compile_team_dir_spec,
        _quick_start_existing_context,
        ensure_workspace_dirs,
        launch,
        start_coordinator,
    )

    team_dir = prepare_quick_start_team(agents_dir.resolve(), Path.cwd().resolve(), name, team_id=team_id)
    workspace = team_workspace(team_dir)
    ensure_workspace_dirs(workspace)
    ensure_profiles_for_roles(team_dir)
    compiled = _compile_team_dir_spec(team_dir, workspace)
    spec_path = team_dir / "team.spec.yaml"
    existing = _quick_start_existing_context(workspace, compiled["spec"]["runtime"]["session_name"])
    if existing and not fresh:
        return {
            "ok": False,
            "step": "existing_runtime_state",
            "summary": (
                "quick-start would start fresh workers from role docs for an existing team. "
                "Use restart to continue the previous worker context, or pass --fresh to intentionally start new workers."
            ),
            "team": existing.get("team_name"),
            "session_name": existing.get("session_name"),
            "state_path": existing.get("state_path"),
            "next_actions": [
                f"team-agent restart {workspace} --team {existing.get('session_name')}",
                f"team-agent quick-start {team_dir} --fresh",
            ],
        }
    preflight_result = preflight(team_dir)
    if not preflight_result.get("ok"):
        return {
            "ok": False,
            "step": "preflight",
            "summary": preflight_result.get("summary"),
            "details_log": preflight_result.get("details_log"),
            "blockers": preflight_result.get("blockers", []),
            "next_actions": preflight_result.get("next_actions", []),
            "checks": preflight_result.get("checks", []),
        }
    dangerous = bool(compiled["spec"].get("runtime", {}).get("dangerous_auto_approve"))
    if dangerous and not yes:
        raise RuntimeError("quick-start requires --yes when dangerous_auto_approve is true")
    launched = launch(spec_path, auto_approve=True, skip_profile_smoke=True)
    from team_agent.leader import autobind_leader_receiver_from_env
    leader_provider = str(compiled["spec"].get("leader", {}).get("provider") or "codex")
    autobind_leader_receiver_from_env(workspace, leader_provider, source="quick_start")
    coordinator = start_coordinator(workspace)
    ready = wait_ready(workspace, timeout=120)
    summary = (
        f"team {compiled['spec']['team']['name']} ready: "
        f"{len(launched.get('agents', []))} agent"
        f"{'' if len(launched.get('agents', [])) == 1 else 's'} "
        f"in session {launched.get('session_name')} (coordinator pid {coordinator.get('pid')})"
    )
    ready_signal = (
        "quick-start completed; workers are ready. "
        "Do not wait, sleep, or poll status after this success line unless diagnosing a failure."
    )
    details_log = logs_dir(workspace) / f"quick-start-{int(time.time())}.json"
    details_log.write_text(
        json.dumps(
            {
                "team_dir": str(team_dir),
                "preflight": preflight_result,
                "compile": compiled,
                "launch": launched,
                "ready": ready,
                "coordinator": coordinator,
            },
            indent=2,
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    return {
        "ok": bool(launched.get("ok") and ready.get("ok") and coordinator.get("ok")),
        "summary": summary,
        "ready_signal": ready_signal,
        "next_actions": ["Dispatch work with team-agent send, or return control to the user."],
        "team_dir": str(team_dir),
        "spec": str(spec_path),
        "session_name": launched.get("session_name"),
        "coordinator": coordinator,
        "details_log": str(details_log),
    }


def prepare_quick_start_team(agents_dir: Path, workspace: Path, name: str | None, team_id: str | None = None) -> Path:
    from team_agent.runtime import RuntimeError, _safe_snapshot_name

    if (agents_dir / "TEAM.md").exists() and (agents_dir / "agents").is_dir():
        return agents_dir
    team_source = agents_dir / "TEAM.md"
    role_docs = [path for path in sorted(agents_dir.glob("*.md")) if path.name != "TEAM.md"] if agents_dir.is_dir() else []
    if not role_docs:
        raise RuntimeError(f"{agents_dir}: expected .team/current or a directory of role .md files")
    team_dir = workspace / ".team" / (_safe_snapshot_name(team_id) if team_id else "current")
    target_agents = team_dir / "agents"
    target_profiles = team_dir / "profiles"
    target_agents.mkdir(parents=True, exist_ok=True)
    target_profiles.mkdir(parents=True, exist_ok=True)
    for role_doc in role_docs:
        shutil.copy2(role_doc, target_agents / role_doc.name)
    team_doc = team_dir / "TEAM.md"
    if team_source.exists():
        shutil.copy2(team_source, team_doc)
        if name:
            EventLog(workspace).write("quick_start.name_ignored_existing_team_doc", name=name, team_doc=str(team_doc))
    elif not team_doc.exists():
        team_name = name or agents_dir.name.replace(" ", "-") or "team-agent-team"
        team_doc.write_text(
            f"---\nname: {team_name}\nobjective: Quick-start Team Agent team.\n---\n\nQuick-start team.\n",
            encoding="utf-8",
        )
    elif name:
        # Keep the existing body; name override is only for fresh TEAM.md to avoid hand-editing user docs.
        EventLog(workspace).write("quick_start.name_ignored_existing_team_doc", name=name, team_doc=str(team_doc))
    return team_dir


def wait_ready(workspace: Path, timeout: int = 120) -> dict[str, Any]:
    from team_agent.runtime import status

    start_time = time.monotonic()
    last: dict[str, Any] = {}
    while time.monotonic() - start_time <= timeout:
        last = status(workspace, as_json=True)
        agents = last.get("agents", {})
        if agents and all(agent.get("tmux_window_present") and agent.get("status") in {"running", "busy"} for agent in agents.values()):
            break
        time.sleep(1.0)
    readiness = {
        "process_started": bool(last.get("tmux_session_present")),
        "cli_prompt_ready": all(agent.get("status") in {"running", "busy"} for agent in last.get("agents", {}).values()) if last.get("agents") else False,
        "mcp_ready": all(Path(agent.get("mcp_config", "")).exists() for agent in last.get("agents", {}).values()) if last.get("agents") else False,
        "task_prompt_delivered": bool(MessageStore(workspace).message_counts()),
    }
    ok = readiness["process_started"] and readiness["cli_prompt_ready"] and readiness["mcp_ready"]
    details_log = logs_dir(workspace) / f"wait-ready-{int(time.time())}.json"
    details_log.write_text(json.dumps({"readiness": readiness, "status": last}, indent=2, ensure_ascii=False), encoding="utf-8")
    return {
        "ok": ok,
        "summary": "workers ready" if ok else "workers not fully ready before timeout",
        "next_actions": ["Dispatch a task with team-agent send."] if ok else ["Run team-agent diagnose --json."],
        "details_log": str(details_log),
        "readiness": readiness,
    }


def settle(workspace: Path) -> dict[str, Any]:
    from team_agent.runtime import collect, status

    collected = collect(workspace)
    current = status(workspace, as_json=True)
    details_log = logs_dir(workspace) / f"settle-{int(time.time())}.json"
    details_log.write_text(json.dumps({"collect": collected, "status": current}, indent=2, ensure_ascii=False), encoding="utf-8")
    return {
        "ok": collected.get("ok", False),
        "summary": f"collected {len(collected.get('collected', []))} result(s)",
        "next_actions": ["Review team_state.md and decide whether to continue or shutdown."],
        "details_log": str(details_log),
        "collect": collected,
    }


def repair_state(
    workspace: Path,
    task_id: str,
    assignee: str | None = None,
    status_value: str | None = None,
    summary: str | None = None,
) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError, _find_task, _leader_id

    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    task = _find_task(state.get("tasks", []), task_id)
    if assignee is not None:
        valid_agents = {agent["id"] for agent in spec.get("agents", [])}
        valid_agents.add(_leader_id(state, spec))
        if assignee not in valid_agents:
            raise RuntimeError(f"unknown agent id for repair: {assignee}")
    if status_value is not None and status_value not in TASK_STATUSES:
        raise RuntimeError(f"unknown task status for repair: {status_value}")
    before = {
        "assignee": task.get("assignee"),
        "status": task.get("status"),
        "last_result_summary": task.get("last_result_summary"),
    }
    if assignee is not None:
        task["assignee"] = assignee
    if status_value is not None:
        task["status"] = status_value
    if summary is not None:
        task["last_result_summary"] = summary
    after = {
        "assignee": task.get("assignee"),
        "status": task.get("status"),
        "last_result_summary": task.get("last_result_summary"),
    }
    save_runtime_state(workspace, state)
    state_path = write_team_state(workspace, spec, state)
    EventLog(workspace).write("repair_state.task", task_id=task_id, before=before, after=after)
    return {"ok": True, "task_id": task_id, "before": before, "after": after, "state_file": str(state_path)}
