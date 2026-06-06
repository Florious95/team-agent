// ===========================================================================
// RED CONTRACTS (wave-2) — step 8 provider parity, encoded against v0.2.11
// truth source `team-agent-public` @ 439bef8. Golden values captured via
// PYTHONPATH=…/src python3 /tmp/probe_{turn_state,idle_predicate,approvals}.py.
//
// Two tiers:
//  * WIRE/PREDICATE tier — pure enum serde + helper-method contracts. These pin
//    the exact Python byte strings (`TurnState`/`FactKind`/`Confidence`/…) and
//    the §11 fail-safe predicates so the porter cannot drift a rename or sneak a
//    `_ => idle` fallthrough. (Skeleton enums are already concrete → tier is the
//    locked spec the impl must keep satisfying, not the RED driver.)
//  * BEHAVIORAL tier — drives every parity-critical behavior through
//    `get_adapter(..)` + `ProviderAdapter` trait methods, which are
//    `unimplemented!()` in ROUND-0 → these PANIC = genuinely RED today, and only
//    green once the porter wires the adapters AND the neutral classify/idle/
//    approval pipeline the trait sits on top of.
// ===========================================================================
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde::Serialize;
use std::path::PathBuf;

fn ser(v: &impl Serialize) -> String {
    serde_json::to_string(v).expect("serialize")
}

include!("tests/wire.rs");
include!("tests/classify.rs");
include!("tests/idle.rs");
include!("tests/adapter.rs");
include!("tests/faults.rs");
