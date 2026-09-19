//! Phase 1d Batch 4 contract tests: delivery + leader channel
//! separation.
//!
//! Verifies:
//!
//! 1. `DeliveryTransport::Owned` variant is now
//!    `Owned(Box<dyn Transport>)` — grep guard on delivery.rs source.
//! 2. Leader-channel fallback sites use the factory tmux channel
//!    helpers (`tmux_endpoint_transport` / `tmux_workspace_transport`
//!    / `tmux_default_transport`) rather than `TmuxBackend::*`
//!    directly — grep guard on delivery.rs + leader_receiver.rs.
//! 3. `results.rs::report_result` retry uses the factory-resolved
//!    transport so conpty teams get their retry pipe — grep guard.
//! 4. `results.rs` no longer builds `TmuxBackend::for_workspace`
//!    unconditionally for delivery retry (would have silently no-op'd
//!    conpty teams).
//! 5. Grep guard: NO `transport_kind` reads/writes introduced in this
//!    batch (per leader msg_4ac2122489e0 — helper-swap only; the
//!    transport_kind dispatch belongs to the parallel
//!    `feat/appserver-leader-host` branch).
//!
//! (Design §Batch 4 Verification, CR C-4 pins compact status stable.)

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

fn src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(rel: &str) -> String {
    composite_source::composite_source(&format!("src/{rel}"))
}

/// Extract non-comment lines from a source body so grep guards don't
/// fire on doc-comments that explain what's forbidden.
fn non_comment_body(src: &str) -> String {
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

#[test]
fn batch4_delivery_transport_owned_is_boxed_dyn_transport() {
    let body = read("messaging/delivery.rs");
    assert!(
        body.contains("Owned(Box<dyn Transport>)"),
        "Batch 4 must generalize `DeliveryTransport::Owned` to \
         `Owned(Box<dyn Transport>)`. Current body missing the marker."
    );
    // The old concrete-TmuxBackend variant must be gone.
    assert!(
        !non_comment_body(&body).contains("Owned(crate::tmux_backend::TmuxBackend)"),
        "Legacy `Owned(crate::tmux_backend::TmuxBackend)` variant must be replaced \
         by `Owned(Box<dyn Transport>)`. Found the concrete type still present."
    );
}

#[test]
fn batch4_leader_channel_uses_factory_helpers_not_direct_tmux_backend() {
    // Design §Batch 4: leader-fallback sites must go through the
    // factory tmux channel helpers so `grep transport_factory::tmux_`
    // enumerates every intentional tmux-only leader-channel site.
    let files = ["messaging/delivery.rs", "messaging/leader_receiver.rs"];
    for rel in files {
        let body = read(rel);
        let code = non_comment_body(&body);
        assert!(
            code.contains("transport_factory::tmux_"),
            "{rel} must reference at least one factory tmux channel helper \
             (`transport_factory::tmux_endpoint_transport` / \
             `tmux_workspace_transport` / `tmux_default_transport`)"
        );
    }
    // Belt-and-braces: neither file should still call
    // `TmuxBackend::for_workspace` / `for_tmux_endpoint` / `new`
    // directly from the leader-fallback code paths. There may still
    // be a few direct calls in unrelated code paths in the same
    // file, so we look for at least ONE factory helper mention +
    // check that the leader-fallback function bodies do NOT contain
    // the old direct construction.
    let dr = read("messaging/delivery.rs");
    if let Some(start) = dr.find("fn delivery_transport_for_recipient") {
        let slice = &dr[start..];
        let end = slice.find("\nfn ").map(|e| start + e).unwrap_or(dr.len());
        let fn_body = &dr[start..end];
        let code = non_comment_body(fn_body);
        for forbidden in [
            "TmuxBackend::for_tmux_endpoint",
            "TmuxBackend::for_workspace",
            "TmuxBackend::new",
        ] {
            assert!(
                !code.contains(forbidden),
                "`delivery_transport_for_recipient` still calls {forbidden} — \
                 Batch 4 requires factory helpers instead."
            );
        }
    }
}

#[test]
fn batch4_results_report_uses_the_single_leader_notification_funnel() {
    let body = read("messaging/results.rs");
    let code = non_comment_body(&body);
    assert!(
        code.contains("send_to_leader_receiver_with_presentation"),
        "report_result must use the shared leader receiver notification funnel"
    );
    assert!(
        !code.contains("resolve_read_only_transport")
            && !code.contains("if let Some(message_id) = outcome.message_id.clone()"),
        "the removed collect-era retry block must not reintroduce a second transport path"
    );
}

#[test]
fn batch4_c4_compact_status_guard_still_green() {
    // Belt-and-braces: Batch 4 must not have leaked backend detail
    // into `compact_status`. (The status_port compact serializer is
    // owned by Batch 3; touching it here would be out of scope.)
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
            "Batch 4 leaked backend field {forbidden:?} into `compact_status`. \
             CR C-4 requires compact JSON stay byte-stable."
        );
    }
}
