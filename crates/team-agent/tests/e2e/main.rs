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

mod cases {
    mod shut_001_clean_shutdown;
    mod shut_002_false_green_guard;
    mod shut_003_idempotent;
    mod shut_004_cleans_coordinator;
    mod rest_001_refuses_stale_session;
    mod rest_002_backing_store_missing;
    mod rest_011_resume_happy_path;
    mod rest_012_mixed_resume_fresh;
    mod rest_013_allow_fresh_flag;
    mod rest_014_multi_team_selector;
    mod lnch_001_quick_start_basic;
    mod lnch_002_duplicate_session;
    mod lnch_003_missing_profile;
    mod lnch_004_display_backend_none;
    mod send_001_fake_worker;
    mod send_002_send_to_stopped;
    mod send_003_send_json_shape;
    mod send_007_send_broadcast;
    mod send_008_watch_result_missing_worker_window;
    mod send_009_worker_repair_requeues_blocked_message;
    mod inbox_001_delivery_status_visible;
    mod agent_001_start_agent_repair_missing_window;
    mod agent_002_stop_agent_single_worker;
    mod agent_003_reset_agent_discard_session;
    mod agent_004_add_agent_runtime;
    mod agent_005_remove_agent_runtime;
    mod agent_006_fork_agent;
    mod stat_001_status_json_shape;
    mod stat_002_status_stopped_team;
    mod stat_003_status_dirty_state_reporting;
    mod dirty_001_stale_pane_id;
    mod dirty_002_missing_tmux_socket;
    mod dirty_003_orphan_coordinator;
    mod dirty_004_stale_session_id_missing_backing;
    mod dirty_005_cross_team_binding_pollution;
    mod dirty_006_message_stuck_in_accepted;
    mod rec_001_repair_state_basic;
    mod rec_002_doctor_checks;
    mod rec_003_diagnose_output;
}
