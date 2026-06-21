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

mod cases {
    mod shut_001_clean_shutdown;
    mod shut_002_false_green_guard;
    mod rest_001_refuses_stale_session;
    mod rest_002_backing_store_missing;
    mod lnch_001_quick_start_basic;
    mod send_001_fake_worker;
}
