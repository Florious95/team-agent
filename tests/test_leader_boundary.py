from __future__ import annotations

import inspect
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import leader, runtime


PUBLIC_NAMES = [
    "attach_leader",
    "attach_leader_to_state",
    "leader_session_name",
    "leader_start_plan",
    "start_leader",
]


class LeaderBoundaryTests(unittest.TestCase):
    """Calibrated convention: identity smoke + PUBLIC_NAMES loop + behavior
    + e2e for the main orchestration symbol (leader_start_plan covers the
    full plan-builder path; attach_leader_to_state covers the receiver
    plumbing)."""

    def test_runtime_alias_identity_smoke(self) -> None:
        self.assertIs(runtime.attach_leader, leader.attach_leader)
        self.assertIs(runtime.leader_start_plan, leader.leader_start_plan)

    def test_every_public_name_is_re_exported_on_runtime(self) -> None:
        for name in PUBLIC_NAMES:
            self.assertTrue(hasattr(leader, name), f"team_agent.leader missing {name}")
            public_attr = getattr(leader, name)
            runtime_attr = getattr(runtime, name, None) or getattr(runtime, f"_{name}", None)
            self.assertIsNotNone(runtime_attr, f"runtime missing alias for {name}")
            self.assertIs(public_attr, runtime_attr, f"runtime alias for {name} drifted")

    def test_helpers_have_explicit_signatures(self) -> None:
        for name in PUBLIC_NAMES:
            fn = getattr(leader, name)
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{name} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{name} uses **kwargs")

    def test_leader_module_does_not_top_level_import_runtime(self) -> None:
        source = inspect.getsource(leader)
        for line in source.splitlines():
            if not line or line.startswith((" ", "\t")):
                continue
            self.assertFalse(
                line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                f"team_agent.leader top-level imports runtime: {line!r}",
            )


class LeaderSessionNameProbeTests(unittest.TestCase):
    def test_session_name_is_stable_for_same_workspace_and_provider(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-name-") as tmp:
            ws = Path(tmp)
            a = leader.leader_session_name("codex", ws)
            b = leader.leader_session_name("codex", ws)
            self.assertEqual(a, b)
            self.assertTrue(a.startswith("team-agent-leader-codex-"))

    def test_session_name_differs_per_provider(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-name-prov-") as tmp:
            ws = Path(tmp)
            self.assertNotEqual(
                leader.leader_session_name("codex", ws),
                leader.leader_session_name("claude", ws),
            )


class LeaderStartPlanProbeTests(unittest.TestCase):
    """E2E for the main orchestration symbol leader_start_plan: three
    branches (exec_provider when TMUX is set, attach_existing when the
    tmux session already exists, new_tmux_session otherwise)."""

    def test_exec_provider_branch_when_tmux_env_set(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-exec-") as tmp:
            ws = Path(tmp)
            adapter = Mock(command_name="codex", is_installed=lambda: True)
            with patch("team_agent.runtime.get_adapter", return_value=adapter), \
                 patch.dict(os.environ, {"TMUX": "/tmp/tmux-1.sock,1,0"}, clear=False):
                plan = leader.leader_start_plan("codex", ["--foo"], ws)
        self.assertEqual(plan["mode"], "exec_provider")
        self.assertEqual(plan["argv"], ["codex", "--foo"])

    def test_attach_existing_branch_when_session_present(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-attach-") as tmp:
            ws = Path(tmp)
            adapter = Mock(command_name="claude", is_installed=lambda: True)
            with patch("team_agent.runtime.get_adapter", return_value=adapter), \
                 patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"), \
                 patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch.dict(os.environ, {}, clear=True):
                plan = leader.leader_start_plan("claude", [], ws)
        self.assertEqual(plan["mode"], "attach_existing")
        self.assertEqual(plan["argv"][:3], ["tmux", "attach-session", "-t"])

    def test_new_tmux_session_branch_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-new-") as tmp:
            ws = Path(tmp)
            adapter = Mock(command_name="codex", is_installed=lambda: True)
            with patch("team_agent.runtime.get_adapter", return_value=adapter), \
                 patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"), \
                 patch("team_agent.runtime._tmux_session_exists", return_value=False), \
                 patch.dict(os.environ, {}, clear=True):
                plan = leader.leader_start_plan("codex", [], ws)
        self.assertEqual(plan["mode"], "new_tmux_session")
        self.assertEqual(plan["argv"][0], "tmux")
        self.assertEqual(plan["argv"][1], "new-session")

    def test_missing_provider_raises(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-missing-") as tmp:
            ws = Path(tmp)
            adapter = Mock(command_name="codex", is_installed=lambda: False)
            with patch("team_agent.runtime.get_adapter", return_value=adapter), \
                 patch.dict(os.environ, {}, clear=True):
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    leader.leader_start_plan("codex", [], ws)
        self.assertIn("not found", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
