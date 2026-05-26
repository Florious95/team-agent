from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import logs_dir


# Stage 14 (Gap 36a) — bounded retention. 5 MB cap × 5 archives = 25 MB worst-case.
# Mac mini 2026-05-26 evidence: events.jsonl grew to 28 MB / 128k lines in one day with
# unbounded retention; coordinator's tick-time scan over the file was a ~22% CPU hot path.
# Rotation keeps the current segment small so reads are cheap; archives preserve forensic
# history but are NOT consulted by hot-path scans.
EVENT_LOG_ROTATE_BYTES = 5 * 1024 * 1024
EVENT_LOG_ARCHIVE_KEEP = 5


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
        self._maybe_rotate()
        with self.path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")
        return event

    def tail(self, limit: int = 20) -> list[dict[str, Any]]:
        # Hot-path scan reads only the current segment. Archives are forensic; if a
        # caller genuinely needs longer history it can iterate _archive_paths explicitly.
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

    def _maybe_rotate(self) -> None:
        try:
            size = self.path.stat().st_size
        except FileNotFoundError:
            return
        if size < EVENT_LOG_ROTATE_BYTES:
            return
        # Shift archives: events.jsonl.4 → .5, .3 → .4, …, .1 → .2, current → .1
        # Drop the oldest if it would overflow the keep budget.
        oldest = self._archive_path(EVENT_LOG_ARCHIVE_KEEP)
        if oldest.exists():
            try:
                oldest.unlink()
            except OSError:
                pass
        for idx in range(EVENT_LOG_ARCHIVE_KEEP - 1, 0, -1):
            src = self._archive_path(idx)
            dst = self._archive_path(idx + 1)
            if src.exists():
                try:
                    os.replace(src, dst)
                except OSError:
                    pass
        try:
            os.replace(self.path, self._archive_path(1))
        except OSError:
            pass

    def _archive_path(self, index: int) -> Path:
        return self.path.with_name(f"{self.path.name}.{index}")

    def _archive_paths(self) -> list[Path]:
        return [self._archive_path(i) for i in range(1, EVENT_LOG_ARCHIVE_KEEP + 1)]
