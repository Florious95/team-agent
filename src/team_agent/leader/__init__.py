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
from team_agent.state import apply_first_time_leader_binding, derive_leader_session_uuid, leader_env_exports, load_runtime_state, save_runtime_state, save_team_scoped_state, select_runtime_state, team_state_key, validate_leader_uuid_from_targets


def attach_leader(workspace: Path, pane: str | None = None, provider: str = "codex") -> dict[str, Any]:
    from team_agent.message_store import MessageStore
    from team_agent.runtime import _attach_leader_to_state, _runtime_lock, ensure_workspace_dirs
    ensure_workspace_dirs(workspace)
    # MED1/MED3: attach is a lease mutation; hold the single lease mutex so the state
    # change + event emission + dual-state write happen in one critical section.
    with _runtime_lock(workspace, LEADER_OWNERSHIP_LOCK):
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
        core_list_targets,
        get_adapter,
        run_cmd,
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
    if not state.get("team_owner") and source in {"launch", "quick_start"}:
        validation = apply_first_time_leader_binding(workspace, state, receiver, pane_info, identity, source)
        if not validation["ok"]:
            event_log.write("leader_receiver.attach_failed", target=pane or pane_info.get("pane_id"), discovery=discovery, provider=provider, reason=validation["reason"], error=validation.get("error"), source=source, first_time=True, uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12])
            raise RuntimeError(f"leader pane validation failed: {validation['reason']}")
        _set_tmux_leader_environment(receiver, identity, event_log, run_cmd)
        event_log.write("leader_receiver.attached", target=receiver["pane_id"], session_name=receiver["session_name"], window_index=receiver["window_index"], window_name=receiver["window_name"], pane_index=receiver["pane_index"], pane_tty=receiver["pane_tty"], pane_current_command=receiver["pane_current_command"], provider=receiver_provider, requested_provider=provider if receiver_provider != provider else None, discovery=discovery, source=source, first_time=True, uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12], leader_session_uuid_source=identity.get("leader_session_uuid_source"))
        return receiver, validation
    owner_record = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    if receiver_provider != "fake":
        # C10/C12: carry the recorded owner's identity rather than re-deriving one
        # that can drift under symlinked/worktree paths.
        receiver["leader_session_uuid"] = str(owner_record.get("leader_session_uuid") or identity["leader_session_uuid"])
    if receiver_provider != provider:
        receiver["requested_provider"] = provider
    targets = core_list_targets()
    validation = validate_leader_uuid_from_targets(receiver, targets)
    if validation["ok"]:
        validation = _validate_leader_receiver(receiver)
    if not validation["ok"]:
        readopt = _try_readopt_leader_pane(workspace, state, receiver, pane_info, targets, owner_record, receiver_provider, source, event_log)
        if readopt is not None:
            return readopt
        event_log.write("leader_receiver.attach_failed", target=pane or pane_info.get("pane_id"), discovery=discovery, provider=provider, reason=validation["reason"], error=validation.get("error"), source=source, uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12])
        raise RuntimeError(_strict_leader_validation_error(validation))
    if validation.get("warning"):
        receiver["warning"] = validation["warning"]
    state["leader_receiver"] = receiver
    event_log.write("leader_receiver.attached", target=receiver["pane_id"], session_name=receiver["session_name"], window_index=receiver["window_index"], window_name=receiver["window_name"], pane_index=receiver["pane_index"], pane_tty=receiver["pane_tty"], pane_current_command=receiver["pane_current_command"], provider=receiver_provider, requested_provider=provider if receiver_provider != provider else None, discovery=discovery, source=source, uuid_prefix=str(identity.get("leader_session_uuid") or "")[:12], leader_session_uuid_source=identity.get("leader_session_uuid_source"))
    return receiver, validation


def _set_tmux_leader_environment(receiver: dict[str, Any], identity: dict[str, Any], event_log: EventLog, run_cmd: Any) -> None:
    session_name = receiver.get("session_name")
    if not session_name:
        return
    failures: dict[str, str] = {}
    for key, value in leader_env_exports(receiver, identity).items():
        proc = run_cmd(["tmux", "set-environment", "-t", str(session_name), key, value], timeout=5)
        if proc.returncode != 0:
            failures[key] = proc.stderr.strip() or "tmux set-environment failed"
    event_log.write(
        "leader_receiver.first_time_env_seeded",
        pane_id=receiver.get("pane_id"),
        session_name=session_name,
        ok=not failures,
        failed_keys=sorted(failures),
    )

def _strict_leader_validation_error(validation: dict[str, Any]) -> str:
    return (
        f"leader pane validation failed: {validation['reason']}. "
        "first quick-start uses cwd+command match only; this team already has team_owner "
        "so strict UUID gate applies; use team-agent takeover --confirm if you intend to take over"
    )


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


_LEASE_REASON_ENUM = frozenset(
    {
        "vacant_acquired",
        "previous_owner_pane_dead",
        "previous_owner_alive_refused",
        "owner_epoch_advanced",
        "force_confirm_required",
        "caller_not_leader_shaped",
        "caller_cwd_mismatch",
        "not_in_tmux_pane",
    }
)
_LEASE_REBIND_REQUIRED_REASONS = frozenset(
    {"not_in_tmux_pane", "caller_not_leader_shaped", "caller_cwd_mismatch"}
)

# MED1/MED3 (spark, 2026-05-27): one lease mutex serializes every lease mutation —
# takeover, claim-leader, attach-leader, and autobind. It is the "send" lock so that
# ownership transfer also serializes against the send mutator (a concurrent send by
# the old owner cannot race a rebind). takeover must stay on this lock for the same
# reason, so the three verbs share a single named critical section.
LEADER_OWNERSHIP_LOCK = "send"


def _lease_caller_pane() -> str:
    return os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or os.environ.get("TMUX_PANE") or ""


def _lease_epoch(owner: dict[str, Any] | None, receiver: dict[str, Any] | None) -> int:
    return int((owner or {}).get("owner_epoch") or (receiver or {}).get("owner_epoch") or 0)


def _pane_is_live_leader(target: dict[str, Any] | None) -> bool:
    # C1/C2: liveness is a live tmux probe. A pane is a live leader if the
    # process tree carries the leader session env (set even when a child command
    # is foreground), or the pane's current command is a provider leader host.
    if not isinstance(target, dict):
        return False
    from team_agent.messaging.leader_panes import _leader_command_looks_usable, _leader_command_provider, _target_leader_session_uuid
    if _target_leader_session_uuid(target):
        return True
    command = str(target.get("pane_current_command", ""))
    return _leader_command_looks_usable(command, "") or _leader_command_provider(command) is not None


def _owner_pane_is_live(target: dict[str, Any] | None, owner_record: dict[str, Any] | None) -> bool:
    # MED2 (spark, 2026-05-27): a recorded owner is only "live" when the candidate
    # pane carries the OWNER's identity, not merely a leader-looking command name.
    # When the owner has a recorded leader_session_uuid, that uuid is the identity:
    # a stray node/claude pane without the matching uuid is not the owner (so a
    # dead-owner recover proceeds), and the real owner is still live even with a
    # non-leader foreground command as long as its session uuid is in the tree.
    if not isinstance(target, dict):
        return False
    owner = owner_record or {}
    owner_uuid = str(owner.get("leader_session_uuid") or "")
    if owner_uuid:
        from team_agent.messaging.leader_panes import _target_leader_session_uuid
        return _target_leader_session_uuid(target) == owner_uuid
    # No recorded uuid: fall back to provider identity (process tree / command for
    # the owner's provider) rather than any leader-looking command.
    owner_provider = str(owner.get("provider") or "")
    if owner_provider:
        from team_agent.messaging.leader_panes import _leader_command_looks_usable, _target_leader_session_uuid
        if _target_leader_session_uuid(target):
            return True
        return _leader_command_looks_usable(str(target.get("pane_current_command", "")), owner_provider)
    return _pane_is_live_leader(target)


def _cwd_inside_workspace(cwd: str | None, workspace: Path) -> bool:
    # C7/C8: realpath both sides; membership is subtree containment.
    if not cwd:
        return True
    ws = os.path.realpath(str(workspace.resolve()))
    candidate = os.path.realpath(str(cwd))
    return candidate == ws or candidate.startswith(ws + os.sep)


def _caller_pane_eligibility(target: dict[str, Any] | None, workspace: Path) -> dict[str, Any]:
    # C5: acquire binds the caller pane only when it is leader-shaped and its cwd
    # is inside the workspace. A plain shell / worker pane never self-binds.
    if not _pane_is_live_leader(target):
        return {"ok": False, "reason": "caller_not_leader_shaped", "action": "run team-agent claim-leader from a leader (claude/codex) tmux pane"}
    if not _cwd_inside_workspace((target or {}).get("pane_current_path"), workspace):
        return {"ok": False, "reason": "caller_cwd_mismatch", "action": "run from a leader pane whose cwd is inside this workspace"}
    return {"ok": True}


def _lease_refused(reason: str, *, action: str | None = None, **extra: Any) -> dict[str, Any]:
    result: dict[str, Any] = {"ok": False, "status": "refused", "reason": reason}
    if action:
        result["action"] = action
    result.update(extra)
    return result


def _emit_lease_refusal(
    event_log: EventLog,
    reason: str,
    owner: dict[str, Any] | None,
    old_pane: str | None,
    new_pane: str | None,
    team_id: str | None,
    host: str,
    os_user: str,
) -> None:
    # C20/C21/C22: every refusal emits a structured audit event with a closed-enum
    # reason, redacted uuid prefix, old/new pane id, host, and OS user.
    name = "leader_receiver.rebind_required" if reason in _LEASE_REBIND_REQUIRED_REASONS else "leader_receiver.claim_refused"
    event_log.write(
        name,
        reason=reason,
        old_pane_id=old_pane,
        new_pane_id=new_pane,
        uuid_prefix=str((owner or {}).get("leader_session_uuid") or "")[:8],
        team_id=team_id,
        host=host,
        os_user=os_user,
    )


def _try_readopt_leader_pane(
    workspace: Path,
    state: dict[str, Any],
    receiver: dict[str, Any],
    pane_info: dict[str, Any],
    targets: dict[str, Any],
    owner_record: dict[str, Any],
    receiver_provider: str,
    source: str,
    event_log: EventLog,
) -> tuple[dict[str, Any], dict[str, Any]] | None:
    # C4/C11/C12: attach-leader converges on the lease claim. When the strict UUID
    # gate would refuse, re-adopt the pane instead IF it is a live workspace leader
    # (real injected uuid + cwd inside the workspace subtree) and the lease is either
    # vacant or already owned by that same identity. A genuinely different live owner
    # still requires explicit takeover.
    from team_agent.messaging.leader_panes import _leader_command_looks_usable, _target_leader_session_uuid
    target_list = targets.get("targets", []) if isinstance(targets, dict) and targets.get("ok") else []
    pane_target = next((item for item in target_list if isinstance(item, dict) and str(item.get("pane_id")) == str(pane_info.get("pane_id"))), None)
    pane_uuid = _target_leader_session_uuid(pane_target or {}) or _target_leader_session_uuid(pane_info)
    if not pane_uuid:
        return None
    if not _cwd_inside_workspace(pane_info.get("pane_current_path"), workspace):
        return None
    if not _leader_command_looks_usable(str(pane_info.get("pane_current_command", "")), receiver_provider):
        return None
    owner_uuid = str(owner_record.get("leader_session_uuid") or "")
    if owner_uuid and owner_uuid != pane_uuid:
        return None
    epoch = _lease_epoch(owner_record, receiver) + (1 if owner_record else 0)
    receiver["leader_session_uuid"] = pane_uuid
    receiver["owner_epoch"] = epoch
    receiver["discovery"] = "attach_readopt"
    receiver.pop("warning", None)
    old_pane = owner_record.get("pane_id") or (state.get("leader_receiver") or {}).get("pane_id")
    state["team_owner"] = {
        "pane_id": pane_info["pane_id"],
        "provider": receiver.get("provider") or receiver_provider,
        "machine_fingerprint": owner_record.get("machine_fingerprint") or pane_info.get("machine_fingerprint") or "",
        "leader_session_uuid": pane_uuid,
        "owner_epoch": epoch,
        "claimed_at": datetime.now(timezone.utc).isoformat(),
        "claimed_via": "attach-leader",
    }
    state["leader_receiver"] = receiver
    _write_lease_dual_state(workspace, state)
    if old_pane and old_pane != pane_info["pane_id"]:
        event_log.write("owner.adopted_on_restart", reason="attach_readopt", old_pane_id=old_pane, new_pane_id=pane_info["pane_id"], owner_epoch=epoch, uuid_prefix=pane_uuid[:8], team_id=team_state_key(state))
    event_log.write("leader_receiver.rebind_applied", reason="attach_readopt", old_pane_id=old_pane, new_pane_id=pane_info["pane_id"], owner_epoch=epoch, uuid_prefix=pane_uuid[:8], team_id=team_state_key(state))
    event_log.write("leader_receiver.attached", target=pane_info["pane_id"], session_name=pane_info.get("session_name"), provider=receiver.get("provider"), discovery="attach_readopt", source=source, owner_epoch=epoch, uuid_prefix=pane_uuid[:8])
    return receiver, {"ok": True, "pane": pane_info, "readopted": True, "warning": None}


def _detect_dual_state_divergence(workspace: Path, state: dict[str, Any]) -> dict[str, Any] | None:
    # C18: the workspace-level state.json and the team-level runtime snapshot must
    # agree on owner_uuid, receiver_pane_id, and owner_epoch. Detect a pre-existing
    # split so the repair can be audited.
    session = state.get("session_name")
    if not session:
        return None
    from team_agent.restart.snapshot import load_snapshot_state, team_runtime_snapshot_dir
    snap_path = team_runtime_snapshot_dir(workspace, str(session)) / "state.json"
    if not snap_path.exists():
        return None
    snap = load_snapshot_state(snap_path) or {}
    ws_owner = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    snap_owner = snap.get("team_owner") if isinstance(snap.get("team_owner"), dict) else {}
    ws_receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    snap_receiver = snap.get("leader_receiver") if isinstance(snap.get("leader_receiver"), dict) else {}
    diverged = (
        ws_owner.get("pane_id") != snap_owner.get("pane_id")
        or ws_owner.get("leader_session_uuid") != snap_owner.get("leader_session_uuid")
        or _lease_epoch(ws_owner, ws_receiver) != _lease_epoch(snap_owner, snap_receiver)
        or ws_receiver.get("pane_id") != snap_receiver.get("pane_id")
    )
    if not diverged:
        return None
    return {
        "workspace_owner_pane": ws_owner.get("pane_id"),
        "team_owner_pane": snap_owner.get("pane_id"),
        "workspace_receiver_pane": ws_receiver.get("pane_id"),
        "team_receiver_pane": snap_receiver.get("pane_id"),
    }


def _write_lease_dual_state(workspace: Path, state: dict[str, Any]) -> None:
    # C17: write team_owner + leader_receiver to both state locations in one lock
    # hold. The workspace-level state.json and the team-level runtime snapshot
    # (teams/<session>/state.json) must never diverge after a lease mutation.
    save_team_scoped_state(workspace, state)
    if state.get("session_name"):
        from team_agent.restart.snapshot import save_team_runtime_snapshot
        save_team_runtime_snapshot(workspace, state)


def _claim_lease_no_incident(
    workspace: Path,
    state: dict[str, Any],
    team: str | None,
    team_id: str,
    caller_pane: str,
    confirm: bool,
    event_log: EventLog,
) -> dict[str, Any]:
    # Gap 39 unified lease: no ambiguous incident is recorded, so this is a direct
    # acquire/claim against live evidence (not the Gap 26 broadcast-claim flow).
    from team_agent.runtime import core_list_targets
    owner = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    precheck_epoch = _lease_epoch(owner, receiver)
    host = str(owner.get("machine_fingerprint") or _identity_machine_fingerprint(state))
    os_user = _identity_os_user()

    if not caller_pane:
        _emit_lease_refusal(event_log, "not_in_tmux_pane", owner, receiver.get("pane_id"), None, team_id, host, os_user)
        return _lease_refused("not_in_tmux_pane", action="run team-agent claim-leader from the leader's tmux pane")

    targets_result = core_list_targets()
    targets = targets_result.get("targets", []) if isinstance(targets_result, dict) and targets_result.get("ok") else []
    by_pane = {str(item.get("pane_id")): item for item in targets if isinstance(item, dict)}

    bound_pane = receiver.get("pane_id") or owner.get("pane_id")
    bound_alive = _owner_pane_is_live(by_pane.get(str(bound_pane)), owner) if bound_pane else False

    if bound_pane and str(bound_pane) == str(caller_pane):
        return {
            "ok": True,
            "status": "already_bound",
            "leader_receiver": receiver or None,
            "team_owner": owner or None,
            "owner_epoch": precheck_epoch,
        }

    caller_target = by_pane.get(str(caller_pane))
    eligibility = _caller_pane_eligibility(caller_target, workspace)
    if not eligibility["ok"]:
        _emit_lease_refusal(event_log, eligibility["reason"], owner, bound_pane, caller_pane, team_id, host, os_user)
        return _lease_refused(eligibility["reason"], action=eligibility.get("action"))

    if bound_alive and not confirm:
        # C4/C13: a live recorded owner is never stolen without --confirm. The audit
        # reason classifies the WHY (owner alive); the result hint tells the operator
        # the action (rerun with --confirm).
        _emit_lease_refusal(event_log, "previous_owner_alive_refused", owner, bound_pane, caller_pane, team_id, host, os_user)
        return _lease_refused(
            "force_confirm_required",
            action="rerun with --confirm to take over the live leader pane",
            bound_pane_id=bound_pane,
            owner_epoch=precheck_epoch,
        )

    # C3/C15: revalidate under the lock. Re-read both the persisted epoch and live
    # liveness; if the epoch advanced or a previously-dead owner revived since the
    # precheck, abort the claim without double-binding (lost the epoch race).
    locked_state = select_runtime_state(workspace, team)
    locked_owner = locked_state.get("team_owner") if isinstance(locked_state.get("team_owner"), dict) else {}
    locked_receiver = locked_state.get("leader_receiver") if isinstance(locked_state.get("leader_receiver"), dict) else {}
    locked_epoch = _lease_epoch(locked_owner, locked_receiver)
    recheck_result = core_list_targets()
    recheck_targets = recheck_result.get("targets", []) if isinstance(recheck_result, dict) and recheck_result.get("ok") else []
    recheck_by_pane = {str(item.get("pane_id")): item for item in recheck_targets if isinstance(item, dict)}
    revived = bool(bound_pane) and not bound_alive and _owner_pane_is_live(recheck_by_pane.get(str(bound_pane)), owner)
    if locked_epoch != precheck_epoch or (revived and not confirm):
        _emit_lease_refusal(event_log, "owner_epoch_advanced", locked_owner or owner, bound_pane, caller_pane, team_id, host, os_user)
        return _lease_refused(
            "owner_epoch_advanced",
            bound_pane_id=bound_pane,
            owner_epoch=max(locked_epoch, precheck_epoch),
        )

    divergence = _detect_dual_state_divergence(workspace, state)
    # C10/C12: the caller pane's injected TEAM_AGENT_LEADER_SESSION_UUID is the
    # authoritative identity for the bind; fall back to the recorded owner/receiver
    # uuid, then to the deterministic derivation.
    from team_agent.messaging.leader_panes import _target_leader_session_uuid
    next_epoch = precheck_epoch + 1
    leader_uuid = str(
        _target_leader_session_uuid(caller_target or {})
        or owner.get("leader_session_uuid")
        or receiver.get("leader_session_uuid")
        or _leader_identity_context(workspace, team=team, state=state)["leader_session_uuid"]
    )
    new_receiver = _receiver_from_claim_target(caller_target, receiver, leader_uuid, next_epoch)
    new_owner = {
        "pane_id": caller_pane,
        "provider": new_receiver.get("provider") or owner.get("provider") or "codex",
        "machine_fingerprint": host,
        "leader_session_uuid": leader_uuid,
        "owner_epoch": next_epoch,
        "claimed_at": datetime.now(timezone.utc).isoformat(),
        "claimed_via": "claim-leader",
    }
    state["team_owner"] = new_owner
    state["leader_receiver"] = new_receiver
    _write_lease_dual_state(workspace, state)
    dead_owner = bool(bound_pane) and not bound_alive
    reason = "previous_owner_pane_dead" if dead_owner else "vacant_acquired"
    if dead_owner:
        event_log.write(
            "owner.adopted_on_restart",
            reason=reason,
            old_pane_id=bound_pane,
            new_pane_id=caller_pane,
            owner_epoch=next_epoch,
            uuid_prefix=leader_uuid[:8],
            team_id=team_id,
            host=host,
            os_user=os_user,
        )
    event_log.write(
        "leader_receiver.rebind_applied",
        reason=reason,
        old_pane_id=bound_pane,
        new_pane_id=caller_pane,
        owner_epoch=next_epoch,
        uuid_prefix=leader_uuid[:8],
        team_id=team_id,
    )
    event_log.write(
        "owner_epoch_advanced",
        reason=reason,
        old_pane_id=bound_pane,
        new_pane_id=caller_pane,
        owner_epoch=next_epoch,
        uuid_prefix=leader_uuid[:8],
        team_id=team_id,
    )
    if divergence:
        # C18/C19: the workspace-level and team-level state had diverged before this
        # mutation; the single dual-write above re-converged them.
        event_log.write("leader_receiver.state_divergence_repaired", team_id=team_id, owner_epoch=next_epoch, new_pane_id=caller_pane, **divergence)
    return {
        "ok": True,
        "status": "claimed",
        "leader_receiver": new_receiver,
        "team_owner": new_owner,
        "owner_epoch": next_epoch,
        "reason": reason,
    }


def claim_leader(workspace: Path, team: str | None = None, confirm: bool = False) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError, _runtime_lock, core_list_targets
    current_pane = _lease_caller_pane()
    with _runtime_lock(workspace, LEADER_OWNERSHIP_LOCK):
        state = select_runtime_state(workspace, team)
        event_log = EventLog(workspace)
        team_id = team_state_key(state)
        incident = _latest_ambiguous_incident(event_log, team_id)
        if not incident:
            return _claim_lease_no_incident(workspace, state, team, team_id, current_pane, confirm, event_log)
        if not current_pane:
            return {"ok": False, "status": "refused", "reason": "no_caller_pane", "action": "run from a tmux leader pane"}
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
        # HIGH (spark, 2026-05-27): the multi-candidate claim branch must write both
        # state locations atomically (workspace state.json + team/<session> snapshot),
        # exactly like the no-incident lease path, so the branches never split state.
        divergence = _detect_dual_state_divergence(workspace, state)
        _write_lease_dual_state(workspace, state)
        if divergence:
            event_log.write("leader_receiver.state_divergence_repaired", team_id=team_id, owner_epoch=epoch, new_pane_id=current_pane, **divergence)
        losers = [pane for pane in candidates if pane != current_pane]
        event_log.write(
            "leader_receiver.claim_applied",
            incident_id=incident.get("incident_id"),
            winner_pane_id=current_pane,
            losers=losers,
            owner_epoch=epoch,
            uuid_prefix=expected_uuid[:12],
        )
        # Stage 11.9 (Gap 26 Mac mini Scenario 3): result watchers that stalled while the
        # broadcast was waiting for a human claim need fresh budget against the newly bound
        # pane. Per-watcher leader_receiver.claim_requeue events + immediate retry.
        from team_agent.message_store import MessageStore
        from team_agent.messaging.result_delivery import requeue_after_claim_leader
        requeued = requeue_after_claim_leader(
            workspace,
            MessageStore(workspace),
            event_log,
            team_state_key(state),
            current_pane,
            incident_ts=incident.get("ts"),
        )
        response: dict[str, Any] = {
            "ok": True,
            "status": "claimed",
            "leader_receiver": state["leader_receiver"],
            "owner_epoch": epoch,
            "losers": losers,
            "requeued_watchers": [item["watcher_id"] for item in requeued],
        }
        # Stage 13 (silent-loss arm mailbox-hint route, 2026-05-26 second roundtable):
        # the framework cannot guarantee every worker message reached the leader pane during
        # the ambiguous-state window (retry budgets may have exhausted before the human
        # claimed). Pointing the leader agent at the inbox lets it self-recover by reading
        # the messages that landed in storage but never injected to a pane.
        incident_ts = incident.get("ts")
        if incident_ts:
            response["inbox_hint"] = {
                "message": (
                    "During the previous ambiguous-leader state, some worker messages may "
                    "not have been auto-delivered to this pane. Run the command below to "
                    "retrieve them."
                ),
                "command": f"team-agent inbox leader --since {incident_ts}",
                "since": incident_ts,
                "incident_id": incident.get("incident_id"),
            }
        return response


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
    from team_agent.runtime import _runtime_lock, ensure_workspace_dirs
    ensure_workspace_dirs(workspace)
    # MED1/MED3: the startup autobind is a lease mutation; hold the single lease
    # mutex so it cannot interleave with takeover / claim / attach / send.
    with _runtime_lock(workspace, LEADER_OWNERSHIP_LOCK):
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
