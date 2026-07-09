//! 0.5.20 doctor/orphan workspace-scope RED contracts.
//!
//! References:
//! - `.team/artifacts/coordinator-lifecycle-hardening-locate.md` §12.
//! - R1: foreign live coordinator metadata mismatch is not a current workspace
//!   orphan; it is ignored or reported only as `ignored_foreign/action:none`.
//! - R2: the same metadata mismatch in the current workspace remains reportable.
//! - R3: `--fix --confirm` must never terminate or mark a foreign workspace row
//!   killable in the default orphan gate.
//! - R4: packaging `doctor_gate_blockers(... Orphans ...)` uses the same
//!   current-workspace scope as the CLI gate.
//!
//! These contracts intentionally stay at the API/source boundary. The 0.5.19
//! orphan scanner is host-wide and can enumerate real tmux/process state; RED
//! must not risk touching or waiting on unrelated workspaces just to prove scope.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

#[test]
fn orphan_gate_ignores_foreign_live_metadata_mismatch() {
    let orphans = repo_file("crates/team-agent/src/diagnose/orphans.rs");
    let missing = missing_requirements(&[
        (
            "OrphanScanScope::CurrentWorkspace",
            orphans.contains("OrphanScanScope") && orphans.contains("CurrentWorkspace"),
        ),
        (
            "foreign rows split away from orphans",
            orphans.contains("ignored_foreign")
                && orphans.contains("foreign_workspace")
                && orphans.contains("\"none\""),
        ),
        (
            "scan filters coordinator process rows by current workspace before classification",
            orphans.contains("parse_workspace_arg")
                && (orphans.contains("workspace_matches_scope")
                    || orphans.contains("scope.contains_workspace")
                    || orphans.contains("canonical_current_workspace")),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R1 RED: default orphan gate must ignore/report-only foreign live \
         metadata_mismatch rows instead of placing them in blocking orphans; \
         missing={missing:?}"
    );
}

#[test]
fn orphan_gate_still_flags_current_workspace_metadata_mismatch() {
    let orphans = repo_file("crates/team-agent/src/diagnose/orphans.rs");

    assert!(
        orphans.contains("fn classify_workspace_orphan")
            && orphans.contains("OrphanReason::MetadataMismatch")
            && orphans.contains("coordinator_process_metadata_mismatch"),
        "R2 guard: current workspace coordinator metadata mismatch must remain \
         classifiable as MetadataMismatch; orphans.rs no longer shows that path"
    );
    assert!(
        orphans.contains("OrphanReason::MetadataMismatch => \"metadata_mismatch\""),
        "R2 guard: current workspace mismatch must keep JSON reason=metadata_mismatch"
    );
}

#[test]
fn orphan_gate_fix_confirm_never_kills_foreign() {
    let orphans = repo_file("crates/team-agent/src/diagnose/orphans.rs");
    let missing = missing_requirements(&[
        (
            "cleanup_report receives workspace/scope-filtered report",
            !orphans.contains("fn cleanup_report(report: ScanReport)")
                && (orphans.contains("fn cleanup_report(scope")
                    || orphans.contains("fn cleanup_report(report: ScopedScanReport")
                    || orphans.contains("fn cleanup_report(report: ScanReport, scope")),
        ),
        (
            "foreign rows are action none, never would_kill/killed",
            orphans.contains("ignored_foreign")
                && orphans.contains("foreign_workspace")
                && orphans.contains("\"none\""),
        ),
        (
            "terminate_pid_tree is guarded by current workspace ownership",
            !orphans.contains("if terminate_pid_tree(pid)")
                || orphans.contains("workspace_matches_scope")
                || orphans.contains("assert_current_workspace_orphan"),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R3 RED: orphan --fix --confirm must not be able to terminate a \
         foreign workspace coordinator through the default gate; missing={missing:?}"
    );
}

#[test]
fn packaging_doctor_gate_uses_workspace_scope() {
    let adapters = repo_file("crates/team-agent/src/cli/adapters.rs");
    let diagnose = repo_file("crates/team-agent/src/diagnose/mod.rs");
    let cli = repo_file("crates/team-agent/src/cli/mod.rs");
    let orphans = repo_file("crates/team-agent/src/diagnose/orphans.rs");
    let missing = missing_requirements(&[
        (
            "orphan_gate_json accepts workspace/scope",
            !orphans.contains("pub fn orphan_gate_json(fix: bool, confirm: bool)")
                && (orphans.contains("pub fn orphan_gate_json(workspace")
                    || orphans.contains("pub fn orphan_gate_json(scope")),
        ),
        (
            "cleanup_orphans_json accepts workspace/scope",
            !orphans.contains("pub fn cleanup_orphans_json(confirm: bool)")
                && (orphans.contains("pub fn cleanup_orphans_json(workspace")
                    || orphans.contains("pub fn cleanup_orphans_json(scope")),
        ),
        (
            "doctor CLI passes args.workspace into orphan gate",
            adapters.contains("orphan_gate_json(&args.workspace")
                || adapters.contains("orphan_gate_json(args.workspace.as_path()"),
        ),
        (
            "diagnose_port wrappers require workspace",
            !cli.contains("pub fn orphan_gate(fix: bool, confirm: bool)")
                && !cli.contains("pub fn cleanup_orphans(confirm: bool)"),
        ),
        (
            "packaging doctor_gate_blockers threads workspace",
            diagnose.contains("has_orphan_residue(workspace)")
                && diagnose.contains("orphan_blocker_detail(workspace)"),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R4 RED: packaging and CLI orphan gates must thread current workspace \
         scope into the orphan scanner/blocker APIs; missing={missing:?}"
    );
}

fn missing_requirements(requirements: &[(&'static str, bool)]) -> Vec<&'static str> {
    requirements
        .iter()
        .filter_map(|(label, ok)| (!ok).then_some(*label))
        .collect()
}

fn repo_file(relative: &str) -> String {
    std::fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
