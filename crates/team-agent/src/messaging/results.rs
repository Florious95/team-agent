//! results.py — collect + report_result + envelope 校验编排 (card §66/§67)。

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;

use super::helpers::{next_result_id, required_str, validate_result_envelope};
use super::types::SEND_RETRY_MAX_ATTEMPTS;
use super::watchers::retry_result_deliveries;
use super::MessagingError;
use crate::model::ids::TaskId;
use crate::state::projection::OwnerTeamResolution;

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
    let paths = collect_paths(workspace)?;
    let log = EventLog::new(&paths.run_workspace);
    let resolved_owner_team_id = match owner_team_id.filter(|team| !team.is_empty()) {
        Some(team) => Some(resolve_owner_team_for_read(
            &paths.run_workspace,
            team,
            Some(&log),
        )?),
        None => None,
    };
    let owner_team_id = resolved_owner_team_id.as_deref();
    let mut state = match owner_team_id {
        Some(team) => {
            crate::state::projection::select_runtime_state(&paths.run_workspace, Some(team))?
        }
        None => crate::state::persist::load_runtime_state(&paths.run_workspace)?,
    };
    let spec_workspace = owner_team_id
        .and_then(|_| state_spec_workspace_from_value(&state))
        .unwrap_or_else(|| paths.spec_workspace.clone());
    let spec_path = spec_workspace.join("team.spec.yaml");
    if !spec_path.exists() {
        return Err(MessagingError::Validation(format!(
            "Cannot read {}",
            spec_path.display()
        )));
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
    let mut fatal_invalid_results = 0usize;
    let mut state_dirty = false;
    let mut task_updates = Vec::new();
    for row in rows {
        let envelope: serde_json::Value = match serde_json::from_str(&row.envelope) {
            Ok(envelope) => envelope,
            Err(error) => {
                fatal_invalid_results = fatal_invalid_results.saturating_add(1);
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
            fatal_invalid_results = fatal_invalid_results.saturating_add(1);
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
            if result_file.is_some() || row.task_id != "manual" {
                fatal_invalid_results = fatal_invalid_results.saturating_add(1);
            }
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
            task_updates.push((row.task_id.clone(), row.result_id.clone()));
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
            crate::state::projection::save_team_scoped_state_reapplying_after_conflict(
                &paths.run_workspace,
                &state,
                |latest| {
                    for (task_id, result_id) in &task_updates {
                        mark_task_done(latest, task_id, result_id);
                    }
                },
            )?;
        } else {
            crate::state::persist::save_runtime_state_reapplying_after_conflict(
                &paths.run_workspace,
                &state,
                |latest| {
                    for (task_id, result_id) in &task_updates {
                        mark_task_done(latest, task_id, result_id);
                    }
                },
            )?;
        }
    }
    let counts = result_counts(&conn, owner_team_id)?;
    // results.py:157 — ensure_coordinator=true runs the REAL ensure step; the
    // `{ok:false,status:"not_required"}` literal is ONLY the ensure=false branch.
    let coordinator = if ensure_coordinator {
        ensure_coordinator_after_collect(&paths.run_workspace, &state, &log)
    } else {
        serde_json::json!({"ok": false, "status": "not_required"})
    };
    Ok(serde_json::json!({
        "ok": fatal_invalid_results == 0,
        "collected": collected,
        "collected_results": collected_results,
        "delivered_messages": [],
        "invalid_results": invalid_results,
        "results": counts,
        "state_file": spec_workspace.join("team_state.md").to_string_lossy().to_string(),
        "coordinator": coordinator,
    }))
}

/// `_ensure_coordinator_after_collect`(`results.py:176-184`)。
fn ensure_coordinator_after_collect(
    workspace: &Path,
    state: &serde_json::Value,
    log: &EventLog,
) -> serde_json::Value {
    if !coordinator_should_run(state) {
        return serde_json::json!({"ok": false, "status": "not_required"});
    }
    let workspace_path = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let coordinator = match crate::coordinator::start_coordinator(&workspace_path) {
        Ok(report) => start_report_value(&report),
        Err(e) => {
            serde_json::json!({"ok": false, "status": "start_failed", "error": e.to_string()})
        }
    };
    let _ = log.write(
        "collect.coordinator_checked",
        serde_json::json!({"coordinator": coordinator.clone()}),
    );
    coordinator
}

/// `_coordinator_should_run`(`results.py:187-188`)。
fn coordinator_should_run(state: &serde_json::Value) -> bool {
    let has_session = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty());
    has_session || leader_receiver_is_direct(state.get("leader_receiver"))
}

/// `_leader_receiver_is_direct`(`messaging/leader.py:449-450`)。
fn leader_receiver_is_direct(receiver: Option<&serde_json::Value>) -> bool {
    receiver.is_some_and(|receiver| {
        receiver.get("mode").and_then(serde_json::Value::as_str) == Some("direct_tmux")
            && receiver
                .get("pane_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|pane| !pane.is_empty())
    })
}

/// `start_coordinator` dict 形(`lifecycle.py:54/86/121` 的 JSON 面)。
fn start_report_value(report: &crate::coordinator::StartReport) -> serde_json::Value {
    let status = match report.status {
        crate::coordinator::StartOutcome::AlreadyRunning => "already_running",
        crate::coordinator::StartOutcome::RestartIncompatibleStopFailed => {
            "restart_incompatible_stop_failed"
        }
        crate::coordinator::StartOutcome::SchemaIncompatible => "schema_incompatible",
        crate::coordinator::StartOutcome::Started => "started",
    };
    let mut value = serde_json::json!({
        "ok": report.ok,
        "pid": report.pid.map(|p| p.get()),
        "status": status,
    });
    if let Some(log) = &report.log {
        value["log"] = serde_json::json!(log.to_string_lossy().to_string());
    }
    if let Some(error) = &report.schema_error {
        value["schema_error"] = serde_json::json!(format!("{error:?}"));
    }
    if let Some(action) = &report.action {
        value["action"] = serde_json::json!(action);
    }
    value
}

fn resolve_owner_team_for_read(
    workspace: &Path,
    requested: &str,
    event_log: Option<&EventLog>,
) -> Result<String, MessagingError> {
    let state = crate::state::persist::load_runtime_state(workspace)?;
    match crate::state::projection::resolve_owner_team_id(&state, requested) {
        OwnerTeamResolution::Canonical(canonical) => Ok(canonical),
        OwnerTeamResolution::LegacyAlias {
            requested,
            canonical,
        } => {
            crate::messaging::delivery::normalize_owner_team_id_rows(
                workspace, &requested, &canonical, None, event_log,
            )?;
            Ok(canonical)
        }
        OwnerTeamResolution::Unresolved { requested } => Err(MessagingError::Routing(format!(
            "owner_team_unresolved: {requested}"
        ))),
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
    let Some(tasks) = state
        .get_mut("tasks")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for task in tasks {
        if task.get("id").and_then(serde_json::Value::as_str) != Some(task_id) {
            continue;
        }
        if let Some(obj) = task.as_object_mut() {
            obj.insert(
                "status".to_string(),
                serde_json::Value::String("done".to_string()),
            );
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
    let row_mapper =
        |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
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
    report_result_for_owner_team(workspace, envelope, None)
}

pub fn report_result_for_owner_team(
    workspace: &Path,
    envelope: &serde_json::Value,
    explicit_owner_team: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    report_result_for_owner_team_inner(workspace, envelope, explicit_owner_team, None)
}

pub fn report_result_for_owner_team_with_primary_error(
    workspace: &Path,
    envelope: &serde_json::Value,
    explicit_owner_team: Option<&str>,
    fallback_primary_error: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    report_result_for_owner_team_inner(
        workspace,
        envelope,
        explicit_owner_team,
        fallback_primary_error,
    )
}

fn report_result_for_owner_team_inner(
    workspace: &Path,
    envelope: &serde_json::Value,
    explicit_owner_team: Option<&str>,
    fallback_primary_error: Option<&str>,
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
            obj.insert(
                "result_id".to_string(),
                serde_json::Value::String(result_id.clone()),
            );
        }
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let state_for_owner =
        crate::state::persist::load_runtime_state(workspace).unwrap_or(serde_json::json!({}));
    let owner_team = explicit_owner_team
        .filter(|team| !team.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| super::leader_receiver::active_team_key(workspace, &state_for_owner));
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
                "owner_team_id": owner_team,
                "result_id": result_id,
            }),
        )?;
        let mut out = serde_json::Map::new();
        out.insert("ok".to_string(), serde_json::Value::Bool(true));
        out.insert(
            "status".to_string(),
            serde_json::Value::String("duplicate_ignored".to_string()),
        );
        out.insert(
            "result_id".to_string(),
            serde_json::Value::String(result_id),
        );
        out.insert(
            "task_id".to_string(),
            serde_json::Value::String(task_id.to_string()),
        );
        out.insert(
            "agent_id".to_string(),
            serde_json::Value::String(agent_id.to_string()),
        );
        out.insert("acknowledged_messages".to_string(), serde_json::json!([]));
        out.insert(
            "leader_notified".to_string(),
            serde_json::Value::Bool(false),
        );
        out.insert(
            "notification_message_id".to_string(),
            serde_json::Value::Null,
        );
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
    let content =
        format_report_result_notification(&result_id, task_id, agent_id, status, envelope);
    let state = report_owner_state(&state_for_owner, &owner_team);
    let event_log = EventLog::new(workspace);
    let mut outcome = match super::leader_receiver::send_to_leader_receiver(
        workspace,
        &state,
        "leader",
        &content,
        Some(&TaskId::new(task_id.to_string())),
        agent_id,
        false,
        Some(&result_id),
        &event_log,
    ) {
        Ok(outcome) => outcome,
        Err(error) if report_state_has_app_server_receiver(&state) => {
            let message_id = format!("fallback:{result_id}");
            crate::messaging::DeliveryOutcome {
                ok: false,
                status: crate::messaging::DeliveryStatus::Failed,
                message_status: super::helpers::MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id),
                verification: Some(format!("leader_funnel_error:{error}")),
                stage: None,
                reason: Some(crate::messaging::DeliveryRefusal::LeaderNotAttached),
                channel: Some("codex_app_server".to_string()),
            }
        }
        Err(error) => {
            let message_id = format!("fallback:{result_id}");
            let fallback_error = fallback_primary_error_text(
                fallback_primary_error,
                format!("leader_funnel_error:{error}"),
            );
            super::leader_receiver::deliver_to_leader_fallback_pane(
                workspace,
                &state,
                &message_id,
                Some(&result_id),
                &content,
                false,
                Some(&fallback_error),
                &event_log,
            )?
        }
    };
    if let Some(message_id) = outcome.message_id.clone() {
        let store = MessageStore::open(workspace)?;
        let transport = crate::tmux_backend::TmuxBackend::for_workspace(workspace);
        let delivery_state_raw = crate::state::persist::load_runtime_state(workspace)
            .unwrap_or_else(|_| state_for_owner.clone());
        let delivery_state = report_owner_state(&delivery_state_raw, &owner_team);
        let mut primary_error: Option<String> = None;
        for attempt in 0..3 {
            let _ = store.mark(&message_id, "accepted", None);
            match super::delivery::deliver_pending_message(
                workspace,
                &store,
                &transport,
                &message_id,
                &event_log,
                &delivery_state,
            ) {
                Ok(next) => outcome = next,
                Err(error) => {
                    primary_error = Some(format!("primary_delivery_error:{error}"));
                    outcome = crate::messaging::DeliveryOutcome {
                        ok: false,
                        status: crate::messaging::DeliveryStatus::Failed,
                        message_status: super::helpers::MessageStatusShadow("failed".to_string()),
                        message_id: Some(message_id.clone()),
                        verification: Some(error.to_string()),
                        stage: None,
                        reason: None,
                        channel: Some("leader_receiver".to_string()),
                    };
                    break;
                }
            }
            if outcome.ok {
                break;
            }
            match super::delivery::deliver_pending_messages(
                workspace,
                &delivery_state,
                &transport,
                &event_log,
            ) {
                Ok(delivered)
                    if delivered
                        .iter()
                        .any(|delivered_id| delivered_id == &message_id) =>
                {
                    outcome = crate::messaging::DeliveryOutcome {
                        ok: true,
                        status: crate::messaging::DeliveryStatus::Delivered,
                        message_status: super::helpers::MessageStatusShadow(
                            "delivered".to_string(),
                        ),
                        message_id: Some(message_id.clone()),
                        verification: None,
                        stage: None,
                        reason: None,
                        channel: Some("leader_receiver".to_string()),
                    };
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    primary_error = Some(format!("primary_delivery_error:{error}"));
                    outcome = crate::messaging::DeliveryOutcome {
                        ok: false,
                        status: crate::messaging::DeliveryStatus::Failed,
                        message_status: super::helpers::MessageStatusShadow("failed".to_string()),
                        message_id: Some(message_id.clone()),
                        verification: Some(error.to_string()),
                        stage: None,
                        reason: None,
                        channel: Some("leader_receiver".to_string()),
                    };
                    break;
                }
            }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        if !outcome.ok {
            if !report_state_has_app_server_receiver(&delivery_state) {
                let fallback_error = primary_error.unwrap_or_else(|| {
                    format!(
                        "leader_notification_primary_failed:{}",
                        super::helpers::status_wire(outcome.status)
                    )
                });
                let fallback_error =
                    fallback_primary_error_text(fallback_primary_error, fallback_error);
                outcome = super::leader_receiver::deliver_to_leader_fallback_pane(
                    workspace,
                    &delivery_state,
                    &message_id,
                    Some(&result_id),
                    &content,
                    false,
                    Some(&fallback_error),
                    &event_log,
                )?;
            }
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
    let channel = outcome
        .channel
        .clone()
        .unwrap_or_else(|| "leader_receiver".to_string());
    event_log.write(
        "mcp.report_result",
        serde_json::json!({
            "leader_notified": leader_notified,
            "notification_channel": channel,
            "notification_message_id": outcome.message_id,
            "notification_status": notification_status_wire,
            "owner_team_id": owner_team,
            "result_id": result_id,
        }),
    )?;

    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), serde_json::Value::Bool(true));
    out.insert(
        "result_id".to_string(),
        serde_json::Value::String(result_id),
    );
    out.insert(
        "task_id".to_string(),
        serde_json::Value::String(task_id.to_string()),
    );
    out.insert(
        "agent_id".to_string(),
        serde_json::Value::String(agent_id.to_string()),
    );
    out.insert("acknowledged_messages".to_string(), serde_json::json!([]));
    out.insert(
        "leader_notified".to_string(),
        serde_json::Value::Bool(leader_notified),
    );
    out.insert(
        "notification_message_id".to_string(),
        outcome
            .message_id
            .map_or(serde_json::Value::Null, serde_json::Value::String),
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
            serde_json::Value::String(
                "run team-agent claim-leader or team-agent takeover".to_string(),
            ),
        );
    }
    out.insert("notification_event_id".to_string(), serde_json::Value::Null);
    Ok(serde_json::Value::Object(out))
}

fn fallback_primary_error_text(cli_primary_error: Option<&str>, observed: String) -> String {
    match cli_primary_error.filter(|error| !error.trim().is_empty()) {
        Some(error) => format!("{error}; {observed}"),
        None => observed,
    }
}

fn report_owner_state(state: &serde_json::Value, owner_team: &str) -> serde_json::Value {
    let mut state =
        match crate::state::projection::resolve_owner_team_id(state, owner_team).canonical_key() {
            Some(team) => crate::state::projection::project_top_level_view(state, team),
            None => state.clone(),
        };
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "active_team_key".to_string(),
            serde_json::Value::String(owner_team.to_string()),
        );
    }
    state
}

fn report_state_has_app_server_receiver(state: &serde_json::Value) -> bool {
    report_leader_receiver_value(state).is_some_and(crate::codex_app_server::receiver_is_app_server)
}

fn report_leader_receiver_value(state: &serde_json::Value) -> Option<&serde_json::Value> {
    state
        .get("leader_receiver")
        .or_else(|| {
            state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .and_then(|team| state.get("teams").and_then(|teams| teams.get(team)))
                .and_then(|team| team.get("leader_receiver"))
        })
        .or_else(|| {
            let teams = state.get("teams").and_then(serde_json::Value::as_object)?;
            if teams.len() == 1 {
                teams
                    .values()
                    .next()
                    .and_then(|team| team.get("leader_receiver"))
            } else {
                None
            }
        })
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
    if let Some(changes) = format_report_result_changes(envelope) {
        lines.push(changes);
    }
    if let Some(risks) = format_report_result_risks(envelope) {
        lines.push(risks);
    }
    if let Some(artifacts) = format_report_result_artifacts(envelope) {
        lines.push(artifacts);
    }
    if let Some(next_actions) = format_report_result_next_actions(envelope) {
        lines.push(next_actions);
    }
    lines.push(format!("Result id: {result_id}"));
    lines.push(
        "Team Agent stored this result. The coordinator/collect path will update team_state.md; no manual polling loop is needed."
            .to_string(),
    );
    lines.join("\n")
}

fn format_report_result_tests(envelope: &serde_json::Value) -> Option<String> {
    let tests = envelope
        .get("tests")
        .and_then(serde_json::Value::as_array)?;
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

fn report_result_array<'a>(
    envelope: &'a serde_json::Value,
    key: &str,
) -> Option<&'a Vec<serde_json::Value>> {
    let values = envelope.get(key).and_then(serde_json::Value::as_array)?;
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn report_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

fn report_field_any<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| report_field(value, key))
}

fn format_report_result_changes(envelope: &serde_json::Value) -> Option<String> {
    let parts = report_result_array(envelope, "changes")?
        .iter()
        .filter_map(|change| {
            let path = report_field_any(change, &["path", "file", "filepath", "filename"])?;
            let kind = report_field_any(change, &["kind", "type", "action"]).unwrap_or("changed");
            let description = report_field_any(
                change,
                &["description", "summary", "detail", "details", "message"],
            )
            .unwrap_or(path);
            Some(format!("{kind} {path}: {description}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Changes: {}", parts.join(", ")))
    }
}

fn format_report_result_risks(envelope: &serde_json::Value) -> Option<String> {
    let parts = report_result_array(envelope, "risks")?
        .iter()
        .filter_map(|risk| {
            let severity = report_field_any(risk, &["severity", "level"]).unwrap_or("low");
            let description =
                report_field_any(risk, &["description", "summary", "detail", "message"])?;
            Some(format!("{severity}: {description}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Risks: {}", parts.join(", ")))
    }
}

fn format_report_result_artifacts(envelope: &serde_json::Value) -> Option<String> {
    let parts = report_result_array(envelope, "artifacts")?
        .iter()
        .filter_map(|artifact| {
            let path = report_field_any(artifact, &["path", "file", "filepath", "filename"])?;
            let description =
                report_field_any(artifact, &["description", "summary", "detail"]).unwrap_or(path);
            Some(format!("{path}: {description}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Artifacts: {}", parts.join(", ")))
    }
}

fn format_report_result_next_actions(envelope: &serde_json::Value) -> Option<String> {
    let parts = report_result_array(envelope, "next_actions")?
        .iter()
        .filter_map(|action| {
            report_field_any(
                action,
                &["description", "summary", "action", "todo", "message"],
            )
            .map(|text| text.to_string())
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Next actions: {}", parts.join(", ")))
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

#[cfg(test)]
mod tests {
    use super::format_report_result_notification;

    #[test]
    fn report_result_notification_includes_full_envelope_sections() {
        let envelope = serde_json::json!({
            "schema_version": "result_envelope_v1",
            "task_id": "task-1",
            "agent_id": "worker",
            "status": "success",
            "summary": "done",
            "changes": [
                {"path": "src/a.rs", "kind": "modified", "description": "patched delivery"}
            ],
            "tests": [
                {"command": "cargo test", "status": "passed"}
            ],
            "risks": [
                {"severity": "low", "description": "none known"}
            ],
            "artifacts": [
                {"path": ".team/artifacts/evidence.md", "description": "evidence"}
            ],
            "next_actions": [
                {"description": "ship after review"}
            ]
        });
        let notification =
            format_report_result_notification("res_1", "task-1", "worker", "success", &envelope);
        assert!(notification.contains("Task task-1 reported success from worker: done"));
        assert!(notification.contains("Tests: cargo test=passed"));
        assert!(notification.contains("Changes: modified src/a.rs: patched delivery"));
        assert!(notification.contains("Risks: low: none known"));
        assert!(notification.contains("Artifacts: .team/artifacts/evidence.md: evidence"));
        assert!(notification.contains("Next actions: ship after review"));
        assert!(notification.contains("Result id: res_1"));
    }
}
