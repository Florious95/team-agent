from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    RuntimeError,
    TMUX_PANE_FORMAT,
    _infer_active_tmux_pane as _runtime_infer_active_tmux_pane,
    _infer_workspace_tmux_pane as _runtime_infer_workspace_tmux_pane,
    _tmux_current_client_pane_info as _runtime_tmux_current_client_pane_info,
    _tmux_list_panes as _runtime_tmux_list_panes,
    _tmux_pane_info as _runtime_tmux_pane_info,
    core_list_targets,
    datetime,
    os,
    re,
    run_cmd,
    timezone,
)

from pathlib import Path
from typing import Any

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
    if len(parts) not in {8, 10}:
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
    if len(parts) == 10:
        keys.extend(["pane_current_path", "session_attached"])
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


def _rediscover_leader_receiver(receiver: dict[str, Any], event_log: EventLog) -> dict[str, Any]:
    provider = str(receiver.get("provider") or "codex")
    if provider != "codex":
        return {"status": "missing", "reason": "rediscovery_only_for_codex"}
    targets = core_list_targets()
    if not targets.get("ok"):
        event_log.write("leader_receiver.rediscover_failed", provider=provider, error=targets.get("error"))
        return {"status": "failed", "error": targets.get("error")}
    candidates = [
        target
        for target in targets.get("targets", [])
        if _leader_command_looks_usable(str(target.get("pane_current_command", "")), provider)
    ]
    if len(candidates) == 1:
        target = candidates[0]
        updated = {
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
            "discovery": "stale_rediscovery_unique_candidate",
        }
        event_log.write(
            "leader_receiver.rediscovered",
            provider=provider,
            old_target=receiver.get("pane_id"),
            new_target=updated["pane_id"],
            candidate_count=1,
        )
        return {"status": "updated", "receiver": updated}
    if len(candidates) > 1:
        event_log.write(
            "leader_receiver.rediscover_ambiguous",
            provider=provider,
            old_target=receiver.get("pane_id"),
            candidates=[target.get("pane_id") for target in candidates],
        )
        return {"status": "ambiguous", "candidates": candidates}
    event_log.write("leader_receiver.rediscover_missing", provider=provider, old_target=receiver.get("pane_id"))
    return {"status": "missing"}


def _validate_leader_receiver(receiver: dict[str, Any]) -> dict[str, Any]:
    pane_info = _runtime_tmux_pane_info(receiver.get("pane_id"))
    if not pane_info:
        return {"ok": False, "reason": "leader_pane_missing", "error": "tmux pane does not exist"}
    capture = run_cmd(["tmux", "capture-pane", "-p", "-S", "-40", "-t", pane_info["pane_id"]], timeout=5)
    if capture.returncode != 0:
        return {
            "ok": False,
            "reason": "leader_capture_failed",
            "error": capture.stderr.strip() or "tmux capture-pane failed",
            "pane": pane_info,
        }
    warning = None
    provider = str(receiver.get("provider") or "codex")
    if not _leader_command_looks_usable(pane_info.get("pane_current_command", ""), provider):
        warning = (
            f"pane command {pane_info.get('pane_current_command')!r} is not a typical {provider} host; "
            "continuing because tmux capture works"
        )
    return {"ok": True, "pane": pane_info, "capture": capture.stdout, "warning": warning}


def _leader_command_looks_usable(command: str, provider: str) -> bool:
    if provider == "fake":
        return True
    command_name = Path(command).name
    if provider == "codex":
        return command_name in {"codex", "node", "nodejs"}
    return bool(command_name)


def _choose_leader_submit_key(provider: str, capture_text: str) -> tuple[str, str]:
    if provider != "codex":
        return "Enter", "non_codex_provider"
    if re.search(r"esc to interrupt|working|running", capture_text, re.IGNORECASE):
        return "Enter", "codex_busy_submit_followup"
    if re.search(r"(›|❯|codex>)", capture_text):
        return "Enter", "codex_idle_prompt"
    return "Enter", "codex_state_unknown_submit"
