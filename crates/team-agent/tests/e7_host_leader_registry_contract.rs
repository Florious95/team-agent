//! E7 RED contract: host leader registry + `leaders` + `send --to-leader`.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E7.
//! - design `.team/artifacts/host-leader-registry-design.md` §§3, 4, 6, 8, 9, 11.
//! - CR red-line spirit from `.team/artifacts/phase-dx-invariant-review.md`:
//!   registry/advisory discovery must not become identity truth or route authority.
//!
//! Contract: `~/.team-agent/leaders` is a derived host discovery index. The
//! send path resolves it back to canonical workspace/team state, then delegates
//! to E6 live/offline leader delivery without writing identity fields.

#![allow(clippy::expect_used)]

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::{Path, PathBuf};

#[test]
fn e7_registry_module_defines_v1_file_schema_and_only_derived_events() {
    let registry = source("src/leader/registry.rs");
    assert!(
        !registry.is_empty(),
        "E7 RED: add src/leader/registry.rs as the host-level derived index module; registry is not state truth and not shared SQLite"
    );

    let mut missing = Vec::new();
    for required in [
        "schema_version",
        "delivery_name",
        "qualified_name",
        "stable_qualified_name",
        "aliases",
        "workspace",
        "workspace_hash",
        "workspace_short",
        "team_key",
        "transport_kind",
        "channel",
        "owner_epoch",
        "attached_at",
        "updated_at",
        "leader_registry.registered",
        "leader_registry.write_failed",
        "leader_registry.unregistered",
    ] {
        if !registry.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E7 RED: registry v1 file schema and derived event names must be present. Missing markers: {missing:?}"
    );
}

#[test]
fn e7_cli_exposes_leaders_command_and_to_leader_parser() {
    let emit = source("src/cli/emit.rs");
    let types = source("src/cli/types.rs");
    let send = source("src/cli/send.rs");
    let combined = format!("{emit}\n{types}\n{send}");

    let mut missing = Vec::new();
    for required in [
        "\"leaders\"",
        "cmd_leaders",
        "--to-leader",
        "to_leader",
        "leader_name_not_found",
        "name_ambiguous",
        "registry_stale",
        "resolved_via",
        "host_leader_registry",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E7 RED: CLI must expose `team-agent leaders` and parse `send --to-leader NAME` as a mutually exclusive leader target, with typed not_found/ambiguous/stale output. Missing markers: {missing:?}"
    );
}

#[test]
fn e7_leaders_json_lists_live_stale_ambiguous_leaders_only() {
    let combined = [
        source("src/leader/registry.rs"),
        source("src/cli/leaders.rs"),
        source("src/cli/mod.rs"),
        source("src/cli/emit.rs"),
    ]
    .join("\n");

    let mut missing = Vec::new();
    for required in [
        "\"registry_dir\"",
        "\"leaders\"",
        "\"ambiguous_names\"",
        "\"LIVE\"",
        "\"STALE\"",
        "\"AMBIGUOUS\"",
        "\"stale_reason\"",
        "\"send_hint\"",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E7 RED: `team-agent leaders --json` must list leaders only, validate entries against canonical state, expose LIVE/STALE/AMBIGUOUS and ambiguous_names, and not list workers/tasks/results. Missing markers: {missing:?}"
    );
    for forbidden in [
        "worker_id",
        "task_id",
        "result_id",
        "\"agents\"",
        "\"tasks\"",
        "\"results\"",
    ] {
        assert!(
            !source("src/cli/leaders.rs").contains(forbidden),
            "E7 RED guard: leaders command is host leader discovery only and must not list {forbidden}"
        );
    }
}

#[test]
fn e7_to_leader_delegates_to_e6_mailbox_and_preserves_honest_delivery_state() {
    let send = source("src/cli/send.rs");
    let registry = source("src/leader/registry.rs");
    let combined = format!("{send}\n{registry}");

    let mut missing = Vec::new();
    for required in [
        "send_to_canonical_leader_target",
        "queued_until_leader_attach",
        "leader_mailbox",
        "delivered",
        "name_ambiguous",
        "candidates",
        "workspace_hash",
        "stable_qualified_name",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E7 RED: send --to-leader must resolve short/qualified/hash names through registry, refuse ambiguous names with candidates/no side effects, and delegate live/offline delivery to E6 without pretending queued mailbox rows are delivered. Missing markers: {missing:?}"
    );
}

#[test]
fn e7_send_and_registry_paths_do_not_write_identity_fields() {
    let offenders = identity_write_offenders(&[
        "src/cli/send.rs",
        "src/messaging/watchers.rs",
        "src/messaging/results.rs",
        "src/coordinator/tick.rs",
        "src/leader/registry.rs",
    ]);
    assert!(
        offenders.is_empty(),
        "E7 RED guard: send/report_result/coordinator/watch/registry paths must not write canonical leader_receiver/team_owner/owner_epoch; registry is only a derived discovery index. Offenders: {offenders:#?}"
    );
}

#[test]
fn e7_registry_does_not_introduce_sqlite_or_global_db() {
    let registry = source("src/leader/registry.rs");
    let mut offenders = Vec::new();
    for (idx, line) in registry.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("sqlite")
            || lower.contains("rusqlite")
            || lower.contains(".db")
            || lower.contains("connection::open")
        {
            offenders.push((idx + 1, line.trim().to_string()));
        }
    }

    assert!(
        offenders.is_empty(),
        "E7 RED guard: host leader registry is file-per-team discovery under ~/.team-agent/leaders, not a shared SQLite/global DB. Offenders: {offenders:#?}"
    );
}

#[test]
fn e7_short_name_collision_refuses_instead_of_first_writer_wins() {
    let combined = format!(
        "{}\n{}",
        source("src/leader/registry.rs"),
        source("src/cli/send.rs")
    );
    let mut missing = Vec::new();
    for required in ["ambiguous_names", "name_ambiguous", "candidates"] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }
    assert!(
        missing.is_empty(),
        "E7 RED: two live registry entries with the same short name must both remain visible and send --to-leader <short> must return name_ambiguous with candidates, not pick a winner. Missing markers: {missing:?}"
    );

    let forbidden = [
        "first_writer",
        "first-writer",
        "first writer wins",
        "first match wins",
        "first live wins",
        "mtime",
    ];
    let offenders = forbidden
        .iter()
        .filter(|needle| combined.to_ascii_lowercase().contains(**needle))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "E7 RED guard: short-name collision handling must not use first-writer/mtime priority. Forbidden markers present: {offenders:?}"
    );
}

fn source(rel: &str) -> String {
    composite_source::composite_source(rel)
}

fn identity_write_offenders(files: &[&str]) -> Vec<(PathBuf, usize, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for rel in files {
        let path = root.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            let writes_identity = trimmed.contains(".insert(\"leader_receiver\"")
                || trimmed.contains(".insert(\"team_owner\"")
                || trimmed.contains(".insert(\"owner_epoch\"")
                || trimmed.contains("\"leader_receiver\":")
                || trimmed.contains("\"team_owner\":")
                || trimmed.contains("\"owner_epoch\":");
            if writes_identity {
                offenders.push((PathBuf::from(rel), idx + 1, trimmed.to_string()));
            }
        }
    }
    offenders
}
