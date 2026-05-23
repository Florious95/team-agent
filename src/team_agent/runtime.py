from __future__ import annotations

import json
import hashlib
import os
import re
import signal
import shlex
import shutil
import subprocess
import sys
import time
import copy
import fcntl
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.errors import RuntimeError, ValidationError
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.paths import artifacts_dir, logs_dir, messages_dir, runtime_dir, team_workspace
from team_agent.permissions import missing_tools, resolve_permissions
from team_agent.profiles import (
    compact_profile_check,
    effective_model,
    prepare_agent_profile_launch,
    smoke_check_agent_profile,
    validate_agent_profile,
)
from team_agent.rust_core import core_binary, list_targets as core_list_targets, redact_text, render_message as core_render_message
from team_agent.providers import (
    ResumeUnavailable,
    get_adapter,
    shell_command_for_agent,
    shell_fork_command_for_agent,
    shell_resume_command_for_agent,
)
from team_agent.routing import route_task
from team_agent.simple_yaml import dumps
from team_agent.spec import load_spec, validate_result_envelope, validate_spec, workspace_from_spec
from team_agent.state import (
    SESSION_CAPTURE_FIELDS,
    SESSION_STATE_FIELDS,
    load_runtime_state,
    normalize_agent_session_state,
    runtime_state_path,
    save_runtime_state,
    write_spec,
    write_team_state,
)
from team_agent.task_graph import ready_tasks, update_task_status
from team_agent.task_graph import TASK_STATUSES


TMUX_PANE_FORMAT = (
    "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t"
    "#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t"
    "#{pane_current_path}\t#{session_attached}"
)
HEALTH_STATUSES = {"RUNNING", "IDLE", "AWAITING_APPROVAL", "BLOCKED", "ERROR", "DONE"}
GHOSTTY_DISPLAY_BACKENDS = {"ghostty", "ghostty_window", "ghostty_workspace"}
GHOSTTY_WORKSPACE_PANES_PER_WINDOW = 3
STATUS_TEXT_LIMIT = 240
STATUS_EVENT_LIMIT = 3
PEEK_MAX_LINES = 80
PEEK_SEARCH_SCAN_LINES = 300
PEEK_MAX_MATCHES = 5
APPROVAL_SCAN_LINES = 120
DELIVERY_CAPTURE_LINES = 40
PENDING_DELIVERY_STATUSES = {
    "pending",
    "accepted",
    "queued_until_idle",
    "queued_until_start",
    "queued_stopped",
    "queued_pane_missing",
}
SUBMITTED_DELIVERY_STATUSES = {"injected", "visible", "submitted", "submitted_unverified", "delivered", "acknowledged"}
STARTUP_PROMPT_RUNTIME_CHECK_LIMIT = 3
TMUX_STDIN_BUFFER_THRESHOLD = 16 * 1024
TMUX_PASTE_MIN_READY_TIMEOUT = 1.5
TMUX_PASTE_MAX_READY_TIMEOUT = 30.0
TMUX_PASTE_BYTES_PER_SECOND = 25_000
COORDINATOR_PROTOCOL_VERSION = 2
TMUX_SUBMIT_MIN_SETTLE_TIMEOUT = 0.35
TMUX_SUBMIT_MAX_SETTLE_TIMEOUT = 15.0
TMUX_SUBMIT_BYTES_PER_SECOND = 50_000
PASTED_CONTENT_PROMPT_RE = re.compile(
    r"\[\s*Pasted\s+(?:Content\s+\d+\s+chars?|text\s+#\d+\s+\+\s*\d+\s+lines?)\s*\]",
    re.IGNORECASE,
)
INTERNAL_MCP_AUTO_APPROVE_TOOLS = {"send_message", "report_result", "get_team_status", "request_human"}
INTERNAL_MCP_APPROVAL_CHOICE = "Allow for this session"
DANGEROUS_LEADER_FLAGS = (
    ("claude", "--dangerously-skip-permissions"),
    ("claude", "--dangerously-skip-permission"),
    ("codex", "--dangerously-bypass-approvals-and-sandbox"),
)


def run_cmd(args: list[str], timeout: int = 20) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, text=True, capture_output=True, timeout=timeout, check=False)


def ensure_workspace_dirs(workspace: Path) -> None:
    for path in [runtime_dir(workspace), logs_dir(workspace), artifacts_dir(workspace), messages_dir(workspace)]:
        path.mkdir(parents=True, exist_ok=True)


def _effective_runtime_config(runtime_cfg: dict[str, Any]) -> dict[str, Any]:
    effective = dict(runtime_cfg)
    if effective.get("dangerous_auto_approve"):
        effective["dangerous_auto_approve_source"] = "runtime_config"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    inherited = _detect_inherited_dangerous_permissions()
    if not inherited.get("enabled"):
        effective["dangerous_auto_approve"] = False
        effective["dangerous_auto_approve_source"] = "disabled"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    effective["dangerous_auto_approve"] = True
    effective["dangerous_auto_approve_source"] = "leader_process"
    effective["dangerous_auto_approve_inherited"] = True
    effective["dangerous_auto_approve_provider"] = inherited.get("provider")
    effective["dangerous_auto_approve_flag"] = inherited.get("flag")
    return effective


def _requires_direct_leader_receiver(spec: dict[str, Any], runtime_cfg: dict[str, Any]) -> bool:
    if runtime_cfg.get("require_leader_receiver") is not None:
        return bool(runtime_cfg.get("require_leader_receiver"))
    return any(agent.get("provider") != "fake" for agent in spec.get("agents", []))


def _detect_inherited_dangerous_permissions() -> dict[str, Any]:
    for proc in _process_ancestry(os.getpid()):
        command = str(proc.get("command") or "")
        for provider, flag in DANGEROUS_LEADER_FLAGS:
            if _command_has_flag(command, flag):
                return {
                    "enabled": True,
                    "provider": provider,
                    "flag": flag,
                    "pid": proc.get("pid"),
                }
    return {"enabled": False}


def _command_has_flag(command: str, flag: str) -> bool:
    return re.search(rf"(?<!\S){re.escape(flag)}(?!\S)", command) is not None


def _process_ancestry(pid: int, max_depth: int = 12) -> list[dict[str, Any]]:
    ancestry: list[dict[str, Any]] = []
    current = pid
    seen: set[int] = set()
    for _ in range(max_depth):
        if current in seen or current <= 0:
            break
        seen.add(current)
        info = _process_info(current)
        if not info:
            break
        ancestry.append(info)
        parent = info.get("ppid")
        if not isinstance(parent, int) or parent <= 1 or parent == current:
            break
        current = parent
    return ancestry


def _process_info(pid: int) -> dict[str, Any] | None:
    try:
        proc = subprocess.run(
            ["ps", "-p", str(pid), "-o", "ppid=", "-o", "command="],
            text=True,
            capture_output=True,
            timeout=2,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    line = proc.stdout.strip()
    if not line:
        return None
    parts = line.split(None, 1)
    try:
        ppid = int(parts[0])
    except (IndexError, ValueError):
        return None
    return {"pid": pid, "ppid": ppid, "command": parts[1] if len(parts) > 1 else ""}


def init_workspace(workspace: Path, force: bool = False) -> dict[str, Path]:
    ensure_workspace_dirs(workspace)
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    spec_path = team_dir / "team.spec.yaml"
    state_path = workspace / "team_state.md"
    from team_agent.paths import example_path, template_path

    if spec_path.exists() and not force:
        raise RuntimeError(f"{spec_path} already exists; pass --force to overwrite")
    spec_path.write_text(example_path("team.spec.yaml").read_text(encoding="utf-8"), encoding="utf-8")
    if not state_path.exists() or force:
        state_path.write_text(template_path("team_state.md").read_text(encoding="utf-8"), encoding="utf-8")
    EventLog(workspace).write("init", spec_path=str(spec_path), state_path=str(state_path))
    return {"spec": spec_path, "state": state_path}


def validate_file(spec_path: Path) -> dict[str, Any]:
    if spec_path.is_dir():
        from team_agent.compiler import compile_team

        result = compile_team(spec_path)
        spec = result["spec"]
        return {
            "ok": True,
            "type": "team_dir",
            "workspace": str(Path(spec["team"]["workspace"]).resolve()),
            "team": spec["team"]["name"],
            "agents": [agent["id"] for agent in spec.get("agents", [])],
        }
    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    return {"ok": True, "workspace": str(workspace), "team": spec["team"]["name"]}


def _tmux_session_conflict_error(session_name: str) -> str:
    return (
        f"tmux session already exists: {session_name}. "
        "Startup will not terminate existing tmux sessions because they may belong to active teams. "
        "Use a different team name or runtime.session_name and start again."
    )


def _spec_team_dir(spec_path: Path, workspace: Path) -> Path:
    spec_dir = spec_path.resolve().parent
    if spec_dir.parent.name == ".team":
        return spec_dir
    return workspace.resolve() / ".team" / "current"


def _is_team_doc_dir(team_dir: Path) -> bool:
    return (team_dir / "TEAM.md").exists() and (team_dir / "agents").is_dir()


def _compile_team_dir_spec(team_dir: Path, workspace: Path) -> dict[str, Any]:
    from team_agent.compiler import compile_team

    spec_path = team_dir / "team.spec.yaml"
    compiled = compile_team(team_dir, spec_path)
    if compiled["spec"].get("context", {}).get("state_file") == "team_state.md":
        state_file = str(team_dir.relative_to(workspace) / "team_state.md") if team_dir.is_relative_to(workspace) else "team_state.md"
        compiled["spec"]["context"]["state_file"] = state_file
        spec_path.write_text(dumps(compiled["spec"]), encoding="utf-8")
    return compiled


def _attach_team_profile_dirs(spec: dict[str, Any], spec_path: Path, workspace: Path | None = None, team_dir: Path | None = None) -> None:
    workspace = workspace.resolve() if workspace else workspace_from_spec(spec, spec_path)
    team_dir = team_dir.resolve() if team_dir else _spec_team_dir(spec_path, workspace)
    profiles_dir = team_dir / "profiles"
    for agent in spec.get("agents", []):
        if isinstance(agent, dict) and agent.get("profile"):
            agent["_profile_dir"] = str(profiles_dir)


def launch(
    spec_path: Path,
    dry_run: bool = False,
    auto_approve: bool = False,
    skip_profile_smoke: bool = False,
) -> dict[str, Any]:
    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    team_dir = _spec_team_dir(spec_path, workspace)
    _attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
    ensure_workspace_dirs(workspace)
    event_log = EventLog(workspace)
    session_name = spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    state = {
        "spec_path": str(spec_path.resolve()),
        "workspace": str(workspace),
        "team_dir": str(team_dir),
        "session_name": session_name,
        "leader": spec.get("leader"),
        "agents": {},
        "tasks": [dict(task) for task in spec.get("tasks", [])],
        "display_backend": spec.get("runtime", {}).get("display_backend", "none"),
    }
    runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
    dangerous_auto_approve = bool(runtime_cfg.get("dangerous_auto_approve"))
    dangerous_inherited = bool(runtime_cfg.get("dangerous_auto_approve_inherited"))

    routing_decisions: list[dict[str, Any]] = []
    for task in state["tasks"]:
        route = route_task(spec, task)
        task["assignee"] = route["agent_id"]
        decision = {
            "source": "launch",
            "task_id": task.get("id"),
            "selected_agent": route["agent_id"],
            "reason": route["reason"],
            "manual_override": False,
        }
        routing_decisions.append(decision)
        event_log.write("routing.decision", **decision)

    permission_summary = [resolve_permissions(agent) for agent in spec.get("agents", [])]
    event_log.write(
        "launch.permissions_resolved",
        permissions=permission_summary,
        dangerous_auto_approve=dangerous_auto_approve,
        dangerous_auto_approve_source=runtime_cfg.get("dangerous_auto_approve_source"),
    )
    if dry_run:
        return {
            "ok": True,
            "dry_run": True,
            "session_name": session_name,
            "permissions": permission_summary,
            "routes": routing_decisions,
            "safety": {
                "dangerous_auto_approve": dangerous_auto_approve,
                "dangerous_auto_approve_source": runtime_cfg.get("dangerous_auto_approve_source"),
                "dangerous_auto_approve_inherited": dangerous_inherited,
                "requires_explicit_yes": dangerous_auto_approve and not dangerous_inherited,
            },
        }
    if dangerous_auto_approve:
        event_log.write(
            "launch.dangerous_auto_approve_requested",
            reason="provider may bypass approvals or sandbox",
            source=runtime_cfg.get("dangerous_auto_approve_source"),
            inherited=dangerous_inherited,
            inherited_provider=runtime_cfg.get("dangerous_auto_approve_provider"),
            inherited_flag=runtime_cfg.get("dangerous_auto_approve_flag"),
        )
    if dangerous_auto_approve and not dangerous_inherited and not auto_approve:
        raise RuntimeError("dangerous_auto_approve requires explicit --yes after reviewing launch risk")
    if runtime_cfg.get("require_user_approval_before_launch", True) and not auto_approve:
        raise RuntimeError("launch requires approval; rerun with --yes after reviewing resolved permissions")

    tmux = get_adapter_or_raise("tmux")
    _ = tmux
    if _tmux_session_exists(session_name):
        event_log.write(
            "launch.session_conflict",
            session=session_name,
            action="use a different team name or runtime.session_name; do not terminate existing tmux sessions from startup",
        )
        raise RuntimeError(_tmux_session_conflict_error(session_name))
    _ensure_agent_start_requirements(
        workspace,
        spec.get("agents", []),
        event_log,
        "launch",
        skip_profile_smoke=skip_profile_smoke,
    )

    leader_receiver = None
    leader_provider = state.get("leader", {}).get("provider")
    require_leader_receiver = _requires_direct_leader_receiver(spec, runtime_cfg)
    if runtime_cfg.get("auto_attach_leader", True) and leader_provider != "fake":
        try:
            leader_receiver, _ = _attach_leader_to_state(
                workspace,
                state,
                pane=None,
                provider=leader_provider,
                event_log=event_log,
                source="launch",
                require_current=require_leader_receiver,
            )
        except RuntimeError as exc:
            event_log.write(
                "leader_receiver.auto_attach_skipped",
                provider=leader_provider,
                reason=str(exc),
                required=require_leader_receiver,
                suggestion="Start the leader with `team-agent codex` or run quick-start from an existing tmux pane.",
            )
            if require_leader_receiver:
                raise

    first = True
    started: list[dict[str, Any]] = []
    display_jobs: list[tuple[str, dict[str, Any]]] = []
    for agent in spec.get("agents", []):
        if agent.get("paused"):
            state["agents"][agent["id"]] = {"status": "paused", "provider": agent["provider"]}
            continue
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                "launch.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
        mcp_config = adapter.mcp_config(workspace, agent["id"])
        mcp_path = adapter.install_mcp(workspace, agent["id"], mcp_config)
        command_agent = dict(agent)
        command_agent["_runtime"] = runtime_cfg
        command = shell_command_for_agent(command_agent, workspace, mcp_config)
        spawn_time = datetime.now(timezone.utc)
        event_log.write(
            "launch.agent_start",
            agent_id=agent["id"],
            provider=agent["provider"],
            session=session_name,
            window=agent["id"],
            command=command,
            mcp_config=str(mcp_path),
        )
        if first:
            proc = run_cmd(["tmux", "new-session", "-d", "-s", session_name, "-n", agent["id"], "sh", "-lc", command])
            first = False
        else:
            proc = run_cmd(["tmux", "new-window", "-t", session_name, "-n", agent["id"], "sh", "-lc", command])
        if proc.returncode != 0:
            try:
                adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
            except Exception as exc:
                event_log.write(
                    "launch.mcp_cleanup_failed",
                    agent_id=agent["id"],
                    provider=agent["provider"],
                    mcp_config=str(mcp_path),
                    error=str(exc),
                )
            event_log.write(
                "launch.agent_failed",
                agent_id=agent["id"],
                stderr=proc.stderr,
                stdout=proc.stdout,
            )
            raise RuntimeError(f"Failed to start agent {agent['id']}: {proc.stderr.strip()}")
        handled_prompts = adapter.handle_startup_prompts(session_name, agent["id"], checks=1, sleep_s=0.0)
        for prompt_event in handled_prompts:
            event_log.write(
                "launch.startup_prompt_handled",
                agent_id=agent["id"],
                provider=agent["provider"],
                **prompt_event,
            )
        if runtime_cfg.get("fast") and agent.get("provider") == "codex":
            fast_result = _enable_codex_fast_mode(session_name, agent["id"])
            event_log.write("launch.codex_fast_mode", agent_id=agent["id"], **fast_result)
        state["agents"][agent["id"]] = {
            "status": "running",
            "provider": agent["provider"],
            "agent_id": agent["id"],
            "model": agent.get("model"),
            "auth_mode": agent.get("auth_mode"),
            "profile": agent.get("profile"),
            "window": agent["id"],
            "mcp_config": str(mcp_path),
            "permissions": resolve_permissions(agent),
            "session_id": None,
            "rollout_path": None,
            "captured_at": None,
            "captured_via": None,
            "attribution_confidence": None,
            "spawn_cwd": str(workspace),
            "spawned_at": spawn_time.isoformat(),
        }
        profile_launch = command_agent.get("_provider_profile") or {}
        if profile_launch.get("claude_projects_root"):
            state["agents"][agent["id"]]["claude_projects_root"] = profile_launch["claude_projects_root"]
        if command_agent.get("_session_id"):
            state["agents"][agent["id"]]["_pending_session_id"] = command_agent["_session_id"]
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in state.get("agents", {}).items()
            if aid != agent["id"] and item.get("session_id")
        }
        _capture_agent_session(
            workspace,
            agent["id"],
            state["agents"][agent["id"]],
            event_log,
            timeout_s=1.5,
            exclude_session_ids=known_session_ids,
        )
        if state.get("display_backend") in GHOSTTY_DISPLAY_BACKENDS:
            display_jobs.append((agent["id"], agent))
        started.append({"agent_id": agent["id"], "provider": agent["provider"], "window": agent["id"]})
    for agent_id, display in _open_worker_displays(
        workspace,
        session_name,
        display_jobs,
        event_log,
        state.get("display_backend", "none"),
    ).items():
        if agent_id in state["agents"]:
            state["agents"][agent_id]["display"] = display
    save_runtime_state(workspace, state)
    _save_team_runtime_snapshot(workspace, state)
    MessageStore(workspace)
    write_team_state(workspace, spec, state)
    event_log.write("launch.complete", session=session_name, started=started)
    return {
        "ok": True,
        "session_name": session_name,
        "agents": started,
        "permissions": permission_summary,
        "routes": routing_decisions,
        "leader_receiver": leader_receiver,
    }


def _save_team_runtime_snapshot(workspace: Path, state: dict[str, Any]) -> Path | None:
    session_name = state.get("session_name")
    if not session_name:
        return None
    snapshot_dir = _team_runtime_snapshot_dir(workspace, str(session_name))
    snapshot_dir.mkdir(parents=True, exist_ok=True)
    snapshot_state = copy.deepcopy(state)
    spec_path = Path(str(state.get("spec_path") or ""))
    if spec_path.is_file():
        if not snapshot_state.get("team_dir"):
            snapshot_state["team_dir"] = str(_spec_team_dir(spec_path, workspace))
        snapshot_spec = snapshot_dir / "team.spec.yaml"
        if spec_path.resolve() != snapshot_spec.resolve():
            shutil.copy2(spec_path, snapshot_spec)
        snapshot_state["spec_path"] = str(snapshot_spec)
    snapshot_state["team_snapshot"] = {
        "session_name": session_name,
        "team_name": _state_team_name(snapshot_state),
        "snapshot_dir": str(snapshot_dir),
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    state_path = snapshot_dir / "state.json"
    tmp_path = state_path.with_suffix(".json.tmp")
    tmp_path.write_text(json.dumps(snapshot_state, indent=2, ensure_ascii=False), encoding="utf-8")
    os.replace(tmp_path, state_path)
    return state_path


def _team_runtime_snapshot_dir(workspace: Path, session_name: str) -> Path:
    return runtime_dir(workspace) / "teams" / _safe_snapshot_name(session_name)


def _safe_snapshot_name(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]", "_", value).strip("._-") or "team"


def _state_team_name(state: dict[str, Any]) -> str | None:
    spec_path = state.get("spec_path")
    if not spec_path:
        return None
    try:
        return str(load_spec(Path(str(spec_path))).get("team", {}).get("name") or "")
    except Exception:
        return None


def _load_snapshot_state(path: Path) -> dict[str, Any] | None:
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    normalize_agent_session_state(state)
    return state


def _restart_candidates(workspace: Path) -> list[dict[str, Any]]:
    by_session: dict[str, dict[str, Any]] = {}
    snapshots_root = runtime_dir(workspace) / "teams"
    for path in sorted(snapshots_root.glob("*/state.json")) if snapshots_root.exists() else []:
        state = _load_snapshot_state(path)
        if not state or not state.get("session_name"):
            continue
        session_name = str(state["session_name"])
        by_session[session_name] = _restart_candidate_from_state(state, path)
    active = load_runtime_state(workspace)
    if active.get("session_name"):
        by_session[str(active["session_name"])] = _restart_candidate_from_state(active, runtime_state_path(workspace))
    return sorted(by_session.values(), key=lambda item: item.get("session_name") or "")


def _restart_candidate_from_state(state: dict[str, Any], state_path: Path) -> dict[str, Any]:
    session_name = str(state.get("session_name") or "")
    return {
        "session_name": session_name,
        "team_name": _state_team_name(state),
        "state_path": str(state_path),
        "spec_path": state.get("spec_path"),
        "agents": sorted(state.get("agents", {}).keys()),
        "has_context": _state_has_restart_context(state),
        "state": state,
    }


def _state_has_restart_context(state: dict[str, Any]) -> bool:
    for agent_state in state.get("agents", {}).values():
        if not isinstance(agent_state, dict):
            continue
        if agent_state.get("session_id") or agent_state.get("rollout_path") or agent_state.get("captured_at"):
            return True
    return bool(state.get("agents"))


def _select_restart_state(workspace: Path, team: str | None = None) -> dict[str, Any]:
    candidates = [item for item in _restart_candidates(workspace) if item.get("has_context")]
    if team:
        matches = [
            item
            for item in candidates
            if team in {item.get("session_name"), item.get("team_name"), Path(str(item.get("state_path"))).parent.name}
        ]
        if len(matches) == 1:
            return copy.deepcopy(matches[0]["state"])
        if len(matches) > 1:
            raise RuntimeError("restart team selector is ambiguous. " + _format_restart_candidates(matches))
        raise RuntimeError(f"restart team {team!r} not found. " + _format_restart_candidates(candidates))
    if len(candidates) == 1:
        return copy.deepcopy(candidates[0]["state"])
    if len(candidates) > 1:
        raise RuntimeError(
            "multiple restartable teams found in this workspace; pass --team <session_name> to choose. "
            + _format_restart_candidates(candidates)
        )
    return load_runtime_state(workspace)


def _format_restart_candidates(candidates: list[dict[str, Any]]) -> str:
    if not candidates:
        return "No restartable team state was found."
    parts = []
    for item in candidates:
        parts.append(
            f"{item.get('session_name')} team={item.get('team_name') or '-'} "
            f"agents={','.join(item.get('agents') or []) or '-'}"
        )
    return "Candidates: " + "; ".join(parts)


def _quick_start_existing_context(workspace: Path, session_name: str) -> dict[str, Any] | None:
    for item in _restart_candidates(workspace):
        if item.get("session_name") == session_name and item.get("has_context"):
            return item
    return None


def status(workspace: Path, as_json: bool = False, *, compact: bool = False) -> dict[str, Any]:
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
        "queued_messages": _queued_message_statuses(store.messages()),
        "results": store.result_counts(),
        "latest_results": _latest_result_summaries(store),
        "coordinator": coordinator_health(workspace),
        "last_events": EventLog(workspace).tail(10),
    }
    return _compact_status(result) if compact else result


def _compact_status(data: dict[str, Any]) -> dict[str, Any]:
    return {
        "team": data.get("team"),
        "session_name": data.get("session_name"),
        "tmux_session_present": data.get("tmux_session_present"),
        "leader_receiver": _compact_mapping(
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
            agent_id: _compact_agent_state(agent_id, agent)
            for agent_id, agent in (data.get("agents") or {}).items()
        },
        "agent_health": data.get("agent_health", {}),
        "tasks": [_compact_task(task) for task in data.get("tasks", [])],
        "messages": data.get("messages", {}),
        "queued_messages": data.get("queued_messages", [])[:8],
        "results": data.get("results", {}),
        "latest_results": data.get("latest_results", [])[:5],
        "coordinator": _compact_mapping(data.get("coordinator", {}), {"status", "pid", "metadata_ok", "schema_ok"}),
        "last_events": [_compact_event(event) for event in data.get("last_events", [])[-STATUS_EVENT_LIMIT:]],
    }


def _latest_result_summaries(store: MessageStore, limit: int = 5) -> list[dict[str, Any]]:
    summaries: list[dict[str, Any]] = []
    for row in store.latest_results(limit=limit):
        summary = _result_summary_from_row(row)
        if summary:
            summaries.append(summary)
    return summaries


def _result_summary_from_row(row: dict[str, Any]) -> dict[str, Any] | None:
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


def _queued_message_statuses(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
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


def _compact_agent_state(agent_id: str, agent: dict[str, Any]) -> dict[str, Any]:
    display = agent.get("display") or {}
    result = _compact_mapping(
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
        result["display"] = _compact_mapping(
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


def _compact_task(task: dict[str, Any]) -> dict[str, Any]:
    return _compact_mapping(
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


def _compact_event(event: dict[str, Any]) -> dict[str, Any]:
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
                _compact_mapping(item, {"agent_id", "restart_mode", "session_id"})
                for item in value[:8]
                if isinstance(item, dict)
            ]
            continue
        result[key] = _compact_value(value)
    return result


def _compact_mapping(source: Any, keys: set[str]) -> dict[str, Any]:
    if not isinstance(source, dict):
        return {}
    return {key: _compact_value(source[key]) for key in keys if key in source}


def _compact_value(value: Any) -> Any:
    if isinstance(value, str):
        return value if len(value) <= STATUS_TEXT_LIMIT else value[: STATUS_TEXT_LIMIT - 1] + "…"
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    if isinstance(value, list):
        if all(isinstance(item, (str, int, float, bool)) or item is None for item in value):
            compact = [_compact_value(item) for item in value[:8]]
            if len(value) > 8:
                compact.append(f"... {len(value) - 8} more")
            return compact
        return f"{len(value)} item(s)"
    if isinstance(value, dict):
        return {
            key: _compact_value(item)
            for key, item in value.items()
            if key not in {"command", "payload", "launch_args", "content", "prompt", "developer_instructions"}
        }
    return str(value)


def format_status(workspace: Path, agent_id: str | None = None) -> str:
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


def peek(
    workspace: Path,
    agent_id: str,
    *,
    head: int | None = None,
    tail: int | None = None,
    search: str | None = None,
    context: int = 3,
) -> dict[str, Any]:
    modes = [head is not None, tail is not None, search is not None]
    if sum(modes) != 1:
        raise RuntimeError("peek requires exactly one of --head, --tail, or --search")
    if head is not None:
        _validate_line_count("--head", head)
    if tail is not None:
        _validate_line_count("--tail", tail)
    if search is not None and not search.strip():
        raise RuntimeError("--search must not be empty")
    if context < 0 or context > 10:
        raise RuntimeError("--context must be between 0 and 10")
    state = load_runtime_state(workspace)
    agent = state.get("agents", {}).get(agent_id)
    if not agent:
        raise RuntimeError(f"unknown agent id: {agent_id}")
    session_name = state.get("session_name")
    window = agent.get("window", agent_id)
    if not session_name or not _tmux_window_exists(session_name, window):
        raise RuntimeError(f"agent terminal is not available: {agent_id}")
    scan_lines = tail or PEEK_SEARCH_SCAN_LINES
    proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{scan_lines}", "-t", f"{session_name}:{window}"], timeout=5)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"capture failed for {agent_id}")
    captured = proc.stdout.splitlines()
    if head is not None:
        selected = captured[:head]
        return {
            "ok": True,
            "agent_id": agent_id,
            "mode": "head",
            "lines": head,
            "scanned_lines": scan_lines,
            "text": "\n".join(selected),
        }
    if tail is not None:
        return {
            "ok": True,
            "agent_id": agent_id,
            "mode": "tail",
            "lines": tail,
            "scanned_lines": scan_lines,
            "text": "\n".join(captured[-tail:]),
        }
    assert search is not None
    matches = _search_lines(captured, search, context)
    return {
        "ok": True,
        "agent_id": agent_id,
        "mode": "search",
        "search": search,
        "context": context,
        "scanned_lines": scan_lines,
        "matches": matches,
        "truncated": len(matches) >= PEEK_MAX_MATCHES,
        "text": _format_search_matches(matches),
    }


def _validate_line_count(flag: str, value: int) -> None:
    if value < 1 or value > PEEK_MAX_LINES:
        raise RuntimeError(f"{flag} must be between 1 and {PEEK_MAX_LINES}")


def _search_lines(lines: list[str], needle: str, context: int) -> list[dict[str, Any]]:
    needle_lower = needle.lower()
    matches: list[dict[str, Any]] = []
    used_ranges: list[tuple[int, int]] = []
    for index, line in enumerate(lines):
        if needle_lower not in line.lower():
            continue
        start = max(0, index - context)
        end = min(len(lines), index + context + 1)
        if used_ranges and start <= used_ranges[-1][1]:
            previous = matches[-1]
            previous["lines"] = lines[previous["start_line"] - 1 : end]
            previous["end_line"] = end
            used_ranges[-1] = (previous["start_line"] - 1, end)
        else:
            matches.append({"line": index + 1, "start_line": start + 1, "end_line": end, "lines": lines[start:end]})
            used_ranges.append((start, end))
        if len(matches) >= PEEK_MAX_MATCHES:
            break
    return matches


def _format_search_matches(matches: list[dict[str, Any]]) -> str:
    if not matches:
        return "no matches"
    blocks: list[str] = []
    for match in matches:
        blocks.append(f"match line {match['line']} ({match['start_line']}-{match['end_line']}):")
        blocks.extend(str(line) for line in match["lines"])
    return "\n".join(blocks)


def approvals(workspace: Path, agent_id: str | None = None) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    session_name = state.get("session_name")
    approvals_found: list[dict[str, Any]] = []
    agents = state.get("agents", {})
    target_ids = [agent_id] if agent_id else sorted(agents)
    for target_id in target_ids:
        agent = agents.get(target_id)
        if not agent:
            raise RuntimeError(f"unknown agent id: {target_id}")
        window = agent.get("window", target_id)
        if not session_name or not _tmux_window_exists(session_name, window):
            continue
        proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", f"{session_name}:{window}"], timeout=5)
        if proc.returncode != 0:
            continue
        prompt = _extract_approval_prompt(target_id, proc.stdout)
        if prompt:
            approvals_found.append(prompt)
    return {
        "ok": True,
        "waiting": bool(approvals_found),
        "waiting_count": len(approvals_found),
        "approvals": approvals_found,
        "scan": {"mode": "tail", "lines": APPROVAL_SCAN_LINES, "raw_output": False},
    }


def format_approvals(workspace: Path, agent_id: str | None = None) -> str:
    result = approvals(workspace, agent_id=agent_id)
    if not result["approvals"]:
        return "No pending approvals."
    lines: list[str] = []
    for item in result["approvals"]:
        detail = item.get("tool") or item.get("command") or item.get("kind")
        lines.append(f"{item['agent_id']}: {item['state']} {item['kind']} {detail}".rstrip())
        if item.get("prompt"):
            lines.append(f"  prompt: {item['prompt']}")
        if item.get("choices"):
            lines.append("  choices: " + "; ".join(item["choices"]))
        lines.append("  raw terminal output omitted; use debug-only peek with --search/--tail/--head if the user explicitly asks.")
    return "\n".join(lines)


def inbox(workspace: Path, agent_id: str, limit: int = 20) -> dict[str, Any]:
    rows = MessageStore(workspace).inbox(agent_id, limit=limit)
    return {"ok": True, "agent_id": agent_id, "messages": rows}


def format_inbox(workspace: Path, agent_id: str, limit: int = 20) -> str:
    store = MessageStore(workspace)
    rows = store.inbox(agent_id, limit=limit)
    result_counts = store.result_counts()
    note = "final results are not in inbox; use team-agent collect"
    if result_counts.get("uncollected", 0):
        note += f" ({result_counts['uncollected']} uncollected result(s) pending)"
    if not rows:
        return f"{agent_id}: no messages\n{note}"
    lines = [
        f"{row['created_at']} {row['sender']} -> {row['recipient']} {row['status']}: {row['content']}"
        for row in rows
    ]
    lines.append(note)
    return "\n".join(lines)


def attach_leader(workspace: Path, pane: str | None = None, provider: str = "codex") -> dict[str, Any]:
    ensure_workspace_dirs(workspace)
    state = load_runtime_state(workspace)
    event_log = EventLog(workspace)
    receiver, validation = _attach_leader_to_state(
        workspace,
        state,
        pane=pane,
        provider=provider,
        event_log=event_log,
        source="manual",
    )
    save_runtime_state(workspace, state)
    return {"ok": True, "leader_receiver": receiver, "validation": validation}


def start_leader(provider: str, provider_args: list[str], workspace: Path) -> None:
    plan = leader_start_plan(provider, provider_args, workspace)
    EventLog(workspace).write(
        "leader.start",
        provider=provider,
        workspace=str(workspace),
        mode=plan["mode"],
        session_name=plan.get("session_name"),
        argv=plan["argv"],
    )
    if plan["mode"] == "exec_provider":
        os.chdir(workspace)
    os.execvp(plan["argv"][0], plan["argv"])


def leader_start_plan(provider: str, provider_args: list[str], workspace: Path) -> dict[str, Any]:
    workspace = workspace.resolve()
    ensure_workspace_dirs(workspace)
    adapter = get_adapter(provider)
    if not adapter.is_installed():
        raise RuntimeError(f"Provider {provider} command {adapter.command_name!r} not found")
    argv = [adapter.command_name, *provider_args]
    if os.environ.get("TMUX"):
        return {"mode": "exec_provider", "provider": provider, "workspace": str(workspace), "argv": argv}
    if not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ or start the leader from an existing tmux pane")
    session_name = _leader_session_name(provider, workspace)
    if _tmux_session_exists(session_name):
        return {
            "mode": "attach_existing",
            "provider": provider,
            "workspace": str(workspace),
            "session_name": session_name,
            "argv": ["tmux", "attach-session", "-t", session_name],
        }
    shell = f"cd {shlex.quote(str(workspace))} && exec {shlex.join(argv)}"
    return {
        "mode": "new_tmux_session",
        "provider": provider,
        "workspace": str(workspace),
        "session_name": session_name,
        "argv": ["tmux", "new-session", "-s", session_name, "-n", provider, "-c", str(workspace), "sh", "-lc", shell],
    }


def _leader_session_name(provider: str, workspace: Path) -> str:
    digest = hashlib.sha1(str(workspace.resolve()).encode("utf-8")).hexdigest()[:8]
    folder = re.sub(r"[^A-Za-z0-9_.-]", "_", workspace.name)[:48].strip("._-") or "workspace"
    return f"team-agent-leader-{provider}-{folder}-{digest}"


def _attach_leader_to_state(
    workspace: Path,
    state: dict[str, Any],
    pane: str | None,
    provider: str,
    event_log: EventLog,
    source: str,
    require_current: bool = False,
) -> tuple[dict[str, Any], dict[str, Any]]:
    get_adapter(provider)
    pane_info, discovery = _resolve_leader_pane(pane, provider, workspace=workspace, require_current=require_current)
    inferred_provider = _leader_command_provider(pane_info.get("pane_current_command", ""))
    receiver_provider = inferred_provider or provider
    receiver = {
        "mode": "direct_tmux",
        "status": "attached",
        "provider": receiver_provider,
        "pane_id": pane_info["pane_id"],
        "session_name": pane_info["session_name"],
        "window_index": pane_info["window_index"],
        "window_name": pane_info["window_name"],
        "pane_index": pane_info["pane_index"],
        "pane_tty": pane_info["pane_tty"],
        "pane_current_command": pane_info["pane_current_command"],
        "fingerprint": _target_fingerprint(pane_info),
        "attached_at": datetime.now(timezone.utc).isoformat(),
        "discovery": discovery,
    }
    if receiver_provider != provider:
        receiver["requested_provider"] = provider
    validation = _validate_leader_receiver(receiver)
    if not validation["ok"]:
        event_log.write(
            "leader_receiver.attach_failed",
            target=pane or pane_info.get("pane_id"),
            discovery=discovery,
            provider=provider,
            reason=validation["reason"],
            error=validation.get("error"),
            source=source,
        )
        raise RuntimeError(f"leader pane validation failed: {validation['reason']}")
    if validation.get("warning"):
        receiver["warning"] = validation["warning"]
    state["leader_receiver"] = receiver
    event_log.write(
        "leader_receiver.attached",
        target=receiver["pane_id"],
        session_name=receiver["session_name"],
        window_index=receiver["window_index"],
        window_name=receiver["window_name"],
        pane_index=receiver["pane_index"],
        pane_tty=receiver["pane_tty"],
        pane_current_command=receiver["pane_current_command"],
        provider=receiver_provider,
        requested_provider=provider if receiver_provider != provider else None,
        discovery=discovery,
        source=source,
    )
    return receiver, validation


def send_message(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import send_message as impl

    return impl(*args, **kwargs)


def _send_message_unlocked(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import _send_message_unlocked as impl

    return impl(*args, **kwargs)


def _send_single_message_unlocked(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import _send_single_message_unlocked as impl

    return impl(*args, **kwargs)


def _broadcast_message_unlocked(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import _broadcast_message_unlocked as impl

    return impl(*args, **kwargs)


def collect(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import collect as impl

    return impl(*args, **kwargs)


def _team_state_result_entries(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _team_state_result_entries as impl

    return impl(*args, **kwargs)


def _ensure_coordinator_after_collect(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _ensure_coordinator_after_collect as impl

    return impl(*args, **kwargs)


def _coordinator_should_run(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _coordinator_should_run as impl

    return impl(*args, **kwargs)


def report_result(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import report_result as impl

    return impl(*args, **kwargs)


def _notify_leader_of_report_result(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _notify_leader_of_report_result as impl

    return impl(*args, **kwargs)


def _format_report_result_notification(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _format_report_result_notification as impl

    return impl(*args, **kwargs)


def _record_invalid_result(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _record_invalid_result as impl

    return impl(*args, **kwargs)


def _capture_missing_sessions(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    timeout_s: float,
    log_miss: bool = True,
) -> list[str]:
    captured: list[str] = []
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("session_id"):
            continue
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in state.get("agents", {}).items()
            if aid != agent_id and item.get("session_id")
        }
        result = _capture_agent_session(
            workspace,
            agent_id,
            agent_state,
            event_log,
            timeout_s=timeout_s,
            exclude_session_ids=known_session_ids,
        )
        if result:
            captured.append(agent_id)
        elif log_miss:
            event_log.write(
                "session.capture_timeout",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                timeout_s=timeout_s,
                spawn_cwd=agent_state.get("spawn_cwd"),
            )
    return captured


def _capture_agent_session(
    workspace: Path,
    agent_id: str,
    agent_state: dict[str, Any],
    event_log: EventLog,
    timeout_s: float,
    exclude_session_ids: set[str] | None = None,
) -> dict[str, Any] | None:
    if agent_state.get("session_id"):
        return None
    adapter = get_adapter(agent_state["provider"])
    spawn_context = {
        "agent_id": agent_id,
        "cwd": agent_state.get("spawn_cwd") or str(workspace),
        "spawn_time": agent_state.get("spawned_at") or datetime.now(timezone.utc).isoformat(),
        "tmux_target": f"{agent_state.get('session_name', '')}:{agent_state.get('window', agent_id)}",
        "predetermined_session_id": agent_state.get("_pending_session_id"),
        "exclude_session_ids": sorted(exclude_session_ids or set()),
        "claude_projects_root": agent_state.get("claude_projects_root"),
    }
    result = adapter.capture_session_id(agent_id, spawn_context, timeout_s=timeout_s)
    if not isinstance(result, dict) or not result.get("session_id"):
        return None
    _copy_session_metadata(agent_state, result)
    agent_state.pop("_pending_session_id", None)
    event_log.write(
        "session.captured",
        agent_id=agent_id,
        provider=agent_state.get("provider"),
        session_id=agent_state.get("session_id"),
        rollout_path=agent_state.get("rollout_path"),
        captured_via=agent_state.get("captured_via"),
        attribution_confidence=agent_state.get("attribution_confidence"),
    )
    return result


def _copy_session_metadata(target: dict[str, Any], source: dict[str, Any]) -> None:
    for key in SESSION_STATE_FIELDS:
        target[key] = source.get(key)


def _clear_session_capture_fields(target: dict[str, Any]) -> None:
    for key in SESSION_CAPTURE_FIELDS:
        target[key] = None


def _attach_profile_resume_root(workspace: Path, command_agent: dict[str, Any], previous: dict[str, Any]) -> dict[str, Any]:
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if not profile_launch:
        return previous
    command_agent["_provider_profile"] = profile_launch
    root = profile_launch.get("claude_projects_root")
    if not root:
        return previous
    prepared = dict(previous)
    prepared["claude_projects_root"] = root
    return prepared


def _prepare_resume_state(
    workspace: Path,
    agent_id: str,
    previous: dict[str, Any],
    adapter: Any,
    event_log: EventLog,
    exclude_session_ids: set[str] | None = None,
    allow_fresh_on_resume_failure: bool = False,
) -> dict[str, Any]:
    prepared = dict(previous)
    session_id = prepared.get("session_id")
    if session_id and adapter.session_is_resumable(prepared, workspace):
        return prepared
    if session_id:
        event_log.write(
            "resume.session_unverified",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            session_id=session_id,
            captured_via=prepared.get("captured_via"),
            spawn_cwd=prepared.get("spawn_cwd"),
        )
    else:
        event_log.write(
            "resume.session_missing_repair_attempt",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            spawn_cwd=prepared.get("spawn_cwd"),
        )
    repaired = _recover_resume_session_from_events(workspace, agent_id, prepared, adapter, exclude_session_ids or set())
    if not repaired:
        repaired = adapter.recover_session_id(agent_id, prepared, workspace, exclude_session_ids or set())
    if repaired:
        _copy_session_metadata(prepared, repaired)
        event_log.write(
            "resume.session_repaired",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            old_session_id=session_id,
            session_id=prepared.get("session_id"),
            rollout_path=prepared.get("rollout_path"),
            captured_via=prepared.get("captured_via"),
            attribution_confidence=prepared.get("attribution_confidence"),
        )
        return prepared
    if session_id and not allow_fresh_on_resume_failure:
        event_log.write(
            "resume.session_required_missing",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            old_session_id=session_id,
            rollout_path=prepared.get("rollout_path"),
            reason="provider transcript not found",
        )
        raise ResumeUnavailable(
            f"Cannot resume agent {agent_id}: stored session {session_id} is not available. "
            "Use --allow-fresh only if losing that worker context is acceptable."
        )
    _clear_session_capture_fields(prepared)
    event_log.write(
        "resume.session_unavailable",
        agent_id=agent_id,
        provider=prepared.get("provider"),
        old_session_id=session_id,
        reason="provider transcript not found",
    )
    return prepared


def _recover_resume_session_from_events(
    workspace: Path,
    agent_id: str,
    previous: dict[str, Any],
    adapter: Any,
    exclude_session_ids: set[str],
) -> dict[str, Any] | None:
    events_path = logs_dir(workspace) / "events.jsonl"
    try:
        lines = events_path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    current_session_id = str(previous.get("session_id") or "")
    for line in reversed(lines):
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("event") != "session.captured" or event.get("agent_id") != agent_id:
            continue
        session_id = str(event.get("session_id") or "")
        if not session_id or session_id == current_session_id or session_id in exclude_session_ids:
            continue
        candidate = dict(previous)
        candidate.update(
            {
                "session_id": session_id,
                "rollout_path": event.get("rollout_path"),
                "captured_at": event.get("ts"),
                "captured_via": "event_log_repair",
                "attribution_confidence": event.get("attribution_confidence"),
            }
        )
        if adapter.session_is_resumable(candidate, workspace):
            return candidate
    return None


def shutdown(workspace: Path, keep_logs: bool = True) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    session_name = state.get("session_name")
    event_log = EventLog(workspace)
    captured: list[str] = []
    closed_displays: set[str] = set()
    missing_before = [agent_id for agent_id, agent_state in state.get("agents", {}).items() if not agent_state.get("session_id")]
    fallback_captured = _capture_missing_sessions(workspace, state, event_log, timeout_s=2.0, log_miss=False)
    event_log.write("shutdown.session_capture_checked", missing_before=missing_before, captured=fallback_captured)
    for agent_id, agent_state in state.get("agents", {}).items():
        if not agent_state.get("session_id"):
            event_log.write(
                "shutdown.session_capture_missed",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                spawn_cwd=agent_state.get("spawn_cwd"),
            )
    coordinator = stop_coordinator(workspace)
    if session_name and _tmux_session_exists(session_name):
        leader_receiver = state.get("leader_receiver", {})
        leader_window = leader_receiver.get("window") if leader_receiver.get("mode") != "direct_tmux" else None
        if leader_window and _tmux_window_exists(session_name, leader_window):
            log_path = logs_dir(workspace) / f"{leader_window}.scrollback"
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-", "-t", f"{session_name}:{leader_window}"], timeout=10)
            if proc.returncode == 0:
                log_path.write_text(proc.stdout, encoding="utf-8")
                captured.append(str(log_path))
        for agent_id, agent_state in state.get("agents", {}).items():
            window = agent_state.get("window", agent_id)
            log_path = logs_dir(workspace) / f"{agent_id}.scrollback"
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-", "-t", f"{session_name}:{window}"], timeout=10)
            if proc.returncode == 0:
                log_path.write_text(proc.stdout, encoding="utf-8")
                captured.append(str(log_path))
        _close_ghostty_workspace(state, event_log)
        for agent_id, agent_state in state.get("agents", {}).items():
            _close_ghostty_display(agent_id, agent_state, event_log)
            closed_displays.add(agent_id)
        proc = run_cmd(["tmux", "kill-session", "-t", session_name], timeout=10)
        if proc.returncode != 0:
            if "can't find session" in proc.stderr:
                event_log.write("shutdown.idempotent", session=session_name, reason="session disappeared before kill")
            else:
                raise RuntimeError(f"tmux kill-session failed: {proc.stderr.strip()}")
        else:
            event_log.write("shutdown.kill_session", session=session_name, keep_logs=keep_logs, captured=captured)
    else:
        event_log.write("shutdown.idempotent", session=session_name, reason="session missing")
        _close_ghostty_workspace(state, event_log)
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_id not in closed_displays:
            _close_ghostty_display(agent_id, agent_state, event_log)
        mcp_path = Path(agent_state["mcp_config"]) if agent_state.get("mcp_config") else None
        try:
            get_adapter(agent_state["provider"]).cleanup_mcp(workspace, agent_id, mcp_path)
            event_log.write(
                "shutdown.mcp_cleanup",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                mcp_config=str(mcp_path) if mcp_path else None,
            )
        except Exception as exc:
            event_log.write(
                "shutdown.mcp_cleanup_failed",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                mcp_config=str(mcp_path) if mcp_path else None,
                error=str(exc),
            )
        if agent_state.get("status") != "paused":
            agent_state["status"] = "stopped"
    save_runtime_state(workspace, state)
    _save_team_runtime_snapshot(workspace, state)
    return {"ok": True, "session_name": session_name, "logs": captured, "coordinator": coordinator}


def restart(workspace: Path, allow_fresh: bool = False, team: str | None = None) -> dict[str, Any]:
    state = _select_restart_state(workspace, team)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    team_dir = Path(str(state.get("team_dir"))) if state.get("team_dir") else _spec_team_dir(spec_path, workspace)
    if _is_team_doc_dir(team_dir):
        compiled = _compile_team_dir_spec(team_dir, workspace)
        spec = compiled["spec"]
        spec_path = team_dir / "team.spec.yaml"
        state["spec_path"] = str(spec_path)
    else:
        if not spec_path.exists():
            raise RuntimeError(f"missing spec for restart: {spec_path}")
        spec = load_spec(spec_path)
    _attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
    ensure_workspace_dirs(workspace)
    event_log = EventLog(workspace)
    session_name = state.get("session_name") or spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    state.setdefault("team_dir", str(team_dir))
    if _tmux_session_exists(session_name):
        event_log.write(
            "restart.session_conflict",
            session=session_name,
            action="use a different team name or runtime.session_name; do not terminate existing tmux sessions from restart",
        )
        raise RuntimeError(_tmux_session_conflict_error(session_name))
    runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
    display_backend = spec.get("runtime", {}).get("display_backend", state.get("display_backend", "none"))
    _close_ghostty_workspace(state, event_log)
    for agent_id, agent_state in state.get("agents", {}).items():
        _close_ghostty_display(agent_id, agent_state, event_log)
    state["display_backend"] = display_backend
    restart_agents = [
        agent
        for agent in spec.get("agents", [])
        if state.get("agents", {}).get(agent["id"], {}).get("status") != "paused" and not agent.get("paused")
    ]
    _ensure_agent_start_requirements(workspace, restart_agents, event_log, "restart")
    first = True
    restarted: list[dict[str, Any]] = []
    new_agents: dict[str, Any] = {}
    display_jobs: list[tuple[str, dict[str, Any]]] = []
    for agent in spec.get("agents", []):
        previous = state.get("agents", {}).get(agent["id"], {})
        if previous.get("status") == "paused" or agent.get("paused"):
            new_agents[agent["id"]] = dict(previous or {"status": "paused", "provider": agent["provider"]})
            new_agents[agent["id"]]["status"] = "paused"
            continue
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                "restart.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
        mcp_config = adapter.mcp_config(workspace, agent["id"])
        mcp_path = adapter.install_mcp(workspace, agent["id"], mcp_config)
        command_agent = copy.deepcopy(agent)
        command_agent["_runtime"] = runtime_cfg
        previous = _attach_profile_resume_root(workspace, command_agent, previous)
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in {**state.get("agents", {}), **new_agents}.items()
            if aid != agent["id"] and item.get("session_id")
        }
        try:
            previous = _prepare_resume_state(
                workspace,
                agent["id"],
                previous,
                adapter,
                event_log,
                known_session_ids,
                allow_fresh_on_resume_failure=allow_fresh,
            )
        except ResumeUnavailable as exc:
            try:
                adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
            except Exception as cleanup_exc:
                event_log.write(
                    "restart.mcp_cleanup_failed",
                    agent_id=agent["id"],
                    provider=agent["provider"],
                    mcp_config=str(mcp_path),
                    error=str(cleanup_exc),
                )
            raise RuntimeError(str(exc)) from exc
        restart_mode = "resumed" if previous.get("session_id") else "fresh"
        if restart_mode == "resumed":
            try:
                command = shell_resume_command_for_agent(command_agent, previous, workspace, mcp_config)
            except ResumeUnavailable as exc:
                event_log.write("restart.resume_unavailable", agent_id=agent["id"], error=str(exc))
                if not allow_fresh:
                    try:
                        adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
                    except Exception as cleanup_exc:
                        event_log.write(
                            "restart.mcp_cleanup_failed",
                            agent_id=agent["id"],
                            provider=agent["provider"],
                            mcp_config=str(mcp_path),
                            error=str(cleanup_exc),
                        )
                    raise RuntimeError(
                        f"Cannot resume agent {agent['id']}: {exc}. "
                        "Use team-agent restart --allow-fresh only if losing that worker context is acceptable."
                    ) from exc
                command = shell_command_for_agent(command_agent, workspace, mcp_config)
                restart_mode = "fresh"
        else:
            command = shell_command_for_agent(command_agent, workspace, mcp_config)
            event_log.write("restart.fresh_spawn", agent_id=agent["id"], provider=agent["provider"], reason="session_id_missing")
        event_log.write(
            "restart.agent_start",
            agent_id=agent["id"],
            provider=agent["provider"],
            restart_mode=restart_mode,
            session_id=previous.get("session_id"),
            session=session_name,
            window=agent["id"],
            tmux_start_mode="new-session" if first else "new-window",
            command=command,
            mcp_config=str(mcp_path),
        )
        if first:
            proc = run_cmd(["tmux", "new-session", "-d", "-s", session_name, "-n", agent["id"], "sh", "-lc", command])
            first = False
        else:
            proc = run_cmd(["tmux", "new-window", "-t", session_name, "-n", agent["id"], "sh", "-lc", command])
        if proc.returncode != 0:
            raise RuntimeError(f"Failed to restart agent {agent['id']}: {proc.stderr.strip()}")
        if not _handle_startup_prompts_and_verify_window(
            adapter, event_log, "restart", agent["id"], agent["provider"], session_name, restart_mode
        ):
            if restart_mode != "resumed":
                raise RuntimeError(f"Failed to restart agent {agent['id']}: tmux window exited after start")
            if not allow_fresh:
                try:
                    adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
                except Exception as cleanup_exc:
                    event_log.write(
                        "restart.mcp_cleanup_failed",
                        agent_id=agent["id"],
                        provider=agent["provider"],
                        mcp_config=str(mcp_path),
                        error=str(cleanup_exc),
                    )
                raise RuntimeError(
                    f"Cannot resume agent {agent['id']}: resume window exited or did not become visible. "
                    "Use team-agent restart --allow-fresh only if losing that worker context is acceptable."
                )
            event_log.write(
                "restart.resume_window_missing_fallback_fresh",
                agent_id=agent["id"],
                provider=agent["provider"],
                session_id=previous.get("session_id"),
            )
            command = shell_command_for_agent(command_agent, workspace, mcp_config)
            restart_mode = "fresh"
            tmux_cmd, tmux_start_mode = _tmux_start_command_for_agent_window(session_name, agent["id"], command)
            event_log.write(
                "restart.agent_start",
                agent_id=agent["id"],
                provider=agent["provider"],
                restart_mode=restart_mode,
                session_id=None,
                session=session_name,
                window=agent["id"],
                tmux_start_mode=tmux_start_mode,
                command=command,
                mcp_config=str(mcp_path),
            )
            proc = run_cmd(tmux_cmd)
            if proc.returncode != 0:
                raise RuntimeError(f"Failed to restart agent {agent['id']} fresh after resume exit: {proc.stderr.strip()}")
            if not _handle_startup_prompts_and_verify_window(
                adapter, event_log, "restart", agent["id"], agent["provider"], session_name, restart_mode
            ):
                raise RuntimeError(f"Failed to restart agent {agent['id']} fresh: tmux window exited after start")
        spawn_time = datetime.now(timezone.utc)
        agent_state = dict(previous)
        agent_state.update(
            {
                "status": "running",
                "provider": agent["provider"],
                "agent_id": agent["id"],
                "model": agent.get("model"),
                "auth_mode": agent.get("auth_mode"),
                "profile": agent.get("profile"),
                "window": agent["id"],
                "mcp_config": str(mcp_path),
                "permissions": resolve_permissions(agent),
                "spawn_cwd": str(workspace),
                "spawned_at": spawn_time.isoformat(),
            }
        )
        profile_launch = command_agent.get("_provider_profile") or {}
        if profile_launch.get("claude_projects_root"):
            agent_state["claude_projects_root"] = profile_launch["claude_projects_root"]
        if restart_mode == "fresh":
            _clear_session_capture_fields(agent_state)
            if command_agent.get("_session_id"):
                agent_state["_pending_session_id"] = command_agent["_session_id"]
            _capture_agent_session(
                workspace,
                agent["id"],
                agent_state,
                event_log,
                timeout_s=1.5,
                exclude_session_ids=known_session_ids,
            )
        if display_backend in GHOSTTY_DISPLAY_BACKENDS:
            display_jobs.append((agent["id"], agent))
        new_agents[agent["id"]] = agent_state
        restarted.append(
            {
                "agent_id": agent["id"],
                "restart_mode": restart_mode,
                "session_id": agent_state.get("session_id"),
                "display_target": None,
            }
        )
    display_results = _open_worker_displays(workspace, session_name, display_jobs, event_log, display_backend)
    for agent_id, display in display_results.items():
        if agent_id in new_agents:
            new_agents[agent_id]["display"] = display
    for item in restarted:
        agent_id = item["agent_id"]
        if agent_id in display_results:
            item["display_target"] = display_results[agent_id]
    missing_after_start = [item["agent_id"] for item in restarted if not _tmux_window_exists(session_name, item["agent_id"])]
    if missing_after_start:
        for agent_id in missing_after_start:
            event_log.write("restart.agent_missing_after_start", agent_id=agent_id, target=f"{session_name}:{agent_id}")
        rollback = _rollback_restart_session(session_name, event_log)
        raise RuntimeError(
            f"Failed to restart agent {missing_after_start[0]}: tmux window exited after start; "
            f"rollback_session_ok={rollback.get('ok')}"
        )
    state["session_name"] = session_name
    state["agents"] = new_agents
    save_runtime_state(workspace, state)
    _save_team_runtime_snapshot(workspace, state)
    MessageStore(workspace)
    write_team_state(workspace, spec, state)
    coordinator = start_coordinator(workspace)
    event_log.write("restart.complete", session=session_name, agents=restarted, coordinator=coordinator)
    return {"ok": True, "session_name": session_name, "agents": restarted, "coordinator": coordinator}


def _rollback_restart_session(session_name: str, event_log: EventLog) -> dict[str, Any]:
    proc = run_cmd(["tmux", "kill-session", "-t", session_name], timeout=10)
    result = {
        "ok": proc.returncode == 0,
        "session": session_name,
        "stdout": proc.stdout.strip(),
        "stderr": proc.stderr.strip(),
    }
    event_log.write("restart.rollback_session", **result)
    return result


def stop_agent(workspace: Path, agent_id: str) -> dict[str, Any]:
    from team_agent.lifecycle.operations import stop_agent as impl

    return impl(workspace, agent_id)


def reset_agent(workspace: Path, agent_id: str, *, discard_session: bool = False, open_display: bool = True) -> dict[str, Any]:
    from team_agent.lifecycle.operations import reset_agent as impl

    return impl(workspace, agent_id, discard_session=discard_session, open_display=open_display)


def add_agent(workspace: Path, agent_id: str, *, role_file_path: str, open_display: bool = True) -> dict[str, Any]:
    from team_agent.lifecycle.operations import add_agent as impl

    return impl(workspace, agent_id, role_file_path=role_file_path, open_display=open_display)


def fork_agent(
    workspace: Path,
    source_agent_id: str,
    *,
    as_agent_id: str,
    label: str | None = None,
    open_display: bool = True,
) -> dict[str, Any]:
    from team_agent.lifecycle.operations import fork_agent as impl

    return impl(workspace, source_agent_id, as_agent_id=as_agent_id, label=label, open_display=open_display)


def start_agent(
    workspace: Path,
    agent_id: str,
    force: bool = False,
    open_display: bool = True,
    allow_fresh: bool = False,
) -> dict[str, Any]:
    from team_agent.lifecycle.start import start_agent as impl

    return impl(workspace, agent_id, force=force, open_display=open_display, allow_fresh=allow_fresh)


def remove_agent(
    workspace: Path,
    agent_id: str,
    *,
    from_spec: bool = False,
    confirm: bool = False,
    force: bool = False,
) -> dict[str, Any]:
    from team_agent.lifecycle.agents import remove_agent as lifecycle_remove_agent

    with _runtime_lock(workspace, "remove-agent"):
        return lifecycle_remove_agent(workspace, agent_id, from_spec=from_spec, confirm=confirm, force=force)


def _start_agent_unlocked(workspace: Path, agent_id: str, force: bool, open_display: bool, allow_fresh: bool) -> dict[str, Any]:
    from team_agent.lifecycle.start import _start_agent_unlocked as impl

    return impl(workspace, agent_id, force, open_display, allow_fresh)


def _running_agent_state(workspace: Path, agent: dict[str, Any], previous: dict[str, Any]) -> dict[str, Any]:
    agent_state = dict(previous)
    agent_state.update(
        {
            "status": "running",
            "provider": agent["provider"],
            "agent_id": agent["id"],
            "model": agent.get("model"),
            "auth_mode": agent.get("auth_mode"),
            "profile": agent.get("profile"),
            "window": agent["id"],
            "permissions": resolve_permissions(agent),
            "spawn_cwd": str(workspace),
        }
    )
    return agent_state


def _handle_startup_prompts_and_verify_window(
    adapter: Any,
    event_log: EventLog,
    event_prefix: str,
    agent_id: str,
    provider: str,
    session_name: str,
    start_mode: str,
) -> bool:
    handled_prompts = adapter.handle_startup_prompts(session_name, agent_id, checks=1, sleep_s=0.0)
    for prompt_event in handled_prompts:
        event_log.write(f"{event_prefix}.startup_prompt_handled", agent_id=agent_id, provider=provider, **prompt_event)
    deadline = time.monotonic() + 1.0
    saw_window = False
    while True:
        if _tmux_window_exists(session_name, agent_id):
            saw_window = True
            if time.monotonic() >= deadline:
                return True
        elif saw_window or time.monotonic() >= deadline:
            break
        time.sleep(0.2)
    event_log.write(
        f"{event_prefix}.window_missing_after_start",
        agent_id=agent_id,
        provider=provider,
        start_mode=start_mode,
        target=f"{session_name}:{agent_id}",
        saw_window=saw_window,
    )
    return False


def coordinator_pid_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.pid"


def coordinator_meta_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.json"


def coordinator_log_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.log"


def _pid_is_running(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    proc = run_cmd(["ps", "-p", str(pid), "-o", "stat="], timeout=5)
    if proc.returncode == 0 and proc.stdout.strip().upper().startswith("Z"):
        return False
    return True


def _read_coordinator_metadata(workspace: Path) -> dict[str, Any] | None:
    path = coordinator_meta_path(workspace)
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return raw if isinstance(raw, dict) else None


def _coordinator_metadata_ok(metadata: dict[str, Any] | None, pid: int) -> bool:
    return bool(
        metadata
        and metadata.get("pid") == pid
        and metadata.get("protocol_version") == COORDINATOR_PROTOCOL_VERSION
        and metadata.get("message_store_schema_version") == MessageStore.SCHEMA_VERSION
    )


def write_coordinator_metadata(workspace: Path, pid: int, source: str) -> None:
    path = coordinator_meta_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "pid": pid,
                "protocol_version": COORDINATOR_PROTOCOL_VERSION,
                "message_store_schema_version": MessageStore.SCHEMA_VERSION,
                "source": source,
                "updated_at": datetime.now(timezone.utc).isoformat(),
            },
            indent=2,
        ),
        encoding="utf-8",
    )


def coordinator_health(workspace: Path) -> dict[str, Any]:
    schema = _message_store_schema_health(workspace)
    pid_path = coordinator_pid_path(workspace)
    if not pid_path.exists():
        return {"ok": False, "status": "missing", "pid": None, "metadata": None, "metadata_ok": False, **schema}
    try:
        pid = int(pid_path.read_text(encoding="utf-8").strip())
    except ValueError:
        return {"ok": False, "status": "invalid_pid", "pid": None, "metadata": None, "metadata_ok": False, **schema}
    running = _pid_is_running(pid)
    metadata = _read_coordinator_metadata(workspace)
    metadata_ok = _coordinator_metadata_ok(metadata, pid)
    ok = running and metadata_ok and bool(schema.get("schema_ok"))
    return {
        "ok": ok,
        "status": "running" if running else "stale",
        "pid": pid,
        "metadata": metadata,
        "metadata_ok": metadata_ok,
        **schema,
    }


def start_coordinator(workspace: Path) -> dict[str, Any]:
    ensure_workspace_dirs(workspace)
    health = coordinator_health(workspace)
    if health["ok"]:
        return {"ok": True, "pid": health["pid"], "status": "already_running", "log": str(coordinator_log_path(workspace))}
    if health["status"] == "running" and not health.get("metadata_ok"):
        EventLog(workspace).write(
            "coordinator.restart_incompatible",
            pid=health.get("pid"),
            metadata=health.get("metadata"),
            expected_protocol=COORDINATOR_PROTOCOL_VERSION,
            expected_schema=MessageStore.SCHEMA_VERSION,
        )
        stopped = stop_coordinator(workspace)
        if not stopped.get("ok"):
            EventLog(workspace).write(
                "coordinator.restart_incompatible_stop_failed",
                pid=health.get("pid"),
                stop_result=stopped,
            )
            return {
                "ok": False,
                "pid": health.get("pid"),
                "status": "restart_incompatible_stop_failed",
                "error": stopped.get("error") or stopped.get("status"),
                "stop_result": stopped,
            }
    if not health.get("schema_ok", False):
        EventLog(workspace).write(
            "coordinator.schema_incompatible",
            error=health.get("schema_error"),
            schema=health.get("schema"),
        )
        return {
            "ok": False,
            "pid": None,
            "status": "schema_incompatible",
            "error": health.get("schema_error"),
            "schema": health.get("schema"),
        }
    if health["status"] in {"stale", "invalid_pid"}:
        coordinator_pid_path(workspace).unlink(missing_ok=True)
        coordinator_meta_path(workspace).unlink(missing_ok=True)
    log_path = coordinator_log_path(workspace)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    repo_src = str(Path(__file__).resolve().parents[1])
    env["PYTHONPATH"] = repo_src + (os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else "")
    log = log_path.open("a", encoding="utf-8")
    proc = subprocess.Popen(
        [sys.executable, "-m", "team_agent.coordinator", "--workspace", str(workspace)],
        cwd=str(workspace),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        env=env,
        start_new_session=True,
    )
    log.close()
    coordinator_pid_path(workspace).write_text(str(proc.pid), encoding="utf-8")
    write_coordinator_metadata(workspace, proc.pid, source="start")
    EventLog(workspace).write("coordinator.started", pid=proc.pid, log=str(log_path))
    return {"ok": True, "pid": proc.pid, "status": "started", "log": str(log_path)}


def _message_store_schema_health(workspace: Path) -> dict[str, Any]:
    try:
        MessageStore(workspace)
    except Exception as exc:
        return {
            "schema_ok": False,
            "schema_error": str(exc),
            "schema": {"message_store_schema_version": MessageStore.SCHEMA_VERSION},
        }
    return {
        "schema_ok": True,
        "schema_error": None,
        "schema": {
            "message_store_schema_version": MessageStore.SCHEMA_VERSION,
        },
    }


def stop_coordinator(workspace: Path) -> dict[str, Any]:
    pid_path = coordinator_pid_path(workspace)
    if not pid_path.exists():
        return {"ok": True, "status": "missing"}
    try:
        pid = int(pid_path.read_text(encoding="utf-8").strip())
    except ValueError:
        pid_path.unlink(missing_ok=True)
        coordinator_meta_path(workspace).unlink(missing_ok=True)
        return {"ok": True, "status": "invalid_pid_removed"}
    if _pid_is_running(pid):
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError as exc:
            return {"ok": False, "status": "kill_failed", "pid": pid, "error": str(exc)}
    pid_path.unlink(missing_ok=True)
    coordinator_meta_path(workspace).unlink(missing_ok=True)
    EventLog(workspace).write("coordinator.stopped", pid=pid)
    return {"ok": True, "status": "stopped", "pid": pid}


def coordinator_tick(workspace: Path) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    event_log = EventLog(workspace)
    store = MessageStore(workspace)
    session_name = state.get("session_name")
    if session_name and not _tmux_session_exists(session_name):
        event_log.write("coordinator.session_missing", session=session_name)
        return {"ok": False, "stop": True, "reason": "tmux_session_missing"}
    _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)
    _refresh_agent_runtime_statuses(workspace, state, event_log)
    _handle_provider_startup_prompts(workspace, state, event_log)
    _handle_provider_runtime_prompts(workspace, state, event_log)
    _sync_agent_health(workspace, state, store)
    delivered = _deliver_pending_messages(workspace, state, event_log)
    fired = _fire_due_scheduled_events(workspace, store, event_log)
    stuck = _detect_stuck_agents(workspace, state, store, event_log)
    save_runtime_state(workspace, state)
    results = _collect_results_and_notify_watchers(workspace, event_log)
    return {"ok": True, "stop": False, "delivered": delivered, "scheduled": fired, "stuck": stuck, "results": results}


def _collect_results_and_notify_watchers(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _collect_results_and_notify_watchers as impl

    return impl(*args, **kwargs)


def _notify_result_watchers(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _notify_result_watchers as impl

    return impl(*args, **kwargs)


def _watcher_matches_result(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _watcher_matches_result as impl

    return impl(*args, **kwargs)


def _format_result_watcher_notification(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.results import _format_result_watcher_notification as impl

    return impl(*args, **kwargs)


def _ensure_agent_start_requirements(
    workspace: Path,
    agents: list[dict[str, Any]],
    event_log: EventLog,
    event_prefix: str,
    skip_profile_smoke: bool = False,
) -> None:
    active_agents = [agent for agent in agents if not agent.get("paused")]
    for agent in active_agents:
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                f"{event_prefix}.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
    profile_checks = _profile_checks_for_agents(workspace, active_agents)
    profile_failures = [item for item in profile_checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.profile_check", ok=not profile_failures, checks=[compact_profile_check(item) for item in profile_checks])
    if profile_failures:
        raise RuntimeError(_format_profile_check_failures(profile_failures))
    if skip_profile_smoke:
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=True, skipped=True, reason="already_checked")
    else:
        smoke_checks = _profile_smoke_checks_for_agents(workspace, active_agents)
        smoke_failures = [item for item in smoke_checks if item.get("ok") is False]
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=not smoke_failures, checks=[compact_profile_check(item) for item in smoke_checks])
        if smoke_failures:
            raise RuntimeError(_format_profile_smoke_failures(smoke_failures))
    checks = _model_checks_for_agents(active_agents, workspace)
    failures = [item for item in checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.model_check", ok=not failures, checks=_compact_model_checks(checks))
    if failures:
        raise RuntimeError(_format_model_check_failures(failures))


def _profile_checks_for_agents(workspace: Path, agents: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [validate_agent_profile(workspace, agent) for agent in agents if not agent.get("paused")]


def _profile_smoke_checks_for_agents(workspace: Path, agents: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [smoke_check_agent_profile(workspace, agent) for agent in agents if not agent.get("paused")]


def _model_checks_for_agents(agents: list[dict[str, Any]], workspace: Path | None = None) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    for agent in agents:
        if agent.get("paused"):
            continue
        if agent.get("auth_mode") == "compatible_api" and agent.get("provider") == "codex":
            checks.append(
                {
                    "ok": True,
                    "status": "profile_model_deferred_to_smoke",
                    "provider": agent["provider"],
                    "model": effective_model(agent, workspace),
                    "agent_id": agent["id"],
                }
            )
            continue
        adapter = get_adapter(agent["provider"])
        validator = getattr(adapter, "validate_model", None)
        model = effective_model(agent, workspace)
        if not callable(validator):
            result = {"ok": True, "status": "not_checked", "provider": agent["provider"], "model": model}
        else:
            result = validator(model)
            if not isinstance(result, dict):
                result = {"ok": True, "status": "not_checked", "provider": agent["provider"], "model": model}
        result = dict(result)
        result.setdefault("provider", agent["provider"])
        result.setdefault("model", model)
        result["agent_id"] = agent["id"]
        checks.append(result)
    return checks


def _compact_model_checks(checks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    compact: list[dict[str, Any]] = []
    for item in checks:
        compact.append(
            {
                key: item.get(key)
                for key in ["agent_id", "provider", "model", "ok", "status", "reason", "suggested_model", "command"]
                if key in item
            }
        )
    return compact


def _format_model_check_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["model validation failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: provider={item.get('provider')} model={item.get('model')!r}"
        if item.get("suggested_model"):
            message += f" is not an exact model id; use {item['suggested_model']!r}"
        else:
            message += f" is unsupported ({item.get('reason') or item.get('status')})"
        lines.append(message)
    return "\n".join(lines)


def _format_profile_check_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["profile validation failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: profile={item.get('profile')!r} auth_mode={item.get('auth_mode')}"
        if item.get("missing_required"):
            message += f" missing {', '.join(item['missing_required'])}"
        else:
            message += f" failed ({item.get('reason') or item.get('status')})"
        if item.get("suggestion"):
            message += f"; {item['suggestion']}"
        lines.append(message)
    return "\n".join(lines)


def _format_profile_smoke_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["provider profile smoke check failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: provider={item.get('provider')} profile={item.get('profile')!r}"
        message += f" status={item.get('status')} reason={item.get('reason') or 'unknown'}"
        if item.get("http_status"):
            message += f" http_status={item['http_status']}"
        if item.get("error"):
            message += f"; {item['error']}"
        message += "; fix the local profile file or model id, then start again"
        lines.append(message)
    return "\n".join(lines)


def _fire_due_scheduled_events(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.scheduler import _fire_due_scheduled_events as impl

    return impl(*args, **kwargs)


def _schedule_send_retry(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.scheduler import _schedule_send_retry as impl

    return impl(*args, **kwargs)


def _detect_stuck_agents(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.scheduler import _detect_stuck_agents as impl

    return impl(*args, **kwargs)


def diagnose(workspace: Path) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path) if spec_path.exists() else {}
    store = MessageStore(workspace)
    issues: list[dict[str, Any]] = []
    suggested_repairs: list[dict[str, Any]] = [
        {
            "kind": "mcp_approval_prompt",
            "action": "If a worker pane asks to allow team_orchestrator, select Allow for this session; then run team-agent collect.",
        },
        {
            "kind": "codex_command_approval_prompt",
            "action": "If a worker pane asks to run a shell command, approve only after checking the command; long servers should use pid/log/health-check protocol.",
        },
        {
            "kind": "interrupted_worker",
            "action": "Send: Continue from the current interrupted prompt. Do not redo completed work. Do the next bounded step, then report result_envelope_v1.",
        },
        {
            "kind": "leader_receiver",
            "action": "Worker-to-leader status requires a direct tmux leader receiver. Run team-agent attach-leader --workspace . --provider codex, or pass --pane <pane_id>.",
        },
        {
            "kind": "process_list_unavailable",
            "action": "If pgrep/lsof fail, use pid files, logs, and health-check URLs; record the environment blocker instead of retrying process-list commands.",
        },
    ]
    session_name = state.get("session_name")
    if session_name and not _tmux_session_exists(session_name):
        issues.append(
            {
                "kind": "tmux_session_missing",
                "session": session_name,
                "reason": "tmux has no matching session",
                "suggestion": "Run team-agent launch again or inspect .team/logs/events.jsonl for the shutdown/failure event.",
            }
        )
    leader_receiver = state.get("leader_receiver", {})
    if not _leader_receiver_is_direct(leader_receiver):
        issues.append(
            {
                "kind": "leader_not_attached",
                "mode": leader_receiver.get("mode", "fallback_inbox" if leader_receiver else "none"),
                "suggestion": "Run team-agent attach-leader --workspace . --provider codex, or pass --pane <pane_id> for the existing Codex leader pane.",
            }
        )
    else:
        validation = _validate_leader_receiver(leader_receiver)
        if not validation["ok"]:
            issues.append(
                {
                    "kind": validation["reason"],
                    "target": leader_receiver.get("pane_id"),
                    "provider": leader_receiver.get("provider"),
                    "error": validation.get("error"),
                    "suggestion": "Run team-agent attach-leader --workspace . --provider codex again with a live Codex pane.",
                }
            )
        elif validation.get("warning"):
            issues.append(
                {
                    "kind": "leader_command_unexpected",
                    "target": leader_receiver.get("pane_id"),
                    "provider": leader_receiver.get("provider"),
                    "command": validation.get("pane", {}).get("pane_current_command"),
                    "warning": validation["warning"],
                    "suggestion": "If this is not the real Codex leader pane, rerun attach-leader with --pane <pane_id>.",
                }
            )
    for agent in spec.get("agents", []):
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            issues.append(
                {
                    "kind": "provider_missing",
                    "agent_id": agent["id"],
                    "provider": agent["provider"],
                    "command": adapter.command_name,
                    "suggestion": f"Install {adapter.command_name} and authenticate it before launch.",
                }
            )
        mcp_path = runtime_dir(workspace) / "mcp" / f"{agent['id']}.json"
        if not mcp_path.exists():
            issues.append(
                {
                    "kind": "mcp_not_installed",
                    "agent_id": agent["id"],
                    "provider": agent["provider"],
                    "path": str(mcp_path),
                    "suggestion": "Run team-agent launch to regenerate provider MCP config.",
                }
            )
        agent_state = state.get("agents", {}).get(agent["id"], {})
        if agent_state.get("status") == "interrupted":
            issues.append(
                {
                    "kind": "worker_interrupted",
                    "agent_id": agent["id"],
                    "suggestion": "Send the standard recovery prompt instead of redispatching the full task.",
                }
            )
        window = agent_state.get("window", agent["id"])
        if session_name and _tmux_window_exists(session_name, window):
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-80", "-t", f"{session_name}:{window}"], timeout=5)
            output = proc.stdout if proc.returncode == 0 else ""
            if _capture_has_team_orchestrator_mcp_prompt(output):
                issues.append(
                    {
                        "kind": "mcp_approval_prompt",
                        "agent_id": agent["id"],
                        "suggestion": "Team Agent will auto-approve allowlisted internal MCP prompts; if still blocked, inspect team-agent approvals.",
                    }
                )
            if "Would you like to run the following command" in output:
                issues.append(
                    {
                        "kind": "codex_command_approval_prompt",
                        "agent_id": agent["id"],
                        "suggestion": "Review and approve or reject the command in the worker pane; do not keep waiting silently.",
                    }
                )
            if "Conversation interrupted" in output:
                issues.append(
                    {
                        "kind": "worker_interrupted",
                        "agent_id": agent["id"],
                        "suggestion": "Send the standard recovery prompt instead of redispatching the full task.",
                    }
                )
    timeout_sec = int(spec.get("communication", {}).get("ack_timeout_sec", 60)) if spec else 60
    failed_messages = store.fail_timeouts(timeout_sec)
    for message_id in failed_messages:
        issues.append(
            {
                "kind": "message_ack_timeout",
                "message_id": message_id,
                "suggestion": "Check target worker status and scrollback; message stayed unacknowledged past timeout.",
            }
        )
    return {
        "ok": not issues,
        "issues": issues,
        "suggested_repairs": suggested_repairs,
        "runtime": status(workspace, as_json=True),
        "event_log": str(logs_dir(workspace) / "events.jsonl"),
    }


def doctor(spec_path: Path | None = None) -> dict[str, Any]:
    providers = ["codex"]
    spec = None
    workspace = Path.cwd()
    if spec_path:
        spec = load_spec(spec_path)
        workspace = workspace_from_spec(spec, spec_path)
        _attach_team_profile_dirs(spec, spec_path, workspace)
        providers = sorted({a["provider"] for a in spec.get("agents", []) if a["provider"] != "fake"})
    checks: dict[str, Any] = {
        "tmux": {
            "installed": bool(shutil_which("tmux")),
            "path": shutil_which("tmux"),
        },
        "workspace": str(workspace),
        "workspace_is_git_repo": (workspace / ".git").exists(),
        "providers": {},
        "mcp": {
            "server_command": shutil_which("team_orchestrator"),
            "local_module": True,
        },
        "coordinator": coordinator_health(workspace),
    }
    for provider in providers:
        adapter = get_adapter(provider)
        checks["providers"][provider] = {
            "command": adapter.command_name,
            "installed": adapter.is_installed(),
            "version": adapter.version(),
            "auth": adapter.auth_hint(),
        }
    model_checks = _model_checks_for_agents(spec.get("agents", []), workspace) if spec else []
    if spec:
        checks["models"] = _compact_model_checks(model_checks)
        profile_checks = _profile_checks_for_agents(workspace, spec.get("agents", []))
        checks["profiles"] = [compact_profile_check(item) for item in profile_checks]
    missing_required = [
        provider for provider, result in checks["providers"].items() if not result["installed"] and spec_path
    ]
    missing_auth = [
        provider
        for provider, result in checks["providers"].items()
        if spec_path and result.get("auth", {}).get("status") == "missing"
    ]
    invalid_models = [item for item in model_checks if item.get("ok") is False]
    invalid_profiles = [item for item in checks.get("profiles", []) if item.get("ok") is False]
    checks["ok"] = (
        checks["tmux"]["installed"]
        and not missing_required
        and not missing_auth
        and not invalid_models
        and not invalid_profiles
    )
    if missing_required:
        checks["missing_required_providers"] = missing_required
    if missing_auth:
        checks["missing_provider_auth"] = missing_auth
    if invalid_models:
        checks["invalid_models"] = _compact_model_checks(invalid_models)
    if invalid_profiles:
        checks["invalid_profiles"] = invalid_profiles
    return checks


def preflight(team_dir: Path) -> dict[str, Any]:
    from team_agent.compiler import compile_team
    from team_agent.profiles import profile_dir

    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    ensure_workspace_dirs(workspace)
    _ensure_profiles_for_roles(team_dir)
    event_log = EventLog(workspace)
    checks: list[dict[str, Any]] = []
    ok = True
    spec = None
    try:
        compiled = compile_team(team_dir)
        spec = compiled["spec"]
        _attach_team_profile_dirs(spec, team_dir / "team.spec.yaml", workspace, team_dir)
        checks.append({"name": "compile", "ok": True, "agents": [a["id"] for a in spec.get("agents", [])]})
    except Exception as exc:
        ok = False
        checks.append({"name": "compile", "ok": False, "error": str(exc)})
    tmux_path = shutil_which("tmux")
    checks.append({"name": "tmux", "ok": bool(tmux_path), "path": tmux_path})
    ok = ok and bool(tmux_path)
    ghostty = _ghostty_command()
    ghostty_check = {"name": "ghostty", "ok": bool(ghostty), "path": ghostty, "required": False}
    if spec and spec.get("runtime", {}).get("display_backend") in GHOSTTY_DISPLAY_BACKENDS:
        ghostty_check["required"] = True
        ok = ok and bool(ghostty)
    checks.append(ghostty_check)
    if spec:
        profile_checks = _profile_checks_for_agents(workspace, spec.get("agents", []))
        profile_failures = [item for item in profile_checks if item.get("ok") is False]
        checks.append({"name": "profiles", "ok": not profile_failures, "checks": [compact_profile_check(item) for item in profile_checks]})
        ok = ok and not profile_failures
        smoke_checks = _profile_smoke_checks_for_agents(workspace, spec.get("agents", []))
        smoke_failures = [item for item in smoke_checks if item.get("ok") is False]
        checks.append({"name": "profile_smoke", "ok": not smoke_failures, "checks": [compact_profile_check(item) for item in smoke_checks]})
        ok = ok and not smoke_failures
        model_checks = _model_checks_for_agents(spec.get("agents", []), workspace)
        model_failures = [item for item in model_checks if item.get("ok") is False]
        checks.append({"name": "models", "ok": not model_failures, "checks": _compact_model_checks(model_checks)})
        ok = ok and not model_failures
    core = core_binary()
    checks.append(
        {
            "name": "rust_core",
            "ok": True,
            "required": False,
            "available": bool(core),
            "path": str(core) if core else None,
            "status": "available" if core else "python_fallback",
        }
    )
    checks.append({"name": "profile_dir", "ok": profile_dir(workspace).exists() or (team_dir / "profiles").exists()})
    details_log = logs_dir(workspace) / f"preflight-{int(time.time())}.json"
    details = {"team_dir": str(team_dir), "checks": checks}
    details_log.write_text(json.dumps(details, indent=2, ensure_ascii=False), encoding="utf-8")
    event_log.write("preflight.complete", ok=ok, details_log=str(details_log), checks=checks)
    blockers = [] if ok else _preflight_blockers(checks)
    return {
        "ok": ok,
        "summary": "preflight passed" if ok else "preflight found blockers: " + "; ".join(blockers[:3]),
        "next_actions": [f"team-agent start --team {team_dir} --yes --json"] if ok else _preflight_next_actions(blockers),
        "details_log": str(details_log),
        "checks": checks,
        "blockers": blockers,
    }


def start(team_dir: Path, yes: bool = False) -> dict[str, Any]:
    from team_agent.compiler import compile_team

    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    spec_path = team_dir / "team.spec.yaml"
    compiled = compile_team(team_dir, spec_path)
    if compiled["spec"].get("context", {}).get("state_file") == "team_state.md":
        state_file = str(team_dir.relative_to(workspace) / "team_state.md") if team_dir.is_relative_to(workspace) else "team_state.md"
        compiled["spec"]["context"]["state_file"] = state_file
        spec_path.write_text(dumps(compiled["spec"]), encoding="utf-8")
    launched = launch(spec_path, auto_approve=yes)
    details_log = logs_dir(workspace) / f"start-{int(time.time())}.json"
    details_log.write_text(json.dumps({"compile": compiled, "launch": launched}, indent=2, ensure_ascii=False), encoding="utf-8")
    return {
        "ok": bool(launched.get("ok")),
        "summary": f"compiled {team_dir} and launched {len(launched.get('agents', []))} agents",
        "next_actions": ["team-agent wait-ready --workspace . --timeout 120 --json"],
        "details_log": str(details_log),
        "spec": str(spec_path),
        "launch": launched,
    }


def quick_start(
    agents_dir: Path,
    name: str | None = None,
    yes: bool = False,
    fresh: bool = False,
    team_id: str | None = None,
) -> dict[str, Any]:
    team_dir = _prepare_quick_start_team(agents_dir.resolve(), Path.cwd().resolve(), name, team_id=team_id)
    workspace = team_workspace(team_dir)
    ensure_workspace_dirs(workspace)
    _ensure_profiles_for_roles(team_dir)
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


def _prepare_quick_start_team(agents_dir: Path, workspace: Path, name: str | None, team_id: str | None = None) -> Path:
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


def _preflight_blockers(checks: list[dict[str, Any]]) -> list[str]:
    blockers: list[str] = []
    for check in checks:
        if check.get("ok", True):
            continue
        name = check.get("name") or "check"
        if name == "compile":
            blockers.append(f"compile: {check.get('error')}")
            continue
        for item in check.get("checks", []) or []:
            agent = item.get("agent_id") or item.get("profile") or "-"
            reason = item.get("reason") or item.get("status") or "failed"
            detail = f"{name}: {agent} {reason}"
            if item.get("endpoint"):
                detail += f" endpoint={item['endpoint']}"
            if item.get("proxy_configured"):
                detail += f" proxy={item.get('proxy_url') or item.get('proxy_scheme')}"
            if item.get("proxy_source"):
                detail += f" proxy_source={item['proxy_source']}"
            if item.get("proxy_mode"):
                detail += f" proxy_mode={item['proxy_mode']}"
            if item.get("missing_required"):
                detail += " missing=" + ",".join(item["missing_required"])
            if item.get("effective_model"):
                detail += f" model={item['effective_model']}"
            if item.get("suggestion"):
                detail += f" suggestion={item['suggestion']}"
            blockers.append(detail)
        if not check.get("checks"):
            blockers.append(f"{name}: failed")
    return blockers or ["unknown preflight blocker"]


def _preflight_next_actions(blockers: list[str]) -> list[str]:
    actions = ["Fix failed checks, then rerun preflight."]
    if any("proxy_connectivity_failed" in item for item in blockers):
        actions.insert(0, "Allow the profile BASE_URL through the configured proxy, or disable the proxy for Team Agent startup.")
    if any("proxy_source=ambient" in item for item in blockers):
        actions.insert(0, "Current environment proxy is being used for this compatible_api worker; either fix that proxy for BASE_URL, set HTTPS_PROXY/HTTP_PROXY in the profile, or set PROXY_MODE=direct in the profile to bypass proxy for this worker.")
    if any("missing=" in item or "profile_required_values_missing" in item for item in blockers):
        actions.insert(
            0,
            "Ask the human user to fill the local profile file; agents must inspect only with `team-agent profile show <name> --workspace . --json` or the returned --team variant and must not read .team/*/profiles/*.env.",
        )
    if any("model_mismatch" in item or "does not match profile MODEL" in item for item in blockers):
        actions.insert(0, "Keep the model in the profile MODEL field or make the role model exactly match it.")
    return actions


def _ensure_profiles_for_roles(team_dir: Path) -> None:
    from team_agent.compiler import _read_front_matter
    from team_agent.profiles import ensure_profile_secret_boundary, ensure_profile_secret_boundary_dir, init_profile

    workspace = team_workspace(team_dir)
    profiles_dir = team_dir / "profiles"
    profiles_dir.mkdir(parents=True, exist_ok=True)
    ensure_profile_secret_boundary(workspace)
    ensure_profile_secret_boundary_dir(profiles_dir)
    for role_doc in sorted((team_dir / "agents").glob("*.md")):
        meta, _ = _read_front_matter(role_doc)
        profile = meta.get("profile")
        auth_mode = meta.get("auth_mode") or "subscription"
        if not profile:
            continue
        if not (profiles_dir / f"{profile}.env").exists() and not (profiles_dir / f"{profile}.example.env").exists():
            init_profile(workspace, str(profile), str(auth_mode))
            if auth_mode == "subscription":
                body = f"AUTH_MODE=subscription\nPROFILE_NAME={profile}\n"
            elif auth_mode == "official_api":
                body = f"AUTH_MODE=official_api\nPROFILE_NAME={profile}\nAPI_KEY=\nMODEL=\n"
            else:
                body = f"AUTH_MODE={auth_mode}\nPROFILE_NAME={profile}\nBASE_URL=\nAPI_KEY=\nMODEL=\n"
            (profiles_dir / f"{profile}.example.env").write_text(body, encoding="utf-8")


def wait_ready(workspace: Path, timeout: int = 120) -> dict[str, Any]:
    start_time = time.monotonic()
    last = {}
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


def sessions(workspace: Path) -> dict[str, Any]:
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


def repair_state(
    workspace: Path,
    task_id: str,
    assignee: str | None = None,
    status_value: str | None = None,
    summary: str | None = None,
) -> dict[str, Any]:
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


def shutil_which(command: str) -> str | None:
    from shutil import which

    return which(command)


@contextmanager
def _runtime_lock(workspace: Path, name: str, timeout: float = 5.0):
    lock_path = runtime_dir(workspace) / f"{name}.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    event_log = EventLog(workspace)
    start = time.monotonic()
    with lock_path.open("w", encoding="utf-8") as lock_file:
        while True:
            try:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                waited = time.monotonic() - start
                event_log.write("runtime.lock_acquired", lock=name, waited_sec=round(waited, 3))
                break
            except BlockingIOError:
                if time.monotonic() - start >= timeout:
                    event_log.write("runtime.lock_busy", lock=name, timeout_sec=timeout)
                    raise RuntimeError(
                        f"{name} is locked by another team-agent process; serialize team-agent {name} calls and retry"
                    )
                time.sleep(0.05)
        try:
            yield
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            event_log.write("runtime.lock_released", lock=name)


def _leader_id(state: dict[str, Any], spec: dict[str, Any]) -> str:
    return state.get("leader", {}).get("id") or spec.get("leader", {}).get("id") or "leader"


def _is_leader_sender(sender: str, leader_id: str) -> bool:
    return sender in {leader_id, "leader", "Leader"}


def _is_leader_target(target: str | None, leader_id: str) -> bool:
    return target in {leader_id, "leader", "Leader"}


def _spec_agent_ids(spec: dict[str, Any]) -> list[str]:
    return [str(agent["id"]) for agent in spec.get("agents", [])]


def _runtime_team_agent_ids(state: dict[str, Any], spec: dict[str, Any]) -> list[str]:
    runtime_agents = state.get("agents", {})
    return [agent_id for agent_id in _spec_agent_ids(spec) if agent_id in runtime_agents]


def _is_runtime_team_agent(agent_id: str, state: dict[str, Any], spec: dict[str, Any]) -> bool:
    return agent_id in set(_runtime_team_agent_ids(state, spec))


def _broadcast_targets(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import _broadcast_targets as impl

    return impl(*args, **kwargs)


def _compact_broadcast_delivery(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.send import _compact_broadcast_delivery as impl

    return impl(*args, **kwargs)


def allow_peer_talk(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import allow_peer_talk as impl

    return impl(*args, **kwargs)


def _mirror_peer_message_to_leader(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _mirror_peer_message_to_leader as impl

    return impl(*args, **kwargs)


def _leader_inbox_path(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _leader_inbox_path as impl

    return impl(*args, **kwargs)


def _send_to_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _send_to_leader_receiver as impl

    return impl(*args, **kwargs)


def _fail_leader_delivery(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _fail_leader_delivery as impl

    return impl(*args, **kwargs)


def _write_leader_fallback_audit(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _write_leader_fallback_audit as impl

    return impl(*args, **kwargs)


def _leader_receiver_is_direct(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _leader_receiver_is_direct as impl

    return impl(*args, **kwargs)


def _message_by_id(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _message_by_id as impl

    return impl(*args, **kwargs)


def _message_payload(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _message_payload as impl

    return impl(*args, **kwargs)


def _format_team_agent_message(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader import _format_team_agent_message as impl

    return impl(*args, **kwargs)


def _resolve_leader_pane(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _resolve_leader_pane as impl

    return impl(*args, **kwargs)


def _tmux_current_client_pane_info(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _tmux_current_client_pane_info as impl

    return impl(*args, **kwargs)


def _tmux_list_panes(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _tmux_list_panes as impl

    return impl(*args, **kwargs)


def _infer_active_tmux_pane(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _infer_active_tmux_pane as impl

    return impl(*args, **kwargs)


def _tmux_pane_info(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _tmux_pane_info as impl

    return impl(*args, **kwargs)


def _parse_tmux_pane_info(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _parse_tmux_pane_info as impl

    return impl(*args, **kwargs)


def _infer_workspace_tmux_pane(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _infer_workspace_tmux_pane as impl

    return impl(*args, **kwargs)


def _pane_is_usable_leader(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _pane_is_usable_leader as impl

    return impl(*args, **kwargs)


def _pane_path_matches_workspace(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _pane_path_matches_workspace as impl

    return impl(*args, **kwargs)


def _leader_pane_rank(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _leader_pane_rank as impl

    return impl(*args, **kwargs)


def _tmux_truthy(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _tmux_truthy as impl

    return impl(*args, **kwargs)


def _leader_command_is_exact(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _leader_command_is_exact as impl

    return impl(*args, **kwargs)


def _leader_command_provider(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _leader_command_provider as impl

    return impl(*args, **kwargs)


def _format_leader_pane_candidates(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _format_leader_pane_candidates as impl

    return impl(*args, **kwargs)


def _target_fingerprint(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _target_fingerprint as impl

    return impl(*args, **kwargs)


def _rediscover_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _rediscover_leader_receiver as impl

    return impl(*args, **kwargs)


def _validate_leader_receiver(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _validate_leader_receiver as impl

    return impl(*args, **kwargs)


def _leader_command_looks_usable(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _leader_command_looks_usable as impl

    return impl(*args, **kwargs)


def _choose_leader_submit_key(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.leader_panes import _choose_leader_submit_key as impl

    return impl(*args, **kwargs)


def _tmux_inject_text(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_inject_text as impl

    return impl(*args, **kwargs)


def _leader_submit_verification(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _leader_submit_verification as impl

    return impl(*args, **kwargs)


def _tmux_text_size(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_text_size as impl

    return impl(*args, **kwargs)


def _tmux_paste_ready_timeout(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_paste_ready_timeout as impl

    return impl(*args, **kwargs)


def _tmux_submit_settle_timeout(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_submit_settle_timeout as impl

    return impl(*args, **kwargs)


def _tmux_set_buffer_text(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_set_buffer_text as impl

    return impl(*args, **kwargs)


def _tmux_load_buffer_stdin(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _tmux_load_buffer_stdin as impl

    return impl(*args, **kwargs)


def _prepare_tmux_pane_for_input(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_io import _prepare_tmux_pane_for_input as impl

    return impl(*args, **kwargs)


def _enable_codex_fast_mode(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _enable_codex_fast_mode as impl

    return impl(*args, **kwargs)


def _wait_for_visible_token(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _wait_for_visible_token as impl

    return impl(*args, **kwargs)


def _capture_tmux_pane_text(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _capture_tmux_pane_text as impl

    return impl(*args, **kwargs)


def _wait_for_message_ready(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _wait_for_message_ready as impl

    return impl(*args, **kwargs)


def _wait_for_worker_message_ready(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _wait_for_worker_message_ready as impl

    return impl(*args, **kwargs)


def _capture_has_pasted_content_prompt(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _capture_has_pasted_content_prompt as impl

    return impl(*args, **kwargs)


def _capture_contains_message_fragment(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _capture_contains_message_fragment as impl

    return impl(*args, **kwargs)


def _message_fragment_candidates(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _message_fragment_candidates as impl

    return impl(*args, **kwargs)


def _message_content_lines(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _message_content_lines as impl

    return impl(*args, **kwargs)


def _is_strong_message_fragment(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _is_strong_message_fragment as impl

    return impl(*args, **kwargs)


def _compact_visible_text(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _compact_visible_text as impl

    return impl(*args, **kwargs)


def _submit_worker_prompt(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _submit_worker_prompt as impl

    return impl(*args, **kwargs)


def _wait_for_pasted_prompt_cleared(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.tmux_prompt import _wait_for_pasted_prompt_cleared as impl

    return impl(*args, **kwargs)


def _ghostty_command() -> str | None:
    return shutil_which("ghostty") or (
        "/Applications/Ghostty.app/Contents/MacOS/ghostty"
        if Path("/Applications/Ghostty.app/Contents/MacOS/ghostty").exists()
        else None
    )


def _ghostty_app_exists() -> bool:
    return Path("/Applications/Ghostty.app").exists()


def _ghostty_pids_by_title(title: str, wait_s: float = 0.0) -> list[int]:
    deadline = time.monotonic() + max(wait_s, 0.0)
    while True:
        pgrep = run_cmd(["pgrep", "-f", f"--title={title}"], timeout=5)
        if pgrep.returncode == 0:
            pids = [int(pid) for pid in pgrep.stdout.split() if pid.isdigit()]
            if pids:
                return pids
        if time.monotonic() >= deadline:
            return []
        time.sleep(0.2)


def _open_worker_displays(
    workspace: Path,
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    display_backend: str = "ghostty_window",
) -> dict[str, dict[str, Any]]:
    if not jobs:
        return {}
    if display_backend == "ghostty_workspace":
        return _open_ghostty_workspace(workspace, session_name, jobs, event_log)
    if len(jobs) == 1:
        agent_id, agent = jobs[0]
        return {agent_id: _open_ghostty_worker_window(workspace, session_name, agent_id, agent, event_log)}
    results: dict[str, dict[str, Any]] = {}
    max_workers = min(4, len(jobs))
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = {
            executor.submit(_open_ghostty_worker_window, workspace, session_name, agent_id, agent, event_log): agent_id
            for agent_id, agent in jobs
        }
        for future in as_completed(futures):
            agent_id = futures[future]
            try:
                results[agent_id] = future.result()
            except Exception as exc:
                display = {
                    "backend": "ghostty_window",
                    "status": "blocked",
                    "reason": "display_open_exception",
                    "error": str(exc),
                    "fallback": "tmux_headless",
                }
                event_log.write("display.ghostty_blocked", agent_id=agent_id, **display)
                results[agent_id] = display
    return results


def _open_ghostty_worker_window(
    workspace: Path,
    session_name: str,
    window_name: str,
    agent: dict[str, Any],
    event_log: EventLog,
) -> dict[str, Any]:
    if not _ghostty_app_exists():
        blocker = {
            "backend": "ghostty_window",
            "status": "blocked",
            "reason": "ghostty_app_missing",
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_blocked", agent_id=agent["id"], **blocker)
        return blocker
    title = f"team-agent:{agent['id']}:{agent.get('role', '')}"
    display_session = _ghostty_display_session_name(session_name, window_name)
    prepared = _prepare_ghostty_display_session(session_name, window_name, display_session)
    if not prepared["ok"]:
        blocker = {
            "backend": "ghostty_window",
            "status": "blocked",
            "reason": prepared["reason"],
            "error": prepared.get("error"),
            "target": f"{session_name}:{window_name}",
            "display_session": display_session,
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_blocked", agent_id=agent["id"], **blocker)
        return blocker
    launch_args = _ghostty_attach_args(display_session, title)
    proc = run_cmd(launch_args, timeout=10)
    display = {
        "backend": "ghostty_window",
        "status": "opened" if proc.returncode == 0 else "blocked",
        "title": title,
        "target": f"{session_name}:{window_name}",
        "display_session": display_session,
        "launch_args": launch_args,
        "pid": None,
        "pids": [],
        "tty": None,
        "fallback": "tmux_headless",
        "note": "Ghostty opens a dedicated linked tmux session per worker so each display has an independent active window; runtime injection remains tmux-backed.",
    }
    if proc.returncode != 0:
        display["reason"] = proc.stderr.strip() or proc.stdout.strip() or "open Ghostty.app failed"
    else:
        display["pids"] = _ghostty_pids_by_title(title, wait_s=3.0)
        display["pid"] = display["pids"][0] if display["pids"] else None
    event_log.write("display.ghostty_window", agent_id=agent["id"], **display)
    return display


def _open_ghostty_workspace(
    workspace: Path,
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
) -> dict[str, dict[str, Any]]:
    if not _ghostty_app_exists():
        return _ghostty_workspace_blocked(jobs, event_log, "ghostty_app_missing")
    aggregator_session = _ghostty_workspace_aggregator_name(session_name)
    linked_results = _prepare_ghostty_workspace_linked_sessions(session_name, jobs)
    displays: dict[str, dict[str, Any]] = {}
    linked_jobs: list[tuple[str, dict[str, Any], str]] = []
    for agent_id, agent in jobs:
        linked = linked_results.get(agent_id, {})
        linked_session = linked.get("linked_session") or _ghostty_display_session_name(session_name, agent_id)
        if linked.get("ok"):
            linked_jobs.append((agent_id, agent, linked_session))
            continue
        displays.update(
            _ghostty_workspace_blocked(
                [(agent_id, agent)],
                event_log,
                linked.get("reason", "display_session_create_failed"),
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session},
                error=linked.get("error"),
                target=f"{session_name}:{agent_id}",
            )
        )
    if not linked_jobs:
        return displays
    prepared = _prepare_ghostty_workspace_aggregator(aggregator_session, linked_jobs)
    if not prepared["ok"]:
        _kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        displays.update(
            _ghostty_workspace_blocked(
                [(agent_id, agent) for agent_id, agent, _linked_session in linked_jobs],
                event_log,
                prepared["reason"],
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session for agent_id, _agent, linked_session in linked_jobs},
                error=prepared.get("error"),
                target=prepared.get("target"),
            )
        )
        return displays
    title = f"team-agent:{session_name}:workspace"
    launch_args = _ghostty_attach_args(aggregator_session, title)
    proc = run_cmd(launch_args, timeout=10)
    if proc.returncode != 0:
        run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        _kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        displays.update(
            _ghostty_workspace_blocked(
                [(agent_id, agent) for agent_id, agent, _linked_session in linked_jobs],
                event_log,
                "open Ghostty.app failed",
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session for agent_id, _agent, linked_session in linked_jobs},
                error=proc.stderr.strip() or proc.stdout.strip(),
            )
        )
        return displays
    pids = _ghostty_pids_by_title(title, wait_s=3.0)
    panes = {pane["agent_id"]: pane for pane in prepared["panes"]}
    for agent_id, agent, linked_session in linked_jobs:
        pane = panes.get(agent_id, {})
        display = {
            "backend": "ghostty_workspace",
            "status": "opened",
            "title": title,
            "pane_title": pane.get("title") or _ghostty_workspace_pane_title(agent),
            "target": f"{session_name}:{agent_id}",
            "linked_session": linked_session,
            "aggregator_session": aggregator_session,
            "display_session": aggregator_session,
            "workspace_window": pane.get("window_name"),
            "pane_id": pane.get("pane_id"),
            "launch_args": launch_args,
            "pid": pids[0] if pids else None,
            "pids": pids,
            "tty": None,
            "fallback": "tmux_headless",
            "note": "Ghostty opens one aggregator tmux session; each pane attaches to a distinct linked session pinned to one base worker window, so runtime injection remains session:agent_id addressed.",
        }
        event_log.write("display.ghostty_workspace", agent_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def _ghostty_workspace_blocked(
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    reason: str,
    aggregator_session: str | None = None,
    linked_sessions: dict[str, str] | None = None,
    error: str | None = None,
    target: str | None = None,
) -> dict[str, dict[str, Any]]:
    displays: dict[str, dict[str, Any]] = {}
    for agent_id, _agent in jobs:
        linked_session = (linked_sessions or {}).get(agent_id)
        display = {
            "backend": "ghostty_workspace",
            "status": "blocked",
            "reason": reason,
            "error": error,
            "target": target or f"{agent_id}",
            "linked_session": linked_session,
            "aggregator_session": aggregator_session,
            "display_session": aggregator_session,
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_workspace_blocked", agent_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def _ghostty_display_session_name(session_name: str, window_name: str) -> str:
    raw = f"{session_name}:{window_name}"
    digest = hashlib.sha1(raw.encode("utf-8")).hexdigest()[:8]
    safe_session = re.sub(r"[^A-Za-z0-9_.-]", "_", session_name)[:80].strip("._-") or "team"
    safe_window = re.sub(r"[^A-Za-z0-9_.-]", "_", window_name)[:40].strip("._-") or "agent"
    return f"{safe_session}__display__{safe_window}__{digest}"


def _prepare_ghostty_display_session(session_name: str, window_name: str, display_session: str) -> dict[str, Any]:
    if not _tmux_window_exists(session_name, window_name):
        return {"ok": False, "reason": "tmux_target_missing"}
    if display_session == session_name:
        return {"ok": False, "reason": "display_session_conflicts_with_base_session"}
    if _tmux_session_exists(display_session):
        proc = run_cmd(["tmux", "kill-session", "-t", display_session], timeout=10)
        if proc.returncode != 0:
            return {"ok": False, "reason": "display_session_cleanup_failed", "error": proc.stderr.strip()}
    proc = run_cmd(["tmux", "new-session", "-d", "-t", session_name, "-s", display_session], timeout=10)
    if proc.returncode != 0:
        return {"ok": False, "reason": "display_session_create_failed", "error": proc.stderr.strip()}
    proc = run_cmd(["tmux", "select-window", "-t", f"{display_session}:{window_name}"], timeout=10)
    if proc.returncode != 0:
        run_cmd(["tmux", "kill-session", "-t", display_session], timeout=10)
        return {"ok": False, "reason": "display_session_select_window_failed", "error": proc.stderr.strip()}
    return {"ok": True, "display_session": display_session}


def _ghostty_workspace_aggregator_name(session_name: str) -> str:
    raw = f"{session_name}:workspace"
    digest = hashlib.sha1(raw.encode("utf-8")).hexdigest()[:8]
    safe_session = re.sub(r"[^A-Za-z0-9_.-]", "_", session_name)[:80].strip("._-") or "team"
    return f"{safe_session}__display__workspace__{digest}"


def _ghostty_workspace_window_name(index: int) -> str:
    return "overview" if index == 0 else f"overview-{index + 1}"


def _ghostty_workspace_pane_command(linked_session: str) -> str:
    return f"TMUX= tmux attach-session -t {shlex.quote(linked_session)}"


def _ghostty_workspace_pane_title(agent: dict[str, Any]) -> str:
    return f"team-agent:{agent['id']}:{agent.get('role', '')}"


def _prepare_ghostty_workspace_linked_sessions(
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
) -> dict[str, dict[str, Any]]:
    def prepare(agent_id: str) -> dict[str, Any]:
        linked_session = _ghostty_display_session_name(session_name, agent_id)
        result = _prepare_ghostty_display_session(session_name, agent_id, linked_session)
        result["linked_session"] = linked_session
        return result

    if len(jobs) == 1:
        agent_id, _agent = jobs[0]
        return {agent_id: prepare(agent_id)}
    results: dict[str, dict[str, Any]] = {}
    max_workers = min(4, len(jobs))
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = {executor.submit(prepare, agent_id): agent_id for agent_id, _agent in jobs}
        for future in as_completed(futures):
            agent_id = futures[future]
            try:
                results[agent_id] = future.result()
            except Exception as exc:
                results[agent_id] = {
                    "ok": False,
                    "reason": "display_session_create_exception",
                    "error": str(exc),
                    "linked_session": _ghostty_display_session_name(session_name, agent_id),
                }
    return results


def _prepare_ghostty_workspace_aggregator(
    aggregator_session: str,
    linked_jobs: list[tuple[str, dict[str, Any], str]],
) -> dict[str, Any]:
    if _tmux_session_exists(aggregator_session):
        proc = run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        if proc.returncode != 0:
            return {"ok": False, "reason": "display_session_cleanup_failed", "error": proc.stderr.strip()}

    def fail(reason: str, proc: Any | None = None, target: str | None = None) -> dict[str, Any]:
        run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        result = {"ok": False, "reason": reason}
        if proc is not None:
            result["error"] = proc.stderr.strip()
        if target:
            result["target"] = target
        return result

    panes: list[dict[str, Any]] = []
    for window_index, start in enumerate(range(0, len(linked_jobs), GHOSTTY_WORKSPACE_PANES_PER_WINDOW)):
        window_name = _ghostty_workspace_window_name(window_index)
        window_jobs = linked_jobs[start : start + GHOSTTY_WORKSPACE_PANES_PER_WINDOW]
        first_agent_id, first_agent, first_linked_session = window_jobs[0]
        if window_index == 0:
            proc = run_cmd(
                [
                    "tmux",
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-s",
                    aggregator_session,
                    "-n",
                    window_name,
                    _ghostty_workspace_pane_command(first_linked_session),
                ],
                timeout=10,
            )
            if proc.returncode != 0:
                return {"ok": False, "reason": "display_session_create_failed", "error": proc.stderr.strip()}
        else:
            proc = run_cmd(
                [
                    "tmux",
                    "new-window",
                    "-t",
                    aggregator_session,
                    "-n",
                    window_name,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    _ghostty_workspace_pane_command(first_linked_session),
                ],
                timeout=10,
            )
            if proc.returncode != 0:
                return fail("display_session_window_create_failed", proc, first_linked_session)
        first_pane_id = _tmux_stdout_last_line(proc.stdout) or f"{aggregator_session}:{window_name}.0"
        first_title = _ghostty_workspace_pane_title(first_agent)
        title_result = _set_ghostty_workspace_pane_title(first_pane_id, first_title)
        if not title_result["ok"]:
            return fail(title_result["reason"], target=first_pane_id)
        panes.append(
            {
                "agent_id": first_agent_id,
                "pane_id": first_pane_id,
                "title": first_title,
                "linked_session": first_linked_session,
                "window_name": window_name,
            }
        )

        proc = run_cmd(["tmux", "set-window-option", "-t", f"{aggregator_session}:{window_name}", "remain-on-exit", "on"], timeout=10)
        if proc.returncode != 0:
            return fail("display_session_remain_on_exit_failed", proc)

        for index, (agent_id, agent, linked_session) in enumerate(window_jobs[1:], start=1):
            proc = run_cmd(
                [
                    "tmux",
                    "split-window",
                    "-t",
                    f"{aggregator_session}:{window_name}",
                    "-h",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    _ghostty_workspace_pane_command(linked_session),
                ],
                timeout=10,
            )
            if proc.returncode != 0:
                return fail("display_session_split_failed", proc, linked_session)
            pane_id = _tmux_stdout_last_line(proc.stdout) or f"{aggregator_session}:{window_name}.{index}"
            title = _ghostty_workspace_pane_title(agent)
            title_result = _set_ghostty_workspace_pane_title(pane_id, title)
            if not title_result["ok"]:
                return fail(title_result["reason"], target=pane_id)
            panes.append(
                {
                    "agent_id": agent_id,
                    "pane_id": pane_id,
                    "title": title,
                    "linked_session": linked_session,
                    "window_name": window_name,
                }
            )

        proc = run_cmd(["tmux", "select-layout", "-t", f"{aggregator_session}:{window_name}", "even-horizontal"], timeout=10)
        if proc.returncode != 0:
            return fail("display_session_layout_failed", proc)

    proc = run_cmd(["tmux", "set-option", "-t", aggregator_session, "mouse", "on"], timeout=10)
    if proc.returncode != 0:
        return fail("display_session_mouse_failed", proc)
    run_cmd(["tmux", "select-window", "-t", f"{aggregator_session}:{_ghostty_workspace_window_name(0)}"], timeout=10)
    return {"ok": True, "aggregator_session": aggregator_session, "panes": panes}


def _set_ghostty_workspace_pane_title(pane_id: str, title: str) -> dict[str, Any]:
    proc = run_cmd(["tmux", "select-pane", "-t", pane_id, "-T", title], timeout=10)
    if proc.returncode != 0:
        return {"ok": False, "reason": "display_session_pane_title_failed", "error": proc.stderr.strip()}
    return {"ok": True}


def _tmux_stdout_last_line(stdout: str) -> str | None:
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    return lines[-1] if lines else None


def _open_ghostty_workspace_agent_display(
    session_name: str,
    agent_id: str,
    agent: dict[str, Any],
    previous_display: dict[str, Any],
    event_log: EventLog,
) -> dict[str, Any]:
    if not _ghostty_app_exists():
        return _ghostty_workspace_blocked(
            [(agent_id, agent)],
            event_log,
            "ghostty_app_missing",
            aggregator_session=_ghostty_workspace_aggregator_name(session_name),
            linked_sessions={agent_id: _ghostty_display_session_name(session_name, agent_id)},
            target=f"{session_name}:{agent_id}",
        )[agent_id]
    aggregator_session = str(
        previous_display.get("aggregator_session")
        or previous_display.get("display_session")
        or _ghostty_workspace_aggregator_name(session_name)
    )
    linked_session = _ghostty_display_session_name(session_name, agent_id)
    prepared = _prepare_ghostty_display_session(session_name, agent_id, linked_session)
    if not prepared["ok"]:
        return _ghostty_workspace_blocked(
            [(agent_id, agent)],
            event_log,
            prepared["reason"],
            aggregator_session=aggregator_session,
            linked_sessions={agent_id: linked_session},
            error=prepared.get("error"),
            target=f"{session_name}:{agent_id}",
        )[agent_id]
    if not _tmux_session_exists(aggregator_session):
        return _ghostty_workspace_partial_update_display(
            session_name,
            agent_id,
            agent,
            event_log,
            reason="aggregator_session_missing",
            note="pane refresh requires full team restart",
        )

    pane_title = _ghostty_workspace_pane_title(agent)
    command = _ghostty_workspace_pane_command(linked_session)
    pane_id = str(previous_display.get("pane_id") or "")
    workspace_window = str(previous_display.get("workspace_window") or _ghostty_workspace_window_name(0))
    refreshed = False
    if pane_id:
        proc = run_cmd(["tmux", "respawn-pane", "-k", "-t", pane_id, command], timeout=10)
        refreshed = proc.returncode == 0
    if not refreshed:
        proc = run_cmd(
            [
                "tmux",
                "split-window",
                "-t",
                f"{aggregator_session}:{workspace_window}",
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
                command,
            ],
            timeout=10,
        )
        if proc.returncode != 0:
            return _ghostty_workspace_partial_update_display(
                session_name,
                agent_id,
                agent,
                event_log,
                reason="aggregator_pane_refresh_failed",
                note=proc.stderr.strip() or "pane refresh requires full team restart",
            )
        pane_id = _tmux_stdout_last_line(proc.stdout) or pane_id
    title_result = _set_ghostty_workspace_pane_title(pane_id, pane_title)
    if not title_result["ok"]:
        return _ghostty_workspace_partial_update_display(
            session_name,
            agent_id,
            agent,
            event_log,
            reason=title_result["reason"],
            note=title_result.get("error") or "pane refresh requires full team restart",
        )
    run_cmd(["tmux", "select-layout", "-t", f"{aggregator_session}:{workspace_window}", "even-horizontal"], timeout=10)
    title = str(previous_display.get("title") or f"team-agent:{session_name}:workspace")
    pids = [int(pid) for pid in previous_display.get("pids", []) if str(pid).isdigit()]
    display = {
        "backend": "ghostty_workspace",
        "status": "opened",
        "title": title,
        "pane_title": pane_title,
        "target": f"{session_name}:{agent_id}",
        "linked_session": linked_session,
        "aggregator_session": aggregator_session,
        "display_session": aggregator_session,
        "workspace_window": workspace_window,
        "pane_id": pane_id,
        "pid": pids[0] if pids else None,
        "pids": pids,
        "tty": None,
        "fallback": "tmux_headless",
        "note": "Refreshed this worker's Ghostty workspace pane by respawning it against a distinct linked session.",
    }
    event_log.write("display.ghostty_workspace", agent_id=agent_id, **display)
    return display


def _ghostty_workspace_partial_update_display(
    session_name: str,
    agent_id: str,
    agent: dict[str, Any],
    event_log: EventLog,
    reason: str = "partial_update_requires_team_restart",
    note: str = "pane refresh requires full team restart",
) -> dict[str, Any]:
    aggregator_session = _ghostty_workspace_aggregator_name(session_name)
    display = {
        "backend": "ghostty_workspace",
        "status": "blocked",
        "reason": reason,
        "target": f"{session_name}:{agent_id}",
        "linked_session": _ghostty_display_session_name(session_name, agent_id),
        "aggregator_session": aggregator_session,
        "display_session": aggregator_session,
        "pane_title": _ghostty_workspace_pane_title(agent),
        "fallback": "tmux_headless",
        "note": note,
        "action": "restart the team to rebuild the Ghostty workspace layout",
    }
    event_log.write("display.ghostty_workspace_partial_update", agent_id=agent_id, **display)
    return display


def _kill_ghostty_workspace_linked_sessions(linked_sessions: list[str]) -> list[str]:
    killed: list[str] = []
    for linked_session in dict.fromkeys(linked_sessions):
        if _tmux_session_exists(linked_session):
            proc = run_cmd(["tmux", "kill-session", "-t", linked_session], timeout=10)
            if proc.returncode == 0:
                killed.append(linked_session)
    return killed


def _ghostty_attach_args(display_session: str, title: str) -> list[str]:
    return [
        "open",
        "-na",
        "Ghostty.app",
        "--args",
        f"--title={title}",
        "-e",
        "tmux",
        "attach-session",
        "-t",
        display_session,
    ]


def _close_ghostty_display(
    agent_id: str,
    agent_state: dict[str, Any],
    event_log: EventLog,
) -> None:
    display = agent_state.get("display") or {}
    if display.get("backend") != "ghostty_window":
        return
    display_session = display.get("display_session")
    pids = [str(pid) for pid in display.get("pids", []) if str(pid).isdigit()]
    title = display.get("title")
    if not pids and title:
        pids = [str(pid) for pid in _ghostty_pids_by_title(str(title))]
    killed: list[str] = []
    for pid in pids:
        proc = run_cmd(["kill", pid], timeout=5)
        if proc.returncode == 0:
            killed.append(pid)
    if killed:
        event_log.write("display.ghostty_closed", agent_id=agent_id, pids=killed, title=title)
    if display_session and _tmux_session_exists(str(display_session)):
        proc = run_cmd(["tmux", "kill-session", "-t", str(display_session)], timeout=10)
        if proc.returncode == 0:
            event_log.write("display.ghostty_display_session_closed", agent_id=agent_id, display_session=display_session)
        else:
            event_log.write(
                "display.ghostty_display_session_close_failed",
                agent_id=agent_id,
                display_session=display_session,
                error=proc.stderr.strip(),
            )


def _close_ghostty_workspace(state: dict[str, Any], event_log: EventLog) -> None:
    displays = [
        (agent_id, agent_state.get("display") or {})
        for agent_id, agent_state in state.get("agents", {}).items()
        if (agent_state.get("display") or {}).get("backend") == "ghostty_workspace"
    ]
    if not displays:
        return
    aggregator_session = next(
        (
            str(display.get("aggregator_session") or display.get("display_session"))
            for _agent_id, display in displays
            if display.get("aggregator_session") or display.get("display_session")
        ),
        None,
    )
    title = next((str(display.get("title")) for _agent_id, display in displays if display.get("title")), None)
    pids = {
        str(pid)
        for _agent_id, display in displays
        for pid in display.get("pids", [])
        if str(pid).isdigit()
    }
    if not pids and title:
        pids = {str(pid) for pid in _ghostty_pids_by_title(str(title))}

    aggregator_closed = False
    if aggregator_session and _tmux_session_exists(aggregator_session):
        proc = run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        if proc.returncode == 0:
            aggregator_closed = True
        else:
            event_log.write(
                "display.ghostty_workspace_close_failed",
                aggregator_session=aggregator_session,
                error=proc.stderr.strip(),
            )

    linked_sessions = [
        str(display.get("linked_session"))
        for _agent_id, display in displays
        if display.get("linked_session")
    ]
    linked_closed = _kill_ghostty_workspace_linked_sessions(linked_sessions)

    killed: list[str] = []
    for pid in sorted(pids):
        proc = run_cmd(["kill", pid], timeout=5)
        if proc.returncode == 0:
            killed.append(pid)
    event_log.write(
        "display.ghostty_workspace_closed",
        pids=killed,
        title=title,
        aggregator_session=aggregator_session,
        linked_sessions=linked_closed,
        aggregator_closed=aggregator_closed,
    )


def get_adapter_or_raise(name: str) -> str:
    if name == "tmux" and not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ before launch")
    return name


def _deliver_pending_message(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.delivery import _deliver_pending_message as impl

    return impl(*args, **kwargs)


def _deliver_pending_messages(*args: Any, **kwargs: Any) -> Any:
    from team_agent.messaging.delivery import _deliver_pending_messages as impl

    return impl(*args, **kwargs)


def _refresh_agent_runtime_statuses(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
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
            detected = _detect_provider_status(agent_state["provider"], session_name, window)
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


def _sync_agent_health(workspace: Path, state: dict[str, Any], store: MessageStore | None = None) -> None:
    store = store or MessageStore(workspace)
    session_name = state.get("session_name")
    for agent_id, agent_state in state.get("agents", {}).items():
        health_status = _agent_health_status(agent_state)
        last_output_at = agent_state.get("last_output_at")
        window = agent_state.get("window", agent_id)
        if session_name and _tmux_window_exists(session_name, window):
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-40", "-t", f"{session_name}:{window}"], timeout=5)
            if proc.returncode == 0:
                digest = hashlib.sha256(proc.stdout.encode("utf-8", errors="ignore")).hexdigest()
                if digest != agent_state.get("last_output_hash"):
                    last_output_at = datetime.now(timezone.utc).isoformat()
                    agent_state["last_output_hash"] = digest
                    agent_state["last_output_at"] = last_output_at
                if _capture_has_approval_prompt(proc.stdout):
                    health_status = "AWAITING_APPROVAL"
        current_task = _current_task_for_agent(state.get("tasks", []), agent_id)
        store.upsert_agent_health(
            agent_id,
            health_status,
            last_output_at=last_output_at,
            context_usage_pct=agent_state.get("context_usage_pct"),
            current_task_id=current_task,
        )


def _agent_health_status(agent_state: dict[str, Any]) -> str:
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


def _current_task_for_agent(tasks: list[dict[str, Any]], agent_id: str) -> str | None:
    active = {"pending", "ready", "running", "blocked", "needs_retry"}
    for task in reversed(tasks):
        if task.get("assignee") == agent_id and task.get("status", "pending") in active:
            return task.get("id")
    return None


def _capture_has_approval_prompt(text: str) -> bool:
    return _extract_approval_prompt("_", text) is not None


def _extract_approval_prompt(agent_id: str, text: str) -> dict[str, Any] | None:
    lines = text.splitlines()
    control_index = _active_approval_control_index(lines)
    if control_index is None:
        return None
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Allow the team_orchestrator MCP server to run tool" not in line:
            continue
        tool_match = re.search(r'run tool "([^"]+)"', line)
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1) if tool_match else None,
            "prompt": line.strip(),
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if _line_is_approval_choice(line):
            continue
        tool_match = re.search(r"\bteam_orchestrator\s*[-.]\s*([A-Za-z_][A-Za-z0-9_]*)\b", line)
        if not tool_match:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1),
            "prompt": f"team_orchestrator - {tool_match.group(1)}",
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Would you like to run the following command" not in line:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "command",
            "command": _extract_command_approval_subject(lines[: control_index + 1], index),
            "prompt": line.strip(),
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    return {
        "agent_id": agent_id,
        "state": "waiting_approval",
        "kind": "unknown",
        "prompt": "approval prompt detected",
        "choices": _extract_approval_choices(lines[: control_index + 1]),
    }


def _active_approval_control_index(lines: list[str]) -> int | None:
    control_indices = [
        index
        for index, line in enumerate(lines)
        if _is_approval_control_line(line)
    ]
    if not control_indices:
        return None
    control_index = control_indices[-1]
    if any(line.strip() for line in lines[control_index + 1 :]):
        return None
    return control_index


def _is_approval_control_line(line: str) -> bool:
    normalized = line.lower()
    return "enter to submit | esc to cancel" in normalized or ("esc to cancel" in normalized and "tab to amend" in normalized)


def _extract_approval_choices(lines: list[str]) -> list[str]:
    choices: list[str] = []
    for line in lines:
        stripped = line.strip()
        match = _APPROVAL_CHOICE_RE.match(stripped)
        if not match:
            continue
        label = match.group(2).strip()
        if label and label not in choices:
            choices.append(label)
    return choices


_APPROVAL_CHOICE_RE = re.compile(r"(?:[›❯>]\s*)?(\d+)\.\s+(.+?)(?:\s{2,}.+)?$")


def _line_is_approval_choice(line: str) -> bool:
    return _APPROVAL_CHOICE_RE.match(line.strip()) is not None


def _extract_command_approval_subject(lines: list[str], prompt_index: int) -> str | None:
    for line in reversed(lines[:prompt_index]):
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    for line in lines[prompt_index + 1 : prompt_index + 8]:
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    return None


def _age_text(iso_text: str | None) -> str:
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


def _detect_provider_status(provider: str, session_name: str, window: str) -> str | None:
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


def _handle_provider_runtime_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    session_name = state.get("session_name")
    if not session_name or not _tmux_session_exists(session_name):
        return
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped", "missing"}:
            continue
        window = agent_state.get("window", agent_id)
        if not _tmux_window_exists(session_name, window):
            continue
        internal_mcp = _handle_internal_mcp_approval_prompt(agent_id, session_name, window, event_log)
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


def _handle_provider_startup_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
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
        for prompt_event in adapter.handle_startup_prompts(session_name, window, checks=1, sleep_s=0.0):
            event_log.write(
                "runtime.startup_prompt_handled",
                agent_id=agent_id,
                provider=agent_state["provider"],
                **prompt_event,
            )


def _handle_internal_mcp_approval_prompt(
    agent_id: str,
    session_name: str,
    window: str,
    event_log: EventLog,
) -> dict[str, Any] | None:
    target = f"{session_name}:{window}"
    proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", target], timeout=5)
    if proc.returncode != 0:
        return None
    prompt = _extract_approval_prompt(agent_id, proc.stdout)
    if not prompt or prompt.get("kind") != "mcp_tool":
        return None
    tool = str(prompt.get("tool") or "")
    fingerprint = _approval_prompt_fingerprint(prompt)
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
    result = _submit_internal_mcp_approval(agent_id, target, tool, prompt, proc.stdout)
    event_log.write("runtime.internal_mcp_approval.auto", agent_id=agent_id, **result)
    return result


def _submit_internal_mcp_approval(
    agent_id: str,
    target: str,
    tool: str,
    prompt: dict[str, Any],
    capture_text: str,
    attempts: int = 3,
) -> dict[str, Any]:
    choice = _choose_internal_mcp_approval_choice(prompt)
    fingerprint = _approval_prompt_fingerprint(prompt)
    attempt_log: list[dict[str, Any]] = []
    current_prompt = prompt
    current_capture = capture_text
    for attempt in range(1, attempts + 1):
        keys = _approval_choice_keys(current_prompt, current_capture, choice)
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
        after_prompt = _extract_approval_prompt(agent_id, verify.stdout)
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


def _choose_internal_mcp_approval_choice(prompt: dict[str, Any]) -> str:
    choices = prompt.get("choices") or []
    if INTERNAL_MCP_APPROVAL_CHOICE in choices:
        return INTERNAL_MCP_APPROVAL_CHOICE
    for choice in choices:
        if str(choice).startswith("Yes, and don't ask again"):
            return str(choice)
    if "Allow" in choices:
        return "Allow"
    if "Yes" in choices:
        return "Yes"
    return INTERNAL_MCP_APPROVAL_CHOICE


def _approval_choice_keys(prompt: dict[str, Any], capture_text: str, choice: str) -> list[str]:
    choices = prompt.get("choices") or []
    try:
        target_index = choices.index(choice)
    except ValueError:
        return ["Down", "Enter"]
    active_index = _active_approval_choice_index(capture_text)
    if active_index is None:
        return [str(target_index + 1), "Enter"]
    delta = target_index - active_index
    if delta > 0:
        return ["Down"] * delta + ["Enter"]
    if delta < 0:
        return ["Up"] * abs(delta) + ["Enter"]
    return ["Enter"]


def _active_approval_choice_index(text: str) -> int | None:
    for line in text.splitlines():
        stripped = line.strip()
        if not (stripped.startswith("›") or stripped.startswith("❯") or stripped.startswith(">")):
            continue
        match = re.match(r"[›❯>]\s*(\d+)\.", stripped)
        if match:
            return int(match.group(1)) - 1
    return None


def _capture_has_team_orchestrator_mcp_prompt(text: str) -> bool:
    return (
        "Allow the team_orchestrator MCP server to run tool" in text
        or re.search(r"\bteam_orchestrator\s*[-.]\s*[A-Za-z_][A-Za-z0-9_]*\b", text) is not None
    )


def _approval_prompt_fingerprint(prompt: dict[str, Any]) -> str:
    data = {
        "kind": prompt.get("kind"),
        "tool": prompt.get("tool"),
        "prompt": prompt.get("prompt"),
        "choices": prompt.get("choices") or [],
    }
    return hashlib.sha256(json.dumps(data, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()[:16]


def _tmux_session_exists(session_name: str | None) -> bool:
    if not session_name:
        return False
    proc = run_cmd(["tmux", "has-session", "-t", session_name], timeout=5)
    return proc.returncode == 0


def _tmux_start_command_for_agent_window(session_name: str, window_name: str, command: str) -> tuple[list[str], str]:
    if _tmux_session_exists(session_name):
        return ["tmux", "new-window", "-t", session_name, "-n", window_name, "sh", "-lc", command], "new-window"
    return ["tmux", "new-session", "-d", "-s", session_name, "-n", window_name, "sh", "-lc", command], "new-session"


def _tmux_window_exists(session_name: str | None, window: str | None) -> bool:
    if not session_name or not window:
        return False
    proc = run_cmd(["tmux", "list-windows", "-t", session_name, "-F", "#{window_name}"], timeout=5)
    if proc.returncode != 0:
        return False
    return window in proc.stdout.splitlines()


def _find_task(tasks: list[dict[str, Any]], task_id: str) -> dict[str, Any]:
    for task in tasks:
        if task.get("id") == task_id:
            return task
    raise RuntimeError(f"unknown task id: {task_id}")


def _find_task_or_none(tasks: list[dict[str, Any]], task_id: str) -> dict[str, Any] | None:
    for task in tasks:
        if task.get("id") == task_id:
            return task
    return None


def _is_message_scoped_result(store: MessageStore, envelope: dict[str, Any]) -> bool:
    task_id = str(envelope.get("task_id") or "")
    agent_id = str(envelope.get("agent_id") or "")
    if not task_id.startswith("msg_"):
        return False
    message = _message_by_id(store, task_id)
    return bool(message and message.get("recipient") == agent_id)


def _find_agent(spec: dict[str, Any], agent_id: str | None) -> dict[str, Any] | None:
    if not agent_id:
        return None
    for agent in spec.get("agents", []):
        if agent.get("id") == agent_id:
            return agent
    if spec.get("leader", {}).get("id") == agent_id:
        return spec["leader"]
    return None


def _result_status_to_task_status(task: dict[str, Any], result_status: str) -> str:
    if result_status == "success":
        return "done"
    if result_status == "blocked":
        return "blocked"
    if result_status in {"partial", "failed"}:
        return _retry_or_failed(task)
    raise KeyError(result_status)


def _retry_or_failed(task: dict[str, Any]) -> str:
    retry_count = int(task.get("retry_count") or 0)
    retry_limit = int(task.get("retry_limit") or 0)
    if retry_count < retry_limit:
        task["retry_count"] = retry_count + 1
        return "needs_retry"
    task["retry_count"] = retry_count
    return "failed"
