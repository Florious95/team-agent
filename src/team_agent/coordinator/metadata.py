from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.coordinator.paths import coordinator_meta_path
from team_agent.message_store import MessageStore


COORDINATOR_PROTOCOL_VERSION = 2


def pid_is_running(pid: int) -> bool:
    from team_agent.runtime import run_cmd
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    proc = run_cmd(["ps", "-p", str(pid), "-o", "stat="], timeout=5)
    if proc.returncode == 0 and proc.stdout.strip().upper().startswith("Z"):
        return False
    return True


def read_coordinator_metadata(workspace: Path) -> dict[str, Any] | None:
    path = coordinator_meta_path(workspace)
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return raw if isinstance(raw, dict) else None


def coordinator_metadata_ok(metadata: dict[str, Any] | None, pid: int) -> bool:
    return bool(
        metadata
        and metadata.get("pid") == pid
        and metadata.get("protocol_version") == COORDINATOR_PROTOCOL_VERSION
        and metadata.get("message_store_schema_version") == MessageStore.SCHEMA_VERSION
    )


def write_coordinator_metadata(workspace: Path, pid: int, source: str) -> None:
    path = coordinator_meta_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "pid": pid,
                "protocol_version": COORDINATOR_PROTOCOL_VERSION,
                "message_store_schema_version": MessageStore.SCHEMA_VERSION,
                "source": source,
                "updated_at": datetime.now(timezone.utc).isoformat(),
            },
            indent=2,
        ),
        encoding="utf-8",
    )
