//! 孤儿自终止判定(Gap 37b)+ provider-neutral 异常轨(abnormal_track:整队消失检测 + 去重通知)。

use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::model::enums::Provider;
use crate::provider::{ProcessLiveness, Signature, TurnId, TurnState};

use super::types::{
    AbnormalDecision, AbnormalError, AbnormalNotification, AbnormalNotificationState,
    AbnormalProcessOutput, MarkerStore, ProviderRegistry, TeamPresenceSnapshot, WholeTeamGoneClass,
    WholeTeamGoneReport, WorkspacePath,
};

/// 孤儿自终止判定(`__main__.py:51-59`,Gap 37b)。仅当
/// `current_ppid != initial_ppid ∧ current_ppid == 1 ∧ !workspace.exists()` 三者**同时**成立 → true。
/// 少一个条件都不能误杀正常 daemon(card §91)。
pub fn should_orphan_self_terminate(
    initial_ppid: u32,
    current_ppid: u32,
    workspace: &WorkspacePath,
) -> bool {
    current_ppid != initial_ppid && current_ppid == 1 && !workspace.as_path().exists()
}

// ===========================================================================
// abnormal_track 公共面(abnormal_track.py)—— Gap 32 §4 provider-neutral
// ===========================================================================

/// #236 dedicated `worker.abnormal_exit` surface.
///
/// This is intentionally separate from `process_abnormal_records`: the generic
/// abnormal path is transcript-only, while `worker.abnormal_exit` requires a fresh
/// latest explicit provider error before leader notification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event")]
pub enum WorkerAbnormalExitSurface {
    #[serde(rename = "worker.abnormal_exit")]
    WorkerAbnormalExit {
        agent_id: String,
        provider: Provider,
        path: String,
        provider_process_dead: bool,
        latest_explicit_error: bool,
        signature: Signature,
        turn_id: Option<TurnId>,
        process_liveness: ProcessLiveness,
    },
    #[serde(rename = "abnormal_exit.single_signal_suppressed")]
    SingleSignalSuppressed {
        agent_id: String,
        provider: Provider,
        path: String,
        provider_process_dead: bool,
        latest_explicit_error: bool,
        reason: String,
    },
}

/// `process_abnormal_records`(`abnormal_track.py:14`)。把原始 provider session records 经
/// provider reader 翻译成结构化 fault facts(本 module **不命名 provider、不读屏**),catch-bias
/// (C9)+ `(signature, turn_id|fingerprint)` 去重(C8)。`registry` 携带 provider + 黑白名单。
pub fn process_abnormal_records(
    records: &[Value],
    registry: &dyn ProviderRegistry,
    provider: Provider,
    notification_state: &AbnormalNotificationState,
) -> Result<AbnormalProcessOutput, AbnormalError> {
    let lists = registry.error_lists(provider);
    let mut state = notification_state.clone();
    let mut notifications = Vec::new();
    for record in records {
        let raw = record
            .get("raw")
            .and_then(Value::as_str)
            .unwrap_or_else(|| record.as_str().unwrap_or(""))
            .to_string();
        let signature = record
            .get("signature")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let signature_lower = signature.to_lowercase();
        let turn_id = record.get("turn_id").and_then(Value::as_str).map(TurnId::new);
        let bucket = match &turn_id {
            Some(id) => id.as_str().to_string(),
            None => stable_fingerprint(record),
        };
        let key = format!("{signature}\0{bucket}");
        if state.seen.contains(&key) {
            continue;
        }
        state.seen.insert(key);
        if lists.whitelist.iter().any(|needle| raw.contains(needle)) {
            continue;
        }
        let decision = if lists.blacklist.iter().any(|needle| {
            raw.contains(needle) || signature_lower.contains(&needle.to_lowercase())
        }) {
            AbnormalDecision::NotifyBlacklist
        } else {
            AbnormalDecision::NotifyDefault
        };
        let kind = record.get("kind").and_then(Value::as_str);
        notifications.push(AbnormalNotification {
            signature,
            turn_id,
            state: if kind == Some("approval") {
                TurnState::BlockedOnHuman
            } else {
                TurnState::Abnormal
            },
            decision,
            provider: Some(provider),
            raw: record.clone(),
        });
    }
    Ok(AbnormalProcessOutput {
        notifications,
        notification_state: state,
    })
}

/// `detect_whole_team_gone`(`abnormal_track.py:91`,C10/C13)。coordinator-independent 整队消失检测:
/// 全死(coordinator + leader + 所有 worker + 所有 session)且非 clean_shutdown/restart →
/// 写 durable marker + 延迟到下条 leader 命令再 escalate。clean/restart 静默。
pub fn detect_whole_team_gone(
    snapshot: &TeamPresenceSnapshot,
    marker_store: &mut dyn MarkerStore,
) -> WholeTeamGoneReport {
    let any_provider_alive = snapshot.provider_processes_alive.iter().any(|alive| *alive);
    if snapshot.coordinator_alive
        || snapshot.leader_alive
        || any_provider_alive
        || snapshot.tmux_sessions_present
    {
        return whole_team_report(false, WholeTeamGoneClass::Alive, false, false, false);
    }
    if snapshot.clean_shutdown {
        return whole_team_report(true, WholeTeamGoneClass::CleanShutdown, false, false, false);
    }
    if snapshot.restart_in_progress {
        return whole_team_report(true, WholeTeamGoneClass::RestartInProgress, false, false, false);
    }
    let marker_written = marker_store.set_marker(
        "whole_team_gone",
        serde_json::json!({"classification": "unexpected_exit"}),
    );
    whole_team_report(
        true,
        WholeTeamGoneClass::UnexpectedExit,
        true,
        true,
        marker_written,
    )
}

fn stable_fingerprint(value: &Value) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string()).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn whole_team_report(
    whole_team_gone: bool,
    classification: WholeTeamGoneClass,
    notify: bool,
    escalate_user_on_next_leader_command: bool,
    marker_written: bool,
) -> WholeTeamGoneReport {
    WholeTeamGoneReport {
        whole_team_gone,
        classification,
        notify,
        escalate_user_on_next_leader_command,
        marker_written,
    }
}
