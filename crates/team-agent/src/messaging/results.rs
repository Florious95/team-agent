//!
//! results.py — report_result + envelope 校验编排 (card §66/§67)。

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::ids::TaskId;

use super::helpers::{next_result_id, required_str, validate_result_envelope};
use super::types::SEND_RETRY_MAX_ATTEMPTS;
use super::MessagingError;
use crate::state::projection::OwnerTeamResolution;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultRoute {
    Leader,
    Pipeline,
}

impl ResultRoute {
    pub const ALL: [Self; 2] = [Self::Leader, Self::Pipeline];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Pipeline => "pipeline",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "pipeline" => Some(Self::Pipeline),
            _ => None,
        }
    }
}

impl Default for ResultRoute {
    fn default() -> Self {
        Self::Leader
    }
}

/// Collect legacy persisted results synchronously without starting a background service.
pub fn collect(
    workspace: &Path,
    result_file: Option<&Path>,
) -> Result<serde_json::Value, MessagingError> {
    collect_scoped(workspace, result_file, None)
}

pub fn collect_for_team(
    workspace: &Path,
    result_file: Option<&Path>,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    collect_scoped(workspace, result_file, owner_team_id)
}

fn collect_scoped(
    workspace: &Path,
    result_file: Option<&Path>,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
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

fn task_exists_for_owner(state: &serde_json::Value, owner_team: &str, task_id: &str) -> bool {
    state
        .get("teams")
        .and_then(|teams| teams.get(owner_team))
        .is_some_and(|team| task_exists(team, task_id))
        || task_exists(state, task_id)
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

/// `report_result` (`results.py:191`): worker 报结果 —— 校验 envelope、持久化并自动结案。
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
    _fallback_primary_error: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

fn report_owner_state(state: &serde_json::Value, owner_team: &str) -> serde_json::Value {
    let mut state = match crate::state::projection::resolve_owner_team_id(state, owner_team)
        .canonical_key()
    {
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

fn leader_result_presentation(task_id: &str) -> super::presentation::PresentationDecision {
    super::presentation::decide_presentation(
        &super::presentation::PresentationRequest {
            sink: super::presentation::PresentationSink::Leader,
            class: super::presentation::PresentationClass::StageResult,
            case_id: Some(task_id.to_string()),
        },
        super::presentation::PresentationSource::ReportResult,
    )
}

fn format_report_result_leader_summary(
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
    format!("Task {task_id} reported {status} from {agent_id}: {summary}; Result id: {result_id}")
}

fn auto_finalize_result(
    workspace: &Path,
    conn: &rusqlite::Connection,
    state: &serde_json::Value,
    owner_team: &str,
    result_id: &str,
    task_id: &str,
    agent_id: &str,
) -> Result<Option<&'static str>, MessagingError> {
    let message_scoped = is_message_scoped_result(conn, task_id, agent_id, Some(owner_team))?;
    let scope = if message_scoped { "message" } else { "task" };
    let projected_task_id = if message_scoped {
        crate::mcp_server::latest_task_for_assignee(workspace, agent_id, Some(owner_team))
    } else if task_exists_for_owner(state, owner_team, task_id) {
        Some(task_id.to_string())
    } else {
        None
    };
    if let Some(projected_task_id) = projected_task_id {
        crate::state::repository::StateRepository::new(workspace).commit(
            crate::state::repository::StateWriteIntent::ResultCollection {
                owner_team_id: Some(owner_team),
            },
            |latest| mark_task_done(latest, &projected_task_id, result_id),
        )?;
    }
    let finalized = conn.execute(
        "update results set status = 'collected' where result_id = ?1 and owner_team_id = ?2 and status not in ('collected', 'invalid')",
        params![result_id, owner_team],
    )?;
    if finalized == 0 {
        return Ok(None);
    }
    EventLog::new(workspace).write(
        "collect.result",
        serde_json::json!({
            "result_id": result_id,
            "task_id": task_id,
            "agent_id": agent_id,
            "scope": scope,
        }),
    )?;
    Ok(Some(scope))
}

fn task_result_route(
    state: &serde_json::Value,
    owner_team: &str,
    task_id: &str,
) -> Result<(ResultRoute, bool), MessagingError> {
    let scoped_tasks = state
        .get("teams")
        .and_then(|teams| teams.get(owner_team))
        .and_then(|team| team.get("tasks"))
        .and_then(serde_json::Value::as_array);
    let top_level_tasks = state.get("tasks").and_then(serde_json::Value::as_array);
    let task = scoped_tasks
        .into_iter()
        .flatten()
        .chain(top_level_tasks.into_iter().flatten())
        .find(|task| task.get("id").and_then(serde_json::Value::as_str) == Some(task_id));
    let Some(route) = task.and_then(|task| task.get("result_route")) else {
        return Ok((ResultRoute::default(), false));
    };
    let route = route.as_str().ok_or_else(|| {
        MessagingError::Validation(format!(
            "task {task_id} has non-string result_route and cannot accept results"
        ))
    })?;
    ResultRoute::parse(route)
        .map(|route| (route, true))
        .ok_or_else(|| {
            MessagingError::Validation(format!(
                "task {task_id} has unknown result_route {route:?} and cannot accept results"
            ))
        })
}

fn copy_report_attribution_fields(
    envelope: &serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    for key in [
        "attributed_message_id",
        "attribution_scope",
        "task_id_source",
    ] {
        if let Some(value) = envelope.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
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
    if let Some(warnings) = format_report_result_warnings(envelope) {
        lines.push(warnings);
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

fn format_report_result_warnings(envelope: &serde_json::Value) -> Option<String> {
    let parts = report_result_array(envelope, "warnings")?
        .iter()
        .filter_map(|warning| {
            let code = report_field(warning, "code")?;
            let field = report_field(warning, "field").unwrap_or("result");
            let severity = report_field(warning, "severity").unwrap_or("warning");
            let advisory =
                if warning.get("advisory").and_then(serde_json::Value::as_bool) == Some(true) {
                    " advisory"
                } else {
                    ""
                };
            Some(format!("{severity}{advisory} {field}: {code}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(format!("Verification warnings: {}", parts.join(", ")))
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

#[cfg(test)]
mod collection_tests;

#[cfg(test)]
mod tests {
    use super::{format_report_result_notification, report_result};
    use crate::message_store::MessageStore;

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
            "warnings": [
                {
                    "code": "result_success_without_executed_tests",
                    "field": "tests",
                    "severity": "warning",
                    "advisory": true
                }
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
        assert!(notification.contains(
            "Verification warnings: warning advisory tests: result_success_without_executed_tests"
        ));
        assert!(notification.contains("Changes: modified src/a.rs: patched delivery"));
        assert!(notification.contains("Risks: low: none known"));
        assert!(notification.contains("Artifacts: .team/artifacts/evidence.md: evidence"));
        assert!(notification.contains("Next actions: ship after review"));
        assert!(notification.contains("Result id: res_1"));
    }

    #[test]
    fn leader_result_summary_preserves_long_multiline_text_verbatim() {
        let source = format!("首行\nbranch: fix/keyboard-receive-and-compose\ncommit: {}", "a1b2c3d4e5f67890".repeat(24));
        let envelope = serde_json::json!({"summary": source});
        let summary = super::format_report_result_leader_summary(
            "res-1", "task-1", "worker", "success", &envelope,
        );
        let source = envelope["summary"].as_str().unwrap();
        assert_eq!(summary, format!("Task task-1 reported success from worker: {source}; Result id: res-1"));
        assert!(summary.len() > 160);
        assert!(summary.contains("\nbranch: fix/keyboard-receive-and-compose\ncommit: "));
        assert!(summary.contains("a1b2c3d4e5f67890".repeat(24).as_str()));
    }

}
