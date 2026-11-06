//! Result types for current communication diagnostics.

use serde::{Deserialize, Serialize};



/// selftest check 状态 (card §47;`diagnose/comms.py`)。§19 必变 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Deferred,
    NotImplemented,
    NotChallenged,
}

/// selftest check 验证项 (`verifies` 字段;`diagnose/comms.py:149`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    ReceiverBinding,
    ContractSuite,
    /// 机械门 (§84/MUST-NOT-13):`{anthropic,openai,httpx} == 0`。
    NoProviderSdkCalls,
}

/// selftest 单 check (card §47;`diagnose/comms.py:119`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelftestCheck {
    pub status: CheckStatus,
    pub verifies: CheckKind,
    /// 证据 (proof/calls 等;leader 集成时按 CheckKind 收口具体形状)。
    pub evidence: CheckEvidence,
}

/// selftest check 证据 (`proof`/`calls`/`mismatches` 的 typed union)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckEvidence {
    /// `no_provider_sdk_calls` 的机械证据 (§84):三 SDK 调用计数。
    ProviderSdkCalls(ProviderSdkCalls),
    /// binding 一致性比对结果 (mismatch 列表)。
    Binding {
        mismatches: Vec<String>,
        details: serde_json::Value,
    },
    /// executable zero-token comms contract suite evidence.
    ContractSuite { checks: Vec<ContractSuiteCheck> },
}

/// One executable zero-token comms contract-suite subcheck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractSuiteCheck {
    pub name: String,
    pub status: CheckStatus,
    pub reason: Option<String>,
}

/// **机械门** (§84/MUST-NOT-13;`diagnose/comms.py:142`):selftest 路径 provider SDK 调用
/// 计数,断言 `{anthropic,openai,httpx} == 0`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderSdkCalls {
    pub anthropic: u32,
    pub openai: u32,
    pub httpx: u32,
}

impl ProviderSdkCalls {
    /// `any(calls.values())` 取反:三者全 0 才 pass。
    pub fn is_zero(self) -> bool {
        self.anthropic == 0 && self.openai == 0 && self.httpx == 0
    }
}

/// comms selftest 顶层结果 (`run_comms_selftest` 返回;`diagnose/comms.py:40`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelftestReport {
    pub ok: bool,
    pub status: CheckStatus,
    pub run_id: String,
    /// `scope = "binding_consistency"`。
    pub scope: String,
    pub boundary: String,
    pub receiver_binding: SelftestCheck,
    pub contract_suite: SelftestCheck,
    pub provider_sdk_calls: SelftestCheck,
}
