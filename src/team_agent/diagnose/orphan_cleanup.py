"""Stage 14 (Gap 37a) — `team-agent doctor --cleanup-orphans` implementation.

Scans `ps` for processes matching `team_agent.coordinator --workspace <path>` and
classifies any whose workspace path no longer exists (or matches the test-tempdir
pattern) as an orphan. Dry-run by default; --confirm sends SIGTERM.

Mac mini 2026-05-26 evidence: 35 orphan coordinator processes alive simultaneously
pointing at /var/folders/.../T/team-agent-watcher-dedupe-* paths that had been removed
hours earlier. Each holds a long-lived Python interpreter + SQLite connection.
"""
from __future__ import annotations

import os
import re
import signal
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# Pattern: argv contains "team_agent.coordinator --workspace <path>" anywhere.
_COORDINATOR_ARGV_RE = re.compile(
    r"team_agent\.coordinator(?:\.__main__)?(?:\s+|.*?)\s--workspace\s+(\S+)"
)
# Test-tempdir patterns that indicate the workspace is ephemeral and almost certainly orphan.
_EPHEMERAL_PATH_HINTS = (
    "team-agent-watcher-dedupe-",
    "team-agent-gap",
    "team-agent-stage",
    "team-agent-orchestrator-",
    "team-agent-rm-",
    "team-agent-claim-",
    "team-agent-hotfix",
    "team-agent-multi",
    "team-agent-progress-",
    "team-agent-fanout-",
    "team-agent-in-flight-",
    "team-agent-test-",
)
_SIGTERM_WAIT_SECONDS = 3.0


def find_coordinator_processes(*, runner=subprocess.run) -> list[dict[str, Any]]:
    """Return list of {pid, etime, cmdline, workspace} dicts for every running
    team_agent.coordinator process visible to ps. workspace is None when the cmdline
    doesn't parse — those are noted but not auto-classified as orphan."""
    try:
        proc = runner(
            ["ps", "-Awwo", "pid=,etime=,command="],
            text=True,
            capture_output=True,
            timeout=5,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return []
    if proc.returncode != 0 or not proc.stdout:
        return []
    rows: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        pid_s, etime, cmdline = parts[0], parts[1], parts[2]
        if "team_agent.coordinator" not in cmdline:
            continue
        if "ps -Awwo" in cmdline:
            continue
        try:
            pid = int(pid_s)
        except ValueError:
            continue
        if pid == os.getpid():
            continue
        match = _COORDINATOR_ARGV_RE.search(cmdline)
        workspace = match.group(1) if match else None
        rows.append({
            "pid": pid,
            "etime": etime,
            "cmdline": cmdline,
            "workspace": workspace,
        })
    return rows


def classify_orphan(entry: dict[str, Any]) -> tuple[bool, str]:
    """Return (is_orphan, reason). An entry is orphan when its workspace path no longer
    exists on disk OR matches a known ephemeral-tempdir pattern (test workspaces should
    NEVER spawn long-lived coordinators)."""
    workspace = entry.get("workspace")
    if not workspace:
        return False, "cmdline_unparsed"
    if not Path(workspace).exists():
        return True, "workspace_path_missing"
    for hint in _EPHEMERAL_PATH_HINTS:
        if hint in workspace:
            return True, f"ephemeral_tempdir_pattern:{hint}"
    return False, "workspace_alive"


def cleanup_orphan_coordinators(
    *,
    confirm: bool = False,
    runner=subprocess.run,
    killer=os.kill,
    sleeper=time.sleep,
) -> dict[str, Any]:
    """Scan for orphan coordinators. Without confirm: dry-run (just classify and report).
    With confirm: SIGTERM each orphan and wait up to _SIGTERM_WAIT_SECONDS for the
    process to exit; report success/failure per pid."""
    now = datetime.now(timezone.utc).isoformat()
    entries = find_coordinator_processes(runner=runner)
    classified: list[dict[str, Any]] = []
    orphans: list[dict[str, Any]] = []
    for entry in entries:
        is_orphan, reason = classify_orphan(entry)
        annotated = {**entry, "is_orphan": is_orphan, "reason": reason}
        classified.append(annotated)
        if is_orphan:
            orphans.append(annotated)
    if not confirm:
        return {
            "ok": True,
            "scanned": len(classified),
            "orphans": orphans,
            "dry_run": True,
            "scanned_at": now,
            "action_required": "re-run with --confirm to send SIGTERM",
        }
    killed: list[dict[str, Any]] = []
    failed: list[dict[str, Any]] = []
    for entry in orphans:
        pid = entry["pid"]
        try:
            killer(pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError, OSError) as exc:
            failed.append({**entry, "error": str(exc)})
            continue
        # Wait briefly; if the process is still alive after _SIGTERM_WAIT_SECONDS,
        # mark as failed (caller may want to SIGKILL).
        deadline = time.monotonic() + _SIGTERM_WAIT_SECONDS
        gone = False
        while time.monotonic() < deadline:
            try:
                killer(pid, 0)
            except ProcessLookupError:
                gone = True
                break
            except (PermissionError, OSError):
                gone = True
                break
            sleeper(0.1)
        if gone:
            killed.append(entry)
        else:
            failed.append({**entry, "error": "still_alive_after_sigterm"})
    return {
        "ok": True,
        "scanned": len(classified),
        "orphans": orphans,
        "killed": killed,
        "failed": failed,
        "dry_run": False,
        "scanned_at": now,
    }


def format_cleanup_orphans(result: dict[str, Any]) -> str:
    lines = [
        f"Coordinator orphan scan @ {result.get('scanned_at')}",
        f"  scanned: {result.get('scanned', 0)} coordinator processes",
        f"  orphans: {len(result.get('orphans') or [])}",
    ]
    if result.get("dry_run"):
        lines.append("  mode: DRY-RUN (no SIGTERM sent; re-run with --confirm)")
    else:
        lines.append(f"  killed: {len(result.get('killed') or [])}")
        lines.append(f"  failed: {len(result.get('failed') or [])}")
    for orphan in result.get("orphans") or []:
        lines.append(
            f"  PID {orphan['pid']} etime={orphan['etime']} "
            f"workspace={orphan.get('workspace') or '?'} reason={orphan.get('reason')}"
        )
    return "\n".join(lines)


__all__ = [
    "cleanup_orphan_coordinators",
    "classify_orphan",
    "find_coordinator_processes",
    "format_cleanup_orphans",
]
