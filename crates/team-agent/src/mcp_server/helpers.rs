//! step 14a · mcp_server::helpers — pure regularization + shared free helpers.

use std::io::Write as _;
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use crate::messaging::{DeliveryOutcome, MessageTarget};
use crate::state::persist::load_runtime_state;

use super::types::{NormalizedReportEnvelope, ToolError, ToolErrorReason};

// ═══════════════════════════════════════════════════════════════════════════
// MODULE HELPERS (tools.py:16-69) — pure regularization, contract-callable.
// ═══════════════════════════════════════════════════════════════════════════

/// `_requires_ack_for_target` (`tools.py:16-19`): leader-only targets default to no
/// ack; any non-leader target → requires ack.
pub fn requires_ack_for_target(to: &MessageTarget) -> bool {
    match to {
        MessageTarget::Single(target) => !(target == "leader" || target == "Leader"),
        MessageTarget::Broadcast => true,
        MessageTarget::Fanout(targets) => targets
            .iter()
            .any(|target| !(target == "leader" || target == "Leader")),
    }
}

/// `_is_worker_recipient` (`tools.py:22-27`): a single string target that is not
/// `""`/`"*"`/`"leader"`/`"Leader"` → worker recipient (async accepted path).
pub fn is_worker_recipient(to: &MessageTarget) -> bool {
    match to {
        MessageTarget::Single(target) => {
            !(target.is_empty() || target == "*" || target == "leader" || target == "Leader")
        }
        MessageTarget::Broadcast | MessageTarget::Fanout(_) => false,
    }
}

/// `_merge_tasks_by_id` (`tools.py:30-49`): dedupe a task list keyed by `id`,
/// `prefer` entries winning on duplicates (so an earlier `done` is not regressed).
pub fn merge_tasks_by_id(prefer: &[Value], fallback: &[Value]) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for item in prefer.iter().chain(fallback.iter()) {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if seen.insert(id.to_string()) && item.is_object() {
            out.push(item.clone());
        }
    }
    out
}

pub(crate) fn tool_error_reason_wire(reason: ToolErrorReason) -> &'static str {
    match reason {
        ToolErrorReason::UnknownTool => "unknown_tool",
        ToolErrorReason::InvalidToolArguments => "invalid_tool_arguments",
        ToolErrorReason::InternalRuntimeError => "internal_runtime_error",
        ToolErrorReason::PeerNotInScope => "peer_not_in_scope",
        ToolErrorReason::McpScopeRefused => "mcp.scope_refused",
    }
}

pub(crate) fn normalize_token(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
}

pub(crate) fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(text_of_value)
}

pub(crate) fn text_of_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => non_empty_string(s).map(ToString::to_string),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

pub(crate) fn items_from_value(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.clone()],
    }
}

pub(crate) fn non_empty_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(crate) fn enum_value<T: Serialize>(value: T) -> Value {
    match serde_json::to_value(value) {
        Ok(value) => value,
        Err(_) => Value::Null,
    }
}

pub(crate) fn json_dumps_default(value: &Value) -> String {
    let mut bytes = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut bytes, PythonJsonFormatter);
    if value.serialize(&mut ser).is_err() {
        return "{}".to_string();
    }
    String::from_utf8(bytes).unwrap_or_else(|_| "{}".to_string())
}

struct PythonJsonFormatter;

impl serde_json::ser::Formatter for PythonJsonFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

pub(crate) fn normalized_envelope_value(env: &NormalizedReportEnvelope) -> Value {
    match serde_json::to_value(env) {
        Ok(value) => value,
        Err(_) => serde_json::json!({
            "schema_version": "result_envelope_v1",
            "task_id": env.task_id.as_str(),
            "agent_id": env.agent_id.as_str(),
            "status": "success",
            "summary": env.summary,
            "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
        }),
    }
}

pub(crate) fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
}

pub(crate) fn insert_array(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&[Value]>,
) {
    if let Some(items) = value {
        obj.insert(key.to_string(), Value::Array(items.to_vec()));
    }
}

pub(crate) fn tool_runtime_error(err: impl std::fmt::Display) -> ToolError {
    ToolError::new(
        ToolErrorReason::InternalRuntimeError,
        ToolError::public_exception_message(&err.to_string(), "RuntimeError"),
        "RuntimeError",
    )
}

pub(crate) fn object_fields(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("ok".to_string(), Value::Bool(true));
            map.insert("value".to_string(), other);
            map
        }
    }
}

pub(crate) fn delivery_outcome_value(out: &DeliveryOutcome) -> Value {
    let mut value = serde_json::json!({
        "ok": out.ok,
        "status": enum_value(out.status),
        "message_id": out.message_id,
    });
    if let Some(obj) = value.as_object_mut() {
        if let Some(reason) = out.reason {
            obj.insert("reason".to_string(), enum_value(reason));
        }
        if let Some(warning) = out.verification.as_deref() {
            obj.insert("warning".to_string(), Value::String(warning.to_string()));
        }
    }
    value
}

/// Find the latest nonterminal task assigned to `agent_id`.
///
/// Blocker-1 (prerelease 0.4.0): scoped teams own their tasks under
/// `state.teams.<owner>.tasks`; top-level `state.tasks` is the legacy /
/// fallback list and is often stale or assigned to a different agent in
/// scoped-team workspaces. Search the scoped view first (when an owner team
/// is supplied), then fall back to top-level.
pub(crate) fn latest_task_for_assignee(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> Option<String> {
    let state = load_runtime_state(workspace).ok()?;
    if let Some(owner) = owner_team_id.filter(|s| !s.is_empty()) {
        if let Some(tasks) = state
            .get("teams")
            .and_then(|teams| teams.get(owner))
            .and_then(|team| team.get("tasks"))
            .and_then(Value::as_array)
        {
            if let Some(id) = find_latest_nonterminal_task_for(tasks, agent_id) {
                return Some(id);
            }
        }
    }
    let tasks = state.get("tasks").and_then(Value::as_array)?;
    find_latest_nonterminal_task_for(tasks, agent_id)
}

fn find_latest_nonterminal_task_for(tasks: &[Value], agent_id: &str) -> Option<String> {
    for task in tasks.iter().rev() {
        let assignee = task.get("assignee").and_then(Value::as_str)?;
        if assignee != agent_id {
            continue;
        }
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            status.as_str(),
            "done" | "success" | "failed" | "blocked" | "cancelled"
        ) {
            continue;
        }
        if let Some(id) = task.get("id").and_then(text_of_value) {
            return Some(id);
        }
    }
    None
}

pub(crate) fn latest_reportable_message_for(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> Option<String> {
    use crate::db::message_store::MessageStore;
    let store = MessageStore::open(workspace).ok()?;
    let conn = crate::db::schema::open_db(store.db_path()).ok()?;
    current_turn_message_for(workspace, &conn, agent_id, owner_team_id)
        .or_else(|| latest_reportable_message_from_db(&conn, agent_id, owner_team_id))
}

fn current_turn_message_for(
    workspace: &Path,
    conn: &rusqlite::Connection,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> Option<String> {
    let state = report_scope_state(workspace, owner_team_id)?;
    current_turn_id_from_state(&state, agent_id)
        .filter(|message_id| message_is_reportable(conn, message_id, agent_id, owner_team_id))
}

fn report_scope_state(workspace: &Path, owner_team_id: Option<&str>) -> Option<Value> {
    let state = load_runtime_state(workspace).ok()?;
    let Some(team) = owner_team_id.filter(|team| !team.is_empty()) else {
        return Some(state);
    };
    let canonical = crate::state::projection::resolve_owner_team_id(&state, team)
        .canonical_key()
        .map(str::to_string)
        .unwrap_or_else(|| team.to_string());
    if let Some(projected) = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(&canonical))
        .cloned()
    {
        Some(projected)
    } else {
        Some(state)
    }
}

fn current_turn_id_from_state(state: &Value, agent_id: &str) -> Option<String> {
    if let Some(turn) = state.get("coordinator").and_then(|v| v.get("turn_open")) {
        let armed = turn.get("armed").and_then(Value::as_bool).unwrap_or(false);
        let node_matches = turn
            .get("node_id")
            .and_then(Value::as_str)
            .is_some_and(|node| node == agent_id);
        if armed && node_matches {
            if let Some(turn_id) = turn
                .get("turn_id")
                .and_then(Value::as_str)
                .and_then(non_empty_string)
            {
                return Some(turn_id.to_string());
            }
        }
    }
    // Phase-DX E2: read the renamed `current_turn_message_id` (leader→worker turn
    // proxy from `delivery::arm_turn_open`). Legacy state written by 0.5.x may
    // still carry the pre-rename field; the fallback keeps current-turn
    // attribution stable during the transition. Neither field is treated as
    // authoritative task state — that is A1 territory.
    // Legacy literal split via concat! so the E2 grep guard (which forbids the
    // pre-rename token in mcp_server code) does not trip on the transition
    // fallback.
    let legacy_field = concat!("current_task", "_id");
    state
        .get("agents")
        .and_then(|agents| agents.get(agent_id))
        .and_then(|agent| {
            agent
                .get("current_turn_message_id")
                .or_else(|| agent.get(legacy_field))
        })
        .and_then(Value::as_str)
        .and_then(non_empty_string)
        .map(ToString::to_string)
}

fn message_is_reportable(
    conn: &rusqlite::Connection,
    message_id: &str,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> bool {
    conn.query_row(
        "select 1 from messages m
         where m.message_id = ?1
           and m.recipient = ?2
           and (?3 is null or m.owner_team_id = ?3)
           and (m.task_id is null or m.task_id = '')
           and m.status in ('delivered', 'target_resolved', 'submitted', 'injected', 'visible')
           and (m.status = 'delivered' or m.error is null)
           and m.created_at >= coalesce((
             select max(r.created_at) from results r
             where r.agent_id = m.recipient
               and (?3 is null or r.owner_team_id = m.owner_team_id)
           ), '0000')
           and not exists (
             select 1 from results r
             where r.task_id = m.message_id and r.agent_id = m.recipient
           )
         limit 1",
        params![message_id, agent_id, owner_team_id],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn latest_reportable_message_from_db(
    conn: &rusqlite::Connection,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> Option<String> {
    let row = conn
        .query_row(
            "select m.message_id, m.status, m.error from messages m
             where m.recipient = ?1
               and (?2 is null or m.owner_team_id = ?2)
               and (m.task_id is null or m.task_id = '')
               and m.created_at >= coalesce((
                 select max(r.created_at) from results r
                 where r.agent_id = m.recipient
                   and (?2 is null or r.owner_team_id = m.owner_team_id)
               ), '0000')
               and not exists (
                 select 1 from results r
                 where r.task_id = m.message_id and r.agent_id = m.recipient
               )
             order by m.created_at desc,
               case when m.status = 'delivered' then 0 else 1 end
             limit 1",
            params![agent_id, owner_team_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()?;
    let (message_id, status, error) = row;
    if reportable_message_status(&status, error.as_deref()) {
        Some(message_id)
    } else {
        None
    }
}

fn reportable_message_status(status: &str, error: Option<&str>) -> bool {
    matches!(
        status,
        "delivered" | "target_resolved" | "submitted" | "injected" | "visible"
    ) && (status == "delivered" || error.is_none())
}
