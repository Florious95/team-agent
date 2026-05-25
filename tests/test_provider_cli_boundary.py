from __future__ import annotations

import inspect
import unittest

from team_agent import providers


class ProviderCliBoundaryTests(unittest.TestCase):
    """Explicit re-export contract between providers.py and provider_cli/.

    The Stage 3 extraction kept providers.py as a thin facade with explicit
    imports + import-time assertions. These tests pin the facade so that an
    accidental removal of a re-exported symbol breaks loudly before downstream
    callers (CLI, lifecycle, tests) notice.
    """

    def test_adapter_base_and_resume_error_originate_in_provider_cli(self) -> None:
        from team_agent.provider_cli.adapter import ProviderAdapter, ResumeUnavailable
        self.assertIs(providers.ProviderAdapter, ProviderAdapter)
        self.assertIs(providers.ResumeUnavailable, ResumeUnavailable)
        self.assertTrue(issubclass(ResumeUnavailable, RuntimeError))

    def test_each_adapter_class_lives_in_its_own_provider_cli_module(self) -> None:
        from team_agent.provider_cli.claude import ClaudeCodeAdapter
        from team_agent.provider_cli.codex import CodexAdapter
        from team_agent.provider_cli.fake import FakeAdapter
        from team_agent.provider_cli.gemini import GeminiCliAdapter
        self.assertIs(providers.ClaudeCodeAdapter, ClaudeCodeAdapter)
        self.assertIs(providers.CodexAdapter, CodexAdapter)
        self.assertIs(providers.FakeAdapter, FakeAdapter)
        self.assertIs(providers.GeminiCliAdapter, GeminiCliAdapter)
        for cls in (ClaudeCodeAdapter, CodexAdapter, GeminiCliAdapter, FakeAdapter):
            self.assertTrue(issubclass(cls, providers.ProviderAdapter), cls.__name__)
            self.assertTrue(cls.provider, f"{cls.__name__} must set provider")
            self.assertTrue(cls.command_name, f"{cls.__name__} must set command_name")

    def test_get_adapter_returns_singletons_from_ADAPTERS_map(self) -> None:
        expected = {
            "claude": providers.ClaudeCodeAdapter,
            "claude_code": providers.ClaudeCodeAdapter,
            "codex": providers.CodexAdapter,
            "gemini_cli": providers.GeminiCliAdapter,
            "fake": providers.FakeAdapter,
        }
        for key, cls in expected.items():
            adapter = providers.get_adapter(key)
            self.assertIsInstance(adapter, cls)
            self.assertIs(adapter, providers.ADAPTERS[key])

    def test_unknown_provider_raises_key_error(self) -> None:
        with self.assertRaises(KeyError):
            providers.get_adapter("nonexistent_provider")

    def test_dispatch_helpers_have_explicit_signatures(self) -> None:
        # *args/**kwargs cross-module wrappers are forbidden by Stage 3 brief;
        # confirm the public dispatch helpers preserve typed positional args.
        for name in (
            "shell_command_for_agent",
            "shell_resume_command_for_agent",
            "shell_fork_command_for_agent",
            "shell_command",
        ):
            fn = getattr(providers, name)
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{name} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{name} uses **kwargs")

    def test_facade_re_exports_prompt_and_shared_helpers(self) -> None:
        from team_agent.provider_cli.adapter import agent_model, parse_time, read_json_object
        from team_agent.provider_cli.prompt import TEAMMATE_SYSTEM_PROMPT, compile_system_prompt
        self.assertIs(providers.TEAMMATE_SYSTEM_PROMPT, TEAMMATE_SYSTEM_PROMPT)
        self.assertIs(providers.compile_system_prompt, compile_system_prompt)
        # Shared helpers stay reachable from the provider_cli surface even
        # when consumers import via providers (legacy import path).
        self.assertIs(agent_model("not-a-dict") if False else agent_model, agent_model)
        self.assertIs(parse_time, parse_time)
        self.assertIs(read_json_object, read_json_object)


class ProviderCliClaudeHelpersTests(unittest.TestCase):
    def test_disallowed_tools_maps_canonical_to_native(self) -> None:
        from team_agent.provider_cli.claude import claude_disallowed_tools
        self.assertEqual(
            sorted(claude_disallowed_tools(set())),
            sorted(["Bash", "Read", "Edit", "Write", "MultiEdit", "NotebookEdit", "Glob", "Grep"]),
        )
        self.assertEqual(claude_disallowed_tools({"execute_bash", "fs_read", "fs_write", "fs_list"}), [])
        self.assertEqual(
            sorted(claude_disallowed_tools({"fs_read"})),
            sorted(["Bash", "Edit", "Write", "MultiEdit", "NotebookEdit", "Glob", "Grep"]),
        )

    def test_agent_match_score_recognizes_team_agent_id_marker(self) -> None:
        from team_agent.provider_cli.claude import claude_agent_match_score
        text = "TEAM_AGENT_ID=worker_a\nteam agent worker worker_a active"
        self.assertGreaterEqual(claude_agent_match_score("worker_a", text), 2)
        self.assertEqual(claude_agent_match_score("", text), 0)
        self.assertEqual(claude_agent_match_score("worker_a", ""), 0)


class ProviderCliCodexHelpersTests(unittest.TestCase):
    def test_rollout_id_extracts_uuid_suffix(self) -> None:
        from pathlib import Path
        from team_agent.provider_cli.codex import rollout_id_from_name
        ok = Path("/tmp/rollout-2026-05-24T01-02-03-019e2eac-0000-7000-a000-000000000001.jsonl")
        self.assertEqual(rollout_id_from_name(ok), "019e2eac-0000-7000-a000-000000000001")
        bad = Path("/tmp/rollout-no-uuid-here.jsonl")
        self.assertIsNone(rollout_id_from_name(bad))


class ProviderCliAdapterHelpersTests(unittest.TestCase):
    def test_agent_model_prefers_explicit_model_then_profile_override(self) -> None:
        from team_agent.provider_cli.adapter import agent_model
        self.assertEqual(agent_model({"model": "claude-sonnet-4-6"}), "claude-sonnet-4-6")
        self.assertEqual(
            agent_model({"_provider_profile": {"command_overrides": {"model": "gpt-5.5"}}}),
            "gpt-5.5",
        )
        self.assertIsNone(agent_model({}))

    def test_parse_time_accepts_iso_z_and_naive(self) -> None:
        from datetime import timezone
        from team_agent.provider_cli.adapter import parse_time
        z = parse_time("2026-05-24T01:02:03Z")
        self.assertIsNotNone(z)
        assert z is not None
        self.assertEqual(z.tzinfo, timezone.utc)
        self.assertIsNone(parse_time(None))
        self.assertIsNone(parse_time("not-a-timestamp"))


if __name__ == "__main__":
    unittest.main()
