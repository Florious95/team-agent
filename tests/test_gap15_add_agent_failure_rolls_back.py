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

            rolled_back = [e for e in events if e.get("event") == "lifecycle.add_step_rolled_back"]
            self.assertEqual({e.get("step") for e in rolled_back}, {"spec_yaml", "workspace_state", "team_state_md", "role_file"},
                             f"expected per-step rollback events; got {rolled_back}")
            for evt in rolled_back:
                self.assertEqual(evt.get("agent_id"), "extra_helper")
                self.assertIsNotNone(evt.get("resource"))

            failed = [e for e in events if e.get("event") == "lifecycle.add_failed"]
            self.assertEqual(len(failed), 1, f"expected exactly one lifecycle.add_failed event; got {failed}")
            evt = failed[0]
            self.assertEqual(evt.get("agent_id"), "extra_helper")
            self.assertEqual(evt.get("failed_step"), "start_agent",
                             f"failed_step must point at start_agent (where the injection landed); got {evt}")
            self.assertEqual(evt.get("failed_resource"), "extra_helper")
            self.assertIn("injected add-agent startup failure", evt.get("reason", ""))
            self.assertEqual(set(evt.get("rolled_back") or []), {"spec_yaml", "workspace_state", "team_state_md", "role_file"})
            self.assertEqual(evt.get("rollback_errors") or [], [])
            # cleared_locations captures the steps that DID succeed before the failure landed.
            self.assertEqual(set(evt.get("cleared_locations") or []),
                             {"role_file", "compile_role_doc", "spec_yaml", "team_state_md"},
                             f"cleared_locations should list steps completed before the failure; got {evt}")

    def test_gap15_add_failed_event_carries_resource_ids(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap15-add-resource-ids-") as tmp:
            workspace = Path(tmp)
            spec, role_file = _write_add_agent_workspace(workspace)

            def fake_compile(_path: Path, _team_dir: Path, agent_id: str) -> dict:
                agent = copy.deepcopy(spec["agents"][0])
                agent["id"] = agent_id
                agent["role"] = "Extra helper"
                agent["model"] = "gpt-nonexistent-9999"
                return agent

            def failing_start(_ws: Path, agent_id: str, **_kwargs: object) -> dict:
                raise TeamAgentRuntimeError(
                    f"model_check refused: model gpt-nonexistent-9999 is not a known model for agent {agent_id}"
                )

            with (
                patch("team_agent.compiler.compile_role_doc_agent", side_effect=fake_compile),
                patch("team_agent.runtime.start_agent", side_effect=failing_start),
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.add_agent(workspace, "broken_helper", role_file_path=str(role_file), open_display=False)

            events = _events(workspace)
            failed = [e for e in events if e.get("event") == "lifecycle.add_failed"]
            self.assertEqual(len(failed), 1)
            evt = failed[0]
            self.assertEqual(evt.get("agent_id"), "broken_helper")
            self.assertEqual(evt.get("failed_step"), "start_agent")
            self.assertEqual(evt.get("failed_resource"), "broken_helper")
            self.assertIn("model_check refused", evt.get("reason", ""))
            self.assertIn("gpt-nonexistent-9999", evt.get("reason", ""))
            # rolled_back must name every storage path the rollback handler touched.
            self.assertIn("spec_yaml", evt.get("rolled_back") or [])
            self.assertIn("workspace_state", evt.get("rolled_back") or [])
            self.assertIn("role_file", evt.get("rolled_back") or [])
            # Per-step rolled-back events must also be present, one per cleared storage path.
            per_step = [e for e in events if e.get("event") == "lifecycle.add_step_rolled_back"]
            self.assertEqual({e.get("step") for e in per_step}, {"spec_yaml", "workspace_state", "team_state_md", "role_file"})
            # Step events must carry the actual resource id (file path or state key), not just a label.
            spec_evt = next(e for e in per_step if e.get("step") == "spec_yaml")
            self.assertTrue(spec_evt.get("resource", "").endswith("team.spec.yaml"))
            role_evt = next(e for e in per_step if e.get("step") == "role_file")
            self.assertTrue(role_evt.get("resource", "").endswith("broken_helper.md"))

    def test_team_state_md_rolled_back_on_add_agent_failure(self) -> None:
        # Stage 11.11 (Gap 15 follow-up): Mac mini Scenario 6 (run res_05b2592c011f) showed
        # rollback handler restored spec.yaml / state.json / dynamic-role-file but left
        # .team/current/team_state.md with the orphan entry. Pre-Stage-11.11 rollback path
        # never snapshotted or restored team_state.md.
        with tempfile.TemporaryDirectory(prefix="team-agent-gap15-team-state-rollback-") as tmp:
            workspace = Path(tmp)
            spec, role_file = _write_add_agent_workspace(workspace)
            team_state_path = workspace / "team_state.md"
            # Seed a known pre-state so byte-equality is meaningful.
            seed_text = "# Pre-add canonical content\nseed-line-A\nseed-line-B\n"
            team_state_path.write_text(seed_text, encoding="utf-8")
            seed_bytes = team_state_path.read_bytes()

            def fake_compile(_path: Path, _team_dir: Path, agent_id: str) -> dict:
                agent = copy.deepcopy(spec["agents"][0])
                agent["id"] = agent_id
                agent["role"] = "Extra helper"
                return agent

            def failing_start(_ws: Path, agent_id: str, **_kwargs: object) -> dict:
                raise TeamAgentRuntimeError("injected post-team-state failure")

            with (
                patch("team_agent.compiler.compile_role_doc_agent", side_effect=fake_compile),
                patch("team_agent.runtime.start_agent", side_effect=failing_start),
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.add_agent(workspace, "extra_helper", role_file_path=str(role_file), open_display=False)

            # After failure the markdown MUST be byte-equal to the seeded pre-state.
            self.assertEqual(team_state_path.read_bytes(), seed_bytes,
                             "team_state.md must be restored byte-equal after rollback")
            events = _events(workspace)
            rolled_back_steps = [e for e in events
                                 if e.get("event") == "lifecycle.add_step_rolled_back"
                                 and e.get("step") == "team_state_md"]
            self.assertEqual(len(rolled_back_steps), 1,
                             f"expected lifecycle.add_step_rolled_back step=team_state_md; got events={rolled_back_steps}")
            self.assertEqual(rolled_back_steps[0].get("agent_id"), "extra_helper")
            self.assertTrue(str(rolled_back_steps[0].get("resource") or "").endswith("team_state.md"))
            # The terminal failure event must list team_state_md among rolled_back locations.
            failed = next(e for e in events if e.get("event") == "lifecycle.add_failed")
            self.assertIn("team_state_md", failed.get("rolled_back") or [])


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
