//! Phase 1d Batch 5+6 contract tests.
//!
//! ## Batch 5 — shutdown/kill 语义
//! Verifies:
//! - `shutdown_workspace_transport` is now routed through
//!   `resolve_read_only_transport(TransportPurpose::Shutdown)`.
//! - `shutdown_transport_for_endpoint` stays a tmux channel helper.
//! - `KillDecision` (:306-370) and owned-socket cleanup (:535-566)
//!   remain unchanged (no `TransportPurpose::Shutdown` inside them).
//! - `session_residuals_after_reap` short-circuits the workspace-tmux
//!   + default-tmux probes when the primary transport is
//!   `BackendKind::ConPty`.
//!
//! ## Batch 6 — 标注性收尾
//! Verifies:
//! - `leader/start.rs`, `leader/lease.rs`, `leader/owner_bind.rs`,
//!   `diagnose/orphans.rs` reference at least one factory tmux channel
//!   helper (`transport_factory::tmux_*_transport`) so
//!   `grep transport_factory::tmux_` enumerates every tmux-only site.
//! - Behavior is preserved (helpers are thin wrappers over
//!   `TmuxBackend::{new, for_workspace, for_tmux_endpoint,
//!   for_socket_name}`).
//!
//! (Design §Batch 5 + §Batch 6.)

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

fn src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(rel: &str) -> String {
    composite_source::composite_source(&format!("src/{rel}"))
}

fn non_comment(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// ── Batch 5 ───────────────────────────────────────────────────────────

#[test]
fn batch5_shutdown_workspace_branch_routes_through_factory() {
    let body = read("cli/mod.rs");
    let code = non_comment(&body);
    // The top-level `pub fn shutdown` must call
    // `resolve_read_only_transport` with `TransportPurpose::Shutdown`
    // for the workspace branch (the endpoint literal branch keeps
    // `shutdown_transport_for_endpoint`).
    assert!(
        code.contains("TransportPurpose::Shutdown"),
        "Batch 5 requires the workspace shutdown branch to route through \
         `resolve_read_only_transport(TransportPurpose::Shutdown)`; marker \
         missing from cli/mod.rs."
    );
}

#[test]
fn batch5_endpoint_helper_stays_tmux_channel_helper() {
    // `shutdown_transport_for_endpoint` is intentionally tmux-typed —
    // it targets a caller-supplied tmux endpoint literal from state.
    // The fn signature should still return `TmuxBackend` (i.e. NOT
    // `Box<dyn Transport>`).
    let body = read("cli/mod.rs");
    let idx = body
        .find("fn shutdown_transport_for_endpoint")
        .expect("shutdown_transport_for_endpoint must exist as a tmux channel helper");
    let slice = &body[idx..];
    let sig_end = slice
        .find('{')
        .expect("shutdown_transport_for_endpoint body must open");
    let sig = &slice[..sig_end];
    assert!(
        sig.contains("TmuxBackend"),
        "Batch 5: `shutdown_transport_for_endpoint` must remain \
         tmux-typed (returns `TmuxBackend`) — endpoint literal branch \
         stays a tmux channel helper. Got signature: {sig}"
    );
}

#[test]
fn batch5_kill_decision_and_owned_socket_cleanup_untouched() {
    // The design forbids putting kill policy in the factory (CR C-2).
    // Ensure `KillDecision` is still an enum defined in cli/mod.rs and
    // that `cleanup_owned_empty_endpoint` still calls
    // `transport.kill_server()` (not a factory helper).
    let body = read("cli/mod.rs");
    assert!(
        body.contains("enum KillDecision") || body.contains("KillDecision {"),
        "KillDecision enum must remain in cli/mod.rs (CR C-2 hard守)"
    );
    let code = non_comment(&body);
    assert!(
        code.contains("kill_server()")
            || code.contains(".kill_server(")
            || code.contains("kill_server()"),
        "cli/mod.rs owned-socket cleanup must still call `.kill_server()` — \
         Batch 5 does not move kill policy into the factory (CR C-2)."
    );
}

#[test]
fn batch5_residual_check_short_circuits_tmux_probes_for_conpty_transport() {
    // Design §Batch 5 point 4: conpty 不拿 tmux socket 当 conpty 残留证据.
    let body = read("cli/mod.rs");
    let code = non_comment(&body);
    // Anchor markers: BackendKind::ConPty check inside
    // session_residuals_after_reap, and a guarded branch (`if
    // !primary_is_conpty`) around the workspace+default tmux probes.
    let idx = code
        .find("fn session_residuals_after_reap")
        .expect("session_residuals_after_reap must exist");
    let slice = &code[idx..];
    let end = slice
        .find("\n    fn session_residuals_after_reap_many")
        .unwrap_or_else(|| slice.len().min(4000));
    let fn_body = &slice[..end];
    assert!(
        fn_body.contains("BackendKind::ConPty"),
        "Batch 5: `session_residuals_after_reap` must check for \
         `BackendKind::ConPty` to skip tmux socket probes on a conpty \
         team (design §Batch 5 point 4)."
    );
    assert!(
        fn_body.contains("primary_is_conpty") || fn_body.contains("!primary_is_conpty"),
        "Batch 5: fn body must guard the tmux workspace/default probes on a \
         `primary_is_conpty` flag."
    );
}

/// ── Batch 6 ───────────────────────────────────────────────────────────

#[test]
fn batch6_leader_and_orphans_reference_factory_tmux_helpers() {
    // Design §Batch 6: optionally route these through explicit
    // `transport_factory::tmux_*` helpers for grep clarity.
    // Grep guard: each of the four files references at least one
    // factory helper.
    let files = [
        "leader/start.rs",
        "leader/lease.rs",
        "leader/owner_bind.rs",
        "diagnose/orphans.rs",
    ];
    for rel in files {
        let body = read(rel);
        let code = non_comment(&body);
        assert!(
            code.contains("transport_factory::tmux_"),
            "Batch 6: `{rel}` must reference at least one factory tmux \
             channel helper (`transport_factory::tmux_workspace_transport` / \
             `tmux_endpoint_transport` / `tmux_default_transport` / \
             `tmux_socket_name_transport`). Grep clarity anchor missing."
        );
    }
}

#[test]
fn batch6_factory_gains_tmux_socket_name_helper() {
    // Batch 6 adds `tmux_socket_name_transport` so orphans.rs can
    // migrate off `TmuxBackend::for_socket_name` directly.
    let body = read("transport_factory.rs");
    assert!(
        body.contains("pub fn tmux_socket_name_transport"),
        "Batch 6 must add `tmux_socket_name_transport` to \
         transport_factory.rs so tmux orphan scanning can grep-visible."
    );
}

#[test]
fn batch6_c4_compact_status_guard_still_green() {
    // Belt-and-braces: Batch 5+6 must not have leaked backend fields
    // into `compact_status`.
    let body = read("cli/status_port.rs");
    let sig = body
        .find("fn compact_status")
        .expect("compact_status must exist");
    let after = &body[sig..];
    let brace = after.find('{').expect("compact_status body must open");
    let start = sig + brace;
    let bytes = body.as_bytes();
    let mut depth: i32 = 0;
    let mut end = start;
    for (i, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let fn_body = &body[start..end];
    for forbidden in [
        "backend_kind",
        "pipe_name",
        "shim_pid",
        "child_pid",
        "transport.kind",
        "transport.source",
    ] {
        assert!(
            !fn_body.contains(forbidden),
            "Batch 5+6 leaked backend field {forbidden:?} into `compact_status`. \
             CR C-4 requires compact JSON stay byte-stable."
        );
    }
}
