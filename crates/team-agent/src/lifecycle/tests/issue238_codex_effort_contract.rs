//! Issue #238 lifecycle-side contracts for effort admission and Pi leader
//! argument admission. This module deliberately calls the same
//! lifecycle helpers used by spawn/restart instead of duplicating resolution.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use crate::lifecycle::launch::pi_mcp::parse_pi_leader_args;
use crate::lifecycle::launch::provider_effort_from_raw;
use crate::model::enums::{Provider, ProviderEffort};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn r6_unsupported_ultra_is_not_silently_dropped_on_legacy_provider() {
    let error = provider_effort_from_raw(Some("ultra"), Provider::Fake)
        .expect_err("ultra must not be silently ignored")
        .to_string();
    assert!(error.contains("effort 'ultra' is only supported by codex"), "{error}");
}

#[test]
fn r7_pi_leader_parser_resolves_thinking_by_provider_semantics() {
    let parsed = parse_pi_leader_args(&args(&[
        "--",
        "--model",
        "openai-codex/gpt-5.6-luna",
        "--thinking",
        "max",
    ]))
    .expect("Pi max arguments");
    assert_eq!(parsed.model.as_deref(), Some("openai-codex/gpt-5.6-luna"));
    assert_eq!(parsed.effort, Some(ProviderEffort::Max));

    let error = parse_pi_leader_args(&args(&[
        "--model",
        "openai-codex/gpt-5.6-luna",
        "--thinking",
        "ultra",
    ]))
    .expect_err("Pi ultra must be rejected by shared resolver")
    .to_string();
    assert!(error.contains("effort 'ultra' is only supported by codex"), "{error}");
}

#[test]
fn r7_pi_leader_parser_keeps_boundary_errors_fail_closed() {
    for invalid in [
        args(&["--thinking"]),
        args(&["--unknown", "value"]),
        args(&["--model", "codex"]),
        args(&["--model", "openai-codex/gpt-5.6-luna", "--model", "other/x"]),
    ] {
        assert!(parse_pi_leader_args(&invalid).is_err(), "must reject {invalid:?}");
    }

    let no_effort = parse_pi_leader_args(&args(&["--model", "openai-codex/gpt-5.6-luna"]))
        .expect("Pi model-only arguments");
    assert_eq!(no_effort.effort, None);
}

#[test]
fn r7_pi_provider_authority_is_not_model_namespace_authority() {
    let error = provider_effort_from_raw(Some("ultra"), Provider::Pi)
        .expect_err("Pi ultra");
    assert!(error.to_string().contains("effort 'ultra' is only supported by codex"));
}
