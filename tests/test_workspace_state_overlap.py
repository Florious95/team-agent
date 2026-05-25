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


def _team_spec(workspace: Path, team_id: str, agent_id: str) -> tuple[dict, Path]:
    spec = _fake_spec(workspace)
    spec["team"]["name"] = team_id
    spec["runtime"]["session_name"] = f"team-{team_id}"
    spec["agents"][0]["id"] = agent_id
    spec["agents"][0]["role"] = f"{team_id} worker"
    spec["runtime"]["startup_order"] = [agent_id]
    spec["routing"]["default_assignee"] = agent_id
    spec["routing"]["rules"] = []
    spec["tasks"][0]["id"] = f"task_{team_id}"
    spec["tasks"][0]["assignee"] = agent_id
    spec["context"]["state_file"] = f".team/{team_id}/team_state.md"
    team_dir = workspace / ".team" / team_id
    team_dir.mkdir(parents=True)
    spec_path = team_dir / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec, spec_path


class WorkspaceStateOverlapTests(unittest.TestCase):
    def test_second_team_does_not_implicitly_replace_first_team_active_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-state-overlap-") as tmp:
            workspace = Path(tmp)
            _alpha, alpha_spec_path = _team_spec(workspace, "alpha", "alpha_worker")
            _beta, beta_spec_path = _team_spec(workspace, "beta", "beta_worker")
            launched_sessions: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if args[-1] in launched_sessions else 1
                elif args[:3] == ["tmux", "new-session", "-d"]:
                    launched_sessions.add(args[args.index("-s") + 1])
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = ""
                return proc

            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                alpha_launch = runtime.launch(alpha_spec_path, auto_approve=True, skip_profile_smoke=True)
                alpha_state = copy.deepcopy(load_runtime_state(workspace))
                beta_launch = runtime.launch(beta_spec_path, auto_approve=True, skip_profile_smoke=True)

            self.assertTrue(alpha_launch["ok"])
            self.assertTrue(beta_launch["ok"])
            workspace_state = load_runtime_state(workspace)
            self.assertEqual(
                workspace_state.get("spec_path"),
                str(alpha_spec_path.resolve()),
                "starting Team B in the same workspace must not silently switch Team A's active state",
            )
            self.assertEqual(
                alpha_state["agents"],
                workspace_state.get("teams", {}).get("alpha", {}).get("agents"),
                "Team A state must remain readable without Team B agent data mixed in",
            )
            with patch("team_agent.runtime._deliver_pending_message", return_value={"ok": True, "status": "queued", "queued": True}):
                ambiguous = runtime.send_message(workspace, "alpha_worker", "must require explicit team target")
            self.assertFalse(ambiguous["ok"])
            self.assertEqual(ambiguous.get("reason"), "team_target_ambiguous")

    def test_send_with_explicit_team_preserves_sibling_team_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-sibling-") as tmp:
            workspace = Path(tmp)
            _alpha, alpha_spec_path = _team_spec(workspace, "alpha", "alpha_worker")
            _beta, beta_spec_path = _team_spec(workspace, "beta", "beta_worker")
            launched_sessions: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if args[-1] in launched_sessions else 1
                elif args[:3] == ["tmux", "new-session", "-d"]:
                    launched_sessions.add(args[args.index("-s") + 1])
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = ""
                return proc

            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                runtime.launch(alpha_spec_path, auto_approve=True, skip_profile_smoke=True)
                runtime.launch(beta_spec_path, auto_approve=True, skip_profile_smoke=True)
                before = load_runtime_state(workspace)
                beta_before = copy.deepcopy(before.get("teams", {}).get("beta"))
                self.assertIsNotNone(beta_before, "precondition: beta should exist before send")
                with patch("team_agent.runtime._deliver_pending_message", return_value={"ok": True, "status": "queued", "queued": True}):
                    sent = runtime.send_message(workspace, "alpha_worker", "hi alpha", team="alpha")

            self.assertTrue(sent.get("ok"), sent)
            after = load_runtime_state(workspace)
            self.assertIn("beta", after.get("teams", {}), "send --team alpha must not drop sibling team beta from workspace state")
            self.assertEqual(
                after["teams"]["beta"].get("agents"),
                beta_before.get("agents"),
                "send --team alpha must not mutate beta's agent state",
            )
            self.assertEqual(
                after["teams"]["beta"].get("session_name"),
                beta_before.get("session_name"),
                "send --team alpha must not mutate beta's session_name",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
