//! Claude 的锁内新席位 fork：同一运行时、同一 cwd，新席位首次 spawn 以原生
//! `--session-id <child> --resume <parent> --fork-session` 继承源会话；源只读。
use crate::lifecycle::launch::role_source::materialize_latest_role;
use crate::lifecycle::{ForkAgentReport, ForkBackingState, LifecycleError};
use crate::model::enums::AuthMode;
use crate::model::ids::AgentId;
use crate::model::yaml::Value as YamlValue;
use crate::provider::SessionId;
use crate::state::selector::SelectedTeam;
use crate::transport::Transport;

use super::common::{
    agent_auth_mode, agent_provider, agent_rollout_path, agent_session_id,
    resume_backing_exists_for_agent,
};

/// `fork-agent <claude seat> --as <new>` under the source's lifecycle lock.
/// The target is an ordinary add-agent seat of the parent's provider whose
/// first spawn forks the parent's captured session; the parent's transcript,
/// row and role are never written. Claude has no admitted in-place branch
/// (`/branch` moves the parent itself), so the same id is refused.
pub(crate) fn fork_claude_new_seat_locked(
    selected: &SelectedTeam,
    source: &AgentId,
    target: &AgentId,
    label: Option<&str>,
    open_display: bool,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let run_workspace = selected.run_workspace.as_path();
    let team_key = selected.team_key.as_str();
    let row = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source.as_str()))
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source}"))
        })?;
    let provider_raw = row
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("claude");
    if agent_auth_mode(row) == AuthMode::CompatibleApi {
        return Err(LifecycleError::Provider(format!(
            "{provider_raw} does not support native session fork"
        )));
    }
    if target == source {
        return Err(LifecycleError::RequirementUnmet(format!(
            "{source} forks into a new seat: pass --as <new agent id>; an in-place branch would move {source}'s own session"
        )));
    }
    if target.as_str() == "leader" {
        return Err(LifecycleError::RequirementUnmet(
            "fork target id `leader` is reserved".to_string(),
        ));
    }
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&selected.state, Some(source))?;
    let provider = agent_provider(row);
    let parent = agent_session_id(row).ok_or_else(|| {
        LifecycleError::RequirementUnmet(format!(
            "{source} has no captured {provider_raw} session to fork"
        ))
    })?;
    if !resume_backing_exists_for_agent(
        run_workspace,
        source,
        row,
        provider,
        &parent,
        agent_rollout_path(row).as_ref(),
    ) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "{source} session {} has no transcript to fork; nothing was created",
            parent.as_str()
        )));
    }
    let mut role = materialize_latest_role(
        run_workspace,
        &selected.team_dir,
        &selected.state,
        source,
        target,
        label,
    )?;
    // A fork keeps the parent's provider: a role since edited to another one
    // (or to the contract leaf) would silently start a fresh seat instead.
    let team_meta = crate::compiler::read_front_matter(&selected.team_dir.join("TEAM.md"))
        .map(|(meta, _)| meta)
        .unwrap_or(YamlValue::Null);
    let compiled = crate::compiler::compile_role_agent(
        role.path(),
        &team_meta,
        &run_workspace.to_string_lossy(),
    )
    .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    if compiled.agent.get("provider").and_then(YamlValue::as_str) != Some(provider_raw) {
        return Err(LifecycleError::Compile(format!(
            "{source}'s role no longer declares provider {provider_raw}; a fork keeps the parent's provider"
        )));
    }
    let added = crate::lifecycle::launch::add_forked_agent_locked(
        run_workspace,
        &selected.team_dir,
        target,
        role.path(),
        open_display,
        Some(team_key),
        transport,
        &parent,
    )?;
    role.keep();
    // Verified only once the child transcript exists; until then the seat runs
    // and the ordinary capture binds its preassigned child session.
    let state = crate::state::projection::select_runtime_state(run_workspace, Some(team_key))
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let child = state
        .get("agents")
        .and_then(|agents| agents.get(target.as_str()))
        .and_then(|row| {
            let child = ["session_id", "_pending_session_id"]
                .iter()
                .find_map(|key| {
                    row.get(*key)
                        .and_then(serde_json::Value::as_str)
                        .filter(|child| !child.is_empty())
                })
                .filter(|child| *child != parent.as_str())
                .map(SessionId::new)?;
            resume_backing_exists_for_agent(
                run_workspace,
                target,
                row,
                provider,
                &child,
                agent_rollout_path(row).as_ref(),
            )
            .then_some(child)
        });
    let _ = crate::event_log::EventLog::new(run_workspace).write(
        "lifecycle.fork.native_new_seat",
        serde_json::json!({
            "team": team_key,
            "source_agent_id": source.as_str(),
            "agent_id": target.as_str(),
            "provider": provider_raw,
            "source_session_id": parent.as_str(),
            "session_id": child.as_ref().map(SessionId::as_str),
        }),
    );
    Ok(ForkAgentReport {
        source_agent_id: source.clone(),
        new_agent_id: target.clone(),
        env: added.env,
        backing_state: if child.is_some() {
            ForkBackingState::Verified
        } else {
            ForkBackingState::PendingContextFork
        },
        session_id: child,
        pi_fork: None,
    })
}
