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


def _orchestrator():
    return importlib.import_module("team_agent.orchestrator")


def _orchestrator_state():
    return importlib.import_module("team_agent.orchestrator.state")


def _orchestrator_plan():
    return importlib.import_module("team_agent.orchestrator.plan")


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


def _write_two_agent_runtime(workspace: Path) -> None:
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
            "session_name": "team-orchestrator-hotfix",
            "leader": spec["leader"],
            "agents": {
                "agent_a": {"status": "running", "provider": "fake", "window": "agent_a"},
                "agent_b": {"status": "running", "provider": "fake", "window": "agent_b"},
            },
            "tasks": spec["tasks"],
        },
    )


def _write_simple_plan(workspace: Path, *, plan_id: str = "nightly-demo", team: str | None = None) -> Path:
    plan_path = workspace / f"plan-{plan_id}.yaml"
    team_line = f"team: {team}\n" if team else ""
    plan_path.write_text(
        f"""id: {plan_id}
{team_line}stages:
  - id: stage_1
    dispatch:
      to: agent_a
      content: "stage 1"
    advance_on: "report_result.status == 'success'"
    halt_on: "report_result.status == 'failed'"
""",
        encoding="utf-8",
    )
    return plan_path


def _record_dispatch(dispatches: list[dict]):
    def fake_send(_workspace, to, content, **kwargs):
        dispatches.append({"to": to, "content": content, "kwargs": kwargs})
        return {"ok": True, "status": "queued", "message_id": f"msg_{len(dispatches)}"}
    return fake_send


class PhaseFHotfixSparkConstitutionTests(unittest.TestCase):

    def test_spark_h1_fanout_dedupes_duplicate_recipients(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-h1-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"] = [
                {**spec["agents"][0], "id": "agent_a"},
                {**spec["agents"][0], "id": "agent_b"},
            ]
            spec["runtime"]["max_active_agents"] = 2
            spec["runtime"]["startup_order"] = ["agent_a", "agent_b"]
            spec["routing"]["default_assignee"] = "agent_a"
            spec["routing"]["rules"] = []
            spec["tasks"][0]["id"] = "task_dedupe"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-dedupe",
                    "leader": spec["leader"],
                    "agents": {
                        agent_id: {"status": "running", "provider": "fake", "window": agent_id}
                        for agent_id in ("agent_a", "agent_b")
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_deliver(workspace_arg, state, message_id, wait_visible=True, timeout=30.0):
                MessageStore(workspace_arg).mark(message_id, "submitted")
                return {"ok": True, "status": "submitted"}

            with patch("team_agent.runtime._deliver_pending_message", side_effect=fake_deliver):
                result = runtime.send_message(
                    workspace,
                    ["agent_a", "agent_b", "agent_a", "agent_b", "agent_a"],
                    "duplicate-spam",
                    task_id="task_dedupe",
                    wait_visible=False,
                )

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "fanout_delivered")
            self.assertEqual(result["to"], ["agent_a", "agent_b"])
            self.assertEqual(result["delivered_count"], 2)
            recipients_in_store = [row["recipient"] for row in MessageStore(workspace).messages()]
            self.assertEqual(sorted(recipients_in_store), ["agent_a", "agent_b"])

    def test_spark_h2b_plan_id_with_slash_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-h2b-") as tmp:
            workspace = Path(tmp)
            _write_two_agent_runtime(workspace)
            plan_path = workspace / "bad-plan.yaml"
            plan_path.write_text(
                """id: a/b
stages:
  - id: stage_1
    dispatch:
      to: agent_a
      content: "x"
    advance_on: "any"
""",
                encoding="utf-8",
            )
            outcome = _orchestrator().start_plan(workspace, plan_path, start=False)
            self.assertFalse(outcome["ok"], outcome)
            self.assertEqual(outcome.get("reason"), "invalid_plan_id")

            state_mod = _orchestrator_state()
            with self.assertRaises(state_mod.InvalidPlanIdError):
                state_mod.sanitize_plan_id("../escape")
            with self.assertRaises(state_mod.InvalidPlanIdError):
                state_mod.sanitize_plan_id("a b")

    def test_spark_h2a_save_plan_state_uses_atomic_replace_no_partial_files(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-h2a-") as tmp:
            workspace = Path(tmp)
            state_mod = _orchestrator_state()
            state_mod.save_plan_state(workspace, {"plan_id": "atomic-demo", "current_stage": 1})
            target = state_mod.state_path(workspace, "atomic-demo")
            self.assertTrue(target.exists())
            stray = [p.name for p in target.parent.iterdir() if p.name.endswith(".tmp")]
            self.assertEqual(stray, [], f"unexpected tempfiles left behind: {stray}")
            loaded = state_mod.load_plan_state(workspace, "atomic-demo")
            self.assertEqual(loaded["plan_id"], "atomic-demo")

    def test_spark_m_malformed_condition_at_load_plan_raises(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-m-") as tmp:
            workspace = Path(tmp)
            plan_path = workspace / "bad-cond.yaml"
            plan_path.write_text(
                """id: bad-cond
stages:
  - id: s1
    dispatch:
      to: agent_a
      content: x
    advance_on: "report_result.status = 'success'"
""",
                encoding="utf-8",
            )
            plan_mod = _orchestrator_plan()
            with self.assertRaises(plan_mod.InvalidPlanError) as ctx:
                plan_mod.load_plan(plan_path)
            self.assertIn("invalid_condition", str(ctx.exception))

            _write_two_agent_runtime(workspace)
            outcome = _orchestrator().start_plan(workspace, plan_path, start=False)
            self.assertFalse(outcome["ok"], outcome)
            self.assertIn("invalid_condition", outcome.get("error", ""))

    def test_constitution_f1_dispatch_failure_halts_plan_with_artifact(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-f1-") as tmp:
            workspace = Path(tmp)
            _write_two_agent_runtime(workspace)
            plan_path = _write_simple_plan(workspace, plan_id="f1-demo")

            def raising_send(*_args, **_kwargs):
                raise RuntimeError("tmux pane vanished")

            with patch("team_agent.runtime.send_message", side_effect=raising_send):
                outcome = _orchestrator().start_plan(workspace, plan_path, start=True)

            self.assertTrue(outcome["ok"], outcome)
            self.assertEqual(outcome["status"], "halted")
            self.assertEqual(outcome["halt_reason"], "dispatch_failed")
            halt_path = Path(outcome["halt_artifact"])
            self.assertTrue(halt_path.exists())
            self.assertIn("dispatch_failed", halt_path.read_text(encoding="utf-8"))

    def test_constitution_f2_out_of_band_task_id_does_not_advance(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-f2-") as tmp:
            workspace = Path(tmp)
            _write_two_agent_runtime(workspace)
            plan_path = workspace / "f2-plan.yaml"
            plan_path.write_text(
                """id: f2-demo
stages:
  - id: stage_only
    dispatch:
      to: agent_a
      task_id: task_one
      content: "go"
    advance_on: "report_result.status == 'success'"
    halt_on: "report_result.status == 'failed'"
""",
                encoding="utf-8",
            )
            dispatches: list[dict] = []
            with patch("team_agent.runtime.send_message", side_effect=_record_dispatch(dispatches)):
                started = _orchestrator().start_plan(workspace, plan_path, start=True)
            self.assertTrue(started["ok"], started)
            self.assertEqual(started["status"], "running")

            out_of_band = _result("unrelated_task", "agent_a", "success")
            outcome = _orchestrator().handle_report_result(workspace, out_of_band)
            self.assertEqual(outcome.get("status"), "no_match")

            persisted = json.loads(
                _orchestrator_state().state_path(workspace, "f2-demo").read_text(encoding="utf-8")
            )
            self.assertEqual(persisted["current_stage"], 1)
            self.assertEqual(persisted["completed_stages"], [])

            correlated = _result("task_one", "agent_a", "success")
            advance = _orchestrator().handle_report_result(workspace, correlated)
            self.assertEqual(advance["status"], "completed")

    def test_constitution_f3_team_field_threaded_through_send_message(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-hotfix-f3-") as tmp:
            workspace = Path(tmp)
            _write_two_agent_runtime(workspace)
            plan_path = _write_simple_plan(workspace, plan_id="f3-demo", team="alpha")

            dispatches: list[dict] = []
            with patch("team_agent.runtime.send_message", side_effect=_record_dispatch(dispatches)):
                _orchestrator().start_plan(workspace, plan_path, start=True)

            self.assertEqual(len(dispatches), 1, dispatches)
            self.assertEqual(dispatches[0]["kwargs"].get("team"), "alpha")


if __name__ == "__main__":
    unittest.main(verbosity=2)
