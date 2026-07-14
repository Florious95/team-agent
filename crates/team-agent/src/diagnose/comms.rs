use std::path::Path;

use serde_json::{json, Value};

use crate::cli::{CliError, COMMS_BOUNDARY_TEXT};
use crate::messaging::{
    run_comms_selftest, CheckEvidence, CheckStatus, CommsSelftestDriver, ProviderSdkCalls,
    SelftestCheck, SelftestReport,
};
use crate::model::ids::TeamKey;
use crate::transport::Transport;

pub fn doctor_comms_json(
    workspace: &Path,
    team: Option<&str>,
    gate: Option<&str>,
) -> Result<Value, CliError> {
    let _ = (team, gate);
    let team_key = team.map(TeamKey::new);
    let report = comms_selftest_report(workspace, team_key.as_ref())
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(report_json(&report))
}

pub fn comms_selftest_report(
    workspace: &Path,
    team: Option<&TeamKey>,
) -> Result<SelftestReport, crate::messaging::MessagingError> {
    run_comms_selftest(workspace, team, &RuntimeCommsDriver)
}

pub fn failing_check_names(report: &SelftestReport) -> Vec<&'static str> {
    let mut names = Vec::new();
    if report.receiver_binding.status != CheckStatus::Pass {
        names.push("receiver_binding");
    }
    if report.contract_suite.status != CheckStatus::Pass {
        names.push("contract_suite");
    }
    if report.provider_sdk_calls.status != CheckStatus::Pass {
        names.push("provider_sdk_calls");
    }
    names
}

struct RuntimeCommsDriver;

impl CommsSelftestDriver for RuntimeCommsDriver {
    fn run_id(&self) -> Option<String> {
        None
    }

    fn provider_sdk_calls(&self) -> ProviderSdkCalls {
        ProviderSdkCalls::default()
    }

    fn receiver_binding(&self, workspace: &Path, team: Option<&TeamKey>) -> Value {
        receiver_binding_snapshot(workspace, team)
    }
}

fn receiver_binding_snapshot(workspace: &Path, team: Option<&TeamKey>) -> Value {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team.map(TeamKey::as_str),
        crate::state::selector::SelectorMode::RuntimeOnly,
    );
    let Ok(selected) = selected else {
        let reason = selected.err().map(|e| e.to_string()).unwrap_or_default();
        return json!({
            "status": "fail",
            "verifies": "binding_consistency",
            "proof": "state_read",
            "state_read_observed": false,
            "pane_id": Value::Null,
            "owner_pane_id": Value::Null,
            "caller_pane_id": std::env::var("TMUX_PANE").ok().map(Value::String).unwrap_or(Value::Null),
            "mismatches": ["runtime_state_unresolved"],
            "reason": reason,
            "configured": false,
        });
    };
    let state = selected.state;
    let receiver = state.get("leader_receiver").and_then(Value::as_object);
    // Stage 2 (identity-boundary unified plan, architect direction
    // 2026-06-23): route owner pane lookup through the ownership repository.
    // `selected.state` is already team-projected by the selector, so the
    // empty-team-key path returns the same top-level owner the legacy direct
    // read produced — but Stage 5 will swap the data source under the
    // repository and this diagnose surface follows automatically. The
    // `owner` key fallback is retained for callers that pre-date the
    // `team_owner` rename.
    let owner_pane_id = state
        .get("owner")
        .cloned()
        .or_else(|| crate::state::ownership::read_owner_value(&state, "").cloned())
        .and_then(|v| v.get("pane_id").cloned())
        .unwrap_or(Value::Null);
    let caller_pane_id = std::env::var("TMUX_PANE")
        .ok()
        .map(Value::String)
        .unwrap_or(Value::Null);
    let pane_id = receiver
        .and_then(|r| r.get("pane_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let tmux_socket = receiver
        .and_then(|r| r.get("tmux_socket"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let mut mismatches = receiver_binding_mismatches(&owner_pane_id, &caller_pane_id, &pane_id);
    if !crate::state::persist::runtime_state_path(&selected.run_workspace).exists() {
        mismatches.push(json!("runtime_state_missing"));
    }
    if receiver.is_none() {
        mismatches.push(json!("leader_receiver_missing"));
    }
    if pane_id.as_str().filter(|s| !s.is_empty()).is_none() {
        mismatches.push(json!("leader_receiver_pane_missing"));
    }
    if let (Some(socket), Some(pane)) = (tmux_socket.as_deref(), pane_id.as_str()) {
        if receiver_pane_stale(socket, pane) {
            mismatches.push(json!("receiver_pane_stale"));
        }
    }
    json!({
        "status": if mismatches.is_empty() { "pass" } else { "fail" },
        "verifies": "binding_consistency",
        "proof": "state_read",
        "state_read_observed": true,
        "team_key": selected.team_key,
        "workspace": selected.run_workspace.to_string_lossy().to_string(),
        "pane_id": pane_id,
        "owner_pane_id": owner_pane_id,
        "caller_pane_id": caller_pane_id,
        "tmux_socket": tmux_socket,
        "mismatches": mismatches,
        "configured": receiver.is_some(),
    })
}

fn receiver_pane_stale(socket: &str, pane_id: &str) -> bool {
    crate::tmux_backend::TmuxBackend::for_tmux_endpoint(socket)
        .list_targets()
        .map(|targets| {
            !targets
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
        })
        .unwrap_or(true)
}

fn report_json(report: &SelftestReport) -> Value {
    json!({
        "ok": report.ok,
        "status": status_json(report.status),
        "run_id": report.run_id,
        "scope": report.scope,
        "boundary": COMMS_BOUNDARY_TEXT,
        "checks": {
            "receiver_binding": check_json(&report.receiver_binding),
            "contract_suite": check_json(&report.contract_suite),
            "provider_sdk_calls": check_json(&report.provider_sdk_calls),
        },
    })
}

fn check_json(check: &SelftestCheck) -> Value {
    let mut value = serde_json::Map::new();
    value.insert("status".to_string(), status_json(check.status));
    value.insert("verifies".to_string(), verifies_json(check));
    match &check.evidence {
        CheckEvidence::ProviderSdkCalls(calls) => {
            value.insert("calls".to_string(), json!(calls));
        }
        CheckEvidence::Binding { details, .. } => {
            if let Some(obj) = details.as_object() {
                for (key, detail) in obj {
                    value.entry(key.clone()).or_insert_with(|| detail.clone());
                }
            }
        }
        CheckEvidence::ContractSuite { checks } => {
            let checks_json: Vec<Value> = checks
                .iter()
                .map(|check| {
                    json!({
                        "name": check.name,
                        "status": status_json(check.status),
                        "reason": check.reason,
                    })
                })
                .collect();
            let failed: Vec<&str> = checks
                .iter()
                .filter(|check| check.status != CheckStatus::Pass)
                .map(|check| check.name.as_str())
                .collect();
            value.insert("checks".to_string(), Value::Array(checks_json));
            value.insert("failed".to_string(), json!(failed));
        }
    }
    Value::Object(value)
}

fn status_json(status: CheckStatus) -> Value {
    serde_json::to_value(status).unwrap_or_else(|_| json!("fail"))
}

fn verifies_json(check: &SelftestCheck) -> Value {
    serde_json::to_value(check.verifies).unwrap_or_else(|_| json!("unknown"))
}

pub fn receiver_binding_mismatches(
    owner_pane_id: &Value,
    caller_pane_id: &Value,
    pane_id: &Value,
) -> Vec<Value> {
    let mut mismatches = Vec::new();
    if pane_mismatch(owner_pane_id, pane_id) {
        mismatches.push(json!("owner_receiver_pane_mismatch"));
    }
    if pane_mismatch(caller_pane_id, owner_pane_id) {
        mismatches.push(json!("caller_owner_pane_mismatch"));
    }
    if pane_mismatch(caller_pane_id, pane_id) {
        mismatches.push(json!("caller_receiver_pane_mismatch"));
    }
    mismatches
}

fn pane_mismatch(left: &Value, right: &Value) -> bool {
    let Some(left) = left.as_str().filter(|s| !s.is_empty()) else {
        return false;
    };
    let Some(right) = right.as_str().filter(|s| !s.is_empty()) else {
        return false;
    };
    left != right
}
