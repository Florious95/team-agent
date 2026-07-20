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
        }));
    }
    if matches!(
        start.status,
        crate::coordinator::StartOutcome::Started
            | crate::coordinator::StartOutcome::StartedAfterRotation
    ) {
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
