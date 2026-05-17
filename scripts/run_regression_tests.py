#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path


REGRESSION_TESTS = [
    "tests.run_tests.RuntimeTests.test_send_default_timeout_reports_submitted_unverified",
    "tests.run_tests.RuntimeTests.test_worker_delivery_retries_paste_until_message_ready",
    "tests.run_tests.RuntimeTests.test_worker_pasted_content_prompt_retries_enter_until_submitted",
    "tests.run_tests.RuntimeTests.test_worker_pasted_content_prompt_reports_unverified_when_enter_does_not_submit",
    "tests.run_tests.RuntimeTests.test_delivery_claim_prevents_duplicate_worker_injection",
    "tests.run_tests.RuntimeTests.test_approvals_returns_structured_prompt_without_terminal_page",
    "tests.run_tests.RuntimeTests.test_coordinator_auto_approves_internal_mcp_prompt_with_retry_verification",
    "tests.run_tests.RuntimeTests.test_coordinator_auto_approves_claude_internal_mcp_prompt",
    "tests.run_tests.RuntimeTests.test_stale_approval_prompt_in_scrollback_is_not_current_approval",
    "tests.run_tests.RuntimeTests.test_ghostty_display_session_is_linked_and_window_selected",
    "tests.run_tests.RuntimeTests.test_shutdown_closes_ghostty_display_session_before_base_session_without_pid",
    "tests.run_tests.RuntimeTests.test_restart_resumes_known_sessions_and_fresh_spawns_missing_sessions",
    "tests.run_tests.RuntimeTests.test_restart_first_resume_exit_fallback_recreates_session_and_opens_display",
    "tests.run_tests.RuntimeTests.test_start_agent_repairs_missing_worker_window_without_restart",
    "tests.run_tests.RuntimeTests.test_start_agent_falls_back_to_fresh_when_resume_window_exits",
    "tests.run_tests.RuntimeTests.test_broadcast_sends_only_to_current_team_and_excludes_sender",
    "tests.run_tests.RuntimeTests.test_status_and_collect_expose_uncollected_report_result",
    "tests.run_tests.RuntimeTests.test_report_result_queues_leader_notification_without_blocking_mcp",
    "tests.run_tests.RuntimeTests.test_mcp_send_message_accepts_thin_args_and_returns_compact_result",
    "tests.run_tests.RuntimeTests.test_mcp_send_message_accepts_broadcast_target",
    "tests.run_tests.RuntimeTests.test_mcp_send_message_without_env_infers_worker_before_leader_send",
    "tests.run_tests.RuntimeTests.test_mcp_report_result_without_env_infers_task_and_agent",
    "tests.run_tests.RuntimeTests.test_mcp_report_result_normalizes_common_loose_shapes",
    "tests.run_tests.RuntimeTests.test_compile_system_prompt_prepends_teammate_runtime_contract",
    "tests.run_tests.RuntimeTests.test_codex_default_command_avoids_dangerous_bypass",
    "tests.run_tests.RuntimeTests.test_launch_inherits_leader_dangerous_permissions_in_dry_run",
    "tests.run_tests.RuntimeTests.test_launch_passes_inherited_dangerous_permissions_to_worker_runtime",
    "tests.run_tests.RuntimeTests.test_restart_passes_inherited_dangerous_permissions_to_resume_and_fresh_workers",
    "tests.run_tests.RuntimeTests.test_quick_start_refuses_to_overwrite_existing_context_without_fresh",
    "tests.run_tests.RuntimeTests.test_quick_start_team_id_stores_loose_docs_outside_current",
    "tests.run_tests.RuntimeTests.test_start_writes_compiled_spec_inside_selected_team_dir",
    "tests.run_tests.RuntimeTests.test_preflight_uses_selected_team_profile_dir_not_current",
    "tests.run_tests.RuntimeTests.test_restart_requires_team_selector_when_multiple_snapshots_exist",
    "tests.run_tests.RuntimeTests.test_leader_start_plan_creates_tmux_session_outside_tmux_and_passes_args",
    "tests.run_tests.RuntimeTests.test_leader_start_plan_inside_tmux_execs_provider_in_current_pane",
    "tests.run_tests.RuntimeTests.test_launch_requires_current_tmux_leader_for_real_workers",
    "tests.run_tests.RuntimeTests.test_resolve_leader_scans_workspace_when_tool_shell_has_wrong_tmux_client",
    "tests.run_tests.RuntimeTests.test_resolve_leader_reports_ambiguous_workspace_panes",
    "tests.run_tests.CliContractTests.test_leader_commands_pass_provider_flags_without_argparse_consuming_them",
    "tests.run_tests.CliContractTests.test_npx_installer_installs_runtime_wrappers_and_skills",
    "tests.run_tests.CliContractTests.test_skill_blackbox_lint",
]


def main() -> int:
    parser = argparse.ArgumentParser(description="Run the fixed Team Agent regression asset.")
    parser.add_argument("--iterations", type=int, default=1, help="repeat the regression batch N times")
    parser.add_argument("--list", action="store_true", help="print the selected unittest ids and exit")
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be >= 1")
    if args.list:
        print("\n".join(REGRESSION_TESTS))
        return 0

    repo = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env["PYTHONPATH"] = str(repo / "src") + os.pathsep + env.get("PYTHONPATH", "")
    for index in range(1, args.iterations + 1):
        print(f"[team-agent-regression] iteration {index}/{args.iterations}", flush=True)
        proc = subprocess.run([sys.executable, "-m", "unittest", *REGRESSION_TESTS], cwd=repo, env=env, check=False)
        if proc.returncode != 0:
            return proc.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
