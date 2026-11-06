//! Read-only diagnostics for canonical owner-team aliases.

use std::path::Path;

use crate::event_log::EventLog;

use super::MessagingError;

pub(crate) fn normalize_owner_team_id_rows(
    workspace: &Path,
    requested: &str,
    canonical: &str,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
) -> Result<(), MessagingError> {
    if requested == canonical {
        return Ok(());
    }
    if let Some(event_log) = event_log {
        event_log.write(
            "owner_team_id.compatibility_alias_detected",
            serde_json::json!({
                "requested_owner_team_id": requested,
                "canonical_owner_team_id": canonical,
                "message_id": message_id,
                "action": "read_only_no_db_update",
                "workspace": workspace.to_string_lossy().to_string(),
            }),
        )?;
    }
    Ok(())
}
