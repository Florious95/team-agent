//! results.py — collect + report_result + envelope 校验编排 (card §66/§67)。

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;

use super::helpers::{next_result_id, required_str, validate_result_envelope};
use super::types::SEND_RETRY_MAX_ATTEMPTS;
use crate::model::ids::TaskId;
use crate::state::projection::OwnerTeamResolution;
use super::watchers::retry_result_deliveries;
use super::MessagingError;

/// `collect` (`results.py:45`):投递 pending、捞 uncollected results、校验 envelope、更新任务态、
/// 写 team_state、ensure coordinator。CLI `collect` + coordinator tick 调。
pub fn collect(
    workspace: &Path,
    result_file: Option<&Path>,
    ensure_coordinator: bool,
) -> Result<serde_json::Value, MessagingError> {
    collect_scoped(workspace, result_file, ensure_coordinator, None)
}

pub fn collect_for_team(
    workspace: &Path,
    result_file: Option<&Path>,
    ensure_coordinator: bool,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    collect_scoped(workspace, result_file, ensure_coordinator, owner_team_id)
}

fn collect_scoped(
    workspace: &Path,
    result_file: Option<&Path>,
    ensure_coordinator: bool,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    let _ = ensure_coordinator;
    let paths = collect_paths(workspace)?;
    let log = EventLog::new(&paths.run_workspace);
    let resolved_owner_team_id = match owner_team_id.filter(|team| !team.is_empty()) {
        Some(team) => Some(resolve_owner_team_for_read(&paths.run_workspace, team, Some(&log))?),
        None => None,
    };
    let owner_team_id = resolved_owner_team_id.as_deref();
    let mut state = match owner_team_id {
        Some(team) => crate::state::projection::select_runtime_state(&paths.run_workspace, Some(team))?,
        None => crate::state::persist::load_runtime_state(&paths.run_workspace)?,
    };
    let spec_workspace = owner_team_id
        .and_then(|_| state_spec_workspace_from_value(&state))
        .unwrap_or_else(|| paths.spec_workspace.clone());
    let spec_path = spec_workspace.join("team.spec.yaml");
    if !spec_path.exists() {
        return Err(MessagingError::Validation(format!("Cannot read {}", spec_path.display())));
    }
    let store = MessageStore::open(&paths.run_workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    if let Some(path) = result_file {
        ingest_result_file(&conn, path, owner_team_id)?;
    }
    let sql = match owner_team_id {
        Some(_) => {
            "select result_id, task_id, agent_id, envelope, status, created_at
             from results
             where status not in ('collected', 'invalid') and owner_team_id = ?1
             order by created_at, result_id"
        }
        None => {
            "select result_id, task_id, agent_id, envelope, status, created_at
             from results
             where status not in ('collected', 'invalid')
             order by created_at, result_id"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let row_mapper = |row: &rusqlite::Row<'_>| {
            Ok(StoredResult {
                result_id: row.get(0)?,
                task_id: row.get(1)?,
                agent_id: row.get(2)?,
                envelope: row.get(3)?,
                status: row.get(4)?,
                created_at: row.get(5)?,
            })
        };
    let rows = match owner_team_id {
        Some(team) => stmt.query_map(params![team], row_mapper),
        None => stmt.query_map([], row_mapper),
    }?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut collected = Vec::new();
    let mut collected_results = Vec::new();
    let mut invalid_results = Vec::new();
    let mut state_dirty = false;
    for row in rows {
        let envelope: serde_json::Value = match serde_json::from_str(&row.envelope) {
            Ok(envelope) => envelope,
            Err(error) => {
                record_invalid_result(
                    &conn,
                    &mut invalid_results,
                    &row,
                    result_file,
                    &error.to_string(),
                )?;
                continue;
            }
        };
        if let Err(error) = validate_result_envelope(&envelope) {
            record_invalid_result(
                &conn,
                &mut invalid_results,
                &row,
                result_file,
                &error.to_string(),
            )?;
            continue;
        }
        let scope = if task_exists(&state, &row.task_id) {
            "task"
        } else if is_message_scoped_result(&conn, &row.task_id, &row.agent_id, owner_team_id)? {
            "message"
        } else {
            record_invalid_result(
                &conn,
                &mut invalid_results,
                &row,
                result_file,
                &format!("unknown task id: {}", row.task_id),
            )?;
            continue;
        };
        match owner_team_id {
            Some(team) => {
                conn.execute(
                    "update results set status = 'collected' where result_id = ?1 and owner_team_id = ?2",
                    params![row.result_id.as_str(), team],
                )?;
            }
            None => {
                conn.execute(
                    "update results set status = 'collected' where result_id = ?1",
                    params![row.result_id.as_str()],
                )?;
            }
        }
        if scope == "task" {
            mark_task_done(&mut state, &row.task_id, &row.result_id);
            state_dirty = true;
        }
        log.write(
            "collect.result",
            serde_json::json!({
                "result_id": row.result_id,
                "task_id": row.task_id,
                "agent_id": row.agent_id,
                "scope": scope,
            }),
        )?;
        collected.push(envelope.clone());
        let summary = serde_json::json!({
            "result_id": row.result_id,
            "task_id": row.task_id,
            "agent_id": row.agent_id,
            "status": envelope
                .get("status")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::String(row.status)),
            "summary": envelope.get("summary").cloned().unwrap_or(serde_json::Value::Null),
            "tests": envelope.get("tests").cloned().unwrap_or_else(|| serde_json::json!([])),
            "created_at": row.created_at,
            "scope": scope,
        });
        collected_results.push(summary);
    }
    if state_dirty {
        if owner_team_id.is_some() {
            crate::state::projection::save_team_scoped_state(&paths.run_workspace, &state)?;
        } else {
            crate::state::persist::save_runtime_state(&paths.run_workspace, &state)?;
        }
    }
    let counts = result_counts(&conn, owner_team_id)?;
    Ok(serde_json::json!({
        "ok": invalid_results.is_empty(),
        "collected": collected,
        "collected_results": collected_results,
        "delivered_messages": [],
        "invalid_results": invalid_results,
        "results": counts,
        "state_file": spec_workspace.join("team_state.md").to_string_lossy().to_string(),
        "coordinator": {
            "ok": false,
            "status": "not_required",
        },
    }))
}

fn resolve_owner_team_for_read(
    workspace: &Path,
    requested: &str,
    event_log: Option<&EventLog>,
) -> Result<String, MessagingError> {
    let state = crate::state::persist::load_runtime_state(workspace)?;
    match crate::state::projection::resolve_owner_team_id(&state, requested) {
        OwnerTeamResolution::Canonical(canonical) => Ok(canonical),
        OwnerTeamResolution::LegacyAlias { requested, canonical } => {
            crate::messaging::delivery::normalize_owner_team_id_rows(
                workspace,
                &requested,
                &canonical,
                None,
                event_log,
            )?;
            Ok(canonical)
        }
        OwnerTeamResolution::Unresolved { requested } => {
            Err(MessagingError::Routing(format!("owner_team_unresolved: {requested}")))
        }
        OwnerTeamResolution::Ambiguous { requested, matches } => {
            Err(MessagingError::Routing(format!(
                "owner_team_ambiguous: {requested} matches {}",
                matches.join(",")
            )))
        }
    }
}

struct CollectPaths {
    run_workspace: PathBuf,
    spec_workspace: PathBuf,
}

fn collect_paths(workspace: &Path) -> Result<CollectPaths, MessagingError> {
    if collect_input_has_no_local_team_context(workspace) {
        return Ok(CollectPaths {
            run_workspace: workspace.to_path_buf(),
            spec_workspace: workspace.to_path_buf(),
        });
    }
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| MessagingError::Routing(e.to_string()))?;
    let spec_workspace = if workspace.join("team.spec.yaml").exists() {
        workspace.to_path_buf()
    } else if run_workspace.join("team.spec.yaml").exists() {
        run_workspace.clone()
    } else {
        state_spec_workspace(&run_workspace).unwrap_or_else(|| run_workspace.clone())
    };
    Ok(CollectPaths {
        run_workspace,
        spec_workspace,
    })
}

fn collect_input_has_no_local_team_context(workspace: &Path) -> bool {
    !workspace.join("team.spec.yaml").exists()
        && !workspace.join(".team").exists()
        && !crate::state::persist::runtime_state_path(workspace).exists()
        && workspace.file_name().and_then(|s| s.to_str()) != Some(".team")
        && workspace
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            != Some(".team")
}

fn state_spec_workspace(run_workspace: &Path) -> Option<PathBuf> {
    let state = crate::state::persist::load_runtime_state(run_workspace).ok()?;
    state_spec_workspace_from_value(&state)
}

fn state_spec_workspace_from_value(state: &serde_json::Value) -> Option<PathBuf> {
    if let Some(spec_path) = state.get("spec_path").and_then(serde_json::Value::as_str) {
        return PathBuf::from(spec_path).parent().map(Path::to_path_buf);
    }
    state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

fn record_invalid_result(
    conn: &rusqlite::Connection,
    invalid_results: &mut Vec<serde_json::Value>,
    row: &StoredResult,
    result_file: Option<&Path>,
    error: &str,
) -> Result<(), MessagingError> {
    conn.execute(
        "update results set status = 'invalid' where result_id = ?1",
        params![row.result_id.as_str()],
    )?;
    invalid_results.push(serde_json::json!({
        "result_id": row.result_id,
        "task_id": row.task_id,
        "agent_id": row.agent_id,
        "path": result_file.map(|p| p.to_string_lossy().to_string()),
        "error": error,
    }));
    Ok(())
}

fn ingest_result_file(
    conn: &rusqlite::Connection,
    path: &Path,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    let raw = std::fs::read_to_string(path)?;
    let mut envelope: serde_json::Value = serde_json::from_str(&raw)?;
    validate_result_envelope(&envelope)?;
    let result_id = envelope
        .get("result_id")
        .and_then(serde_json::Value::as_str)
        .map_or_else(next_result_id, ToString::to_string);
    if envelope.get("result_id").is_none() {
        if let Some(obj) = envelope.as_object_mut() {
            obj.insert(
                "result_id".to_string(),
                serde_json::Value::String(result_id.clone()),
            );
        }
    }
    let task_id = required_str(&envelope, "task_id")?;
    let agent_id = required_str(&envelope, "agent_id")?;
    let status = required_str(&envelope, "status")?;
    insert_result_if_absent(
        conn,
        &result_id,
        task_id,
        agent_id,
        &envelope.to_string(),
        status,
        owner_team_id,
    )?;
    Ok(())
}

struct StoredResult {
    result_id: String,
    task_id: String,
    agent_id: String,
    envelope: String,
    status: String,
    created_at: String,
}

fn mark_task_done(state: &mut serde_json::Value, task_id: &str, result_id: &str) {
    let Some(tasks) = state.get_mut("tasks").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    for task in tasks {
        if task.get("id").and_then(serde_json::Value::as_str) != Some(task_id) {
            continue;
        }
        if let Some(obj) = task.as_object_mut() {
            obj.insert("status".to_string(), serde_json::Value::String("done".to_string()));
            obj.insert(
                "accepted_result_id".to_string(),
                serde_json::Value::String(result_id.to_string()),
            );
        }
    }
}

fn task_exists(state: &serde_json::Value, task_id: &str) -> bool {
    state
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.get("id").and_then(serde_json::Value::as_str) == Some(task_id))
        })
}

fn is_message_scoped_result(
    conn: &rusqlite::Connection,
    task_id: &str,
    agent_id: &str,
    owner_team_id: Option<&str>,
) -> Result<bool, MessagingError> {
    if !task_id.starts_with("msg_") {
        return Ok(false);
    }
    let count: i64 = match owner_team_id {
        Some(team) => conn.query_row(
            "select count(*) from messages where message_id = ?1 and recipient = ?2 and owner_team_id = ?3",
            params![task_id, agent_id, team],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "select count(*) from messages where message_id = ?1 and recipient = ?2",
            params![task_id, agent_id],
            |row| row.get(0),
        )?,
    };
    Ok(count > 0)
}

fn result_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    let total: i64 = count_results(conn, owner_team_id, None)?;
    let collected: i64 = count_results(conn, owner_team_id, Some("collected"))?;
    let invalid: i64 = count_results(conn, owner_team_id, Some("invalid"))?;
    let uncollected = total - collected - invalid;
    let mut by_status = serde_json::Map::new();
    let sql = match owner_team_id {
        Some(_) => {
            "select status, count(*) from results
             where status not in ('collected', 'invalid') and owner_team_id = ?1
             group by status
             order by status"
        }
        None => {
            "select status, count(*) from results
             where status not in ('collected', 'invalid')
             group by status
             order by status"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let row_mapper = |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
    let rows = match owner_team_id {
        Some(team) => stmt.query_map(params![team], row_mapper),
        None => stmt.query_map([], row_mapper),
    }?;
    for row in rows {
        let (status, count) = row?;
        by_status.insert(status, serde_json::Value::Number(count.into()));
    }
    Ok(serde_json::json!({
        "total": total,
        "uncollected": uncollected,
        "collected": collected,
        "invalid": invalid,
        "by_status": by_status,
    }))
}

fn count_results(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
    status: Option<&str>,
) -> Result<i64, MessagingError> {
    match (owner_team_id, status) {
        (Some(team), Some(status)) => Ok(conn.query_row(
            "select count(*) from results where owner_team_id = ?1 and status = ?2",
            params![team, status],
            |row| row.get(0),
        )?),
        (Some(team), None) => Ok(conn.query_row(
            "select count(*) from results where owner_team_id = ?1",
            params![team],
            |row| row.get(0),
        )?),
        (None, Some(status)) => Ok(conn.query_row(
            "select count(*) from results where status = ?1",
            params![status],
            |row| row.get(0),
        )?),
        (None, None) => Ok(conn.query_row("select count(*) from results", [], |row| row.get(0))?),
    }
}

/// `report_result` (`results.py:191`):worker 报结果 —— 校验 envelope、存 result、ack 任务消息、
/// **排队** send 事件通知 leader、推进 orchestrator (软依赖,失败仅记 `orchestrator.advance_skipped`)。
/// MCP `report_result` 工具调。
pub fn report_result(
    workspace: &Path,
    envelope: &serde_json::Value,
) -> Result<serde_json::Value, MessagingError> {
    validate_result_envelope(envelope)?;
    let store = MessageStore::open(workspace)?;
    let result_id = envelope
        .get("result_id")
        .and_then(|v| v.as_str())
        .map_or_else(next_result_id, ToString::to_string);
    let task_id = required_str(envelope, "task_id")?;
    let agent_id = required_str(envelope, "agent_id")?;
    let status = required_str(envelope, "status")?;
    let mut stored = envelope.clone();
    if stored.get("result_id").is_none() {
        if let Some(obj) = stored.as_object_mut() {
            obj.insert("result_id".to_string(), serde_json::Value::String(result_id.clone()));
        }
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let state_for_owner = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or(serde_json::json!({}));
    let owner_team = super::leader_receiver::active_team_key(workspace, &state_for_owner);
    let inserted = insert_result_if_absent(
        &conn,
        &result_id,
        task_id,
        agent_id,
        &stored.to_string(),
        status,
        Some(&owner_team),
    )?;
    if !inserted {
        let log = EventLog::new(workspace);
        log.write(
            "mcp.report_result_duplicate_ignored",
            serde_json::json!({
                "notification_status": "duplicate_ignored",
                "owner_team_id": null,
                "result_id": result_id,
            }),
        )?;
        let mut out = serde_json::Map::new();
        out.insert("ok".to_string(), serde_json::Value::Bool(true));
        out.insert(
            "status".to_string(),
            serde_json::Value::String("duplicate_ignored".to_string()),
        );
        out.insert("result_id".to_string(), serde_json::Value::String(result_id));
        out.insert("task_id".to_string(), serde_json::Value::String(task_id.to_string()));
        out.insert("agent_id".to_string(), serde_json::Value::String(agent_id.to_string()));
        out.insert("acknowledged_messages".to_string(), serde_json::json!([]));
        out.insert("leader_notified".to_string(), serde_json::Value::Bool(false));
        out.insert("notification_message_id".to_string(), serde_json::Value::Null);
        out.insert(
            "notification_status".to_string(),
            serde_json::Value::String("duplicate_ignored".to_string()),
        );
        out.insert(
            "notification_channel".to_string(),
            serde_json::Value::String("coordinator".to_string()),
        );
        out.insert("notification_event_id".to_string(), serde_json::Value::Null);
        return Ok(serde_json::Value::Object(out));
    }
    // #230 N31/N32 funnel: report_result must go through the shared leader-delivery
    // primitive synchronously, NOT via a parallel queued scheduled_events row. The
    // legacy path was MUST-8 / I-3 violating (the deferred notification status was returned
    // to the caller as "success" while leader actually never saw the result text).
    let content = format_report_result_notification(&result_id, task_id, agent_id, status, envelope);
    let state = crate::state::persist::load_runtime_state(workspace).unwrap_or(serde_json::json!({}));
    let event_log = EventLog::new(workspace);
    let mut outcome = super::leader_receiver::send_to_leader_receiver(
        workspace,
        &state,
        "leader",
        &content,
        Some(&TaskId::new(task_id.to_string())),
        agent_id,
        false,
        Some(&result_id),
        &event_log,
    )?;
    if matches!(outcome.status, crate::messaging::DeliveryStatus::Queued) {
        if let Some(message_id) = outcome.message_id.clone() {
            let store = MessageStore::open(workspace)?;
            let transport = crate::tmux_backend::TmuxBackend::for_workspace(workspace);
            outcome = super::delivery::deliver_pending_message(
                workspace,
                &store,
                &transport,
                &message_id,
                &event_log,
                &state,
            )?;
        }
    }
    let leader_notified = outcome.ok;
    let notification_status_wire = if outcome.ok {
        "delivered"
    } else if outcome.channel.as_deref() == Some("rebind_required")
        || matches!(outcome.status, crate::messaging::DeliveryStatus::Blocked)
    {
        "rebind_required"
    } else {
        "refused"
    };
    let channel = outcome.channel.clone().unwrap_or_else(|| "leader_receiver".to_string());
    event_log.write(
        "mcp.report_result",
        serde_json::json!({
            "leader_notified": leader_notified,
            "notification_channel": channel,
            "notification_message_id": outcome.message_id,
            "notification_status": notification_status_wire,
            "owner_team_id": null,
            "result_id": result_id,
        }),
    )?;

    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), serde_json::Value::Bool(true));
    out.insert("result_id".to_string(), serde_json::Value::String(result_id));
    out.insert("task_id".to_string(), serde_json::Value::String(task_id.to_string()));
    out.insert("agent_id".to_string(), serde_json::Value::String(agent_id.to_string()));
    out.insert("acknowledged_messages".to_string(), serde_json::json!([]));
    out.insert("leader_notified".to_string(), serde_json::Value::Bool(leader_notified));
    out.insert(
        "notification_message_id".to_string(),
        outcome.message_id.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    out.insert(
        "notification_status".to_string(),
        serde_json::Value::String(notification_status_wire.to_string()),
    );
    out.insert(
        "notification_channel".to_string(),
        serde_json::Value::String(channel.clone()),
    );
    if channel == "rebind_required" {
        out.insert(
            "notification_action".to_string(),
            serde_json::Value::String("run team-agent claim-leader or team-agent takeover".to_string()),
        );
    }
    out.insert("notification_event_id".to_string(), serde_json::Value::Null);
    Ok(serde_json::Value::Object(out))
}

fn insert_result_if_absent(
    conn: &rusqlite::Connection,
    result_id: &str,
    task_id: &str,
    agent_id: &str,
    envelope: &str,
    status: &str,
    owner_team_id: Option<&str>,
) -> Result<bool, MessagingError> {
    let rows = conn.execute(
        "insert or ignore into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            result_id,
            owner_team_id,
            task_id,
            agent_id,
            envelope,
            status,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(rows == 1)
}

fn format_report_result_notification(
    result_id: &str,
    task_id: &str,
    agent_id: &str,
    status: &str,
    envelope: &serde_json::Value,
) -> String {
    let summary = envelope
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut lines = vec![format!(
        "Task {task_id} reported {status} from {agent_id}: {summary}"
    )];
    if let Some(tests) = format_report_result_tests(envelope) {
        lines.push(tests);
    }
    lines.push(format!("Result id: {result_id}"));
    lines.push(
        "Team Agent stored this result. The coordinator/collect path will update team_state.md; no manual polling loop is needed."
            .to_string(),
    );
    lines.join("\n")
}

fn format_report_result_tests(envelope: &serde_json::Value) -> Option<String> {
    let tests = envelope.get("tests").and_then(serde_json::Value::as_array)?;
    let parts = tests
        .iter()
        .filter_map(|test| {
            let command = test.get("command").and_then(serde_json::Value::as_str)?;
            let status = test.get("status").and_then(serde_json::Value::as_str)?;
            Some(format!("{command}={status}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Tests: {}", parts.join(", ")))
    }
}

/// `_collect_results_and_notify_watchers` (`results.py:430`):coordinator tick 调用 —— collect +
/// notify_result_watchers 编排。daemon-path → Result。
pub fn collect_results_and_notify_watchers(
    workspace: &Path,
    event_log: &EventLog,
) -> Result<serde_json::Value, MessagingError> {
    let notified = retry_result_deliveries(workspace, event_log)?;
    Ok(serde_json::json!({
        "ok": true,
        "collected": 0,
        "notified": notified
    }))
}
