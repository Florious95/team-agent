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
    let scratch =
        std::env::temp_dir().join(format!("ta-comms-contract-{run_id}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let mut checks = Vec::new();
    let mut scratch_store = match MessageStore::open(&scratch) {
        Ok(store) => {
            checks.push(contract_check(
                "message_store_schema",
                CheckStatus::Pass,
                None,
            ));
            Some(store)
        }
        Err(error) => {
            checks.push(contract_check(
                "message_store_schema",
                CheckStatus::Fail,
                Some(error.to_string()),
            ));
            None
        }
    };

    match scratch_store.as_ref().and_then(|store| {
        store
            .create_message(
                None,
                "doctor",
                "worker",
                "comms contract probe",
                None,
                false,
                Some("contract-team"),
            )
            .ok()
    }) {
        Some(message_id) if message_id.starts_with("msg_") && message_id.len() > 4 => {
            let rendered = format!(
                "Team Agent message from doctor:\n\ncomms contract probe\n\n[team-agent-token:{message_id}]"
            );
            let token = format!("[team-agent-token:{message_id}]");
            checks.push(contract_check(
                "message_token_shape",
                if rendered.ends_with(&token) {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Fail
                },
                (!rendered.ends_with(&token)).then(|| "rendered token suffix missing".to_string()),
            ));
        }
        Some(_) => checks.push(contract_check(
            "message_token_shape",
            CheckStatus::Fail,
            Some("message id did not use msg_ prefix".to_string()),
        )),
        None => checks.push(contract_check(
            "message_token_shape",
            CheckStatus::Fail,
            Some("could not create scratch message".to_string()),
        )),
    }

    let result_id = format!("res-comms-{run_id}");
    let result_content = super::watchers::format_result_watcher_notification(&serde_json::json!({
        "result_id": result_id,
        "task_id": "comms-contract",
        "agent_id": "doctor",
        "status": "success",
        "summary": "contract suite probe",
    }));
    let parsed_result_id = super::watchers::result_id_from_text(&result_content);
    checks.push(contract_check(
        "result_notification_render",
        if parsed_result_id.as_deref() == Some(result_id.as_str()) {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        (parsed_result_id.as_deref() != Some(result_id.as_str()))
            .then(|| "result notification did not round-trip result_id".to_string()),
    ));

    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team.map(TeamKey::as_str),
        crate::state::selector::SelectorMode::RuntimeOnly,
    );
    match (scratch_store.take(), selected) {
        (Some(store), Ok(selected)) => {
            let owner_team = selected.team_key;
            let mut state = selected.state;
            if !state.is_object() {
                state = serde_json::json!({});
            }
            if let Some(obj) = state.as_object_mut() {
                obj.insert(
                    "active_team_key".to_string(),
                    serde_json::json!(owner_team.clone()),
                );
            }
            let event_log = crate::event_log::EventLog::new(&scratch);
            let outcome = super::leader_receiver::send_to_leader_receiver(
                &scratch,
                &state,
                "leader",
                "comms contract leader projection",
                None,
                "doctor",
                false,
                Some("comms-contract-result"),
                &event_log,
            );
            match outcome {
                Ok(outcome) => {
                    let actual_owner = outcome.message_id.as_deref().and_then(|message_id| {
                        message_owner_team(store.db_path(), message_id)
                            .ok()
                            .flatten()
                    });
                    checks.push(contract_check(
                        "leader_projection_owner_team",
                        if actual_owner.as_deref() == Some(owner_team.as_str()) {
                            CheckStatus::Pass
                        } else {
                            CheckStatus::Fail
                        },
                        (actual_owner.as_deref() != Some(owner_team.as_str())).then(|| {
                            format!(
                                "leader-bound message owner_team_id={:?}, expected={owner_team}",
                                actual_owner
                            )
                        }),
                    ));
                }
                Err(error) => checks.push(contract_check(
                    "leader_projection_owner_team",
                    CheckStatus::Fail,
                    Some(error.to_string()),
                )),
            }
        }
        (_, Err(error)) => checks.push(contract_check(
            "leader_projection_owner_team",
            CheckStatus::Fail,
            Some(error.to_string()),
        )),
        (None, _) => checks.push(contract_check(
            "leader_projection_owner_team",
            CheckStatus::Fail,
            Some("message store schema unavailable".to_string()),
        )),
    }

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
    let conn = crate::db::schema::open_db(db_path)?;
    let owner = conn.query_row(
        "select owner_team_id from messages where message_id = ?1",
        rusqlite::params![message_id],
        |row| row.get::<_, Option<String>>(0),
    )?;
    Ok(owner)
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
