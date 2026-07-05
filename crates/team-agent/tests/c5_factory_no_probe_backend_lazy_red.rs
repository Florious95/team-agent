//! CR C-5 violation fix RED (leader msg_a89b5a03bbf0):
//!
//! CR C-5 明文禁 factory probe。设计原文:connect 属 backend。
//! Batch 5 起 factory 里的 `NamedPipeClient::connect(pipe_name, 2000ms)`
//! 是真实违规 —— 每次 `status/diagnose` 在 Windows 上都可能挂 2 秒。
//!
//! 契约:
//!
//! 1. `resolve_transport` on Windows with a `pipe_ready=true` state hint
//!    but no live shim on the pipe must complete FAST (<100ms).
//!    Currently: hits `NamedPipeClient::connect(_, 2000)` inside
//!    `build_conpty` → ≥2000ms wall-clock. Post-fix: pure assembly,
//!    the connect is deferred to first Op on the backend.
//!
//! 2. `ConPtyBackend` must connect lazily on first Op that needs the
//!    pipe (Spawn/Inject/Capture/Liveness/HasPane/ListTargets/HasSession/
//!    KillSession/Attach/SendKeys/QueryPane). Callers that don't need
//!    the pipe never pay the connect cost.
//!
//! 3. Backend gains a `with_pipe_hint(pipe_name)` constructor that
//!    stashes the pipe name for lazy-connect use. The factory calls
//!    this when it sees `pipe_ready=true` in state.
//!
//! 4. Batch 6 Hello direct-handoff path (used by `lifecycle/launch.rs`
//!    quick-start conpty spawn) stays byte-compatible: the already-
//!    connected client becomes the backend's initial cache entry via
//!    `with_pipe_client`. No new API needed for direct-handoff.
//!
//! These are structural / behavioral tests (both platforms compile).

use std::path::PathBuf;
use std::time::Instant;

fn read_src(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn factory_build_conpty_does_not_call_named_pipe_client_connect() {
    // The primary CR C-5 lock: `build_conpty` in `transport_factory.rs`
    // must NOT call `NamedPipeClient::connect`. `connect` is a
    // backend concern, not a factory concern.
    let factory = read_src("transport_factory.rs");
    let code = strip_comments(&factory);
    assert!(
        !code.contains("NamedPipeClient::connect"),
        "CR C-5 violation: `transport_factory` must not call \
         `NamedPipeClient::connect`. connect belongs to the backend's \
         lazy-connect path (leader msg_a89b5a03bbf0)."
    );
}

#[test]
fn backend_exposes_pipe_hint_constructor() {
    // Backend must expose a `with_pipe_hint(pipe_name)` (or equivalent)
    // so the factory can stash the pipe name for lazy-connect without
    // performing I/O.
    let backend = read_src("conpty/backend.rs");
    let code = strip_comments(&backend);
    assert!(
        code.contains("pub fn with_pipe_hint"),
        "ConPtyBackend must expose `with_pipe_hint(pipe_name)` so \
         the factory can defer connect to first Op (CR C-5)."
    );
}

#[test]
fn backend_lazy_connect_helper_present() {
    // Backend must have a helper that performs the deferred connect
    // + Hello on demand. Name doesn't matter but the intent must be
    // clear.
    let backend = read_src("conpty/backend.rs");
    let code = strip_comments(&backend);
    assert!(
        code.contains("ensure_pipe_client") || code.contains("connect_pipe_client_if_needed"),
        "ConPtyBackend must have an `ensure_pipe_client` / \
         `connect_pipe_client_if_needed` helper that lazily connects \
         on first Op (CR C-5)."
    );
}

#[test]
fn resolve_transport_windows_conpty_completes_fast_when_no_shim() {
    // Behavioral test: `resolve_transport` on a workspace whose
    // state hints at `pipe_ready=true` but there's actually no shim
    // listening must complete FAST (<100ms). Pre-fix: ~2000ms
    // because of `NamedPipeClient::connect(_, 2000)` inside
    // `build_conpty`. Post-fix: pure assembly, connect deferred.
    //
    // Runs on both platforms:
    // - On Unix, the cfg(windows) block in `build_conpty` is
    //   compiled out, so this test just proves the fast path on
    //   Unix (a sanity floor).
    // - On Windows, this is the actual regression lock.
    use team_agent::transport_factory::{
        resolve_transport, RequestedTransportBackend, TransportFactoryInput, TransportPurpose,
    };
    let workspace = std::env::temp_dir().join(format!(
        "ta_c5_no_probe_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&workspace).unwrap();
    let state = serde_json::json!({
        "active_team_key": "team-a",
        "transport": {
            "kind": "conpty",
            "shim": {
                "pid": 999999,
                "pipe_name": r"\\.\pipe\team-agent-conpty-nonexistent-team-a",
                "pipe_ready": true
            }
        }
    });
    let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
        .with_team_key(Some("team-a"))
        .with_explicit_backend(Some(RequestedTransportBackend::ConPty))
        .with_state(Some(&state));
    let start = Instant::now();
    let _resolved = resolve_transport(input).expect("resolve must succeed");
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 100,
        "CR C-5: resolve_transport must be pure assembly (no I/O). \
         Got {}ms (limit 100ms). See leader msg_a89b5a03bbf0.",
        elapsed.as_millis()
    );
    // Cleanup
    let _ = std::fs::remove_dir_all(&workspace);
}
