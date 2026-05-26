from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent import rust_core


def _completed(stdout: str = "", stderr: str = "", returncode: int = 0) -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr=stderr)


_TMUX_LIST_PANES_BASE = (
    "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tclaude.exe\t1\t4242\n"
    "%77\tteam-alpha\t0\tworker_a\t1\t/dev/ttys002\tclaude.exe\t0\t4243\n"
)


def _make_tmux_runner(tmux_stdout: str, *, env_branch):
    """Return a fake subprocess.run that routes by argv shape."""
    def fake(args, **kwargs):
        if args[:2] == ["tmux", "list-panes"]:
            return _completed(stdout=tmux_stdout)
        return env_branch(args, **kwargs)
    return fake


class PythonFallbackEnvTests(unittest.TestCase):

    def test_python_fallback_reads_macos_ps_eww_env(self) -> None:
        ps_outputs = {
            "4242": "  PID TT  STAT TIME COMMAND\n 4242 s001 S+   0:00.12 claude.exe TEAM_AGENT_LEADER_SESSION_UUID=u-leader TEAM_AGENT_LEADER_PROVIDER=claude_code TEAM_AGENT_MACHINE_FINGERPRINT=mfp\n",
            "4243": "  PID TT  STAT TIME COMMAND\n 4243 s002 S+   0:00.01 claude.exe TEAM_AGENT_ID=worker_a\n",
        }

        def env_branch(args, **_kw):
            if args[:2] == ["ps", "-E"]:
                pid = args[args.index("-p") + 1]
                return _completed(stdout=ps_outputs.get(pid, ""))
            return _completed(stderr="unexpected", returncode=1)

        with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(_TMUX_LIST_PANES_BASE, env_branch=env_branch)), \
             patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
             patch.object(rust_core.platform, "system", return_value="Darwin"):
            result = rust_core.list_targets()

        self.assertTrue(result["ok"])
        by_pane = {t["pane_id"]: t for t in result["targets"]}
        leader = by_pane["%76"]
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_SESSION_UUID"], "u-leader")
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_PROVIDER"], "claude_code")
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_MACHINE_FINGERPRINT"], "mfp")
        self.assertEqual(leader["leader_session_uuid"], "u-leader")
        worker = by_pane["%77"]
        # Worker has no leader uuid env; leader_env present but no uuid -> distinguishable from "scan failed".
        self.assertEqual(worker.get("leader_env"), {})
        self.assertNotIn("leader_session_uuid", worker)

    def test_python_fallback_reads_proc_environ(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-fake-proc-") as tmp:
            proc_dir = Path(tmp) / "proc" / "4242"
            proc_dir.mkdir(parents=True)
            environ_bytes = b"\x00".join([
                b"TEAM_AGENT_LEADER_SESSION_UUID=u-leader-linux",
                b"TEAM_AGENT_LEADER_PANE_ID=%76",
                b"PATH=/usr/bin",
            ]) + b"\x00"
            (proc_dir / "environ").write_bytes(environ_bytes)

            def fake_read_bytes(self):
                if str(self) == "/proc/4242/environ":
                    return environ_bytes
                raise FileNotFoundError(str(self))

            tmux_one = "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tclaude\t1\t4242\n"

            def env_branch(args, **_kw):
                return _completed(stderr="should not invoke ps on Linux", returncode=1)

            with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(tmux_one, env_branch=env_branch)), \
                 patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
                 patch.object(rust_core.platform, "system", return_value="Linux"), \
                 patch("pathlib.Path.read_bytes", new=fake_read_bytes):
                result = rust_core.list_targets()

        self.assertTrue(result["ok"])
        leader = result["targets"][0]
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_SESSION_UUID"], "u-leader-linux")
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_PANE_ID"], "%76")
        self.assertNotIn("PATH", leader["leader_env"])
        self.assertEqual(leader["leader_session_uuid"], "u-leader-linux")

    def test_python_fallback_populates_leader_session_uuid_when_env_present(self) -> None:
        tmux_one = "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tclaude.exe\t1\t4242\n"

        def env_branch(args, **_kw):
            if args[:2] == ["ps", "-E"]:
                return _completed(stdout="  PID TT STAT TIME COMMAND\n 4242 s001 S+ 0:00 claude.exe TEAM_AGENT_LEADER_SESSION_UUID=u-promote-me\n")
            return _completed(stderr="unexpected", returncode=1)

        with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(tmux_one, env_branch=env_branch)), \
             patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
             patch.object(rust_core.platform, "system", return_value="Darwin"):
            result = rust_core.list_targets()

        leader = result["targets"][0]
        self.assertEqual(leader["leader_session_uuid"], "u-promote-me")
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_SESSION_UUID"], "u-promote-me")

    def test_python_fallback_returns_null_leader_env_when_ps_fails(self) -> None:
        tmux_one = "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tclaude.exe\t1\t4242\n"

        def env_branch(args, **_kw):
            if args[:2] == ["ps", "-E"]:
                return _completed(stderr="ps: invalid pid", returncode=1)
            return _completed(stderr="unexpected", returncode=1)

        with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(tmux_one, env_branch=env_branch)), \
             patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
             patch.object(rust_core.platform, "system", return_value="Darwin"):
            result = rust_core.list_targets()

        self.assertTrue(result["ok"])
        leader = result["targets"][0]
        self.assertIsNone(leader["leader_env"], "scan failure must produce leader_env=None (sentinel), not raise")
        self.assertNotIn("leader_session_uuid", leader)

    def test_python_fallback_walks_child_pid_for_provider_when_shell_pane_pid_lacks_env(self) -> None:
        tmux_one = "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tzsh\t1\t9000\n"
        ps_outputs = {
            "9000": "  PID TT STAT TIME COMMAND\n 9000 s001 S+ 0:00 -zsh\n",
            "9100": "  PID TT STAT TIME COMMAND\n 9100 s001 S+ 0:00 claude.exe TEAM_AGENT_LEADER_SESSION_UUID=u-from-child TEAM_AGENT_LEADER_PROVIDER=claude_code\n",
        }
        ps_tree = "9000 1 zsh\n9100 9000 claude.exe\n9200 9100 some-helper\n9300 1 unrelated\n"

        def env_branch(args, **_kw):
            if args[:3] == ["ps", "-E", "-ww"]:
                pid = args[args.index("-p") + 1]
                return _completed(stdout=ps_outputs.get(pid, ""))
            if args[:3] == ["ps", "-o", "pid=,ppid=,comm="]:
                return _completed(stdout=ps_tree)
            return _completed(stderr="unexpected", returncode=1)

        with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(tmux_one, env_branch=env_branch)), \
             patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
             patch.object(rust_core.platform, "system", return_value="Darwin"):
            result = rust_core.list_targets()

        leader = result["targets"][0]
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_SESSION_UUID"], "u-from-child")
        self.assertEqual(leader["leader_env"]["TEAM_AGENT_LEADER_PROVIDER"], "claude_code")
        self.assertEqual(leader["leader_session_uuid"], "u-from-child")


    def test_ps_output_without_target_pid_row_returns_empty_dict_not_arbitrary_row(self) -> None:
        # Spark MEDIUM #2: lines[1] fallback could leak another process's env into our pane's
        # leader_env. _parse_ps_eww_output must return {} when the requested pid is not in the
        # output, even if other rows are present.
        ps_other_pid = (
            "  PID TT  STAT TIME COMMAND\n"
            " 9999 s001 S+   0:00.01 some-other TEAM_AGENT_LEADER_SESSION_UUID=u-WRONG-LEAKED\n"
            " 8888 s001 S+   0:00.02 unrelated TEAM_AGENT_LEADER_SESSION_UUID=u-also-WRONG\n"
        )
        parsed = rust_core._parse_ps_eww_output(ps_other_pid, "4242")
        self.assertEqual(parsed, {}, "must not leak env from unrelated PID rows")

        # End-to-end through list_targets: ps returns rows for OTHER pids, our 4242 pane scan
        # must produce leader_env={} (scanned, no marker found) — not the leaked uuid.
        tmux_one = "%76\tteam-alpha\t0\tleader\t0\t/dev/ttys001\tclaude.exe\t1\t4242\n"

        def env_branch(args, **_kw):
            if args[:3] == ["ps", "-E", "-ww"]:
                return _completed(stdout=ps_other_pid)
            if args[:3] == ["ps", "-o", "pid=,ppid=,comm="]:
                return _completed(stdout="")  # no children
            return _completed(stderr="unexpected", returncode=1)

        with patch.object(rust_core, "_run_subprocess", side_effect=_make_tmux_runner(tmux_one, env_branch=env_branch)), \
             patch.object(rust_core, "call_core", return_value={"ok": False, "error": "binary not found"}), \
             patch.object(rust_core.platform, "system", return_value="Darwin"):
            result = rust_core.list_targets()

        leader = result["targets"][0]
        self.assertEqual(leader["leader_env"], {}, leader)
        self.assertNotIn("leader_session_uuid", leader, "must NOT promote a leaked uuid from an unrelated PID")


if __name__ == "__main__":
    unittest.main(verbosity=2)
