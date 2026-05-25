from __future__ import annotations

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from typing import Any, Callable
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.mcp_server.tools import TeamOrchestratorTools
from team_agent.simple_yaml import dumps
from team_agent.state import load_runtime_state, save_runtime_state

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


OWNER = {"pane_id": "%owner", "provider": "codex", "machine_fingerprint": "machine-a"}
INTRUDER_ENV = {
    "TEAM_AGENT_LEADER_PANE_ID": "%intruder",
    "TEAM_AGENT_LEADER_PROVIDER": "codex",
    "TEAM_AGENT_MACHINE_FINGERPRINT": "machine-b",
    "TEAM_AGENT_ID": "leader",
}


class TeamOwnerGateTests(unittest.TestCase):
    def test_non_owner_mutators_refuse_cleanly(self) -> None:
        actions: dict[str, Callable[[Path], dict[str, Any]]] = {
            "send_message": lambda ws: TeamOrchestratorTools(ws).send_message("fake_impl", "hello", sender="leader"),
            "stop_agent": lambda ws: TeamOrchestratorTools(ws).stop_agent("fake_impl"),
            "reset_agent": lambda ws: TeamOrchestratorTools(ws).reset_agent("fake_impl", discard_session=True),
            "add_agent": lambda ws: TeamOrchestratorTools(ws).add_agent("new_worker", "new_worker.md"),
            "remove_agent": lambda ws: runtime.remove_agent(ws, "fake_impl", from_spec=True, confirm=True),
            "shutdown": lambda ws: runtime.shutdown(ws),
            "restart": lambda ws: runtime.restart(ws),
            "acknowledge_idle": lambda ws: getattr(runtime, "acknowledge_idle", lambda *_a, **_kw: {"ok": False, "reason": "mutator_missing"})(ws, "fake_impl"),
            "stuck_cancel": lambda ws: TeamOrchestratorTools(ws).stuck_cancel("fake_impl"),
        }
        for name, action in actions.items():
            with self.subTest(name=name):
                with tempfile.TemporaryDirectory(prefix=f"team-agent-owner-{name}-") as tmp:
                    workspace = _owner_workspace(Path(tmp))
                    with (
                        patch.dict(os.environ, INTRUDER_ENV, clear=False),
                        patch("team_agent.runtime._deliver_pending_message", return_value={"ok": True, "status": "queued", "queued": True}),
                        patch("team_agent.runtime.start_agent", return_value={"ok": True, "status": "running"}),
                        patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}),
                        patch("team_agent.runtime.run_cmd", side_effect=_fake_tmux_run),
                    ):
                        try:
                            result = action(workspace)
                        except Exception as exc:  # clean owner gate must return, not explode after partial mutation
                            result = {"ok": False, "reason": type(exc).__name__, "error": str(exc)}
                    self.assertFalse(result.get("ok"), result)
                    self.assertEqual(result.get("reason"), "team_owner_mismatch", result)

    def test_recorded_owner_can_send_without_ownership_refusal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-owner-ok-") as tmp:
            workspace = _owner_workspace(Path(tmp))
            owner_env = {
                "TEAM_AGENT_LEADER_PANE_ID": OWNER["pane_id"],
                "TEAM_AGENT_LEADER_PROVIDER": OWNER["provider"],
                "TEAM_AGENT_MACHINE_FINGERPRINT": OWNER["machine_fingerprint"],
                "TEAM_AGENT_ID": "leader",
            }
            with (
                patch.dict(os.environ, owner_env, clear=False),
                patch("team_agent.runtime._deliver_pending_message", return_value={"ok": True, "status": "queued", "queued": True}),
            ):
                result = TeamOrchestratorTools(workspace).send_message("fake_impl", "owner send", sender="leader")
            self.assertNotEqual(result.get("reason"), "team_owner_mismatch")

    def test_takeover_with_team_targets_team_level_state_and_preserves_siblings(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-takeover-team-") as tmp:
            workspace = Path(tmp)
            alpha_owner = {"pane_id": "%alpha", "provider": "codex", "machine_fingerprint": "machine-a"}
            beta_owner = {"pane_id": "%beta", "provider": "codex", "machine_fingerprint": "machine-b"}
            spec = _fake_spec(workspace)
            alpha_team_dir = workspace / ".team" / "alpha"
            beta_team_dir = workspace / ".team" / "beta"
            alpha_team_dir.mkdir(parents=True)
            beta_team_dir.mkdir(parents=True)
            alpha_spec_path = alpha_team_dir / "team.spec.yaml"
            beta_spec_path = beta_team_dir / "team.spec.yaml"
            alpha_spec_path.write_text(dumps(spec), encoding="utf-8")
            beta_spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(alpha_spec_path),
                    "team_dir": str(alpha_team_dir),
                    "workspace": str(workspace),
                    "session_name": "team-alpha",
                    "team_owner": alpha_owner,
                    "agents": {"alpha_worker": {"status": "running", "provider": "fake", "window": "alpha_worker"}},
                    "tasks": spec["tasks"],
                    "teams": {
                        "alpha": {
                            "spec_path": str(alpha_spec_path),
                            "team_dir": str(alpha_team_dir),
                            "session_name": "team-alpha",
                            "team_owner": alpha_owner,
                            "agents": {"alpha_worker": {"status": "running", "provider": "fake", "window": "alpha_worker"}},
                        },
                        "beta": {
                            "spec_path": str(beta_spec_path),
                            "team_dir": str(beta_team_dir),
                            "session_name": "team-beta",
                            "team_owner": beta_owner,
                            "agents": {"beta_worker": {"status": "running", "provider": "fake", "window": "beta_worker"}},
                        },
                    },
                },
            )
            claimant_env = {
                "TEAM_AGENT_LEADER_PANE_ID": "%claimant",
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "machine-c",
                "TEAM_AGENT_ID": "leader",
            }
            with patch.dict(os.environ, claimant_env, clear=False):
                result = runtime.takeover(workspace, team="beta", confirm=True)
            self.assertTrue(result.get("ok"), result)
            self.assertEqual(result.get("team"), "beta")
            self.assertEqual(result["team_owner"]["pane_id"], "%claimant")
            state = load_runtime_state(workspace)
            self.assertEqual(
                state["teams"]["beta"]["team_owner"]["pane_id"],
                "%claimant",
                "takeover --team beta must update beta's team_owner at team level",
            )
            self.assertEqual(
                state["teams"]["alpha"]["team_owner"],
                alpha_owner,
                "takeover --team beta must not touch alpha's team_owner",
            )
            self.assertEqual(
                state.get("team_owner"),
                alpha_owner,
                "takeover --team beta must not touch the workspace-primary team_owner (alpha)",
            )

    def test_lifecycle_mutator_with_team_routes_to_correct_team(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-lifecycle-team-") as tmp:
            workspace = Path(tmp)
            alpha_owner = {"pane_id": "%alpha", "provider": "codex", "machine_fingerprint": "machine-a"}
            beta_owner = {"pane_id": "%beta", "provider": "codex", "machine_fingerprint": "machine-b"}
            spec = _fake_spec(workspace)
            alpha_team_dir = workspace / ".team" / "alpha"
            beta_team_dir = workspace / ".team" / "beta"
            alpha_team_dir.mkdir(parents=True)
            beta_team_dir.mkdir(parents=True)
            alpha_spec_path = alpha_team_dir / "team.spec.yaml"
            beta_spec_path = beta_team_dir / "team.spec.yaml"
            alpha_spec_path.write_text(dumps(spec), encoding="utf-8")
            beta_spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(alpha_spec_path),
                    "team_dir": str(alpha_team_dir),
                    "workspace": str(workspace),
                    "session_name": "team-alpha",
                    "team_owner": alpha_owner,
                    "agents": {"alpha_worker": {"status": "running", "provider": "fake", "window": "alpha_worker", "session_id": "s-alpha"}},
                    "tasks": spec["tasks"],
                    "teams": {
                        "alpha": {
                            "spec_path": str(alpha_spec_path),
                            "team_dir": str(alpha_team_dir),
                            "session_name": "team-alpha",
                            "team_owner": alpha_owner,
                            "agents": {"alpha_worker": {"status": "running", "provider": "fake", "window": "alpha_worker", "session_id": "s-alpha"}},
                        },
                        "beta": {
                            "spec_path": str(beta_spec_path),
                            "team_dir": str(beta_team_dir),
                            "session_name": "team-beta",
                            "team_owner": beta_owner,
                            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl", "session_id": "s-beta"}},
                        },
                    },
                },
            )
            beta_caller_env = {
                "TEAM_AGENT_LEADER_PANE_ID": "%beta",
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "machine-b",
                "TEAM_AGENT_ID": "leader",
            }

            def fake_tmux(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                return proc

            with (
                patch.dict(os.environ, beta_caller_env, clear=False),
                patch("team_agent.runtime.run_cmd", side_effect=fake_tmux),
            ):
                result = runtime.stop_agent(workspace, "fake_impl", team="beta")
            self.assertTrue(result.get("ok"), result)
            after = load_runtime_state(workspace)
            self.assertEqual(
                after["teams"]["beta"]["agents"]["fake_impl"]["status"],
                "stopped",
                "stop-agent --team beta must mutate beta's worker state",
            )
            self.assertEqual(
                after["teams"]["alpha"]["agents"]["alpha_worker"]["status"],
                "running",
                "stop-agent --team beta must not touch alpha's worker state",
            )

    def test_lifecycle_mutator_without_team_in_multi_team_workspace_refuses_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-lifecycle-ambig-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            alpha_team_dir = workspace / ".team" / "alpha"
            beta_team_dir = workspace / ".team" / "beta"
            alpha_team_dir.mkdir(parents=True)
            beta_team_dir.mkdir(parents=True)
            alpha_spec_path = alpha_team_dir / "team.spec.yaml"
            beta_spec_path = beta_team_dir / "team.spec.yaml"
            alpha_spec_path.write_text(dumps(spec), encoding="utf-8")
            beta_spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(alpha_spec_path),
                    "team_dir": str(alpha_team_dir),
                    "workspace": str(workspace),
                    "session_name": "team-alpha",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                    "teams": {
                        "alpha": {
                            "spec_path": str(alpha_spec_path),
                            "team_dir": str(alpha_team_dir),
                            "session_name": "team-alpha",
                            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                        },
                        "beta": {
                            "spec_path": str(beta_spec_path),
                            "team_dir": str(beta_team_dir),
                            "session_name": "team-beta",
                            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                        },
                    },
                },
            )
            result = runtime.stop_agent(workspace, "fake_impl")
            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "team_target_ambiguous")
            self.assertIn("alpha", result.get("candidates") or [])
            self.assertIn("beta", result.get("candidates") or [])

    def test_takeover_acquires_same_lock_namespace_as_send_mutator(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-takeover-lock-") as tmp:
            workspace = _owner_workspace(Path(tmp))
            owner_env = {
                "TEAM_AGENT_LEADER_PANE_ID": OWNER["pane_id"],
                "TEAM_AGENT_LEADER_PROVIDER": OWNER["provider"],
                "TEAM_AGENT_MACHINE_FINGERPRINT": OWNER["machine_fingerprint"],
                "TEAM_AGENT_ID": "leader",
            }
            acquired_locks: list[str] = []
            real_runtime_lock = runtime._runtime_lock

            from contextlib import contextmanager

            @contextmanager
            def tracking_runtime_lock(workspace_arg, name, timeout=5.0):
                acquired_locks.append(name)
                with real_runtime_lock(workspace_arg, name, timeout=timeout):
                    yield

            with (
                patch.dict(os.environ, owner_env, clear=False),
                patch("team_agent.runtime._runtime_lock", side_effect=tracking_runtime_lock),
                patch("team_agent.runtime._deliver_pending_message", return_value={"ok": True, "status": "queued", "queued": True}),
            ):
                send_result = TeamOrchestratorTools(workspace).send_message("fake_impl", "first", sender="leader")
                takeover_result = runtime.takeover(workspace, team=None, confirm=True)
            self.assertTrue(send_result.get("ok"), send_result)
            self.assertTrue(takeover_result.get("ok"), takeover_result)
            self.assertIn(
                "send",
                acquired_locks,
                f"send_message must acquire the standard mutator lock; acquired={acquired_locks}",
            )
            takeover_locks = [name for name in acquired_locks if name in {"send", "takeover"}]
            self.assertTrue(
                "send" in takeover_locks,
                f"takeover must acquire the same lock namespace as send (got {acquired_locks}); otherwise ownership-transfer can race with a concurrent send by the old owner",
            )


def _owner_workspace(workspace: Path) -> Path:
    spec = _fake_spec(workspace)
    spec["runtime"]["session_name"] = f"team-owner-{workspace.name[-6:]}"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    (workspace / "new_worker.md").write_text(
        """---
name: new_worker
role: New Worker
provider: fake
tools:
  - mcp_team
---

New worker.
""",
        encoding="utf-8",
    )
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "workspace": str(workspace),
            "session_name": spec["runtime"]["session_name"],
            "team_owner": OWNER,
            "leader": spec["leader"],
            "agents": {
                "fake_impl": {
                    "status": "running",
                    "provider": "fake",
                    "window": "fake_impl",
                    "session_id": "session-fake",
                }
            },
            "tasks": spec["tasks"],
            "display_backend": "none",
        },
    )
    return workspace


def _fake_tmux_run(args: list[str], timeout: int = 20):
    proc = Mock(returncode=0, stdout="", stderr="")
    if args[:3] == ["tmux", "list-windows", "-t"]:
        proc.stdout = "fake_impl\n"
    return proc


if __name__ == "__main__":
    unittest.main(verbosity=2)
