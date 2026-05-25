from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

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


class Gap15ForkAgentRollbackTests(unittest.TestCase):
    def test_gap15_fork_agent_failure_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap15-fork-rollback-") as tmp:
            workspace = Path(tmp)
            spec = _write_fork_workspace(workspace)
            spec_path = workspace / "team.spec.yaml"
            before_spec = spec_path.read_text(encoding="utf-8")
            before_state = copy.deepcopy(load_runtime_state(workspace))
            before_health = MessageStore(workspace).agent_health()
            windows = {"source_worker"}
            cleaned_mcp: list[str] = []

            class FakeForkAdapter:
                provider = "fake"

                def supports_session_fork(self, _agent: dict | None = None) -> bool:
                    return True

                def mcp_config(self, _workspace: Path, _agent_id: str) -> dict:
                    return {}

                def install_mcp(self, workspace_arg: Path, agent_id: str, _config: dict) -> Path:
                    path = workspace_arg / ".team" / "runtime" / "mcp" / f"{agent_id}.json"
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("{}", encoding="utf-8")
                    return path

                def cleanup_mcp(self, _workspace: Path, agent_id: str, _mcp_path: Path | None = None) -> None:
                    cleaned_mcp.append(agent_id)

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "new-window"]:
                    windows.add(args[args.index("-n") + 1])
                elif args[:3] == ["tmux", "kill-window", "-t"]:
                    windows.discard(args[3].split(":", 1)[1])
                return proc

            with (
                patch("team_agent.lifecycle.operations.get_adapter", return_value=FakeForkAdapter()),
                patch("team_agent.lifecycle.operations.shell_fork_command_for_agent", return_value="fake fork command"),
                patch("team_agent.lifecycle.operations._tmux_window_exists", side_effect=lambda _session, window: window in windows),
                patch("team_agent.lifecycle.operations.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.lifecycle.operations._handle_startup_prompts_and_verify_window", return_value=False),
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.fork_agent(workspace, "source_worker", as_agent_id="forked_worker", open_display=False)

            self.assertEqual(spec_path.read_text(encoding="utf-8"), before_spec)
            self.assertEqual(load_runtime_state(workspace), before_state)
            self.assertEqual(MessageStore(workspace).agent_health(), before_health)
            self.assertEqual(windows, {"source_worker"})
            self.assertEqual(cleaned_mcp, ["forked_worker"])
            self.assertFalse(any(event.get("event") == "fork_agent.complete" for event in _events(workspace)))


def _write_fork_workspace(workspace: Path) -> dict:
    spec = _fake_spec(workspace)
    source = copy.deepcopy(spec["agents"][0])
    source["id"] = "source_worker"
    spec["agents"] = [source]
    spec["runtime"]["session_name"] = "team-gap15-fork"
    spec["runtime"]["startup_order"] = ["source_worker"]
    spec["runtime"]["display_backend"] = "none"
    spec["routing"]["rules"] = []
    spec["routing"]["default_assignee"] = "source_worker"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": "team-gap15-fork",
            "display_backend": "none",
            "leader": spec["leader"],
            "agents": {
                "source_worker": {
                    "status": "running",
                    "provider": "fake",
                    "window": "source_worker",
                    "session_id": "source-session",
                }
            },
            "tasks": spec["tasks"],
        },
    )
    MessageStore(workspace).upsert_agent_health("source_worker", "IDLE")
    return spec


if __name__ == "__main__":
    unittest.main(verbosity=2)
