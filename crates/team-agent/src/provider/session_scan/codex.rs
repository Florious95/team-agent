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
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .map(|mtime| mtime >= cutoff)
            .unwrap_or(false)
    });
}

pub(super) fn parse_spawned_at(raw: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| std::time::SystemTime::from(dt.with_timezone(&chrono::Utc)))
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
