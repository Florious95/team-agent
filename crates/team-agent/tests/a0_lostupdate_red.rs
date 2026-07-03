//! A0 lost-update Phase C contracts: stale topology conflicts + capture tuple backfill.
//!
//! Locate doc (sole basis): `.team/artifacts/a0-rs-lostupdate-locate.md` (fable-architect).
//! Python 0.2.11 truth: coordinator tick does whole-file load->mutate->save with NO merge
//! (state.py:493), so a concurrent add-agent registration is permanently overwritten.
//!
//! Phase C design §Behavior Preservation flips the old full-row resurrection contract:
//! stale non-lifecycle saves racing live topology must fail with SaveConflict and leave
//! latest topology intact, not clone rows to mask the race. Non-topology merge exceptions
//! such as complete session-capture tuple backfill remain valid.
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
use team_agent::state::StateError;

/// Phase C rewrite of RED-R1: when another process flipped `active_team_key` and
/// registered live topology in `teams.team-b` between this writer's load and save,
/// the stale save must be rejected and latest topology must remain intact.
#[test]
fn stale_team_projection_save_conflicts_and_preserves_latest_topology() {
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
                "team-b": { "session_name": "team-b", "agents": { "new_agent": live_agent_row("new_agent") } },
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

    let err = save_runtime_state(&ws, &incoming).expect_err("stale live topology must conflict");
    assert!(matches!(err, StateError::SaveConflict(_)));
    let message = err.to_string();
    assert!(message.contains("agent_id=new_agent"), "message={message}");
    assert!(
        message.contains("projection=teams.team-b.agents"),
        "message={message}"
    );

    let saved = read_disk_state(&ws);
    assert!(
        saved
            .pointer("/teams/team-b/agents/new_agent")
            .is_some_and(Value::is_object),
        "Phase C design §Behavior Preservation: stale saves must not corrupt active \
topology; saved={saved}"
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
    let row = saved.pointer("/agents/w1").cloned().unwrap_or(Value::Null);
    let mut failures = Vec::new();
    for (field, expected) in [
        ("session_id", json!("sess-cap-1")),
        (
            "rollout_path",
            json!("/home/user/.codex/sessions/rollout-sess-cap-1.jsonl"),
        ),
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

/// Phase C rewrite of the A0 GREEN lock: old tests asserted full-row resurrection. The
/// new contract is stricter around topology: a stale snapshot missing live topology is
/// rejected, and the disk latest remains the source of truth.
#[test]
fn stale_snapshot_missing_live_agent_conflicts_without_corrupting_disk() {
    let ws = tmp_ws("a0-green");
    write_disk_state(
        &ws,
        &json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": agent_row("w1", Some("sess-w1")),
                "joined": live_agent_row("joined"),
            },
            "teams": {
                "team-a": {
                    "session_name": "team-a",
                    "agents": {
                        "w1": agent_row("w1", Some("sess-w1")),
                        "joined": live_agent_row("joined"),
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

    let err = save_runtime_state(&ws, &incoming).expect_err("missing live row must conflict");
    assert!(matches!(err, StateError::SaveConflict(_)));
    assert!(err.to_string().contains("agent_id=joined"));

    let saved = read_disk_state(&ws);
    for pointer in ["/agents/joined", "/teams/team-a/agents/joined"] {
        assert!(
            saved.pointer(pointer).is_some_and(Value::is_object),
            "Phase C design §Behavior Preservation: latest active topology must stay \
intact at {pointer}; saved={saved}"
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

fn live_agent_row(id: &str) -> Value {
    json!({
        "status": "running",
        "provider": "codex",
        "agent_id": id,
        "window": id,
        "pane_id": format!("%{id}"),
        "pane_pid": 1000,
        "spawned_at": "2026-06-01T00:00:00Z",
        "spawn_epoch": 1,
        "session_id": null,
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
