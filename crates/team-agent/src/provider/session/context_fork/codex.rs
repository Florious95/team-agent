use super::*;

pub(super) fn verify_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
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
