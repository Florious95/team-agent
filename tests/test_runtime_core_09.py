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

class RuntimeTests09(unittest.TestCase):
    def test_failed_result_consumes_retry_then_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["retry_limit"] = 1
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            result_path = workspace / "failed-result.json"

            result_path.write_text(json.dumps(_result_envelope("failed")), encoding="utf-8")
            first = runtime.collect(workspace, result_file=result_path)
            self.assertTrue(first["ok"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "needs_retry")
            self.assertEqual(state["tasks"][0]["retry_count"], 1)

            result_path.write_text(json.dumps(_result_envelope("failed")), encoding="utf-8")
            second = runtime.collect(workspace, result_file=result_path)
            self.assertTrue(second["ok"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "failed")
            self.assertEqual(state["tasks"][0]["retry_count"], 1)

    def test_diagnose_missing_session_and_mcp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "definitely-missing-session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.diagnose(workspace)
            kinds = {issue["kind"] for issue in result["issues"]}
            self.assertIn("tmux_session_missing", kinds)
            self.assertIn("mcp_not_installed", kinds)
            repair_kinds = {repair["kind"] for repair in result["suggested_repairs"]}
            self.assertIn("mcp_approval_prompt", repair_kinds)
            self.assertIn("leader_receiver", repair_kinds)

    def test_diagnose_reports_interrupted_worker_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-interrupted-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "interrupted", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.diagnose(workspace)
            self.assertIn("worker_interrupted", {issue["kind"] for issue in result["issues"]})
            self.assertIn("interrupted_worker", {repair["kind"] for repair in result["suggested_repairs"]})

    def test_diagnose_provider_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-provider-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            adapter = Mock(command_name="missing-fake")
            adapter.is_installed.return_value = False
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                result = runtime.diagnose(workspace)
            provider_issue = next(issue for issue in result["issues"] if issue["kind"] == "provider_missing")
            self.assertEqual(provider_issue["agent_id"], "fake_impl")
            self.assertEqual(provider_issue["command"], "missing-fake")

    def test_doctor_reports_missing_provider_auth(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-doctor-auth-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "claude_code"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock(command_name="claude")
            adapter.is_installed.return_value = True
            adapter.version.return_value = "Claude Code"
            adapter.auth_hint.return_value = {"status": "missing", "detail": "not logged in"}
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                result = runtime.doctor(spec_path)
            self.assertFalse(result["ok"])
            self.assertEqual(result["missing_provider_auth"], ["claude_code"])

    def test_claude_auth_hint_uses_cli_status(self) -> None:
        adapter = get_adapter("claude_code")
        proc = Mock(returncode=1, stdout='{"loggedIn": false, "authMethod": "none"}', stderr="")
        with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.providers.subprocess.run", return_value=proc):
            result = adapter.auth_hint()
        self.assertEqual(result["status"], "missing")
        self.assertIn("loggedIn", result["detail"])

    def test_mcp_json_rpc_tools_list(self) -> None:
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-") as tmp:
            proc = subprocess.Popen(
                [sys.executable, "-m", "team_agent.mcp_server", "--workspace", tmp],
                cwd=ROOT,
                text=True,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert proc.stdin is not None
            assert proc.stdout is not None
            try:
                proc.stdin.write('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}\n')
                proc.stdin.flush()
                init = json.loads(proc.stdout.readline())
                self.assertEqual(init["result"]["serverInfo"]["name"], "team_orchestrator")
                proc.stdin.write('{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n')
                proc.stdin.flush()
                tools = json.loads(proc.stdout.readline())
                by_name = {tool["name"]: tool for tool in tools["result"]["tools"]}
                names = set(by_name)
                self.assertIn("report_result", names)
                self.assertIn("assign_task", names)
                send_schema = by_name["send_message"]["inputSchema"]
                self.assertEqual(send_schema["required"], ["to", "content"])
                self.assertEqual(set(send_schema["properties"]), {"to", "content"})
                self.assertFalse(send_schema["additionalProperties"])
                report_schema = by_name["report_result"]["inputSchema"]
                self.assertEqual(report_schema["required"], ["summary"])
                self.assertNotIn("envelope", report_schema["properties"])
                self.assertFalse(report_schema["additionalProperties"])
            finally:
                proc.stdin.close()
                proc.stdout.close()
                if proc.stderr is not None:
                    proc.stderr.close()
                proc.kill()
                proc.wait(timeout=5)

    def test_compile_system_prompt_prepends_teammate_runtime_contract(self) -> None:
        agent = _provider_agent("codex", "codex_implementer")
        agent["system_prompt"]["inline"] = "ROLE_MARKER: review code."
        prompt = compile_system_prompt(agent)
        self.assertLess(prompt.index("Team Agent worker `codex_implementer`"), prompt.index("Team Agent Teammate Runtime Contract"))
        self.assertLess(prompt.index("Team Agent Teammate Runtime Contract"), prompt.index("ROLE_MARKER"))
        self.assertIn("role `reviewer`", prompt)
        self.assertIn("Plain text you write in this worker", prompt)
        self.assertIn("team_orchestrator.send_message(to='leader'", prompt)
        self.assertIn("to='*' to notify every other team member", prompt)
        self.assertIn("team_orchestrator.report_result exactly once", prompt)

    def test_provider_mcp_config_uses_local_python_module(self) -> None:
        config = get_adapter("codex").mcp_config(ROOT, "codex_implementer")["team_orchestrator"]
        self.assertIn("python", Path(config["command"]).name)
        self.assertEqual(config["args"][:2], ["-m", "team_agent.mcp_server"])
        self.assertIn("PYTHONPATH", config["env"])

    def test_worker_command_exports_current_path_for_codex_wrapper(self) -> None:
        from team_agent.providers import shell_command_for_agent

        agent = _provider_agent("fake", "fake_impl")
        with patch.dict(os.environ, {"PATH": "/Users/alauda/.local/bin:/opt/homebrew/bin"}):
            command = shell_command_for_agent(agent, ROOT, {})
        self.assertIn("PATH=/Users/alauda/.local/bin:/opt/homebrew/bin", command)
        self.assertNotIn("HTTPS_PROXY", command)

    def test_gemini_install_mcp_writes_settings_and_cleanup_restores(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gemini-mcp-") as tmp:
            home = Path(tmp) / "home"
            workspace = Path(tmp) / "workspace"
            settings_path = home / ".gemini" / "settings.json"
            settings_path.parent.mkdir(parents=True)
            settings_path.write_text(
                json.dumps(
                    {
                        "mcpServers": {
                            "team_orchestrator": {"command": "old", "args": [], "env": {}},
                            "unrelated": {"command": "keep", "args": [], "env": {}},
                        }
                    }
                ),
                encoding="utf-8",
            )
            adapter = get_adapter("gemini_cli")
            config = adapter.mcp_config(workspace, "gemini_researcher")
            with patch("team_agent.providers.Path.home", return_value=home):
                mcp_path = adapter.install_mcp(workspace, "gemini_researcher", config)
                settings = json.loads(settings_path.read_text(encoding="utf-8"))
                self.assertEqual(settings["mcpServers"]["team_orchestrator"]["args"][:2], ["-m", "team_agent.mcp_server"])
                self.assertEqual(settings["mcpServers"]["unrelated"]["command"], "keep")
                adapter.cleanup_mcp(workspace, "gemini_researcher", mcp_path)
            restored = json.loads(settings_path.read_text(encoding="utf-8"))
            self.assertEqual(restored["mcpServers"]["team_orchestrator"]["command"], "old")
            self.assertEqual(restored["mcpServers"]["unrelated"]["command"], "keep")

    def test_shutdown_restores_gemini_mcp_settings(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gemini-shutdown-") as tmp:
            home = Path(tmp) / "home"
            workspace = Path(tmp) / "workspace"
            settings_path = home / ".gemini" / "settings.json"
            settings_path.parent.mkdir(parents=True)
            settings_path.write_text(json.dumps({"mcpServers": {}}), encoding="utf-8")
            adapter = get_adapter("gemini_cli")
            config = adapter.mcp_config(workspace, "gemini_researcher")
            with patch("team_agent.providers.Path.home", return_value=home):
                mcp_path = adapter.install_mcp(workspace, "gemini_researcher", config)
                save_runtime_state(
                    workspace,
                    {
                        "spec_path": str(workspace / "team.spec.yaml"),
                        "session_name": None,
                        "agents": {
                            "gemini_researcher": {
                                "status": "running",
                                "provider": "gemini_cli",
                                "window": "gemini_researcher",
                                "mcp_config": str(mcp_path),
                            }
                        },
                        "tasks": [],
                    },
                )
                runtime.shutdown(workspace)
            settings = json.loads(settings_path.read_text(encoding="utf-8"))
            self.assertNotIn("team_orchestrator", settings["mcpServers"])

    def test_claude_default_command_avoids_dangerous_bypass(self) -> None:
        agent = _provider_agent("claude_code", "claude_reviewer")
        cmd = get_adapter("claude_code").build_command(agent, ROOT, {})
        self.assertNotIn("--dangerously-skip-permissions", cmd)
        self.assertIn("--permission-mode", cmd)
        self.assertIn("default", cmd)

    def test_claude_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        agent = _provider_agent("claude_code", "claude_reviewer")
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("claude_code").build_command(agent, ROOT, {})
        self.assertIn("--dangerously-skip-permissions", cmd)
        self.assertNotIn("--permission-mode", cmd)

    def test_gemini_default_command_avoids_dangerous_bypass(self) -> None:
        agent = _provider_agent("gemini_cli", "gemini_reviewer")
        cmd = get_adapter("gemini_cli").build_command(agent, ROOT, {})
        self.assertNotIn("--yolo", cmd)
        self.assertNotIn("--sandbox", cmd)

    def test_gemini_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        agent = _provider_agent("gemini_cli", "gemini_reviewer")
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("gemini_cli").build_command(agent, ROOT, {})
        self.assertIn("--yolo", cmd)
        self.assertIn("--sandbox", cmd)
        self.assertIn("false", cmd)

    def test_codex_default_command_avoids_dangerous_bypass(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        agent = next(a for a in spec["agents"] if a["id"] == "codex_implementer")
        cmd = get_adapter("codex").build_command(agent, ROOT, {})
        self.assertNotIn("--yolo", cmd)
        self.assertNotIn("--dangerously-bypass-approvals-and-sandbox", cmd)
        self.assertIn("--sandbox", cmd)
        self.assertIn("--ask-for-approval", cmd)
        self.assertIn("--disable", cmd)
        self.assertIn("apps", cmd)
        self.assertIn("shell_snapshot", cmd)

    def test_codex_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        agent = dict(next(a for a in spec["agents"] if a["id"] == "codex_implementer"))
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("codex").build_command(agent, ROOT, {})
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", cmd)
        self.assertNotIn("--yolo", cmd)

    def test_dangerous_auto_approve_visible_in_dry_run(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-safety-") as tmp:
            workspace = Path(tmp)
            spec = load_spec(ROOT / "examples" / "team.spec.yaml")
            spec["team"]["workspace"] = str(workspace)
            spec["runtime"]["dangerous_auto_approve"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            result = runtime.launch(spec_path, dry_run=True)
            self.assertTrue(result["safety"]["dangerous_auto_approve"])
            self.assertTrue(result["safety"]["requires_explicit_yes"])

    def test_leader_dangerous_permissions_detect_from_process_ancestry(self) -> None:
        with patch(
            "team_agent.runtime._process_ancestry",
            return_value=[
                {"pid": 10, "ppid": 9, "command": "python3 -m team_agent"},
                {"pid": 9, "ppid": 8, "command": "codex --dangerously-bypass-approvals-and-sandbox"},
            ],
        ):
            inherited = runtime._detect_inherited_dangerous_permissions()
        self.assertTrue(inherited["enabled"])
        self.assertEqual(inherited["provider"], "codex")
        self.assertEqual(inherited["flag"], "--dangerously-bypass-approvals-and-sandbox")

    def test_launch_inherits_leader_dangerous_permissions_in_dry_run(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-inherit-dangerous-") as tmp:
            workspace = Path(tmp)
            spec = load_spec(ROOT / "examples" / "team.spec.yaml")
            spec["team"]["workspace"] = str(workspace)
            spec["runtime"]["dangerous_auto_approve"] = False
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with patch(
                "team_agent.runtime._detect_inherited_dangerous_permissions",
                return_value={
                    "enabled": True,
                    "provider": "claude",
                    "flag": "--dangerously-skip-permissions",
                    "pid": 123,
                },
            ):
                result = runtime.launch(spec_path, dry_run=True)
            self.assertTrue(result["safety"]["dangerous_auto_approve"])
            self.assertTrue(result["safety"]["dangerous_auto_approve_inherited"])
            self.assertEqual(result["safety"]["dangerous_auto_approve_source"], "leader_process")
            self.assertFalse(result["safety"]["requires_explicit_yes"])

    def test_launch_session_conflict_guides_to_rename_not_shutdown(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-conflict-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-conflict"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
            ):
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    runtime.launch(spec_path, auto_approve=True)
            message = str(ctx.exception)
            self.assertIn("tmux session already exists: team-conflict", message)
            self.assertIn("will not terminate existing tmux sessions", message)
            self.assertIn("different team name", message)
            self.assertNotIn("team-agent shutdown", message)

    def test_launch_passes_inherited_dangerous_permissions_to_worker_runtime(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-inherit-runtime-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-inherit-runtime-" + workspace.name[-6:]
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.is_installed.return_value = True
            adapter.mcp_config.return_value = {}
            adapter.install_mcp.return_value = workspace / ".team/runtime/mcp/fake_impl.json"
            adapter.handle_startup_prompts.return_value = []
            captured_runtime: list[dict[str, Any]] = []

            def fake_shell_command(agent, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "codex",
                        "flag": "--dangerously-bypass-approvals-and-sandbox",
                    },
                ),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_shell_command),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="", stderr="")),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)
            self.assertTrue(launched["ok"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve_inherited"])
            self.assertEqual(captured_runtime[0]["dangerous_auto_approve_source"], "leader_process")


if __name__ == "__main__":
    unittest.main(verbosity=2)
