from __future__ import annotations

import inspect
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import launch, runtime


PUBLIC_NAMES = [
    "DANGEROUS_LEADER_FLAGS",
    "attach_team_profile_dirs",
    "command_has_flag",
    "compile_team_dir_spec",
    "detect_inherited_dangerous_permissions",
    "effective_runtime_config",
    "ensure_agent_start_requirements",
    "init_workspace",
    "is_team_doc_dir",
    "launch",
    "process_ancestry",
    "process_info",
    "requires_direct_leader_receiver",
    "spec_team_dir",
    "tmux_session_conflict_error",
    "validate_file",
]


class LaunchBoundaryTests(unittest.TestCase):
    """Calibrated convention: identity smoke + PUBLIC_NAMES loop + behavior +
    e2e for the main orchestration symbol."""

    def test_runtime_alias_identity_smoke(self) -> None:
        self.assertIs(runtime.launch, launch.launch)
        self.assertIs(runtime.init_workspace, launch.init_workspace)
        self.assertIs(runtime.validate_file, launch.validate_file)

    def test_every_public_name_is_re_exported_on_runtime(self) -> None:
        for name in PUBLIC_NAMES:
            self.assertTrue(hasattr(launch, name), f"team_agent.launch missing {name}")
            public_attr = getattr(launch, name)
            runtime_attr = getattr(runtime, name, None) or getattr(runtime, f"_{name}", None)
            self.assertIsNotNone(runtime_attr, f"runtime missing alias for {name}")
            self.assertIs(public_attr, runtime_attr, f"runtime alias for {name} drifted")

    def test_helpers_have_explicit_signatures(self) -> None:
        for name in PUBLIC_NAMES:
            attr = getattr(launch, name)
            if not callable(attr):
                continue
            sig = inspect.signature(attr)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{name} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{name} uses **kwargs")

    def test_modules_do_not_top_level_import_runtime(self) -> None:
        for module_name in (
            "team_agent.launch.bootstrap",
            "team_agent.launch.config",
            "team_agent.launch.requirements",
            "team_agent.launch.core",
            "team_agent.launch",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class ConfigProbeTests(unittest.TestCase):
    def test_command_has_flag_word_boundary(self) -> None:
        self.assertTrue(launch.command_has_flag("claude --dangerously-skip-permissions", "--dangerously-skip-permissions"))
        # Substring of a longer flag must not match.
        self.assertFalse(launch.command_has_flag("claude --dangerously-skip-permissions-extended", "--dangerously-skip-permissions"))

    def test_dangerous_leader_flags_inventory(self) -> None:
        flags = {flag for _, flag in launch.DANGEROUS_LEADER_FLAGS}
        self.assertIn("--dangerously-skip-permissions", flags)
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", flags)

    def test_requires_direct_leader_receiver_explicit_override_wins(self) -> None:
        self.assertTrue(launch.requires_direct_leader_receiver({"agents": [{"provider": "fake"}]}, {"require_leader_receiver": True}))
        self.assertFalse(launch.requires_direct_leader_receiver({"agents": [{"provider": "claude"}]}, {"require_leader_receiver": False}))

    def test_requires_direct_leader_receiver_defaults_to_any_non_fake_agent(self) -> None:
        self.assertFalse(launch.requires_direct_leader_receiver({"agents": [{"provider": "fake"}]}, {}))
        self.assertTrue(launch.requires_direct_leader_receiver({"agents": [{"provider": "codex"}]}, {}))

    def test_effective_runtime_config_marks_explicit_config(self) -> None:
        out = launch.effective_runtime_config({"dangerous_auto_approve": True})
        self.assertEqual(out["dangerous_auto_approve_source"], "runtime_config")
        self.assertFalse(out["dangerous_auto_approve_inherited"])


class BootstrapProbeTests(unittest.TestCase):
    def test_tmux_session_conflict_error_mentions_session_name(self) -> None:
        text = launch.tmux_session_conflict_error("team-x")
        self.assertIn("team-x", text)
        self.assertIn("Use a different team name", text)

    def test_is_team_doc_dir_requires_team_md_and_agents_dir(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-isdir-") as tmp:
            team_dir = Path(tmp)
            self.assertFalse(launch.is_team_doc_dir(team_dir))
            (team_dir / "TEAM.md").write_text("# team")
            (team_dir / "agents").mkdir()
            self.assertTrue(launch.is_team_doc_dir(team_dir))

    def test_spec_team_dir_returns_team_directory_when_under_team(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-specdir-") as tmp:
            ws = Path(tmp)
            team = ws / ".team" / "alpha"
            team.mkdir(parents=True)
            spec = team / "team.spec.yaml"
            spec.touch()
            self.assertEqual(launch.spec_team_dir(spec, ws), team.resolve())

    def test_spec_team_dir_falls_back_to_current_when_outside_team(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-spec-fallback-") as tmp:
            ws = Path(tmp)
            spec = ws / "team.spec.yaml"
            spec.touch()
            self.assertEqual(launch.spec_team_dir(spec, ws), ws.resolve() / ".team" / "current")


class LaunchEndToEndProbeTests(unittest.TestCase):
    """E2E probe for the main orchestration symbol. dry_run=True exercises
    the full early-return path including routing decisions and the safety
    block but without touching tmux."""

    def test_dry_run_returns_safety_summary_without_starting_tmux(self) -> None:
        from team_agent.simple_yaml import dumps
        from team_agent.cli import _fake_spec as cli_fake_spec
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-dryrun-") as tmp:
            ws = Path(tmp)
            spec = cli_fake_spec(ws)
            spec["runtime"]["session_name"] = "team-launch-e2e"
            spec_path = ws / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            # Force a clean dangerous-permissions inheritance probe so the
            # safety block is deterministic regardless of how the test runner
            # was launched.
            with patch("team_agent.runtime.run_cmd") as run_mock, \
                 patch("team_agent.runtime._detect_inherited_dangerous_permissions", return_value={"enabled": False}):
                out = launch.launch(spec_path, dry_run=True, auto_approve=True)
            run_mock.assert_not_called()
        self.assertTrue(out["ok"])
        self.assertTrue(out["dry_run"])
        self.assertEqual(out["session_name"], "team-launch-e2e")
        self.assertIn("safety", out)
        self.assertFalse(out["safety"]["dangerous_auto_approve"])


if __name__ == "__main__":
    unittest.main()
