//! Shared-framework seams. Publish a native pane only with its private endpoint:
//! its `%0` must never be interpreted on the legacy workspace tmux server.
use super::{
    backend::{self, Backend, BackendError},
    config,
};
use serde_json::{json, Value};
use std::path::Path;
use team_agent_contract::contract::types::SeatId;
use team_agent_contract::host::process::{sample_process, ProcessState};
use team_agent_contract::orchestration::{
    store::{Outcome, SeatStatus},
};

pub fn is_contract(agent: &Value) -> bool {
    agent
        .get("provider")
        .and_then(Value::as_str)
        .is_some_and(|provider| super::registry::descriptor(provider).is_some())
}
/// Published paused roles without an instance never crossed native startup.
/// A bound instance is never skipped, even if a stale row says paused.
pub(crate) fn is_paused_unstarted(agent: &Value) -> bool {
    is_contract(agent)
        && agent.get("status").and_then(Value::as_str) == Some("paused")
        && agent.get("contract_instance").is_none_or(Value::is_null)
}

pub fn has_contract(state: &Value) -> bool {
    state
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| agents.values().any(is_contract))
}
pub fn only_contract(state: &Value) -> bool {
    state
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| !agents.is_empty() && agents.values().all(is_contract))
}
/// Unsupported lifecycle operations must stop before the legacy provider enum
/// can turn an unknown native seat into its historical Codex fallback.
pub fn require_legacy_lifecycle(
    workspace: &Path,
    team: Option<&str>,
    agent: Option<&str>,
    operation: &str,
) -> Result<(), crate::cli::CliError> {
    let Ok(selected) = crate::state::selector::resolve_active_team_readonly(
        workspace,
        team,
        crate::state::selector::SelectorMode::RuntimeOnly,
    ) else {
        return Ok(());
    };
    let row = agent.and_then(|id| selected.state.get("agents").and_then(|agents| agents.get(id)));
    // A pre-upgrade legacy Cursor, Codex or Grok seat has no engine left; only teardown is admitted.
    let teardown = matches!(operation, "stop-agent" | "remove-agent");
    if let Some(retired) = row
        .and_then(|row| row.get("provider"))
        .and_then(Value::as_str)
        .filter(|provider| !teardown && crate::provider::wire::is_retired_provider(provider))
    {
        return Err(crate::cli::CliError::Runtime(
            crate::provider::wire::retired_provider_error(retired).into(),
        ));
    }
    let selected_native = match agent {
        Some(_) => row.is_some_and(is_contract),
        None => has_contract(&selected.state),
    };
    // A recorded legacy seat is torn down on its own runtime even when its role
    // document now declares a contract leaf (`cursor_agent` before the upgrade).
    let configured_native = !(teardown && row.is_some())
        && selected
        .state
        .get("team_dir")
        .and_then(Value::as_str)
        .and_then(|directory| {
            team_config(
                &selected.run_workspace,
                Path::new(directory),
                &selected.team_key,
                agent,
            )
            .ok()
            .flatten()
        })
        .is_some_and(|config| match agent {
            Some(id) => config.roles.iter().any(|role| role.id.as_str() == id),
            None => !config.roles.is_empty(),
        });
    if selected_native || configured_native {
        return Err(crate::cli::CliError::Runtime(format!("contract_capability_unverified: {operation} is not admitted for contract-native seats; no legacy fallback or fresh replacement was performed")));
    }
    Ok(())
}
pub fn require_legacy_role(
    path: Option<&Path>,
    provider: Option<&str>,
) -> Result<(), crate::cli::CliError> {
    if declares_contract(path, provider) {
        return Err(crate::cli::CliError::Runtime("contract_capability_unverified: mutable contract-native role lifecycle is not admitted; use initial quick-start; no role file was changed".into()));
    }
    Ok(())
}
/// Whether a role document or CLI provider value selects a contract leaf.
pub fn declares_contract(path: Option<&Path>, provider: Option<&str>) -> bool {
    let meta = path
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|raw| crate::compiler::split_front_matter(&raw).0);
    let declared = meta.as_ref().and_then(|meta| {
        meta.get("provider")
            .and_then(crate::model::yaml::Value::as_str)
            .map(str::to_owned)
    });
    provider
        .or(declared.as_deref())
        .is_some_and(super::registry::recognizes)
}
pub fn team_config(
    workspace: &Path,
    directory: &Path,
    team: &str,
    seat: Option<&str>,
) -> Result<Option<config::TeamConfig>, config::ConfigError> {
    let args = crate::cli::QuickStartArgs {
        workspace: workspace.into(),
        agents_dir: directory.into(),
        name: None,
        team_id: Some(team.into()),
        yes: false,
        json: false,
        detail: false,
        backend: Some("tmux".into()),
    };
    match seat {
        Some(seat) => config::read_seat(&args, seat),
        None => config::read_team(&args),
    }
}
pub fn start_role(
    workspace: &Path,
    directory: &Path,
    team: &str,
    id: &str,
) -> Result<(), crate::lifecycle::LifecycleError> {
    let result = super::commands::with_agent(workspace, Some(team), id, "start-role", |selected, seat| {
        let error = |error: String| crate::cli::CliError::Runtime(error);
        if directory.canonicalize()? != selected.team_dir.canonicalize()? {
            return Err(error("native role source changed while acquiring lifecycle ownership".into()));
        }
        start_configured(&selected.run_workspace, &selected.team_dir, &selected.team_key, seat)
    }).map_err(|e| crate::lifecycle::LifecycleError::Provider(e.to_string()))?;
    if result.is_none() {
        return Err(crate::lifecycle::LifecycleError::Provider("native startup scope/seat is not addressable; no legacy fallback".into()));
    }
    Ok(())
}

/// Start one configured contract role and publish its seat. The caller holds
/// the lifecycle lock (or owns the whole launch) and has seeded the state row.
pub(crate) fn start_configured(
    workspace: &Path,
    directory: &Path,
    team: &str,
    seat: &SeatId,
) -> Result<Value, crate::cli::CliError> {
    let error = |error: String| crate::cli::CliError::Runtime(error);
    let config = team_config(workspace, directory, team, Some(seat.as_str()))
        .map_err(|e| error(e.to_string()))?
        .ok_or_else(|| error("contract role no longer configured".into()))?;
    let role = config.roles.iter().find(|role| &role.id == seat)
        .ok_or_else(|| error(format!("contract role not found in {}", directory.join("agents").display())))?;
    let (_, record) = backend::start(workspace, team, role, &config.members)
        .map_err(|e| error(e.to_string()))?;
    if record.outcome != Outcome::Committed {
        return Err(error(format!("native startup seat={} operation={:?} phase={:?} outcome={:?}: {}",
            seat.as_str(), record.id, record.phase, record.outcome, record.failure.unwrap_or_default())));
    }
    // Preserve each committed seat before a later discovery/spawn can fail.
    // A whole-batch error must never hide this generation's physical effects.
    super::commands::publish_at(workspace, team, seat, true)
}

/// add-agent owns a brand-new seat. If its start committed but publication
/// failed, stop that seat before the caller rolls its row back: a refused add
/// never leaves a running native process outside status/stop. An unpublished
/// spawn stays with its fenced receipt for explicit recovery.
pub(crate) fn start_new_seat(
    workspace: &Path,
    directory: &Path,
    team: &str,
    seat: &SeatId,
) -> Result<(), crate::cli::CliError> {
    let Err(error) = start_configured(workspace, directory, team, seat) else {
        return Ok(());
    };
    let started = Backend::open_readonly(workspace, team)
        .ok()
        .and_then(|backend| backend.store.seat(seat).ok().flatten())
        .is_some_and(|current| current.status != SeatStatus::Stopped && current.physical.is_some());
    if started {
        if let Err(stop) = backend::stop(workspace, team, seat) {
            return Err(crate::cli::CliError::Runtime(format!(
                "{error}; stopping the started native seat failed: {stop}"
            )));
        }
    }
    Err(error)
}

pub fn project_seat(workspace: &Path, team: &str, id: &str) -> Result<Value, BackendError> {
    let mut backend = Backend::open_readonly(workspace, team)?;
    let seat = backend
        .store
        .seat(&SeatId::new(id)?)?
        .ok_or(BackendError::Metadata)?;
    let role = backend::read_role(&backend.binding, &seat.identity)?;
    if role.provider != seat.provider {
        return Err(BackendError::Metadata);
    }
    let executing_candidate = backend::executing_candidate_sha()?;
    let foreign = seat.physical.as_ref().is_some_and(|target| target.candidate_sha256 != executing_candidate);
    let alive = seat
        .physical
        .as_ref()
        .is_some_and(|target| sample_process(&target.process) == ProcessState::Alive);
    // Present only individually proven tools. A successful report_result
    // never turns send_message/get_team_status into invented discoveries.
    let mut observed = Vec::new();
    let mut binding_source = None;
    if super::registry::provider(seat.provider.as_str())?.uses_tool_roundtrips()
        && seat.session.is_some() && !foreign
    {
        let protocol = super::pump::protocol(&mut backend, &seat, &team_agent_contract::host::clock::RealClock::new())?;
        if let team_agent_contract::contract::probe::ProbeOutcome::Observed(binding) = protocol.binding.outcome {
            binding_source = Some(format!("{:?}", binding.source));
            observed = binding.tools;
        }
    }
    let tools: serde_json::Map<String, Value> = [
        (team_agent_contract::contract::types::LogicalTool::SendMessage, "send_message"),
        (team_agent_contract::contract::types::LogicalTool::ReportResult, "report_result"),
        (team_agent_contract::contract::types::LogicalTool::GetTeamStatus, "get_team_status"),
    ].into_iter().map(|(tool, name)| (name.into(), json!(if observed.contains(&tool) { "observed" } else { "unknown" }))).collect();
    let binding_status = if observed.len() == 3 { "observed" } else if observed.is_empty() { "unverified" } else { "partial" };
    let status = if seat.status == SeatStatus::Stopped {
        "stopped"
    } else if foreign {
        "foreign"
    } else if seat.status != SeatStatus::Unknown && alive {
        "running"
    } else {
        "unknown"
    };
    let session_warning = if seat.session.is_none() {
        backend.store.creation_receipt(&seat.identity.seat, Some(&seat.identity))?
            .filter(|receipt| receipt.outcome == team_agent_contract::orchestration::store::Outcome::Committed)
            .and_then(|receipt| receipt.failure)
    } else { None };
    Ok(
        json!({"agent_id":id,"provider":if seat.provider.as_str() == "grok_build" { "grok" } else { seat.provider.as_str() },
         "contract_provider":seat.provider,"status":status,"foreign_candidate":foreign,"model":role.model,"effort":role.effort,
        "contract_prompt_sha256":team_agent_contract::host::digest_hex(role.prompt.sha256),
        "runtime_family":"contract","contract_scope":seat.identity.scope,"contract_instance":seat.identity.instance,
        "generation":seat.identity.generation,"owner_team_id":team,"team_key":team,
        "session_observation":if seat.session.is_some() { "observed" } else { "unobserved" },
        "session_warning":session_warning,
        "session_id":seat.session.map(|session| session.native_session),
        "native_endpoint":seat.endpoint,"native_pane":seat.pane,
        "pane_id":seat.pane,"tmux_endpoint":seat.endpoint,"window":null,"session_name":null,
        "mcp_binding":binding_status,"mcp_binding_source":binding_source,"mcp_tools":tools,
        "bootstrap_used":seat.bootstrap_used}),
    )
}

pub fn stop_selected(workspace: &Path, state: &Value) -> Result<Vec<Value>, BackendError> {
    stop_selected_with_budget(workspace, state, std::time::Duration::from_secs(20))
}

pub fn stop_selected_with_budget(workspace: &Path, state: &Value, budget: std::time::Duration) -> Result<Vec<Value>, BackendError> {
    let deadline = std::time::Instant::now() + budget;
    let team = crate::state::projection::team_state_key(state);
    let mut receipts = Vec::new();
    if let Some(agents) = state.get("agents").and_then(Value::as_object) {
        let agents: Vec<_> = agents.iter().filter(|(_, agent)| is_contract(agent)).collect();
        for (index, &(id, agent)) in agents.iter().enumerate() {
            // Leave time for siblings rather than giving the first seat the entire budget.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let seat_budget = remaining / u32::try_from(agents.len() - index).unwrap_or(u32::MAX);
            if is_paused_unstarted(agent) {
                receipts.push(json!({"agent_id":id,"status":"paused","stopped":false}));
                continue;
            }
            let mut physical = None;
            let result = (|| -> Result<Value, BackendError> {
                let agent_id = crate::model::ids::AgentId::new(id);
                let _ownership = crate::lifecycle::lock::try_acquire_seat_lifecycle_lock(crate::lifecycle::lock::LifecycleLockRequest {
                    workspace, operation: "native-team-stop", team: Some(&team), agent_id: Some(&agent_id),
                }).map_err(|error| std::io::Error::other(error.to_string()))?;
                let backend = Backend::open_readonly(workspace, &team)?;
                let seat_id = SeatId::new(id.as_str())?;
                let seat = backend.store.seat(&seat_id)?;
                physical = seat.as_ref().and_then(|seat| seat.physical.clone());
                if seat.as_ref().is_some_and(|seat| seat.status == SeatStatus::Stopped) && !backend.store.leased(&seat_id)? {
                    return Ok(json!({"agent_id":id,"status":"already_stopped"}));
                }
                let receipt = backend::stop_with_budget(workspace, &team, &seat_id, seat_budget)?;
                backend::require_stopped(&receipt)?;
                Ok(json!({"agent_id":id,"status":"stopped","operation":receipt.id,"outcome":receipt.outcome,"stop_receipt":receipt.stop_receipt}))
            })();
            receipts.push(result.unwrap_or_else(|error| json!({
                "agent_id":id,"status":"failed","error":error.to_string(),
                "effect":if matches!(&error, BackendError::StopRefused { .. }) { "NoEffect" } else { "Unknown" },
                "needs_recovery":!matches!(&error, BackendError::StopRefused { .. }),
                "pane_id":physical.as_ref().map(|target| json!(target.pane)).unwrap_or_else(|| agent.get("pane_id").cloned().unwrap_or(Value::Null)),
                "session_name":physical.as_ref().map(|target| &target.session),
                "shared_endpoint":physical.as_ref().map(|target| target.shared_endpoint),
                "endpoint":physical.as_ref().map(|target| json!(target.endpoint)).unwrap_or_else(|| agent.get("tmux_endpoint").cloned().unwrap_or(Value::Null)),
                "process_pid":physical.as_ref().map(|target| target.process.identity.pid),
            })));
        }
    }
    Ok(receipts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_shutdown_collects_failures_and_continues_to_other_seats() {
        let root = std::env::temp_dir().join(format!("native-stop-selected-{}-{}",
            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&root).unwrap();
        let state = json!({"team_key":"test","agents":{
            "a":{"provider":"cursor","status":"running","pane_id":"%1","tmux_endpoint":"/tmp/fixture.sock"},
            "b":{"provider":"cursor","status":"running"},
            "z":{"provider":"cursor","status":"paused"}
        }});
        let receipts = stop_selected(&root, &state).unwrap();
        assert_eq!(receipts.len(), 3);
        assert_eq!(receipts[0]["status"], "failed");
        assert!(receipts[0]["error"].as_str().is_some_and(|error| !error.is_empty()));
        assert_eq!(receipts[0]["pane_id"], "%1");
        assert_eq!(receipts[1]["status"], "failed");
        assert_eq!(receipts[2]["status"], "paused");
        std::fs::remove_dir_all(root).unwrap();
    }
}
