//! Phase-DX E3 RED contract: `send --to-name` diagnostic candidates.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E3 and §5 Phase-DX.
//! - CR `.team/artifacts/phase-dx-invariant-review.md` E3 supplements A/B/C.
//! - CR P0 red lines #1 and #6.
//!
//! Contract: named-address failure candidates are advisory diagnostics grouped
//! by recorded team/socket. They must never become binding or route authority.

#![allow(clippy::expect_used)]

use std::path::Path;

#[test]
fn e3_failed_to_name_returns_structured_advisory_candidates() {
    let named = source("src/cli/named_address.rs");
    let mut missing = Vec::new();
    for required in [
        "\"candidates\"",
        "\"pane_id\"",
        "\"session\"",
        "\"socket\"",
        "\"source\"",
        "\"advisory\"",
        "json!(true)",
    ] {
        if !named.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E3 RED: wrong/missing --to-name leader errors must return diagnostic candidates shaped as {{pane_id, session, socket, source, advisory:true}}, not plain strings or route authority. Missing markers: {missing:?}"
    );
}

#[test]
fn e3_named_address_resolver_does_not_write_owner_or_receiver_fields() {
    let named = source("src/cli/named_address.rs");
    for forbidden in [
        "leader_receiver.*=",
        "team_owner.*=",
        "owner_epoch.*=",
        "\"leader_receiver\":",
        "\"team_owner\":",
        "\"owner_epoch\":",
    ] {
        assert!(
            !named.contains(forbidden),
            "E3 RED guard: crate::cli::named_address resolver must not write {forbidden}; candidate ranking is diagnostic-only"
        );
    }
}

#[test]
fn e3_socket_candidate_source_is_recorded_state_not_global_tmux_scan() {
    let named = source("src/cli/named_address.rs");
    assert!(
        named.contains("tmux_socket") || named.contains("tmux_endpoint"),
        "E3 RED precondition: candidate diagnostics must be tied to recorded socket fields"
    );
    assert!(
        !named.contains("list-sessions") && !named.contains("list_sessions"),
        "E3 RED guard: --to-name diagnostics may list targets on recorded sockets, but must not scan all tmux servers/sessions as reverse authority"
    );
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}
