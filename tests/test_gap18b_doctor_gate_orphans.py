from __future__ import annotations

import signal
import subprocess
from unittest.mock import patch

import pytest

from team_agent.cli import parser as cli_parser
from team_agent.diagnose.orphan_cleanup import orphan_gate


def _ps_runner(stdout: str):
    def run(*_args, **_kwargs):
        return subprocess.CompletedProcess(_args[0] if _args else [], 0, stdout=stdout, stderr="")

    return run


def test_doctor_gate_orphans_dry_run_lists_orphans_and_fails() -> None:
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

    assert result["ok"] is False
    assert result["status"] == "failed"
    assert result["dry_run"] is True
    assert [entry["pid"] for entry in result["orphans"]] == [123]
    assert result["orphans"][0]["reason"] == "workspace_path_missing"
    assert result["action_required"] == "re-run with --gate orphans --fix --confirm"
    assert killed == []


def test_doctor_gate_orphans_dry_run_passes_when_none_found() -> None:
    stdout = "456 00:02 python -m team_agent.coordinator --workspace /workspaces/live\n"

    with patch("team_agent.diagnose.orphan_cleanup.os.path.exists", return_value=True):
        result = orphan_gate(runner=_ps_runner(stdout), sleeper=lambda _seconds: None)

    assert result["ok"] is True
    assert result["status"] == "passed"
    assert result["orphans"] == []


def test_doctor_gate_orphans_fix_requires_confirm() -> None:
    result = orphan_gate(fix=True, confirm=False, runner=_ps_runner(""))

    assert result == {
        "ok": False,
        "gate": "orphans",
        "status": "refused",
        "reason": "fix_requires_confirm",
        "action": "re-run with --gate orphans --fix --confirm",
    }


def test_doctor_gate_orphans_fix_confirm_sigterms_orphans() -> None:
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

    assert result["ok"] is True
    assert result["status"] == "passed"
    assert [entry["pid"] for entry in result["killed"]] == [123]
    assert calls == [(123, signal.SIGTERM), (123, 0)]


def test_doctor_gate_orphans_cli_exit_code_tracks_gate_ok(capsys) -> None:
    with patch(
        "team_agent.diagnose.orphan_cleanup.orphan_gate",
        return_value={"ok": False, "gate": "orphans", "status": "failed", "orphans": [{"pid": 123}]},
    ):
        with pytest.raises(SystemExit) as exc:
            cli_parser.main(["doctor", "--gate", "orphans", "--json"])

    assert exc.value.code == 1
    assert '"status": "failed"' in capsys.readouterr().out

    with patch(
        "team_agent.diagnose.orphan_cleanup.orphan_gate",
        return_value={"ok": True, "gate": "orphans", "status": "passed", "orphans": []},
    ):
        cli_parser.main(["doctor", "--gate", "orphans", "--json"])

    assert '"status": "passed"' in capsys.readouterr().out
