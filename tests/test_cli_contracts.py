from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})

class CliContractTests(unittest.TestCase):
    def test_all_help_commands(self) -> None:
        import subprocess
        import sys

        commands = [
            "codex",
            "claude",
            "quick-start",
            "init",
            "validate",
            "compile",
            "profile",
            "launch",
            "preflight",
            "start",
            "wait-ready",
            "settle",
            "status",
            "approvals",
            "peek",
            "inbox",
            "sessions",
            "attach-leader",
            "send",
            "collect",
            "diagnose",
            "repair-state",
            "validate-result",
            "doctor",
            "shutdown",
            "restart",
            "start-agent",
            "install-skill",
            "e2e",
            "allow-peer-talk",
            "advanced",
        ]
        for command in commands:
            proc = subprocess.run(
                [sys.executable, "-m", "team_agent", command, "--help"],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("usage:", proc.stdout)

    def test_top_help_is_blackbox_surface(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "--help"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn(
            "{codex,claude,quick-start,send,status,approvals,inbox,shutdown,restart,start-agent,"
            "stop-agent,reset-agent,add-agent,fork-agent,remove-agent,stuck-list,stuck-cancel,doctor}",
            proc.stdout,
        )
        self.assertNotIn("peek", proc.stdout)
        self.assertNotIn("compile", proc.stdout)
        self.assertNotIn("launch", proc.stdout)
        self.assertIn("advanced --help", proc.stdout)

    def test_leader_commands_pass_provider_flags_without_argparse_consuming_them(self) -> None:
        with patch("team_agent.runtime.start_leader") as started:
            cli.main(["codex", "--dangerously-bypass-approvals-and-sandbox", "hello"])
        self.assertEqual(started.call_args.args[0], "codex")
        self.assertEqual(started.call_args.args[1], ["--dangerously-bypass-approvals-and-sandbox", "hello"])
        with patch("team_agent.runtime.start_leader") as started:
            cli.main(["claude", "--dangerously-skip-permissions"])
        self.assertEqual(started.call_args.args[0], "claude_code")
        self.assertEqual(started.call_args.args[1], ["--dangerously-skip-permissions"])

    def test_skill_blackbox_lint(self) -> None:
        text = (ROOT / "skills" / "team-agent" / "SKILL.md").read_text(encoding="utf-8")
        required = [
            "cat > .team/current/TEAM.md",
            "cat > .team/current/agents/coder.md",
            "tools:\\n  - fs_read",
            "~/.codex/config.toml",
            "team-agent quick-start .team/current",
            "team-agent restart .",
            "team-agent start-agent",
            "team-agent approvals",
            "team-agent profile show <name> --workspace . --json",
            "session_id",
            "captured_via",
            "restart.fresh_spawn",
            "report_result",
            "AWAITING_APPROVAL",
            "Never read raw provider profile files into model context",
            ".team/runtime/provider-env/*.env",
        ]
        for item in required:
            self.assertIn(item, text)
        self.assertNotIn("team-agent peek", text)
        self.assertNotIn("team-agent peek coder", text)
        self.assertNotIn("provider: codex\nmodel: gpt-5.5\nauth_mode", text.split("cat > .team/current/TEAM.md", 1)[1].split("EOF", 1)[0])

    def test_cli_errors_are_three_part_and_logged(self) -> None:
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-cli-error-") as tmp:
            env = dict(os.environ)
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            env["PYTHONPATH"] = str(ROOT / "src")
            proc = subprocess.run(
                [sys.executable, "-m", "team_agent", "peek", "missing", "--tail", "10", "--workspace", tmp],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("raw worker terminal inspection requires explicit user authorization", proc.stderr)
            self.assertIn("error:", proc.stderr)
            self.assertIn("action:", proc.stderr)
            self.assertIn("log:", proc.stderr)

    def test_quick_start_session_conflict_payload_only_guides_rename(self) -> None:
        args = Mock(command="quick-start")
        payload = cli._cli_error_payload(
            TeamAgentRuntimeError(
                "tmux session already exists: team-same. "
                "Startup will not terminate existing tmux sessions because they may belong to active teams."
            ),
            args,
            Path("/tmp/team-agent-error.log"),
        )
        self.assertEqual(payload["reason"], "tmux_session_name_conflict")
        self.assertEqual(payload["session_name"], "team-same")
        self.assertIn("change `name:` in TEAM.md", payload["action"])
        self.assertEqual(payload["next_actions"], ["Change `name:` in TEAM.md and run `team-agent quick-start` again."])
        self.assertNotIn("team-agent shutdown", payload["action"])

    def test_send_help_shows_canonical_examples(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "send", "--help"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('team-agent send --task <task_id> --json "<message>"', proc.stdout)
        self.assertIn('team-agent send --no-ack --json <agent_id> "<message>"', proc.stdout)

    def test_send_option_order_error_hint(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "send", "blackbox_tester", "--no-ack", "--json", "message"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("options must appear before target/message", proc.stderr)

    def test_install_script_writes_working_wrappers(self) -> None:
        import os
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-install-") as tmp:
            proc = subprocess.run(
                [sys.executable, "scripts/install.py", "--prefix", tmp],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            bin_dir = Path(tmp) / "bin"
            team_agent = bin_dir / "team-agent"
            orchestrator = bin_dir / "team_orchestrator"
            self.assertTrue(team_agent.exists())
            self.assertTrue(orchestrator.exists())
            env = dict(os.environ)
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            for wrapper in [team_agent, orchestrator]:
                help_proc = subprocess.run(
                    [str(wrapper), "--help"],
                    cwd=ROOT,
                    env=env,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(help_proc.returncode, 0, help_proc.stderr)
                self.assertIn("usage:", help_proc.stdout)

    def test_npx_installer_installs_runtime_wrappers_and_skills(self) -> None:
        import os
        import subprocess
        import sys

        node = shutil.which("node")
        if not node:
            self.skipTest("node not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-npx-install-") as tmp:
            home = Path(tmp) / "home"
            prefix = Path(tmp) / "prefix"
            runtime_dir = Path(tmp) / "runtime"
            home.mkdir()
            env = dict(os.environ)
            env["HOME"] = str(home)
            env["TEAM_AGENT_PYTHON"] = sys.executable
            proc = subprocess.run(
                [
                    node,
                    "npm/install.mjs",
                    "install",
                    "--prefix",
                    str(prefix),
                    "--runtime-dir",
                    str(runtime_dir),
                ],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            team_agent = prefix / "bin" / "team-agent"
            self.assertTrue(team_agent.exists())
            self.assertTrue((home / ".codex" / "skills" / "team-agent" / "SKILL.md").exists())
            self.assertTrue((home / ".claude" / "skills" / "team-agent" / "SKILL.md").exists())
            help_proc = subprocess.run(
                [str(team_agent), "--help"],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(help_proc.returncode, 0, help_proc.stderr)
            self.assertIn("usage:", help_proc.stdout)
            uninstall = subprocess.run(
                [
                    node,
                    "npm/install.mjs",
                    "uninstall",
                    "--prefix",
                    str(prefix),
                    "--runtime-dir",
                    str(runtime_dir),
                    "--purge-runtime",
                ],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(uninstall.returncode, 0, uninstall.stderr)
            self.assertFalse(team_agent.exists())
            self.assertFalse((home / ".codex" / "skills" / "team-agent").exists())
            self.assertFalse((home / ".claude" / "skills" / "team-agent").exists())
            self.assertFalse(runtime_dir.exists())

    def test_install_skill_dry_run_json(self) -> None:
        import json as json_module
        import os
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "install-skill", "--target", "codex", "--dry-run", "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json_module.loads(proc.stdout)
        self.assertTrue(result["ok"])
        self.assertTrue(result["dry_run"])
        self.assertTrue(result["source"].endswith("skills/team-agent/SKILL.md"))

    def test_install_skill_all_dry_run_reports_both_targets(self) -> None:
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "install-skill", "--target", "all", "--dry-run", "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        self.assertTrue(result["ok"])
        destinations = [item["dest"] for item in result["targets"]]
        self.assertTrue(any("/.codex/skills/team-agent/SKILL.md" in dest for dest in destinations))
        self.assertTrue(any("/.claude/skills/team-agent/SKILL.md" in dest for dest in destinations))
        self.assertTrue(all(item["dry_run"] for item in result["targets"]))
        self.assertTrue(all(item["source"].endswith("skills/team-agent/SKILL.md") for item in result["targets"]))

    def test_validate_result_command(self) -> None:
        import json as json_module
        import os
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        envelope = _result_envelope("success")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "validate-result", json_module.dumps(envelope), "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json_module.loads(proc.stdout)
        self.assertTrue(result["ok"])
        self.assertEqual(result["task_id"], "task_impl")


if __name__ == "__main__":
    unittest.main(verbosity=2)
