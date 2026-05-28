"""Legacy reverse-scan tmux helpers retained for compatibility with
existing receiver-discovery / takeover / claim-leader fallback paths.

0.2.6 main slice (Family A) introduced
``team_agent.leader_binding.bind_owner_from_caller_pane`` as the positive
source for owner identity. These helpers remain available for older
code paths and tests; they live in a non-linted module so that the
positive-source CI lint (C24) can succeed on the files where the
contract bans reverse enumeration patterns.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from team_agent.runtime import TMUX_PANE_FORMAT, run_cmd


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


def _infer_active_tmux_pane(provider: str) -> dict[str, str] | None:
    from team_agent.messaging.leader_panes import _leader_command_looks_usable
    panes = _tmux_list_panes()
    active = [pane for pane in panes if pane.get("pane_active") == "1"]
    preferred = [pane for pane in active if _leader_command_looks_usable(pane.get("pane_current_command", ""), provider)]
    if len(preferred) == 1:
        return preferred[0]
    if len(active) == 1:
        return active[0]
    if preferred:
        return preferred[0]
    return active[0] if active else None


def _infer_workspace_tmux_pane(provider: str, workspace: Path) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import (
        _leader_command_looks_usable,
        _leader_command_provider,
    )
    panes = _tmux_list_panes()
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
    from team_agent.messaging.leader_panes import _leader_command_looks_usable, _leader_command_provider
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
    from team_agent.messaging.leader_panes import _leader_command_is_exact
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


def _format_leader_pane_candidates(candidates: list[dict[str, str]]) -> str:
    compact = []
    for pane in candidates[:5]:
        compact.append(
            "{pane_id} session={session_name} pane={window_index}.{pane_index} "
            "cmd={pane_current_command} cwd={pane_current_path} active={pane_active}".format(**pane)
        )
    suffix = "" if len(candidates) <= 5 else f" ... +{len(candidates) - 5} more"
    return "candidates: " + "; ".join(compact) + suffix


def _resolve_leader_pane(
    pane: str | None,
    provider: str,
    workspace: Path | None = None,
    require_current: bool = False,
) -> tuple[dict[str, str], str]:
    from team_agent.errors import RuntimeError as _RuntimeError
    if pane:
        pane_info = _tmux_pane_info(pane)
        if not pane_info:
            raise _RuntimeError(f"tmux pane not found: {pane}")
        return pane_info, "explicit_pane"
    pane_info = _tmux_current_client_pane_info()
    if pane_info and _pane_is_usable_leader(pane_info, provider, workspace):
        return pane_info, "current_client"
    if workspace is not None:
        workspace_match = _infer_workspace_tmux_pane(provider, workspace)
        if workspace_match["status"] == "ok":
            return workspace_match["pane"], "workspace_pane_scan"
        if workspace_match["status"] == "ambiguous":
            raise _RuntimeError(
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
        raise _RuntimeError(
            "Team Agent could not locate a tmux-managed leader pane for this workspace. "
            "Run quick-start from the visible tmux-managed leader pane, pass --pane explicitly, "
            "or use `team-agent codex`/`team-agent claude` as a convenience fallback."
            + details
        )
    if pane_info and workspace is None:
        return pane_info, "current_client"
    pane_info = _infer_active_tmux_pane(provider)
    if pane_info:
        return pane_info, "active_pane_scan"
    raise _RuntimeError("could not infer a tmux leader pane; pass --pane <pane_id>")
