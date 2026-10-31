//! diagnose/comms.py — comms selftest (零 token / 零 provider SDK;card §31/§75)。

use std::path::Path;

use crate::message_store::MessageStore;
use crate::model::ids::TeamKey;

use super::helpers::next_run_id;
use super::{
    CheckEvidence, CheckKind, CheckStatus, ContractSuiteCheck, IdleEvaluation, MessagingError,
    ProviderSdkCalls, SelftestCheck, SelftestReport,
};

/// selftest driver (`diagnose/comms.py` `CommsSelftestDriver`):**零 token / 零 provider SDK**
/// 注入面 (§84/MUST-NOT-13)。trait mock + 断言调用计数 = 0。
pub trait CommsSelftestDriver {
    /// 此 run 的稳定 id (None → 随机 hex[:12])。
    fn run_id(&self) -> Option<String>;
    /// provider SDK 调用计数 (机械门:必须全 0)。
    fn provider_sdk_calls(&self) -> ProviderSdkCalls;
    /// receiver binding 一致性快照 (供 `_receiver_binding_check`)。
    fn receiver_binding(&self, workspace: &Path, team: Option<&TeamKey>) -> serde_json::Value;
}

/// `run_comms_selftest` (`diagnose/comms.py:21`):binding 一致性 + **零 provider SDK 调用断言**。
/// §10/§84 机械门 selftest 落点 —— **零 token,零 provider SDK 调用**,走 [`CommsSelftestDriver`]
/// trait mock。CLI `selftest` + 诊断调。
pub fn run_comms_selftest(
    workspace: &Path,
    team: Option<&TeamKey>,
    driver: &dyn CommsSelftestDriver,
) -> Result<SelftestReport, MessagingError> {
    let run_id = driver.run_id().unwrap_or_else(next_run_id);
    let binding = driver.receiver_binding(workspace, team);
    let mismatches = binding
        .get("mismatches")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let receiver_status = if mismatches.is_empty() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let calls = driver.provider_sdk_calls();
    let provider_status = if calls.is_zero() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let contract_checks = run_contract_suite(workspace, team, &run_id);
    let contract_status = if contract_checks
        .iter()
        .all(|check| check.status == CheckStatus::Pass)
    {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let receiver_binding = SelftestCheck {
        status: receiver_status,
        verifies: CheckKind::ReceiverBinding,
        evidence: CheckEvidence::Binding {
            mismatches,
            details: binding,
        },
    };
    let contract_suite = SelftestCheck {
        status: contract_status,
        verifies: CheckKind::ContractSuite,
        evidence: CheckEvidence::ContractSuite {
            checks: contract_checks,
        },
    };
    let provider_sdk_calls = SelftestCheck {
        status: provider_status,
        verifies: CheckKind::NoProviderSdkCalls,
        evidence: CheckEvidence::ProviderSdkCalls(calls),
    };
    let ok = receiver_binding.status == CheckStatus::Pass
        && contract_suite.status == CheckStatus::Pass
        && provider_sdk_calls.status == CheckStatus::Pass;
    Ok(SelftestReport {
        ok,
        status: if ok {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        run_id,
        scope: "binding_consistency".to_string(),
        boundary: "messaging".to_string(),
        receiver_binding,
        contract_suite,
        provider_sdk_calls,
    })
}

fn run_contract_suite(
    workspace: &Path,
    team: Option<&TeamKey>,
    run_id: &str,
) -> Vec<ContractSuiteCheck> {
    let scratch = std::env::temp_dir().join(format!("ta-comms-contract-{run_id}-{}", std::process::id()));
    let mut checks = Vec::new();
    let probe = (|| -> Result<(), String> {
        let record = crate::inbox_store::record("doctor", "worker", "C3 probe", Some("contract-a"), "message");
        crate::inbox_store::store(&scratch, "worker", &record)?;
        crate::inbox_store::store(&scratch, "worker", &record)?;
        let own = crate::inbox_store::read(&scratch, "worker", 10, Some("contract-a"))?;
        let other = crate::inbox_store::read(&scratch, "worker", 10, Some("contract-b"))?;
        if own["messages"].as_array().map(Vec::len) != Some(1)
            || other["messages"].as_array().map(Vec::len) != Some(0) {
            return Err("C3 idempotency or team isolation failed".into());
        }
        let mut conflict = record;
        conflict["content"] = serde_json::json!("conflicting body");
        if crate::inbox_store::store(&scratch, "worker", &conflict).is_ok() {
            return Err("C3 accepted a conflicting message id".into());
        }
        Ok(())
    })();
    checks.push(contract_check("c3_scope_and_idempotency",
        if probe.is_ok() { CheckStatus::Pass } else { CheckStatus::Fail }, probe.err()));
    let result_id = format!("res-comms-{run_id}");
    let rendered = super::watchers::format_result_watcher_notification(&serde_json::json!({
        "result_id": result_id, "task_id": "comms-contract", "agent_id": "doctor",
        "status": "success", "summary": "contract suite probe"
    }));
    let valid = super::watchers::result_id_from_text(&rendered).as_deref() == Some(result_id.as_str());
    checks.push(contract_check("result_notification_render",
        if valid { CheckStatus::Pass } else { CheckStatus::Fail },
        (!valid).then(|| "result notification did not round-trip result_id".into())));
    let _ = std::fs::remove_dir_all(&scratch);
    checks
}

fn contract_check(name: &str, status: CheckStatus, reason: Option<String>) -> ContractSuiteCheck {
    ContractSuiteCheck {
        name: name.to_string(),
        status,
        reason,
    }
}

fn message_owner_team(db_path: &Path, message_id: &str) -> Result<Option<String>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

/// `evaluate_idle_behavior` (`diagnose/comms.py:50`):idle 分类准确性评估。零 token,走 driver。
pub(crate) fn evaluate_idle_behavior(
    workspace: &Path,
    agent_id: &str,
    claimed_status: &str,
    token: Option<&str>,
    driver: &dyn CommsSelftestDriver,
) -> Result<IdleEvaluation, MessagingError> {
    let _ = (workspace, driver);
    let normalized = claimed_status.to_ascii_lowercase();
    let status = match normalized.as_str() {
        "idle" | "working" | "running" => CheckStatus::NotChallenged,
        _ => CheckStatus::Fail,
    };
    Ok(IdleEvaluation {
        ok: status == CheckStatus::NotChallenged,
        agent_id: agent_id.to_string(),
        claimed_status: claimed_status.to_string(),
        token: token.unwrap_or("").to_string(),
        status,
        execution_ack: "not_challenged".to_string(),
    })
}
