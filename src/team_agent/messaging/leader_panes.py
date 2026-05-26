from __future__ import annotations

import hashlib

from team_agent.messaging.deps import (
    EventLog,
    RuntimeError,
    TMUX_PANE_FORMAT,
    _infer_active_tmux_pane as _runtime_infer_active_tmux_pane,
    _infer_workspace_tmux_pane as _runtime_infer_workspace_tmux_pane,
    _tmux_current_client_pane_info as _runtime_tmux_current_client_pane_info,
    _tmux_list_panes as _runtime_tmux_list_panes,
    _tmux_pane_info as _runtime_tmux_pane_info,
    _tmux_inject_text,
    core_list_targets,
    datetime,
    os,
    re,
    run_cmd,
    timezone,
)

from pathlib import Path
from typing import Any

_AMBIGUOUS_DEBOUNCE_SECONDS = 60

def _resolve_leader_pane(
    pane: str | None,
    provider: str,
    workspace: Path | None = None,
    require_current: bool = False,
) -> tuple[dict[str, str], str]:
    if pane:
        pane_info = _tmux_pane_info(pane)
        if not pane_info:
            raise RuntimeError(f"tmux pane not found: {pane}")
        return pane_info, "explicit_pane"
    pane_info = _runtime_tmux_current_client_pane_info()
    if pane_info and _pane_is_usable_leader(pane_info, provider, workspace):
        return pane_info, "current_client"
    if workspace is not None:
        workspace_match = _runtime_infer_workspace_tmux_pane(provider, workspace)
        if workspace_match["status"] == "ok":
            return workspace_match["pane"], "workspace_pane_scan"
        if workspace_match["status"] == "ambiguous":
            raise RuntimeError(
                "multiple tmux leader panes match this workspace; pass --pane explicitly. "
                + _format_leader_pane_candidates(workspace_match["candidates"])
            )
    if require_current:
        details = ""
        if pane_info:
            details = (
                f" Current tmux client points at pane {pane_info.get('pane_id')} "
                f"command={pane_info.get('pane_current_command')!r} "
                f"cwd={pane_info.get('pane_current_path')!r}, not a usable pane for this workspace."
            )
        raise RuntimeError(
            "Team Agent could not locate a tmux-managed leader pane for this workspace. "
            "Run quick-start from the visible tmux-managed leader pane, pass --pane explicitly, "
            "or use `team-agent codex`/`team-agent claude` as a convenience fallback."
            + details
        )
    if pane_info and workspace is None:
        return pane_info, "current_client"
    pane_info = _runtime_infer_active_tmux_pane(provider)
    if pane_info:
        return pane_info, "active_pane_scan"
    raise RuntimeError("could not infer a tmux leader pane; pass --pane <pane_id>")


def _tmux_current_client_pane_info() -> dict[str, str] | None:
    proc = run_cmd(["tmux", "display-message", "-p", "-F", TMUX_PANE_FORMAT], timeout=5)
    if proc.returncode != 0:
        return None
    return _parse_tmux_pane_info(proc.stdout.strip())


def _tmux_list_panes() -> list[dict[str, str]]:
    proc = run_cmd(["tmux", "list-panes", "-a", "-F", TMUX_PANE_FORMAT], timeout=5)
    if proc.returncode != 0:
        return []
    return [pane for line in proc.stdout.splitlines() if (pane := _parse_tmux_pane_info(line))]


def _infer_active_tmux_pane(provider: str) -> dict[str, str] | None:
    panes = _runtime_tmux_list_panes()
    active = [pane for pane in panes if pane.get("pane_active") == "1"]
    preferred = [pane for pane in active if _leader_command_looks_usable(pane.get("pane_current_command", ""), provider)]
    if len(preferred) == 1:
        return preferred[0]
    if len(active) == 1:
        return active[0]
    if preferred:
        return preferred[0]
    return active[0] if active else None


def _tmux_pane_info(target: str | None) -> dict[str, str] | None:
    if not target:
        return None
    proc = run_cmd(["tmux", "display-message", "-p", "-t", target, "-F", TMUX_PANE_FORMAT], timeout=5)
    if proc.returncode != 0:
        return None
    return _parse_tmux_pane_info(proc.stdout.strip())


def _parse_tmux_pane_info(line: str) -> dict[str, str] | None:
    parts = line.split("\t")
    if len(parts) not in {8, 10, 11}:
        return None
    keys = [
        "pane_id",
        "session_name",
        "window_index",
        "window_name",
        "pane_index",
        "pane_tty",
        "pane_current_command",
        "pane_active",
    ]
    if len(parts) >= 10:
        keys.extend(["pane_current_path", "session_attached"])
    if len(parts) == 11:
        keys.append("pane_in_mode")
    return dict(zip(keys, parts))


def _infer_workspace_tmux_pane(provider: str, workspace: Path) -> dict[str, Any]:
    panes = _runtime_tmux_list_panes()
    workspace_panes = [pane for pane in panes if _pane_path_matches_workspace(pane, workspace)]
    candidates = [
        pane
        for pane in workspace_panes
        if _leader_command_looks_usable(pane.get("pane_current_command", ""), provider)
        or _leader_command_provider(pane.get("pane_current_command", "")) is not None
    ]
    if not candidates:
        return {"status": "missing", "workspace_panes": workspace_panes}
    ranked = sorted(candidates, key=lambda item: _leader_pane_rank(item, provider), reverse=True)
    best_rank = _leader_pane_rank(ranked[0], provider)
    best = [pane for pane in ranked if _leader_pane_rank(pane, provider) == best_rank]
    if len(best) == 1:
        return {"status": "ok", "pane": best[0], "candidates": candidates}
    return {"status": "ambiguous", "candidates": best}


def _pane_is_usable_leader(pane: dict[str, str], provider: str, workspace: Path | None) -> bool:
    command = pane.get("pane_current_command", "")
    if not _leader_command_looks_usable(command, provider) and _leader_command_provider(command) is None:
        return False
    if workspace is not None and not _pane_path_matches_workspace(pane, workspace):
        return False
    return True


def _pane_path_matches_workspace(pane: dict[str, str], workspace: Path) -> bool:
    current_path = pane.get("pane_current_path")
    if not current_path:
        return False
    return os.path.realpath(current_path) == os.path.realpath(str(workspace.resolve()))


def _leader_pane_rank(pane: dict[str, str], provider: str) -> tuple[int, int, int]:
    return (
        _tmux_truthy(pane.get("session_attached", "")),
        1 if pane.get("pane_active") == "1" else 0,
        1 if _leader_command_is_exact(pane.get("pane_current_command", ""), provider) else 0,
    )


def _tmux_truthy(value: str) -> int:
    try:
        return 1 if int(value) > 0 else 0
    except (TypeError, ValueError):
        return 1 if value and value != "0" else 0


def _leader_command_is_exact(command: str, provider: str) -> bool:
    command_name = Path(command).name
    if provider == "codex":
        return command_name == "codex"
    if provider in {"claude", "claude_code"}:
        return command_name in {"claude", "claude.exe"}
    return provider == "fake"


def _leader_command_provider(command: str) -> str | None:
    command_name = Path(command).name
    if command_name in {"codex", "node", "nodejs"}:
        return "codex"
    if command_name in {"claude", "claude.exe"}:
        return "claude_code"
    return None


def _format_leader_pane_candidates(candidates: list[dict[str, str]]) -> str:
    compact = []
    for pane in candidates[:5]:
        compact.append(
            "{pane_id} session={session_name} pane={window_index}.{pane_index} "
            "cmd={pane_current_command} cwd={pane_current_path} active={pane_active}".format(**pane)
        )
    suffix = "" if len(candidates) <= 5 else f" ... +{len(candidates) - 5} more"
    return "candidates: " + "; ".join(compact) + suffix


def _target_fingerprint(pane_info: dict[str, Any]) -> str:
    return "|".join(
        str(pane_info.get(key, ""))
        for key in ["session_name", "window_index", "pane_index", "pane_tty"]
    )


def is_bound_pane_still_valid(state: dict[str, Any], store: Any | None = None) -> dict[str, Any]:
    receiver = dict(state.get("leader_receiver") or {})
    owner = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    if owner and owner.get("leader_session_uuid") and not receiver.get("leader_session_uuid"):
        receiver["leader_session_uuid"] = owner["leader_session_uuid"]
    return _validate_leader_receiver(receiver)


def _rediscover_leader_receiver(
    receiver: dict[str, Any],
    event_log: EventLog,
    owner_identity: dict[str, Any] | None = None,
    invalidation_reason: str | None = None,
    team_id: str | None = None,
) -> dict[str, Any]:
    provider = str(receiver.get("provider") or "codex")
    if provider == "fake":
        return {"status": "missing", "reason": "rediscovery_not_supported_for_fake"}
    targets = core_list_targets()
    if not targets.get("ok"):
        event_log.write("leader_receiver.rediscover_failed", provider=provider, error=targets.get("error"))
        # Stage 15 CI fix: when the tmux target scan itself fails (no server, no daemon,
        # CI env without tmux), the caller has no way to recover unless we also emit
        # rebind_required. Without this, _refresh_leader_receiver_or_flag_rebind silently
        # returns and report_result queues against the stale pane with zero audit signal.
        event_log.write(
            "leader_receiver.rebind_required",
            old_pane_id=receiver.get("pane_id"),
            reason=invalidation_reason,
            provider=provider,
            team_id=team_id,
            rediscovery_status="failed",
            error=targets.get("error"),
        )
        return {"status": "failed", "error": targets.get("error")}
    candidates = [
        target
        for target in targets.get("targets", [])
        if _leader_command_looks_usable(str(target.get("pane_current_command", "")), provider)
    ]
    if owner_identity:
        owner_candidates = [target for target in candidates if _target_matches_owner_identity(target, owner_identity)]
        if len(owner_candidates) == 1:
            return _rediscovered_receiver(receiver, provider, owner_candidates[0], event_log, owner_identity, invalidation_reason)
        if len(owner_candidates) > 1:
            incident = _broadcast_ambiguous_candidates(
                receiver,
                provider,
                owner_candidates,
                event_log,
                owner_identity,
                team_id,
            )
            event_log.write(
                "leader_receiver.rediscover_ambiguous",
                provider=provider,
                old_target=receiver.get("pane_id"),
                candidates=[target.get("pane_id") for target in owner_candidates],
                owner_identity=owner_identity,
                incident_id=incident.get("incident_id"),
                deduped=incident.get("deduped"),
            )
            return {"status": "ambiguous", "candidates": owner_candidates, "owner_identity": owner_identity, **incident}
        event_log.write(
            "leader_receiver.rediscover_missing",
            provider=provider,
            old_target=receiver.get("pane_id"),
            owner_identity=owner_identity,
            candidate_count=len(candidates),
        )
        event_log.write(
            "leader_receiver.rebind_required",
            old_pane_id=receiver.get("pane_id"),
            reason=invalidation_reason,
            provider=provider,
            team_id=team_id,
            uuid_prefix=_uuid_prefix(owner_identity),
            owner_identity=owner_identity,
            recovery_action="open the owning leader pane or run team-agent claim-leader --confirm from a matching pane",
        )
        return {"status": "missing", "owner_identity": owner_identity}
    if len(candidates) == 1:
        return _rediscovered_receiver(receiver, provider, candidates[0], event_log, None, invalidation_reason)
    if len(candidates) > 1:
        event_log.write(
            "leader_receiver.rediscover_ambiguous",
            provider=provider,
            old_target=receiver.get("pane_id"),
            candidates=[target.get("pane_id") for target in candidates],
        )
        event_log.write("leader_receiver.rebind_required", old_pane_id=receiver.get("pane_id"), reason=invalidation_reason, provider=provider, team_id=team_id, rediscovery_status="ambiguous")
        return {"status": "ambiguous", "candidates": candidates}
    event_log.write("leader_receiver.rediscover_missing", provider=provider, old_target=receiver.get("pane_id"))
    event_log.write("leader_receiver.rebind_required", old_pane_id=receiver.get("pane_id"), reason=invalidation_reason, provider=provider, team_id=team_id, rediscovery_status="missing")
    return {"status": "missing"}


def _target_matches_owner_identity(target: dict[str, Any], owner_identity: dict[str, Any]) -> bool:
    expected_uuid = owner_identity.get("leader_session_uuid")
    if expected_uuid:
        actual_uuid = _target_leader_session_uuid(target)
        if actual_uuid:
            return actual_uuid == expected_uuid
    env = target.get("leader_env") if isinstance(target.get("leader_env"), dict) else {}
    return (
        env.get("TEAM_AGENT_LEADER_PANE_ID") == (owner_identity.get("pane_id") or "")
        and env.get("TEAM_AGENT_LEADER_PROVIDER") == (owner_identity.get("provider") or "")
        and env.get("TEAM_AGENT_MACHINE_FINGERPRINT") == (owner_identity.get("machine_fingerprint") or "")
    )


def _target_leader_session_uuid(target: dict[str, Any]) -> str:
    env = target.get("leader_env") if isinstance(target.get("leader_env"), dict) else {}
    return str(target.get("leader_session_uuid") or env.get("TEAM_AGENT_LEADER_SESSION_UUID") or "")


def _leader_uuid_for_bound_pane(receiver: dict[str, Any], pane_info: dict[str, Any]) -> str:
    direct = _target_leader_session_uuid(pane_info) or _target_leader_session_uuid(receiver)
    if direct:
        return direct
    targets = core_list_targets()
    if not targets.get("ok"):
        return ""
    pane_id = pane_info.get("pane_id")
    for target in targets.get("targets", []):
        if target.get("pane_id") == pane_id:
            return _target_leader_session_uuid(target)
    return ""


def _uuid_prefix(owner_identity: dict[str, Any] | None) -> str:
    return str((owner_identity or {}).get("leader_session_uuid") or "")[:8]


def _receiver_from_target(target: dict[str, Any], provider: str, leader_uuid: str | None, owner_epoch: int | None = None) -> dict[str, Any]:
    receiver = {
        "mode": "direct_tmux",
        "status": "attached",
        "provider": provider,
        "pane_id": target["pane_id"],
        "session_name": target["session_name"],
        "window_index": str(target["window_index"]),
        "window_name": target["window_name"],
        "pane_index": str(target["pane_index"]),
        "pane_tty": target["pane_tty"],
        "pane_current_command": target["pane_current_command"],
        "fingerprint": target.get("fingerprint") or _target_fingerprint(target),
        "attached_at": datetime.now(timezone.utc).isoformat(),
    }
    if leader_uuid:
        receiver["leader_session_uuid"] = leader_uuid
    if owner_epoch is not None:
        receiver["owner_epoch"] = owner_epoch
    return receiver


def _broadcast_ambiguous_candidates(
    receiver: dict[str, Any],
    provider: str,
    candidates: list[dict[str, Any]],
    event_log: EventLog,
    owner_identity: dict[str, Any],
    team_id: str | None,
) -> dict[str, Any]:
    candidate_ids = sorted(str(candidate.get("pane_id")) for candidate in candidates)
    bucket = _ambiguous_debounce_bucket()
    incident_id = hashlib.sha256("\0".join([str(team_id or ""), *candidate_ids, bucket]).encode("utf-8")).hexdigest()[:16]
    if any(event.get("event") == "leader_receiver.ambiguous_candidates" and event.get("incident_id") == incident_id for event in event_log.tail(200)):
        return {"incident_id": incident_id, "deduped": True}
    prompt = _ambiguous_candidate_prompt(team_id, len(candidates))
    event_log.write(
        "leader_receiver.ambiguous_candidates",
        incident_id=incident_id,
        old_pane_id=receiver.get("pane_id"),
        candidates=candidate_ids,
        provider=provider,
        team_id=team_id,
        uuid_prefix=_uuid_prefix(owner_identity),
        debounce_bucket=bucket,
    )
    for candidate in candidates:
        pane_id = str(candidate.get("pane_id") or "")
        injected = _tmux_inject_text(
            pane_id,
            prompt,
            "Enter",
            f"team-agent-leader-ambiguous-{incident_id}-{pane_id.strip('%')}",
            provider=provider,
        )
        event_log.write(
            "leader_receiver.ambiguous_candidate_queued",
            incident_id=incident_id,
            pane_id=pane_id,
            ok=bool(injected.get("ok")),
            error=injected.get("error"),
        )
    return {"incident_id": incident_id, "deduped": False}


def _ambiguous_debounce_bucket() -> str:
    now = datetime.now(timezone.utc)
    epoch = int(now.timestamp() // _AMBIGUOUS_DEBOUNCE_SECONDS) * _AMBIGUOUS_DEBOUNCE_SECONDS
    return datetime.fromtimestamp(epoch, timezone.utc).isoformat()


def _ambiguous_candidate_prompt(team_id: str | None, candidate_count: int) -> str:
    others = max(candidate_count - 1, 0)
    return (
        f"Team `{team_id or 'current'}` has no bound leader. This window and {others} other window(s) all qualify. "
        "To claim this window as the team leader, run: `team-agent claim-leader --confirm`. "
        "Only the first such call wins; subsequent calls from other windows will be refused."
    )


def _rediscovered_receiver(
    receiver: dict[str, Any],
    provider: str,
    target: dict[str, Any],
    event_log: EventLog,
    owner_identity: dict[str, Any] | None,
    invalidation_reason: str | None = None,
) -> dict[str, Any]:
    leader_uuid = _target_leader_session_uuid(target) or (owner_identity or {}).get("leader_session_uuid") or receiver.get("leader_session_uuid")
    updated = _receiver_from_target(target, provider, leader_uuid)
    updated["discovery"] = "stale_rediscovery_owner_identity" if owner_identity else "stale_rediscovery_unique_candidate"
    event_log.write(
        "leader_receiver.rediscovered",
        provider=provider,
        old_target=receiver.get("pane_id"),
        new_target=updated["pane_id"],
        candidate_count=1,
        owner_identity=owner_identity,
    )
    event_log.write(
        "leader_receiver.rebind_applied",
        old_pane_id=receiver.get("pane_id"),
        new_pane_id=updated["pane_id"],
        reason=invalidation_reason,
        owner_identity=owner_identity,
        uuid_prefix=_uuid_prefix(owner_identity),
    )
    return {"status": "updated", "receiver": updated, "owner_identity": owner_identity}


def _validate_leader_receiver(receiver: dict[str, Any]) -> dict[str, Any]:
    pane_info = _runtime_tmux_pane_info(receiver.get("pane_id"))
    if not pane_info:
        return {"ok": False, "reason": "leader_pane_missing", "error": "tmux pane does not exist"}
    provider = str(receiver.get("provider") or "codex")
    if not _leader_command_looks_usable(pane_info.get("pane_current_command", ""), provider):
        return {
            "ok": False,
            "reason": "leader_pane_wrong_command",
            "error": f"pane command {pane_info.get('pane_current_command')!r} is not a leader host",
            "pane": pane_info,
        }
    expected_uuid = receiver.get("leader_session_uuid")
    if expected_uuid:
        actual_uuid = _leader_uuid_for_bound_pane(receiver, pane_info)
        if not actual_uuid:
            return {"ok": False, "reason": "leader_uuid_missing", "error": "bound pane has no TEAM_AGENT_LEADER_SESSION_UUID", "pane": pane_info}
        if actual_uuid != expected_uuid:
            return {
                "ok": False,
                "reason": "leader_uuid_mismatch",
                "error": "bound pane TEAM_AGENT_LEADER_SESSION_UUID does not match stored team owner",
                "pane": pane_info,
            }
    capture = run_cmd(["tmux", "capture-pane", "-p", "-S", "-40", "-t", pane_info["pane_id"]], timeout=5)
    if capture.returncode != 0:
        return {
            "ok": False,
            "reason": "leader_capture_failed",
            "error": capture.stderr.strip() or "tmux capture-pane failed",
            "pane": pane_info,
        }
    return {"ok": True, "pane": pane_info, "capture": capture.stdout, "warning": None}


def _leader_command_looks_usable(command: str, provider: str) -> bool:
    if provider == "fake":
        return True
    command_name = Path(command).name
    if provider == "codex":
        return command_name in {"codex", "node", "nodejs"}
    if provider in {"claude", "claude_code"}:
        return command_name in {"claude", "claude.exe"}
    return command_name in {"codex", "node", "nodejs", "claude", "claude.exe"}


def attempt_trust_auto_answer(
    workspace: Path,
    pane_id: str | None,
    pane_capture_tail: str,
    event_log: EventLog,
    *,
    spec: dict[str, Any] | None = None,
    state: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Gap 29 (Slice 2 Stage 2) — opt-in auto-answer of the codex first-run trust prompt.

    Called by the inject path when developer's structured envelope reports
    detected=='codex_trust_prompt'. Auto-answers ONLY when both:
      (1) runtime is opted in via spec.runtime.auto_trust_own_workspace=True OR env
          TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE in {1,true,yes,on}; and
      (2) the trust-prompt pane capture references this workspace's absolute path
          (so a worker can only trust its own dir, never some arbitrary path).

    On match, sends '1' + Enter to the pane and emits
    leader_panes.trust_auto_answered. Default is opt-out — every refusal returns
    answered=False with a structured reason and the existing failure envelope
    bubbles up unchanged.

    Return: {"ok": bool, "answered": bool, "reason": str, ...}
    """
    if spec is None and state is not None:
        spec_path_str = state.get("spec_path")
        if spec_path_str:
            try:
                from team_agent.spec import load_spec as _load_spec
                spec = _load_spec(Path(spec_path_str))
            except Exception:
                spec = None
    if not _auto_trust_opt_in(spec):
        return {"ok": False, "answered": False, "reason": "not_opted_in"}
    if not pane_id:
        return {"ok": False, "answered": False, "reason": "pane_id_missing"}
    if not _capture_tail_references_workspace(pane_capture_tail, workspace):
        event_log.write(
            "leader_panes.trust_auto_answer_refused",
            pane_id=pane_id,
            workspace=str(workspace),
            reason="workspace_dir_mismatch",
        )
        return {"ok": False, "answered": False, "reason": "workspace_dir_mismatch"}
    answer = run_cmd(["tmux", "send-keys", "-t", str(pane_id), "1", "Enter"], timeout=5)
    if answer.returncode != 0:
        error = answer.stderr.strip() or "tmux send-keys failed"
        event_log.write(
            "leader_panes.trust_auto_answer_failed",
            pane_id=pane_id,
            workspace=str(workspace),
            error=error,
        )
        return {"ok": False, "answered": False, "reason": "tmux_send_keys_failed", "error": error}
    event_log.write(
        "leader_panes.trust_auto_answered",
        pane_id=pane_id,
        workspace=str(workspace),
        opted_in=True,
    )
    return {"ok": True, "answered": True, "reason": "trust_auto_answered"}


def _auto_trust_opt_in(spec: dict[str, Any] | None) -> bool:
    env = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", "").strip().lower()
    if env in {"1", "true", "yes", "on"}:
        return True
    if not isinstance(spec, dict):
        return False
    runtime = spec.get("runtime") or {}
    return bool(runtime.get("auto_trust_own_workspace"))


def _capture_tail_references_workspace(tail: str, workspace: Path) -> bool:
    if not tail:
        return False
    try:
        resolved = str(workspace.resolve())
    except OSError:
        resolved = str(workspace)
    raw = str(workspace)
    return resolved in tail or (raw and raw in tail)


def _choose_leader_submit_key(provider: str, capture_text: str) -> tuple[str, str]:
    if provider != "codex":
        return "Enter", "non_codex_provider"
    if re.search(r"esc to interrupt|working|running", capture_text, re.IGNORECASE):
        return "Enter", "codex_busy_submit_followup"
    if re.search(r"(›|❯|codex>)", capture_text):
        return "Enter", "codex_idle_prompt"
    return "Enter", "codex_state_unknown_submit"
