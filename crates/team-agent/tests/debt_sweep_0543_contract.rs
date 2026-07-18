//! 0.5.43 debt-sweep A-car RED contracts.
//!
//! User-visible invariant: test fixtures must remain deterministic under long
//! macOS temp paths and must clean only the exact processes/tmux servers they
//! created. Small debt deletions must preserve the existing product behavior.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hermetic_guard::{short_tmux_socket, HermeticTestEnv};

static CASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[cfg(target_os = "macos")]
fn short_fixture_base_uses_private_tmp_on_macos() {
    assert_eq!(short_test_base(), PathBuf::from("/private/tmp"));
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn short_fixture_base_uses_tmp_on_other_unix_hosts() {
    assert_eq!(short_test_base(), PathBuf::from("/tmp"));
}

#[test]
#[serial_test::serial(env)]
fn long_tmpdir_still_yields_unique_short_real_tmux_sockets() {
    let long_tmp = short_test_base().join(format!(
        "ta-0543-long-{}-{}",
        std::process::id(),
        "x".repeat(190)
    ));
    fs::create_dir_all(&long_tmp).expect("create >180-byte TMPDIR fixture");
    let _long_tmp_cleanup = PathCleanup(long_tmp.clone());
    let _tmpdir = EnvGuard::set("TMPDIR", long_tmp.as_os_str());
    let _test_tmp = EnvGuard::unset("TEAM_AGENT_TEST_TMP");

    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| short_tmux_socket("parallel"));
        let right = scope.spawn(|| short_tmux_socket("parallel"));
        (
            left.join().expect("left short socket fixture"),
            right.join().expect("right short socket fixture"),
        )
    });
    assert_ne!(left, right, "parallel fixtures need distinct exact sockets");
    for socket in [&left, &right] {
        assert_short_non_default_socket(socket);
    }

    let mut server = TmuxServer::start(left, unique("short-socket"));
    assert!(
        server.is_alive(),
        "tmux -S list-sessions must reach the owned server"
    );
    server.kill();
    assert!(
        !server.is_alive(),
        "tmux -S kill-server must stop the exact server"
    );
}

#[test]
fn real_tmux_gate_fixtures_use_the_short_socket_helper() {
    for relative in [
        "crates/team-agent/tests/restart_build_before_destroy_0540_contract.rs",
        "crates/team-agent/tests/acceptance_b_batch_red.rs",
    ] {
        let source = read_repo(relative);
        assert!(
            source.contains("short_tmux_socket"),
            "RED: real tmux -S fixture must use the shared short socket helper: {relative}"
        );
    }
}

#[test]
#[serial_test::serial(env)]
fn hermetic_drop_cleans_exact_owned_resources_and_preserves_foreign_server() {
    let mut fixture = CleanupFixture::new("normal");
    {
        let env = HermeticTestEnv::enter("0543-owned-normal");
        env.register_owned_pid(fixture.child.pid());
        env.register_owned_tmux_socket(fixture.owned.socket());
    }
    fixture.assert_owned_gone_and_foreign_alive();
}

#[test]
#[serial_test::serial(env)]
fn hermetic_drop_during_unwind_cleans_exact_owned_resources_only() {
    let mut fixture = CleanupFixture::new("unwind");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let env = HermeticTestEnv::enter("0543-owned-unwind");
        env.register_owned_pid(fixture.child.pid());
        env.register_owned_tmux_socket(fixture.owned.socket());
        panic!("intentional fixture failure after resource creation");
    }));
    assert!(result.is_err(), "fixture must exercise unwind Drop");
    fixture.assert_owned_gone_and_foreign_alive();
}

#[test]
fn e2e_workspace_drop_cleans_exact_tmux_before_removing_workspace() {
    let source = read_repo("crates/team-agent/tests/e2e/framework.rs");
    let drop_body = function_block_after(&source, "fn drop(&mut self)");
    let cleanup = drop_body
        .find("cleanup_owned_tmux")
        .expect("RED: TestWorkspace::Drop must call exact owned tmux cleanup");
    let remove = drop_body
        .find("remove_workspace_dir")
        .expect("TestWorkspace::Drop keeps workspace removal");
    assert!(
        cleanup < remove,
        "exact tmux cleanup must precede workspace removal"
    );
    assert!(!source.contains("pgrep target/debug/team-agent"));
    assert!(!source.contains("/private/tmp/tmux-*"));
}

#[test]
fn e6_fixture_uses_unique_team_identity_and_exact_socket_fallback() {
    let source = read_repo("crates/team-agent/tests/e6_offline_mailbox_toname_e2e_contract.rs");
    let constructor = function_block_after(&source, "fn new(tag: &str) -> Self");
    let normalized = normalize(&constructor);
    assert!(
        normalized.contains("teamkey:format")
            && normalized.contains("stdprocessid")
            && normalized.contains("n"),
        "RED: E6 team_key/session must include pid+counter; constructor={constructor}"
    );
    assert!(
        !constructor.contains("team_key: \"mail059\".to_string()"),
        "fixed E6 team key can collide with host fixtures"
    );

    let drop_body = function_block_after(&source, "impl Drop for E6Case");
    assert!(
        (drop_body.contains("kill-server") && drop_body.contains("\"-S\""))
            || drop_body.contains("cleanup_exact_tmux_socket"),
        "RED: shutdown failure needs exact -S socket fallback; drop={drop_body}"
    );
}

#[test]
fn packaging_dry_run_enters_the_same_serial_home_boundary_as_real_copy() {
    let source = read_repo("crates/team-agent/src/packaging/tests.rs");
    let region = attributed_test_region(
        &source,
        "fn install_skill_dry_run_is_pure_no_provider_state",
    );
    for required in [
        "#[serial_test::serial(env)]",
        "ENV_LOCK_PKG",
        "HomeGuard::set",
    ] {
        assert!(
            region.contains(required),
            "RED: packaging dry-run must isolate HOME inside serial(env); missing {required}; region={region}"
        );
    }
}

#[test]
fn file_lock_timeout_fixture_has_unique_path_and_preserves_timeout_shape() {
    let source = read_repo("crates/team-agent/src/platform/file_lock.rs");
    let body = function_block_after(&source, "fn timeout_error_carries_path_and_seconds");
    assert!(!body.contains("join(\"ta-b2-timeout\")"));
    assert!(body.contains("std::process::id()"));
    assert!(body.contains("fetch_add"));
    for guard in [
        "LockError::Timeout",
        "timeout_secs > 0.0",
        "timeout-lock.tmp",
    ] {
        assert!(
            body.contains(guard),
            "timeout behavior guard disappeared: {guard}"
        );
    }
}

#[test]
fn line_count_missing_allowlist_is_valid() {
    let case = LineCountCase::new(1);
    let output = case.run(None, false, false);
    assert_success(&output, "missing allowlist");
}

#[test]
fn line_count_empty_allowlist_is_valid() {
    let case = LineCountCase::new(1);
    let allowlist = case.write_allowlist("{}");
    let output = case.run(Some(&allowlist), false, false);
    assert_success(&output, "empty allowlist");
}

#[test]
fn line_count_unknown_key_fails_closed() {
    let case = LineCountCase::new(1);
    let allowlist =
        case.write_allowlist(r#"{"approved_exceptions":{},"temporary_debt":{},"unexpected":{}}"#);
    let output = case.run(Some(&allowlist), false, false);
    assert_failure_contains(&output, "unknown", "unknown allowlist key");
}

#[test]
fn line_count_temporary_debt_blocks_completion() {
    let case = LineCountCase::new(1);
    let allowlist = case.write_allowlist(
        r#"{"approved_exceptions":{},"temporary_debt":{"fixture.rs":{"reason":"debt"}}}"#,
    );
    let output = case.run(Some(&allowlist), true, false);
    assert_failure_contains(&output, "temporary_debt", "temporary debt");
}

#[test]
fn line_count_soft_mode_reports_over_limit_without_hard_failure() {
    let case = LineCountCase::new(4);
    let output = case.run(None, false, false);
    assert_success(&output, "soft over-limit");
    assert!(text(&output.stdout).contains("over-limit: 1"));
}

#[test]
fn line_count_hard_mode_fails_over_limit() {
    let case = LineCountCase::new(4);
    let output = case.run(None, false, true);
    assert_failure_contains(&output, "over-limit: 1", "hard over-limit");
}

#[test]
fn coordinator_debug_eprintlns_are_deleted_but_inject_report_shape_remains() {
    let source = read_repo("crates/team-agent/src/tmux_backend.rs");
    for debug in [
        "team-agent submit consumption: consumed=false",
        "team-agent submit consumption fallback: consumed=false",
        "busy_capture_error=",
    ] {
        assert!(
            !source.contains(debug),
            "RED: unconditional coordinator debug remains: {debug}"
        );
    }
    let body = function_block_after(&source, "fn inject(");
    for field in [
        "InjectReport",
        "stage_reached",
        "inject_verification",
        "submit_verification",
        "turn_verification",
        "attempts",
        "submit_diagnostics",
    ] {
        assert!(
            body.contains(field),
            "InjectReport shape drifted while deleting debug: {field}"
        );
    }
}

#[test]
fn send_pane_compat_surface_has_no_direct_inject_path() {
    let source = read_repo("crates/team-agent/src/cli/send.rs");
    assert!(!source.contains("fn send_to_pane_direct"));
    assert!(!source.contains("send.pane_direct"));
    assert!(source.contains("--pane {pane_id} is deprecated and sunset"));
    assert!(source.contains("use a logical TARGET"));
}

#[test]
fn d_j_terminal_scrub_is_pure_and_attempt_path_loads_once() {
    let source = read_repo("crates/team-agent/src/coordinator/steps/abnormal.rs");
    let attempt = function_block_after(&source, "pub(crate) fn attempt_due_recoveries");
    assert_eq!(
        attempt.matches("load_runtime_state").count(),
        1,
        "D-j: attempt_due_recoveries must load one fresh post-save state"
    );
    assert!(attempt.contains("let Ok(mut state)"));
    assert!(attempt.contains("clear_stale_terminal_next_retry_at(&mut state)"));

    let scrub = function_block_after(&source, "fn clear_stale_terminal_next_retry_at");
    let signature = &source[source
        .find("fn clear_stale_terminal_next_retry_at")
        .unwrap()
        ..source
            .find("fn clear_stale_terminal_next_retry_at")
            .unwrap()
            + 120];
    assert!(signature.contains("&mut Value"));
    assert!(signature.contains("-> bool"));
    assert!(!scrub.contains("load_runtime_state"));
    assert!(!scrub.contains("save_runtime_state"));
    for status in ["succeeded", "blocked", "exhausted"] {
        assert!(scrub.contains(status));
    }
    assert!(scrub.contains("next_retry_at"));
}

include!("support/debt_sweep_0543.rs");
