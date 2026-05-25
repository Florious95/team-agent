from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import team_agent.profiles as profiles


LEGACY_PROFILE_EXPORTS = (
    "AUTH_MODES",
    "PROFILE_KEY_RE",
    "SECRET_KEYS",
    "PROXY_ENV_KEYS",
    "CA_ENV_KEYS",
    "COMPATIBLE_API_NETWORK_ENV_KEYS",
    "PROFILE_SECRET_BOUNDARY_TEXT",
    "profile_dir",
    "_profile_lookup_dir",
    "_agent_profile_dir",
    "_safe_inspection_command",
    "_safe_init_command",
    "ensure_profile_secret_boundary_dir",
    "ensure_profile_secret_boundary",
    "init_profile",
    "doctor_profile",
    "show_profile",
    "known_profiles",
    "gitignore_patterns",
    "load_profile",
    "parse_env_file",
    "validate_agent_profile",
    "smoke_check_agent_profile",
    "required_profile_keys",
    "compact_profile_check",
    "prepare_agent_profile_launch",
    "effective_model",
    "_provider_env_exports",
    "_provider_env_unsets",
    "_provider_command_overrides",
    "_write_runtime_env_file",
    "_compatible_claude_config_dir",
    "ensure_compatible_claude_mcp_config",
    "_ensure_compatible_claude_config",
    "_claude_project_keys",
    "_read_json_object",
    "_write_json",
    "_compatible_api_network_exports",
    "_profile_proxy_mode",
    "_anthropic_compatible_smoke",
    "_openai_compatible_smoke",
    "_http_json_smoke",
    "_anthropic_messages_url",
    "_openai_chat_url",
    "_redacted_endpoint",
    "_proxy_info_for_endpoint",
    "_proxy_url_from_env",
    "_temporary_profile_network_env",
    "_redact_proxy_url",
    "_format_profile_check_failure",
    "_alternate_value",
    "_strip_env_value",
    "_safe_codex_provider_id",
    "_is_secret_key",
    "_safe_profile_value",
    "_common_missing_values",
    "_safe_plain_profile_value",
)


class _FakeResponse:
    status = 200

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *args: object) -> None:
        return None

    def read(self, limit: int) -> bytes:
        return b'{"ok":true}'


class ProfilesBoundaryTests(unittest.TestCase):
    def test_package_reexports_profile_surface(self) -> None:
        for name in profiles._REQUIRED_EXPORTS:
            self.assertIn(name, profiles.__all__)
            self.assertTrue(hasattr(profiles, name))

    def test_package_reexports_legacy_profile_module_names(self) -> None:
        for name in LEGACY_PROFILE_EXPORTS:
            self.assertIn(name, profiles.__all__)
            self.assertTrue(hasattr(profiles, name), name)

    def test_temporary_profile_network_env_is_contextmanager_used_by_smoke_probe(self) -> None:
        values = {
            "BASE_URL": "https://api.example.invalid/v1",
            "API_KEY": "secret",
            "MODEL": "test-model",
            "HTTPS_PROXY": "http://proxy.local:8080",
        }
        with profiles._temporary_profile_network_env(values):
            pass
        with patch("team_agent.profiles.urllib.request.urlopen", return_value=_FakeResponse()) as urlopen:
            result = profiles._openai_compatible_smoke(values, "test-model", {"provider": "codex"}, timeout=1.0)
        self.assertTrue(result["ok"])
        self.assertEqual(result["status"], "smoke_passed")
        self.assertEqual(urlopen.call_count, 1)

    def test_split_profile_modules_preserve_basic_flow(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-boundary-") as tmp:
            workspace = Path(tmp)
            created = profiles.init_profile(workspace, "codex-default", "compatible_api")
            self.assertTrue(created["ok"])
            profile_path = Path(created["path"])
            profile_path.write_text(
                "AUTH_MODE=compatible_api\nBASE_URL=https://example.invalid/v1\nAPI_KEY=secret\nMODEL=test-model\n",
                encoding="utf-8",
            )
            shown = profiles.show_profile(workspace, "codex-default")
            self.assertTrue(shown["ok"])
            self.assertTrue(shown["values"]["API_KEY"]["redacted"])
            agent = {
                "id": "worker",
                "provider": "codex",
                "profile": "codex-default",
                "auth_mode": "compatible_api",
            }
            validation = profiles.validate_agent_profile(workspace, agent)
            self.assertTrue(validation["ok"])
            self.assertEqual(profiles.effective_model(agent, workspace), "test-model")


if __name__ == "__main__":
    unittest.main()
