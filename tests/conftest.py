"""Stage 14 (Gap 37b) — pytest fixture teardown to reap coordinators spawned during tests.

Mac mini 2026-05-26 evidence: 35 orphan coordinator processes alive simultaneously, all
pointing at /var/folders/.../T/team-agent-watcher-dedupe-* paths that no longer exist.
The test harness somewhere spawns a real coordinator subprocess (via runtime.start_coordinator
→ subprocess.Popen of `team_agent.coordinator --workspace <tmp>`) and never reaps it.
The tmpdir gets cleaned up at fixture teardown but the subprocess hangs around indefinitely
until SIGKILL by a human.

This conftest does two things:
  (1) Wraps subprocess.Popen at session start to record every PID that gets spawned with
      `team_agent.coordinator` in argv. Tests don't need to know.
  (2) At session end, sends SIGTERM to every recorded PID still alive.

Belt-and-braces with Gap 37b's in-coordinator ppid self-check (coordinator/__main__.py).
"""
from __future__ import annotations

import atexit
import os
import signal
import subprocess
import sys
import time

_TRACKED_COORDINATOR_PIDS: set[int] = set()
_ORIGINAL_POPEN = subprocess.Popen


def _is_coordinator_cmdline(argv: object) -> bool:
    if isinstance(argv, (list, tuple)):
        return any("team_agent.coordinator" in str(item) for item in argv)
    if isinstance(argv, str):
        return "team_agent.coordinator" in argv
    return False


class _TrackingPopen(_ORIGINAL_POPEN):  # type: ignore[misc, valid-type]
    def __init__(self, args, *a, **kw):  # type: ignore[no-untyped-def]
        super().__init__(args, *a, **kw)
        try:
            if _is_coordinator_cmdline(args):
                _TRACKED_COORDINATOR_PIDS.add(self.pid)
        except Exception:
            pass


subprocess.Popen = _TrackingPopen  # type: ignore[assignment, misc]


def _reap_tracked_coordinators() -> None:
    """SIGTERM every coordinator we recorded; wait briefly; SIGKILL stragglers."""
    if not _TRACKED_COORDINATOR_PIDS:
        return
    for pid in list(_TRACKED_COORDINATOR_PIDS):
        try:
            os.kill(pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError, OSError):
            _TRACKED_COORDINATOR_PIDS.discard(pid)
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline and _TRACKED_COORDINATOR_PIDS:
        for pid in list(_TRACKED_COORDINATOR_PIDS):
            try:
                os.kill(pid, 0)
            except (ProcessLookupError, PermissionError, OSError):
                _TRACKED_COORDINATOR_PIDS.discard(pid)
        time.sleep(0.1)
    for pid in list(_TRACKED_COORDINATOR_PIDS):
        try:
            os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            pass
        _TRACKED_COORDINATOR_PIDS.discard(pid)


atexit.register(_reap_tracked_coordinators)


# pytest-style session teardown hook. Active when run under pytest; harmless under unittest
# because the function is just an atexit handler that we also expose for explicit calls.
def pytest_sessionfinish(session, exitstatus):  # type: ignore[no-untyped-def]
    _reap_tracked_coordinators()


def _explicit_reap_tracked_coordinators_for_tests() -> set[int]:
    """Test-only accessor — returns the PIDs that were recorded so tests can assert the
    reaper sees them."""
    return set(_TRACKED_COORDINATOR_PIDS)
