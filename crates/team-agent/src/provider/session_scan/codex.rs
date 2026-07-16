use super::{CaptureSessionContext, CapturedSessionCandidate};

pub(super) fn apply_spawned_at_filter(
    out: &mut Vec<CapturedSessionCandidate>,
    context: &CaptureSessionContext,
) {
    let Some(spawned_at) = context.spawned_at.as_deref().and_then(parse_spawned_at) else {
        return;
    };
    let grace = std::time::Duration::from_secs(5);
    let cutoff = spawned_at.checked_sub(grace).unwrap_or(spawned_at);
    out.retain(|candidate| {
        let path = match candidate.captured.rollout_path.as_ref() {
            Some(p) => p.as_path(),
            None => return false,
        };
        match codex_rollout_created_at(path) {
            Some(created_at) => created_at >= cutoff,
            None => std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .map(|mtime| mtime >= cutoff)
                .unwrap_or(false),
        }
    });
}

pub(super) fn parse_spawned_at(raw: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| std::time::SystemTime::from(dt.with_timezone(&chrono::Utc)))
}

fn codex_rollout_created_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
    created_at_from_rollout_head(path).or_else(|| created_at_from_rollout_filename(path))
}

fn created_at_from_rollout_head(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let text = super::common::read_head_text(path, super::common::CAPTURE_HEAD_BYTES).ok()?;
    super::common::parse_session_records(&text)
        .iter()
        .find_map(record_created_at)
        .and_then(|raw| parse_spawned_at(&raw))
}

fn record_created_at(record: &serde_json::Value) -> Option<String> {
    record
        .get("created_at")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            record
                .get("session_meta")
                .and_then(|v| v.get("payload"))
                .or_else(|| record.get("payload"))
                .and_then(|v| v.get("created_at"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

fn created_at_from_rollout_filename(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let name = path.file_name()?.to_str()?;
    let start = name.find("rollout-")? + "rollout-".len();
    let stamp = name.get(start..start + 19)?;
    if stamp.as_bytes().get(10).copied() != Some(b'T') {
        return None;
    }
    let raw = format!(
        "{}T{}:{}:{}+00:00",
        &stamp[0..10],
        &stamp[11..13],
        &stamp[14..16],
        &stamp[17..19]
    );
    parse_spawned_at(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spawned_at_rfc3339_roundtrips_and_rejects_junk() {
        assert!(parse_spawned_at("2026-06-10T21:40:00+00:00").is_some());
        assert!(parse_spawned_at("not-a-date").is_none());
        assert!(parse_spawned_at("").is_none());
    }
}
