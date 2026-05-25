from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.events import EventLog

_TEAM_AGENT_BUFFER_PREFIXES = ("team-agent-send-", "team-agent-leader-receiver-", "team-agent-")


def _is_team_agent_buffer(name: str) -> bool:
    return any(name.startswith(prefix) for prefix in _TEAM_AGENT_BUFFER_PREFIXES)


def cleanup_stale_team_agent_buffers(workspace: Path, event_log: EventLog, *, context: str) -> dict[str, Any]:
    from team_agent.runtime import run_cmd
    proc = run_cmd(["tmux", "list-buffers", "-F", "#{buffer_name}"], timeout=5)
    if proc.returncode != 0:
        event_log.write("paste_buffer_hygiene.list_failed", context=context, stderr=proc.stderr.strip()[:200])
        return {"ok": False, "deleted": [], "reason": "list_buffers_failed"}
    names = [line.strip() for line in proc.stdout.splitlines() if line.strip()]
    targets = [name for name in names if _is_team_agent_buffer(name)]
    deleted: list[str] = []
    for name in targets:
        delete_proc = run_cmd(["tmux", "delete-buffer", "-b", name], timeout=5)
        if delete_proc.returncode == 0:
            deleted.append(name)
    if deleted:
        event_log.write(
            "paste_buffer_hygiene.prevented_resume_injection",
            context=context,
            deleted_buffers=deleted,
            scanned_count=len(names),
            matched_count=len(targets),
        )
    return {"ok": True, "deleted": deleted, "scanned": len(names), "matched": len(targets)}


__all__ = ["cleanup_stale_team_agent_buffers"]
