from __future__ import annotations

import signal
import subprocess
import unittest
from contextlib import redirect_stdout
from io import StringIO
from unittest.mock import patch

from team_agent.cli import parser as cli_parser
from team_agent.diagnose.orphan_cleanup import orphan_gate


class Gap18DoctorGateOrphansTests(unittest.TestCase):
    def test_doctor_gate_orphans_dry_run_lists_orphans_and_fails(self) -> None:
        stdout = (
            "123 00:01 python -m team_agent.coordinator --workspace /workspaces/missing\n"
            "456 00:02 python -m team_agent.coordinator --workspace /workspaces/live\n"
        )
        killed: list[tuple[int, int]] = []

        with patch("team_agent.diagnose.orphan_cleanup.os.path.exists", side_effect=lambda path: path == "/workspaces/live"):
            result = orphan_gate(
                runner=_ps_runner(stdout),
                killer=lambda pid, sig: killed.append((pid, sig)),
                sleeper=lambda _seconds: None,
            )

        self.assertIs(result["ok"], False)
        self.assertEqual(result["status"], "failed")
        self.assertIs(result["dry_run"], True)
        self.assertEqual([entry["pid"] for entry in result["orphans"]], [123])
        self.assertEqual(result["orphans"][0]["reason"], "workspace_path_missing")
        self.assertEqual(result["action_required"], "re-run with --gate orphans --fix --confirm")
        self.assertEqual(killed, [])

    def test_doctor_gate_orphans_dry_run_passes_when_none_found(self) -> None:
        stdout = "456 00:02 python -m team_agent.coordinator --workspace /workspaces/live\n"

        with patch("team_agent.diagnose.orphan_cleanup.os.path.exists", return_value=True):
            result = orphan_gate(runner=_ps_runner(stdout), sleeper=lambda _seconds: None)

        self.assertIs(result["ok"], True)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["orphans"], [])

    def test_doctor_gate_orphans_fix_requires_confirm(self) -> None:
        result = orphan_gate(fix=True, confirm=False, runner=_ps_runner(""))

        self.assertEqual(
            result,
            {
                "ok": False,
                "gate": "orphans",
                "status": "refused",
                "reason": "fix_requires_confirm",
                "action": "re-run with --gate orphans --fix --confirm",
            },
        )

    def test_doctor_gate_orphans_fix_confirm_sigterms_orphans(self) -> None:
        stdout = "123 00:01 python -m team_agent.coordinator --workspace /workspaces/missing\n"
        calls: list[tuple[int, int]] = []

        def killer(pid: int, sig: int) -> None:
            calls.append((pid, sig))
            if sig == 0:
                raise ProcessLookupError

        with patch("team_agent.diagnose.orphan_cleanup.os.path.exists", return_value=False):
            result = orphan_gate(
                fix=True,
                confirm=True,
                runner=_ps_runner(stdout),
                killer=killer,
                sleeper=lambda _seconds: None,
            )

        self.assertIs(result["ok"], True)
        self.assertEqual(result["status"], "passed")
        self.assertEqual([entry["pid"] for entry in result["killed"]], [123])
        self.assertEqual(calls, [(123, signal.SIGTERM), (123, 0)])

    def test_doctor_gate_orphans_cli_exit_code_tracks_gate_ok(self) -> None:
        with patch(
            "team_agent.diagnose.orphan_cleanup.orphan_gate",
            return_value={"ok": False, "gate": "orphans", "status": "failed", "orphans": [{"pid": 123}]},
        ):
            buf = StringIO()
            with redirect_stdout(buf):
                with self.assertRaises(SystemExit) as ctx:
                    cli_parser.main(["doctor", "--gate", "orphans", "--json"])

        self.assertEqual(ctx.exception.code, 1)
        self.assertIn('"status": "failed"', buf.getvalue())

        with patch(
            "team_agent.diagnose.orphan_cleanup.orphan_gate",
            return_value={"ok": True, "gate": "orphans", "status": "passed", "orphans": []},
        ):
            buf = StringIO()
            with redirect_stdout(buf):
                cli_parser.main(["doctor", "--gate", "orphans", "--json"])

        self.assertIn('"status": "passed"', buf.getvalue())


def _ps_runner(stdout: str):
    def run(*args, **_kwargs):
        return subprocess.CompletedProcess(args[0] if args else [], 0, stdout=stdout, stderr="")

    return run


if __name__ == "__main__":
    unittest.main(verbosity=2)
