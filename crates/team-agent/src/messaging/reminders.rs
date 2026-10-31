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
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

pub(crate) fn list(workspace: &Path, owner: &str) -> Result<Vec<Value>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

pub(crate) fn cancel(workspace: &Path, owner: &str, id: i64) -> Result<bool, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

pub(super) fn fire(
    workspace: &Path,
    id: i64,
    _event_log: &EventLog,
) -> Result<Value, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
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




}
