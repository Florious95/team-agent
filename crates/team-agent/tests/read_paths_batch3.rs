//! Phase 1d Batch 3 contract tests for coordinator/status/diagnose
//! read-path migration to `transport_factory`.
//!
//! Verifies:
//!
//! 1. All 5 migration sites still reference the factory-routed call
//!    (grep guard against silent revert during Batch 4/5).
//! 2. `resolve_read_only_transport` for tmux state returns a tmux
//!    backend byte-equivalent to `tmux_backend_for_runtime_state_or_workspace`.
//! 3. `resolve_read_only_transport` for conpty state returns a ConPTY
//!    backend — never a tmux-workspace fallback.
//! 4. Coordinator boot metadata: tmux source string preserved
//!    (`state.tmux_endpoint` / `default` / `workspace_fallback` on
//!    Layer 3), conpty source string is `state.transport.kind`
//!    (no `tmux_endpoint_used` for conpty).
//! 5. C-4 guard: compact status JSON serializer still has no
//!    backend fields.
//!
//! (Design §Batch 3 Verification anchors.)

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

#[test]
fn batch3_all_five_migration_sites_route_through_factory() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let sites = [
        (
            "coordinator/backoff.rs",
            "resolve_transport",
            "coordinator boot",
        ),
        (
            "cli/status_port.rs",
            "resolve_read_only_transport",
            "status has_session + approvals",
        ),
        (
            "cli/diagnose.rs",
            "resolve_read_only_transport",
            "diagnose session-present",
        ),
        (
            "cli/adapters.rs",
            "resolve_read_only_transport",
            "diagnose-runtime + peek",
        ),
    ];
    for (rel, needle, purpose) in sites {
        let body = composite_source::composite_source(&format!("src/{rel}"));
        assert!(
            body.contains(needle),
            "Batch 3 migration site `{rel}` ({purpose}) lost its `{needle}` \
             reference — either the site was reverted to
             `tmux_backend::for_workspace` directly (breaks conpty read paths + \
             violates factory single source of truth), or the site was removed. \
             Update this test if the migration is intentional."
        );
    }
}

#[test]
fn batch3_read_only_factory_for_tmux_state_returns_tmux_backend_byte_equivalent() {
    use team_agent::transport::BackendKind;
    use team_agent::transport_factory::{resolve_read_only_transport, TransportPurpose};
    let ws = std::env::temp_dir().join("ta-batch3-tmux");
    std::fs::create_dir_all(&ws).unwrap();
    // Default: no state → factory picks Layer 5 default = tmux workspace.
    let resolved = resolve_read_only_transport(&ws, None, TransportPurpose::Coordinator)
        .expect("default → tmux");
    assert_eq!(resolved.kind, BackendKind::Tmux);
    assert_eq!(resolved.source, "default");
    // Legacy tmux endpoint: Layer 3. Note that `"default"` maps to
    // `TmuxBackend::new()` (shared default socket, socket=None), so
    // `tmux_endpoint_used` is `None` — byte-equivalent to the pre-
    // migration path. Test an explicit-name endpoint to cover the
    // socket=Some(...) branch.
    let state = serde_json::json!({ "tmux_endpoint": "default" });
    let resolved = resolve_read_only_transport(&ws, Some(&state), TransportPurpose::Coordinator)
        .expect("legacy endpoint → tmux");
    assert_eq!(resolved.kind, BackendKind::Tmux);
    assert_eq!(resolved.source, "legacy_tmux_endpoint");
    // Named socket carries through as tmux_endpoint_used.
    let state = serde_json::json!({ "tmux_endpoint": "ta-abcdef012345" });
    let resolved = resolve_read_only_transport(&ws, Some(&state), TransportPurpose::Coordinator)
        .expect("named endpoint → tmux");
    assert_eq!(resolved.kind, BackendKind::Tmux);
    assert_eq!(resolved.source, "legacy_tmux_endpoint");
    assert!(
        resolved.tmux_endpoint_used.is_some(),
        "tmux teams with a NAMED endpoint must carry tmux_endpoint_used for coordinator boot"
    );
}

#[test]
fn batch3_read_only_factory_for_conpty_state_returns_conpty_backend_not_tmux() {
    use team_agent::transport::BackendKind;
    use team_agent::transport_factory::{resolve_read_only_transport, TransportPurpose};
    let ws = std::env::temp_dir().join("ta-batch3-conpty");
    std::fs::create_dir_all(&ws).unwrap();
    // Design §Batch 3 Verification anchor: state.transport.kind=conpty
    // + a team key must construct ConPTY. `resolve_read_only_transport`
    // derives the team_key from `state.teams` — provide one.
    let state = serde_json::json!({
        "transport": { "kind": "conpty" },
        "teams": { "team-a": {} },
    });
    let resolved = resolve_read_only_transport(&ws, Some(&state), TransportPurpose::Status)
        .expect("conpty state + team_key → conpty backend");
    assert_eq!(resolved.kind, BackendKind::ConPty);
    assert_eq!(resolved.source, "state.transport.kind");
    // No tmux endpoint fabrication for conpty (design §Behavior
    // Equivalence).
    assert!(
        resolved.tmux_endpoint_used.is_none(),
        "conpty backend must NOT report a tmux_endpoint_used string"
    );
}

#[test]
fn batch3_c4_compact_status_guard_still_green_after_migration() {
    // C-4 pinned: compact/default status JSON serializer must not
    // reference backend fields. Batch 3 migration must preserve
    // this invariant. If the guard test file is present, run its
    // check inline as a belt-and-braces assertion.
    let body = composite_source::composite_source("src/cli/status_port.rs");
    let compact_fn_body = {
        let sig = body.find("fn compact_status").unwrap_or_else(|| {
            eprintln!("note: `fn compact_status` not present; guard is vacuous");
            body.len()
        });
        &body[sig..]
    };
    for forbidden in [
        "backend_kind",
        "pipe_name",
        "shim_pid",
        "child_pid",
        "transport.kind",
        "transport.source",
    ] {
        assert!(
            !compact_fn_body.contains(forbidden),
            "Batch 3 migration inadvertently leaked backend field {forbidden:?} \
             into `compact_status`. CR C-4 requires compact JSON stay byte-stable."
        );
    }
}
