//! MED A-batch (1/3): data-panel diff contracts — slices A-1 / A-3 / A-4 / A-5.
//!
//! Triage doc (sole basis): `.team/artifacts/med-triage-fixed-failure-sweep.md`.
//! Python truth source: 0.2.11 (`~/.team-agent/runtime/0.2.11/src/team_agent/`).
//!
//! A-1 collect/coordinator severed wiring     — results.rs:43 `let _ = ensure_coordinator`
//!     + constant `{ok:false,status:"not_required"}` vs Python results.py:157,176+
//!     `_ensure_coordinator_after_collect` (real start_coordinator result when state has
//!     a session_name; the literal not_required ONLY when ensure=False or no session).
//! A-3 watch ignores team + store              — health.rs:493 `let _ = (store, team)` vs
//!     Python watch/__init__.py:41-42 (event team filter + `_collect_result_lines` from
//!     store.latest_results(owner_team_id=team)).
//! A-4 status panel severed                    — status_port.rs:66 `latest_results: []`
//!     constant vs Python queries.py:76; :20 `let _ = detail` vs Python commands.py:99
//!     (`--json --detail` => compact=False, i.e. detail returns the FULL payload);
//!     format_status agent branch returns a bool string vs Python queries.py:130-160
//!     (multi-line agent detail; unknown agent raises).
//! A-5 missing leader_receiver = ready false positive — cli/diagnose.rs:395-407
//!     `is_none_or(...)` reports attached=true when leader_receiver is absent; fixed
//!     expectation (user truthfulness rule + triage adjudication): unreadable/missing
//!     receiver must NOT count as attached.
//!
//! Note on two audited A-5 sub-items NOT contracted here (verified equivalent at 23d8a8b):
//! - doctor_comms_json drops `gate`: Python diagnose/comms.py:30 does `del gate` itself.
//! - doctor --fix/--confirm: RS cli/adapters.rs:1489-1502 wires fix/confirm through
//!   orphan_gate_json exactly like Python cli/commands.py:218-236 (incl. "--fix requires
//!   --gate"); the dropped pair in diagnose/mod.rs doctor_gate_blockers is a packaging
//!   seam not on the user doctor path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::coordinator::{collect_watch_lines, WatchCursor, WorkspacePath};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::save_runtime_state;

/// A-1: with a live coordinator and a session-bearing state, `collect(.., ensure=true)`
/// must report the REAL ensure outcome (Python: start_coordinator -> already-running =>
/// ok:true), not the constant `{ok:false,status:"not_required"}`.
#[test]
fn a1_collect_ensure_coordinator_reports_real_status() {
    let ws = tmp_ws("a1-ensure");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    save_runtime_state(&ws, &json!({"session_name": "team-x"})).unwrap();
    seed_healthy_coordinator(&ws);

    let out =
        team_agent::messaging::results::collect(&ws, None, true).expect("collect should succeed");
    let coordinator = out.get("coordinator").cloned().unwrap_or(Value::Null);
    assert_ne!(
        coordinator,
        json!({"ok": false, "status": "not_required"}),
        "A-1: ensure_coordinator=true with a session-bearing state must run the real \
ensure step (Python results.py:157 -> _ensure_coordinator_after_collect), not return \
the not_required constant; out={out}"
    );
    assert_eq!(
        coordinator.get("ok"),
        Some(&json!(true)),
        "A-1: with a healthy coordinator the ensure result must report ok:true \
(Python start_coordinator no-op on healthy); coordinator={coordinator}"
    );
}

/// A-1 (Python literal lock, green today): ensure_coordinator=false returns exactly
/// `{ok:false,status:"not_required"}` (results.py:157 right-hand branch).
#[test]
fn a1_collect_without_ensure_returns_not_required_literal() {
    let ws = tmp_ws("a1-noensure");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    save_runtime_state(&ws, &json!({"session_name": "team-x"})).unwrap();

    let out =
        team_agent::messaging::results::collect(&ws, None, false).expect("collect should succeed");
    assert_eq!(
        out.get("coordinator"),
        Some(&json!({"ok": false, "status": "not_required"})),
        "A-1 literal lock: ensure_coordinator=false must return the Python literal; out={out}"
    );
}

/// A-3: collect_watch_lines must (a) filter events by team (Python watch/__init__.py:91
/// `_event_team_id(event) != team -> continue`) and (b) emit result lines from
/// store.latest_results scoped to the team (:100-111, `result_received: {agent} -> {summary}`).
#[test]
fn a3_watch_filters_by_team_and_emits_result_lines() {
    let ws = tmp_ws("a3-watch");
    let logs = ws.join(".team/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(
        logs.join("events.jsonl"),
        concat!(
            "{\"event\":\"send.failed\",\"recipient\":\"wa\",\"reason\":\"boom-a\",\"team_id\":\"team-a\"}\n",
            "{\"event\":\"send.failed\",\"recipient\":\"wb\",\"reason\":\"boom-b\",\"team_id\":\"team-b\"}\n",
        ),
    )
    .unwrap();
    let store = MessageStore::open(&ws).unwrap();
    insert_result(&store, "res-a", "team-a", "wa", "done A");

    let mut cursor = WatchCursor::default();
    let lines = collect_watch_lines(
        &WorkspacePath::new(ws.clone()),
        &mut cursor,
        &store,
        Some("team-a"),
    )
    .expect("collect_watch_lines should succeed");

    let mut failures = Vec::new();
    if !lines
        .iter()
        .any(|line| line.contains("wa") && line.contains("boom-a"))
    {
        failures.push("A-3: team-a event line missing".to_string());
    }
    if lines
        .iter()
        .any(|line| line.contains("wb") || line.contains("boom-b"))
    {
        failures.push(
            "A-3: team-b event leaked into team-a watch (Python filters by _event_team_id, watch/__init__.py:91)"
                .to_string(),
        );
    }
    if !lines
        .iter()
        .any(|line| line.starts_with("result_received: wa") && line.contains("done A"))
    {
        failures.push(
            "A-3: result line missing — Python appends `result_received: {agent} -> {summary}` \
from store.latest_results(owner_team_id=team) (watch/__init__.py:100-116)"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "A-3 watch contract failed:\n{}\nlines={lines:?}",
        failures.join("\n")
    );
}

/// A-4: status latest_results must reflect the store (Python queries.py:76
/// latest_result_summaries), not a constant empty array.
#[test]
fn a4_status_latest_results_reflects_store() {
    let ws = tmp_ws("a4-latest");
    seed_status_state(&ws);
    let store = MessageStore::open(&ws).unwrap();
    insert_result(&store, "res-1", "team-x", "w1", "did the thing");

    let status =
        team_agent::cli::status_port::status(&ws, false, false).expect("status should succeed");
    let latest = status
        .get("latest_results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !latest.is_empty() && latest[0].get("result_id") == Some(&json!("res-1")),
        "A-4: status.latest_results must surface store.latest_results (Python \
queries.py:76); got latest_results={latest:?}"
    );
}

/// A-4: `format_status(workspace, Some(agent))` must return the Python multi-line agent
/// detail (provider/model/session_id/... lines, queries.py:142-153), and unknown agent
/// ids must error (queries.py:136-137) — not `agent w1: true`.
#[test]
fn a4_format_status_agent_branch_returns_agent_detail() {
    let ws = tmp_ws("a4-format");
    seed_status_state(&ws);

    let text = team_agent::cli::status_port::format_status(&ws, Some("w1"))
        .expect("format_status for a known agent should succeed");
    let mut failures = Vec::new();
    for marker in ["provider:", "model:", "session_id:"] {
        if !text.contains(marker) {
            failures.push(format!(
                "A-4: agent status text missing `{marker}` line (Python queries.py:142-153)"
            ));
        }
    }
    if team_agent::cli::status_port::format_status(&ws, Some("ghost-agent")).is_ok() {
        failures.push(
            "A-4: unknown agent id must error (Python queries.py:136-137 raises \
`unknown agent id`), not render a fabricated line"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "A-4 format_status agent contract failed:\n{}\ntext={text:?}",
        failures.join("\n")
    );
}

/// A-4: detail=true must return the FULL payload (Python commands.py:99 maps
/// `--json --detail` to compact=False), so full agent rows survive even when the
/// caller passed compact=true.
#[test]
fn a4_status_detail_returns_full_payload() {
    let ws = tmp_ws("a4-detail");
    seed_status_state(&ws);

    let status =
        team_agent::cli::status_port::status(&ws, true, true).expect("status should succeed");
    assert!(
        status
            .pointer("/agents/w1/spawn_cwd")
            .and_then(Value::as_str)
            .is_some(),
        "A-4: detail=true must yield the full (non-compacted) payload — Python \
`status --json --detail` => compact=False keeps raw agent fields like spawn_cwd; \
agents={:?}",
        status.get("agents")
    );
}

/// A-5: a state WITHOUT leader_receiver must not report all_attached_receiver=true.
/// Today cli/diagnose.rs:395-407 `is_none_or(..)` turns "missing/unreadable" into
/// "attached" — a doctor/wait-ready false positive. Fixed expectation per triage:
/// unreadable means NOT attached.
#[test]
fn a5_missing_leader_receiver_must_not_report_attached() {
    let ws = tmp_ws("a5-receiver");
    seed_status_state(&ws); // note: seeds NO leader_receiver key

    let status =
        team_agent::cli::status_port::status(&ws, false, false).expect("status should succeed");
    assert_eq!(
        status.get("all_attached_receiver"),
        Some(&json!(false)),
        "A-5: missing leader_receiver must read as NOT attached (truthfulness rule: \
unreadable is never ready); status={status}"
    );
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn seed_status_state(ws: &Path) {
    save_runtime_state(
        ws,
        &json!({
            "session_name": "team-x",
            "active_team_key": "team-x",
            "agents": {
                "w1": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "w1",
                    "window": "w1",
                    "model": "gpt-5.5",
                    "profile": null,
                    "session_id": "sess-w1",
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "spawn_cwd": ws.to_string_lossy(),
                },
            },
        }),
    )
    .unwrap();
}

fn insert_result(store: &MessageStore, result_id: &str, team: &str, agent: &str, summary: &str) {
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let envelope = json!({
        "task_id": "t1",
        "agent_id": agent,
        "status": "success",
        "summary": summary,
        "artifacts": [],
    });
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, 't1', ?3, ?4, 'stored', '2026-06-10T00:00:00+00:00')",
        params![result_id, team, agent, envelope.to_string()],
    )
    .unwrap();
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(
        team_agent::coordinator::coordinator_pid_path(&workspace)
            .parent()
            .unwrap(),
    )
    .unwrap();
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-med-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
