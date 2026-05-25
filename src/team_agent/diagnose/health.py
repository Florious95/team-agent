from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.diagnose.checks import (
    compact_model_checks,
    model_checks_for_agents,
    profile_checks_for_agents,
)
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.paths import logs_dir, runtime_dir
from team_agent.profiles import compact_profile_check
from team_agent.spec import load_spec, workspace_from_spec
from team_agent.state import load_runtime_state


def diagnose(workspace: Path) -> dict[str, Any]:
    from team_agent.runtime import (
        _capture_has_team_orchestrator_mcp_prompt,
        _leader_receiver_is_direct,
        _tmux_session_exists,
        _tmux_window_exists,
        _validate_leader_receiver,
        get_adapter,
        run_cmd,
        status,
    )
    _ = EventLog  # imported for symmetry / future use
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
    from team_agent.runtime import _attach_team_profile_dirs, coordinator_health, get_adapter, shutil_which
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
    model_checks = model_checks_for_agents(spec.get("agents", []), workspace) if spec else []
    if spec:
        checks["models"] = compact_model_checks(model_checks)
        profile_checks = profile_checks_for_agents(workspace, spec.get("agents", []))
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
        checks["invalid_models"] = compact_model_checks(invalid_models)
    if invalid_profiles:
        checks["invalid_profiles"] = invalid_profiles
    return checks
