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
fn result_writes_do_not_use_insert_or_replace() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let results_rs = manifest.join("src/messaging/results.rs");
    let source = std::fs::read_to_string(results_rs).expect("read results.rs");
    assert!(
        !source.to_ascii_lowercase().contains("insert or replace into results"),
        "results writes must use insert-or-ignore / duplicate_ignored semantics, not insert-or-replace history overwrite"
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
