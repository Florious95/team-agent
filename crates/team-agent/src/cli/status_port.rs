//! status_port extracted from cli::mod inline placeholder.
use super::*;
use crate::state::projection::OwnerTeamResolution;
use crate::transport::Transport;
use rusqlite::params;

    /// `status.status(workspace, as_json, compact)`(`queries.py:33`,**有副作用**:capture→refresh→save)。
    pub fn status(workspace: &Path, compact: bool, detail: bool) -> Result<Value, CliError> {
        let state = read_runtime_state(workspace);
        status_scoped(workspace, &state, None, compact, detail)
    }

    pub fn status_scoped(
        workspace: &Path,
        state: &Value,
        owner_team_id: Option<&str>,
        compact: bool,
        detail: bool,
    ) -> Result<Value, CliError> {
        // commands.py:99 — `--json --detail` maps to compact=False: detail wins and
        // returns the FULL payload.
        let compact = compact && !detail;
        let resolved_owner_team_id = resolve_status_owner_team(workspace, owner_team_id)?;
        let owner_team_id = resolved_owner_team_id.as_deref().or(owner_team_id);
        let health = crate::coordinator::coordinator_health(
            &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
        );
        let store = crate::message_store::MessageStore::open(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let conn = crate::db::schema::open_db(store.db_path())
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        // B-5 / 036b N38 explicable — status 出口 runtime 块:把 coordinator_health
        // (现状)+ undelivered backlog count 一起暴露;coordinator not running ∧
        // backlog>0 才挂 down-hint(anti-nag)。auto-recovery 不做(user 已裁)。
        let coordinator_running = coordinator_status_running(&health);
        let undelivered_backlog = count_undelivered_backlog(&conn, owner_team_id)?;
        let runtime_block = build_runtime_status_block(
            coordinator_running,
            undelivered_backlog,
        );
        let agents = enrich_agents(state.get("agents"));
        let tasks = state
            .get("tasks")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let leader_receiver = state
            .get("leader_receiver")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let session_name = state.get("session_name").cloned().unwrap_or(Value::Null);
        let is_external_leader = crate::state::projection::state_is_external_leader(state);
        let leader_topology = if is_external_leader { "external" } else { "managed" };
        let leader_attach_command = if is_external_leader {
            None
        } else {
            let window_name = state
                .pointer("/leader_receiver/window_name")
                .and_then(Value::as_str)
                .unwrap_or("leader");
            session_name.as_str().and_then(|session| {
                crate::tmux_backend::attach_command_for_workspace(
                    workspace,
                    &crate::transport::SessionName::new(session.to_string()),
                    window_name,
                )
            })
        };
        let tmux_present = tmux_session_present(workspace, session_name.as_str());
        let mut readiness_state = state.clone();
        if let Some(obj) = readiness_state.as_object_mut() {
            obj.insert("tmux_session_present".to_string(), serde_json::json!(tmux_present));
        }
        let readiness = crate::cli::diagnose::wait_readiness(&readiness_state);
        let full = json!({
            "ok": true,
            "team": state.pointer("/leader/id").cloned().unwrap_or_else(|| json!("leader")),
            "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
            "leader_topology": leader_topology,
            "is_external_leader": is_external_leader,
            "leader_attach_command": leader_attach_command,
            "leader_client": state.get("leader_client").cloned().unwrap_or(Value::Null),
            "tmux_session_present": tmux_present,
            "all_spawned": readiness.get("all_spawned").cloned().unwrap_or(Value::Bool(false)),
            "all_attached_receiver": readiness.get("all_attached_receiver").cloned().unwrap_or(Value::Bool(true)),
            "all_resumable_have_session": readiness.get("all_resumable_have_session").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_complete": readiness.get("session_capture_complete").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_incomplete": readiness.get("session_capture_incomplete").cloned().unwrap_or(Value::Bool(false)),
            "incomplete_session_capture_agents": readiness.get("incomplete_session_capture_agents").cloned().unwrap_or_else(|| json!([])),
            "pending_session_agent_ids": readiness.get("pending_session_agent_ids").cloned().unwrap_or_else(|| json!([])),
            "leader_receiver": leader_receiver,
            "teams": state.get("teams").cloned().unwrap_or_else(|| json!({})),
            "agents": agents,
            "agent_health": agent_health(&conn, owner_team_id)?,
            "tasks": tasks,
            "messages": message_counts(&conn, owner_team_id)?,
            "queued_messages": queued_messages(&conn, owner_team_id, 8)?,
            "results": result_counts(&conn, owner_team_id)?,
            "latest_results": latest_result_summaries(&store, owner_team_id)?,
            "readiness": readiness,
            "coordinator": coordinator_health_value(health),
            "runtime": runtime_block,
            "reminder": crate::cli::STATUS_REMINDER,
            "last_events": Value::Array(
                crate::event_log::EventLog::new(workspace)
                    .tail(10)
                    .map_err(|e| CliError::Runtime(e.to_string()))?,
            ),
        });
        if compact {
            Ok(compact_status(full))
        } else {
            Ok(full)
        }
    }
    /// `status.format_status(workspace, agent)`(人读)。
    pub fn format_status(workspace: &Path, agent: Option<&str>) -> Result<String, CliError> {
        let state = read_runtime_state(workspace);
        format_status_scoped(workspace, &state, None, agent)
    }

    pub fn format_status_scoped(
        workspace: &Path,
        state: &Value,
        owner_team_id: Option<&str>,
        agent: Option<&str>,
    ) -> Result<String, CliError> {
        match agent {
            // queries.py:130-162 — the agent branch renders the multi-line agent detail
            // from the FULL status payload; an unknown agent id errors.
            Some(agent) => {
                let status = status_scoped(workspace, state, owner_team_id, false, false)?;
                format_agent_status(workspace, &status, agent)
            }
            None => {
                let status = status_scoped(workspace, state, owner_team_id, false, false)?;
                Ok(crate::cli::format_status_csv(&status))
            }
        }
    }

    /// `format_status` agent 分支(`queries.py:135-162`)。
    fn format_agent_status(
        workspace: &Path,
        status: &Value,
        agent_id: &str,
    ) -> Result<String, CliError> {
        let agents = status.get("agents").and_then(Value::as_object);
        let health = status.get("agent_health").and_then(Value::as_object);
        let known = agents.is_some_and(|map| map.contains_key(agent_id))
            || health.is_some_and(|map| map.contains_key(agent_id));
        if !known {
            return Err(CliError::Runtime(format!("unknown agent id: {agent_id}")));
        }
        let empty = json!({});
        let agent = agents
            .and_then(|map| map.get(agent_id))
            .unwrap_or(&empty);
        let row = health.and_then(|map| map.get(agent_id)).unwrap_or(&empty);
        let status_text = row
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(||

                agent_health_status_text(agent.get("status").and_then(Value::as_str).unwrap_or(""))
            );
        let tasks = status.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
        let task_id = current_task_for_agent(&tasks, agent_id).unwrap_or_else(|| "-".to_string());
        let inbox_rows = crate::message_store::MessageStore::open(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?
            .inbox(agent_id, 3, None)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut lines = vec![
            format!("{agent_id}  {status_text}"),
            format!("  provider: {}", py_get(agent, "provider")),
            format!("  model: {}", py_get(agent, "model")),
            format!("  profile: {}", py_get(agent, "profile")),
            format!("  session_id: {}", py_get_or_dash(agent, "session_id")),
            format!("  captured_via: {}", py_get_or_dash(agent, "captured_via")),
            format!(
                "  attribution_confidence: {}",
                py_get_or_dash(agent, "attribution_confidence")
            ),
            format!("  task: {task_id}"),
            format!("  handoff: {}", py_get(agent, "handoff_path")),
            "  recent messages:".to_string(),
        ];
        if inbox_rows.is_empty() {
            lines.push("    none".to_string());
        } else {
            for item in &inbox_rows {
                let content = item.get("content").and_then(Value::as_str).unwrap_or("");
                let content: String = content.chars().take(120).collect();
                lines.push(format!(
                    "    {} {} -> {} {}: {content}",
                    py_get_or_dash(item, "created_at"),
                    py_get_or_dash(item, "sender"),
                    py_get_or_dash(item, "recipient"),
                    py_get_or_dash(item, "status"),
                ));
            }
        }
        Ok(lines.join("\n"))
    }

    /// `current_task_for_agent`(`approvals/status.py:127-132`)。
    fn current_task_for_agent(tasks: &[Value], agent_id: &str) -> Option<String> {
        const ACTIVE: [&str; 5] = ["pending", "ready", "running", "blocked", "needs_retry"];
        for task in tasks.iter().rev() {
            let assignee = task.get("assignee").and_then(Value::as_str);
            let status = task.get("status").and_then(Value::as_str).unwrap_or("pending");
            if assignee == Some(agent_id) && ACTIVE.contains(&status) {
                return task.get("id").and_then(Value::as_str).map(str::to_string);
            }
        }
        None
    }

    fn agent_health_status_text(status: &str) -> String {
        serde_json::to_value(crate::provider::agent_health_status(status))
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "-".to_string())
    }

    /// Python `agent.get(key, '-')`:键缺失 → `-`;键存在但为 null → 打印 `None`。
    fn py_get(agent: &Value, key: &str) -> String {
        match agent.get(key) {
            None => "-".to_string(),
            Some(Value::Null) => "None".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }
    }

    /// Python `agent.get(key) or '-'`:缺失/null/空串都落 `-`。
    fn py_get_or_dash(agent: &Value, key: &str) -> String {
        match agent.get(key) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => "-".to_string(),
        }
    }

    /// `latest_result_summaries`(`queries.py:83-89`)。
    fn latest_result_summaries(
        store: &crate::message_store::MessageStore,
        owner_team_id: Option<&str>,
    ) -> Result<Value, CliError> {
        let rows = store
            .latest_results(5, owner_team_id)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        Ok(Value::Array(
            rows.iter()
                .filter_map(crate::message_store::result_summary_from_row)
                .collect(),
        ))
    }
    /// `status.approvals(workspace, agent_id)`(JSON)/`format_approvals`(人读)。
    pub fn approvals(workspace: &Path, agent: Option<&str>, as_json: bool) -> Result<Value, CliError> {
        let _ = as_json;
        let state = read_runtime_state(workspace);
        let session = state.get("session_name").and_then(Value::as_str).filter(|s| !s.is_empty());
        let mut approvals = Vec::new();
        if let (Some(session), Some(agents)) = (session, state.get("agents").and_then(Value::as_object)) {
            let run_ws = crate::model::paths::canonical_run_workspace(workspace)
                .unwrap_or_else(|_| workspace.to_path_buf());
            let backend = crate::tmux_backend::TmuxBackend::for_workspace(&run_ws);
            for (agent_id, agent_state) in agents {
                if agent.is_some_and(|wanted| wanted != agent_id) {
                    continue;
                }
                let window = agent_window(agent_id, agent_state);
                let target = crate::transport::Target::SessionWindow {
                    session: crate::transport::SessionName::new(session.to_string()),
                    window: crate::transport::WindowName::new(window.clone()),
                };
                let Ok(captured) = backend.capture(&target, crate::transport::CaptureRange::Tail(120)) else {
                    continue;
                };
                if let Some(prompt) = crate::provider::extract_approval_prompt(agent_id, &captured.text) {
                    approvals.push(prompt.to_ordered_value());
                }
            }
        }
        let waiting_count = approvals.len();
        Ok(json!({
            "ok": true,
            "waiting": waiting_count > 0,
            "waiting_count": waiting_count,
            "approvals": approvals,
            "scan": {
                "mode": "tail",
                "lines": 120,
                "raw_output": false,
            },
        }))
    }

    pub fn format_approvals(value: &Value) -> String {
        let approvals = value
            .get("approvals")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if approvals.is_empty() {
            return "No pending approvals.".to_string();
        }
        approvals
            .iter()
            .map(|approval| {
                let agent = approval.get("agent_id").and_then(Value::as_str).unwrap_or("-");
                let kind = approval.get("kind").and_then(Value::as_str).unwrap_or("unknown");
                let prompt = approval
                    .get("prompt")
                    .and_then(Value::as_str)
                    .or_else(|| approval.get("subject").and_then(Value::as_str))
                    .unwrap_or("-");
                format!("{agent}: {kind} {prompt}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// `status.inbox(workspace, agent, limit, since)`(JSON)/`format_inbox`(人读)。
    pub fn inbox(
        workspace: &Path,
        agent: &str,
        limit: usize,
        since: Option<&str>,
        as_json: bool,
    ) -> Result<Value, CliError> {
        let _ = as_json;
        let store = crate::message_store::MessageStore::open(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut messages = store
            .inbox(agent, limit, None)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        if let Some(cutoff) = since.and_then(parse_rfc3339) {
            messages.retain(|message| {
                message
                    .get("created_at")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339)
                    .is_some_and(|created| created >= cutoff)
            });
        }
        Ok(json!({
            "ok": true,
            "agent_id": agent,
            "messages": messages,
            "since": since,
        }))
    }

    fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc3339(value).ok()
    }

    fn read_runtime_state(workspace: &Path) -> Value {
        let path = workspace.join(".team").join("runtime").join("state.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn resolve_status_owner_team(
        workspace: &Path,
        owner_team_id: Option<&str>,
    ) -> Result<Option<String>, CliError> {
        let Some(requested) = owner_team_id.filter(|team| !team.is_empty()) else {
            return Ok(None);
        };
        let state = read_runtime_state(workspace);
        match crate::state::projection::resolve_owner_team_id(&state, requested) {
            OwnerTeamResolution::Canonical(canonical) => Ok(Some(canonical)),
            OwnerTeamResolution::LegacyAlias { requested, canonical } => {
                let log = crate::event_log::EventLog::new(workspace);
                crate::messaging::delivery::normalize_owner_team_id_rows(
                    workspace,
                    &requested,
                    &canonical,
                    None,
                    Some(&log),
                )
                .map_err(CliError::from)?;
                Ok(Some(canonical))
            }
            OwnerTeamResolution::Unresolved { .. } | OwnerTeamResolution::Ambiguous { .. } => Ok(None),
        }
    }

    fn agent_window(agent_id: &str, agent_state: &Value) -> String {
        ["window", "window_name"]
            .iter()
            .find_map(|key| agent_state.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
            .unwrap_or(agent_id)
            .to_string()
    }

    fn enrich_agents(agents: Option<&Value>) -> Value {
        let Some(Value::Object(input)) = agents else {
            return json!({});
        };
        let mut out = Map::new();
        for (agent_id, value) in input {
            match value {
                Value::Object(obj) => {
                    let mut enriched = obj.clone();
                    enriched.insert(
                        "interacted".to_string(),
                        Value::String(interacted_marker(obj.get("first_send_at"))),
                    );
                    out.insert(agent_id.clone(), Value::Object(enriched));
                }
                _ => {
                    out.insert(agent_id.clone(), value.clone());
                }
            }
        }
        Value::Object(out)
    }

    fn interacted_marker(value: Option<&Value>) -> String {
        let Some(raw) = value.and_then(Value::as_str) else {
            return "never".to_string();
        };
        if raw.is_empty() {
            return "never".to_string();
        }
        if chrono::DateTime::parse_from_rfc3339(raw).is_ok() {
            raw.to_string()
        } else {
            "never".to_string()
        }
    }

    fn tmux_session_present(workspace: &Path, session_name: Option<&str>) -> bool {
        let Some(name) = session_name else {
            return false;
        };
        if name.is_empty() {
            return false;
        }
        let run_ws = crate::model::paths::canonical_run_workspace(workspace)
            .unwrap_or_else(|_| workspace.to_path_buf());
        crate::tmux_backend::TmuxBackend::for_workspace(&run_ws)
            .has_session(&crate::transport::SessionName::new(name))
            .unwrap_or(false)
    }

    fn message_counts(conn: &rusqlite::Connection, owner_team_id: Option<&str>) -> Result<Value, CliError> {
        status_counts(conn, "messages", owner_team_id)
    }

    fn result_counts(conn: &rusqlite::Connection, owner_team_id: Option<&str>) -> Result<Value, CliError> {
        let by_status = result_status_counts(conn, owner_team_id)?;
        let total = count_rows(conn, "results", owner_team_id)?;
        let invalid = count_where_status(conn, "results", owner_team_id, "invalid")?;
        let collected = count_where_status(conn, "results", owner_team_id, "collected")?;
        let uncollected = total.saturating_sub(collected).saturating_sub(invalid);
        Ok(json!({
            "total": total,
            "uncollected": uncollected,
            "collected": collected,
            "invalid": invalid,
            "by_status": by_status,
        }))
    }

    fn status_counts(
        conn: &rusqlite::Connection,
        table: &str,
        owner_team_id: Option<&str>,
    ) -> Result<Value, CliError> {
        let sql = match owner_team_id {
            Some(_) => format!(
                "select status, count(*) from {table}
                 where owner_team_id = ?1
                 group by status order by status"
            ),
            None => format!("select status, count(*) from {table} group by status order by status"),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut rows = match owner_team_id {
            Some(team) => stmt.query(params![team]).map_err(|e| CliError::Runtime(e.to_string()))?,
            None => stmt.query([]).map_err(|e| CliError::Runtime(e.to_string()))?,
        };
        let mut out = Map::new();
        while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
            let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
            let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
            out.insert(status, json!(count));
        }
        Ok(Value::Object(out))
    }

    fn result_status_counts(conn: &rusqlite::Connection, owner_team_id: Option<&str>) -> Result<Value, CliError> {
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
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut rows = match owner_team_id {
            Some(team) => stmt.query(params![team]).map_err(|e| CliError::Runtime(e.to_string()))?,
            None => stmt.query([]).map_err(|e| CliError::Runtime(e.to_string()))?,
        };
        let mut out = Map::new();
        while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
            let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
            let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
            out.insert(status, json!(count));
        }
        Ok(Value::Object(out))
    }

    fn queued_messages(
        conn: &rusqlite::Connection,
        owner_team_id: Option<&str>,
        limit: usize,
    ) -> Result<Value, CliError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = match owner_team_id {
            Some(_) => {
                "select message_id, recipient, status, created_at, delivery_attempts
                 from messages
                 where status like 'queued%' and owner_team_id = ?1
                 order by created_at desc
                 limit ?2"
            }
            None => {
                "select message_id, recipient, status, created_at, delivery_attempts
                 from messages
                 where status like 'queued%'
                 order by created_at desc
                 limit ?1"
            }
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let map_row = |row: &rusqlite::Row<'_>| {
                Ok(json!({
                    "message_id": row.get::<_, String>(0)?,
                    "recipient": row.get::<_, Option<String>>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, Option<String>>(3)?,
                    "delivery_attempts": row.get::<_, i64>(4)?,
                }))
            };
        let rows = match owner_team_id {
            Some(team) => stmt.query_map(params![team, limit], map_row),
            None => stmt.query_map(params![limit], map_row),
        }
        .map_err(|e| CliError::Runtime(e.to_string()))?;
        let values = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        Ok(Value::Array(values))
    }

    fn compact_status(full: Value) -> Value {
        json!({
            "team": full.get("team").cloned().unwrap_or(Value::Null),
            "session_name": full.get("session_name").cloned().unwrap_or(Value::Null),
            "leader_topology": full.get("leader_topology").cloned().unwrap_or_else(|| json!("managed")),
            "is_external_leader": full.get("is_external_leader").cloned().unwrap_or(Value::Bool(false)),
            "leader_attach_command": full.get("leader_attach_command").cloned().unwrap_or(Value::Null),
            "leader_client": full.get("leader_client").cloned().unwrap_or(Value::Null),
            "tmux_session_present": full.get("tmux_session_present").cloned().unwrap_or(Value::Bool(false)),
            "all_spawned": full.get("all_spawned").cloned().unwrap_or(Value::Bool(false)),
            "all_attached_receiver": full.get("all_attached_receiver").cloned().unwrap_or(Value::Bool(true)),
            "all_resumable_have_session": full.get("all_resumable_have_session").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_complete": full.get("session_capture_complete").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_incomplete": full.get("session_capture_incomplete").cloned().unwrap_or(Value::Bool(false)),
            "incomplete_session_capture_agents": full.get("incomplete_session_capture_agents").cloned().unwrap_or_else(|| json!([])),
            "pending_session_agent_ids": full.get("pending_session_agent_ids").cloned().unwrap_or_else(|| json!([])),
            "leader_receiver": compact_object(full.get("leader_receiver"), &[
                "status", "provider", "mode", "session_name", "window_name", "pane_id", "pane_current_command",
            ]),
            "agents": compact_agents(full.get("agents")),
            "agent_health": full.get("agent_health").cloned().unwrap_or_else(|| json!({})),
            "tasks": compact_tasks(full.get("tasks")),
            "messages": full.get("messages").cloned().unwrap_or_else(|| json!({})),
            "queued_messages": take_array(full.get("queued_messages"), 8),
            "results": full.get("results").cloned().unwrap_or_else(|| json!({})),
            "latest_results": take_array(full.get("latest_results"), 5),
            "readiness": full.get("readiness").cloned().unwrap_or_else(|| json!({})),
            "coordinator": compact_object(full.get("coordinator"), &["status", "pid", "metadata_ok", "schema_ok"]),
            "reminder": full.get("reminder").cloned().unwrap_or_else(|| json!(crate::cli::STATUS_REMINDER)),
            "last_events": take_array_tail(full.get("last_events"), 10),
        })
    }

    fn compact_agents(value: Option<&Value>) -> Value {
        let Some(Value::Object(input)) = value else {
            return json!({});
        };
        let mut out = Map::new();
        for (agent_id, agent) in input {
            out.insert(agent_id.clone(), compact_agent_state(agent_id, agent));
        }
        Value::Object(out)
    }

    fn compact_agent_state(agent_id: &str, agent: &Value) -> Value {
        let Some(input) = agent.as_object() else {
            return json!({});
        };
        let mut out = Map::new();
        out.insert(
            "agent_id".to_string(),
            input
                .get("agent_id")
                .cloned()
                .unwrap_or_else(|| Value::String(agent_id.to_string())),
        );
        for key in [
            "status",
            "provider",
            "model",
            "tmux_window_present",
            "session_id",
            "captured_via",
            "attribution_confidence",
            // RM-039-STAT-001 fix (real-machine evidence 2026-06-22): the
            // coordinator-tick activity classifier writes activity (status,
            // confidence, rationale) to the top-level agent state, but the
            // status --json compact projection dropped it. Operators saw
            // "status: running" with no working/idle signal even when the
            // pane was clearly producing output. Add the classifier output
            // here so the compact form surfaces it.
            // last_output_at is the timestamp the classifier advanced when
            // the scrollback digest changed; keeping it adjacent to activity
            // gives the operator a one-glance "is something moving" view.
            // interacted is enriched in enrich_agents (true|false|never)
            // and is the canonical "leader has ever sent this worker a
            // message" signal — also needed for status sanity at a glance.
            "activity",
            "last_output_at",
            "interacted",
        ] {
            if let Some(value) = input.get(key) {
                out.insert(key.to_string(), value.clone());
            }
        }
        if let Some(display) = input.get("display") {
            let compact_display = compact_object(
                Some(display),
                &["backend", "status", "workspace_window", "pane_id", "pid", "pids", "reason"],
            );
            if compact_display.as_object().is_some_and(|obj| !obj.is_empty()) {
                out.insert("display".to_string(), compact_display);
            }
        }
        Value::Object(out)
    }

    fn compact_tasks(value: Option<&Value>) -> Value {
        let Some(Value::Array(tasks)) = value else {
            return json!([]);
        };
        Value::Array(
            tasks.iter()
                .map(|task| compact_object(Some(task), &["id", "title", "status", "assignee", "type", "accepted_result_id"]))
                .collect(),
        )
    }

    fn compact_object(value: Option<&Value>, keys: &[&str]) -> Value {
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

    fn take_array(value: Option<&Value>, limit: usize) -> Value {
        let Some(Value::Array(items)) = value else {
            return json!([]);
        };
        Value::Array(items.iter().take(limit).cloned().collect())
    }

    fn take_array_tail(value: Option<&Value>, limit: usize) -> Value {
        let Some(Value::Array(items)) = value else {
            return json!([]);
        };
        let start = items.len().saturating_sub(limit);
        Value::Array(items.iter().skip(start).cloned().collect())
    }

    fn count_rows(
        conn: &rusqlite::Connection,
        table: &str,
        owner_team_id: Option<&str>,
    ) -> Result<i64, CliError> {
        match owner_team_id {
            Some(team) => {
                let sql = format!("select count(*) from {table} where owner_team_id = ?1");
                conn.query_row(&sql, [team], |row| row.get::<_, i64>(0))
                    .map_err(|e| CliError::Runtime(e.to_string()))
            }
            None => {
                let sql = format!("select count(*) from {table}");
                conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
                    .map_err(|e| CliError::Runtime(e.to_string()))
            }
        }
    }

    fn count_where_status(
        conn: &rusqlite::Connection,
        table: &str,
        owner_team_id: Option<&str>,
        status: &str,
    ) -> Result<i64, CliError> {
        match owner_team_id {
            Some(team) => {
                let sql = format!("select count(*) from {table} where status = ?1 and owner_team_id = ?2");
                conn.query_row(&sql, params![status, team], |row| row.get::<_, i64>(0))
                    .map_err(|e| CliError::Runtime(e.to_string()))
            }
            None => {
                let sql = format!("select count(*) from {table} where status = ?1");
                conn.query_row(&sql, [status], |row| row.get::<_, i64>(0))
                    .map_err(|e| CliError::Runtime(e.to_string()))
            }
        }
    }

    fn agent_health(conn: &rusqlite::Connection, owner_team_id: Option<&str>) -> Result<Value, CliError> {
        let sql = match owner_team_id {
            Some(_) => {
                "select agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at, owner_team_id
                 from agent_health where owner_team_id = ?1 order by agent_id"
            }
            None => {
                "select agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at, owner_team_id
                 from agent_health order by agent_id"
            }
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut rows = match owner_team_id {
            Some(team) => stmt.query(params![team]).map_err(|e| CliError::Runtime(e.to_string()))?,
            None => stmt.query([]).map_err(|e| CliError::Runtime(e.to_string()))?,
        };
        let mut out = Map::new();
        while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
            let agent_id: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
            let status: String = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
            let mut item = Map::new();
            item.insert("status".to_string(), json!(status));
            item.insert(
                "health_status".to_string(),
                json!(crate::provider::agent_health_status(
                    item.get("status").and_then(Value::as_str).unwrap_or("")
                )),
            );
            insert_optional_string(&mut item, "last_output_at", row.get(2).map_err(|e| CliError::Runtime(e.to_string()))?);
            insert_optional_i64(&mut item, "context_usage_pct", row.get(3).map_err(|e| CliError::Runtime(e.to_string()))?);
            insert_optional_string(&mut item, "current_task_id", row.get(4).map_err(|e| CliError::Runtime(e.to_string()))?);
            item.insert(
                "updated_at".to_string(),
                json!(row.get::<_, String>(5).map_err(|e| CliError::Runtime(e.to_string()))?),
            );
            insert_optional_string(&mut item, "owner_team_id", row.get(6).map_err(|e| CliError::Runtime(e.to_string()))?);
            out.insert(agent_id, Value::Object(item));
        }
        Ok(Value::Object(out))
    }

    fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
        if let Some(value) = value {
            map.insert(key.to_string(), Value::String(value));
        }
    }

    fn insert_optional_i64(map: &mut Map<String, Value>, key: &str, value: Option<i64>) {
        if let Some(value) = value {
            map.insert(key.to_string(), json!(value));
        }
    }

    /// B-5 / 036b N38 — status 出口的 runtime 块:把 coordinator_health 与
    /// undelivered backlog 合体暴露。down-hint 只在【coordinator 不在跑 ∧ 有 backlog】
    /// 两条件同时满足才挂(anti-nag);健康状态下不挂提示。auto-recovery 不做。
    fn build_runtime_status_block(coordinator_running: bool, undelivered: i64) -> Value {
        let mut runtime = serde_json::Map::new();
        runtime.insert(
            "coordinator".to_string(),
            json!({"ok": coordinator_running}),
        );
        runtime.insert("undelivered".to_string(), json!(undelivered));
        if !coordinator_running && undelivered > 0 {
            runtime.insert(
                "hint".to_string(),
                json!(format!(
                    "coordinator not running with {undelivered} undelivered — run team-agent restart"
                )),
            );
        }
        Value::Object(runtime)
    }

    /// Whether the coordinator HealthReport reflects a running tick loop. Used by the
    /// runtime block + the hint gate.
    fn coordinator_status_running(health: &crate::coordinator::HealthReport) -> bool {
        matches!(health.status, crate::coordinator::CoordinatorHealthStatus::Running)
    }

    /// Count of messages currently sitting in delivery-able backlog
    /// (accepted/pending/queued forms — not delivered / not failed / not refused).
    /// owner_team_id scope honored when present.
    fn count_undelivered_backlog(
        conn: &rusqlite::Connection,
        owner_team_id: Option<&str>,
    ) -> Result<i64, CliError> {
        // Backlog statuses chosen to mirror what `deliver_pending` would pick up.
        let sql = match owner_team_id {
            Some(_) => "select count(*) from messages
                       where owner_team_id = ?1 and status in ('accepted','pending','queued','queued_until_trust')",
            None => "select count(*) from messages
                     where status in ('accepted','pending','queued','queued_until_trust')",
        };
        let count: i64 = match owner_team_id {
            Some(team) => conn
                .query_row(sql, params![team], |row| row.get(0))
                .map_err(|e| CliError::Runtime(e.to_string()))?,
            None => conn
                .query_row(sql, [], |row| row.get(0))
                .map_err(|e| CliError::Runtime(e.to_string()))?,
        };
        Ok(count)
    }

    fn coordinator_health_value(health: crate::coordinator::HealthReport) -> Value {
        json!({
            "ok": health.ok,
            "status": coordinator_status_wire(health.status),
            "pid": health.pid.map(|p| p.get()),
            "metadata": health.metadata.map(|m| json!({
                "pid": m.pid.get(),
                "protocol_version": m.protocol_version,
                "message_store_schema_version": m.message_store_schema_version,
                "source": m.source,
                "updated_at": m.updated_at,
            })),
            "metadata_ok": health.metadata_ok,
            "schema_ok": health.schema.ok,
            "schema_error": health.schema.error.map(|e| format!("{e:?}")),
            "schema": {
                "message_store_schema_version": health.schema.schema_version,
            },
        })
    }

    fn coordinator_status_wire(status: crate::coordinator::CoordinatorHealthStatus) -> &'static str {
        match status {
            crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
            crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
            crate::coordinator::CoordinatorHealthStatus::Running => "running",
            crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
        }
    }
