from __future__ import annotations

import importlib
import importlib.util
import json
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


class OrchestratorPlanContinuityTests(unittest.TestCase):
    def test_run_overnight_plan_advances_on_success_and_halts_with_artifact_on_failure(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-orchestrator-plan-") as tmp:
            workspace = Path(tmp)
            _write_runtime_state(workspace)
            plan_path = _write_two_stage_plan(workspace)
            dispatches: list[dict] = []

            with patch("team_agent.messaging.internal_delivery.deliver_stored_message", side_effect=_record_dispatch(dispatches)):
                started = _orchestrator().start_plan(workspace, plan_path, start=True)

            self.assertTrue(started["ok"], started)
            self.assertEqual(started["status"], "running")
            self.assertEqual(started["plan_id"], "nightly-demo")
            self.assertEqual([item["to"] for item in dispatches], ["agent_a"])
            self.assertTrue((workspace / ".team" / "runtime" / "orchestrator" / "plan-nightly-demo.state.json").exists())

            stage_1_result = _result("stage_1", "agent_a", "success")
            with patch("team_agent.messaging.internal_delivery.deliver_stored_message", side_effect=_record_dispatch(dispatches)):
                advanced = _orchestrator().handle_report_result(workspace, stage_1_result)

            self.assertTrue(advanced["ok"], advanced)
            self.assertEqual(advanced["status"], "running")
            self.assertEqual(advanced["current_stage"], 2)
            self.assertEqual([item["to"] for item in dispatches], ["agent_a", "agent_b"])

            stage_2_result = _result("stage_2", "agent_b", "failed")
            halted = _orchestrator().handle_report_result(workspace, stage_2_result)

            self.assertTrue(halted["ok"], halted)
            self.assertEqual(halted["status"], "halted")
            self.assertEqual(halted["halt_reason"], "report_result.status == 'failed'")
            halt_path = Path(halted["halt_artifact"])
            self.assertTrue(halt_path.exists())
            self.assertTrue(str(halt_path).startswith(str(workspace / ".team" / "artifacts" / "orchestrator")))
            halt_text = halt_path.read_text(encoding="utf-8")
            self.assertIn("stage_2", halt_text)
            self.assertIn('"status": "failed"', halt_text)

    def test_plan_state_survives_coordinator_restart_and_resumes_from_last_completed_stage(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-orchestrator-resume-") as tmp:
            workspace = Path(tmp)
            _write_runtime_state(workspace)
            plan_path = _write_two_stage_plan(workspace)
            dispatches: list[dict] = []

            with patch("team_agent.messaging.internal_delivery.deliver_stored_message", side_effect=_record_dispatch(dispatches)):
                _orchestrator().start_plan(workspace, plan_path, start=True)
                _orchestrator().handle_report_result(workspace, _result("stage_1", "agent_a", "success"))

            state_path = workspace / ".team" / "runtime" / "orchestrator" / "plan-nightly-demo.state.json"
            persisted = json.loads(state_path.read_text(encoding="utf-8"))
            self.assertEqual(persisted["completed_stages"], [1])
            self.assertEqual(persisted["current_stage"], 2)

            resumed = _orchestrator().resume_plans(workspace)

            self.assertTrue(resumed["ok"], resumed)
            self.assertEqual(resumed["plans"][0]["plan_id"], "nightly-demo")
            self.assertEqual(resumed["plans"][0]["current_stage"], 2)
            self.assertEqual(resumed["plans"][0]["status"], "running")


def _orchestrator():
    try:
        return importlib.import_module("team_agent.orchestrator")
    except ModuleNotFoundError as exc:
        raise AssertionError(
            "Gap 17 MVP requires team_agent.orchestrator with start_plan, "
            "handle_report_result, and resume_plans for run-overnight continuity"
        ) from exc


def _write_two_stage_plan(workspace: Path) -> Path:
    plan_path = workspace / "overnight-plan.yaml"
    plan_path.write_text(
        """
id: nightly-demo
stages:
  - id: stage_1
    dispatch:
      to: agent_a
      content: "stage 1"
    advance_on: "report_result.status == 'success'"
    halt_on: "report_result.status == 'failed'"
  - id: stage_2
    dispatch:
      to: agent_b
      content: "stage 2"
    advance_on: "report_result.status == 'success'"
    halt_on: "report_result.status == 'failed'"
""".lstrip(),
        encoding="utf-8",
    )
    return plan_path


def _write_runtime_state(workspace: Path) -> None:
    spec = _fake_spec(workspace)
    spec["agents"] = [
        {**spec["agents"][0], "id": "agent_a"},
        {**spec["agents"][0], "id": "agent_b"},
    ]
    spec["runtime"]["max_active_agents"] = 2
    spec["runtime"]["startup_order"] = ["agent_a", "agent_b"]
    spec["routing"]["default_assignee"] = "agent_a"
    spec["routing"]["rules"] = []
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": "team-orchestrator",
            "leader": spec["leader"],
            "agents": {
                "agent_a": {"status": "running", "provider": "fake", "window": "agent_a"},
                "agent_b": {"status": "running", "provider": "fake", "window": "agent_b"},
            },
            "tasks": spec["tasks"],
        },
    )


def _record_dispatch(dispatches: list[dict]):
    def fake_send(_workspace, to, content, **kwargs):
        dispatches.append({"to": to, "content": content, "kwargs": kwargs})
        return {"ok": True, "status": "queued", "message_id": f"msg_{len(dispatches)}"}

    return fake_send


def _result(task_id: str, agent_id: str, status: str) -> dict:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": task_id,
        "agent_id": agent_id,
        "status": status,
        "summary": status,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
