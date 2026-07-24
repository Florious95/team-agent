use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn verify_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    source_agent_id: &str,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let context = crate::provider::session_scan::CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(spawned_at.to_string()),
        expected_session_id: plan.expected_session_id.clone(),
        provider_projects_root: plan.provider_projects_root.clone(),
    };
    let excluded = outcome::source_exclusions(before, source_session_id);
    let started = std::time::Instant::now();
    loop {
        let current = jsonl_files(&before.root);
        inject_target_identity(
            before,
            &current,
            &excluded,
            source_agent_id,
            agent_id,
            spawn_cwd,
        )?;
        for candidate in
            crate::provider::session_scan::scan_session_candidates_once(Provider::Codex, &context)?
        {
            let Some(path) = candidate.captured.rollout_path.as_ref() else {
                continue;
            };
            let Some(stamp) = current.get(path.as_path()) else {
                continue;
            };
            let snapshot_changed = before.files.get(path.as_path()) != Some(stamp);
            if !snapshot_changed {
                continue;
            }
            let Some(new_session_id) = candidate.captured.session_id else {
                continue;
            };
            if excluded.contains(new_session_id.as_str())
                || excluded.contains(&path.as_path().to_string_lossy().to_string())
            {
                continue;
            }
            return Ok(ContextForkProof {
                provider: Provider::Codex,
                source_session_id: source_session_id.clone(),
                new_session_id,
                backing_path: path.as_path().to_path_buf(),
                captured_via: "context_fork_verified".to_string(),
                attribution_confidence: "high".to_string(),
                managed_backing_root: None,
            });
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ContextForkTermination::Timeout {
        provider: Provider::Codex,
        deadline_ms: deadline.as_millis(),
    })
}

fn inject_target_identity(
    before: &ContextBackingSnapshot,
    current: &BTreeMap<PathBuf, FileStamp>,
    excluded: &BTreeSet<String>,
    source_agent_id: &str,
    target_agent_id: &str,
    spawn_cwd: &Path,
) -> Result<(), ContextForkTermination> {
    let source_marker = format!("You are Team Agent worker `{source_agent_id}`");
    let target_marker = format!("You are Team Agent worker `{target_agent_id}`");
    let mut eligible = Vec::new();
    for (path, stamp) in current {
        if before.files.get(path) == Some(stamp)
            || excluded.contains(&path.to_string_lossy().to_string())
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let session_id = session_id_from_jsonl(path);
        if session_id.as_deref().is_none_or(|id| excluded.contains(id)) {
            continue;
        }
        let cwd_matches = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|record| {
                record
                    .get("session_meta")
                    .and_then(|value| value.get("payload"))
                    .or_else(|| record.get("payload"))
                    .and_then(|value| value.get("cwd"))
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from)
            })
            .any(|cwd| paths_equivalent(&cwd, spawn_cwd));
        if !cwd_matches || !text.contains(&source_marker) || text.contains(&target_marker) {
            continue;
        }
        eligible.push((path, text));
    }
    if let [(path, text)] = eligible.as_slice() {
        let rewritten = text.replacen(&source_marker, &target_marker, 1);
        std::fs::write(path, rewritten)
            .map_err(|error| ProviderError::CaptureFailed(error.to_string()))?;
    }
    Ok(())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    left == right
        || std::fs::canonicalize(left)
            .ok()
            .zip(std::fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}
