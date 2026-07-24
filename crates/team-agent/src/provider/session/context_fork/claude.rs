use super::*;

pub(super) fn verify_claude_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    spawn_cwd: &Path,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let expected = plan.expected_session_id.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no expected session id".to_string(),
        )
    })?;
    let path = expected_backing_path.ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no exact snapshot backing".to_string(),
        )
    })?;
    let expected_name = format!("{}.jsonl", expected.as_str());
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(ProviderError::CaptureFailed(format!(
            "context_fork_unverified: Claude snapshot backing does not match expected session {}",
            expected.as_str()
        ))
        .into());
    }
    let started = std::time::Instant::now();
    loop {
        if let Some(stamp) = file_stamp(path) {
            let changed = before.files.get(path).is_none_or(|old| *old != stamp);
            let observed_matches =
                session_id_from_jsonl(path).is_none_or(|observed| observed == expected.as_str());
            if changed && readable_jsonl(path) && observed_matches && expected != source_session_id
            {
                return Ok(ContextForkProof {
                    provider,
                    source_session_id: source_session_id.clone(),
                    new_session_id: expected.clone(),
                    backing_path: path.to_path_buf(),
                    captured_via: "context_fork_verified".to_string(),
                    attribution_confidence: "high".to_string(),
                    managed_backing_root: None,
                });
            }
        }
        if let Some(provider_path) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|home| {
                crate::provider::session_scan::claude::projects_dir_for_cwd(&home, spawn_cwd)
            })
            .map(|root| root.join(&expected_name))
        {
            let observed_matches = session_id_from_jsonl(&provider_path)
                .is_some_and(|observed| observed == expected.as_str());
            let changed = file_stamp(&provider_path)
                .is_some_and(|stamp| before.files.get(&provider_path) != Some(&stamp));
            if changed
                && readable_jsonl(&provider_path)
                && observed_matches
                && expected != source_session_id
            {
                return Ok(ContextForkProof {
                    provider,
                    source_session_id: source_session_id.clone(),
                    new_session_id: expected.clone(),
                    backing_path: provider_path,
                    captured_via: "context_fork_verified".to_string(),
                    attribution_confidence: "high".to_string(),
                    managed_backing_root: None,
                });
            }
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ContextForkTermination::Timeout {
        provider,
        deadline_ms: deadline.as_millis(),
    })
}
