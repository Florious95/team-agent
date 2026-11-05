//! Canonical team identity shared by current leader operations.

use serde_json::Value;

use std::path::Path;

pub(crate) fn active_team_key(workspace: &Path, state: &Value) -> String {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            workspace
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "current".to_string())
}
