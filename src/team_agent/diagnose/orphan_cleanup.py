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
_SIGKILL_WAIT_SECONDS = 2.0


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
    if not os.path.exists(workspace):
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
    pg_killer=None,
    pgid_getter=None,
    sleeper=time.sleep,
    sigterm_wait_seconds: float = _SIGTERM_WAIT_SECONDS,
    sigkill_wait_seconds: float = _SIGKILL_WAIT_SECONDS,
) -> dict[str, Any]:
    """Scan for orphan coordinators. Without confirm: dry-run (just classify and report).
    With confirm: SIGTERM each orphan, wait up to _SIGTERM_WAIT_SECONDS for graceful
    exit; if still alive, escalate to SIGKILL and wait _SIGKILL_WAIT_SECONDS. Only
    report status='failed' (with error='alive_after_sigkill') when the process
    survives BOTH signals — that's extremely rare and almost always indicates a
    zombie/uninterruptible-sleep kernel state.

    Mac mini 2026-05-26 evidence: real orphan coordinators have been observed alive
    40+ hours; many of them never exit on SIGTERM (signal handler suppressed during
    long sqlite reads, or the python interpreter is hosting an async loop that
    swallows the term signal). SIGKILL escalation is required for production.

    pg_killer / pgid_getter default to os.killpg / os.getpgid; mock them in tests.
    If pgid_getter succeeds AND returns a pgid > 1 AND the pgid != pid (i.e. the
    process leads its own process group with children), we signal the WHOLE group;
    otherwise we signal the pid directly. This catches orphan coordinators that
    spawned subprocess.Popen children which would otherwise survive a pid-only
    SIGTERM."""
    now = datetime.now(timezone.utc).isoformat()
    if pg_killer is None:
        pg_killer = getattr(os, "killpg", None)
    if pgid_getter is None:
        pgid_getter = getattr(os, "getpgid", None)
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
        outcome = _terminate_orphan(
            entry["pid"], killer=killer, pg_killer=pg_killer,
            pgid_getter=pgid_getter, sleeper=sleeper,
            sigterm_wait_seconds=sigterm_wait_seconds,
            sigkill_wait_seconds=sigkill_wait_seconds,
        )
        annotated = {**entry, **outcome}
        if outcome.get("status") == "killed":
            killed.append(annotated)
        elif outcome.get("status") == "missing":
            killed.append(annotated)
        else:
            failed.append(annotated)
    return {
        "ok": True,
        "scanned": len(classified),
        "orphans": orphans,
        "killed": killed,
        "failed": failed,
        "dry_run": False,
        "scanned_at": now,
    }


def _terminate_orphan(
    pid: int,
    *,
    killer,
    pg_killer,
    pgid_getter,
    sleeper,
    sigterm_wait_seconds: float = _SIGTERM_WAIT_SECONDS,
    sigkill_wait_seconds: float = _SIGKILL_WAIT_SECONDS,
) -> dict[str, Any]:
    """SIGTERM → wait 3s → SIGKILL → wait 2s escalation. Returns one of:
      {status: 'killed', sigkill_required: False, signaled: 'pid'|'pgid'}
      {status: 'killed', sigkill_required: True,  signaled: 'pid'|'pgid'}
      {status: 'missing', error: '<exc>'} — process gone before SIGTERM
      {status: 'failed',  error: 'alive_after_sigkill'} — process survived both
      {status: 'failed',  error: '<exc>'} — permission denied / OS error
    """
    pgid, pgid_error = _safe_getpgid(pid, pgid_getter)
    use_group = bool(pg_killer and pgid is not None and pgid > 1 and pgid != pid)
    signaled = "pgid" if use_group else "pid"

    def send(sig: int) -> tuple[bool, str | None]:
        try:
            if use_group:
                pg_killer(pgid, sig)
            else:
                killer(pid, sig)
        except ProcessLookupError:
            return False, "process_lookup_error"
        except (PermissionError, OSError) as exc:
            return False, str(exc)
        return True, None

    ok, err = send(signal.SIGTERM)
    if not ok:
        if err == "process_lookup_error":
            return {"status": "missing", "signaled": signaled, "pgid": pgid}
        return {"status": "failed", "error": err, "signaled": signaled, "pgid": pgid}
    if _wait_for_exit(pid, sigterm_wait_seconds, killer=killer, sleeper=sleeper):
        return {
            "status": "killed",
            "sigkill_required": False,
            "signaled": signaled,
            "pgid": pgid,
            "pgid_error": pgid_error,
        }
    # SIGTERM did not work — escalate.
    ok, err = send(signal.SIGKILL)
    if not ok:
        if err == "process_lookup_error":
            # Race: died between checks.
            return {
                "status": "killed",
                "sigkill_required": False,
                "signaled": signaled,
                "pgid": pgid,
                "pgid_error": pgid_error,
            }
        return {
            "status": "failed",
            "error": err,
            "signaled": signaled,
            "pgid": pgid,
            "sigkill_attempted": True,
        }
    if _wait_for_exit(pid, sigkill_wait_seconds, killer=killer, sleeper=sleeper):
        return {
            "status": "killed",
            "sigkill_required": True,
            "signaled": signaled,
            "pgid": pgid,
            "pgid_error": pgid_error,
        }
    return {
        "status": "failed",
        "error": "alive_after_sigkill",
        "signaled": signaled,
        "pgid": pgid,
        "sigkill_required": True,
    }


def _safe_getpgid(pid: int, pgid_getter) -> tuple[int | None, str | None]:
    if pgid_getter is None:
        return None, "getpgid_unavailable"
    try:
        return pgid_getter(pid), None
    except (ProcessLookupError, PermissionError, OSError) as exc:
        return None, str(exc)


def _wait_for_exit(pid: int, timeout: float, *, killer, sleeper) -> bool:
    deadline = time.monotonic() + max(timeout, 0.0)
    while time.monotonic() < deadline:
        try:
            killer(pid, 0)
        except ProcessLookupError:
            return True
        except (PermissionError, OSError):
            return True
        sleeper(0.1)
    # Final check after the deadline elapses.
    try:
        killer(pid, 0)
    except ProcessLookupError:
        return True
    except (PermissionError, OSError):
        return True
    return False


def orphan_gate(
    *,
    fix: bool = False,
    confirm: bool = False,
    runner=subprocess.run,
    killer=os.kill,
    pg_killer=None,
    pgid_getter=None,
    sleeper=time.sleep,
    sigterm_wait_seconds: float = _SIGTERM_WAIT_SECONDS,
    sigkill_wait_seconds: float = _SIGKILL_WAIT_SECONDS,
) -> dict[str, Any]:
    if fix and not confirm:
        return {
            "ok": False,
            "gate": "orphans",
            "status": "refused",
            "reason": "fix_requires_confirm",
            "action": "re-run with --gate orphans --fix --confirm",
        }
    result = cleanup_orphan_coordinators(
        confirm=fix and confirm,
        runner=runner,
        killer=killer,
        pg_killer=pg_killer,
        pgid_getter=pgid_getter,
        sleeper=sleeper,
        sigterm_wait_seconds=sigterm_wait_seconds,
        sigkill_wait_seconds=sigkill_wait_seconds,
    )
    orphans = result.get("orphans") or []
    failed = result.get("failed") or []
    passed = not orphans if not fix else not failed
    envelope = {
        **result,
        "ok": passed,
        "gate": "orphans",
        "status": "passed" if passed else "failed",
        "fix": bool(fix),
    }
    if not fix and orphans:
        envelope["action_required"] = "re-run with --gate orphans --fix --confirm"
    return envelope


def format_cleanup_orphans(result: dict[str, Any]) -> str:
    lines = [
        f"Coordinator orphan scan @ {result.get('scanned_at')}",
        f"  scanned: {result.get('scanned', 0)} coordinator processes",
        f"  orphans: {len(result.get('orphans') or [])}",
    ]
    if result.get("dry_run"):
        lines.append("  mode: DRY-RUN (no SIGTERM sent; re-run with --confirm)")
    else:
        killed_entries = result.get("killed") or []
        escalated = sum(1 for k in killed_entries if k.get("sigkill_required"))
        lines.append(f"  killed: {len(killed_entries)}  (sigkill_required: {escalated})")
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
    "orphan_gate",
]
