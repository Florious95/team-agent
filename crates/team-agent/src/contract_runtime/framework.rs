//! Shared-framework seams. A contract seat is never projected as a legacy pane:
//! its private socket's `%0` must not be interpreted on the workspace tmux server.
use super::{
    backend::{self, Backend, BackendError},
    config,
};
use serde_json::{json, Value};
use std::path::Path;
use team_agent_contract::contract::types::SeatId;
use team_agent_contract::host::process::{sample_process, ProcessState};
use team_agent_contract::orchestration::{
    operator::OperatorSend,
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
    let selected_native = match agent {
        Some(id) => selected
            .state
            .get("agents")
            .and_then(|agents| agents.get(id))
            .is_some_and(is_contract),
        None => has_contract(&selected.state),
    };
    let configured_native = selected
        .state
        .get("team_dir")
        .and_then(Value::as_str)
        .and_then(|directory| {
            team_config(
                &selected.run_workspace,
                Path::new(directory),
                &selected.team_key,
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
    let declared = path
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| {
            crate::compiler::split_front_matter(&raw)
                .0
                .get("provider")
                .and_then(crate::model::yaml::Value::as_str)
                .map(str::to_owned)
        });
    if provider
        .or(declared.as_deref())
        .is_some_and(super::registry::recognizes)
    {
        return Err(crate::cli::CliError::Runtime("contract_capability_unverified: mutable contract-native role lifecycle is not admitted; use initial quick-start; no role file was changed".into()));
    }
    Ok(())
}
pub fn team_config(
    workspace: &Path,
    directory: &Path,
    team: &str,
) -> Result<Option<config::TeamConfig>, config::ConfigError> {
    config::read_team(&crate::cli::QuickStartArgs {
        workspace: workspace.into(),
        agents_dir: directory.into(),
        name: None,
        team_id: Some(team.into()),
        yes: false,
        json: false,
        detail: false,
        backend: Some("tmux".into()),
    })
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
        let config = team_config(&selected.run_workspace, &selected.team_dir, &selected.team_key)
            .map_err(|e| error(e.to_string()))?
            .ok_or_else(|| error("contract role no longer configured".into()))?;
        let role = config.roles.iter().find(|role| &role.id == seat)
            .ok_or_else(|| error("contract role not found".into()))?;
        let (_, record) = backend::start(&selected.run_workspace, &selected.team_key, role, &config.members)
            .map_err(|e| error(e.to_string()))?;
        if record.outcome != Outcome::Committed {
            return Err(error(format!("native startup seat={id} operation={:?} phase={:?} outcome={:?}: {}",
                record.id, record.phase, record.outcome, record.failure.unwrap_or_default())));
        }
        // Preserve each committed seat before a later discovery/spawn can fail.
        // A whole-batch error must never hide this generation's physical effects.
        let projected = super::commands::publish_at(&selected.run_workspace, &selected.team_key, seat, true)?;
        Ok(projected)
    }).map_err(|e| crate::lifecycle::LifecycleError::Provider(e.to_string()))?;
    if result.is_none() {
        return Err(crate::lifecycle::LifecycleError::Provider("native startup scope/seat is not addressable; no legacy fallback".into()));
    }
    Ok(())
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
    let alive = seat
        .physical
        .as_ref()
        .is_some_and(|target| sample_process(&target.process) == ProcessState::Alive);
    // Present only individually proven tools. A successful report_result
    // never turns send_message/get_team_status into invented discoveries.
    let mut observed = Vec::new();
    let mut binding_source = None;
    if super::registry::provider(seat.provider.as_str())?.uses_tool_roundtrips() {
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
    } else if seat.status != SeatStatus::Unknown && alive {
        "running"
    } else {
        "unknown"
    };
    Ok(
        json!({"agent_id":id,"provider":seat.provider,"status":status,"model":role.model,"effort":role.effort,
        "contract_prompt_sha256":team_agent_contract::host::digest_hex(role.prompt.sha256),
        "runtime_family":"contract","contract_scope":seat.identity.scope,"contract_instance":seat.identity.instance,
        "generation":seat.identity.generation,"owner_team_id":team,"team_key":team,
        "session_id":seat.session.map(|session| session.native_session),
        "native_endpoint":seat.endpoint,"native_pane":seat.pane,
        "pane_id":null,"window":null,"session_name":null,
        "mcp_binding":binding_status,"mcp_binding_source":binding_source,"mcp_tools":tools,
        "bootstrap_used":seat.bootstrap_used}),
    )
}

pub fn enqueue(
    workspace: &Path,
    state: &Value,
    recipient: &str,
    content: &str,
    options: &crate::messaging::SendOptions,
) -> Result<crate::messaging::DeliveryOutcome, crate::messaging::MessagingError> {
    use crate::messaging::{DeliveryOutcome, DeliveryStatus, MessagingError};
    let error = |error: String| MessagingError::Validation(error);
    let team = crate::state::projection::team_state_key(state);
    let mut backend = Backend::open(workspace, &team).map_err(|e| error(e.to_string()))?;
    let request = OperatorSend {
        recipient: SeatId::new(recipient).map_err(|e| error(e.to_string()))?,
        content: content.into(),
        task: options.task_id.as_ref().map(|task| task.as_str().into()),
        message: options
            .message_id
            .clone()
            .map(team_agent_contract::contract::types::MessageId::new)
            .transpose()
            .map_err(|e| error(e.to_string()))?,
        mailbox: options.presentation.sink
            == crate::messaging::presentation::PresentationSink::Casefile,
    };
    let sender = SeatId::new(options.sender.as_str()).map_err(|e| error(e.to_string()))?;
    let receipt = backend
        .store
        .send_from_framework(&request, &sender)
        .map_err(|e| error(e.to_string()))?;
    // Queued is durable admission only. The coordinator consumes this outbox;
    // CLI send never performs its own paste or repeats an uncertain attempt.
    Ok(DeliveryOutcome {
        ok: true,
        status: if receipt.mailbox {
            DeliveryStatus::StoredOnly
        } else {
            DeliveryStatus::Queued
        },
        message_status: crate::messaging::helpers::MessageStatusShadow(
            if receipt.mailbox {
                "stored_only"
            } else {
                "accepted"
            }
            .into(),
        ),
        message_id: Some(receipt.message.as_str().into()),
        verification: Some(
            json!({"runtime":"contract","task_id":receipt.task,
            "target":receipt.target,"native_submission":"not_observed"})
            .to_string(),
        ),
        stage: None,
        reason: None,
        channel: None,
        ack_forced_off: false,
        turn_verification: None,
    })
}

pub fn stop_selected(workspace: &Path, state: &Value) -> Result<Vec<Value>, BackendError> {
    let team = crate::state::projection::team_state_key(state);
    let mut receipts = Vec::new();
    if let Some(agents) = state.get("agents").and_then(Value::as_object) {
        for (id, agent) in agents.iter().filter(|(_, agent)| is_contract(agent)) {
            if is_paused_unstarted(agent) {
                receipts.push(json!({"agent_id":id,"status":"paused","stopped":false}));
                continue;
            }
            let backend = Backend::open(workspace, &team)?;
            let seat = backend
                .store
                .seat(&SeatId::new(id)?)?
                .ok_or(BackendError::Metadata)?;
            if seat.status == SeatStatus::Stopped {
                receipts.push(json!({"agent_id":id,"status":"already_stopped"}));
                continue;
            }
            let _ = agent;
            let receipt = backend::stop(workspace, &team, &SeatId::new(id)?)?;
            if receipt.outcome != Outcome::Committed {
                return Err(BackendError::Shutdown {
                    operation: receipt.id,
                    phase: receipt.phase,
                    outcome: receipt.outcome,
                    failure: receipt.failure,
                });
            }
            receipts.push(json!({"agent_id":id,"status":"stopped","operation":receipt.id}));
        }
    }
    Ok(receipts)
}
