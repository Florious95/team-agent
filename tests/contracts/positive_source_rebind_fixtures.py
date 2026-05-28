from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from typing import Any


def write_runtime_state(workspace: Path, state: dict[str, Any]) -> Path:
    path = workspace / ".team" / "runtime" / "state.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2, sort_keys=True), encoding="utf-8")
    return path


def read_runtime_state(workspace: Path) -> dict[str, Any]:
    return json.loads((workspace / ".team" / "runtime" / "state.json").read_text(encoding="utf-8"))


def read_events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def multi_team_state(*, active: str | None = "team-b", include_ghost_top_level: bool = True) -> dict[str, Any]:
    state = {
        "active_team_key": active,
        "session_name": "team-c-shutdown" if include_ghost_top_level else "team-b-session",
        "team_dir": ".team/team-c" if include_ghost_top_level else ".team/team-b",
        "agents": {},
        "tasks": [],
        "teams": {
            "team-a": _team("team-a-session", ".team/team-a", {"a_worker": {}, "a_peer": {}}, status="alive"),
            "team-b": _team("team-b-session", ".team/team-b", {"b_worker": {}, "b_peer": {}}, status="alive"),
            "team-c": _team("team-c-shutdown", ".team/team-c", {"c_dead": {}}, status="shutdown"),
        },
    }
    return state


def shutdown_state(team_key: str = "current") -> dict[str, Any]:
    return {
        "active_team_key": team_key,
        "session_name": "team-current-session",
        "team_dir": ".team/current",
        "agents": {"worker": {"status": "running", "provider": "fake", "window": "worker"}},
        "tasks": [],
        "teams": {
            team_key: _team(
                "team-current-session",
                ".team/current",
                {"worker": {"status": "running", "provider": "fake", "window": "worker"}},
                status="alive",
            ),
            "other": _team("team-other-session", ".team/other", {"other_worker": {}}, status="alive"),
        },
        "team_owner": {
            "pane_id": "%old",
            "leader_session_uuid": "old-uuid",
            "machine_fingerprint": "old-machine",
            "provider": "claude",
            "os_user": "old-user",
        },
    }


def prepare_team_runtime_dir(workspace: Path, session_name: str, *, log_name: str = "events.jsonl") -> Path:
    path = workspace / ".team" / "runtime" / "teams" / session_name
    logs = path / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    (logs / log_name).write_text("legacy log\n", encoding="utf-8")
    return path


def tmux_available() -> bool:
    return shutil.which("tmux") is not None


class MultiPaneTmuxFixture:
    def __init__(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="ta-positive-source-tmux-")
        self.tmp_path = Path(self.tmp.name)
        self.session = f"ta-pos-src-{os.getpid()}-{int(time.time() * 1000)}"
        self.claude_bin = self._compile_sleep_binary("claude")
        self.broot_bin = self._compile_sleep_binary("broot")
        self.panes: dict[str, str] = {}

    def __enter__(self) -> "MultiPaneTmuxFixture":
        self._run(["tmux", "new-session", "-d", "-s", self.session, "-n", "leader", str(self.claude_bin)])
        self.panes["claude_active"] = self._pane_id(f"{self.session}:0.0")
        self._run(["tmux", "split-window", "-d", "-t", f"{self.session}:0", str(self.claude_bin)])
        self.panes["claude_residual"] = self._pane_id(f"{self.session}:0.1")
        self._run(["tmux", "split-window", "-d", "-t", f"{self.session}:0", str(self.broot_bin)])
        self.panes["broot"] = self._pane_id(f"{self.session}:0.2")
        self._run(["tmux", "select-pane", "-t", self.panes["claude_active"]])
        time.sleep(0.1)
        return self

    def __exit__(self, *_exc: object) -> None:
        subprocess.run(["tmux", "kill-session", "-t", self.session], text=True, capture_output=True, check=False)
        self.tmp.cleanup()

    def command_for(self, pane_id: str) -> str:
        proc = self._run(["tmux", "display-message", "-p", "-t", pane_id, "#{pane_current_command}"])
        return proc.stdout.strip()

    def _compile_sleep_binary(self, name: str) -> Path:
        if shutil.which("cc") is None:
            raise unittest.SkipTest("cc is required for tmux current_command fixture")
        source = self.tmp_path / f"{name}.c"
        binary = self.tmp_path / name
        source.write_text(
            textwrap.dedent(
                """
                #include <unistd.h>
                int main(void) {
                    sleep(600);
                    return 0;
                }
                """
            ),
            encoding="utf-8",
        )
        subprocess.run(["cc", str(source), "-o", str(binary)], text=True, capture_output=True, check=True)
        return binary

    def _pane_id(self, target: str) -> str:
        proc = self._run(["tmux", "display-message", "-p", "-t", target, "#{pane_id}"])
        return proc.stdout.strip()

    def _run(self, args: list[str]) -> subprocess.CompletedProcess[str]:
        proc = subprocess.run(args, text=True, capture_output=True, timeout=10, check=False)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or f"command failed: {args!r}")
        return proc


def _team(session_name: str, team_dir: str, agents: dict[str, Any], *, status: str) -> dict[str, Any]:
    return {
        "session_name": session_name,
        "team_dir": team_dir,
        "status": status,
        "agents": agents,
        "tasks": [],
        "leader": {"id": "leader"},
    }

