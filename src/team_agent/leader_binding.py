"""Family A — positive-source owner binding (0.2.6).

Constitution MUST-11 / MUST-NOT-12: the caller identity for owner binding
is sourced from the caller-supplied positive facts only:

  1. ``$TMUX_PANE``  (the tmux pane the user invoked the CLI from).
  2. ``tmux display-message -p -t $TMUX_PANE '#{pane_current_command}'``
     (a single targeted lookup to confirm the caller pane is hosting a
     leader CLI).

Reverse enumeration of panes / windows / clients is forbidden. Heuristic
ranking ("active pane", "current client", "first leader-shaped pane") is
forbidden. ``$TMUX_PANE`` missing or the caller pane not running a leader
host → refuse and emit ``owner.bind_refused``.  Successful binds emit
``owner.bound_from_caller_pane`` and force-write every owner identity
field; old fields are not merged or migrated.
"""

from __future__ import annotations

import hashlib
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog


def run_cmd(args: list[str], timeout: int = 5) -> subprocess.CompletedProcess[str]:
    """Tmux command bridge — kept here so contract tests can patch
    ``team_agent.leader_binding.run_cmd`` to observe the exact tmux call
    pattern this module makes."""
    return subprocess.run(
        args, text=True, capture_output=True, timeout=timeout, check=False
    )


LEADER_HOST_COMMANDS = frozenset({"claude", "claude.exe", "codex"})

_HINT_RUN_FROM_LEADER_PANE = (
    "run team-agent from inside your leader pane "
    "(the tmux pane currently running claude or codex)."
)


def bind_owner_from_caller_pane(
    workspace: Path,
    team_id: str,
    override_uuid: str | None = None,
) -> dict[str, Any]:
    """Derive the owner identity from the caller pane.

    Returns
    -------
    On success::

        {
            "ok": True,
            "owner": {
                "pane_id": ..., "leader_session_uuid": ...,
                "machine_fingerprint": ..., "provider": ...,
                "os_user": ..., "claimed_at": <iso8601>,
            },
            "caller_pane_id": ..., "caller_current_command": ...,
            "team_id": team_id,
        }

    On refusal::

        {
            "ok": False,
            "reason": "caller_pane_missing" | "caller_not_leader_shaped",
            "caller_pane_id": ..., "caller_current_command": ...,
            "hint": ...,
        }
    """
    event_log = EventLog(workspace)
    caller_pane = os.environ.get("TMUX_PANE") or ""
    if not caller_pane:
        hint = _HINT_RUN_FROM_LEADER_PANE
        event_log.write(
            "owner.bind_refused",
            reason="caller_pane_missing",
            caller_pane_id="",
            caller_current_command="",
            team_id=team_id,
            hint=hint,
        )
        return {
            "ok": False,
            "reason": "caller_pane_missing",
            "caller_pane_id": "",
            "caller_current_command": "",
            "hint": hint,
        }
    proc = run_cmd(
        [
            "tmux",
            "display-message",
            "-p",
            "-t",
            caller_pane,
            "#{pane_current_command}",
        ],
        timeout=5,
    )
    if getattr(proc, "returncode", 1) != 0:
        caller_command = ""
    else:
        caller_command = (getattr(proc, "stdout", "") or "").strip()
    if caller_command not in LEADER_HOST_COMMANDS:
        hint = (
            f"run team-agent from inside your leader pane "
            f"(this pane is running {caller_command or '<unknown>'})."
        )
        event_log.write(
            "owner.bind_refused",
            reason="caller_not_leader_shaped",
            caller_pane_id=caller_pane,
            caller_current_command=caller_command,
            team_id=team_id,
            hint=hint,
        )
        return {
            "ok": False,
            "reason": "caller_not_leader_shaped",
            "caller_pane_id": caller_pane,
            "caller_current_command": caller_command,
            "hint": hint,
        }
    machine_fingerprint = os.environ.get("TEAM_AGENT_MACHINE_FINGERPRINT") or ""
    os_user = os.environ.get("USER") or os.environ.get("USERNAME") or ""
    provider = (
        os.environ.get("TEAM_AGENT_LEADER_PROVIDER")
        or _provider_from_command(caller_command)
    )
    if override_uuid:
        leader_session_uuid = override_uuid
    else:
        env_override = (
            os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") or ""
        )
        if env_override:
            leader_session_uuid = env_override
        else:
            leader_session_uuid = derive_leader_session_uuid(
                machine_fingerprint, workspace, os_user, team_id
            )
    owner = {
        "pane_id": caller_pane,
        "leader_session_uuid": leader_session_uuid,
        "machine_fingerprint": machine_fingerprint,
        "provider": provider,
        "os_user": os_user,
        "claimed_at": datetime.now(timezone.utc).isoformat(),
    }
    return {
        "ok": True,
        "owner": owner,
        "caller_pane_id": caller_pane,
        "caller_current_command": caller_command,
        "team_id": team_id,
    }


def derive_leader_session_uuid(
    machine_fingerprint: str, workspace: Path, os_user: str, team_id: str
) -> str:
    workspace_abspath = str(Path(workspace).resolve())
    payload = "\0".join((machine_fingerprint, workspace_abspath, os_user, team_id))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:32]


def _provider_from_command(cmd: str) -> str:
    if cmd in {"claude", "claude.exe"}:
        return "claude"
    if cmd == "codex":
        return "codex"
    return ""


def emit_owner_bound_event(
    workspace: Path,
    *,
    caller_pane_id: str,
    caller_current_command: str,
    derived_leader_session_uuid: str,
    team_id: str,
    old_leader_session_uuid: str | None,
) -> None:
    """Audit hook for successful owner bind. Use after a caller has
    accepted ``bind_owner_from_caller_pane`` and persisted the new owner.

    Only short uuid prefixes go to the event log so we do not leak the
    full session uuid into operator-readable transcripts."""
    EventLog(workspace).write(
        "owner.bound_from_caller_pane",
        caller_pane_id=caller_pane_id,
        caller_current_command=caller_current_command,
        derived_uuid_prefix=(derived_leader_session_uuid or "")[:12],
        team_id=team_id,
        old_uuid_prefix=(old_leader_session_uuid or "")[:12],
    )
