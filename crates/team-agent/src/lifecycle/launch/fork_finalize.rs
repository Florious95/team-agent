//!
use super::*;

pub(super) struct ForkPostSpawnInput<'a> {
    pub workspace: &'a Path,
    pub transport: &'a dyn Transport,
    pub session_name: &'a SessionName,
    pub window: &'a WindowName,
    pub mcp_config_path: &'a Path,
    pub agent_id: &'a AgentId,
    pub profile_launch: &'a crate::provider::ProviderProfileLaunch,
    pub team_key: &'a str,
    pub spawn: &'a crate::transport::SpawnResult,
}

pub(super) fn ensure_fork_spawn_live(input: ForkPostSpawnInput<'_>) -> Result<(), LifecycleError> {
    let rollback = || {
        rollback_fork_after_spawn(
            input.workspace,
            input.transport,
            input.session_name,
            input.window,
            input.mcp_config_path,
            input.agent_id,
            input.profile_launch,
            input.team_key,
        );
    };
    if !matches!(
        input.transport.liveness(&input.spawn.pane_id),
        Ok(PaneLiveness::Live)
    ) {
        rollback();
        return Err(LifecycleError::RequirementUnmet(format!(
            "fork process is not live after spawn: agent={} pane={}",
            input.agent_id,
            input.spawn.pane_id.as_str()
        )));
    }
    Ok(())
}

pub(super) fn prepare_claude_fork_backing(
    provider: Provider,
    plan: &crate::provider::CommandPlan,
    source_backing: &Path,
    source_session_id: &crate::provider::SessionId,
    source_agent_id: &AgentId,
    target_agent_id: &AgentId,
) -> Result<Option<crate::provider::adapters::claude_fork::ClaudeForkMaterialization>, LifecycleError>
{
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return Ok(None);
    }
    let target_session_id = plan.expected_session_id.as_ref().ok_or_else(|| {
        LifecycleError::Provider("claude fork plan has no snapshot session id".to_string())
    })?;
    crate::provider::adapters::claude_fork::materialize_claude_fork(
        source_backing,
        source_session_id,
        target_session_id,
        source_agent_id.as_str(),
        target_agent_id.as_str(),
    )
    .map(Some)
    .map_err(|error| {
        LifecycleError::Provider(format!(
            "context_fork_unavailable: claude snapshot copy failed before spawn: {error}"
        ))
    })
}

pub(super) struct ForkCoordinatorInput<'a> {
    pub workspace: &'a Path,
    pub team_key: &'a str,
    pub agent_id: &'a AgentId,
    pub transport: &'a dyn Transport,
    pub session_name: &'a SessionName,
    pub window: &'a WindowName,
    pub mcp_config_path: &'a Path,
    pub profile_launch: &'a crate::provider::ProviderProfileLaunch,
}

pub(super) fn start_fork_coordinator(
    input: ForkCoordinatorInput<'_>,
) -> Result<bool, LifecycleError> {
    let rollback = || {
        rollback_fork_after_spawn(
            input.workspace,
            input.transport,
            input.session_name,
            input.window,
            input.mcp_config_path,
            input.agent_id,
            input.profile_launch,
            input.team_key,
        );
    };
    if let Err(error) = maybe_fail_fork_after_spawn("start_coordinator") {
        rollback();
        return Err(error);
    }
    crate::coordinator::start_coordinator(&crate::coordinator::WorkspacePath::new(
        input.workspace.to_path_buf(),
    ))
    .map(|report| report.ok)
    .map_err(|error| {
        rollback();
        LifecycleError::StatePersist(error.to_string())
    })
}

pub(super) struct ForkFinalizeInput<'a> {
    pub workspace: &'a Path,
    pub team_key: &'a str,
    pub source_agent_id: &'a AgentId,
    pub agent_id: &'a AgentId,
    pub spec_agent: &'a Value,
    pub safety: &'a DangerousApproval,
    pub plan: &'a crate::provider::CommandPlan,
    pub profile_launch: &'a crate::provider::ProviderProfileLaunch,
    pub spawn: &'a crate::transport::SpawnResult,
    pub profile_dir: &'a Path,
    pub dynamic_role_file: &'a Path,
    pub context_proof: &'a crate::provider::session::ContextForkProof,
    pub spawned_at: &'a str,
    pub spawn_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextForkFinalized {
    source_agent_id: String,
    agent_id: String,
    session_id: String,
    rollout_path: String,
    captured_via: String,
    attribution_confidence: String,
}

impl ContextForkFinalized {
    fn new(
        source_agent_id: &str,
        agent_id: &str,
        session_id: &str,
        rollout_path: &Path,
        captured_via: &str,
        attribution_confidence: &str,
    ) -> Self {
        Self {
            source_agent_id: source_agent_id.to_string(),
            agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            rollout_path: rollout_path.to_string_lossy().to_string(),
            captured_via: captured_via.to_string(),
            attribution_confidence: attribution_confidence.to_string(),
        }
    }

    fn from_captured(
        source_agent_id: &str,
        agent_id: &str,
        captured: &crate::provider::CapturedSession,
    ) -> Option<Self> {
        let captured_via = serde_json::to_value(captured.captured_via).ok()?;
        let attribution_confidence = serde_json::to_value(captured.attribution_confidence).ok()?;
        Some(Self::new(
            source_agent_id,
            agent_id,
            captured.session_id.as_ref()?.as_str(),
            captured.rollout_path.as_ref()?.as_path(),
            captured_via.as_str()?,
            attribution_confidence.as_str()?,
        ))
    }

    pub(crate) fn write_audit(
        &self,
        event_log: &crate::event_log::EventLog,
    ) -> Result<(), crate::event_log::EventLogError> {
        event_log
            .write(
                crate::lifecycle::types::event_names::CONTEXT_FORK,
                serde_json::json!({
                    "source_agent_id": self.source_agent_id,
                    "agent_id": self.agent_id,
                    "session_id": self.session_id,
                    "rollout_path": self.rollout_path,
                    "captured_via": self.captured_via,
                    "attribution_confidence": self.attribution_confidence,
                }),
            )
            .map(|_| ())
    }
}

pub(super) fn finalize_fork_state(
    input: ForkFinalizeInput<'_>,
) -> Result<ContextForkFinalized, LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: input.workspace,
        operation: "fork-agent-finalize",
        team: Some(input.team_key),
        agent_id: Some(input.agent_id),
    })?;
    let mut next_state = crate::state::selector::resolve_active_team(
        input.workspace,
        Some(input.team_key),
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?
    .state;
    upsert_forked_agent_state(
        &mut next_state,
        input.source_agent_id,
        input.agent_id,
        input.spec_agent,
        input.safety,
        input.plan,
        input.profile_launch,
        input.spawn,
        input.workspace,
        Some(input.profile_dir),
        input.dynamic_role_file,
        input.context_proof,
        input.spawned_at,
        input.spawn_epoch,
    )?;
    if let Some(agent) = next_state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(input.agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    {
        persist_effective_approval_policy(agent, input.safety);
    }
    maybe_fail_fork_after_spawn("save_runtime_state")?;
    crate::state::repository::StateRepository::new(input.workspace)
        .save(
            crate::state::repository::StateWriteIntent::ForkAgent {
                team_key: input.team_key,
                agent_id: input.agent_id.as_str(),
            },
            &next_state,
        )
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    Ok(ContextForkFinalized::new(
        input.source_agent_id.as_str(),
        input.agent_id.as_str(),
        input.context_proof.new_session_id.as_str(),
        &input.context_proof.backing_path,
        &input.context_proof.captured_via,
        &input.context_proof.attribution_confidence,
    ))
}

pub(super) struct ForkPendingFinalizeInput<'a> {
    pub workspace: &'a Path,
    pub team_key: &'a str,
    pub source_agent_id: &'a AgentId,
    pub agent_id: &'a AgentId,
    pub spec_agent: &'a Value,
    pub safety: &'a DangerousApproval,
    pub plan: &'a crate::provider::CommandPlan,
    pub profile_launch: &'a crate::provider::ProviderProfileLaunch,
    pub spawn: &'a crate::transport::SpawnResult,
    pub profile_dir: &'a Path,
    pub dynamic_role_file: &'a Path,
    pub pending: &'a crate::provider::session::PendingContextFork,
    pub spawn_epoch: u64,
}

pub(super) fn finalize_pending_fork_state(
    input: ForkPendingFinalizeInput<'_>,
) -> Result<(), LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: input.workspace,
        operation: "fork-agent-pending",
        team: Some(input.team_key),
        agent_id: Some(input.agent_id),
    })?;
    let mut next_state = crate::state::selector::resolve_active_team(
        input.workspace,
        Some(input.team_key),
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?
    .state;
    upsert_pending_forked_agent_state(
        &mut next_state,
        input.source_agent_id,
        input.agent_id,
        input.spec_agent,
        input.safety,
        input.plan,
        input.profile_launch,
        input.spawn,
        Some(input.profile_dir),
        input.dynamic_role_file,
        input.pending,
        input.spawn_epoch,
    )?;
    maybe_fail_fork_after_spawn("save_runtime_state")?;
    crate::state::repository::StateRepository::new(input.workspace)
        .save(
            crate::state::repository::StateWriteIntent::ForkAgent {
                team_key: input.team_key,
                agent_id: input.agent_id.as_str(),
            },
            &next_state,
        )
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))
}

pub(crate) fn finalize_pending_fork_capture(
    agent: &mut serde_json::Map<String, serde_json::Value>,
    captured: &crate::provider::CapturedSession,
) -> Option<ContextForkFinalized> {
    let Some(session_id) = captured.session_id.as_ref() else {
        return None;
    };
    let Some(rollout_path) = captured.rollout_path.as_ref() else {
        return None;
    };
    if agent
        .get("fork_source_session_id")
        .and_then(serde_json::Value::as_str)
        == Some(session_id.as_str())
    {
        return None;
    }
    let finalized = ContextForkFinalized::from_captured(
        agent
            .get("forked_from")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        agent
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                agent
                    .get("pending_target_agent")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or_default(),
        captured,
    )?;
    agent.insert(
        "session_id".to_string(),
        serde_json::json!(session_id.as_str()),
    );
    agent.insert(
        "rollout_path".to_string(),
        serde_json::json!(rollout_path.as_path().to_string_lossy()),
    );
    agent.insert(
        "captured_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent.insert(
        "captured_via".to_string(),
        serde_json::to_value(captured.captured_via).unwrap_or(serde_json::Value::Null),
    );
    agent.insert(
        "attribution_confidence".to_string(),
        serde_json::to_value(captured.attribution_confidence).unwrap_or(serde_json::Value::Null),
    );
    agent.remove("_pending_session_id");
    agent.remove("attribution_ambiguous");
    agent.remove("pending_target_agent");
    agent.remove("pending_grace_secs");
    agent.insert("capture_state".to_string(), serde_json::json!("captured"));
    Some(finalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_fork_finalized_writes_catalog_event_and_canonical_fields() {
        let workspace = std::env::temp_dir().join(format!(
            "team-agent-context-fork-audit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("create audit workspace");
        let event_log = crate::event_log::EventLog::new(&workspace);
        ContextForkFinalized::new(
            "source",
            "target",
            "target-session",
            Path::new("/tmp/target.jsonl"),
            "context_fork_verified",
            "high",
        )
        .write_audit(&event_log)
        .expect("write context fork audit");

        let events = event_log.tail(0).expect("read context fork audit");
        let event = events.last().expect("context fork audit event");
        assert_eq!(
            event.get("event").and_then(serde_json::Value::as_str),
            Some(crate::lifecycle::types::event_names::CONTEXT_FORK)
        );
        assert_eq!(
            event
                .get("source_agent_id")
                .and_then(serde_json::Value::as_str),
            Some("source")
        );
        assert_eq!(
            event.get("agent_id").and_then(serde_json::Value::as_str),
            Some("target")
        );
        assert!(event.get("prompt").is_none());
        let _ = std::fs::remove_dir_all(workspace);
    }
}

pub(super) fn verify_fork_registration(
    workspace: &Path,
    team_key: &str,
    agent_id: &AgentId,
    spawn: &crate::transport::SpawnResult,
    window: &WindowName,
) -> Result<(), LifecycleError> {
    let saved = crate::state::projection::select_runtime_state(workspace, Some(team_key))
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    let agent = saved
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .ok_or_else(|| LifecycleError::StatePersist("canonical team row is missing".to_string()))?;
    if agent.get("pane_id").and_then(serde_json::Value::as_str) != Some(spawn.pane_id.as_str()) {
        return Err(LifecycleError::StatePersist(
            "canonical team pane_id does not match spawned pane".to_string(),
        ));
    }
    if agent.get("window").and_then(serde_json::Value::as_str) != Some(window.as_str()) {
        return Err(LifecycleError::StatePersist(
            "canonical team window does not match spawned window".to_string(),
        ));
    }
    if let Some(pid) = spawn.child_pid {
        if agent.get("pane_pid").and_then(serde_json::Value::as_u64) != Some(u64::from(pid)) {
            return Err(LifecycleError::StatePersist(
                "canonical team pane_pid does not match spawned process".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn rollback_fork_after_spawn(
    workspace: &Path,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
    mcp_config_path: &Path,
    agent_id: &AgentId,
    profile_launch: &crate::provider::ProviderProfileLaunch,
    team_key: &str,
) {
    let _ = transport.kill_window(&Target::SessionWindow {
        session: session_name.clone(),
        window: window.clone(),
    });
    if let Ok(_lock) = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace,
        operation: "fork-agent-rollback",
        team: Some(team_key),
        agent_id: Some(agent_id),
    }) {
        let _ = crate::lifecycle::restart::remove::remove_agent_with_transport_locked(
            workspace,
            agent_id,
            true,
            true,
            Some(team_key),
            transport,
        );
    }
    cleanup_fork_mcp_artifacts(workspace, agent_id, mcp_config_path, profile_launch);
}
