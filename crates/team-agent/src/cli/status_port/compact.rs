use super::*;

pub(super) fn compact_status(full: Value) -> Value {
    let not_ready = compact_not_ready(&full);
    let ready = compact_ready(&full, &not_ready);
    json!({
        "ok": true,
        "team": full.get("team").cloned().unwrap_or(Value::Null),
        "session_name": full.get("session_name").cloned().unwrap_or(Value::Null),
        "leader_attach_command": full.get("leader_attach_command").cloned().unwrap_or(Value::Null),
        "ready": ready,
        "not_ready": not_ready,
        "agents": compact_agents(full.get("agents")),
    })
}

/// Synthesized readiness boolean for the slim payload. Stricter than the
/// raw `readiness.ready` because it also folds in coordinator + schema +
/// tmux session presence so operators don't need to read separate booleans.
pub(super) fn compact_ready(full: &Value, not_ready: &Value) -> bool {
    not_ready.is_null()
        && full
            .get("readiness")
            .and_then(|r| r.get("ready"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && full
            .get("coordinator")
            .and_then(|c| c.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|s| s == "running" || s == "ok")
        && full
            .get("coordinator")
            .and_then(|c| c.get("schema_ok"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
}

/// Returns `Value::Null` when fully ready, otherwise an object:
/// `{"reasons": [...], "agents": [...]}` listing every gating issue.
pub(super) fn compact_not_ready(full: &Value) -> Value {
    let reasons = not_ready_reasons(full);
    if reasons.is_empty() {
        return Value::Null;
    }
    let agents = full
        .get("incomplete_session_capture_agents")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            full.get("pending_session_agent_ids")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let mut obj = Map::new();
    obj.insert(
        "reasons".to_string(),
        Value::Array(reasons.into_iter().map(Value::String).collect()),
    );
    obj.insert("agents".to_string(), Value::Array(agents));
    Value::Object(obj)
}

pub(super) fn not_ready_reasons(full: &Value) -> Vec<String> {
    let mut reasons = Vec::new();
    let coord = full.get("coordinator");
    let coord_status = coord
        .and_then(|c| c.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if coord_status != "running" && coord_status != "ok" {
        reasons.push("coordinator_not_running".to_string());
    }
    if coord
        .and_then(|c| c.get("schema_ok"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        reasons.push("coordinator_schema_not_ok".to_string());
    }
    if full.get("tmux_session_present").and_then(Value::as_bool) == Some(false) {
        reasons.push("tmux_session_missing".to_string());
    }
    let readiness = full.get("readiness");
    if readiness
        .and_then(|r| r.get("all_spawned"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        reasons.push("workers_not_spawned".to_string());
    }
    if readiness
        .and_then(|r| r.get("all_attached_receiver"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        reasons.push("leader_receiver_unbound".to_string());
    }
    if readiness
        .and_then(|r| r.get("session_capture_complete"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        reasons.push("session_capture_incomplete".to_string());
    }
    if readiness
        .and_then(|r| r.get("awaiting_trust_prompt"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        reasons.push("awaiting_trust_prompt".to_string());
    }
    reasons
}

pub(super) fn compact_agents(value: Option<&Value>) -> Value {
    let Some(Value::Object(input)) = value else {
        return json!({});
    };
    let mut out = Map::new();
    for (agent_id, agent) in input {
        out.insert(agent_id.clone(), compact_agent_state(agent_id, agent));
    }
    Value::Object(out)
}

/// 0.4.x: agent rows in the slim payload have exactly 4 fields. agent_id
/// is no longer copied in — the map key already carries it. Diagnostic
/// fields (model, tmux_window_present, session_id, captured_via,
/// attribution_confidence, display, interacted) move to `--detail`.
/// `activity` + `last_output_at` are preserved (RM-039-STAT-001).
pub(super) fn compact_agent_state(_agent_id: &str, agent: &Value) -> Value {
    let Some(input) = agent.as_object() else {
        return json!({});
    };
    let mut out = Map::new();
    // 0.4.x Phase 1: add `worker_state` (canonical 5-state product
    // surface). `activity` is preserved alongside as the deprecated
    // legacy classifier output (CR R3 same-source contract).
    for key in [
        "status",
        "provider",
        "worker_state",
        "activity",
        "last_output_at",
        "stale",
        "stale_reason",
    ] {
        if let Some(value) = input.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(out)
}

pub(super) fn compact_tasks(value: Option<&Value>) -> Value {
    let Some(Value::Array(tasks)) = value else {
        return json!([]);
    };
    Value::Array(
        tasks
            .iter()
            .map(|task| {
                compact_object(
                    Some(task),
                    &[
                        "id",
                        "title",
                        "status",
                        "assignee",
                        "type",
                        "accepted_result_id",
                    ],
                )
            })
            .collect(),
    )
}

pub(super) fn compact_object(value: Option<&Value>, keys: &[&str]) -> Value {
    let Some(Value::Object(input)) = value else {
        return json!({});
    };
    let mut out = Map::new();
    for key in keys {
        if let Some(value) = input.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

pub(super) fn take_array(value: Option<&Value>, limit: usize) -> Value {
    let Some(Value::Array(items)) = value else {
        return json!([]);
    };
    Value::Array(items.iter().take(limit).cloned().collect())
}

pub(super) fn take_array_tail(value: Option<&Value>, limit: usize) -> Value {
    let Some(Value::Array(items)) = value else {
        return json!([]);
    };
    let start = items.len().saturating_sub(limit);
    Value::Array(items.iter().skip(start).cloned().collect())
}
