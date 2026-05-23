from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

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

class RuntimeTests05(unittest.TestCase):
    def test_coordinator_process_start_stop_restart_and_self_kill(self) -> None:
        import warnings

        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-proc-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                first = runtime.start_coordinator(workspace)
            self.assertTrue(first["ok"])
            self.assertTrue(runtime.coordinator_health(workspace)["ok"])
            stopped = runtime.stop_coordinator(workspace)
            self.assertTrue(stopped["ok"])
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                second = runtime.start_coordinator(workspace)
            self.assertTrue(second["ok"])
            self.assertNotEqual(first["pid"], second["pid"])
            runtime.stop_coordinator(workspace)

        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-selfkill-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "definitely-missing-session",
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                started = runtime.start_coordinator(workspace)
            self.assertTrue(started["ok"])
            import time

            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and runtime.coordinator_health(workspace)["ok"]:
                time.sleep(0.1)
            self.assertFalse(runtime.coordinator_health(workspace)["ok"])

    def test_peer_talk_default_allows_team_scoped_target(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-peer-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            second = copy.deepcopy(spec["agents"][0])
            second["id"] = "fake_peer"
            spec["agents"].append(second)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_peer": {"status": "running", "provider": "fake", "window": "fake_peer"},
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\nfake_peer\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "msg_"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                allowed = runtime.send_message(workspace, "fake_peer", "peer hello", sender="fake_impl", wait_visible=False)
            self.assertTrue(allowed["ok"])
            self.assertEqual(allowed["status"], "injected")
            self.assertFalse([e for e in _events(workspace) if e["event"] == "send.peer_rejected"])

    def test_quick_start_accepts_loose_role_doc_directory(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-start-") as tmp:
            workspace = Path(tmp)
            roles = workspace / "roles"
            roles.mkdir()
            (roles / "TEAM.md").write_text(
                """---
name: quick-test
objective: Team config only.
display_backend: none
fast: false
---

This file is not an agent role.
""",
                encoding="utf-8",
            )
            (roles / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(roles, name="quick-test")
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"])
                self.assertIn("team quick-test ready", result["summary"])
                self.assertIn("Do not wait, sleep, or poll status", result["ready_signal"])
                self.assertIn("Dispatch work with team-agent send", result["next_actions"][0])
                self.assertTrue((workspace / ".team" / "current" / "TEAM.md").exists())
                self.assertFalse((workspace / ".team" / "current" / "agents" / "TEAM.md").exists())
                self.assertTrue((workspace / ".team" / "current" / "profiles" / "fake-default.example.env").exists())
                self.assertTrue((workspace / ".team" / "current" / "team.spec.yaml").exists())
                self.assertTrue((workspace / ".team" / "current" / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                self.assertFalse((workspace / "team_state.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((workspace / ".team" / "current" / "team.spec.yaml").resolve()))
                self.assertIn("fake_impl", state["agents"])
                self.assertNotIn("leader", state["agents"])
            finally:
                runtime.shutdown(workspace)

    def test_quick_start_accepts_team_root_agents_layout_without_leader_worker(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-skill-example-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: skill-example
objective: Minimal example team.
dangerous_auto_approve: false
display_backend: none
fast: false
---

Team config only.
""",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Coder
provider: fake
model: fake
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Coder role.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"])
                self.assertEqual(result["spec"], str((workspace / ".team" / "current" / "team.spec.yaml").resolve()))
                self.assertTrue((workspace / ".team" / "current" / "team.spec.yaml").exists())
                self.assertTrue((workspace / ".team" / "current" / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                self.assertFalse((workspace / "team_state.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(sorted(state["agents"]), ["coder"])
            finally:
                runtime.shutdown(workspace)

    def test_quick_start_refuses_to_overwrite_existing_context_without_fresh(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-start-existing-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(team / "team.spec.yaml"),
                    "workspace": str(workspace),
                    "session_name": "team-doc-team",
                    "leader": {"provider": "codex"},
                    "agents": {
                        "implementer": {
                            "status": "stopped",
                            "provider": "codex",
                            "agent_id": "implementer",
                            "window": "implementer",
                            "session_id": "old-session",
                        }
                    },
                    "tasks": [],
                },
            )
            cwd = os.getcwd()
            os.chdir(workspace)
            try:
                result = runtime.quick_start(team)
            finally:
                os.chdir(cwd)

            self.assertFalse(result["ok"])
            self.assertEqual(result["step"], "existing_runtime_state")
            self.assertEqual(result["session_name"], "team-doc-team")
            self.assertTrue(any("team-agent restart" in action for action in result["next_actions"]))
            self.assertTrue(any("--fresh" in action for action in result["next_actions"]))

    def test_quick_start_team_id_stores_loose_docs_outside_current(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-team-id-") as tmp:
            workspace = Path(tmp)
            roles = workspace / "roles"
            roles.mkdir()
            (roles / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (roles / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(roles, team_id="alpha")
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"], result)
                self.assertEqual(result["team_dir"], str((workspace / ".team" / "alpha").resolve()))
                self.assertTrue((workspace / ".team" / "alpha" / "TEAM.md").exists())
                self.assertFalse((workspace / ".team" / "current" / "TEAM.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((workspace / ".team" / "alpha" / "team.spec.yaml").resolve()))
            finally:
                runtime.shutdown(workspace)

    def test_start_writes_compiled_spec_inside_selected_team_dir(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-start-team-dir-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "alpha"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "fake-default.example.env").write_text(
                "AUTH_MODE=subscription\nPROFILE_NAME=fake-default\n",
                encoding="utf-8",
            )
            (team / "agents" / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            try:
                result = runtime.start(team, yes=True)
                self.assertTrue(result["ok"], result)
                self.assertEqual(result["spec"], str((team / "team.spec.yaml").resolve()))
                self.assertTrue((team / "team.spec.yaml").exists())
                self.assertTrue((team / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((team / "team.spec.yaml").resolve()))
            finally:
                runtime.shutdown(workspace)

    def test_preflight_uses_selected_team_profile_dir_not_current(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-team-dir-") as tmp:
            workspace = Path(tmp)
            current_profiles = workspace / ".team" / "current" / "profiles"
            current_profiles.mkdir(parents=True)
            (current_profiles / "shared.env").write_text(
                "AUTH_MODE=compatible_api\nPROFILE_NAME=shared\nPROFILE_SMOKE=false\n",
                encoding="utf-8",
            )
            team = workspace / ".team" / "alpha"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "shared.env").write_text(
                "AUTH_MODE=compatible_api\nPROFILE_NAME=shared\nMODEL=alpha-model\nPROFILE_SMOKE=false\n",
                encoding="utf-8",
            )
            (team / "agents" / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
auth_mode: compatible_api
profile: shared
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )

            result = runtime.preflight(team)

            profiles = next(check for check in result["checks"] if check["name"] == "profiles")
            self.assertTrue(profiles["ok"], profiles)
            models = next(check for check in result["checks"] if check["name"] == "models")
            model = next(item for item in models["checks"] if item["agent_id"] == "fake_impl")
            self.assertEqual(model["model"], "alpha-model")


if __name__ == "__main__":
    unittest.main(verbosity=2)
