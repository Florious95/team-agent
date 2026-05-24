from __future__ import annotations

import hashlib
import os
import re
import shlex
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.state import load_runtime_state, save_runtime_state


def attach_leader(workspace: Path, pane: str | None = None, provider: str = "codex") -> dict[str, Any]:
    from team_agent.runtime import _attach_leader_to_state, ensure_workspace_dirs
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
    from team_agent.runtime import (
        RuntimeError,
        _tmux_session_exists,
        ensure_workspace_dirs,
        get_adapter,
        shutil_which,
    )
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
    session_name = leader_session_name(provider, workspace)
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


def leader_session_name(provider: str, workspace: Path) -> str:
    digest = hashlib.sha1(str(workspace.resolve()).encode("utf-8")).hexdigest()[:8]
    folder = re.sub(r"[^A-Za-z0-9_.-]", "_", workspace.name)[:48].strip("._-") or "workspace"
    return f"team-agent-leader-{provider}-{folder}-{digest}"


def attach_leader_to_state(
    workspace: Path,
    state: dict[str, Any],
    pane: str | None,
    provider: str,
    event_log: EventLog,
    source: str,
    require_current: bool = False,
) -> tuple[dict[str, Any], dict[str, Any]]:
    from team_agent.runtime import (
        RuntimeError,
        _leader_command_provider,
        _resolve_leader_pane,
        _target_fingerprint,
        _validate_leader_receiver,
        get_adapter,
    )
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


__all__ = [
    "attach_leader",
    "attach_leader_to_state",
    "leader_session_name",
    "leader_start_plan",
    "start_leader",
]
