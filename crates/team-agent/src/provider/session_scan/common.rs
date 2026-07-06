use std::path::{Path, PathBuf};

use crate::provider::helpers::{find_session_id, parse_jsonl_records};
use crate::provider::types::{
    CaptureVia, CapturedSession, Confidence, ProviderError, RolloutPath, SessionId,
};
use crate::provider::Provider;

use super::{CaptureSessionContext, CapturedSessionCandidate};

pub(super) struct SessionCandidate {
    pub(super) path: PathBuf,
    requires_cwd_match: bool,
}

/// P2 (C-P2-2/3) / Python claude.py:300 — candidates are capped to the newest `cap`
/// by mtime (descending priority: old candidates must not crowd out new ones).
const CAPTURE_CANDIDATE_CAP: usize = 300;

/// P2 (C-P2-1): head window >= Python's 200-line read.
pub(super) const CAPTURE_HEAD_BYTES: u64 = 65_536;

pub(super) fn candidate_session_files(
    provider: Provider,
    context: &CaptureSessionContext,
) -> Result<Vec<SessionCandidate>, ProviderError> {
    let mut out = Vec::new();
    if let Some(root) = context.provider_projects_root.as_ref() {
        collect_optional_candidate_files(root, &context.agent_id, &mut out)?;
    }
    collect_candidate_files(&context.spawn_cwd, &context.agent_id, 0, false, &mut out)?;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        match provider {
            Provider::Codex => {
                collect_optional_candidate_files(
                    &home.join(".codex").join("sessions"),
                    &context.agent_id,
                    &mut out,
                )?;
            }
            Provider::Claude | Provider::ClaudeCode => {
                if let Some(dir) = super::claude::projects_dir_for_cwd(&home, &context.spawn_cwd) {
                    collect_optional_candidate_files(&dir, &context.agent_id, &mut out)?;
                }
                collect_optional_candidate_files(
                    &home.join(".claude").join("projects"),
                    &context.agent_id,
                    &mut out,
                )?;
            }
            Provider::Copilot | Provider::GeminiCli | Provider::Fake => {}
        }
    }
    out.sort_by(|a, b| {
        a.requires_cwd_match
            .cmp(&b.requires_cwd_match)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.requires_cwd_match == b.requires_cwd_match);
    cap_candidates_by_mtime(&mut out, CAPTURE_CANDIDATE_CAP);
    Ok(out)
}

pub(super) fn parse_candidate_files(
    provider: Provider,
    context: &CaptureSessionContext,
    candidates: Vec<SessionCandidate>,
) -> Vec<CapturedSessionCandidate> {
    let mut out = Vec::new();
    for candidate in candidates {
        let path = candidate.path;
        let Ok(text) = read_head_text(&path, CAPTURE_HEAD_BYTES) else {
            continue;
        };
        let records = parse_session_records(&text);
        if records.is_empty() {
            continue;
        }
        if candidate.requires_cwd_match
            && !provider_home_records_match_spawn_cwd(&records, &context.spawn_cwd)
        {
            continue;
        }
        let session_id = records.iter().find_map(find_session_id);
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && session_id.is_some()
            && !records.iter().any(super::claude::has_cwd_field)
        {
            continue;
        }
        let captured_via = if session_id.is_some() {
            CaptureVia::FsWatch
        } else {
            CaptureVia::FsMtimeFallback
        };
        let attribution_confidence = if session_id.is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        let embedded_agent_id = embedded_team_agent_worker_id_from_text(&text);
        if embedded_agent_id
            .as_deref()
            .is_some_and(|id| id != context.agent_id.as_str())
        {
            continue;
        }
        let positive_agent_id_match = candidate_text_has_team_agent_id(&text, context)
            || embedded_agent_id.as_deref() == Some(context.agent_id.as_str());
        let agent_path_match = candidate_path_matches_agent_id(&path, context);
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && super::claude::records_have_leader_marker(&records)
        {
            continue;
        }
        out.push(CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: session_id.map(SessionId::new),
                rollout_path: Some(RolloutPath::new(path)),
                captured_via,
                attribution_confidence,
                spawn_cwd: context.spawn_cwd.clone(),
            },
            embedded_agent_id,
            positive_agent_id_match,
            agent_path_match,
        });
    }
    out
}

pub(super) fn apply_spawn_time_window_if_unique(
    context: &CaptureSessionContext,
    out: &mut Vec<CapturedSessionCandidate>,
) {
    if context.expected_session_id.is_some() && out.len() <= 1 {
        return;
    }
    let Some(spawned_at) = context
        .spawned_at
        .as_deref()
        .and_then(super::codex::parse_spawned_at)
    else {
        return;
    };
    let within: Vec<CapturedSessionCandidate> = out
        .iter()
        .filter(|candidate| {
            candidate
                .captured
                .rollout_path
                .as_ref()
                .and_then(|p| {
                    std::fs::metadata(p.as_path())
                        .and_then(|m| m.modified())
                        .ok()
                })
                .is_some_and(|mtime| mtime >= spawned_at)
        })
        .cloned()
        .collect();
    if within.len() == 1 {
        *out = within;
    }
}

pub(super) fn sort_expected_first_if_needed(
    context: &CaptureSessionContext,
    out: &mut [CapturedSessionCandidate],
) {
    if let Some(expected) = context.expected_session_id.as_ref() {
        out.sort_by_key(|candidate| {
            candidate
                .captured
                .session_id
                .as_ref()
                .is_none_or(|session| session.as_str() != expected.as_str())
        });
    }
}

fn cap_candidates_by_mtime(out: &mut Vec<SessionCandidate>, cap: usize) {
    if out.len() <= cap {
        return;
    }
    let mut ranked: Vec<(std::time::SystemTime, usize)> = out
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mtime = std::fs::metadata(&candidate.path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, index)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    let keep: std::collections::BTreeSet<usize> = ranked
        .into_iter()
        .take(cap)
        .map(|(_, index)| index)
        .collect();
    let mut index = 0;
    out.retain(|_| {
        let kept = keep.contains(&index);
        index += 1;
        kept
    });
}

pub(super) fn read_head_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let complete = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(last_newline) => &bytes[..=last_newline],
        None => &bytes[..],
    };
    Ok(String::from_utf8_lossy(complete).into_owned())
}

fn collect_optional_candidate_files(
    dir: &Path,
    agent_id: &str,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if dir.exists() {
        let _ = collect_candidate_files(dir, agent_id, 0, true, out);
    }
    Ok(())
}

fn collect_candidate_files(
    dir: &Path,
    agent_id: &str,
    depth: usize,
    requires_cwd_match: bool,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if depth > 4 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if depth == 0 => return Err(ProviderError::Io(format!("{}: {e}", dir.display()))),
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            collect_candidate_files(
                &path,
                agent_id,
                depth.saturating_add(1),
                requires_cwd_match,
                out,
            )?;
        } else if looks_like_session_file(&path, agent_id) {
            out.push(SessionCandidate {
                path,
                requires_cwd_match,
            });
        }
    }
    Ok(())
}

fn looks_like_session_file(path: &Path, agent_id: &str) -> bool {
    if path_is_under_team_runtime(path) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl")
        || name.ends_with(".json")
        || name.contains("session")
        || name.contains("rollout")
        || (!agent_id.is_empty() && name.contains(agent_id))
}

pub(super) fn parse_session_records(text: &str) -> Vec<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(value) => vec![value],
        Err(_) => parse_jsonl_records(text),
    }
}

fn provider_home_records_match_spawn_cwd(records: &[serde_json::Value], spawn_cwd: &Path) -> bool {
    let cwd_values: Vec<String> = records.iter().filter_map(record_cwd).collect();
    !cwd_values.is_empty()
        && cwd_values
            .iter()
            .any(|cwd| paths_equivalent(Path::new(cwd), spawn_cwd))
}

fn candidate_text_has_team_agent_id(text: &str, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    [
        format!("\"TEAM_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_ID={id}"),
        format!("env.TEAM_AGENT_ID=\"{id}\""),
        format!("env.TEAM_AGENT_ID=\\\"{id}\\\""),
        format!("\"TEAM_AGENT_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_AGENT_ID={id}"),
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(crate) fn embedded_team_agent_worker_id_from_text(text: &str) -> Option<String> {
    const PREFIX: &str = "You are Team Agent worker `";
    let start = text.find(PREFIX)? + PREFIX.len();
    let rest = &text[start..];
    let end = rest.find('`')?;
    let id = &rest[..end];
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return None;
    }
    Some(id.to_string())
}

pub(crate) fn rollout_path_embedded_team_agent_worker_id(path: &Path) -> Option<String> {
    read_head_text(path, CAPTURE_HEAD_BYTES)
        .ok()
        .and_then(|text| embedded_team_agent_worker_id_from_text(&text))
}

fn candidate_path_matches_agent_id(path: &Path, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let dashed = id.replace('_', "-");
    name.contains(id) || name.contains(&dashed)
}

pub(super) fn record_cwd(record: &serde_json::Value) -> Option<String> {
    record
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            record
                .get("session_meta")
                .and_then(|v| v.get("payload"))
                .or_else(|| record.get("payload"))
                .and_then(|v| v.get("cwd"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right || left.parent().is_some_and(|parent| parent == right)
}

fn path_is_under_team_runtime(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(".team"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("ta-session-scan-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_codex_rollout(path: &Path, cwd: &Path, session_id: &str, embedded_agent_id: &str) {
        let text = format!(
            "{{\"session_meta\":{{\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\"}}}}}}\n\
             {{\"type\":\"turn_context\",\"payload\":{{}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"content\":[{{\"type\":\"input_text\",\"text\":\"You are Team Agent worker `{embedded_agent_id}` with role `fixture`.\"}}]}}}}\n",
            cwd.to_string_lossy()
        );
        std::fs::write(path, text).expect("write codex rollout");
    }

    #[test]
    fn codex_prompt_worker_identity_is_a_positive_match() {
        let dir = temp_dir("positive");
        let rollout = dir.join("rollout-frontend.jsonl");
        write_codex_rollout(&rollout, &dir, "sess-frontend", "frontend");
        let context = CaptureSessionContext {
            agent_id: "frontend".to_string(),
            spawn_cwd: dir.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        let candidates = parse_candidate_files(
            Provider::Codex,
            &context,
            vec![SessionCandidate {
                path: rollout.clone(),
                requires_cwd_match: false,
            }],
        );
        assert_eq!(candidates.len(), 1);
        assert!(
            candidates[0].positive_agent_id_match,
            "Codex transcript prompt identity must be treated as a positive worker id source"
        );
        let _ = std::fs::remove_file(&rollout);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_prompt_worker_identity_mismatch_is_rejected_for_that_agent() {
        let dir = temp_dir("mismatch");
        let rollout = dir.join("rollout-ios-dev.jsonl");
        write_codex_rollout(&rollout, &dir, "sess-ios-dev", "ios-dev");
        let context = CaptureSessionContext {
            agent_id: "frontend".to_string(),
            spawn_cwd: dir.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        let candidates = parse_candidate_files(
            Provider::Codex,
            &context,
            vec![SessionCandidate {
                path: rollout.clone(),
                requires_cwd_match: false,
            }],
        );
        assert!(
            candidates.is_empty(),
            "state agent=frontend must not accept a Codex rollout whose prompt says ios-dev"
        );
        let _ = std::fs::remove_file(&rollout);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
