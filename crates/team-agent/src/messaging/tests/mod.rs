#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
    PaneField, PaneInfo, PaneLiveness, SessionName, SetEnvOutcome, SpawnResult, TransportError,
    WindowName,
};

// ── test scaffolding ────────────────────────────────────────────────────

/// Unique throwaway workspace dir (DB / event-log writes never leak between tests).
fn tmp_ws(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("ta-rs-msg-{tag}-{n}-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn store_for(ws: &Path) -> MessageStore {
    MessageStore::open(ws).unwrap()
}

/// Minimal Transport: every method `unimplemented!()`. The fn-under-test panics
/// at its own `unimplemented!()` before reaching the transport, so this is never
/// actually driven — it exists only to satisfy `&dyn Transport` parameters.
struct NoopTransport;
impl Transport for NoopTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("noop")
    }
    fn spawn_into(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("noop")
    }
    fn inject(
        &self,
        _t: &Target,
        _p: &InjectPayload,
        _s: Key,
        _b: bool,
    ) -> Result<InjectReport, TransportError> {
        unimplemented!("noop")
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        unimplemented!("noop")
    }
    fn capture(&self, _t: &Target, _r: CaptureRange) -> Result<CapturedText, TransportError> {
        unimplemented!("noop")
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        unimplemented!("noop")
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        unimplemented!("noop")
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        unimplemented!("noop")
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        unimplemented!("noop")
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        unimplemented!("noop")
    }
    fn set_session_env(
        &self,
        _s: &SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        unimplemented!("noop")
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        unimplemented!("noop")
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        unimplemented!("noop")
    }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
        unimplemented!("noop")
    }
}

/// A `CommsSelftestDriver` mock that asserts the §84 / MUST-NOT-13 mechanical gate:
/// ZERO provider SDK clients on the selftest path. `provider_sdk_calls` MUST stay all-0.
struct ZeroSdkDriver {
    run_id: Option<String>,
    calls: ProviderSdkCalls,
}
impl CommsSelftestDriver for ZeroSdkDriver {
    fn run_id(&self) -> Option<String> {
        self.run_id.clone()
    }
    fn provider_sdk_calls(&self) -> ProviderSdkCalls {
        self.calls
    }
    fn receiver_binding(&self, _ws: &Path, _team: Option<&TeamKey>) -> serde_json::Value {
        serde_json::json!({
            "status": "pass",
            "verifies": "binding_consistency",
            "proof": "state_read",
            "state_read_observed": true,
            "mismatches": [],
        })
    }
}

fn json(v: serde_json::Value) -> serde_json::Value {
    v
}

// ── MessageStore seed helpers ────────────────────────────────────────────
//
// step-7 MessageStore only exposes create_message / claim_for_delivery /
// claim_leader_notification_delivery; the daemon-path fns under test also read
// result_watchers / scheduled_events / results rows that step-7 does not yet
// expose typed inserts for. These helpers seed those rows DIRECTLY via the
// same team.db the store opened (db::schema::open_db on store.db_path()),
// mirroring the on-disk columns probed from team-agent-public @ 439bef8
// (message_store/core.py:272-330, result_watchers.py:14-72, core.py:366).
// They exist ONLY so the WEAK "empty store → empty" contracts can run against
// a SEEDED fixture and assert concrete dedupe/dispatch outcomes.

use rusqlite::params as sql_params;

/// Open a fresh read/write connection onto the store's backing team.db
/// (the store keeps no live connection; every op reopens, so seeds are visible).
fn seed_conn(store: &MessageStore) -> rusqlite::Connection {
    crate::db::schema::open_db(store.db_path()).unwrap()
}

/// Seed a `result_watchers` row. `notified_message_id = Some(_)` marks the
/// watcher as ALREADY notified (Gap-32 dedupe gate: must survive a requeue and
/// block redelivery). `status` is the watcher lifecycle state
/// (`pending` / `notify_failed` / …). Returns the watcher_id.
#[allow(clippy::too_many_arguments)]
fn seed_watcher(
    store: &MessageStore,
    watcher_id: &str,
    owner_team_id: &str,
    task_id: &str,
    agent_id: &str,
    status: &str,
    result_id: Option<&str>,
    notified_message_id: Option<&str>,
) -> String {
    let conn = seed_conn(store);
    conn.execute(
        "insert into result_watchers(
            watcher_id, owner_team_id, task_id, agent_id, message_id, leader_id,
            status, created_at, completed_at, result_id, notified_message_id, error
         ) values (?1, ?2, ?3, ?4, null, 'leader', ?5, ?6, null, ?7, ?8, null)",
        sql_params![
            watcher_id,
            owner_team_id,
            task_id,
            agent_id,
            status,
            "2026-06-02T10:00:00+00:00",
            result_id,
            notified_message_id,
        ],
    )
    .unwrap();
    watcher_id.to_string()
}

/// Read back a watcher's `(status, notified_message_id)` — used to assert the
/// Gap-32 survival invariant (a notified watcher must keep its id after requeue).
fn watcher_state(store: &MessageStore, watcher_id: &str) -> (String, Option<String>) {
    let conn = seed_conn(store);
    conn.query_row(
        "select status, notified_message_id from result_watchers where watcher_id = ?1",
        sql_params![watcher_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .unwrap()
}

/// Seed a `scheduled_events` row of the given [`ScheduledKind`], DUE in the past
/// (`status='pending'`, `due_at` < now) so `due_scheduled_events` selects it.
/// Returns the autoincrement event id (the value `fire_due_scheduled_events`
/// appends to its fired list). scheduler.py:41-121 fires each due kind exhaustively.
fn seed_scheduled_event(
    store: &MessageStore,
    kind: ScheduledKind,
    target: &str,
    payload: &serde_json::Value,
) -> i64 {
    let kind_wire = serde_json::to_value(kind).unwrap();
    let kind_str = kind_wire.as_str().unwrap();
    let conn = seed_conn(store);
    conn.execute(
        "insert into scheduled_events(
            owner_team_id, due_at, target, kind, payload_json, status, created_at
         ) values (null, ?1, ?2, ?3, ?4, 'pending', ?5)",
        sql_params![
            "2000-01-01T00:00:00+00:00", // far past → always due
            target,
            kind_str,
            payload.to_string(),
            "2000-01-01T00:00:00+00:00",
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// Seed a `results` row (an uncollected result envelope) so `retry_result_deliveries`
/// can resolve a watcher's `result_id` via `result_by_id`. Returns the result_id.
fn seed_result(
    store: &MessageStore,
    result_id: &str,
    task_id: &str,
    agent_id: &str,
    status: &str,
) -> String {
    let envelope = serde_json::json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": task_id,
        "agent_id": agent_id,
        "status": status,
        "summary": "done",
        "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
    });
    let conn = seed_conn(store);
    conn.execute(
        "insert into results(
            result_id, owner_team_id, task_id, agent_id, envelope, status, created_at
         ) values (?1, null, ?2, ?3, ?4, ?5, ?6)",
        sql_params![
            result_id,
            task_id,
            agent_id,
            envelope.to_string(),
            status,
            "2026-06-02T10:00:00+00:00",
        ],
    )
    .unwrap();
    result_id.to_string()
}

static ENV_LOCK_MSG: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct EnvGuardMsg {
    key: String,
    prev: Option<String>,
}
impl EnvGuardMsg {
    fn set(key: &str, val: Option<&str>) -> Self {
        let prev = std::env::var(key).ok();
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        Self {
            key: key.to_string(),
            prev,
        }
    }
}
impl Drop for EnvGuardMsg {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

mod basic;
mod e23;
mod leader_inject_acceptance;
mod main_preserved;
mod runtime;
mod spine;
mod wave2;
