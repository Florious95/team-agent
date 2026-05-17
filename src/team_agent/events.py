from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import logs_dir


class EventLog:
    def __init__(self, workspace: Path):
        self.workspace = workspace
        self.path = logs_dir(workspace) / "events.jsonl"
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def write(self, event_type: str, **fields: Any) -> dict[str, Any]:
        event = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "event": event_type,
            **fields,
        }
        with self.path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")
        return event

    def tail(self, limit: int = 20) -> list[dict[str, Any]]:
        if not self.path.exists():
            return []
        lines = self.path.read_text(encoding="utf-8").splitlines()[-limit:]
        out: list[dict[str, Any]] = []
        for line in lines:
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                out.append({"raw": line})
        return out
