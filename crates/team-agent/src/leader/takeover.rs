//! leader::takeover — idle take-over 决策面:build_idle_nodes / leader_node /
//! classify_provider_turn_state / evaluate_takeover_reminder / record_turn_open_after_delivery
//! + provider-neutral wake 层(should_reread / on_file_changed / take_pending)。

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::provider::{Provider, RolloutPath, TurnState};

use super::helpers::{parse_provider, provider_wire, read_rollout_text, turn_state_wire};
use super::{
    IdleNode, LeaderError, LeaderEvent, NodeRole, RereadDecision, RereadReason,
    TakeoverReminderResult, TurnClassification, TurnStateClassifier, WakeWatchState,
};

// ── leader::idle — build_idle_nodes / push_idle_reminder(coordinator tick 面)──

/// `build_idle_nodes`(card §50;`idle_takeover_wiring.py:20`)。从每个 live node 的 provider
/// session-log 文件分类 turn 状态(**绝不读 pane 屏幕**)。
/// **bug-085**:`rollout_path=None` → 该 node `Unknown`(不猜 idle);leader path/provider 缺
/// 则省略 leader 节点(`_leader_node` 返 `None`)而非猜 idle。
/// **MUST-NOT-13**:分类走 `read_turn_state`(provider_state trait,注入 mock),零 provider client。
pub fn build_idle_nodes(
    state: &Value,
    classifier: &dyn TurnStateClassifier,
) -> Result<Vec<IdleNode>, LeaderError> {
    let mut nodes = Vec::new();
    if let Some(agents) = state.get("agents").and_then(Value::as_object) {
        for (node_id, agent) in agents {
            if matches!(
                agent.get("status").and_then(Value::as_str),
                Some("stopped" | "paused")
            ) {
                continue;
            }
            let Some(provider) = agent.get("provider").and_then(Value::as_str).and_then(parse_provider) else {
                continue;
            };
            let rollout_path = agent
                .get("rollout_path")
                .and_then(Value::as_str)
                .map(|p| RolloutPath::new(PathBuf::from(p)));
            let text = read_rollout_text(rollout_path.as_ref())?;
            let classification = classifier.classify(provider, &text)?;
            nodes.push(IdleNode {
                node_id: node_id.clone(),
                role: NodeRole::Worker,
                state: classification.state,
                turn_id: classification.turn_id,
                annotations: classification.annotations,
                provider: Some(provider),
                auth_mode: agent.get("auth_mode").and_then(Value::as_str).map(str::to_string),
                rollout_path,
            });
        }
    }
    if let Some(node) = leader_node(state, classifier)? {
        nodes.push(node);
    }
    Ok(nodes)
}

/// `_leader_node`(`idle_takeover_wiring.py:77`)。leader 自身 transcript 分类(C13)。
/// path 或 provider 缺 → `None`(省略而非猜 idle)。
pub fn leader_node(
    state: &Value,
    classifier: &dyn TurnStateClassifier,
) -> Result<Option<IdleNode>, LeaderError> {
    let leader = state.get("leader");
    let receiver = state.get("leader_receiver");
    let provider = leader
        .and_then(|l| l.get("provider"))
        .and_then(Value::as_str)
        .or_else(|| receiver.and_then(|r| r.get("provider")).and_then(Value::as_str))
        .and_then(parse_provider);
    let rollout_path = leader
        .and_then(|l| l.get("rollout_path"))
        .and_then(Value::as_str)
        .or_else(|| receiver.and_then(|r| r.get("rollout_path")).and_then(Value::as_str))
        .map(|p| RolloutPath::new(PathBuf::from(p)));
    let (Some(provider), Some(rollout_path)) = (provider, rollout_path) else {
        return Ok(None);
    };
    let text = read_rollout_text(Some(&rollout_path))?;
    let classification = classifier.classify(provider, &text)?;
    Ok(Some(IdleNode {
        node_id: "leader".to_string(),
        role: NodeRole::Leader,
        state: classification.state,
        turn_id: classification.turn_id,
        annotations: classification.annotations,
        provider: Some(provider),
        auth_mode: None,
        rollout_path: Some(rollout_path),
    }))
}

// ── leader::idle facade re-export(验收契约 import 面:from team_agent.idle_takeover import ...）──

/// `classify_provider_turn_state`(card §51;`idle_takeover.py:18`)。门面:分类一个 node 的 turn
/// 状态;`state ∈ {unknown, abnormal}` 且有 event_sink → 写 `idle_takeover.classify`。
/// **unknown ≠ idle** 命门下游(`TurnState` 穷尽)。
pub fn classify_provider_turn_state(
    provider: Provider,
    session_log_text: &str,
    classifier: &dyn TurnStateClassifier,
    event_log: Option<&crate::event_log::EventLog>,
) -> Result<TurnClassification, LeaderError> {
    let classification = classifier.classify(provider, session_log_text)?;
    if matches!(classification.state, TurnState::Unknown | TurnState::Abnormal) {
        if let Some(log) = event_log {
            log.write(
                LeaderEvent::IdleTakeoverClassify.name(),
                json!({
                    "provider": provider_wire(provider),
                    "state": turn_state_wire(classification.state),
                    "reason": classification.reason,
                }),
            )?;
        }
    }
    Ok(classification)
}

/// `evaluate_takeover_reminder`(card §51 facade re-export)。provider-neutral 谓词:全 idle 且
/// armed-after-delegation 才 `should_ping`。**CROSS-LANE**:真逻辑在 provider-neutral `idle_predicate`
/// (step 8 相邻),此为 leader facade 暴露面。
pub fn evaluate_takeover_reminder(
    nodes: &[IdleNode],
    arm_state: &Value,
) -> Result<TakeoverReminderResult, LeaderError> {
    if nodes.is_empty() {
        return Ok(TakeoverReminderResult {
            should_ping: false,
            message: None,
            interrupted_nodes: Vec::new(),
            reason: Some("no_nodes".to_string()),
        });
    }
    if let Some(blocking) = nodes.iter().find(|n| !n.state.is_idle_for_takeover()) {
        return Ok(TakeoverReminderResult {
            should_ping: false,
            message: None,
            interrupted_nodes: Vec::new(),
            reason: Some(format!("node_{}", turn_state_wire(blocking.state))),
        });
    }
    // idle_predicate.py:55-62 (C1): only a real worker turn-open arms the watch — an
    // un-armed monitor must never ping. The facade honors both its own write-side key
    // (`armed`, record_turn_open_after_delivery) and the classify-layer monitor_state
    // key (`opened_worker_turn_since_ack`); debounce/episode tiers stay at the classify
    // layer (provider/classify.rs evaluate_takeover_reminder).
    let armed = arm_state.get("armed").and_then(Value::as_bool) == Some(true)
        || arm_state
            .get("opened_worker_turn_since_ack")
            .and_then(Value::as_bool)
            == Some(true);
    if !armed {
        return Ok(TakeoverReminderResult {
            should_ping: false,
            message: None,
            interrupted_nodes: Vec::new(),
            reason: Some("not_armed_no_worker_turn".to_string()),
        });
    }
    let interrupted_nodes = nodes
        .iter()
        .filter(|n| n.state == TurnState::IdleInterrupted)
        .map(|n| n.node_id.clone())
        .collect();
    Ok(TakeoverReminderResult {
        should_ping: true,
        message: Some("All active nodes appear idle; leader takeover may be appropriate.".to_string()),
        interrupted_nodes,
        reason: Some("all_idle_debounce_elapsed".to_string()),
    })
}

/// `record_turn_open_after_delivery`(card §51 / §80;facade re-export)。take-over arm **只来自
/// 真实投递的 turn-open 边**,不凭空 arm。**CROSS-LANE**:真逻辑在 `idle_predicate`(step 8 相邻)。
pub fn record_turn_open_after_delivery(
    arm_state: &mut Value,
    node_id: &str,
    turn_id: Option<&str>,
) -> Result<(), LeaderError> {
    if !arm_state.is_object() {
        *arm_state = json!({});
    }
    if let Some(obj) = arm_state.as_object_mut() {
        obj.insert("armed".to_string(), Value::Bool(true));
        obj.insert("node_id".to_string(), Value::String(node_id.to_string()));
        obj.insert(
            "turn_id".to_string(),
            turn_id.map_or(Value::Null, |id| Value::String(id.to_string())),
        );
    }
    Ok(())
}

// ── leader::wake — provider-neutral wake 层(纯函数,不解析/不轮询/无 provider 名)──

/// `should_reread`(card §52;`wake.py:16`)。决定是否值得重读 session 文件尾巴 + 为何。
/// 纯函数:file 变了 / 静默超 debounce 且未在此 mtime 分类过。provider-neutral(C6)。
pub fn should_reread(
    last_mtime: Option<f64>,
    current_mtime: Option<f64>,
    last_classified_mtime: Option<f64>,
    now: f64,
    debounce_seconds: f64,
) -> RereadDecision {
    let _ = last_mtime;
    let Some(current) = current_mtime else {
        return RereadDecision { reread: false, reason: RereadReason::NoFile };
    };
    let Some(classified) = last_classified_mtime else {
        return RereadDecision { reread: true, reason: RereadReason::NeverClassified };
    };
    if current != classified {
        return RereadDecision { reread: true, reason: RereadReason::FileChanged };
    }
    let silent_for = (now - current).max(0.0);
    if silent_for >= debounce_seconds {
        RereadDecision { reread: false, reason: RereadReason::QuiescentAlreadyClassified }
    } else {
        RereadDecision { reread: false, reason: RereadReason::Unchanged }
    }
}

/// `on_file_changed`(card §52;`wake.py:43`)。记录一个 node 的文件变更 wake(push 路径)。
pub fn on_file_changed(
    watch_state: Option<&WakeWatchState>,
    node_id: &str,
    mtime: f64,
) -> WakeWatchState {
    let mut state = watch_state.cloned().unwrap_or_default();
    if !state.pending.iter().any(|p| p == node_id) {
        state.pending.push(node_id.to_string());
    }
    state.pending.sort();
    state.mtimes.insert(node_id.to_string(), mtime);
    state
}

/// `take_pending`(card §52;`wake.py:53`)。排空自上次 drain 以来文件变更的 node 集。
pub fn take_pending(watch_state: Option<&WakeWatchState>) -> (Vec<String>, WakeWatchState) {
    let mut state = watch_state.cloned().unwrap_or_default();
    let mut drained = state.pending.clone();
    drained.sort();
    state.pending.clear();
    (drained, state)
}
