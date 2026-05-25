from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.events import EventLog

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


class Gap15LifecycleEventOrderTests(unittest.TestCase):
    def test_gap15_lifecycle_event_emission_order(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap15-event-order-") as tmp:
            workspace = Path(tmp)
            spec, role_file = _write_event_order_workspace(workspace)

            def fake_compile(_path: Path, _team_dir: Path, agent_id: str) -> dict:
                agent = copy.deepcopy(spec["agents"][0])
                agent["id"] = agent_id
                agent["role"] = "Event ordered helper"
                return agent

            def fake_start(ws: Path, agent_id: str, **_kwargs: object) -> dict:
                EventLog(ws).write("start_agent.agent_start", agent_id=agent_id, window=agent_id)
                state = load_runtime_state(ws)
                state.setdefault("agents", {})[agent_id] = {"status": "running", "provider": "fake", "window": agent_id}
                save_runtime_state(ws, state)
                EventLog(ws).write("start_agent.complete", agent_id=agent_id, status="running")
                return {"ok": True, "agent_id": agent_id, "status": "running"}

            with (
                patch("team_agent.compiler.compile_role_doc_agent", side_effect=fake_compile),
                patch("team_agent.runtime.start_agent", side_effect=fake_start),
            ):
                result = runtime.add_agent(workspace, "ordered_helper", role_file_path=str(role_file), open_display=False)

            self.assertTrue(result["ok"], result)
            events = [event["event"] for event in _events(workspace)]
            expected = ["start_agent.agent_start", "start_agent.complete", "add_agent.complete"]
            actual = [event for event in events if event in expected]
            self.assertEqual(
                actual,
                expected,
                f"expected add-agent lifecycle event order {expected}, got {actual}; full events={events}",
            )


def _write_event_order_workspace(workspace: Path) -> tuple[dict, Path]:
    spec = _fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-gap15-order"
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
            "session_name": "team-gap15-order",
            "display_backend": "none",
            "leader": spec["leader"],
            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
            "tasks": spec["tasks"],
        },
    )
    role_file = workspace / "ordered_helper.md"
    role_file.write_text("# Ordered helper\n", encoding="utf-8")
    return spec, role_file


if __name__ == "__main__":
    unittest.main(verbosity=2)
