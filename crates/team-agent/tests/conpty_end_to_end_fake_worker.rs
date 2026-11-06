//! End-to-end integration test for the ConPTY Phase 1 flow using the
//! in-memory FakePaneRuntime + LocalShimPipeClient.
//!
//! This preserves the current Phase 1 acceptance criteria at the
//! transport-trait level:
//!
//!   1. quick-start-equivalent: build a ConPtyBackend with a live
//!      client, spawn a fake pane.
//!   2. status-equivalent: `list_targets()` reports the pane with its
//!      backend_kind + child fields.
//!   3. capture: `capture()` preserves native runtime output, including UTF-8.
//!   4. shutdown: `kill_session()` clears the session registry; a subsequent
//!      `list_targets()` reports zero panes.
//!   5. macOS/Linux tmux E2E unchanged: this test uses NO tmux; the
//!      developer host's tmux runtime is untouched.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use team_agent::conpty::{
    backend::{PipeClientAdapter, PipeClientTrait},
    protocol::SpawnRequest,
    shim::{FakePaneRuntime, LocalShimClient, PaneRuntime, Shim},
    ConPtyBackend,
};
use team_agent::transport::{ BackendKind, CaptureRange, Key, PaneId, SessionName, Target, Transport, WindowName };

fn make_backend(output: &[u8]) -> ConPtyBackend {
    let output = output.to_vec();
    let shim = Arc::new(Shim::new(
        "ws-e2e",
        "team-a",
        4242,
        "test-shim-0.1",
        "tok-e2e",
        Box::new(move |_spawn: &SpawnRequest| {
            let runtime = Arc::new(FakePaneRuntime::new());
            assert_eq!(runtime.write_input(&output)?, output.len());
            Ok(runtime as Arc<dyn PaneRuntime>)
        }),
    ));
    let client: Box<dyn PipeClientTrait> = Box::new(PipeClientAdapter(LocalShimClient::new(shim)));
    ConPtyBackend::new("ws-e2e", "team-a")
        .with_pipe_client(client)
        .expect("pipe client should connect for in-memory shim")
}

#[test]
fn phase1_bullet1_quick_start_equivalent_spawns_fake_worker() {
    let backend = make_backend(&[]);
    let spawn = backend
        .spawn_first(
            &SessionName::new("team-a"),
            &WindowName::new("w1"),
            &["fake".to_string()],
            Path::new("/tmp"),
            &BTreeMap::new(),
        )
        .expect("spawn must succeed with a live pipe client");
    // Design §Authority Model:155 pane_id shape.
    assert!(
        spawn
            .pane_id
            .as_str()
            .starts_with("conpty:ws-e2e:team-a:w1:"),
        "pane_id shape drift; got {}",
        spawn.pane_id.as_str()
    );
    assert_eq!(spawn.window.as_str(), "w1");
    assert_eq!(spawn.session.as_str(), "team-a");
}

#[test]
fn phase1_bullet2_status_reports_conpty_backend_and_pane_row() {
    let backend = make_backend(&[]);
    assert_eq!(backend.kind(), BackendKind::ConPty);
    let spawn = backend
        .spawn_first(
            &SessionName::new("team-a"),
            &WindowName::new("w1"),
            &["fake".to_string()],
            Path::new("/tmp"),
            &BTreeMap::new(),
        )
        .unwrap();
    let list = backend.list_targets().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].pane_id.as_str(), spawn.pane_id.as_str());
    assert_eq!(list[0].session.as_str(), "team-a");
    assert_eq!(list[0].window_name.as_ref().map(|w| w.as_str()), Some("w1"));
}



#[test]
fn phase1_bullet4_capture_contains_injected_token() {
    let token = "PHASE1_UNIQUE_TOKEN_20260705";
    let backend = make_backend(token.as_bytes());
    let spawn = backend
        .spawn_first(
            &SessionName::new("team-a"),
            &WindowName::new("w1"),
            &["fake".to_string()],
            Path::new("/tmp"),
            &BTreeMap::new(),
        )
        .unwrap();
    let captured = backend
        .capture(&Target::Pane(spawn.pane_id.clone()), CaptureRange::Full)
        .expect("capture must succeed");
    assert!(
        captured.text.contains(token),
        "capture text must include token; got {:?}",
        captured.text
    );
}

#[test]
fn phase1_bullet4_chinese_token_survives_utf8() {
    // Design §Encoding Strategy: Chinese round-trip.
    let chinese = "王小明·测试·令牌";
    let backend = make_backend(chinese.as_bytes());
    let spawn = backend
        .spawn_first(
            &SessionName::new("team-a"),
            &WindowName::new("w-cn"),
            &["fake".to_string()],
            Path::new("/tmp"),
            &BTreeMap::new(),
        )
        .unwrap();
    let captured = backend
        .capture(&Target::Pane(spawn.pane_id.clone()), CaptureRange::Full)
        .unwrap();
    assert!(captured.text.contains(chinese), "got: {:?}", captured.text);
}

#[test]
fn phase1_bullet5_shutdown_kills_all_panes_and_registry_clears() {
    let backend = make_backend(&[]);
    for w in ["w1", "w2", "w3"] {
        backend
            .spawn_first(
                &SessionName::new("team-a"),
                &WindowName::new(w),
                &["fake".to_string()],
                Path::new("/tmp"),
                &BTreeMap::new(),
            )
            .unwrap();
    }
    assert_eq!(backend.list_targets().unwrap().len(), 3);
    backend.kill_session(&SessionName::new("team-a")).expect("kill_session must succeed");
    assert_eq!(backend.list_targets().unwrap().len(), 0);
}

#[test]
fn typed_capability_refusals_do_not_change_with_live_client() {
    // Retain current ConPTY endpoint truth; attach/env adapters are retired.
    let backend = make_backend(&[]);
    assert_eq!(backend.tmux_endpoint(), None);
    assert!(!backend.probes_real_tmux_socket_roots());
}

#[test]
fn no_pipe_client_still_returns_honest_mux_unavailable() {
    // MUST-NOT-13 + CR C-3 anchor holds even after the shim landed:
    // a backend without a client must degrade honestly.
    let backend = ConPtyBackend::new("ws-x", "team-x");
    let err = backend.list_targets().unwrap_err();
    match err {
        team_agent::transport::TransportError::MuxUnavailable { backend, detail } => {
            assert_eq!(backend, BackendKind::ConPty);
            assert!(
                detail.contains("no_pipe_client"),
                "detail must anchor on `no_pipe_client`; got {detail}"
            );
        }
        other => panic!("expected MuxUnavailable, got {other:?}"),
    }
}
