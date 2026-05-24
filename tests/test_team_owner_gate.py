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
