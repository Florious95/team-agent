from __future__ import annotations

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.message_store import MessageStore
from team_agent.simple_yaml import dumps
from team_agent.spec import load_spec
from team_agent.state import load_runtime_state, save_runtime_state, write_team_state


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def _spec_with_dynamic(workspace: Path, dynamic_id: str = "extra_helper") -> dict[str, Any]:
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-agent-gap16"
    dynamic = copy.deepcopy(spec["agents"][0])
    dynamic["id"] = dynamic_id
    dynamic["forked_from"] = "fake_impl"
    spec["agents"].append(dynamic)
    spec["runtime"]["startup_order"] = ["fake_impl", dynamic_id]
    return spec


def _setup_dynamic_workspace(
    tmp: str,
    *,
    agent_id: str = "extra_helper",
    running: bool = False,
    with_role_file: bool = True,
    seed_health: bool = True,
) -> Path:
    workspace = Path(tmp)
    spec = _spec_with_dynamic(workspace, agent_id)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    agent_state: dict[str, Any] = {
        "window": agent_id,
        "session_id": "session-extra",
        "captured_via": "fs_watch",
        "attribution_confidence": "high",
        "provider": "fake",
        "status": "running" if running else "stopped",
    }
    if with_role_file:
        role_file = workspace / ".team" / "dynamic-role-files" / f"{agent_id}.md"
        role_file.parent.mkdir(parents=True, exist_ok=True)
        role_file.write_text(f"# role for {agent_id}\n", encoding="utf-8")
        agent_state["dynamic_role_file"] = str(role_file.relative_to(workspace))
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": "team-agent-gap16",
            "agents": {agent_id: agent_state},
            "tasks": spec["tasks"],
        },
    )
    write_team_state(workspace, spec, load_runtime_state(workspace))
    if seed_health:
        MessageStore(workspace).upsert_agent_health(agent_id, "IDLE")
    return workspace


def _capture_storage_bytes(workspace: Path) -> dict[str, Any]:
    return {
        "state_json": (workspace / ".team" / "runtime" / "state.json").read_bytes(),
        "spec_yaml": (workspace / "team.spec.yaml").read_bytes(),
        "team_state": (workspace / "team_state.md").read_bytes()
            if (workspace / "team_state.md").exists() else None,
        "health_snapshot": json.dumps(MessageStore(workspace).agent_health(), sort_keys=True),
    }


class Gap16RemoveAgentAtomicTests(unittest.TestCase):

    def test_gap16_dynamic_remove_clears_five_locations_and_restart_plan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c1-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper")
            role_path = workspace / ".team" / "dynamic-role-files" / "extra_helper.md"
            self.assertTrue(role_path.exists())

            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "extra_helper")

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "removed")
            self.assertTrue(result.get("role_file_removed"))
            cleared = result.get("cleared_locations") or []
            for required in ("workspace_state", "team_state_md", "spec_yaml", "role_file", "agent_health"):
                self.assertIn(required, cleared, f"cleared_locations missing {required}; got {cleared}")

            state = load_runtime_state(workspace)
            self.assertNotIn("extra_helper", state.get("agents", {}))
            spec = load_spec(workspace / "team.spec.yaml")
            spec_ids = [agent.get("id") for agent in spec.get("agents", [])]
            self.assertNotIn("extra_helper", spec_ids)
            self.assertNotIn("extra_helper", spec.get("runtime", {}).get("startup_order", []))
            self.assertFalse(role_path.exists())
            self.assertNotIn("extra_helper", MessageStore(workspace).agent_health())

            complete_events = [e for e in _events(workspace) if e.get("event") == "remove_agent.complete"]
            self.assertEqual(len(complete_events), 1)
            self.assertEqual(complete_events[0]["agent_id"], "extra_helper")
            self.assertEqual(set(complete_events[0]["cleared_locations"]),
                             {"workspace_state", "team_state_md", "spec_yaml", "role_file", "agent_health"})
            step_events = [e for e in _events(workspace) if e.get("event") == "lifecycle.remove_step_completed"]
            self.assertGreaterEqual(len(step_events), 5)
            self.assertEqual([e["step"] for e in step_events if e["step"] != "stop_agent"],
                             ["workspace_state", "team_state_md", "spec_yaml", "role_file", "agent_health"])

    def test_gap16_spec_native_refusal_byte_equal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c2-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper")
            before = _capture_storage_bytes(workspace)
            stop_calls: list[str] = []

            def fake_stop(*_a: Any, **_kw: Any) -> dict[str, Any]:
                stop_calls.append("called")
                return {"ok": True, "status": "stopped"}

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop):
                result = runtime.remove_agent(workspace, "fake_impl")

            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "from_spec_confirm_required")
            self.assertEqual(stop_calls, [], "stop_agent must not be called for refused remove")
            after = _capture_storage_bytes(workspace)
            self.assertEqual(before, after, "refused remove must leave all storage byte-equal")

    def test_gap16_running_without_force_refusal_byte_equal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c3-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper", running=True)
            before = _capture_storage_bytes(workspace)
            stop_calls: list[str] = []

            def fake_stop(*_a: Any, **_kw: Any) -> dict[str, Any]:
                stop_calls.append("called")
                return {"ok": True, "status": "stopped"}

            with patch.object(runtime, "_tmux_window_exists", return_value=True), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop):
                result = runtime.remove_agent(workspace, "extra_helper")

            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "force_required")
            self.assertEqual(stop_calls, [])
            after = _capture_storage_bytes(workspace)
            self.assertEqual(before, after, "running-no-force refusal must leave storage unchanged")
            health = MessageStore(workspace).agent_health()
            self.assertIn("extra_helper", health)

    def test_gap16_force_remove_stop_then_cleanup_order(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c4-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper", running=True)
            order: list[str] = []

            def fake_stop(ws: Path, agent_id: str, **_kw: Any) -> dict[str, Any]:
                order.append("stop_agent")
                state = load_runtime_state(ws)
                state["agents"].get(agent_id, {})["status"] = "stopped"
                save_runtime_state(ws, state)
                return {"ok": True, "agent_id": agent_id, "status": "stopped"}

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop):
                result = runtime.remove_agent(workspace, "extra_helper", force=True)

            self.assertTrue(result["ok"], result)
            cleared = result.get("cleared_locations") or []
            self.assertEqual(cleared[0], "stop_agent", f"first step must be stop_agent; got {cleared}")
            for required in ("workspace_state", "team_state_md", "spec_yaml", "agent_health"):
                self.assertIn(required, cleared)
            self.assertEqual(order, ["stop_agent"])
            self.assertEqual(result["stopped"]["status"], "stopped")
            self.assertNotIn("extra_helper", load_runtime_state(workspace).get("agents", {}))

    def test_gap16_force_late_failure_rolls_back_stop_and_storage(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c5-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper", running=True)
            from team_agent.lifecycle import agents as lifecycle_module
            stop_calls: list[str] = []
            start_calls: list[str] = []

            def fake_stop(ws: Path, agent_id: str, **_kw: Any) -> dict[str, Any]:
                stop_calls.append(agent_id)
                state = load_runtime_state(ws)
                state["agents"].get(agent_id, {})["status"] = "stopped"
                save_runtime_state(ws, state)
                return {"ok": True, "agent_id": agent_id, "status": "stopped"}

            def fake_start(ws: Path, agent_id: str, **_kw: Any) -> dict[str, Any]:
                start_calls.append(agent_id)
                state = load_runtime_state(ws)
                state.setdefault("agents", {}).setdefault(agent_id, {})["status"] = "running"
                save_runtime_state(ws, state)
                return {"ok": True}

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop), \
                 patch.object(runtime, "start_agent", side_effect=fake_start), \
                 patch.object(lifecycle_module, "write_spec", side_effect=OSError("spec disk full")):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "extra_helper", force=True)

            self.assertEqual(stop_calls, ["extra_helper"])
            self.assertEqual(start_calls, ["extra_helper"], "rollback must restart the stopped worker")
            state = load_runtime_state(workspace)
            self.assertIn("extra_helper", state.get("agents", {}))
            self.assertIn(state["agents"]["extra_helper"]["status"], {"running", "busy"})
            self.assertTrue((workspace / ".team" / "dynamic-role-files" / "extra_helper.md").exists())
            self.assertIn("extra_helper", MessageStore(workspace).agent_health())
            rollback_events = [e for e in _events(workspace) if e.get("event") == "remove_agent.rollback"]
            self.assertEqual(len(rollback_events), 1)
            self.assertTrue(rollback_events[0]["ok"], rollback_events[0])
            self.assertEqual(rollback_events[0]["failed_step"], "spec_yaml")

    def test_gap16_rollback_failure_reports_resource_id(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap16-c6-") as tmp:
            workspace = _setup_dynamic_workspace(tmp, agent_id="extra_helper")
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "write_spec", side_effect=OSError("primary spec boom")), \
                 patch.object(lifecycle_module._RemoveRollback, "restore",
                              return_value={"ok": False, "errors": ["spec:disk-full /tmp/x/team.spec.yaml"]}):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.remove_agent(workspace, "extra_helper")

            self.assertIn("rollback_ok=False", str(ctx.exception))
            self.assertIn("spec_yaml", str(ctx.exception))
            self.assertIn("extra_helper", str(ctx.exception))
            rollback_events = [e for e in _events(workspace) if e.get("event") == "remove_agent.rollback"]
            self.assertEqual(len(rollback_events), 1)
            evt = rollback_events[0]
            self.assertFalse(evt["ok"])
            self.assertEqual(evt["agent_id"], "extra_helper")
            self.assertEqual(evt["failed_step"], "spec_yaml")
            self.assertIsNotNone(evt.get("resource"))
            self.assertIn("spec:disk-full", "".join(evt["rollback"]["errors"]))
            rolled_back_events = [e for e in _events(workspace) if e.get("event") == "lifecycle.remove_rolled_back"]
            self.assertEqual(len(rolled_back_events), 1)
            self.assertFalse(rolled_back_events[0]["ok"])
            self.assertEqual(rolled_back_events[0]["failed_step"], "spec_yaml")


if __name__ == "__main__":
    unittest.main(verbosity=2)
