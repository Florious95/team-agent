//! Phase 1d Batch 2 contract tests for `quick-start --backend`.
//!
//! Verifies:
//!
//! 1. `--backend tmux` parses and carries into `QuickStartArgs.backend`.
//! 2. `--backend conpty` parses and carries.
//! 3. `--backend pty` refuses with usage error (CR C-1 ②).
//! 4. `--backend nonsense` refuses.
//! 5. quick-start without `--backend` leaves `backend` as `None`
//!    (byte-equivalent tmux path when handed to the lifecycle port).
//! 6. Help copy for `quick-start` advertises `[--backend tmux|conpty]`.
//! 7. `annotate_runtime_transport` writes `state.transport = { kind,
//!    source }` for a tmux transport AND preserves the tmux endpoint
//!    fields.
//! 8. `annotate_runtime_transport` writes `state.transport.kind =
//!    "conpty"` for a conpty transport WITHOUT tmux endpoint fields.
//! 9. Grep guard: help copy MUST NOT advertise `pty` (design §Non-Goals).
//!
//! (Design §Batch 2, CR C-1 ②, CR C-4 pins compact status stable.)
//!
//! ## Wiring deferral note (state.transport annotation)
//!
//! The `annotate_runtime_transport` **function** lands in this batch
//! and is unit-tested here in isolation (tmux + conpty branches). Its
//! **call site** at the launch-hot-path (`launch.rs` where the initial
//! runtime state is persisted) is NOT wired yet in this commit,
//! because inserting a `"transport"` key at the top of `state.json`
//! extends the state-shape pin
//! (`quick_start_state_seeds_spec_path_workspace_leader_display_backend`)
//! and may drift phase_golden fixtures. Per leader msg_6dfbb3c78d38,
//! shape changes touching fixtures must be reported before landing.
//! The call site stays on `annotate_runtime_tmux_endpoint` until
//! leader accepts the schema extension in a follow-up.

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

use team_agent::cli::emit;

fn parse_quick_start(argv: &[&str]) -> Result<team_agent::cli::types::QuickStartArgs, String> {
    let argv_owned: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let cwd = std::env::temp_dir().join("ta-batch2-qs");
    emit::__test_quick_start_args(&argv_owned, &cwd).map_err(|e| e.to_string())
}

fn command_help(command: Option<&str>) -> String {
    emit::__test_command_help(command)
}

#[test]
fn backend_tmux_literal_parses() {
    let args = parse_quick_start(&["--backend", "tmux", "--yes"]).expect("must parse");
    assert_eq!(args.backend.as_deref(), Some("tmux"));
}

#[test]
fn backend_conpty_literal_parses() {
    let args = parse_quick_start(&["--backend", "conpty", "--yes"]).expect("must parse");
    assert_eq!(args.backend.as_deref(), Some("conpty"));
}

#[test]
fn backend_pty_literal_refused_no_silent_map_to_conpty() {
    // CR C-1 ②: `pty` must never silently become ConPTY, either at the
    // parser or the factory.
    let err = parse_quick_start(&["--backend", "pty", "--yes"]).expect_err("must refuse");
    assert!(
        err.contains("pty") && err.contains("`tmux`"),
        "usage error must name the bad literal and list the allowed set; got {err}"
    );
}

#[test]
fn backend_nonsense_literal_refused() {
    let err = parse_quick_start(&["--backend", "nonsense", "--yes"]).expect_err("must refuse");
    assert!(err.contains("nonsense"), "err must echo bad literal: {err}");
}

#[test]
fn no_backend_flag_leaves_backend_none_for_byte_equivalent_tmux_path() {
    // Design §Batch 2 Verification anchor:
    // `quick-start` without `--backend` must be byte-equivalent to
    // the existing tmux path. The first check is: `backend` is `None`
    // so the lifecycle port picks the legacy tmux entrypoint.
    let args = parse_quick_start(&["--yes"]).expect("must parse");
    assert!(
        args.backend.is_none(),
        "no --backend must leave backend=None"
    );
}

#[test]
fn quick_start_help_advertises_backend_flag() {
    let help = command_help(Some("quick-start"));
    assert!(
        help.contains("--backend tmux|conpty") || help.contains("--backend <tmux|conpty>"),
        "quick-start help must advertise --backend flag; got:\n{help}"
    );
    // C-1 ② + design §Non-Goals: help MUST NOT advertise `pty`.
    assert!(
        !help.contains("--backend pty") && !help.contains("|pty"),
        "quick-start help must NOT advertise `pty` literal — Phase 1d does not \
         auto-map pty to conpty (CR C-1 ②). Got:\n{help}"
    );
    // Fresh field is still absent (Stage QR anchor).
    assert!(
        !help.contains("--fresh"),
        "quick-start help must not advertise removed --fresh flag"
    );
}

#[test]
fn annotate_runtime_transport_writes_kind_and_preserves_tmux_endpoint_fields() {
    // Behavior contract: with a tmux transport, state gets both the
    // new `state.transport.kind` field AND the existing
    // `tmux_endpoint`/`tmux_socket` fields (byte-preserving semantics
    // — CR C-4).
    let workspace = std::env::temp_dir().join("ta-batch2-annotate-tmux");
    std::fs::create_dir_all(&workspace).unwrap();
    let tmux = team_agent::tmux_backend::TmuxBackend::for_workspace(&workspace);
    let mut state = serde_json::json!({});
    team_agent::lifecycle::annotate_runtime_transport(&mut state, &tmux, &workspace, Some("cli"));
    assert_eq!(
        state.pointer("/transport/kind"),
        Some(&serde_json::json!("tmux")),
        "state.transport.kind must be `tmux` for a tmux backend"
    );
    assert_eq!(
        state.pointer("/transport/source"),
        Some(&serde_json::json!("cli")),
        "state.transport.source must carry the caller-supplied source"
    );
    // Tmux endpoint fields still populated (Byte-equivalent preservation).
    assert!(
        state.get("tmux_endpoint").is_some(),
        "tmux_endpoint must still be populated for a tmux backend"
    );
}

#[test]
fn annotate_runtime_transport_conpty_writes_kind_without_tmux_endpoint_fields() {
    // ConPTY backend: state gets `state.transport.kind = "conpty"`
    // and does NOT get tmux_endpoint / tmux_socket fields.
    let workspace = std::env::temp_dir().join("ta-batch2-annotate-conpty");
    std::fs::create_dir_all(&workspace).unwrap();
    let conpty = team_agent::conpty::ConPtyBackend::new("wshash-b2", "team-b2");
    let mut state = serde_json::json!({});
    team_agent::lifecycle::annotate_runtime_transport(&mut state, &conpty, &workspace, Some("cli"));
    assert_eq!(
        state.pointer("/transport/kind"),
        Some(&serde_json::json!("conpty"))
    );
    assert_eq!(
        state.pointer("/transport/source"),
        Some(&serde_json::json!("cli"))
    );
    // No tmux endpoint fields.
    assert!(
        state.get("tmux_endpoint").is_none(),
        "tmux_endpoint must NOT appear for a conpty backend"
    );
    assert!(
        state.get("tmux_socket").is_none(),
        "tmux_socket must NOT appear for a conpty backend"
    );
}

#[test]
fn batch2_migration_anchors_present_in_source() {
    // C-6 sibling: prevent silent drift. If quick_start_tmux_backend
    // is deleted/renamed, or annotate_runtime_transport is inlined
    // away, this test fires so Batch 3 cannot proceed on a
    // half-checked abstraction.
    let launch = composite_source::composite_source("src/lifecycle/launch.rs");
    assert!(
        launch.contains("fn quick_start_in_workspace_with_display_and_backend"),
        "Batch 2 new lifecycle entrypoint symbol missing"
    );
    assert!(
        launch.contains("fn annotate_runtime_transport"),
        "Batch 2 generic annotator symbol missing"
    );
}
