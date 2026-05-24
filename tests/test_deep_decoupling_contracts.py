from __future__ import annotations

import tempfile
import unittest
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class DeepDecouplingContractTests(unittest.TestCase):
    def test_line_count_guard_accepts_approved_exceptions_but_requires_empty_temporary_debt(self) -> None:
        from team_agent.quality_gates import check_python_file_line_counts, line_count_failures, load_line_count_allowlist

        allowlist_path = ROOT / "tests" / "line_count_allowlist.json"
        if allowlist_path.exists():
            payload = json.loads(allowlist_path.read_text(encoding="utf-8"))
            self.assertIsInstance(payload.get("approved_exceptions", {}), dict)
            self.assertEqual(payload.get("temporary_debt", {}), {}, "temporary_debt must be empty for completion")

        with tempfile.TemporaryDirectory(prefix="team-agent-empty-allowlist-") as tmp:
            root = Path(tmp)
            src = root / "src" / "team_agent"
            src.mkdir(parents=True)
            (src / "small.py").write_text("x = 1\n", encoding="utf-8")
            empty_allowlist_path = root / "line_count_allowlist.json"
            empty_allowlist_path.write_text('{"approved_exceptions": {}, "temporary_debt": {}}', encoding="utf-8")
            allowlist = load_line_count_allowlist(empty_allowlist_path)
            self.assertEqual(allowlist, {}, "quality gate fallback must treat empty temporary_debt as no debt")
            results = check_python_file_line_counts(root, allowlist_path=empty_allowlist_path)
        failures = line_count_failures(results)
        self.assertEqual(
            [(failure.path, failure.lines) for failure in failures],
            [],
            "All Python files must be at or below 500 lines without an allowlist",
        )

    def test_line_count_guard_rejects_unallowlisted_long_python_file(self) -> None:
        from team_agent.quality_gates import check_python_file_line_counts, line_count_failures

        with tempfile.TemporaryDirectory(prefix="team-agent-line-guard-") as tmp:
            root = Path(tmp)
            src = root / "src" / "team_agent"
            src.mkdir(parents=True)
            (src / "too_long.py").write_text("\n".join("x = 1" for _ in range(501)), encoding="utf-8")
            allowlist_path = root / "allowlist.json"
            allowlist_path.write_text('{"approved_exceptions": {}, "temporary_debt": {}}', encoding="utf-8")

            failures = line_count_failures(check_python_file_line_counts(root, allowlist_path=allowlist_path))
        self.assertEqual([(failure.path, failure.lines) for failure in failures], [("src/team_agent/too_long.py", 501)])

    def test_provider_cli_socket_has_explicit_unsupported_future_plugs(self) -> None:
        from team_agent.provider_cli import (
            CopilotCliPlug,
            OpenCodeCliPlug,
            PLUG_TYPES,
            ProviderCapabilityError,
            ProviderCliSocket,
            ProviderStartupInput,
            build_plug,
        )

        startup = ProviderStartupInput(
            agent_id="future_worker",
            provider="opencode",
            model=None,
            workspace=ROOT,
            system_prompt="",
        )
        opencode = OpenCodeCliPlug()
        copilot = CopilotCliPlug()
        self.assertEqual(set(PLUG_TYPES), {"opencode", "copilot"})
        self.assertIsInstance(build_plug("opencode"), OpenCodeCliPlug)
        self.assertIsInstance(build_plug("copilot"), CopilotCliPlug)
        self.assertIsInstance(opencode, ProviderCliSocket)
        self.assertIsInstance(copilot, ProviderCliSocket)
        self.assertEqual(opencode.provider, "opencode")
        self.assertEqual(copilot.provider, "copilot")

        with self.assertRaises(ProviderCapabilityError) as open_ctx:
            opencode.build_command(startup)
        self.assertEqual(open_ctx.exception.provider, "opencode")
        self.assertEqual(open_ctx.exception.capability, "start")

        with self.assertRaises(ProviderCapabilityError) as copilot_ctx:
            copilot.build_fork_command(startup, "source-session")
        self.assertEqual(copilot_ctx.exception.provider, "copilot")
        self.assertEqual(copilot_ctx.exception.capability, "fork_or_branch")

    def test_path_ownership_declares_three_lanes_and_shared_facades(self) -> None:
        ownership = json.loads((ROOT / "tests" / "path_ownership.json").read_text(encoding="utf-8"))
        lanes = ownership["lanes"]
        self.assertEqual(
            set(lanes),
            {"lifecycle-state-spec", "messaging-delivery-leader-receiver", "provider-session-display"},
        )
        self.assertIn("src/team_agent/lifecycle/**", lanes["lifecycle-state-spec"])
        self.assertIn("src/team_agent/messaging/**", lanes["messaging-delivery-leader-receiver"])
        self.assertIn("src/team_agent/message_store/**", lanes["messaging-delivery-leader-receiver"])
        self.assertIn("src/team_agent/provider_cli/**", lanes["provider-session-display"])
        self.assertEqual(
            set(ownership["integration_owner_only"]),
            {
                "src/team_agent/runtime.py",
                "src/team_agent/cli/**",
                "src/team_agent/mcp_server/**",
                "tests/run_tests.py",
                "tests/path_ownership.json",
            },
        )
