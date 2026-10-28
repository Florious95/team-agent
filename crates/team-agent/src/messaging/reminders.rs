//! Durable leader reminders; the existing coordinator is the only timer process.
use std::path::Path;

use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::MessagingError;
use crate::{event_log::EventLog, message_store::MessageStore};

#[derive(Serialize, Deserialize)]
struct Reminder {
    text: String,
    trigger: String,
    interval_ms: Option<i64>,
}

pub(crate) fn parse_duration(raw: &str) -> Result<Duration, MessagingError> {
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (amount, unit) = raw.split_at(split);
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => 0,
    };
    amount.parse::<i64>().ok()
        .and_then(|amount| amount.checked_mul(multiplier))
        .filter(|ms| *ms > 0)
        .and_then(Duration::try_milliseconds)
        .filter(|duration| Utc::now().checked_add_signed(*duration).is_some())
        .ok_or_else(|| MessagingError::Validation("duration must be a positive integer followed by ms, s, m, h or d, within the supported date range".into()))
}

pub(crate) fn parse_id(id: &str) -> Option<i64> {
    let hex = id.strip_prefix("sched_")?;
    if hex.is_empty() || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    i64::from_str_radix(hex, 16).ok().filter(|id| *id > 0)
}

fn view(id: i64, reminder: &Reminder) -> Value {
    json!({"id": format!("sched_{id:x}"), "trigger": reminder.trigger,
        "text": reminder.text, "status": "active"})
}

pub(crate) fn add(
    workspace: &Path,
    owner: &str,
    text: &str,
    duration: &str,
    recurring: bool,
) -> Result<Value, MessagingError> {
    let interval = parse_duration(duration)?;
    let now = Utc::now();
    let due = now.checked_add_signed(interval).ok_or_else(|| {
        MessagingError::Validation("duration exceeds supported date range".into())
    })?;
    let reminder = Reminder {
        text: text.to_owned(),
        trigger: format!("{} {duration}", if recurring { "every" } else { "in" }),
        interval_ms: recurring.then_some(interval.num_milliseconds()),
    };
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "insert into scheduled_events (due_at, target, kind, payload_json, status, created_at, owner_team_id) values (?1, 'leader', 'reminder', ?2, 'pending', ?3, ?4)",
        params![due.to_rfc3339_opts(SecondsFormat::Nanos, false), serde_json::to_string(&reminder)?, now.to_rfc3339(), owner],
    )?;
    let mut value = view(conn.last_insert_rowid(), &reminder);
    value["ok"] = json!(true);
    Ok(value)
}

pub(crate) fn list(workspace: &Path, owner: &str) -> Result<Vec<Value>, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut statement = conn.prepare("select id, payload_json from scheduled_events where kind='reminder' and owner_team_id=?1 and status='pending' order by id")?;
    let rows = statement.query_map([owner], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (id, payload) = row?;
        Ok(view(id, &serde_json::from_str(&payload)?))
    })
    .collect()
}

pub(crate) fn cancel(workspace: &Path, owner: &str, id: i64) -> Result<bool, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    // An occurrence already fired is not recalled.
    Ok(conn.execute("update scheduled_events set status='cancelled' where id=?1 and kind='reminder' and owner_team_id=?2 and status='pending'", params![id, owner])? == 1)
}

pub(super) fn fire(
    workspace: &Path,
    id: i64,
    _event_log: &EventLog,
) -> Result<Value, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let mut conn = crate::db::schema::open_db(store.db_path())?;
    // Serialize occurrences/cancellation, never wait for a delivery receipt.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let row = tx.query_row(
        "select due_at, payload_json, owner_team_id from scheduled_events where id=?1 and kind='reminder' and status='pending'",
        [id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
    ).optional()?;
    let Some((due_at, payload, owner)) = row else {
        return Ok(json!({"ok": true, "cancelled": true}));
    };
    let reminder: Reminder = serde_json::from_str(&payload)?;
    let directory = crate::direct_delivery::directory(workspace, Some(&owner))
        .map_err(MessagingError::Routing)?;
    // The inbox is durable independently of physical delivery. A stable id also
    // makes replay after an append/before timer commit safe without ledger rows.
    let message_id = format!("reminder_{id}_{due_at}");
    let inbox = crate::inbox_store::read(workspace, "leader", usize::MAX, Some(&owner))
        .map_err(std::io::Error::other)?;
    if !inbox["messages"].as_array().into_iter().flatten()
        .any(|record| record["message_id"] == message_id)
    {
        let mut record = crate::inbox_store::record("scheduler", "leader", &reminder.text, Some(&owner), "reminder");
        record["message_id"] = json!(message_id);
        crate::inbox_store::append(workspace, "leader", &record)
            .map_err(std::io::Error::other)?;
    }
    let next_due = reminder
        .interval_ms
        .and_then(Duration::try_milliseconds)
        .filter(|duration| *duration > Duration::zero())
        .and_then(|interval| Utc::now().checked_add_signed(interval))
        .map(|due| due.to_rfc3339_opts(SecondsFormat::Nanos, false));
    let mut result = json!({"ok": true, "message_id": message_id, "next_due_at": next_due, "delivery": "inbox"});
    // Compare due_at as well as status: concurrent ticks or cancellation cannot
    // advance the same occurrence twice, or resurrect a cancelled reminder.
    tx.execute(
        "update scheduled_events set status=?1, due_at=coalesce(?2, due_at), fired_at=?3, result_json=?4 where id=?5 and status='pending' and due_at=?6",
        params![if next_due.is_some() { "pending" } else { "done" }, next_due, Utc::now().to_rfc3339(), result.to_string(), id, due_at],
    )?;
    tx.commit()?;
    // Once consumed, physical delivery is best effort: a crash or detached pane
    // still leaves the reminder in the inbox, never awaiting a legacy receipt.
    match directory.send("leader", &format!("Team Agent reminder:\n\n{}", reminder.text)) {
        Ok(_) => result["delivery"] = json!("physical"),
        Err(error) => result["delivery_error"] = json!(error),
    }
    conn.execute(
        "update scheduled_events set result_json=?1 where id=?2 and json_extract(result_json, '$.message_id')=?3",
        params![result.to_string(), id, message_id],
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    struct TestWorkspace(std::path::PathBuf);
    impl TestWorkspace {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn workspace() -> TestWorkspace {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = TestWorkspace(std::env::temp_dir().join(format!(
            "ta-reminders-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )));
        let state = json!({"active_team_key": "alpha", "team_key": "alpha", "teams": {
            "alpha": {"team_key": "alpha", "agents": {}},
            "beta": {"team_key": "beta", "agents": {}}
        }});
        let path = crate::state::persist::runtime_state_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, state.to_string()).unwrap();
        dir
    }

    #[test]
    fn durations_and_ids_are_strict_and_bounded() {
        for (raw, milliseconds) in [
            ("1ms", 1),
            ("2s", 2000),
            ("3m", 180000),
            ("1h", 3600000),
            ("1d", 86400000),
        ] {
            assert_eq!(
                parse_duration(raw).unwrap().num_milliseconds(),
                milliseconds
            );
        }
        for raw in [
            "",
            "0s",
            "-1s",
            "1",
            "1.5s",
            "NaNs",
            "1w",
            "1s2m",
            "999999999999999999999d",
            "9223372036854775807ms",
        ] {
            assert!(parse_duration(raw).is_err(), "{raw}");
        }
        assert_eq!(parse_id("sched_1a"), Some(26));
        for raw in [
            "1",
            "sched_",
            "sched_-1",
            "sched_+1",
            "sched_0",
            "sched_ffffffffffffffff",
        ] {
            assert_eq!(parse_id(raw), None, "{raw}");
        }
    }

    #[test]
    fn list_and_cancel_are_persistent_and_team_scoped() {
        let dir = workspace();
        let added = add(dir.path(), "alpha", "check status", "1h", true).unwrap();
        assert_eq!(added["trigger"], "every 1h");
        assert_eq!(added["status"], "active");
        let id = parse_id(added["id"].as_str().unwrap()).unwrap();
        assert_eq!(list(dir.path(), "alpha").unwrap().len(), 1);
        assert!(list(dir.path(), "beta").unwrap().is_empty());
        assert!(!cancel(dir.path(), "beta", id).unwrap());
        assert!(cancel(dir.path(), "alpha", id).unwrap());
        assert!(!cancel(dir.path(), "alpha", id).unwrap());
        assert!(list(dir.path(), "alpha").unwrap().is_empty());
        let log = EventLog::new(dir.path());
        assert_eq!(fire(dir.path(), id, &log).unwrap()["cancelled"], true);
    }

    #[test]
    fn one_shot_stores_once_and_uses_owner_not_active_team() {
        let dir = workspace();
        let added = add(dir.path(), "beta", "beta reminder", "1ms", false).unwrap();
        let id = parse_id(added["id"].as_str().unwrap()).unwrap();
        let log = EventLog::new(dir.path());
        let result = fire(dir.path(), id, &log).unwrap();
        assert_eq!(result["delivery"], "inbox");
        assert!(result["delivery_error"].is_string());
        fire(dir.path(), id, &log).unwrap();
        let inbox = crate::inbox_store::read(dir.path(), "leader", usize::MAX, None).unwrap();
        let records = inbox["messages"].as_array().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["team"], "beta");
        assert_eq!(records[0]["recipient"], "leader");
        assert_eq!(records[0]["kind"], "reminder");
        assert_eq!(records[0]["content"], "beta reminder");
        let store = MessageStore::open(dir.path()).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        assert_eq!(conn.query_row("select count(*) from messages", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert!(list(dir.path(), "beta").unwrap().is_empty());
    }

    #[test]
    fn recurring_occurrences_are_unique_and_crash_replay_is_idempotent() {
        let dir = workspace();
        let added = add(dir.path(), "alpha", "periodic", "1s", true).unwrap();
        let id = parse_id(added["id"].as_str().unwrap()).unwrap();
        let log = EventLog::new(dir.path());
        let store = MessageStore::open(dir.path()).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        let original_due: String = conn.query_row(
            "select due_at from scheduled_events where id=?1", [id], |row| row.get(0),
        ).unwrap();
        let count = || crate::inbox_store::read(dir.path(), "leader", usize::MAX, None).unwrap()["messages"].as_array().unwrap().len();
        fire(dir.path(), id, &log).unwrap();
        assert_eq!(list(dir.path(), "alpha").unwrap().len(), 1);
        // Simulate the crash window after inbox append, before timer commit.
        conn.execute("update scheduled_events set due_at=?1 where id=?2", params![original_due, id]).unwrap();
        fire(dir.path(), id, &log).unwrap();
        assert_eq!(count(), 1);
        fire(dir.path(), id, &log).unwrap();
        assert_eq!(count(), 2);
        assert!(cancel(dir.path(), "alpha", id).unwrap());
        fire(dir.path(), id, &log).unwrap();
        assert_eq!(count(), 2);
        assert_eq!(conn.query_row("select count(*) from messages", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn concurrent_fires_consume_one_occurrence_and_one_inbox_record() {
        let dir = workspace();
        let added = add(dir.path(), "alpha", "once", "1ms", false).unwrap();
        let id = parse_id(added["id"].as_str().unwrap()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2).map(|_| scope.spawn(|| {
                barrier.wait();
                fire(dir.path(), id, &EventLog::new(dir.path())).unwrap()
            })).collect();
            let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|result| result["cancelled"] == true).count(), 1);
        });
        let inbox = crate::inbox_store::read(dir.path(), "leader", usize::MAX, None).unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    }
}
