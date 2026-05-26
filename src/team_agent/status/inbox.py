from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.message_store import MessageStore


def _parse_since(since: str | None) -> datetime | None:
    if not since:
        return None
    try:
        dt = datetime.fromisoformat(since.replace("Z", "+00:00"))
    except (ValueError, AttributeError):
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def _filter_since(rows: list[dict[str, Any]], since: str | None) -> list[dict[str, Any]]:
    cutoff = _parse_since(since)
    if cutoff is None:
        return rows
    filtered: list[dict[str, Any]] = []
    for row in rows:
        ts_raw = str(row.get("created_at") or "")
        ts = _parse_since(ts_raw)
        if ts and ts >= cutoff:
            filtered.append(row)
    return filtered


def inbox(workspace: Path, agent_id: str, limit: int = 20, since: str | None = None) -> dict[str, Any]:
    rows = MessageStore(workspace).inbox(agent_id, limit=limit)
    rows = _filter_since(rows, since)
    return {"ok": True, "agent_id": agent_id, "messages": rows, "since": since}


def format_inbox(workspace: Path, agent_id: str, limit: int = 20, since: str | None = None) -> str:
    store = MessageStore(workspace)
    rows = store.inbox(agent_id, limit=limit)
    rows = _filter_since(rows, since)
    result_counts = store.result_counts()
    note = "final results are not in inbox; use team-agent collect"
    if result_counts.get("uncollected", 0):
        note += f" ({result_counts['uncollected']} uncollected result(s) pending)"
    if not rows:
        if since:
            return f"{agent_id}: no messages since {since}\n{note}"
        return f"{agent_id}: no messages\n{note}"
    lines = [
        f"{row['created_at']} {row['sender']} -> {row['recipient']} {row['status']}: {row['content']}"
        for row in rows
    ]
    lines.append(note)
    return "\n".join(lines)
