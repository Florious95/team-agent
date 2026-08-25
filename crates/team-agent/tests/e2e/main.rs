//! `tests/e2e/` — Team Agent CLI black-box E2E test framework.
//!
//! This is the SINGLE integration test binary (declared in
//! `crates/team-agent/Cargo.toml` as `[[test]] name="e2e"`). Every E2E test
//! goes through the `team-agent` CLI binary (built by `cargo test`), parses
//! JSON output, and asserts on observable side effects (state.json,
//! events.jsonl, tmux sessions, etc.). NO test calls `team_agent::*` library
//! APIs directly — that's what the other 80+ `*_red.rs` integration tests do.
//!
//! Run a single test:
//!
//! ```text
//! cargo test --package team-agent --test e2e -- test_quick_start_and_shutdown
//! ```
//!
//! Run all E2E tests serially (the workspace is per-test, so parallel is fine
//! by default; pass `--test-threads=1` only when debugging):
//!
//! ```text
//! cargo test --package team-agent --test e2e
//! ```
//!
//! Keep workspaces on disk after a failed run for forensics:
//!
//! ```text
//! TEAM_AGENT_KEEP_TEST_TMP=1 cargo test --package team-agent --test e2e
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests assert with unwrap/expect on purpose

pub mod framework;
pub mod scripted_provider;
pub mod support;

mod cases {
    mod agent_001_start_agent_repair_missing_window;
    mod agent_002_stop_agent_single_worker;
    mod agent_003_reset_agent_discard_session;
    mod agent_004_add_agent_runtime;
    mod agent_005_remove_agent_runtime;
    mod agent_006_fork_agent;
    mod dirty_001_stale_pane_id;
    mod dirty_002_missing_tmux_socket;
    mod dirty_003_orphan_coordinator;
    mod dirty_004_stale_session_id_missing_backing;
    mod dirty_005_cross_team_binding_pollution;
    mod dirty_006_message_stuck_in_accepted;
    mod gate_hole_061_red;
    mod inbox_001_delivery_status_visible;
    mod lnch_001_quick_start_basic;
    mod lnch_002_duplicate_session;
    mod lnch_003_missing_profile;
    mod lnch_004_display_backend_none;
    mod rec_002_doctor_checks;
    mod rec_003_diagnose_output;
    mod rest_001_refuses_stale_session;
    mod rest_002_backing_store_missing;
    mod rest_011_resume_happy_path;
    mod rest_012_mixed_resume_fresh;
    mod rest_013_allow_fresh_flag;
    mod rest_014_multi_team_selector;
    mod rfs_restart_false_success_contract;
    mod send_001_fake_worker;
    mod send_002_send_to_stopped;
    mod send_003_send_json_shape;
    mod send_007_send_broadcast;
    mod send_008_watch_result_missing_worker_window;
    mod send_009_worker_repair_requeues_blocked_message;
    mod shut_001_clean_shutdown;
    mod shut_002_false_green_guard;
    mod shut_003_idempotent;
    mod shut_004_cleans_coordinator;
    mod stat_001_status_json_shape;
    mod stat_002_status_stopped_team;
    mod stat_003_status_dirty_state_reporting;
    mod wleak_worker_delivery_socket_leak_contract;
}

// Keep framework self-tests in the canonical E2E target; the bypass target
// imports framework helpers only and must not compile these tests again.
#[cfg(test)]
mod framework_tests {
    use super::framework::*;
    use std::time::Duration;

    #[test]
    fn owned_coordinator_predicate_requires_debug_binary_and_e2e_workspace() {
        let ws = TestWorkspace::new("cleanup-predicate");
        let bin = ta_binary();
        ws.record_ta_binary(&bin);
        let workspace = ws.path().to_string_lossy();

        let owned = format!("{} coordinator --workspace {workspace}", bin.display());
        assert!(
            ws.command_is_owned_coordinator(&owned),
            "debug binary plus exact e2e temp workspace should be owned"
        );

        let platform_tmp_ws = TestWorkspace {
            path: normalize_existing_path(&std::env::temp_dir())
                .join(format!("ta-e2e-platform-{}-0", std::process::id())),
            ta_binary: std::sync::Mutex::new(Some(normalize_existing_path(&bin))),
            owned_tmux_sockets: std::sync::Mutex::new(Vec::new()),
        };
        let platform_tmp_owned = format!(
            "{} coordinator --workspace {}",
            bin.display(),
            platform_tmp_ws.path().display()
        );
        assert!(
            platform_tmp_ws.command_is_owned_coordinator(&platform_tmp_owned),
            "debug binary plus exact platform temp e2e workspace should be owned"
        );

        let local =
            format!("/Users/alauda/.local/bin/team-agent coordinator --workspace {workspace}");
        assert!(
            !ws.command_is_owned_coordinator(&local),
            "installed local binary must never be owned by the E2E cleanup"
        );

        let runtime = format!(
            "/Users/alauda/.team-agent/runtime/0.4.8/bin/team-agent coordinator --workspace {workspace}"
        );
        assert!(
            !ws.command_is_owned_coordinator(&runtime),
            "runtime-installed binary must never be owned by the E2E cleanup"
        );

        let real_workspace = format!(
            "{} coordinator --workspace /Users/alauda/Documents/code/team-agent-public",
            bin.display()
        );
        assert!(
            !ws.command_is_owned_coordinator(&real_workspace),
            "debug binary alone is not enough without the exact private e2e workspace"
        );
    }

    #[test]
    fn drop_stops_owned_coordinator_after_worker_is_stopped() {
        let team_id = format!("cleanup{}", std::process::id());
        let (workspace, coordinator_pid) = {
            let ws = TestWorkspace::new("cleanup-drop").with_fake_spec(&["a"]);
            let qs = quick_start_fake(&ws, &team_id);
            assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

            let stop = run_ta(
                &ws,
                &[
                    "stop-agent",
                    "a",
                    "--workspace",
                    ws.path().to_str().unwrap(),
                    "--json",
                ],
            );
            assert!(
                stop.is_success(),
                "stop-agent exit {}; stdout={} stderr={}",
                stop.exit_code,
                stop.stdout,
                stop.stderr
            );

            wait_for_or_panic(
                "coordinator pid file",
                || read_pid(&ws.coordinator_pid_file()).is_some(),
                Duration::from_secs(3),
            );
            let pid = read_pid(&ws.coordinator_pid_file()).expect("coordinator pid");
            assert!(
                ws.pid_is_owned_coordinator(pid),
                "pid {pid} should be the workspace-owned debug coordinator"
            );
            assert!(
                pid_is_running(pid),
                "coordinator pid {pid} should be live before Drop"
            );
            (ws.path().to_path_buf(), pid)
        };

        assert!(
            wait_until_pid_exits(coordinator_pid, Duration::from_secs(3)),
            "coordinator pid {coordinator_pid} should be stopped by TestWorkspace::Drop"
        );
        assert!(
            !workspace.exists(),
            "workspace {} should be removed by TestWorkspace::Drop",
            workspace.display()
        );
    }
}
