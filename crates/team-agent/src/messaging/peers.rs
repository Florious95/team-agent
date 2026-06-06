//! peer messaging compatibility helpers.

use std::path::Path;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;

use super::MessagingError;

/// `leader.py:allow_peer_talk`: persist a bidirectional peer allowlist entry.
pub fn allow_peer_talk(
    workspace: &Path,
    a: &str,
    b: &str,
) -> Result<serde_json::Value, MessagingError> {
    let store = MessageStore::open(workspace)?;
    store.allow_peer(a, b)?;
    EventLog::new(workspace).write(
        "communication.peer_allowed",
        serde_json::json!({
            "a": a,
            "b": b,
        }),
    )?;
    Ok(serde_json::json!({
        "ok": true,
        "a": a,
        "b": b,
        "status": "compat_noop",
        "reason": "team_scoped_peer_messages_enabled",
    }))
}
