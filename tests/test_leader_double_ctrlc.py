from __future__ import annotations

import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class LeaderDoubleCtrlCTests(unittest.TestCase):
    def test_double_ctrlc_stops_team_agent_launched_leader_process_tree(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed; isolated leader wrapper signal test requires tmux")
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-ctrlc-") as tmp:
            workspace = Path(tmp) / "workspace"
            fake_bin = Path(tmp) / "bin"
            workspace.mkdir()
            fake_bin.mkdir()
            fake_codex = fake_bin / "codex"
            marker = Path(tmp) / "codex.pid"
            fake_codex.write_text(
                "#!/bin/sh\n"
                f"echo $$ > {marker}\n"
                "trap 'exit 130' INT TERM\n"
                "while true; do sleep 1; done\n",
                encoding="utf-8",
            )
            fake_codex.chmod(0o755)
            env = dict(os.environ)
            env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"
            env["PYTHONPATH"] = str(ROOT / "src")
            proc = subprocess.Popen(
                [sys.executable, "-m", "team_agent", "codex"],
                cwd=workspace,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            try:
                deadline = time.time() + 5
                while time.time() < deadline and not marker.exists() and proc.poll() is None:
                    time.sleep(0.1)
                self.assertTrue(marker.exists(), "fake provider did not start under team-agent wrapper")
                provider_pid = int(marker.read_text(encoding="utf-8").strip())
                os.killpg(proc.pid, signal.SIGINT)
                time.sleep(0.2)
                os.killpg(proc.pid, signal.SIGINT)
                deadline = time.time() + 5
                while time.time() < deadline and proc.poll() is None:
                    time.sleep(0.1)
                self.assertIsNotNone(proc.poll(), "team-agent launched leader wrapper survived double Ctrl+C")
                self.assertFalse(_pid_alive(provider_pid), "provider subprocess survived double Ctrl+C")
            finally:
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGTERM)
                _cleanup_tmux_sessions(workspace)


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def _cleanup_tmux_sessions(workspace: Path) -> None:
    proc = subprocess.run(
        ["tmux", "list-sessions", "-F", "#{session_name}"],
        text=True,
        capture_output=True,
        check=False,
    )
    for session in proc.stdout.splitlines():
        if workspace.name in session or "team-agent-leader-codex" in session:
            subprocess.run(["tmux", "kill-session", "-t", session], check=False, capture_output=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
