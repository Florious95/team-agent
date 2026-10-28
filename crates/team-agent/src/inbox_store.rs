//! An append-only inbound log, independent of delivery queues and coordinator state.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

fn path(workspace: &Path, recipient: &str) -> Result<PathBuf, String> {
    if recipient.is_empty()
        || !recipient
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(recipient, "." | "..")
    {
        return Err("invalid inbox recipient".into());
    }
    Ok(workspace
        .join(".team/inbox")
        .join(format!("{recipient}.jsonl")))
}

pub fn append(workspace: &Path, recipient: &str, record: &Value) -> Result<(), String> {
    let path = path(workspace, recipient)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut line = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    // Serialize full records across concurrent MCP processes, including partial write retries.
    // This lock belongs only to this inbox file, never a delivery or runtime-state lock.
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    file.write_all(&line).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())
}

pub fn record(
    sender: &str,
    recipient: &str,
    content: &str,
    team: Option<&str>,
    kind: &str,
) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "inbox-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    json!({"message_id": id,
        "created_at": chrono::Utc::now().to_rfc3339(), "sender": sender, "recipient": recipient,
        "content": content, "team": team, "kind": kind})
}

/// No acknowledgement, compaction, state transition, or filesystem creation on read.
pub fn read(
    workspace: &Path,
    recipient: &str,
    limit: usize,
    team: Option<&str>,
) -> Result<Value, String> {
    let path = path(workspace, recipient)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"recipient": recipient, "messages": []}))
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut messages = VecDeque::new();
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut line_number = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break;
        }
        line_number += 1;
        // A concurrent appender may not have completed its last record yet.
        if !line.ends_with('\n') {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .map_err(|e| format!("{}:{line_number}: {e}", path.display()))?;
        if team.is_some_and(|team| {
            value
                .get("team")
                .and_then(Value::as_str)
                .is_some_and(|record_team| record_team != team)
        }) {
            continue;
        }
        if limit > 0 {
            if messages.len() == limit {
                messages.pop_front();
            }
            messages.push_back(value);
        }
    }
    Ok(json!({"recipient": recipient, "messages": messages}))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TempWorkspace(PathBuf);
    impl TempWorkspace {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{label}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_missing_inbox_is_side_effect_free() {
        let ws = TempWorkspace::new("direct-inbox-missing");
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            json!([])
        );
        assert!(!ws.path().join(".team/inbox").exists());
        assert!(read(ws.path(), "../escape", 3, None).is_err());
    }

    #[test]
    fn tails_records_and_does_not_consume_them() {
        let ws = TempWorkspace::new("direct-inbox-tail");
        for n in 0..5 {
            append(ws.path(), "leader", &json!({"content": n})).unwrap();
        }
        let expected = json!([{"content": 2}, {"content": 3}, {"content": 4}]);
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            expected
        );
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            expected
        );
    }
}
