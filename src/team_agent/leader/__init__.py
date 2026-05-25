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
from team_agent.state import derive_leader_session_uuid, load_runtime_state, save_runtime_state, save_team_scoped_state, select_runtime_state, team_state_key


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
        for watcher_id in requeued:
            event_log.write(
                "result_watcher.requeued",
                watcher_id=watcher_id,
                trigger="attach_leader",
                new_pane_id=receiver.get("pane_id"),
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
    plan = leader_start_plan(provider, provider_args, workspace, attach_existing=attach_existing, confirm_attach=confirm_attach, attach_session=attach_session)
    if plan.get("leader_session_uuid_source") == "override":
        EventLog(workspace).write("leader_session_uuid.override", source="explicit-override", uuid_prefix=str(plan.get("leader_session_uuid") or "")[:12], team_id=plan.get("team_id"))
    if plan["mode"] == "new_tmux_session" and not sys.stdin.isatty():
        plan = dict(plan)
        argv = list(plan["argv"])
        argv.insert(2, "-d")
        plan["argv"] = argv
        plan["detached"] = True
    EventLog(workspace).write("leader.start", provider=provider, workspace=str(workspace), mode=plan["mode"], session_name=plan.get("session_name"), argv=_leader_plan_log_argv(plan), leader_session_uuid_source=plan.get("leader_session_uuid_source"), uuid_prefix=str(plan.get("leader_session_uuid") or "")[:12] or None)
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
    identity = _leader_identity_context(workspace)
    leader_env = _leader_provider_env(provider, identity)
    if attach_session:
        if not confirm_attach:
            raise RuntimeError("--attach-session requires --confirm")
        return {"mode": "attach_existing", "provider": provider, "workspace": str(workspace), "session_name": attach_session, "argv": ["tmux", "attach-session", "-t", attach_session]}
    if os.environ.get("TMUX"):
        return {"mode": "exec_provider", "provider": provider, "workspace": str(workspace), "argv": argv, "env": {**os.environ, **leader_env}, **identity}
    if not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ or start the leader from an existing tmux pane")
    session_name = leader_session_name(provider, workspace)
    if _tmux_session_exists(session_name):
        return {"mode": "attach_existing", "provider": provider, "workspace": str(workspace), "session_name": session_name, "argv": ["tmux", "attach-session", "-t", session_name]}
    exports = " ".join(f"{key}={shlex.quote(value)}" for key, value in leader_env.items())
    if os.environ.get("PATH"):
        exports = f"{exports} PATH={shlex.quote(os.environ['PATH'])}"
    shell = f"cd {shlex.quote(str(workspace))} && export {exports} && exec {shlex.join(argv)}"
    tmux_args = ["tmux", "new-session", "-s", session_name, "-n", provider, "-c", str(workspace)]
    return {
        "mode": "new_tmux_session",
        "provider": provider,
        "workspace": str(workspace),
        "session_name": session_name,
        "argv": [*tmux_args, "sh", "-lc", shell],
        "leader_env": leader_env,
        **identity,
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
        proc = subprocess.Popen(plan["argv"], env=plan.get("env"))
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


def _leader_identity_context(workspace: Path, team: str | None = None, state: dict[str, Any] | None = None) -> dict[str, Any]:
    state = state or _load_identity_state(workspace, team)
    team_id = team_state_key(state)
    machine = _identity_machine_fingerprint(state)
    user = _identity_os_user()
    override = os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") or ""
    leader_uuid = override or _state_leader_session_uuid(state) or derive_leader_session_uuid(
        machine,
        str(workspace.resolve()),
        user,
        team_id,
    )
    return {
        "leader_session_uuid": leader_uuid,
        "leader_session_uuid_source": "override" if override else "derived",
        "machine_fingerprint": machine,
        "workspace_abspath": str(workspace.resolve()),
        "os_user": user,
        "team_id": team_id,
    }


def _load_identity_state(workspace: Path, team: str | None) -> dict[str, Any]:
    try:
        return select_runtime_state(workspace, team)
    except Exception:
        return load_runtime_state(workspace)


def _identity_machine_fingerprint(state: dict[str, Any]) -> str:
    for record in (state.get("team_owner"), state.get("leader_receiver")):
        if isinstance(record, dict) and record.get("machine_fingerprint"):
            return str(record["machine_fingerprint"])
    return os.environ.get("TEAM_AGENT_MACHINE_FINGERPRINT") or os.uname().nodename


def _identity_os_user() -> str:
    return os.environ.get("USER") or os.environ.get("USERNAME") or ""


def _state_leader_session_uuid(state: dict[str, Any]) -> str:
    for record in (state.get("team_owner"), state.get("leader_receiver")):
        if isinstance(record, dict) and record.get("leader_session_uuid"):
            return str(record["leader_session_uuid"])
    return ""


def _leader_provider_env(provider: str, identity: dict[str, Any]) -> dict[str, str]:
    return {
        "TEAM_AGENT_LEADER_PROVIDER": provider,
        "TEAM_AGENT_LEADER_SESSION_UUID": str(identity["leader_session_uuid"]),
        "TEAM_AGENT_MACHINE_FINGERPRINT": str(identity["machine_fingerprint"]),
        "TEAM_AGENT_WORKSPACE": str(identity["workspace_abspath"]),
        "TEAM_AGENT_TEAM_ID": str(identity["team_id"]),
    }


def _leader_plan_log_argv(plan: dict[str, Any]) -> list[str]:
    uuid_value = str(plan.get("leader_session_uuid") or "")
    if not uuid_value:
        return plan["argv"]
    return [str(part).replace(uuid_value, f"{uuid_value[:12]}...") for part in plan["argv"]]


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
    identity = _leader_identity_context(workspace, state=state)
    if identity.get("leader_session_uuid_source") == "override":
        event_log.write("leader_session_uuid.override", source="explicit-override", uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12], team_id=identity.get("team_id"))
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
    if receiver_provider != "fake":
        receiver["leader_session_uuid"] = identity["leader_session_uuid"]
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
            uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12],
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
        uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12],
        leader_session_uuid_source=identity.get("leader_session_uuid_source"),
    )
    return receiver, validation


def leader_identity(workspace: Path, team: str | None = None) -> dict[str, Any]:
    state = _load_identity_state(workspace, team)
    identity = _leader_identity_context(workspace, team=team, state=state)
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    return {
        "ok": True,
        "uuid_prefix": str(identity["leader_session_uuid"])[:12],
        "machine_fingerprint": identity["machine_fingerprint"],
        "workspace_abspath": identity["workspace_abspath"],
        "os_user": identity["os_user"],
        "team_id": identity["team_id"],
        "current_pane_id": os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or os.environ.get("TMUX_PANE") or None,
        "last_seen_at": receiver.get("attached_at") or receiver.get("last_seen_at"),
        "source": identity["leader_session_uuid_source"],
    }


def claim_leader(workspace: Path, team: str | None = None, confirm: bool = False) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError, _runtime_lock, core_list_targets
    current_pane = os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or os.environ.get("TMUX_PANE")
    if not current_pane:
        return {"ok": False, "status": "refused", "reason": "no_caller_pane", "action": "run from a tmux leader pane"}
    with _runtime_lock(workspace, "leader_receiver"):
        state = select_runtime_state(workspace, team)
        event_log = EventLog(workspace)
        incident = _latest_ambiguous_incident(event_log, team_state_key(state))
        if not incident:
            return {"ok": False, "status": "refused", "reason": "no_ambiguous_candidates"}
        candidates = [str(item) for item in incident.get("candidates", [])]
        if current_pane not in candidates:
            return {"ok": False, "status": "refused", "reason": "caller_not_candidate", "candidates": candidates}
        receiver = state.get("leader_receiver") or {}
        if receiver.get("pane_id") == current_pane:
            return {"ok": True, "status": "already_bound", "leader_receiver": receiver}
        if _incident_already_claimed(event_log, str(incident.get("incident_id"))):
            return _claim_lost_race(receiver)
        if receiver.get("pane_id") in candidates and receiver.get("pane_id") != incident.get("old_pane_id"):
            return _claim_lost_race(receiver)
        if not confirm:
            return {"ok": True, "status": "dry_run", "would_bind_pane_id": current_pane, "candidates": candidates}
        targets = core_list_targets()
        if not targets.get("ok"):
            raise RuntimeError(str(targets.get("error") or "tmux target scan failed"))
        target = next((item for item in targets.get("targets", []) if item.get("pane_id") == current_pane), None)
        if not target:
            return {"ok": False, "status": "refused", "reason": "candidate_pane_missing", "pane_id": current_pane}
        owner = state.setdefault("team_owner", {})
        expected_uuid = str(owner.get("leader_session_uuid") or _leader_identity_context(workspace, team=team, state=state)["leader_session_uuid"])
        target_uuid = _target_leader_session_uuid(target)
        if target_uuid != expected_uuid:
            return {"ok": False, "status": "refused", "reason": "leader_session_uuid_mismatch", "uuid_prefix": expected_uuid[:12]}
        epoch = int(owner.get("owner_epoch") or receiver.get("owner_epoch") or 0) + 1
        owner.update({"pane_id": current_pane, "owner_epoch": epoch, "claimed_at": datetime.now(timezone.utc).isoformat(), "claimed_via": "claim-leader"})
        state["leader_receiver"] = _receiver_from_claim_target(target, receiver, expected_uuid, epoch)
        save_team_scoped_state(workspace, state)
        losers = [pane for pane in candidates if pane != current_pane]
        event_log.write(
            "leader_receiver.claim_applied",
            incident_id=incident.get("incident_id"),
            winner_pane_id=current_pane,
            losers=losers,
            owner_epoch=epoch,
            uuid_prefix=expected_uuid[:12],
        )
        return {"ok": True, "status": "claimed", "leader_receiver": state["leader_receiver"], "owner_epoch": epoch, "losers": losers}


def _latest_ambiguous_incident(event_log: EventLog, team_id: str) -> dict[str, Any] | None:
    for event in reversed(event_log.tail(200)):
        if event.get("event") != "leader_receiver.ambiguous_candidates":
            continue
        if event.get("team_id") in {None, team_id}:
            return event
    return None


def _incident_already_claimed(event_log: EventLog, incident_id: str) -> bool:
    return any(event.get("event") == "leader_receiver.claim_applied" and event.get("incident_id") == incident_id for event in event_log.tail(200))


def _claim_lost_race(receiver: dict[str, Any]) -> dict[str, Any]:
    return {"ok": False, "status": "refused", "reason": "owner_epoch_advanced", "error": f"team already bound to pane {receiver.get('pane_id')}; you lost the race", "bound_pane_id": receiver.get("pane_id"), "owner_epoch": receiver.get("owner_epoch")}


def _target_leader_session_uuid(target: dict[str, Any]) -> str:
    env = target.get("leader_env") if isinstance(target.get("leader_env"), dict) else {}
    return str(target.get("leader_session_uuid") or env.get("TEAM_AGENT_LEADER_SESSION_UUID") or "")


def _receiver_from_claim_target(target: dict[str, Any], previous: dict[str, Any], leader_uuid: str, owner_epoch: int) -> dict[str, Any]:
    return {
        "mode": "direct_tmux",
        "status": "attached",
        "provider": previous.get("provider") or "codex",
        "pane_id": target["pane_id"],
        "session_name": target.get("session_name"),
        "window_index": str(target.get("window_index")),
        "window_name": target.get("window_name"),
        "pane_index": str(target.get("pane_index")),
        "pane_tty": target.get("pane_tty"),
        "pane_current_command": target.get("pane_current_command"),
        "leader_session_uuid": leader_uuid,
        "owner_epoch": owner_epoch,
        "attached_at": datetime.now(timezone.utc).isoformat(),
        "discovery": "claim_leader",
    }


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
    "claim_leader",
    "leader_identity",
    "leader_session_name",
    "leader_start_plan",
    "start_leader",
]
