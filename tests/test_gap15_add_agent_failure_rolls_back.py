from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

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


class Gap15AddAgentRollbackTests(unittest.TestCase):
    def test_gap15_add_agent_failure_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap15-add-rollback-") as tmp:
            workspace = Path(tmp)
            spec, role_file = _write_add_agent_workspace(workspace)
            spec_path = workspace / "team.spec.yaml"
            before_spec = spec_path.read_text(encoding="utf-8")
            before_state = copy.deepcopy(load_runtime_state(workspace))
            before_health = MessageStore(workspace).agent_health()
            simulated_windows: set[str] = set()

            def fake_compile(_path: Path, _team_dir: Path, agent_id: str) -> dict:
                agent = copy.deepcopy(spec["agents"][0])
                agent["id"] = agent_id
                agent["role"] = "Extra helper"
                return agent

            def failing_start(ws: Path, agent_id: str, **_kwargs: object) -> dict:
                simulated_windows.add(agent_id)
                state = load_runtime_state(ws)
                state.setdefault("agents", {})[agent_id] = {"status": "running", "provider": "fake", "window": agent_id}
                save_runtime_state(ws, state)
                simulated_windows.discard(agent_id)
                raise TeamAgentRuntimeError("injected add-agent startup failure")

            with (
                patch("team_agent.compiler.compile_role_doc_agent", side_effect=fake_compile),
                patch("team_agent.runtime.start_agent", side_effect=failing_start),
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.add_agent(workspace, "extra_helper", role_file_path=str(role_file), open_display=False)

            self.assertEqual(spec_path.read_text(encoding="utf-8"), before_spec)
            self.assertEqual(load_runtime_state(workspace), before_state)
            self.assertEqual(MessageStore(workspace).agent_health(), before_health)
            self.assertFalse((workspace / ".team" / "dynamic-role-files" / "extra_helper.md").exists())
            self.assertNotIn("extra_helper", simulated_windows)
            events = _events(workspace) if (workspace / ".team" / "logs" / "events.jsonl").exists() else []
            self.assertFalse(any(event.get("event") == "add_agent.complete" for event in events))


def _write_add_agent_workspace(workspace: Path) -> tuple[dict, Path]:
    spec = _fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-gap15-add"
    spec["runtime"]["display_backend"] = "none"
    spec["routing"]["rules"] = []
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    (team_dir / "TEAM.md").write_text("---\nname: gap15\nprovider: fake\n---\n", encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "team_dir": str(team_dir),
            "session_name": "team-gap15-add",
            "display_backend": "none",
            "leader": spec["leader"],
            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
            "tasks": spec["tasks"],
        },
    )
    MessageStore(workspace).upsert_agent_health("fake_impl", "IDLE")
    role_file = workspace / "extra_helper.md"
    role_file.write_text("# Extra helper\n", encoding="utf-8")
    return spec, role_file


if __name__ == "__main__":
    unittest.main(verbosity=2)
