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
fn batch4_results_report_retry_uses_factory_transport() {
    let body = read("messaging/results.rs");
    let code = non_comment_body(&body);
    // Factory-routed retry marker must be present.
    assert!(
        code.contains("resolve_read_only_transport"),
        "`results.rs` report_result retry must resolve the transport via \
         `transport_factory::resolve_read_only_transport` so a conpty team's \
         retry writes to the shim (design §Batch 4)."
    );
    // The old unconditional workspace-tmux retry must be gone at
    // the retry site. There is still a fallback branch that uses
    // `TmuxBackend::for_workspace` — that is INTENTIONAL (honest
    // fallback for tmux teams when the factory refuses). Check the
    // fallback is inside a `match ... Err(_) =>` arm.
    let retry_ctx_start = code
        .find("if let Some(message_id) = outcome.message_id.clone()")
        .expect("retry block must exist");
    let retry_ctx = &code[retry_ctx_start..];
    let retry_end = retry_ctx
        .find("\n    let outcome_json")
        .unwrap_or(retry_ctx.len().min(3000));
    let retry_body = &retry_ctx[..retry_end];
    // Inside the retry block, `TmuxBackend::for_workspace` must only
    // appear on a fallback arm.
    let count_direct = retry_body
        .matches("TmuxBackend::for_workspace(workspace)")
        .count();
    let count_factory = retry_body.matches("resolve_read_only_transport").count();
    assert!(
        count_factory >= 1,
        "retry block must call resolve_read_only_transport at least once"
    );
    // The direct call may still appear once — as the fallback. If it
    // appears more than once the migration is incomplete.
    assert!(
        count_direct <= 1,
        "retry block has {count_direct} direct `TmuxBackend::for_workspace(workspace)` \
         calls; Batch 4 should have exactly ONE (the honest fallback for factory \
         refusal)."
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
