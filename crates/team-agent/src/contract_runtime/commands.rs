//! Public lifecycle dispatch for contract seats. A private native pane is never
//! operated through the legacy workspace socket or provider-enum fallback.
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use team_agent_contract::contract::delivery::InputSurface;
use team_agent_contract::contract::probe::{PaneReadyEvidence, Probe, ProbeOutcome};
use team_agent_contract::contract::types::{Digest, OperationId, SeatId};
use team_agent_contract::host::{clock::RealClock, process::{sample_process, ProcessState}};
use team_agent_contract::orchestration::physical::{PhysicalRuntime, Registration};
use team_agent_contract::orchestration::store::{Outcome, SeatStatus};

use crate::cli::CliError;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};
use crate::model::ids::AgentId;
use crate::state::repository::{StateRepository, StateWriteIntent};
use crate::state::selector::{resolve_active_team_readonly, SelectedTeam, SelectorMode};

use super::{backend::{self, Backend, BackendError}, config, framework, registry};

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
    // Fresh, explicit resume and team restart share this one publication path.
    // Only the exact live receipt may claim a session it actually created.
    backend::bind_workspace_session(workspace, team, id).map_err(error)?;
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
        let original = config::read_seat(&args, agent).map_err(error)?.ok_or(BackendError::Metadata).map_err(error)?;
        let source = original.roles.iter().find(|role| &role.id == id)
            .ok_or(BackendError::Metadata).map_err(error)?.source.clone();
        let previous = std::fs::read_to_string(&source).map_err(error)?;
        let next = crate::lifecycle::role_config::patched_role(&previous, &AgentId::new(agent), patch, false).map_err(error)?;
        let config = config::read_team_with_role_override(&args, &source, &next, agent).map_err(error)?.ok_or(BackendError::Metadata).map_err(error)?;
        let role = config.roles.iter().find(|role| &role.id == id).ok_or(BackendError::Metadata).map_err(error)?;
        let row = selected.state.get("agents").and_then(|agents| agents.get(agent)).ok_or(BackendError::Metadata).map_err(error)?;
        if !registry::same_provider(row.get("provider").and_then(Value::as_str).unwrap_or(""), role.provider.as_str()) {
            return Err(error("provider change requires a new seat identity"));
        }
        if role.paused {
            return Ok(json!({"ok":false,"agent_id":agent,"status":"paused","reason":"agent_paused"}));
        }
        let unstarted = framework::is_paused_unstarted(row);
        if unstarted {
            // Full pure team admission precedes native allocation or role writes.
            backend::preflight(&config).map_err(error)?;
        } else if backend::preflight_start_role(&selected.run_workspace, &selected.team_key, role).map_err(error)? {
            backend::preflight(&config).map_err(error)?;
        }
        let before = if unstarted { None } else {
            let backend = Backend::open_readonly(&selected.run_workspace, &selected.team_key).map_err(error)?;
            // A retired generation is history, not a session this start resumes.
            match backend.store.creation_receipt(id, None).map_err(error)? {
                Some(record) if backend.store.retired(&record.target.identity).map_err(error)? => None,
                other => other,
            }
        };
        crate::lifecycle::role_config::commit_contract_role(&source, &previous, &next).map_err(error)?;
        let receipt = backend::start(&selected.run_workspace, &selected.team_key, role, &config.members).map_err(error)?.1;
        // An uncertain operation never rolls the authoritative native store back
        // to the old generation. The receipt remains available for recovery.
        let projection = checked_projection(&receipt, || publish_at(&selected.run_workspace, &selected.team_key, id, true))?;
        let coordinator = crate::coordinator::start_coordinator_with_team(
            &crate::coordinator::WorkspacePath::new(selected.run_workspace.clone()), Some(&selected.team_key),
        ).map_err(error)?;
        Ok(json!({"ok":coordinator.ok,"agent_id":agent,"status":"running","runtime":"contract",
            "start_mode":match before {
                None => "fresh",
                Some(ref original) if original.outcome == Outcome::Compensated => "fresh",
                Some(ref original) if original.id == receipt.id && original.outcome != Outcome::Committed => "receipt_completed",
                Some(ref original) if original.id == receipt.id => "reused",
                Some(_) => "resumed",
            }, "operation":receipt.id,
            "session_id":projection.get("session_id"),"generation":projection.get("generation"),
            "session_observation":projection.get("session_observation"),"warning":projection.get("session_warning"),
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

/// What remove/reset may rely on. Idle needs positive evidence; anything the
/// framework cannot prove idle is never treated as idle.
#[derive(Debug, PartialEq, Eq)]
enum Activity {
    Stopped,
    Exited,
    Idle,
    Working(String),
    Unverified(String),
}

// Two captures this far apart straddle a one-second elapsed counter, and a
// streamed reply that hides a provider's busy marker still changes the frame.
const SETTLE: Duration = Duration::from_millis(1500);
const REMOVE_WORKING: &str = "agent is currently working; stop the agent before removing it";
const RESET_WORKING: &str =
    "agent is currently working; wait for completion or stop the agent before resetting session";

/// Read-only: a held seat lease, then the seat's own pane read twice, then
/// the lease and queue again so a delivery between the captures still counts.
fn activity(workspace: &Path, team: &str, id: &SeatId) -> Result<Activity, CliError> {
    let backend = Backend::open_readonly(workspace, team).map_err(error)?;
    let busy = || Activity::Working("a lifecycle operation or message delivery holds the seat".into());
    // An interrupted startup holds the lease before its seat row is published.
    if backend.store.leased(id).map_err(error)? {
        return Ok(busy());
    }
    let Some(seat) = backend.store.seat(id).map_err(error)? else {
        return Ok(Activity::Stopped);
    };
    if seat.status == SeatStatus::Stopped {
        return Ok(Activity::Stopped);
    }
    let Some(target) = seat.physical.as_ref() else {
        return Ok(Activity::Unverified("no native target is recorded".into()));
    };
    match sample_process(&target.process) {
        ProcessState::Alive => {}
        ProcessState::Exited | ProcessState::Replaced => return Ok(Activity::Exited),
        ProcessState::Unknown(failure) => {
            return Ok(Activity::Unverified(format!("native process state is unknown: {failure}")))
        }
    }
    // Starting stays until the first delivery; only the surface decides then.
    if seat.status == SeatStatus::Unknown {
        return Ok(Activity::Unverified("native seat is quarantined after an uncertain native effect".into()));
    }
    let selected = registry::provider(seat.provider.as_str()).map_err(error)?;
    let adapter = backend::adapter(&backend.binding, &seat.identity, &seat.provider).map_err(error)?;
    let hooks = adapter.hooks();
    let clock = RealClock::new();
    let nonce = backend.store.propose_identity(id).map_err(error)?.instance;
    let physical = PhysicalRuntime::new(
        Registration {
            descriptor: selected.descriptor(),
            hooks: &hooks,
            controls: selected.controls(),
            startup: selected.startup(),
            session_panel: selected.session_panel(),
        },
        backend::settings(&backend.binding, selected),
        OperationId::new(format!("observe-{}", nonce.as_str())).map_err(error)?,
        &clock,
    ).map_err(error)?;
    let capture = || physical.surface(&seat)
        .map_err(|failure| Activity::Unverified(format!("native surface capture failed: {failure}")));
    let first = match capture() {
        Ok(sample) => sample,
        Err(unverified) => return Ok(unverified),
    };
    std::thread::sleep(SETTLE);
    let observed = match capture() {
        Ok(second) => classify(&first, &second),
        Err(unverified) => unverified,
    };
    if observed == Activity::Idle {
        if backend.store.leased(id).map_err(error)? {
            return Ok(busy());
        }
        if backend.store.has_queued_for(&seat.identity).map_err(error)? {
            return Ok(Activity::Working("a queued message is about to be delivered".into()));
        }
    }
    Ok(observed)
}

fn classify(
    first: &(Probe<PaneReadyEvidence>, Digest),
    second: &(Probe<PaneReadyEvidence>, Digest),
) -> Activity {
    let read = |sample: &(Probe<PaneReadyEvidence>, Digest)| match &sample.0.outcome {
        ProbeOutcome::Observed(pane) => Some(pane.surface),
        _ => None,
    };
    let (before, after) = (read(first), read(second));
    if let Some(busy) = [before, after].into_iter().flatten().find(|surface| matches!(surface,
        InputSurface::Busy | InputSurface::Queued | InputSurface::ComposerContainsPaste | InputSurface::NativeAccepted))
    {
        return Activity::Working(format!("native surface reads {busy:?}"));
    }
    match (before, after) {
        (Some(InputSurface::ComposerReady), Some(InputSurface::ComposerReady)) if first.1 == second.1 => Activity::Idle,
        (Some(InputSurface::ComposerReady), Some(InputSurface::ComposerReady)) => {
            Activity::Working("native output changed between two captures".into())
        }
        _ => Activity::Unverified(format!("native surface reads {before:?} then {after:?}")),
    }
}

fn refused(agent: &str, reason: &str, error: &str, evidence: Option<String>, action: &str) -> Value {
    json!({"ok":false,"agent_id":agent,"status":"refused","runtime":"contract","reason":reason,
        "error":error,"evidence":evidence,"action":action})
}

fn stop_seat(selected: &SelectedTeam, id: &SeatId) -> Result<OperationId, CliError> {
    let receipt = backend::stop(&selected.run_workspace, &selected.team_key, id).map_err(error)?;
    check_record(&receipt)?;
    Ok(receipt.id)
}

/// A working seat is never removed, even with --force. A stopped seat needs
/// only --confirm; an idle live seat also needs --force and is stopped first.
pub fn remove_agent(
    workspace: &Path,
    team: Option<&str>,
    agent: &str,
    from_spec: bool,
    confirm: bool,
    force: bool,
) -> Result<Option<Value>, CliError> {
    with_agent(workspace, team, agent, "remove-agent", |selected, id| {
        if !confirm {
            return Ok(refused(agent, "confirm_required", "remove-agent requires --confirm", None, "rerun with --confirm"));
        }
        let row = selected.state.get("agents").and_then(|agents| agents.get(agent))
            .ok_or(BackendError::Metadata).map_err(error)?;
        let agent_id = AgentId::new(agent);
        // Every shared-record input resolves before the native seat is touched.
        crate::lifecycle::restart::remove::preflight_contract_removal(
            &selected.run_workspace, &agent_id, Some(&selected.team_key),
        ).map_err(error)?;
        let mut stopped = None;
        if !framework::is_paused_unstarted(row) {
            match activity(&selected.run_workspace, &selected.team_key, id)? {
                Activity::Stopped => {}
                Activity::Working(evidence) => return Ok(refused(agent, "agent_working", REMOVE_WORKING,
                    Some(evidence), "wait for the agent to finish, or stop it, then rerun remove-agent")),
                Activity::Unverified(evidence) => return Ok(refused(agent, "agent_activity_unverified",
                    "agent activity could not be verified idle; stop the agent before removing it",
                    Some(evidence), "stop the agent, then rerun remove-agent")),
                Activity::Idle if !force => return Ok(refused(agent, "force_required",
                    "agent is running; remove-agent requires --force", None,
                    "rerun with --force to stop and remove the running agent")),
                Activity::Idle | Activity::Exited => stopped = Some(stop_seat(selected, id)?),
            }
            // The native identity ends before the shared records: a failure
            // below leaves a row a rerun removes, never a resumable old seat.
            backend::retire(&selected.run_workspace, &selected.team_key, id).map_err(error)?;
        }
        let report = crate::lifecycle::restart::remove::remove_contract_records_locked(
            &selected.run_workspace, &agent_id, from_spec, force, stopped.is_some(),
            Some(&selected.team_key),
        ).map_err(error)?;
        Ok(json!({"ok":true,"agent_id":agent,"status":"removed","runtime":"contract",
            "stopped":stopped.is_some(),"operation":stopped,"report":format!("{report:?}")}))
    })
}

/// Ok: the seat is stopped, exited or idle; Err: the reset refusal receipt.
fn reset_guard(observed: Activity, agent: &str) -> Result<Activity, Value> {
    match observed {
        Activity::Working(evidence) => Err(refused(agent, "agent_working", RESET_WORKING,
            Some(evidence), "wait for the agent to finish, or stop it, then rerun reset-agent")),
        Activity::Unverified(evidence) => Err(refused(agent, "agent_activity_unverified",
            "agent activity could not be verified idle; wait for completion or stop the agent before resetting session",
            Some(evidence), "stop the agent, then rerun reset-agent")),
        clear => Ok(clear),
    }
}

/// Discards the native session only when the seat is provably not working,
/// then starts the same seat ID on a fresh generation and session.
pub fn reset_agent(
    workspace: &Path,
    team: Option<&str>,
    agent: &str,
    discard_session: bool,
) -> Result<Option<Value>, CliError> {
    with_agent(workspace, team, agent, "reset-agent", |selected, id| {
        if !discard_session {
            return Ok(json!({"ok":false,"agent_id":agent,"status":"refused","runtime":"contract",
                "reason":"discard_session_required"}));
        }
        let row = selected.state.get("agents").and_then(|agents| agents.get(agent))
            .ok_or(BackendError::Metadata).map_err(error)?;
        let config = framework::team_config(&selected.run_workspace, &selected.team_dir, &selected.team_key, Some(agent))
            .map_err(error)?.ok_or_else(|| error("contract role no longer configured"))?;
        let role = config.roles.iter().find(|role| &role.id == id)
            .ok_or_else(|| error("contract role no longer configured"))?;
        if !registry::same_provider(row.get("provider").and_then(Value::as_str).unwrap_or(""), role.provider.as_str()) {
            return Err(error("provider change requires a new seat identity"));
        }
        if role.paused {
            return Ok(json!({"ok":false,"agent_id":agent,"status":"paused","reason":"agent_paused","runtime":"contract"}));
        }
        let live = !framework::is_paused_unstarted(row);
        // The guard runs when the command is invoked, before the slow native
        // admission below: a message sent just before reset is still queued or
        // in flight then. It runs again right before the stop, so work that
        // began during admission is refused too.
        if live {
            if let Err(refusal) = reset_guard(activity(&selected.run_workspace, &selected.team_key, id)?, agent) {
                return Ok(refusal);
            }
        }
        // Pure admission of the replacement precedes any stop or retirement.
        let mut admission = config.clone();
        admission.roles.retain(|candidate| &candidate.id == id);
        backend::preflight(&admission).map_err(error)?;
        let mut stopped = None;
        let mut discarded = None;
        if live {
            // The fresh start below must not fail its candidate fence after the
            // old seat is already gone.
            backend::refresh_quiescent_candidate(&selected.run_workspace, &selected.team_key, id).map_err(error)?;
            match reset_guard(activity(&selected.run_workspace, &selected.team_key, id)?, agent) {
                Err(refusal) => return Ok(refusal),
                Ok(Activity::Stopped) => {}
                Ok(_) => stopped = Some(stop_seat(selected, id)?),
            }
            discarded = backend::retire(&selected.run_workspace, &selected.team_key, id).map_err(error)?
                .and_then(|seat| seat.session).map(|session| session.native_session);
        }
        // The row stops naming the retired incarnation. If the fresh start
        // below fails, this unbound stopped seed is skipped by team restart
        // and admitted fresh by a later start.
        StateRepository::new(&selected.run_workspace).commit(
            StateWriteIntent::StartAgent { team_key: &selected.team_key, agent_id: agent },
            |state| {
                if let Some(row) = state.get_mut("agents").and_then(|agents| agents.get_mut(agent)).and_then(Value::as_object_mut) {
                    for key in ["contract_scope", "contract_instance", "generation", "session_id"] {
                        row.insert(key.into(), Value::Null);
                    }
                    row.insert("status".into(), json!("stopped"));
                }
            },
        ).map_err(error)?;
        let projection = framework::start_configured(&selected.run_workspace, &selected.team_dir, &selected.team_key, id)?;
        let coordinator = crate::coordinator::start_coordinator_with_team(
            &crate::coordinator::WorkspacePath::new(selected.run_workspace.clone()), Some(&selected.team_key),
        ).map_err(error)?;
        Ok(json!({"ok":coordinator.ok,"agent_id":agent,"status":"reset","runtime":"contract",
            "start_mode":"fresh","stopped":stopped.is_some(),"operation":stopped,
            "discarded_session_id":discarded,"session_id":projection.get("session_id"),
            "new_session_id":projection.get("session_id"),"generation":projection.get("generation"),
            "coordinator":crate::lifecycle::coordinator_start_summary_value(
                &crate::lifecycle::CoordinatorStartSummary::from_start_report(&coordinator))}))
    })
}

#[cfg(test)]
mod tests {
    // Assembly guard, not a substitute for the real shared-session restart/
    // shutdown RED→GREEN journey. All start callers must use this common path.
    #[test]
    fn shared_owner_claim_follows_common_projection_commit_including_team_restart() {
        let source = include_str!("commands.rs");
        let body = source.split_once("pub(super) fn publish_at").unwrap().1
            .split_once("fn check_record").unwrap().0;
        let persisted = body.find("StateRepository::new(workspace).commit").unwrap();
        let claimed = body.find("backend::bind_workspace_session(workspace, team, id)").unwrap();
        let returned = body.find("Ok(projected)").unwrap();
        assert!(persisted < claimed && claimed < returned);
        let restart = source.split_once("pub(crate) fn restart_prepared").unwrap().1
            .split_once("pub fn restart(").unwrap().0;
        assert!(restart.contains("publish_at(workspace, team, &id, false)"));
        assert!(!include_str!("framework.rs").contains("backend::bind_workspace_session"));
    }

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

    fn surface(surface: Option<InputSurface>, text: &[u8]) -> (Probe<PaneReadyEvidence>, Digest) {
        let scope = team_agent_contract::contract::probe::EvidenceScope {
            identity: receipt(Outcome::Committed).target.identity, endpoint: "fixture".into(),
            pane: "%0".into(), binding: "fixture".into(), session: None, sequence: 1,
            observed_at: Duration::ZERO, valid_for: Duration::from_secs(2), source: "fixture".into(),
        };
        let outcome = match surface {
            Some(surface) => ProbeOutcome::Observed(PaneReadyEvidence { profile_id: "fixture".into(), surface }),
            None => ProbeOutcome::Pending { deadline: Duration::ZERO },
        };
        (Probe { scope, outcome }, team_agent_contract::host::digest(text))
    }

    #[test]
    fn working_guard_calls_a_seat_idle_only_on_two_still_composer_ready_captures() {
        let ready = surface(Some(InputSurface::ComposerReady), b"idle");
        assert_eq!(classify(&ready, &ready.clone()), Activity::Idle);
        // A streamed reply can hide the busy marker, but it still moves the frame.
        assert!(matches!(classify(&ready, &surface(Some(InputSurface::ComposerReady), b"idle, then more")),
            Activity::Working(_)));
        for busy in [InputSurface::Busy, InputSurface::Queued, InputSurface::ComposerContainsPaste,
            InputSurface::NativeAccepted]
        {
            assert!(matches!(classify(&ready, &surface(Some(busy), b"idle")), Activity::Working(_)), "{busy:?}");
            assert!(matches!(classify(&surface(Some(busy), b"x"), &surface(None, b"x")), Activity::Working(_)),
                "{busy:?}");
        }
        for other in [None, Some(InputSurface::Starting), Some(InputSurface::ShellOrUnknown),
            Some(InputSurface::StartupRiskWarning), Some(InputSurface::Fault)]
        {
            assert!(matches!(classify(&ready, &surface(other, b"idle")), Activity::Unverified(_)), "{other:?}");
            assert!(matches!(classify(&surface(other, b"idle"), &ready), Activity::Unverified(_)), "{other:?}");
        }
    }

    // 2.8a: a reset issued right after a send must observe the queued or
    // in-flight message, not a turn that the slow admission let finish.
    #[test]
    fn reset_guards_the_seat_before_its_slow_admission_and_again_before_the_stop() {
        let source = include_str!("commands.rs");
        let body = source.split_once("pub fn reset_agent(").unwrap().1
            .split_once("#[cfg(test)]").unwrap().0;
        let first = body.find("reset_guard(activity(").unwrap();
        let admission = body.find("backend::preflight(&admission)").unwrap();
        let second = body.rfind("reset_guard(activity(").unwrap();
        let stop = body.find("stop_seat(selected, id)").unwrap();
        assert!(first < admission && admission < second && second < stop);
    }

    #[test]
    fn remove_and_reset_route_contract_seats_before_the_legacy_refusal() {
        let cli = include_str!("../cli/mod.rs");
        for (entry, native, gate) in [
            ("pub fn remove_agent(", "contract_runtime::commands::remove_agent(", "\"remove-agent\","),
            ("pub fn reset_agent(", "contract_runtime::commands::reset_agent(", "\"reset-agent\","),
        ] {
            let body = cli.split_once(entry).unwrap().1;
            assert!(body.find(native).unwrap() < body.find(gate).unwrap(), "{entry}");
        }
    }
}
