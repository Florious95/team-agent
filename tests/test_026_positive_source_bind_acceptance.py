from __future__ import annotations

import ast
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

try:
    from .contracts.positive_source_rebind_fixtures import (
        MultiPaneTmuxFixture,
        read_events,
        read_runtime_state,
        shutdown_state,
        tmux_available,
        write_runtime_state,
    )
except ImportError:
    from contracts.positive_source_rebind_fixtures import (
        MultiPaneTmuxFixture,
        read_events,
        read_runtime_state,
        shutdown_state,
        tmux_available,
        write_runtime_state,
    )


class PositiveSourceBindAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="ta-pos-bind-")
        self.workspace = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_c1_takeover_claim_leader_and_quick_start_share_bind_owner_from_caller_pane(self) -> None:
        runtime_tree = ast.parse((_repo_root() / "src/team_agent/runtime.py").read_text(encoding="utf-8"))
        commands_tree = ast.parse((_repo_root() / "src/team_agent/cli/commands.py").read_text(encoding="utf-8"))

        runtime_functions = _top_level_functions(runtime_tree)
        commands_functions = _top_level_functions(commands_tree)
        for entry in ("takeover", "claim_leader"):
            self.assertTrue(
                _function_calls_name(runtime_functions[entry], "bind_owner_from_caller_pane"),
                f"runtime.{entry} must call bind_owner_from_caller_pane",
            )
        self.assertTrue(
            _function_calls_name(runtime_functions["quick_start"], "bind_owner_from_caller_pane"),
            "runtime.quick_start must carry the shared bind_owner_from_caller_pane gate",
        )
        self.assertTrue(_function_calls_name(commands_functions["cmd_quick_start"], "quick_start"))

    def test_c2_bind_owner_uses_only_tmux_pane_env_as_source(self) -> None:
        bind_owner = _bind_owner_from_caller_pane(self)
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 5) -> SimpleNamespace:
            calls.append(args)
            if args[:2] == ["tmux", "display-message"]:
                self.assertIn("-t", args)
                self.assertEqual(args[args.index("-t") + 1], "%132")
                return SimpleNamespace(returncode=0, stdout="claude\n", stderr="")
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        run_cmd = Mock(side_effect=fake_run_cmd)
        patch_target = f"{bind_owner.__module__}.run_cmd"
        self.assertIs(bind_owner.__globals__["run_cmd"], __import__(bind_owner.__module__, fromlist=["run_cmd"]).run_cmd)
        with patch.dict(os.environ, {"TMUX_PANE": "%132"}, clear=False), \
             patch(patch_target, run_cmd):
            bind_owner(self.workspace, "current", override_uuid="uuid-caller")

        self.assertGreater(run_cmd.call_count, 0, "contract must patch the run_cmd symbol bind_owner actually calls")
        flat = [part for call in calls for part in call]
        self.assertLessEqual(len(calls), 2)
        self.assertNotIn("list-panes", flat)
        self.assertNotIn("list-windows", flat)
        self.assertNotIn("list-clients", flat)

    def test_c3_missing_tmux_pane_refuses_without_reverse_scan(self) -> None:
        bind_owner = _bind_owner_from_caller_pane(self)
        with patch.dict(os.environ, {}, clear=True):
            result = bind_owner(self.workspace, "current")
        self.assertFalse(result["ok"])
        self.assertEqual(result["reason"], "caller_pane_missing")
        self.assertIn("leader pane", result["hint"])
        events = read_events(self.workspace)
        self.assertTrue(any(event.get("event") == "owner.bind_refused" and event.get("reason") == "caller_pane_missing" for event in events))

    @unittest.skipUnless(tmux_available(), "tmux required for real caller-pane fixture")
    def test_c4_non_provider_command_is_accepted_as_caller_pane_identity(self) -> None:
        bind_owner = _bind_owner_from_caller_pane(self)
        state_path = write_runtime_state(self.workspace, shutdown_state())
        before = state_path.read_text(encoding="utf-8")
        with MultiPaneTmuxFixture() as fixture:
            broot = fixture.panes["broot"]
            self.assertEqual(fixture.command_for(broot), "broot")
            with patch.dict(os.environ, {"TMUX_PANE": broot}, clear=False):
                result = bind_owner(self.workspace, "current")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["caller_pane_id"], broot)
        self.assertEqual(result["caller_current_command"], "broot")
        self.assertEqual(state_path.read_text(encoding="utf-8"), before)

    @unittest.skipUnless(tmux_available(), "tmux required for real caller-pane fixture")
    def test_c5_takeover_force_writes_all_owner_fields_from_caller_and_is_idempotent(self) -> None:
        from team_agent import runtime

        write_runtime_state(self.workspace, shutdown_state())
        with MultiPaneTmuxFixture() as fixture:
            pane = fixture.panes["claude_active"]
            env = {
                "TMUX_PANE": pane,
                "TEAM_AGENT_LEADER_PANE_ID": pane,
                "TEAM_AGENT_LEADER_PROVIDER": "claude",
                "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": "uuid-new-caller",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "machine-new",
                "USER": "user-new",
            }
            with patch.dict(os.environ, env, clear=False):
                first = runtime.takeover(self.workspace, team="current", confirm=True)
                after_first = read_runtime_state(self.workspace)
                second = runtime.takeover(self.workspace, team="current", confirm=True)
                after_second = read_runtime_state(self.workspace)

        self.assertTrue(first["ok"])
        self.assertTrue(second["ok"])
        owner = after_first["teams"]["current"]["team_owner"]
        self.assertEqual(owner["leader_session_uuid"], "uuid-new-caller")
        self.assertEqual(owner["pane_id"], pane)
        self.assertEqual(owner["machine_fingerprint"], "machine-new")
        self.assertEqual(owner["provider"], "claude")
        self.assertNotIn("old-uuid", str(owner))
        self.assertEqual(after_first, after_second)

    def test_c25_tmux_mock_observes_no_enumeration_and_target_is_tmux_pane(self) -> None:
        bind_owner = _bind_owner_from_caller_pane(self)
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 5) -> SimpleNamespace:
            calls.append(args)
            command = args[1] if len(args) > 1 else ""
            self.assertNotIn(command, {"list-panes", "list-windows", "list-clients"})
            if "-t" in args:
                self.assertEqual(args[args.index("-t") + 1], "%caller")
            return SimpleNamespace(returncode=0, stdout="codex\n", stderr="")

        run_cmd = Mock(side_effect=fake_run_cmd)
        patch_target = f"{bind_owner.__module__}.run_cmd"
        self.assertIs(bind_owner.__globals__["run_cmd"], __import__(bind_owner.__module__, fromlist=["run_cmd"]).run_cmd)
        with patch.dict(os.environ, {"TMUX_PANE": "%caller"}, clear=False), \
             patch(patch_target, run_cmd):
            bind_owner(self.workspace, "current", override_uuid="uuid-caller")

        self.assertGreater(run_cmd.call_count, 0, "contract must patch the run_cmd symbol bind_owner actually calls")
        self.assertLessEqual(len(calls), 2)


def _bind_owner_from_caller_pane(testcase: unittest.TestCase):
    try:
        from team_agent.leader_binding import bind_owner_from_caller_pane
    except Exception as exc:  # noqa: BLE001 - missing API is the RED contract.
        testcase.fail(f"missing public bind_owner_from_caller_pane API: {exc}")
    return bind_owner_from_caller_pane


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def _top_level_functions(tree: ast.AST) -> dict[str, ast.FunctionDef]:
    return {
        node.name: node
        for node in getattr(tree, "body", [])
        if isinstance(node, ast.FunctionDef)
    }


def _function_calls_name(function: ast.FunctionDef, name: str) -> bool:
    for node in ast.walk(function):
        if not isinstance(node, ast.Call):
            continue
        callee = node.func
        if isinstance(callee, ast.Name) and callee.id == name:
            return True
        if isinstance(callee, ast.Attribute) and callee.attr == name:
            return True
    return False


if __name__ == "__main__":
    unittest.main(verbosity=2)
