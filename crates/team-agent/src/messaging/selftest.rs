//! diagnose/comms.py — comms selftest (零 token / 零 provider SDK;card §31/§75)。

use std::path::Path;

use crate::model::ids::TeamKey;

use super::helpers::next_run_id;
use super::{
    CheckEvidence, CheckKind, CheckStatus, IdleEvaluation, MessagingError, ProviderSdkCalls,
    SelftestCheck, SelftestReport,
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
    let receiver_status = if mismatches.is_empty() { CheckStatus::Pass } else { CheckStatus::Fail };
    let calls = driver.provider_sdk_calls();
    let provider_status = if calls.is_zero() { CheckStatus::Pass } else { CheckStatus::Fail };
    let run_id = driver.run_id().unwrap_or_else(next_run_id);
    let receiver_binding = SelftestCheck {
        status: receiver_status,
        verifies: CheckKind::ReceiverBinding,
        evidence: CheckEvidence::Binding { mismatches },
    };
    let contract_suite = SelftestCheck {
        status: CheckStatus::Deferred,
        verifies: CheckKind::ContractSuite,
        evidence: CheckEvidence::Deferred { reason: "contract_suite_not_shipped".to_string() },
    };
    let provider_sdk_calls = SelftestCheck {
        status: provider_status,
        verifies: CheckKind::NoProviderSdkCalls,
        evidence: CheckEvidence::ProviderSdkCalls(calls),
    };
    Ok(SelftestReport {
        ok: receiver_binding.status == CheckStatus::Pass && provider_sdk_calls.status == CheckStatus::Pass,
        status: if receiver_binding.status == CheckStatus::Pass && provider_sdk_calls.status == CheckStatus::Pass {
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

/// `evaluate_idle_behavior` (`diagnose/comms.py:50`):idle 分类准确性评估。零 token,走 driver。
pub fn evaluate_idle_behavior(
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
