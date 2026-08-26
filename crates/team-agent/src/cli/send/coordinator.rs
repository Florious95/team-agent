//! ---
//! purpose: mutating send 的 coordinator loud-ensure 与 topology 前置门
//! contract:
//!   provides:
//!     - name: loud_ensure_coordinator
//!       what: 仅在 coordinator 通过 bounded stable health window 后报告自动重启
//!     - name: append_loud_ensure_fields
//!       what: 保持 accepted/pending delivery 语义并附加结构化 ensure 证据
//!   depends:
//!     - crate::coordinator::coordinator_health
//!     - crate::coordinator::wait_for_coordinator_health
//!     - crate::messaging
//!   boundary:
//!     - 不把 spawn 成功当作 coordinator ready
//!     - 不把 queued/pending 当作 delivered
//! maturity: wired
//! ---

use super::*;

pub(super) fn dirty_topology_refusal_value(
    selected: &crate::state::selector::SelectedTeam,
    requested_team: Option<&str>,
) -> Option<Value> {
    let issue_ids = crate::topology::restart_dirty_topology_issue_ids(&selected.state);
    if issue_ids.is_empty() {
        return None;
    }
    let session_name = selected
        .state
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reason = issue_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "dirty_topology".to_string());
    let repair_team = requested_team
        .filter(|team| !team.is_empty())
        .unwrap_or(selected.team_key.as_str());
    Some(json!({
        "ok": false,
        "status": "refused_dirty_topology",
        "reason": reason,
        "session_name": session_name,
        "error": "send refused: tmux endpoint/socket topology is inconsistent; run diagnose from the intended leader socket before sending",
        "issues": issue_ids
            .iter()
            .map(|id| json!({"id": id}))
            .collect::<Vec<_>>(),
        "next_actions": [
            "team-agent diagnose --json",
            format!("team-agent claim-leader --team {repair_team} --confirm --json"),
            format!("team-agent takeover --team {repair_team} --confirm --json")
        ],
    }))
}

pub(super) fn target_has_known_worker(state: &Value, target: &MessageTarget, sender: &str) -> bool {
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return false;
    };
    match target {
        MessageTarget::Single(target) => agents.contains_key(target),
        MessageTarget::Broadcast => agents.keys().any(|agent| agent != sender),
        MessageTarget::Fanout(recipients) => recipients
            .iter()
            .any(|recipient| agents.contains_key(recipient)),
    }
}

#[derive(Debug, Clone)]
pub(super) struct LoudEnsureResult {
    previous_status: String,
    start: crate::coordinator::StartReport,
    readiness_timeout: Option<crate::coordinator::HealthReport>,
}

pub(super) fn loud_ensure_coordinator(
    selected: &crate::state::selector::SelectedTeam,
) -> Result<Option<LoudEnsureResult>, CliError> {
    if in_process_unit_test() {
        return Ok(None);
    }
    let workspace = crate::coordinator::WorkspacePath::new(selected.run_workspace.clone());
    let previous = crate::coordinator::coordinator_health(&workspace);
    if previous.ok {
        return Ok(None);
    }
    if previous.service_available
        && matches!(
            previous.binary_identity_relation,
            crate::coordinator::CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller
        )
    {
        return Ok(None);
    }
    let previous_status = coordinator_health_status_wire(previous.status).to_string();
    let start = crate::coordinator::start_coordinator_with_team(
        &workspace,
        Some(selected.team_key.as_str()),
    )
    .map_err(|error| CliError::Runtime(error.to_string()))?;
    if !start.ok {
        return Ok(Some(LoudEnsureResult {
            previous_status,
            start,
            readiness_timeout: None,
        }));
    }
    if matches!(
        start.status,
        crate::coordinator::StartOutcome::Started
            | crate::coordinator::StartOutcome::StartedAfterRotation
    ) {
        let Some(expected_pid) = start.pid else {
            let mut failed_start = start;
            failed_start.ok = false;
            return Ok(Some(LoudEnsureResult {
                previous_status,
                start: failed_start,
                readiness_timeout: None,
            }));
        };
        if let Err(last_health) =
            crate::coordinator::wait_for_coordinator_health(&workspace, expected_pid)
        {
            let mut failed_start = start;
            failed_start.ok = false;
            crate::event_log::EventLog::new(&selected.run_workspace)
                .write(
                    "coordinator.ensure_readiness_timeout",
                    json!({
                        "coordinator_previous_status": previous_status,
                        "expected_pid": expected_pid.get(),
                        "last_health": coordinator_health_json(&last_health, Some(expected_pid)),
                    }),
                )
                .map_err(|error| CliError::Runtime(error.to_string()))?;
            return Ok(Some(LoudEnsureResult {
                previous_status,
                start: failed_start,
                readiness_timeout: Some(last_health),
            }));
        }
        crate::event_log::EventLog::new(&selected.run_workspace)
            .write(
                "coordinator.ensure_restarted",
                json!({
                    "coordinator_previous_status": previous_status,
                    "status": start.status,
                    "pid": start.pid.map(|pid| pid.get()),
                    "previous_pid": start.previous_pid.map(|pid| pid.get()),
                    "binary_path": start.binary_path,
                    "binary_version": start.binary_version,
                    "rotation_reason": start.rotation_reason,
                }),
            )
            .map_err(|error| CliError::Runtime(error.to_string()))?;
        return Ok(Some(LoudEnsureResult {
            previous_status,
            start,
            readiness_timeout: None,
        }));
    }
    Ok(None)
}

#[cfg(test)]
pub(super) fn in_process_unit_test() -> bool {
    true
}

#[cfg(not(test))]
pub(super) fn in_process_unit_test() -> bool {
    false
}

pub(super) fn append_loud_ensure_fields(value: &mut Value, ensure: Option<&LoudEnsureResult>) {
    let Some(ensure) = ensure else {
        return;
    };
    if let Some(last_health) = ensure.readiness_timeout.as_ref() {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "coordinator".to_string(),
                coordinator_start_json(&ensure.start),
            );
            obj.insert(
                "coordinator_readiness".to_string(),
                json!({
                    "ready": false,
                    "last_health": coordinator_health_json(last_health, ensure.start.pid),
                }),
            );
        }
        return;
    }
    if !ensure.start.ok {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("coordinator_auto_restarted".to_string(), json!(true));
        obj.insert(
            "coordinator_previous_status".to_string(),
            json!(ensure.previous_status),
        );
        obj.insert(
            "coordinator".to_string(),
            coordinator_start_json(&ensure.start),
        );
    }
}

fn coordinator_health_json(
    health: &crate::coordinator::HealthReport,
    expected_pid: Option<crate::coordinator::Pid>,
) -> Value {
    json!({
        "ready": health.ok,
        "status": coordinator_health_status_wire(health.status),
        "pid": health.pid.map(|pid| pid.get()),
        "expected_pid": expected_pid.map(|pid| pid.get()),
        "pid_matches_expected": expected_pid.is_none_or(|pid| health.pid == Some(pid)),
        "process_running": health.process_running,
        "metadata_ok": health.metadata_ok,
        "wire_metadata_ok": health.wire_metadata_ok,
        "binary_identity_ok": health.binary_identity_ok,
        "binary_identity_relation": health.binary_identity_relation.as_str(),
        "service_available": health.service_available,
        "metadata_mismatch_reason": health.metadata_mismatch_reason,
        "schema": {
            "ok": health.schema.ok,
            "version": health.schema.schema_version,
            "error": health.schema.error.as_ref().map(|error| format!("{error:?}")),
            "action": health.schema.action,
        },
    })
}

pub(super) fn coordinator_start_json(report: &crate::coordinator::StartReport) -> Value {
    let summary = crate::lifecycle::CoordinatorStartSummary::from_start_report(report);
    crate::lifecycle::coordinator_start_summary_value(&summary)
}

pub(super) fn coordinator_health_status_wire(
    status: crate::coordinator::CoordinatorHealthStatus,
) -> &'static str {
    match status {
        crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
        crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
        crate::coordinator::CoordinatorHealthStatus::Running => "running",
        crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
    }
}
