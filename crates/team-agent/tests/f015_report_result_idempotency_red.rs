//! F015 / BUG-RS-N21-1 contract: `report_result` is idempotent by `result_id`.
//!
//! Duplicate result submissions must be ignored, not overwritten. The durable
//! `results` row is historical evidence; replacing it can erase what the worker
//! first reported and can re-drive downstream state/notification paths.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::json;
use team_agent::messaging::report_result;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn duplicate_report_result_preserves_first_result_row() {
    let ws = temp_workspace("f015_preserve");
    let first = envelope("res_f015_dup_preserve", "success", "first summary");
    let second = envelope(
        "res_f015_dup_preserve",
        "failed",
        "second summary must not overwrite first",
    );

    report_result(&ws, &first).expect("first report_result accepted");
    report_result(&ws, &second).expect("duplicate report_result returns a typed duplicate outcome");

    let conn = open_runtime_db(&ws);
    let count: i64 = conn
        .query_row(
            "select count(*) from results where result_id = ?1",
            params!["res_f015_dup_preserve"],
            |row| row.get(0),
        )
        .expect("count result rows");
    assert_eq!(count, 1, "duplicate result_id must not create extra rows");

    let (status, raw_envelope): (String, String) = conn
        .query_row(
            "select status, envelope from results where result_id = ?1",
            params!["res_f015_dup_preserve"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read stored result");
    let stored: serde_json::Value =
        serde_json::from_str(&raw_envelope).expect("stored envelope json");
    assert_eq!(
        status, "success",
        "duplicate report_result must preserve the first status, not replace it"
    );
    assert_eq!(
        stored.get("summary").and_then(serde_json::Value::as_str),
        Some("first summary"),
        "duplicate report_result must preserve the first envelope"
    );
}

#[test]
fn duplicate_report_result_is_duplicate_ignored_and_does_not_requeue_notification() {
    // F015 / #230 N31/N32 funnel (cr-approved I-3):
    //
    // [OLD assertion] First report_result queued a `scheduled_events(kind='send',
    // target='leader')` row; duplicate must NOT queue a second → `count == 1` was the
    // dedup signal.
    //
    // [NEW assertion] Post-funnel there's no parallel queued path at all. report_result
    // now synchronously routes through the leader-delivery primitive; the duplicate
    // signal moves to: (a) zero scheduled_events rows ever (the primitive doesn't write
    // them), (b) second call returns `status="duplicate_ignored"`, (c) events.jsonl
    // contains `mcp.report_result_duplicate_ignored`. The N31/N32 invariant — "duplicate
    // result_id triggers no second leader notification" — still holds, just enforced by
    // the `leader_notification_log` PK dedup inside the primitive (and the SQLite
    // `insert or ignore into results` on first hit).
    let ws = temp_workspace("f015_duplicate_ignored");
    let result_id = "res_f015_dup_notify";

    let first_out =
        report_result(&ws, &envelope(result_id, "success", "first")).expect("first report_result");
    let second_out = report_result(&ws, &envelope(result_id, "success", "duplicate"))
        .expect("duplicate report_result");
    assert_eq!(
        first_out.get("ok").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        second_out.get("ok").and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let conn = open_runtime_db(&ws);
    let queued: i64 = conn
        .query_row("select count(*) from scheduled_events", [], |row| {
            row.get(0)
        })
        .expect("scheduled notification count");
    assert_eq!(
        queued, 0,
        "N31/N32 funnel: report_result must NOT insert any scheduled_events row — the primitive is the single funnel"
    );

    assert_eq!(
        second_out.get("status").and_then(serde_json::Value::as_str),
        Some("duplicate_ignored"),
        "duplicate report_result must return an explicit duplicate_ignored outcome"
    );
    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).expect("events.jsonl");
    assert!(
        events.contains("\"event\": \"mcp.report_result_duplicate_ignored\""),
        "duplicate path must be observable in events.jsonl; got {events}"
    );
}

#[test]
fn result_writes_do_not_use_insert_or_replace() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let results_rs = manifest.join("src/messaging/results.rs");
    let source = std::fs::read_to_string(results_rs).expect("read results.rs");
    assert!(
        !source.to_ascii_lowercase().contains("insert or replace into results"),
        "results writes must use insert-or-ignore / duplicate_ignored semantics, not insert-or-replace history overwrite"
    );
}

#[test]
fn coordinator_tick_iteration_count_is_observable() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tick_rs = manifest.join("src/coordinator/tick.rs");
    let source = std::fs::read_to_string(tick_rs).expect("read coordinator tick source");
    assert!(
        source.contains("coordinator_tick_iteration_count"),
        "N21 requires an observable coordinator_tick_iteration_count increment so a non-exiting coordinator proves progress"
    );
}

fn envelope(result_id: &str, status: &str, summary: &str) -> serde_json::Value {
    json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": "task_f015",
        "agent_id": "worker_f015",
        "status": status,
        "summary": summary,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    })
}

fn temp_workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ta-rs-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create temp workspace");
    path
}

fn open_runtime_db(workspace: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(workspace.join(".team/runtime/team.db"))
        .expect("open runtime team.db")
}
