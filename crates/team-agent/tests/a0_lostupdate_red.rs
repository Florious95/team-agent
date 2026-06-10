//! A0 lost-update residual gaps: R1/R2 diff-regression contracts + A0 GREEN regression lock.
//!
//! Locate doc (sole basis): `.team/artifacts/a0-rs-lostupdate-locate.md` (fable-architect).
//! Python 0.2.11 truth: coordinator tick does whole-file load->mutate->save with NO merge
//! (state.py:493), so a concurrent add-agent registration is permanently overwritten. RS has
//! a structural in-lock reload+merge guard at the single save chokepoint
//! (persist.rs:210-221, preserve_latest_roster_entries :272-313) — A0 proper does NOT
//! reproduce — but two narrow residual gaps remain:
//!
//! R1: the `same_runtime_projection` gate (persist.rs:273-275) short-circuits the WHOLE
//!     preserve pass, including the per-team `teams.<k>.agents` merge (:293-304) that is
//!     team-key self-identifying and safe regardless of which team is active.
//! R2: `preserve_missing_agents` (persist.rs:387-389) only fills MISSING agent keys
//!     (`entry().or_insert_with`); for rows present on both sides the stale incoming row
//!     wins wholesale, silently regressing session-capture fields written between the
//!     writer's load and save (same family disease as Python).
//!
//! Test shape per locate doc §5: all writers go through the one chokepoint, so the race
//! is reduced to a single-threaded stale-snapshot direct test — latest on disk, stale
//! incoming in memory, one `save_runtime_state` call, assert what landed on disk.
//! Deterministic: zero sleeps, zero real races, zero tmux.
//!
//! State fixtures mirror the real-machine `.team/runtime/state.json` shape
//! (session_name / active_team_key / agents / teams dual projection; agent rows carry the
//! session-capture field family session_id / rollout_path / captured_at / captured_via /
//! attribution_confidence / spawn_cwd). No invented fields.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

/// RED-R1 (gate boundary loses a registration): when another process flipped
/// `active_team_key` (e.g. quick-start of team-b) between this writer's load and save,
/// the projection gate must not short-circuit the per-team preserve — `teams.team-b`
/// entries are identified by their own key and merging them is always safe.
/// Today persist.rs:273-275 returns early and the team-b registration is lost.
#[test]
fn red_r1_per_team_preserve_must_survive_projection_gate_mismatch() {
    let ws = tmp_ws("r1-gate");
    // Disk latest: another process registered `new_agent` into teams.team-b and flipped
    // the active team key to team-b.
    write_disk_state(
        &ws,
        &json!({
            "session_name": "team-a",
            "active_team_key": "team-b",
            "agents": { "w1": agent_row("w1", Some("sess-w1")) },
            "teams": {
                "team-a": { "session_name": "team-a", "agents": { "w1": agent_row("w1", Some("sess-w1")) } },
                "team-b": { "session_name": "team-b", "agents": { "new_agent": agent_row("new_agent", None) } },
            },
        }),
    );
    // In-memory incoming: this writer's stale snapshot, taken while team-a was still
    // active and before new_agent registered (its teams.team-b roster is stale-empty).
    let incoming = json!({
        "session_name": "team-a",
        "active_team_key": "team-a",
        "agents": { "w1": agent_row("w1", Some("sess-w1")) },
        "teams": {
            "team-a": { "session_name": "team-a", "agents": { "w1": agent_row("w1", Some("sess-w1")) } },
            "team-b": { "session_name": "team-b", "agents": {} },
        },
    });

    save_runtime_state(&ws, &incoming).expect("stale-snapshot save should succeed");

    let saved = read_disk_state(&ws);
    assert!(
        saved
            .pointer("/teams/team-b/agents/new_agent")
            .is_some_and(Value::is_object),
        "R1: the in-lock per-team preserve (persist.rs:293-304) must run even when the \
projection gate (persist.rs:273-275) sees a different active_team_key — teams.<k> entries \
are self-identifying and the team-b registration must survive a stale team-a snapshot save. \
saved={saved}"
    );
}

/// RED-R2 (field-level regression on existing rows): session capture wrote
/// `session_id`/rollout fields to an existing agent row between this writer's load and
/// save; the preserve pass must back-fill those capture fields from latest when the
/// stale incoming row still has them null (field monotonicity). Today
/// `preserve_missing_agents` only or_inserts missing keys, so the stale nulls win.
#[test]
fn red_r2_session_capture_fields_must_not_regress_from_stale_snapshot() {
    let ws = tmp_ws("r2-fields");
    let captured = json!({
        "status": "running",
        "provider": "codex",
        "agent_id": "w1",
        "window": "w1",
        "session_id": "sess-cap-1",
        "rollout_path": "/home/user/.codex/sessions/rollout-sess-cap-1.jsonl",
        "captured_at": "2026-06-10T03:00:00+00:00",
        "captured_via": "fs_watch",
        "attribution_confidence": "high",
        "spawn_cwd": "/ws",
    });
    write_disk_state(
        &ws,
        &json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": { "w1": captured },
        }),
    );
    // Stale snapshot: same agent row from before the capture landed (capture family null).
    let incoming = json!({
        "session_name": "team-a",
        "active_team_key": "team-a",
        "agents": { "w1": agent_row("w1", None) },
    });

    save_runtime_state(&ws, &incoming).expect("stale-snapshot save should succeed");

    let saved = read_disk_state(&ws);
    let row = saved
        .pointer("/agents/w1")
        .cloned()
        .unwrap_or(Value::Null);
    let mut failures = Vec::new();
    for (field, expected) in [
        ("session_id", json!("sess-cap-1")),
        ("rollout_path", json!("/home/user/.codex/sessions/rollout-sess-cap-1.jsonl")),
        ("captured_at", json!("2026-06-10T03:00:00+00:00")),
        ("captured_via", json!("fs_watch")),
        ("attribution_confidence", json!("high")),
    ] {
        if row.get(field) != Some(&expected) {
            failures.push(format!(
                "R2: agents.w1.{field} regressed to {:?}; latest had {expected} and the stale \
incoming row had null — capture fields must be monotonic across the in-lock merge \
(persist.rs:369-391)",
                row.get(field)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "R2 session-capture field monotonicity contract failed:\n{}\nsaved_row={row}",
        failures.join("\n")
    );
}

/// GREEN regression lock (A0 proper): the existing in-lock reload + missing-key merge is
/// the structural guard that makes RS NOT reproduce Python's A0 lost-update. Lock it so
/// the F1/F2 fixes (or any future refactor) cannot regress it: a registration present on
/// disk in BOTH projections (top-level agents + teams.<active>.agents) must survive a
/// stale-snapshot save that lacks it, when the projection gate matches.
#[test]
fn green_a0_lock_stale_snapshot_save_preserves_new_agent_in_both_projections() {
    let ws = tmp_ws("a0-green");
    write_disk_state(
        &ws,
        &json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": agent_row("w1", Some("sess-w1")),
                "joined": agent_row("joined", None),
            },
            "teams": {
                "team-a": {
                    "session_name": "team-a",
                    "agents": {
                        "w1": agent_row("w1", Some("sess-w1")),
                        "joined": agent_row("joined", None),
                    },
                },
            },
        }),
    );
    let incoming = json!({
        "session_name": "team-a",
        "active_team_key": "team-a",
        "agents": { "w1": agent_row("w1", Some("sess-w1")) },
        "teams": {
            "team-a": { "session_name": "team-a", "agents": { "w1": agent_row("w1", Some("sess-w1")) } },
        },
    });

    save_runtime_state(&ws, &incoming).expect("stale-snapshot save should succeed");

    let saved = read_disk_state(&ws);
    for pointer in ["/agents/joined", "/teams/team-a/agents/joined"] {
        assert!(
            saved.pointer(pointer).is_some_and(Value::is_object),
            "A0 GREEN lock: registration `joined` must be preserved at {pointer} by the \
in-lock reload+merge (persist.rs:214,221,276,298) — this is the guard that blocks \
Python's A0 lost-update and it must not be removed by F1/F2 or future refactors. \
saved={saved}"
        );
    }
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Agent row shaped after the real-machine state.json agent entries (subset; every key
/// exists on the live runtime state).
fn agent_row(id: &str, session_id: Option<&str>) -> Value {
    json!({
        "status": "running",
        "provider": "codex",
        "agent_id": id,
        "window": id,
        "session_id": session_id,
        "rollout_path": null,
        "captured_at": null,
        "captured_via": null,
        "attribution_confidence": null,
        "spawn_cwd": "/ws",
    })
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-a0-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// Simulate another process having landed `state` on disk (raw write, not via the
/// chokepoint, so the in-process save cache cannot mask the read-back).
fn write_disk_state(ws: &Path, state: &Value) {
    let path = runtime_state_path(ws);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(state).unwrap()).unwrap();
}

fn read_disk_state(ws: &Path) -> Value {
    let text = std::fs::read_to_string(runtime_state_path(ws)).unwrap();
    serde_json::from_str(&text).unwrap()
}
