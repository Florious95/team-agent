from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.message_store import MessageStore


def inbox(workspace: Path, agent_id: str, limit: int = 20) -> dict[str, Any]:
    rows = MessageStore(workspace).inbox(agent_id, limit=limit)
    return {"ok": True, "agent_id": agent_id, "messages": rows}


def format_inbox(workspace: Path, agent_id: str, limit: int = 20) -> str:
    store = MessageStore(workspace)
    rows = store.inbox(agent_id, limit=limit)
    result_counts = store.result_counts()
    note = "final results are not in inbox; use team-agent collect"
    if result_counts.get("uncollected", 0):
        note += f" ({result_counts['uncollected']} uncollected result(s) pending)"
    if not rows:
        return f"{agent_id}: no messages\n{note}"
    lines = [
        f"{row['created_at']} {row['sender']} -> {row['recipient']} {row['status']}: {row['content']}"
        for row in rows
    ]
    lines.append(note)
    return "\n".join(lines)
