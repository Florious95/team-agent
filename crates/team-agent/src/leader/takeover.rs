//!
//! leader::takeover — idle take-over 提醒判定面。

use serde_json::Value;

use crate::provider::TurnState;

use super::helpers::turn_state_wire;
use super::{IdleNode, LeaderError, TakeoverReminderResult};

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
    // un-armed monitor must never ping. The facade honors the legacy `armed` input and
    // the classify-layer monitor_state key (`opened_worker_turn_since_ack`);
    // debounce/episode tiers stay at the classify layer
    // (provider/classify.rs evaluate_takeover_reminder).
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
        message: Some(
            "All active nodes appear idle; leader takeover may be appropriate.".to_string(),
        ),
        interrupted_nodes,
        reason: Some("all_idle_debounce_elapsed".to_string()),
    })
}
