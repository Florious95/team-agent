from __future__ import annotations

import copy
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


def _spec_for(workspace: Path) -> dict[str, Any]:
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-agent-test"
    keeper = copy.deepcopy(spec["agents"][0])
    keeper["id"] = "fake_keeper"
    keeper["preferred_for"] = ["filler"]
    spec["agents"].append(keeper)
    spec["runtime"]["startup_order"] = ["fake_impl", "fake_keeper"]
    spec["routing"]["default_assignee"] = "fake_keeper"
    spec["routing"]["rules"] = [
        rule for rule in spec["routing"]["rules"] if rule.get("assign_to") != "fake_impl"
    ]
    return spec


def _setup_workspace(
    tmp: str,
    *,
    agent_id: str = "fake_impl",
    dynamic: bool = False,
    running: bool = False,
    with_role_file: bool = False,
    seed_health: bool = True,
) -> Path:
    workspace = Path(tmp)
    spec = _spec_for(workspace)
    if dynamic:
        for agent in spec["agents"]:
            if agent["id"] == agent_id:
                agent["forked_from"] = "fake_impl_origin"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")

    agent_state: dict[str, Any] = {
        "window": agent_id,
        "session_id": "session-x",
        "captured_via": "fs_watch",
        "attribution_confidence": "high",
        "provider": "fake",
        "status": "running" if running else "stopped",
    }
    if dynamic and with_role_file:
        role_file = workspace / ".team" / "dynamic-role-files" / f"{agent_id}.md"
        role_file.parent.mkdir(parents=True, exist_ok=True)
        role_file.write_text(f"# role for {agent_id}\n", encoding="utf-8")
        agent_state["dynamic_role_file"] = str(role_file.relative_to(workspace))
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": "team-agent-test",
            "agents": {agent_id: agent_state},
            "tasks": spec["tasks"],
        },
    )
    write_team_state(workspace, spec, load_runtime_state(workspace))
    if seed_health:
        MessageStore(workspace).upsert_agent_health(agent_id, "IDLE")
    return workspace


def _events(workspace: Path) -> list[dict[str, Any]]:
    import json

    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


class AgentHealthGcHelperTests(unittest.TestCase):
    def test_delete_agent_health_removes_named_row_only(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-delete-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            store.upsert_agent_health("beta", "RUNNING")
            self.assertTrue(store.delete_agent_health("alpha"))
            remaining = store.agent_health()
            self.assertNotIn("alpha", remaining)
            self.assertIn("beta", remaining)
            self.assertEqual(remaining["beta"]["status"], "RUNNING")

    def test_delete_agent_health_missing_row_is_noop(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-missing-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            self.assertFalse(store.delete_agent_health("never_existed"))
            self.assertIn("alpha", store.agent_health())

    def test_gc_agent_health_drops_rows_absent_from_valid_set(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-sweep-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            store.upsert_agent_health("beta", "RUNNING")
            store.upsert_agent_health("removed", "DONE")
            swept = store.gc_agent_health({"alpha", "beta"})
            self.assertEqual(swept, ["removed"])
            remaining = store.agent_health()
            self.assertIn("alpha", remaining)
            self.assertIn("beta", remaining)
            self.assertNotIn("removed", remaining)

    def test_gc_agent_health_preserves_cross_team_rows_when_union_passed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-crossteam-") as tmp:
            store = MessageStore(Path(tmp))
            team_a_live = {"alpha", "beta"}
            team_b_live = {"gamma", "delta"}
            for agent_id in team_a_live | team_b_live:
                store.upsert_agent_health(agent_id, "IDLE")
            store.upsert_agent_health("stale_from_a", "DONE")
            swept = store.gc_agent_health(team_a_live | team_b_live)
            self.assertEqual(swept, ["stale_from_a"])
            remaining = store.agent_health()
            for agent_id in team_a_live | team_b_live:
                self.assertIn(agent_id, remaining)
            self.assertNotIn("stale_from_a", remaining)

    def test_gc_agent_health_empty_valid_set_clears_table(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-empty-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            store.upsert_agent_health("beta", "RUNNING")
            swept = store.gc_agent_health(set())
            self.assertEqual(sorted(swept), ["alpha", "beta"])
            self.assertEqual(store.agent_health(), {})

    def test_gc_agent_health_accepts_iterable_input(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-iter-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            store.upsert_agent_health("beta", "RUNNING")
            swept = store.gc_agent_health(iter(["alpha"]))
            self.assertEqual(swept, ["beta"])
            self.assertEqual(set(store.agent_health()), {"alpha"})

    def test_gc_agent_health_empty_table_returns_empty_list(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-noop-") as tmp:
            store = MessageStore(Path(tmp))
            self.assertEqual(store.gc_agent_health({"alpha"}), [])

    def test_gc_agent_health_rejects_non_string_entry_without_deleting(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-validation-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            store.upsert_agent_health("beta", "RUNNING")
            with self.assertRaises(TypeError):
                store.gc_agent_health(["alpha", None])
            remaining = store.agent_health()
            self.assertIn("alpha", remaining)
            self.assertIn("beta", remaining)

    def test_gc_agent_health_rejects_empty_entry_without_deleting(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-emptyentry-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("alpha", "IDLE")
            with self.assertRaises(ValueError):
                store.gc_agent_health(["alpha", ""])
            self.assertIn("alpha", store.agent_health())

    @unittest.skip(
        "schema lacks agent_health.team_id; full cross-team isolation pending Gap 1/10 schema migration"
    )
    def test_gc_agent_health_isolates_same_agent_id_across_teams(self) -> None:
        # Placeholder for Gap 1/10. Today agent_health.agent_id is a text PK
        # with no team_id column, so two same-workspace teams sharing an
        # agent_id collapse into one row on upsert and gc cannot tell them
        # apart. Once the migration adds agent_health.team_id and the helpers
        # accept a team scope, removing this skip should fail loudly (because
        # the current code path swept the collapsed row), prompting the
        # team-scoped helper rewrite that the migration unlocks.
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-team-collision-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("worker", "IDLE", current_task_id="team_a_task")
            store.upsert_agent_health("worker", "RUNNING", current_task_id="team_b_task")
            store.gc_agent_health(set())
            remaining = store.agent_health()
            self.assertIn("worker", remaining)
            self.assertEqual(remaining["worker"]["current_task_id"], "team_b_task")

    def test_gc_agent_health_broken_derivation_excluding_sibling_team_is_rejected(self) -> None:
        # Simulate a sync-path derivation that silently maps a sibling team's
        # agent id to None (e.g. dict lookup with default=None). Without
        # input validation the helper would treat None as "not in valid set"
        # and delete the sibling team's row. Validation must halt before any
        # DB mutation so sibling-team rows survive the buggy derivation.
        with tempfile.TemporaryDirectory(prefix="team-agent-gc-broken-derive-") as tmp:
            store = MessageStore(Path(tmp))
            for agent_id in ("alpha", "beta", "gamma"):
                store.upsert_agent_health(agent_id, "IDLE")

            team_a_live = ["alpha", "beta"]
            sibling_lookup = {"gamma": None}
            derived = team_a_live + [sibling_lookup["gamma"]]

            with self.assertRaises(TypeError):
                store.gc_agent_health(derived)

            remaining = store.agent_health()
            for agent_id in ("alpha", "beta", "gamma"):
                self.assertIn(agent_id, remaining)


class RemoveAgentRefusalTests(unittest.TestCase):
    def test_unknown_worker_raises(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-unknown-") as tmp:
            workspace = _setup_workspace(tmp)
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "no_such_agent")

    def test_leader_id_refused_as_unknown_worker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-leader-") as tmp:
            workspace = _setup_workspace(tmp)
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "leader", from_spec=True, confirm=True)

    def test_spec_native_without_from_spec_confirm_refused(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-refuse-spec-") as tmp:
            workspace = _setup_workspace(tmp)
            spec_before = (workspace / "team.spec.yaml").read_text(encoding="utf-8")
            state_before = copy.deepcopy(load_runtime_state(workspace))
            health_before = MessageStore(workspace).agent_health()
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "fake_impl")
            self.assertFalse(result["ok"])
            self.assertEqual(result["status"], "refused")
            self.assertEqual(result["reason"], "from_spec_confirm_required")
            self.assertEqual((workspace / "team.spec.yaml").read_text(encoding="utf-8"), spec_before)
            self.assertEqual(load_runtime_state(workspace), state_before)
            self.assertEqual(MessageStore(workspace).agent_health(), health_before)

    def test_spec_native_with_from_spec_only_still_refused(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-refuse-noconfirm-") as tmp:
            workspace = _setup_workspace(tmp)
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "fake_impl", from_spec=True, confirm=False)
            self.assertFalse(result["ok"])
            self.assertEqual(result["reason"], "from_spec_confirm_required")

    def test_running_worker_without_force_refused(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-refuse-running-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True, running=True)
            spec_before = (workspace / "team.spec.yaml").read_text(encoding="utf-8")
            state_before = copy.deepcopy(load_runtime_state(workspace))
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "fake_impl")
            self.assertFalse(result["ok"])
            self.assertEqual(result["reason"], "force_required")
            self.assertEqual((workspace / "team.spec.yaml").read_text(encoding="utf-8"), spec_before)
            self.assertEqual(load_runtime_state(workspace), state_before)


class RemoveAgentHappyPathTests(unittest.TestCase):
    def test_dynamic_agent_remove_clears_all_five_storage_points(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-dynamic-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            role_path = workspace / ".team" / "dynamic-role-files" / "fake_impl.md"
            self.assertTrue(role_path.exists())
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "fake_impl")
            self.assertTrue(result["ok"])
            self.assertEqual(result["status"], "removed")
            self.assertTrue(result["role_file_removed"])
            state = load_runtime_state(workspace)
            self.assertNotIn("fake_impl", state.get("agents", {}))
            spec = load_spec(workspace / "team.spec.yaml")
            self.assertNotIn("fake_impl", [agent.get("id") for agent in spec.get("agents", [])])
            self.assertNotIn("fake_impl", spec.get("runtime", {}).get("startup_order", []))
            self.assertFalse(role_path.exists())
            self.assertNotIn("fake_impl", MessageStore(workspace).agent_health())

    def test_spec_native_removed_with_from_spec_confirm(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-spec-confirm-") as tmp:
            workspace = _setup_workspace(tmp)
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                result = runtime.remove_agent(workspace, "fake_impl", from_spec=True, confirm=True)
            self.assertTrue(result["ok"])
            spec = load_spec(workspace / "team.spec.yaml")
            ids = [agent.get("id") for agent in spec.get("agents", [])]
            self.assertNotIn("fake_impl", ids)
            self.assertIn("fake_keeper", ids)

    def test_force_running_calls_stop_then_removes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-force-happy-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True, running=True)
            stop_calls: list[str] = []

            def fake_stop(ws: Path, agent_id: str) -> dict[str, Any]:
                stop_calls.append(agent_id)
                state = load_runtime_state(ws)
                state["agents"].get(agent_id, {})["status"] = "stopped"
                save_runtime_state(ws, state)
                return {"ok": True, "agent_id": agent_id, "status": "stopped"}

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop):
                result = runtime.remove_agent(workspace, "fake_impl", force=True)
            self.assertTrue(result["ok"])
            self.assertEqual(stop_calls, ["fake_impl"])
            self.assertEqual(result["stopped"]["status"], "stopped")
            self.assertNotIn("fake_impl", load_runtime_state(workspace).get("agents", {}))

    def test_remove_event_log_records_completion(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-event-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                runtime.remove_agent(workspace, "fake_impl")
            events = [evt for evt in _events(workspace) if evt.get("event") == "remove_agent.complete"]
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0]["agent_id"], "fake_impl")


class RemoveAgentRollbackTests(unittest.TestCase):
    def _assert_storage_intact(self, workspace: Path, *, expect_running: bool) -> None:
        spec = load_spec(workspace / "team.spec.yaml")
        self.assertIn("fake_impl", [agent.get("id") for agent in spec.get("agents", [])])
        state = load_runtime_state(workspace)
        self.assertIn("fake_impl", state.get("agents", {}))
        if expect_running:
            self.assertIn(state["agents"]["fake_impl"].get("status"), {"running", "busy"})
        self.assertIn("fake_impl", MessageStore(workspace).agent_health())

    def test_stop_failure_blocks_storage_mutation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-stop-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True, running=True)
            role_path = workspace / ".team" / "dynamic-role-files" / "fake_impl.md"

            def boom(*_a: Any, **_kw: Any) -> Any:
                raise RuntimeError("stop boom")

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=boom):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl", force=True)
            self._assert_storage_intact(workspace, expect_running=True)
            self.assertTrue(role_path.exists())

    def test_workspace_state_write_failure_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-state-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            calls = {"n": 0}
            real = lifecycle_module.save_runtime_state

            def flaky(ws: Path, payload: Any) -> Any:
                calls["n"] += 1
                if calls["n"] == 1:
                    raise OSError("state write boom")
                return real(ws, payload)

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "save_runtime_state", side_effect=flaky):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)
            self.assertTrue((workspace / ".team" / "dynamic-role-files" / "fake_impl.md").exists())

    def test_team_state_write_failure_rolls_back_workspace_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-teamstate-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "write_team_state", side_effect=OSError("team state boom")):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)

    def test_spec_write_failure_rolls_back_states(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-spec-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "write_spec", side_effect=OSError("spec boom")):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)

    def test_role_file_unlink_failure_rolls_back_spec_and_states(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-rolefile-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "_remove_dynamic_role_file", side_effect=OSError("unlink boom")):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)
            self.assertTrue((workspace / ".team" / "dynamic-role-files" / "fake_impl.md").exists())

    def test_missing_required_dynamic_role_file_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-missing-rolefile-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            (workspace / ".team" / "dynamic-role-files" / "fake_impl.md").unlink()
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)

    def test_agent_health_delete_failure_rolls_back_role_file(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-health-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "_delete_agent_health", side_effect=OSError("health boom")):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl")
            self._assert_storage_intact(workspace, expect_running=False)
            self.assertTrue((workspace / ".team" / "dynamic-role-files" / "fake_impl.md").exists())

    def test_force_rollback_restores_worker_after_storage_failure(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-force-rollback-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True, running=True)
            from team_agent.lifecycle import agents as lifecycle_module
            stop_calls: list[str] = []
            start_calls: list[str] = []

            def fake_stop(ws: Path, agent_id: str) -> dict[str, Any]:
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
                return {"ok": True, "agent_id": agent_id, "status": "running"}

            calls = {"n": 0}
            real = lifecycle_module.save_runtime_state

            def flaky_state(ws: Path, payload: Any) -> Any:
                calls["n"] += 1
                if calls["n"] == 1:
                    raise OSError("state write boom")
                return real(ws, payload)

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(runtime, "stop_agent", side_effect=fake_stop), \
                 patch.object(runtime, "start_agent", side_effect=fake_start), \
                 patch.object(lifecycle_module, "save_runtime_state", side_effect=flaky_state):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "fake_impl", force=True)
            self.assertEqual(stop_calls, ["fake_impl"])
            self.assertEqual(start_calls, ["fake_impl"])
            self._assert_storage_intact(workspace, expect_running=True)

    def test_rollback_failure_records_event_and_raises(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-rollback-fail-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            from team_agent.lifecycle import agents as lifecycle_module

            with patch.object(runtime, "_tmux_window_exists", return_value=False), \
                 patch.object(lifecycle_module, "write_spec", side_effect=OSError("primary boom")), \
                 patch.object(lifecycle_module._RemoveRollback, "restore", return_value={"ok": False, "errors": ["spec:disk full"]}):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.remove_agent(workspace, "fake_impl")
            self.assertIn("rollback_ok=False", str(ctx.exception))
            events = [evt for evt in _events(workspace) if evt.get("event") == "remove_agent.rollback"]
            self.assertEqual(len(events), 1)
            self.assertFalse(events[0]["ok"])


if __name__ == "__main__":
    unittest.main()
