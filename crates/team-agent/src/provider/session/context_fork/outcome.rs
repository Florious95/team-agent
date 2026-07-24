use super::*;
use std::collections::BTreeSet;

pub(super) fn source_exclusions(
    before: &ContextBackingSnapshot,
    source_session_id: &SessionId,
) -> BTreeSet<String> {
    let mut excluded = BTreeSet::from([source_session_id.as_str().to_string()]);
    for path in before.files.keys() {
        if session_id_from_jsonl(path).as_deref() == Some(source_session_id.as_str()) {
            excluded.insert(path.to_string_lossy().to_string());
        }
    }
    excluded
}

#[derive(Debug, Clone)]
pub(crate) struct PendingContextFork {
    pub source_session_id: SessionId,
    pub target_agent: String,
    pub spawned_at: String,
    pub scanner_context: crate::provider::session_scan::CaptureSessionContext,
}

#[derive(Debug)]
pub(crate) enum ContextForkOutcome {
    Verified(ContextForkProof),
    Pending(PendingContextFork),
    Rejected(ProviderError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextForkPendingFailure {
    TranscriptMissing,
}

pub(crate) fn transition_pending_context_fork(
    triggered: bool,
    grace_expired: bool,
) -> Option<ContextForkPendingFailure> {
    (triggered && grace_expired).then_some(ContextForkPendingFailure::TranscriptMissing)
}

pub(crate) fn observe_context_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    source_agent_id: &str,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> ContextForkOutcome {
    match verify_context_fork(
        provider,
        source_session_id,
        plan,
        before,
        expected_backing_path,
        source_agent_id,
        agent_id,
        spawn_cwd,
        spawned_at,
        deadline,
    ) {
        Ok(proof) => ContextForkOutcome::Verified(proof),
        Err(ContextForkTermination::Timeout { .. }) => {
            ContextForkOutcome::Pending(PendingContextFork {
                source_session_id: source_session_id.clone(),
                target_agent: agent_id.to_string(),
                spawned_at: spawned_at.to_string(),
                scanner_context: crate::provider::session_scan::CaptureSessionContext {
                    agent_id: agent_id.to_string(),
                    spawn_cwd: spawn_cwd.to_path_buf(),
                    pane_id: None,
                    pane_pid: None,
                    spawned_at: Some(spawned_at.to_string()),
                    expected_session_id: plan.expected_session_id.clone(),
                    provider_projects_root: plan.provider_projects_root.clone(),
                },
            })
        }
        Err(ContextForkTermination::Rejected(error)) => ContextForkOutcome::Rejected(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convergence_deadlines_are_provider_specific() {
        assert_eq!(
            context_fork_convergence_deadline(Provider::Claude),
            Duration::from_secs(45)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Codex),
            Duration::from_secs(10)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Copilot),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn codex_without_new_backing_is_typed_pending() {
        let root =
            std::env::temp_dir().join(format!("ta-codex-fork-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: None,
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Codex, &plan);
        let outcome = observe_context_fork(
            Provider::Codex,
            &SessionId::new("source-session"),
            &plan,
            &before,
            None,
            "source",
            "fork",
            &root,
            "2026-07-24T00:00:00Z",
            Duration::ZERO,
        );
        let ContextForkOutcome::Pending(pending) = outcome else {
            panic!("missing backing must remain a typed pending fork")
        };
        assert_eq!(pending.source_session_id.as_str(), "source-session");
        assert_eq!(pending.target_agent, "fork");
        let _ = std::fs::remove_dir_all(&root);
    }
}
