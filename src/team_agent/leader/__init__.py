from __future__ import annotations

import hashlib
import os
import re
import signal
import shlex
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.state import load_runtime_state, save_runtime_state


def attach_leader(workspace: Path, pane: str | None = None, provider: str = "codex") -> dict[str, Any]:
    from team_agent.message_store import MessageStore
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
    requeued = MessageStore(workspace).requeue_delivery_exhausted_watchers()
    if requeued:
        event_log.write(
            "leader_receiver.requeued_exhausted_watchers",
            watcher_ids=requeued,
            count=len(requeued),
            trigger="attach_leader",
        )
    return {
        "ok": True,
        "leader_receiver": receiver,
        "validation": validation,
        "requeued_exhausted_watchers": requeued,
    }


def start_leader(
    provider: str,
    provider_args: list[str],
    workspace: Path,
    *,
    attach_existing: bool = False,
    confirm_attach: bool = False,
    attach_session: str | None = None,
) -> None:
    plan = leader_start_plan(
        provider,
        provider_args,
        workspace,
        attach_existing=attach_existing,
        confirm_attach=confirm_attach,
        attach_session=attach_session,
    )
    if plan["mode"] == "new_tmux_session" and not sys.stdin.isatty():
        plan = dict(plan)
        argv = list(plan["argv"])
        argv.insert(2, "-d")
        plan["argv"] = argv
        plan["detached"] = True
    EventLog(workspace).write(
        "leader.start",
        provider=provider,
        workspace=str(workspace),
        mode=plan["mode"],
        session_name=plan.get("session_name"),
        argv=plan["argv"],
    )
    _run_leader_plan(plan, workspace)


def leader_start_plan(
    provider: str,
    provider_args: list[str],
    workspace: Path,
    *,
    attach_existing: bool = False,
    confirm_attach: bool = False,
    attach_session: str | None = None,
) -> dict[str, Any]:
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
    if attach_session:
        if not confirm_attach:
            raise RuntimeError("--attach-session requires --confirm")
        return {
            "mode": "attach_existing",
            "provider": provider,
            "workspace": str(workspace),
            "session_name": attach_session,
            "argv": ["tmux", "attach-session", "-t", attach_session],
        }
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
    exports = ""
    if os.environ.get("PATH"):
        exports = f"PATH={shlex.quote(os.environ['PATH'])} "
    shell = f"cd {shlex.quote(str(workspace))} && {exports}exec {shlex.join(argv)}"
    tmux_args = ["tmux", "new-session", "-s", session_name, "-n", provider, "-c", str(workspace)]
    return {
        "mode": "new_tmux_session",
        "provider": provider,
        "workspace": str(workspace),
        "session_name": session_name,
        "argv": [*tmux_args, "sh", "-lc", shell],
        "detached": False,
    }


def _run_leader_plan(plan: dict[str, Any], workspace: Path) -> None:
    session_name = plan.get("session_name")
    proc: subprocess.Popen[Any] | None = None
    sigints = 0

    def stop_process_tree() -> None:
        if session_name and plan["mode"] == "new_tmux_session":
            subprocess.run(["tmux", "kill-session", "-t", str(session_name)], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if proc and proc.poll() is None:
            proc.terminate()

    def handle_sigint(signum: int, _frame: Any) -> None:
        nonlocal sigints
        sigints += 1
        if proc and proc.poll() is None:
            try:
                proc.send_signal(signum)
            except ProcessLookupError:
                pass
        if sigints >= 2:
            stop_process_tree()

    old_sigint = signal.signal(signal.SIGINT, handle_sigint)
    try:
        if plan["mode"] == "exec_provider":
            os.chdir(workspace)
        proc = subprocess.Popen(plan["argv"])
        if plan.get("detached") and session_name:
            proc.wait()
            while _tmux_session_exists_local(str(session_name)):
                time.sleep(0.2)
        else:
            proc.wait()
    finally:
        signal.signal(signal.SIGINT, old_sigint)
        _print_team_running_reminder(workspace)
    raise SystemExit(proc.returncode if proc else 1)


def _tmux_session_exists_local(session_name: str) -> bool:
    proc = subprocess.run(["tmux", "has-session", "-t", session_name], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return proc.returncode == 0


def _print_team_running_reminder(workspace: Path) -> None:
    state = load_runtime_state(workspace)
    team_name = state.get("session_name")
    if not team_name or not _tmux_session_exists_local(str(team_name)):
        return
    print(f"team {team_name} is still running; run team-agent shutdown to close it OR team-agent attach-leader to reconnect.")


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


def autobind_leader_receiver_from_env(
    workspace: Path,
    provider: str,
    source: str,
) -> dict[str, Any] | None:
    tmux_pane = os.environ.get("TMUX_PANE")
    if not tmux_pane:
        return None
    from team_agent.runtime import ensure_workspace_dirs
    ensure_workspace_dirs(workspace)
    state = load_runtime_state(workspace)
    event_log = EventLog(workspace)
    try:
        receiver, _validation = attach_leader_to_state(
            workspace,
            state,
            pane=tmux_pane,
            provider=provider,
            event_log=event_log,
            source=source,
        )
    except Exception as exc:
        event_log.write(
            "leader_receiver.autobind_skipped",
            pane=tmux_pane,
            provider=provider,
            source=source,
            error=str(exc),
        )
        return None
    save_runtime_state(workspace, state)
    return receiver


__all__ = [
    "attach_leader",
    "attach_leader_to_state",
    "autobind_leader_receiver_from_env",
    "leader_session_name",
    "leader_start_plan",
    "start_leader",
]
