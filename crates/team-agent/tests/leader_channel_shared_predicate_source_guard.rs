//! Source-guard (verifier, leader-inbound batch ①): the leader-channel socket
//! authority predicate is SINGLE-SOURCED. The named-address send path
//! (`cli::named_address`) and the internal delivery path (`messaging::delivery`)
//! must BOTH resolve the live leader channel through the one shared function
//! `messaging::leader_channel::resolve_live_leader_channel` — never re-hand-roll
//! their own `(pane_id, socket)` matching.
//!
//! Why a source-guard, not a behavior RED: the divergence that ① names —
//! parent's named path accepted a same-pane-id on a FOREIGN socket while the
//! delivery path refused it — is an `EndpointMismatch`. On the real product path
//! the delivery transport is self-built from `receiver.tmux_socket`
//! (delivery.rs `delivery_transport_for_recipient`), so `transport.endpoint ==
//! receiver.socket` always and `EndpointMismatch` is unreachable at runtime (it
//! collapses to `PaneNotLive`, where BOTH lineages already refuse — no behavior
//! split to pin). And the `pub(crate)` named resolve entrypoints are not
//! reachable from an integration test. So ①'s guarantee is STRUCTURAL: both
//! callers route through the shared resolver, so a future edit cannot silently
//! re-split the predicate. This is the same structural-lock form as the
//! slice2b current-alias source-guard (smell34 lock-anchor lesson): key on the
//! shared-symbol structure, not on fragile line text. Renaming the shared
//! function is a legitimate refactor; re-inlining a bare pane/socket match at
//! either call site is not.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/composite_source.rs"]
mod composite_source;

/// The single shared resolver symbol both paths must route through.
const SHARED_RESOLVER: &str = "resolve_live_leader_channel";

fn named_src() -> String {
    composite_source::composite_source("src/cli/named_address.rs")
}

fn delivery_src() -> String {
    composite_source::composite_source("src/messaging/delivery.rs")
}

fn leader_channel_src() -> String {
    composite_source::composite_source("src/messaging/leader_channel.rs")
}

/// The shared resolver must be DEFINED exactly once (in leader_channel.rs), and
/// both the named path and the delivery path must reference that same symbol —
/// binding one shared function is the structural anchor that keeps the two
/// paths' socket authority identical.
#[test]
fn named_and_delivery_both_route_through_the_single_shared_resolver() {
    let leader_channel = leader_channel_src();
    assert!(
        leader_channel.contains(&format!("pub fn {SHARED_RESOLVER}")),
        "the shared leader-channel resolver {SHARED_RESOLVER} must be DEFINED in leader_channel.rs \
         (single source of the socket authority predicate)"
    );

    let named = named_src();
    assert!(
        named.contains(SHARED_RESOLVER),
        "the named-address send path must resolve the live leader channel via the shared \
         {SHARED_RESOLVER}, not a re-hand-rolled pane/socket match"
    );

    let delivery = delivery_src();
    assert!(
        delivery.contains(SHARED_RESOLVER),
        "the internal delivery path must resolve the live leader channel via the shared \
         {SHARED_RESOLVER} (same predicate as the named path)"
    );
}

/// Teeth (positive structural lock, smell34 form): the named path's leader
/// resolver `resolve_leader` must ROUTE THROUGH the shared resolver — its body
/// must contain a `resolve_live_leader_channel` call. The parent's split was a
/// `resolve_leader` that hand-rolled a `targets`-by-`pane_id` match WITHOUT the
/// shared socket authority (and carried a `stale_pane_seen` bookkeeping var).
/// If a future edit strips the shared-resolver call back out of `resolve_leader`
/// (re-inlining the bare pane/socket match), this guard fires. We lock the
/// positive routing structure, not fragile line text: the shared function may be
/// renamed (update SHARED_RESOLVER), but `resolve_leader` may not stop routing
/// through it.
#[test]
fn named_leader_resolver_routes_through_the_shared_resolver_not_a_hand_rolled_match() {
    let leader_channel = leader_channel_src();
    assert!(
        leader_channel.contains("pane_id")
            && (leader_channel.contains("list_targets") || leader_channel.contains("targets")),
        "the shared resolver must own the pane/target match (single source of truth)"
    );

    let named = named_src();
    // The named leader resolver exists as a thin shell, but its socket authority
    // MUST come from the shared resolver. Assert the call sits inside the named
    // leader resolver's body (between its `fn resolve_leader(` and the next
    // top-level `fn ` at column 0).
    let Some(start) = named.find("fn resolve_leader(") else {
        panic!("named path must define the leader resolver shell `resolve_leader`");
    };
    let body = &named[start..];
    let end = body
        .match_indices("\nfn ")
        .next()
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    let resolve_leader_body = &body[..end];
    assert!(
        resolve_leader_body.contains(SHARED_RESOLVER),
        "the named leader resolver `resolve_leader` must route through the shared {SHARED_RESOLVER} \
         — a hand-rolled `targets`-by-`pane_id` match without the shared socket authority is \
         exactly the parent's split predicate"
    );
    // The parent's split bookkeeping var must be gone from the named path (it
    // only existed to support the hand-rolled leader pane match).
    assert!(
        !named.contains("stale_pane_seen"),
        "the parent's hand-rolled leader pane match (marked by `stale_pane_seen`) must not return \
         to the named path: leader channel liveness belongs to the shared {SHARED_RESOLVER}"
    );
}
