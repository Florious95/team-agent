from __future__ import annotations

import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from team_agent import runtime
from team_agent.cli import _fake_spec, main
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.message_store import MessageStore
from team_agent.spec import load_spec
from team_agent.state import load_runtime_state, save_runtime_state, write_spec, write_team_state


def _workspace_with_dynamic_agent(root: Path) -> tuple[Path, Path]:
    workspace = root
    spec = _fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-remove-agent"
    dynamic = dict(spec["agents"][0])
    dynamic["id"] = "temp_worker"
    dynamic["role"] = "temporary_worker"
    dynamic["preferred_for"] = ["temp_worker", "temporary_worker"]
    spec["agents"].append(dynamic)
    spec["runtime"]["startup_order"].append("temp_worker")
    spec_path = workspace / "team.spec.yaml"
    write_spec(spec_path, spec)
    role_file = workspace / ".team" / "dynamic-role-files" / "temp_worker.md"
    role_file.parent.mkdir(parents=True, exist_ok=True)
    role_file.write_text("---\nname: temp_worker\nrole: temporary_worker\nprovider: fake\ntools: []\n---\nTemporary worker.\n", encoding="utf-8")
    state = {
        "spec_path": str(spec_path),
        "workspace": str(workspace),
        "session_name": "team-remove-agent",
        "leader": spec["leader"],
        "agents": {
            "fake_impl": {"status": "stopped", "provider": "fake", "agent_id": "fake_impl", "window": "fake_impl"},
            "temp_worker": {
                "status": "stopped",
                "provider": "fake",
                "agent_id": "temp_worker",
                "window": "temp_worker",
                "dynamic_role_file": ".team/dynamic-role-files/temp_worker.md",
            },
        },
        "tasks": spec["tasks"],
        "display_backend": "none",
    }
    save_runtime_state(workspace, state)
    write_team_state(workspace, spec, state)
    MessageStore(workspace).upsert_agent_health("temp_worker", "IDLE")
    return spec_path, role_file


class RemoveAgentTests(unittest.TestCase):
    def test_cli_help_wires_remove_agent(self) -> None:
        out = io.StringIO()
        with self.assertRaises(SystemExit) as ctx, redirect_stdout(out):
            main(["--help"])
        self.assertEqual(ctx.exception.code, 0)
        self.assertIn("remove-agent", out.getvalue())

        out = io.StringIO()
        with self.assertRaises(SystemExit) as ctx, redirect_stdout(out):
            main(["remove-agent", "--help"])
        self.assertEqual(ctx.exception.code, 0)
        help_text = out.getvalue()
        self.assertIn("--from-spec", help_text)
        self.assertIn("--confirm", help_text)
        self.assertIn("--force", help_text)

    def test_remove_spec_native_refuses_without_confirm(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-remove-refuse-") as tmp:
            workspace = Path(tmp)
            _workspace_with_dynamic_agent(workspace)

            result = runtime.remove_agent(workspace, "fake_impl")

            self.assertFalse(result["ok"])
            self.assertEqual(result["reason"], "from_spec_confirm_required")
            self.assertIn("fake_impl", {agent["id"] for agent in load_spec(workspace / "team.spec.yaml")["agents"]})

    def test_remove_dynamic_agent_updates_storage_points(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-remove-dynamic-") as tmp:
            workspace = Path(tmp)
            spec_path, role_file = _workspace_with_dynamic_agent(workspace)

            result = runtime.remove_agent(workspace, "temp_worker")

            self.assertTrue(result["ok"])
            self.assertEqual(result["status"], "removed")
            spec = load_spec(spec_path)
            self.assertNotIn("temp_worker", {agent["id"] for agent in spec["agents"]})
            self.assertNotIn("temp_worker", spec["runtime"]["startup_order"])
            self.assertNotIn("temp_worker", load_runtime_state(workspace)["agents"])
            self.assertFalse(role_file.exists())
            self.assertNotIn("temp_worker", MessageStore(workspace).agent_health())
            state_text = (workspace / "team_state.md").read_text(encoding="utf-8")
            self.assertNotIn("temp_worker", state_text)

            events = [
                json.loads(line)
                for line in (workspace / ".team" / "logs" / "events.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            self.assertTrue(any(event["event"] == "remove_agent.complete" for event in events))

    def test_force_remove_stop_failure_does_not_restore_never_stopped_worker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-remove-stop-fails-") as tmp:
            workspace = Path(tmp)
            spec_path, role_file = _workspace_with_dynamic_agent(workspace)
            state = load_runtime_state(workspace)
            state["agents"]["temp_worker"]["status"] = "running"
            save_runtime_state(workspace, state)
            before_spec = spec_path.read_text(encoding="utf-8")
            before_state = load_runtime_state(workspace)
            before_team_state = (workspace / "team_state.md").read_text(encoding="utf-8")

            with (
                patch("team_agent.runtime.stop_agent", side_effect=RuntimeError("stop failed")),
                patch("team_agent.runtime.start_agent") as start_agent,
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.remove_agent(workspace, "temp_worker", force=True)

            start_agent.assert_not_called()
            self.assertEqual(spec_path.read_text(encoding="utf-8"), before_spec)
            self.assertEqual(load_runtime_state(workspace), before_state)
            self.assertEqual((workspace / "team_state.md").read_text(encoding="utf-8"), before_team_state)
            self.assertTrue(role_file.exists())
            self.assertIn("temp_worker", MessageStore(workspace).agent_health())

    def test_success_event_log_failure_returns_warning_after_storage_commit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-remove-event-fails-") as tmp:
            workspace = Path(tmp)
            spec_path, role_file = _workspace_with_dynamic_agent(workspace)

            def fake_write(_self, event: str, **_fields: object) -> None:
                if event == "remove_agent.complete":
                    raise OSError("event log unavailable")

            with patch("team_agent.events.EventLog.write", new=fake_write):
                result = runtime.remove_agent(workspace, "temp_worker")

            self.assertTrue(result["ok"])
            self.assertIn("success event logging failed", result["warning"])
            self.assertNotIn("temp_worker", {agent["id"] for agent in load_spec(spec_path)["agents"]})
            self.assertNotIn("temp_worker", load_runtime_state(workspace)["agents"])
            self.assertFalse(role_file.exists())
            self.assertNotIn("temp_worker", MessageStore(workspace).agent_health())


if __name__ == "__main__":
    unittest.main(verbosity=2)
