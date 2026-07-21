use std::path::{Path, PathBuf};

use crate::provider::types::SessionId;
use crate::provider::Provider;

use super::{CaptureSessionContext, CapturedSessionCandidate};

pub(super) fn projects_dir_for_cwd(home: &Path, spawn_cwd: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(spawn_cwd).unwrap_or_else(|_| spawn_cwd.to_path_buf());
    let encoded = encode_projects_dir(&canonical.to_string_lossy());
    if encoded.is_empty() {
        return None;
    }
    Some(home.join(".claude").join("projects").join(encoded))
}

fn encode_projects_dir(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    out
}

pub(crate) fn rollout_path_has_leader_marker(provider: Provider, rollout_path: &Path) -> bool {
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return false;
    }
    let Ok(text) = super::common::read_head_text(rollout_path, super::common::CAPTURE_HEAD_BYTES)
    else {
        return false;
    };
    let records = super::common::parse_session_records(&text);
    records_have_leader_marker(&records)
}

pub(super) fn records_have_leader_marker(records: &[serde_json::Value]) -> bool {
    records.iter().any(|record| {
        let custom_title = record
            .get("customTitle")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        let agent_name = record
            .get("agentName")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        matches!(custom_title.as_deref(), Some("claude leader"))
            || matches!(agent_name.as_deref(), Some("claude leader"))
    })
}

pub(super) fn has_cwd_field(record: &serde_json::Value) -> bool {
    super::common::record_cwd(record).is_some()
}

pub(super) fn apply_expected_session_filter(
    context: &CaptureSessionContext,
    mut out: Vec<CapturedSessionCandidate>,
) -> Result<Vec<CapturedSessionCandidate>, crate::provider::types::ProviderError> {
    if let Some(expected) = context.expected_session_id.as_ref() {
        if let Some(hit) = out
            .iter()
            .find(|candidate| session_matches(candidate, expected))
        {
            return Ok(vec![hit.clone()]);
        }
        let positive_only: Vec<CapturedSessionCandidate> = out
            .iter()
            .filter(|candidate| candidate.positive_agent_id_match || candidate.agent_path_match)
            .cloned()
            .collect();
        return Ok(positive_only);
    }
    super::common::apply_spawn_time_window_if_unique(Provider::Claude, context, &mut out);
    Ok(out)
}

fn session_matches(candidate: &CapturedSessionCandidate, expected: &SessionId) -> bool {
    candidate
        .captured
        .session_id
        .as_ref()
        .is_some_and(|session| session.as_str() == expected.as_str())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::provider::types::SessionId;
    use crate::provider::Provider;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_root(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-e6-attr-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_transcript(dir: &Path, uuid: &str, cwd: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{uuid}.jsonl"));
        let line = serde_json::json!({
            "sessionId": uuid,
            "cwd": cwd.to_string_lossy(),
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();
        path
    }

    #[test]
    fn claude_leader_marker_in_custom_title_is_detected() {
        let records = vec![serde_json::json!({
            "type": "custom-title",
            "customTitle": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_leader_marker_in_agent_name_is_detected() {
        let records = vec![serde_json::json!({
            "type": "agent-name",
            "agentName": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_worker_records_have_no_leader_marker() {
        let records = vec![
            serde_json::json!({
                "type": "custom-title",
                "customTitle": "claude release-engineer",
                "sessionId": "abc12345",
            }),
            serde_json::json!({
                "type": "user",
                "content": "Team Agent message from leader: do X",
                "sessionId": "abc12345",
            }),
        ];
        assert!(!records_have_leader_marker(&records));
    }

    #[test]
    fn claude_leader_marker_is_case_insensitive() {
        let records = vec![serde_json::json!({
            "customTitle": "Claude Leader",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_projects_dir_for_cwd_encodes_slashes_to_dashes() {
        let home = Path::new("/home/u");
        let cwd = tmp_root("encode");
        let got = projects_dir_for_cwd(home, &cwd).unwrap();
        let canon = std::fs::canonicalize(&cwd).unwrap();
        let expected_leaf = encode_projects_dir(&canon.to_string_lossy());
        assert_eq!(
            got,
            home.join(".claude").join("projects").join(expected_leaf)
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn encode_claude_projects_dir_parity_with_real_claude_naming() {
        assert_eq!(
            encode_projects_dir("/Users/alauda/code"),
            "-Users-alauda-code"
        );
        assert_eq!(
            encode_projects_dir("/Users/alauda/Documents/code/agent前沿探索/多agent协作"),
            "-Users-alauda-Documents-code-agent------agent--"
        );
        assert_eq!(
            encode_projects_dir("/Users/foo bar.baz/v1.2"),
            "-Users-foo-bar-baz-v1-2"
        );
        assert_eq!(
            encode_projects_dir("/proj/.team/runtime"),
            "-proj--team-runtime"
        );
    }

    #[test]
    fn scan_expected_session_id_hit_returns_only_that_candidate() {
        let base = tmp_root("c-hit");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        write_transcript(&proj, "22222222-2222-4222-8222-222222222222", &cwd);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new("22222222-2222-4222-8222-222222222222")),
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "22222222-2222-4222-8222-222222222222"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_spawn_time_window_disambiguates_two_siblings() {
        let base = tmp_root("b-window");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        let old = write_transcript(&proj, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &cwd);
        let new = write_transcript(&proj, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &cwd);
        let long_ago =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        filetime_set(&old, long_ago);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        let _ = new;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_no_spawned_at_keeps_both_siblings_ambiguous() {
        let base = tmp_root("b-noamb");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "cccccccc-cccc-4ccc-8ccc-cccccccccccc", &cwd);
        write_transcript(&proj, "dddddddd-dddd-4ddd-8ddd-dddddddddddd", &cwd);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(out.len() >= 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_expected_session_id_miss_refuses_to_pick_leader_sibling() {
        let base = tmp_root("strict-no-leader-fallback");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        let leader = write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        let stale = write_transcript(&proj, "22222222-2222-4222-8222-222222222222", &cwd);
        let _ = (leader, stale);
        let ctx = CaptureSessionContext {
            agent_id: "claude-worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            expected_session_id: Some(SessionId::new("99999999-9999-4999-8999-999999999999")),
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(
            out.is_empty(),
            "expected-id miss must not fall back to latest"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
