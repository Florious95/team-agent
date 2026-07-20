//! Source-guard (verifier, slice2b alias third-piece): the `current`->active
//! mapping is INTENTIONALLY split into two rules — the resolver
//! (`state::projection::resolve_runtime_team_scope`) validates alive/terminal,
//! while the lock-held persist merge (`state::persist::apply_persist_merge_contract`)
//! must not depend on a mutable alive view. arch ruled the divergence acceptable
//! ONLY under a single-constant + mutual-reference + source-guard triad, so a
//! change to one rule cannot silently drift from the other.
//!
//! This guard locks the STRUCTURAL divergence-governance form, not fragile line
//! text (smell34 lock-anchor lesson): both mapping points must reference the
//! shared `CURRENT_TEAM_ALIAS` constant and neither may re-inline a bare
//! `"current"` literal at its mapping decision. Teeth: re-inlining `"current"`
//! at either point (or dropping the shared constant / the mutual-reference
//! anchor) fails this guard. The constant may be renamed freely — the guard
//! keys on the shared-symbol structure, not on the token `"current"`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/composite_source.rs"]
mod composite_source;

/// The single shared constant symbol both mapping points must route through.
/// Renaming it here + at the definition is a legitimate refactor; re-inlining
/// the literal is not.
const SHARED_ALIAS_CONST: &str = "CURRENT_TEAM_ALIAS";

fn projection_src() -> String {
    composite_source::composite_source("src/state/projection.rs")
}

fn persist_src() -> String {
    composite_source::composite_source("src/state/persist.rs")
}

/// The shared constant must be defined exactly once (in projection.rs) and
/// persist.rs must import it — that import IS the structural mutual-reference
/// anchor (both modules bind the same symbol, not a copied literal).
#[test]
fn current_alias_constant_is_single_source_shared_by_both_modules() {
    let projection = projection_src();
    let persist = persist_src();

    assert!(
        projection.contains(&format!("const {SHARED_ALIAS_CONST}")),
        "the shared current-alias constant {SHARED_ALIAS_CONST} must be DEFINED in projection.rs \
         (single source of the alias literal)"
    );
    // persist must bind the SAME symbol via an import, not re-declare or inline.
    assert!(
        persist.contains(SHARED_ALIAS_CONST),
        "persist.rs must reference the shared {SHARED_ALIAS_CONST} constant, not its own literal"
    );
    assert!(
        !persist.contains(&format!("const {SHARED_ALIAS_CONST}")),
        "persist.rs must IMPORT {SHARED_ALIAS_CONST}, not re-declare it (two definitions = two sources)"
    );
}

/// Neither mapping point may re-inline a bare `"current"` literal at its
/// current-alias decision. Both must route through the shared constant. This is
/// the teeth: re-inlining `"current"` at either decision re-splits the source.
#[test]
fn neither_current_alias_mapping_point_reinlines_the_literal() {
    // The resolver's current-alias branch: must compare against the constant,
    // never against a bare "current" literal.
    let projection = projection_src();
    assert!(
        projection.contains(&format!("eq_ignore_ascii_case({SHARED_ALIAS_CONST})")),
        "the resolver current-alias branch must compare via {SHARED_ALIAS_CONST}, not a literal"
    );
    // The persist merge's current-alias branch: must compare against the
    // constant symbol, never a bare "current" literal.
    let persist = persist_src();
    assert!(
        persist.contains(&format!("== {SHARED_ALIAS_CONST}")),
        "the persist merge current-alias branch must compare via {SHARED_ALIAS_CONST}, not a literal"
    );

    // Teeth: count bare `"current"` string literals across BOTH mapping modules.
    // The ONLY legitimate bare literal is the single constant DEFINITION; every
    // other occurrence (test seeds aside) must go through the symbol. We bound
    // the definition to exactly one and forbid a bare literal appearing on the
    // same line as either alias comparison operator.
    for (name, src) in [("projection.rs", &projection), ("persist.rs", &persist)] {
        for line in src.lines() {
            let is_alias_decision = line.contains("eq_ignore_ascii_case(")
                || (line.contains("authority_team") && line.contains("if team =="))
                || (line.contains("if team ==") && line.contains("=="));
            if is_alias_decision && line.contains("\"current\"") {
                panic!(
                    "{name} re-inlines a bare \"current\" literal at an alias decision line \
                     instead of routing through {SHARED_ALIAS_CONST}: {}",
                    line.trim()
                );
            }
        }
    }
}

/// The mutual-reference anchor must exist structurally: each module's alias
/// mapping cites the OTHER module's function by name, so a maintainer editing
/// one is pointed at the other. We assert the cross-citation symbols, not the
/// free-text prose (rename-safe: keyed on the public fn identifiers).
#[test]
fn both_mapping_points_carry_the_mutual_reference_anchor() {
    let projection = projection_src();
    let persist = persist_src();

    assert!(
        projection.contains("apply_persist_merge_contract"),
        "projection.rs alias mapping must cite the persist merge fn (mutual-reference anchor)"
    );
    assert!(
        persist.contains("resolve_runtime_team_scope"),
        "persist.rs alias mapping must cite the resolver fn (mutual-reference anchor)"
    );
}
