//! Public lifecycle dispatch for contract seats. A private native pane is never
//! operated through the legacy workspace socket or provider-enum fallback.
use std::path::Path;

use serde_json::{json, Value};
use team_agent_contract::contract::types::SeatId;
use team_agent_contract::orchestration::store::{Outcome, SeatStatus};

use crate::cli::CliError;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};
use crate::model::ids::AgentId;
use crate::state::repository::{StateRepository, StateWriteIntent};
use crate::state::selector::{resolve_active_team_readonly, SelectedTeam, SelectorMode};

use super::{backend::{self, Backend, BackendError}, config, framework};

fn error(error: impl std::fmt::Display) -> CliError {
    CliError::Runtime(error.to_string())
}

/// None delegates only a non-contract selection. Once selected, any identity,
/// ownership, state or physical error stays on this route and cannot fall back.
pub(super) fn with_agent(
    workspace: &Path,
    team: Option<&str>,
    agent: &str,
    operation: &'static str,
    apply: impl FnOnce(&SelectedTeam, &SeatId) -> Result<Value, CliError>,
) -> Result<Option<Value>, CliError> {
    let Ok(selected) = resolve_active_team_readonly(workspace, team, SelectorMode::RuntimeOnly) else {
        return Ok(None);
    };
    if !selected.state.get("agents").and_then(|agents| agents.get(agent)).is_some_and(framework::is_contract) {
        return Ok(None);
    }
    let agent_id = AgentId::new(agent);
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation,
        team: Some(&selected.team_key),
        agent_id: Some(&agent_id),
    }).map_err(error)?;
    let latest = resolve_active_team_readonly(
        &selected.run_workspace, Some(&selected.team_key), SelectorMode::RuntimeOnly,
    ).map_err(error)?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&latest.state, Some(&agent_id)).map_err(error)?;
    if !latest.state.get("agents").and_then(|agents| agents.get(agent)).is_some_and(framework::is_contract) {
        return Err(CliError::Runtime("contract seat changed while acquiring lifecycle ownership".into()));
    }
    apply(&latest, &SeatId::new(agent).map_err(error)?).map(Some)
}

fn publish(selected: &SelectedTeam, id: &SeatId, clear_stop: bool) -> Result<Value, CliError> {
    let projected = publish_at(&selected.run_workspace, &selected.team_key, id, clear_stop)?;
    backend::bind_workspace_session(&selected.run_workspace, &selected.team_key, id).map_err(error)?;
    Ok(projected)
}
pub(super) fn publish_at(workspace: &Path, team: &str, id: &SeatId, clear_stop: bool) -> Result<Value, CliError> {
    let projected = framework::project_seat(workspace, team, id.as_str()).map_err(error)?;
    StateRepository::new(workspace).commit(
        StateWriteIntent::StartAgent { team_key: team, agent_id: id.as_str() },
        |state| {
            if let Some(row) = state.get_mut("agents").and_then(|agents| agents.get_mut(id.as_str())).and_then(Value::as_object_mut) {
                for (key, value) in projected.as_object().expect("native projection object") {
                    row.insert(key.clone(), value.clone());
                }
                row.insert("paused".into(), Value::Bool(false));
                if clear_stop {
                    row.insert("explicit_stop".into(), Value::Bool(false));
                }
            }
        },
    ).map_err(error)?;
    Ok(projected)
}

fn check_record(receipt: &team_agent_contract::orchestration::store::OperationRecord) -> Result<(), CliError> {
    if receipt.outcome != Outcome::Committed {
        return Err(error(BackendError::Shutdown {
            operation: receipt.id.clone(), phase: receipt.phase, outcome: receipt.outcome, failure: receipt.failure.clone(),
        }));
    }
    Ok(())
}

// A partial native operation may not have a session for projection yet. Its
// original receipt must win over any secondary projection/metadata error.
fn checked_projection(
    receipt: &team_agent_contract::orchestration::store::OperationRecord,
    project: impl FnOnce() -> Result<Value, CliError>,
) -> Result<Value, CliError> {
    check_record(receipt)?;
    project()
}

pub fn start_agent(
    workspace: &Path,
    team: Option<&str>,
    agent: &str,
    patch: &crate::lifecycle::role_config::RoleConfigPatch,
) -> Result<Option<Value>, CliError> {
    with_agent(workspace, team, agent, "start-agent", |selected, id| {
        let args = crate::cli::QuickStartArgs {
            workspace: selected.run_workspace.clone(), agents_dir: selected.team_dir.clone(),
            name: None, team_id: Some(selected.team_key.clone()), yes: false, json: false,
            detail: false, backend: Some("tmux".into()),
        };
        // Contract configuration, not a stale legacy dynamic_role_file, is the
        // authoritative source. Never patch an unrelated external role file.
        let original = config::read_team(&args).map_err(error)?.ok_or(BackendError::Metadata).map_err(error)?;
        let source = original.roles.iter().find(|role| &role.id == id)
            .ok_or(BackendError::Metadata).map_err(error)?.source.clone();
        let previous = std::fs::read_to_string(&source).map_err(error)?;
        let next = crate::lifecycle::role_config::patched_role(&previous, &AgentId::new(agent), patch, false).map_err(error)?;
        let config = config::read_team_with_role_override(&args, &source, &next).map_err(error)?.ok_or(BackendError::Metadata).map_err(error)?;
        let role = config.roles.iter().find(|role| &role.id == id).ok_or(BackendError::Metadata).map_err(error)?;
        let row = selected.state.get("agents").and_then(|agents| agents.get(agent)).ok_or(BackendError::Metadata).map_err(error)?;
        if row.get("provider").and_then(Value::as_str) != Some(role.provider.as_str()) {
            return Err(error("provider change requires a new seat identity"));
        }
        if role.paused {
            return Ok(json!({"ok":false,"agent_id":agent,"status":"paused","reason":"agent_paused"}));
        }
        let unstarted = framework::is_paused_unstarted(row);
        if unstarted {
            // Full pure team admission precedes native allocation or role writes.
            backend::preflight(&config).map_err(error)?;
        } else {
            backend::preflight_start_role(&selected.run_workspace, &selected.team_key, role).map_err(error)?;
        }
        crate::lifecycle::role_config::commit_contract_role(&source, &previous, &next).map_err(error)?;
        let receipt = backend::start(&selected.run_workspace, &selected.team_key, role, &config.members).map_err(error)?.1;
        // An uncertain operation never rolls the authoritative native store back
        // to the old generation. The receipt remains available for recovery.
        let projection = checked_projection(&receipt, || publish(selected, id, true))?;
        let coordinator = crate::coordinator::start_coordinator_with_team(
            &crate::coordinator::WorkspacePath::new(selected.run_workspace.clone()), Some(&selected.team_key),
        ).map_err(error)?;
        Ok(json!({"ok":coordinator.ok,"agent_id":agent,"status":"running","runtime":"contract",
            "start_mode":if unstarted {"fresh"} else {"resumed"}, "operation":receipt.id,
            "session_id":projection.get("session_id"),"generation":projection.get("generation"),
            "coordinator":crate::lifecycle::coordinator_start_summary_value(
                &crate::lifecycle::CoordinatorStartSummary::from_start_report(&coordinator))}))
    })
}

pub(crate) struct NativeRestartResult {
    pub agent_id: String,
    pub session_id: Option<String>,
    pub outcome: Result<(), String>,
}

/// Called with the ordinary workspace lifecycle lock already held. Native
/// panes never enter legacy transport spawn, backing scan or shared teardown.
pub(crate) fn restart_prepared(
    workspace: &Path,
    team: &str,
    state: &mut Value,
    prepared: Vec<backend::PreparedResume>,
) -> Vec<NativeRestartResult> {
    prepared.into_iter().map(|prepared| {
        let id = prepared.seat().clone();
        let session_id = prepared.session().map(str::to_owned);
        let outcome = (|| {
            let receipt = backend::resume(prepared).map_err(error)?;
            let projection = checked_projection(&receipt, || publish_at(workspace, team, &id, false))?;
            if let Some(row) = state.get_mut("agents").and_then(|agents| agents.get_mut(id.as_str())).and_then(Value::as_object_mut) {
                for (key, value) in projection.as_object().expect("native projection object") {
                    row.insert(key.clone(), value.clone());
                }
            }
            Ok::<(), CliError>(())
        })().map_err(|error| error.to_string());
        NativeRestartResult { agent_id: id.as_str().into(), session_id, outcome }
    }).collect()
}

pub fn restart(workspace: &Path, team: Option<&str>) -> Result<Option<Value>, CliError> {
    let Ok(selected) = resolve_active_team_readonly(workspace, team, SelectorMode::RuntimeOnly) else {
        return Ok(None);
    };
    if !framework::only_contract(&selected.state) { return Ok(None); }
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace, operation: "restart",
        team: Some(&selected.team_key), agent_id: None,
    }).map_err(error)?;
    let latest = resolve_active_team_readonly(&selected.run_workspace, Some(&selected.team_key), SelectorMode::RuntimeOnly).map_err(error)?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&latest.state, None).map_err(error)?;
    if !framework::only_contract(&latest.state) { return Err(error("native team changed while acquiring lifecycle ownership")); }
    let prepared = backend::prepare_restart(&latest.run_workspace, &latest.team_key, &latest.team_dir, &latest.state).map_err(error)?;
    let mut state = latest.state.clone();
    let results = restart_prepared(&latest.run_workspace, &latest.team_key, &mut state, prepared);
    let succeeded: Vec<_> = results.iter().filter(|result| result.outcome.is_ok()).map(|result| json!({
        "agent_id":result.agent_id,"restart_mode":"resumed","session_id":result.session_id,
    })).collect();
    let failed: Vec<_> = results.iter().filter_map(|result| result.outcome.as_ref().err().map(|error| json!({
        "agent_id":result.agent_id,"phase":"native_resume","error":error,
    }))).collect();
    let coordinator = crate::coordinator::start_coordinator_with_team(
        &crate::coordinator::WorkspacePath::new(latest.run_workspace.clone()), Some(&latest.team_key),
    ).map_err(error)?;
    Ok(Some(json!({"ok":failed.is_empty() && coordinator.ok,"runtime":"contract",
        "status":if failed.is_empty() {"restarted"} else if succeeded.is_empty() {"failed"} else {"partial"},
        "team_id":latest.team_key,"agents":succeeded,"failed_agents":failed,
        "coordinator":crate::lifecycle::coordinator_start_summary_value(
            &crate::lifecycle::CoordinatorStartSummary::from_start_report(&coordinator))})))
}

pub fn stop_agent(
    workspace: &Path,
    team: Option<&str>,
    agent: &str,
) -> Result<Option<Value>, CliError> {
    with_agent(workspace, team, agent, "stop-agent", |selected, id| {
        if selected.state.get("agents").and_then(|agents| agents.get(agent)).is_some_and(framework::is_paused_unstarted) {
            return Ok(json!({"ok":true,"agent_id":agent,"stopped":false,"status":"paused","runtime":"contract"}));
        }
        let backend = Backend::open_readonly(&selected.run_workspace, &selected.team_key).map_err(error)?;
        let seat = backend.store.seat(id).map_err(error)?.ok_or(BackendError::Metadata).map_err(error)?;
        let already_stopped = seat.status == SeatStatus::Stopped;
        drop(backend);
        let operation = if already_stopped {
            None
        } else {
            let receipt = backend::stop(&selected.run_workspace, &selected.team_key, id).map_err(error)?;
            if receipt.outcome != Outcome::Committed {
                return Err(error(BackendError::Shutdown {
                    operation: receipt.id, phase: receipt.phase, outcome: receipt.outcome, failure: receipt.failure,
                }));
            }
            Some(receipt.id)
        };
        let projected = framework::project_seat(&selected.run_workspace, &selected.team_key, id.as_str()).map_err(error)?;
        let mut state = selected.state.clone();
        let row = state.get_mut("agents").and_then(|agents| agents.get_mut(agent)).and_then(Value::as_object_mut).ok_or(BackendError::Metadata).map_err(error)?;
        for (key, value) in projected.as_object().expect("native projection object") {
            row.insert(key.clone(), value.clone());
        }
        row.insert("explicit_stop".into(), Value::Bool(true));
        StateRepository::new(&selected.run_workspace).save(
            StateWriteIntent::StopAgent { team_key: &selected.team_key, agent_id: agent }, &state,
        ).map_err(error)?;
        Ok(json!({"ok":true,"agent_id":agent,"stopped":!already_stopped,
            "status":if already_stopped {"already_stopped"} else {"stopped"},
            "runtime":"contract","operation":operation}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use team_agent_contract::contract::{delivery::DeliveryEffect, session::CwdIdentity, types::*};
    use team_agent_contract::orchestration::store::{OperationRecord, Phase, SeatRecord, TransactionKind};

    fn receipt(outcome: Outcome) -> OperationRecord {
        let identity = InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("grok-worker").unwrap(),
            instance: InstanceId::new("instance-two").unwrap(), generation: Generation(2),
        };
        OperationRecord {
            id: OperationId::new("resume-instance-two").unwrap(), kind: TransactionKind::Restart,
            target: SeatRecord {
                identity, provider: ProviderId::new("grok_build").unwrap(),
                native: NativeIdentity { version: "fixture".into(), harness: "fixture".into(), ui: "tui".into(),
                    platform: Platform::MacOs, executable_sha256: Digest([0; 32]) },
                cwd: CwdIdentity { path: "/controlled/native-cwd".into(), identity: Digest([0; 32]) },
                endpoint: "fixture".into(), pane: "%0".into(), binding_key: "fixture".into(),
                profile: "fixture".into(), server_key: "fixture".into(), evidence_kind: EvidenceKind::Fixture,
                status: SeatStatus::Starting, process: None, physical: None, session: None,
                resources: vec![], workspace_resources: vec![], bootstrap_used: false,
            },
            parent: None, phase: Phase::F3Spawn, pending: Some("compensating-stop".into()),
            resources: vec![], effect: DeliveryEffect::NoEffect, outcome, preserved: vec![],
            failure: Some("native control SessionInspect guard-deadline".into()),
        }
    }

    #[test]
    fn uncommitted_receipt_precedes_projection_and_preserves_original_failure() {
        for outcome in [Outcome::Running, Outcome::Compensated, Outcome::NeedsRecovery] {
            let record = receipt(outcome);
            let before = record.clone();
            let mut projected = false;
            let mut state = json!({"status":"stopped","explicit_stop":true,"generation":1});
            let original = state.clone();
            let error = checked_projection(&record, || {
                projected = true;
                state["explicit_stop"] = Value::Bool(false);
                Err(super::error(BackendError::Metadata))
            }).unwrap_err().to_string();
            assert!(!projected);
            assert_eq!(state, original);
            assert_eq!(record, before);
            for value in ["resume-instance-two", "F3Spawn", "SessionInspect", "guard-deadline"] {
                assert!(error.contains(value), "{error}");
            }
            assert!(!error.contains("metadata is invalid"), "{error}");
        }
    }

    #[test]
    fn committed_receipt_retains_projection_success_and_error() {
        let record = receipt(Outcome::Committed);
        let mut calls = 0;
        let projected = checked_projection(&record, || {
            calls += 1;
            Ok(json!({"status":"running","explicit_stop":false,"generation":2}))
        }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(projected["explicit_stop"], false);
        let error = checked_projection(&record, || Err(super::error(BackendError::Metadata)))
            .unwrap_err().to_string();
        assert!(error.contains("metadata is invalid"));
    }
}
