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

class RuntimeTests02(unittest.TestCase):
    def test_profile_init_doctor_and_preflight_are_secret_safe(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-") as tmp:
            workspace = Path(tmp)
            init = init_profile(workspace, "codex-default", "subscription")
            self.assertTrue(init["ok"])
            self.assertFalse(init["secret_written"])
            self.assertTrue((workspace / ".team" / "current" / "profiles" / "codex-default.env").exists())
            boundary = workspace / ".team" / "current" / "profiles" / "AGENTS.md"
            self.assertIn("Do not read", boundary.read_text(encoding="utf-8"))
            claude_boundary = workspace / ".team" / "current" / "profiles" / "CLAUDE.md"
            self.assertIn("Do not read", claude_boundary.read_text(encoding="utf-8"))
            team = _write_doc_team(workspace)
            doctor = doctor_profile(workspace, "codex-default")
            self.assertTrue(doctor["ok"])
            self.assertFalse(doctor["secret_values_printed"])
            self.assertFalse(doctor["raw_file_read_allowed_for_agents"])
            real_profile = team / "profiles" / "codex-default.env"
            real_profile.write_text(
                "AUTH_MODE=subscription\nAPI_KEY=sk-do-not-print\nBASE_URL=https://user:url-password-do-not-print@example.com/v1?api_key=sk-do-not-print\n",
                encoding="utf-8",
            )
            doctor = doctor_profile(workspace, "codex-default")
            self.assertNotIn("sk-do-not-print", json.dumps(doctor))
            show = show_profile(workspace, "codex-default")
            self.assertTrue(show["safe_for_agent_context"])
            self.assertFalse(show["raw_file_read_allowed_for_agents"])
            self.assertEqual(show["values"]["API_KEY"], {"present": True, "redacted": True})
            self.assertNotIn("sk-do-not-print", json.dumps(show))
            self.assertNotIn("url-password-do-not-print", json.dumps(show))
            self.assertEqual(show["values"]["BASE_URL"]["value"], "https://[redacted]@example.com/v1")
            out = workspace / "team.spec.yaml"
            compile_team(team, out)
            self.assertNotIn("sk-do-not-print", out.read_text(encoding="utf-8"))
            preflight = runtime.preflight(team)
            self.assertIn("summary", preflight)
            self.assertIn("next_actions", preflight)
            self.assertIn("details_log", preflight)

    def test_compatible_profile_template_requires_local_values_before_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-compatible-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            init = init_profile(workspace, "deepseek", "compatible_api")
            self.assertTrue(init["ok"])
            profile_text = Path(init["path"]).read_text(encoding="utf-8")
            self.assertIn("BASE_URL=\n", profile_text)
            self.assertIn("API_KEY=\n", profile_text)
            self.assertIn("MODEL=\n", profile_text)
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8")
                .replace("provider: codex\n", "provider: claude_code\n")
                .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
                .replace("profile: codex-default\n", "profile: deepseek\n"),
                encoding="utf-8",
            )
            result = runtime.preflight(team)
            profiles = next(check for check in result["checks"] if check["name"] == "profiles")
            self.assertFalse(profiles["ok"])
            implementer = next(item for item in profiles["checks"] if item["agent_id"] == "implementer")
            self.assertIn("BASE_URL", implementer["missing_required"])
            self.assertIn("API_KEY", implementer["missing_required"])
            self.assertTrue(any("profile show <name>" in action for action in result["next_actions"]))
            self.assertTrue(any("must not read .team/*/profiles/*.env" in action for action in result["next_actions"]))
            self.assertNotIn("sk-", json.dumps(result))

    def test_profile_model_satisfies_compatible_role_doc_without_role_model(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-model-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            profile_dir = team / "profiles"
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            role.write_text(
                """---
name: implementer
role: Implementation Engineer
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Implement bounded tasks and report result_envelope_v1.
""",
                encoding="utf-8",
            )
            compiled = compile_team(team, workspace / "team.spec.yaml")["spec"]
            self.assertIsNone(compiled["agents"][0]["model"])
            with patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"):
                preflight = runtime.preflight(team)
            self.assertTrue(preflight["ok"])
            profiles = next(check for check in preflight["checks"] if check["name"] == "profiles")
            implementer = next(item for item in profiles["checks"] if item["agent_id"] == "implementer")
            self.assertEqual(implementer["effective_model"], "deepseek-v4-flash")
            self.assertEqual(implementer["model_source"], "profile")

    def test_validate_accepts_team_directory_role_docs(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-validate-dir-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)

            result = runtime.validate_file(team)

            self.assertTrue(result["ok"])
            self.assertEqual(result["type"], "team_dir")
            self.assertEqual(result["team"], "doc-team")
            self.assertEqual(result["agents"], ["implementer"])

    def test_preflight_treats_missing_rust_core_as_python_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-preflight-python-fallback-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: fallback-team
objective: Preflight without rust core.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "agents" / "fake.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake
auth_mode: subscription
tools:
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.core_binary", return_value=None),
            ):
                result = runtime.preflight(team)

            self.assertTrue(result["ok"], result)
            rust = next(check for check in result["checks"] if check["name"] == "rust_core")
            self.assertEqual(rust["status"], "python_fallback")

    def test_role_model_mismatch_with_profile_model_fails_preflight(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-model-mismatch-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            profile_dir = team / "profiles"
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8")
                .replace("provider: codex\n", "provider: claude_code\n")
                .replace("model: gpt-5.5\n", "model: deepseek-chat\n")
                .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
                .replace("profile: codex-default\n", "profile: deepseek\n"),
                encoding="utf-8",
            )
            preflight = runtime.preflight(team)
            self.assertFalse(preflight["ok"])
            self.assertIn("does not match profile MODEL", json.dumps(preflight, ensure_ascii=False))

    def test_profile_env_is_sourced_without_leaking_secret_in_launch_command(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-env-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            profile_launch = prepare_agent_profile_launch(workspace, agent)
            self.assertEqual(
                profile_launch["claude_projects_root"],
                str(workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude" / "projects"),
            )
            with patch.dict(
                os.environ,
                {
                    "HTTPS_PROXY": "https://claude:ambient-secret@proxy.example:8443",
                    "NODE_EXTRA_CA_CERTS": "/tmp/claude-proxy-ca.crt",
                },
            ):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_reviewer.env"
            self.assertTrue(env_path.exists())
            self.assertIn(str(env_path), command)
            self.assertIn("--model deepseek-v4-pro", command)
            self.assertNotIn("sk-do-not-print", command)
            self.assertNotIn("ambient-secret", command)
            env_text = env_path.read_text(encoding="utf-8")
            self.assertIn("unset ANTHROPIC_API_KEY", env_text)
            self.assertNotIn("unset HTTPS_PROXY", env_text)
            self.assertNotIn("unset NODE_EXTRA_CA_CERTS", env_text)
            self.assertIn("export CLAUDE_CONFIG_DIR=", env_text)
            config_dir = workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude"
            self.assertIn(str(config_dir), env_text)
            settings = json.loads((config_dir / "settings.json").read_text(encoding="utf-8"))
            self.assertEqual(settings["theme"], "auto")
            state = json.loads((config_dir / ".claude.json").read_text(encoding="utf-8"))
            self.assertTrue(state["hasCompletedOnboarding"])
            self.assertTrue(state["projects"][str(workspace)]["hasTrustDialogAccepted"])
            self.assertIn("export ANTHROPIC_AUTH_TOKEN=", env_text)
            self.assertNotIn("export ANTHROPIC_API_KEY=", env_text)
            self.assertNotIn("ambient-secret", env_text)
            self.assertEqual(
                agent["_provider_profile"]["claude_projects_root"],
                str(config_dir / "projects"),
            )

    def test_compatible_claude_mcp_is_persisted_in_managed_config(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-compatible-mcp-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            mcp_config = get_adapter("claude_code").mcp_config(workspace, "claude_reviewer")

            command = shell_command_for_agent(agent, workspace, mcp_config)

            self.assertNotIn("--mcp-config", command)
            self.assertNotIn("--strict-mcp-config", command)
            self.assertNotIn("sk-do-not-print", command)
            config_dir = workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude"
            state = json.loads((config_dir / ".claude.json").read_text(encoding="utf-8"))
            project = state["projects"][str(workspace)]
            self.assertIn(str(workspace.resolve()), state["projects"])
            self.assertTrue(project["hasTrustDialogAccepted"])
            server = project["mcpServers"]["team_orchestrator"]
            self.assertEqual(server["env"]["TEAM_AGENT_ID"], "claude_reviewer")
            self.assertEqual(server["args"][:2], ["-m", "team_agent.mcp_server"])

    def test_subscription_claude_keeps_strict_command_line_mcp(self) -> None:
        adapter = get_adapter("claude_code")
        agent = _provider_agent("claude_code", "claude_reviewer")
        agent["auth_mode"] = "subscription"
        agent["_runtime"] = {}
        with tempfile.TemporaryDirectory(prefix="team-agent-subscription-mcp-") as tmp:
            workspace = Path(tmp)
            cmd = adapter.build_command(agent, workspace, adapter.mcp_config(workspace, "claude_reviewer"))

        self.assertIn("--mcp-config", cmd)
        self.assertIn("--strict-mcp-config", cmd)

    def test_attach_profile_resume_root_uses_current_compatible_claude_config(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-resume-root-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "coder")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            previous = {"session_id": "old-session", "claude_projects_root": str(Path.home() / ".claude" / "projects")}

            prepared = runtime._attach_profile_resume_root(workspace, agent, previous)

            expected = workspace / ".team" / "runtime" / "provider-config" / "coder" / "claude" / "projects"
            self.assertEqual(prepared["claude_projects_root"], str(expected))
            self.assertEqual(agent["_provider_profile"]["claude_projects_root"], str(expected))

    def test_compatible_profile_direct_proxy_mode_unsets_native_proxy_environment(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-direct-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROXY_MODE=direct",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            with patch.dict(
                os.environ,
                {
                    "HTTPS_PROXY": "https://claude:ambient-secret@proxy.example:8443",
                    "NODE_EXTRA_CA_CERTS": "/tmp/claude-proxy-ca.crt",
                },
            ):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_reviewer.env"
            self.assertTrue(env_path.exists())
            self.assertNotIn("ambient-secret", command)
            env_text = env_path.read_text(encoding="utf-8")
            self.assertIn("unset HTTPS_PROXY", env_text)
            self.assertIn("unset https_proxy", env_text)
            self.assertIn("unset NODE_EXTRA_CA_CERTS", env_text)
            self.assertIn("export CLAUDE_CONFIG_DIR=", env_text)
            self.assertNotIn("ambient-secret", env_text)

    def test_subscription_profile_keeps_native_settings_environment(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-native-") as tmp:
            workspace = Path(tmp)
            init_profile(workspace, "claude-native", "subscription")
            agent = _provider_agent("claude_code", "claude_native")
            agent["auth_mode"] = "subscription"
            agent["profile"] = "claude-native"
            with patch.dict(os.environ, {"HTTPS_PROXY": "https://claude:keep-native-proxy@proxy.example:8443"}):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_native.env"
            self.assertFalse(env_path.exists())
            self.assertNotIn("unset HTTPS_PROXY", command)
            self.assertNotIn("keep-native-proxy", command)


if __name__ == "__main__":
    unittest.main(verbosity=2)
