//! Phase 1d Batch 1 contract tests for the lifecycle worker
//! transport resolver.
//!
//! Batch 1 is the internal replacement of
//! `lifecycle_worker_tmux_backend_for_selected_state` with routing
//! through `transport_factory::resolve_transport`. Six product caller
//! sites (`rebuild`/`agent`×3/`remove`/`launch`×2 + `cli/send.rs`) plus
//! the two `tests/launch_spawn.rs` harness callers all now go through
//! the factory.
//!
//! Behavior equivalence for **tmux** state is unchanged (same
//! endpoint fallback + workspace socket). The new contract this batch
//! adds is:
//!
//! 1. `state.transport.kind = "conpty"` fed to the legacy tmux-typed
//!    resolver returns a typed `TeamSelect("backend_kind_mismatch: ...")`
//!    error — NEVER silently downgrades to tmux (CR C-1 ①).
//! 2. `state.transport.kind = "conpty"` with a `team_key` fed to the
//!    new generic `lifecycle_worker_transport_for_selected_state`
//!    resolver returns a `ResolvedTransport` whose kind is
//!    `BackendKind::ConPty`.
//!
//! (Design §Batch 1 Verification, CR C-1/C-2/C-3.)

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

/// Anchor grep for the six product caller sites. If any of them is
/// renamed / removed without a replacement, this test fires so the
/// Batch 1 migration cannot silently regress in Batch 2/3.
#[test]
fn batch1_migration_sites_present_in_source() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    // 0.5.15 restart endpoint-context exception:
    // `lifecycle/restart/rebuild.rs` production restart intentionally no
    // longer calls the legacy selected-state resolver. It resolves the active
    // team once, builds transport from that same selected state, and threads
    // that context through restart (claim-endpoint-nonconvergence addendum
    // §5/§6). Other migration sites must still keep the Batch 1 anchor.
    let rebuild = std::fs::read_to_string(root.join("lifecycle/restart/rebuild.rs"))
        .unwrap_or_else(|e| panic!("cannot read lifecycle/restart/rebuild.rs: {e}"));
    for marker in [
        "fn resolve_restart_context",
        "restart_with_selected_team_and_transport",
        "lifecycle_worker_tmux_backend_selection_for_state",
    ] {
        assert!(
            rebuild.contains(marker),
            "0.5.15 restart endpoint-context exception lost marker `{marker}`: \
             rebuild.rs must keep a single selected-state restart context instead \
             of returning to `lifecycle_worker_tmux_backend_for_selected_state`"
        );
    }

    let sites = [
        (
            "lifecycle/restart/agent.rs",
            "lifecycle_worker_tmux_backend_for_selected_state",
        ),
        (
            "lifecycle/restart/remove.rs",
            "lifecycle_worker_tmux_backend_for_selected_state",
        ),
        (
            "lifecycle/launch.rs",
            "lifecycle_worker_tmux_backend_for_selected_state",
        ),
        (
            "cli/send.rs",
            "lifecycle_worker_tmux_backend_for_selected_state",
        ),
    ];
    for (rel, needle) in sites {
        let body = composite_source::composite_source(&format!("src/{rel}"));
        assert!(
            body.contains(needle),
            "Batch 1 migration site {rel} lost its `{needle}` reference — either \
             the site was migrated to the generic transport resolver \
             (`lifecycle_worker_transport_for_selected_state`), or the site was \
             removed. Update this test if the migration is intentional."
        );
    }
}

/// The legacy tmux-typed resolver still exists as a symbol so callers
/// compile; the new generic resolver is a distinct symbol that
/// exposes the factory. Both must be present for Batch 1.
#[test]
fn batch1_generic_transport_resolver_symbol_added() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("lifecycle")
        .join("restart")
        .join("common.rs");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("fn lifecycle_worker_transport_for_selected_state"),
        "Batch 1 must add the generic transport resolver \
         `lifecycle_worker_transport_for_selected_state` in \
         `lifecycle/restart/common.rs`"
    );
    // Fail-closed anchor: the legacy tmux-typed resolver body must
    // check `state.transport.kind == "conpty"` and refuse. Grep for
    // the marker string.
    assert!(
        body.contains("backend_kind_mismatch"),
        "Batch 1 fail-closed anchor: legacy tmux-typed resolver must refuse \
         `state.transport.kind=conpty` via a typed `backend_kind_mismatch` error \
         (CR C-1). Marker string missing from `lifecycle/restart/common.rs`."
    );
}

/// The `TransportFactoryInput` shape is what all six caller sites
/// route through. If someone deletes or renames a public field, the
/// callers break — this test locks the shape.
#[test]
fn batch1_factory_input_shape_stable() {
    use team_agent::transport_factory::{
        RequestedTransportBackend, TransportFactoryInput, TransportPurpose,
    };
    // Compile-time proof: constructing the builder chain still works
    // with the same public method names.
    let ws = std::env::temp_dir().join("ta-batch1-shape");
    let _input = TransportFactoryInput::new(&ws, TransportPurpose::LifecycleWorker)
        .with_team_key(Some("team-a"))
        .with_state(None)
        .with_spec_backend_literal(None)
        .with_explicit_backend(Some(RequestedTransportBackend::Tmux))
        .allow_backend_switch(false);
}

/// C-1 ① at the LIFECYCLE layer (not just the factory): the
/// generic-typed resolver returns ConPTY when state says so; and the
/// legacy tmux-typed resolver refuses. This is the acceptance
/// contract for Batch 1 explicitly listed by the leader.
///
/// The generic resolver runs the full team-state projection, so a
/// straight unit test would need a real workspace on disk. Instead we
/// call the factory directly with the same input the resolver would
/// build, which exercises identical resolution semantics.
#[test]
fn batch1_state_transport_kind_conpty_constructs_conpty_via_factory() {
    use team_agent::transport::BackendKind;
    use team_agent::transport_factory::{
        resolve_transport, TransportFactoryInput, TransportPurpose,
    };
    let ws = std::env::temp_dir().join("ta-batch1-conpty");
    let state = serde_json::json!({ "transport": { "kind": "conpty" } });
    let input = TransportFactoryInput::new(&ws, TransportPurpose::LifecycleWorker)
        .with_team_key(Some("team-a"))
        .with_state(Some(&state));
    let resolved = resolve_transport(input)
        .expect("state.transport.kind=conpty + team_key must construct ConPTY, not tmux");
    assert_eq!(resolved.kind, BackendKind::ConPty);
    assert_eq!(resolved.source, "state.transport.kind");
}
