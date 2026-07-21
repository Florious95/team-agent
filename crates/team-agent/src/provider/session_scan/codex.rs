pub(super) fn parse_spawned_at(raw: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| std::time::SystemTime::from(dt.with_timezone(&chrono::Utc)))
}

pub(super) fn rollout_created_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
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
    use crate::provider::session_scan::{common, CaptureSessionContext, CapturedSessionCandidate};
    use crate::provider::Provider;
    use crate::provider::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};
    use std::path::PathBuf;

    #[test]
    fn parse_spawned_at_rfc3339_roundtrips_and_rejects_junk() {
        assert!(parse_spawned_at("2026-06-10T21:40:00+00:00").is_some());
        assert!(parse_spawned_at("not-a-date").is_none());
        assert!(parse_spawned_at("").is_none());
    }

    fn candidate(path: PathBuf, session_id: &str) -> CapturedSessionCandidate {
        CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(session_id)),
                rollout_path: Some(RolloutPath::new(path)),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: PathBuf::from("/tmp"),
            },
            embedded_agent_id: None,
            positive_agent_id_match: false,
            agent_path_match: false,
        }
    }

    #[test]
    fn spawned_at_filter_rejects_prior_round_and_accepts_current_rollout() {
        let dir =
            std::env::temp_dir().join(format!("ta-codex-spawn-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stale =
            dir.join("rollout-2026-07-21T22-45-40-11111111-1111-4111-8111-111111111111.jsonl");
        let fresh =
            dir.join("rollout-2026-07-21T23-20-01-22222222-2222-4222-8222-222222222222.jsonl");
        std::fs::write(&stale, "{}\n").unwrap();
        std::fs::write(&fresh, "{}\n").unwrap();
        let context = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: dir.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2026-07-21T23:20:00+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: None,
        };
        let mut out = vec![
            candidate(stale, "11111111-1111-4111-8111-111111111111"),
            candidate(fresh.clone(), "22222222-2222-4222-8222-222222222222"),
        ];
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.rollout_path.as_ref().unwrap().as_path(),
            fresh
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawned_at_filter_fails_closed_with_an_invalid_boundary() {
        let mut out = vec![candidate(PathBuf::from("/tmp/old.jsonl"), "old")];
        let mut context = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: PathBuf::from("/tmp"),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert_eq!(out.len(), 1, "legacy direct scan has no cohort boundary");

        context.spawned_at = Some("not-a-date".to_string());
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert!(out.is_empty());
    }
}
