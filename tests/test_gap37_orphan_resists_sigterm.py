"""Gap 37 escalation regression — orphan coordinator resists SIGTERM.

Mac mini SC-C (2026-05-26) observed a real orphan coordinator surviving SIGTERM
indefinitely; team-agent doctor --gate orphans --fix --confirm marked it
status='failed' error='still_alive_after_sigterm' and stopped. Real production
needs SIGTERM -> wait 3s -> SIGKILL -> wait 2s escalation; only "alive after
SIGKILL" should ever be reported as a hard failure.

The harness mocks:
  - find_coordinator_processes -> one orphan
  - os.kill -> raises ProcessLookupError ONLY when the test wants the process to
    be considered dead; otherwise returns None (alive). After SIGKILL is sent
    we flip the dead flag so subsequent kill(pid, 0) probes report missing.

Plus a process-group case: when getpgid returns a pgid != pid (process leads
its own group with children), killpg is used and the test asserts that path.
"""
from __future__ import annotations

import os
import signal
import unittest
from typing import Any

from team_agent.diagnose import orphan_cleanup as oc


def _orphan_entries(pid: int = 4242, workspace: str = "/tmp/team-agent-watcher-dedupe-stale") -> list[dict[str, Any]]:
    return [{
        "pid": pid,
        "etime": "40:23:11",
        "cmdline": f"python -m team_agent.coordinator --workspace {workspace}",
        "workspace": workspace,
    }]


class _ProcessGoneAfter:
    """Fake os.kill that reports the pid alive until `dies_after_sig` is sent;
    subsequent kill(pid, 0) probes raise ProcessLookupError (process gone)."""
    def __init__(self, *, dies_after_sig: int | None = signal.SIGKILL):
        self.dies_after_sig = dies_after_sig
        self.dead = False
        self.calls: list[tuple[int, int]] = []

    def __call__(self, pid: int, sig: int) -> None:
        self.calls.append((pid, sig))
        if sig == 0:
            if self.dead:
                raise ProcessLookupError(f"No such process: {pid}")
            return None
        # Real signal — if this is the one that "kills" the fake process,
        # flip the flag so future kill(pid, 0) reports gone.
        if self.dies_after_sig is None:
            return None
        if sig == self.dies_after_sig:
            self.dead = True


class Gap37OrphanResistsSigtermTests(unittest.TestCase):

    def test_orphan_that_ignores_sigterm_exits_on_sigkill_and_reports_escalation(self) -> None:
        """Core regression: orphan survives SIGTERM (still_alive_after_sigterm
        was the production symptom), then SIGKILL terminates it. The result
        envelope must report status='killed' and sigkill_required=True so audit
        shows the escalation actually fired."""
        orphans = _orphan_entries()
        killer = _ProcessGoneAfter(dies_after_sig=signal.SIGKILL)

        def fake_finder(*, runner):
            return orphans

        # Force pid-only path (no process group).
        def fake_pgid(pid: int) -> int:
            return pid

        original_finder = oc.find_coordinator_processes
        oc.find_coordinator_processes = fake_finder  # type: ignore[assignment]
        try:
            result = oc.cleanup_orphan_coordinators(
                confirm=True,
                killer=killer,
                pgid_getter=fake_pgid,
                # pg_killer left default; not invoked because pgid == pid.
                sleeper=lambda _seconds: None,
                sigterm_wait_seconds=0.01,
                sigkill_wait_seconds=0.01,
            )
        finally:
            oc.find_coordinator_processes = original_finder  # type: ignore[assignment]

        self.assertEqual(len(result["killed"]), 1)
        self.assertEqual(len(result["failed"]), 0)
        killed = result["killed"][0]
        self.assertTrue(killed["sigkill_required"],
            f"escalation flag missing; killed entry was {killed}")
        self.assertEqual(killed["status"], "killed")
        self.assertEqual(killed["signaled"], "pid")
        # Signal sequence: SIGTERM probes alive (returns 0), then SIGKILL fires.
        sigs = [sig for (_pid, sig) in killer.calls if sig in {signal.SIGTERM, signal.SIGKILL}]
        self.assertEqual(sigs[0], signal.SIGTERM)
        self.assertEqual(sigs[-1], signal.SIGKILL)

    def test_orphan_that_exits_on_sigterm_does_not_escalate(self) -> None:
        """Happy path baseline: a well-behaved process exits on SIGTERM and the
        result must report sigkill_required=False (no escalation needed)."""
        orphans = _orphan_entries(pid=5555)
        killer = _ProcessGoneAfter(dies_after_sig=signal.SIGTERM)

        def fake_finder(*, runner):
            return orphans

        def fake_pgid(pid: int) -> int:
            return pid

        original_finder = oc.find_coordinator_processes
        oc.find_coordinator_processes = fake_finder  # type: ignore[assignment]
        try:
            result = oc.cleanup_orphan_coordinators(
                confirm=True,
                killer=killer,
                pgid_getter=fake_pgid,
                sleeper=lambda _seconds: None,
                sigterm_wait_seconds=0.01,
                sigkill_wait_seconds=0.01,
            )
        finally:
            oc.find_coordinator_processes = original_finder  # type: ignore[assignment]

        self.assertEqual(len(result["killed"]), 1)
        killed = result["killed"][0]
        self.assertFalse(killed["sigkill_required"])
        sigs = [sig for (_pid, sig) in killer.calls if sig in {signal.SIGTERM, signal.SIGKILL}]
        self.assertEqual(sigs, [signal.SIGTERM],
            "SIGKILL must not be sent when SIGTERM works")

    def test_orphan_with_process_group_uses_killpg_path(self) -> None:
        """When the orphan leads its own process group (getpgid returns a value
        != pid), the cleaner must signal the WHOLE group via killpg, otherwise
        any subprocess.Popen children survive SIGTERM."""
        orphans = _orphan_entries(pid=7777)
        killer = _ProcessGoneAfter(dies_after_sig=signal.SIGTERM)
        pg_calls: list[tuple[int, int]] = []

        def fake_pg_killer(pgid: int, sig: int) -> None:
            pg_calls.append((pgid, sig))
            if sig == signal.SIGTERM:
                # Group dies on SIGTERM → mark pid dead so the cleaner's
                # wait_for_exit probe sees it gone.
                killer.dead = True

        def fake_pgid(pid: int) -> int:
            return 7000  # different from pid → process-group path

        def fake_finder(*, runner):
            return orphans

        original_finder = oc.find_coordinator_processes
        oc.find_coordinator_processes = fake_finder  # type: ignore[assignment]
        try:
            result = oc.cleanup_orphan_coordinators(
                confirm=True,
                killer=killer,
                pg_killer=fake_pg_killer,
                pgid_getter=fake_pgid,
                sleeper=lambda _seconds: None,
                sigterm_wait_seconds=0.01,
                sigkill_wait_seconds=0.01,
            )
        finally:
            oc.find_coordinator_processes = original_finder  # type: ignore[assignment]

        self.assertEqual(len(result["killed"]), 1)
        killed = result["killed"][0]
        self.assertEqual(killed["signaled"], "pgid",
            "process-group path must signal pgid, not pid")
        self.assertEqual(killed["pgid"], 7000)
        # killpg called exactly once with SIGTERM (no escalation needed).
        self.assertEqual(pg_calls, [(7000, signal.SIGTERM)])

    def test_orphan_that_survives_both_signals_reports_failed(self) -> None:
        """The rare 'alive after SIGKILL' case (zombie / kernel UN-interruptible
        sleep) must be reported as status='failed' with the exact error string
        the operator runbook keys on."""
        orphans = _orphan_entries(pid=9999)
        killer = _ProcessGoneAfter(dies_after_sig=None)  # never dies

        def fake_pgid(pid: int) -> int:
            return pid

        def fake_finder(*, runner):
            return orphans

        original_finder = oc.find_coordinator_processes
        oc.find_coordinator_processes = fake_finder  # type: ignore[assignment]
        try:
            result = oc.cleanup_orphan_coordinators(
                confirm=True,
                killer=killer,
                pgid_getter=fake_pgid,
                sleeper=lambda _seconds: None,
                sigterm_wait_seconds=0.01,
                sigkill_wait_seconds=0.01,
            )
        finally:
            oc.find_coordinator_processes = original_finder  # type: ignore[assignment]

        self.assertEqual(len(result["killed"]), 0)
        self.assertEqual(len(result["failed"]), 1)
        failed = result["failed"][0]
        self.assertEqual(failed["error"], "alive_after_sigkill")
        self.assertTrue(failed["sigkill_required"])

    def test_format_cleanup_orphans_reports_sigkill_required_count(self) -> None:
        """The CLI summary line must surface how many kills required SIGKILL,
        so operators see escalation pressure at a glance."""
        result = {
            "scanned_at": "2026-05-26T00:00:00+00:00",
            "scanned": 2,
            "orphans": _orphan_entries(pid=1) + _orphan_entries(pid=2),
            "killed": [
                {"pid": 1, "etime": "1:00", "workspace": "/tmp/x", "reason": "x",
                 "sigkill_required": False},
                {"pid": 2, "etime": "2:00", "workspace": "/tmp/y", "reason": "y",
                 "sigkill_required": True},
            ],
            "failed": [],
            "dry_run": False,
        }
        text = oc.format_cleanup_orphans(result)
        self.assertIn("killed: 2", text)
        self.assertIn("sigkill_required: 1", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
