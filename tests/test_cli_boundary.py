from __future__ import annotations

import unittest


LEGACY_CLI_EXPORTS = (
    "SEND_ORDER_HINT",
    "TeamAgentArgumentParser",
    "main",
    "add_json",
    "_run_leader_passthrough",
    "emit",
    "_workspace_from_args",
    "_emit_cli_error",
    "_cli_error_payload",
    "_tmux_session_conflict_name",
    "_tmux_session_conflict_next_action",
    "_tmux_session_conflict_action",
    "cmd_quick_start",
    "cmd_codex",
    "cmd_claude",
    "_provider_args",
    "_leader_launcher_args",
    "cmd_init",
    "cmd_validate",
    "cmd_compile",
    "_profile_scope",
    "cmd_profile_init",
    "cmd_profile_doctor",
    "cmd_profile_show",
    "cmd_launch",
    "cmd_preflight",
    "cmd_start",
    "cmd_wait_ready",
    "cmd_settle",
    "cmd_status",
    "cmd_approvals",
    "cmd_peek",
    "cmd_inbox",
    "cmd_sessions",
    "cmd_attach_leader",
    "cmd_send",
    "cmd_collect",
    "cmd_diagnose",
    "cmd_repair_state",
    "cmd_validate_result",
    "cmd_doctor",
    "cmd_shutdown",
    "cmd_restart",
    "cmd_start_agent",
    "cmd_stop_agent",
    "cmd_reset_agent",
    "cmd_add_agent",
    "cmd_fork_agent",
    "cmd_remove_agent",
    "cmd_stuck_list",
    "cmd_stuck_cancel",
    "cmd_acknowledge_idle",
    "cmd_allow_peer_talk",
    "cmd_advanced",
    "cmd_install_skill",
    "_skill_dest_dir",
    "_install_skill_to",
    "cmd_e2e",
    "_run_fake_e2e",
    "_run_real_launch_smoke",
    "_fake_spec",
)


class CliBoundaryTests(unittest.TestCase):
    def test_package_reexports_legacy_cli_surface(self) -> None:
        from team_agent import cli
        from team_agent.cli import commands, e2e, helpers, parser

        for name in LEGACY_CLI_EXPORTS:
            self.assertIn(name, cli.__all__)
            self.assertTrue(hasattr(cli, name), name)
        self.assertIs(cli.main, parser.main)
        self.assertIs(cli.cmd_status, commands.cmd_status)
        self.assertIs(cli.cmd_e2e, e2e.cmd_e2e)
        self.assertIs(cli._cli_error_payload, helpers._cli_error_payload)


if __name__ == "__main__":
    unittest.main()
