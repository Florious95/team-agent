//! status_port extracted from cli::mod inline placeholder.
use super::*;
use crate::state::projection::OwnerTeamResolution;
use crate::transport::Transport;
use rusqlite::params;
use std::path::PathBuf;

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
    let health = crate::coordinator::coordinator_health(&crate::coordinator::WorkspacePath::new(
        workspace.to_path_buf(),
    ));
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    // B-5 / 036b N38 explicable — status 出口 runtime 块:把 coordinator_health
    // (现状)+ undelivered backlog count 一起暴露;coordinator not running ∧
    // backlog>0 才挂 down-hint(anti-nag)。auto-recovery 不做(user 已裁)。
    let coordinator_running = coordinator_status_running(&health);
    let undelivered_backlog = count_undelivered_backlog(&conn, owner_team_id)?;
    let session_name = state.get("session_name").cloned().unwrap_or(Value::Null);
    let tmux_present = tmux_session_present(workspace, state, session_name.as_str());
    // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): resolve
    // RuntimeFreshness once and thread it through the runtime block
    // and per-agent enrichment. This is the single-source read that
    // makes host-boot / coordinator / provider-exit staleness win
    // over cached state.agents and DB agent_health.
    let freshness = compute_runtime_freshness(workspace, state, &health);
    let runtime_block = build_runtime_status_block(
        coordinator_running,
        undelivered_backlog,
        !tmux_present,
        &freshness,
    );
    let agents = enrich_agents(workspace, state, tmux_present, &freshness);
    let tasks = state.get("tasks").cloned().unwrap_or_else(|| json!([]));
    let leader_receiver = state
        .get("leader_receiver")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let is_external_leader = crate::state::projection::state_is_external_leader(state);
    let leader_topology = if is_external_leader {
        "external"
    } else {
        "managed"
    };
    let leader_attach_command = if is_external_leader {
        None
    } else {
        let window_name = state
            .pointer("/leader_receiver/window_name")
            .and_then(Value::as_str)
            .unwrap_or("leader");
        session_name.as_str().and_then(|session| {
            // Bug #7 (gate review §6): build the attach command from the
            // SAME endpoint the readiness probe uses (state's persisted
            // tmux_endpoint/tmux_socket), so the printed command matches
            // where the session actually lives.
            crate::tmux_backend::attach_command_for_runtime_state_or_workspace(
                workspace,
                Some(state),
                &crate::transport::SessionName::new(session.to_string()),
                window_name,
            )
        })
    };
    let mut readiness_state = state.clone();
    if let Some(obj) = readiness_state.as_object_mut() {
        obj.insert(
            "tmux_session_present".to_string(),
            serde_json::json!(tmux_present),
        );
    }
    let readiness = crate::cli::diagnose::wait_readiness(&readiness_state);
    let full = crate::redaction::redact_external_value(&json!({
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
        "pending_leader_notifications": pending_leader_notifications(&conn, owner_team_id, 8)?,
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
    }));
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
    let agent = agents.and_then(|map| map.get(agent_id)).unwrap_or(&empty);
    let row = health.and_then(|map| map.get(agent_id)).unwrap_or(&empty);
    let status_text = row
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            agent_health_status_text(agent.get("status").and_then(Value::as_str).unwrap_or(""))
        });
    let tasks = status
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
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
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
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
    approvals_scoped(workspace, &state, agent, as_json)
}

pub fn approvals_scoped(
    workspace: &Path,
    state: &Value,
    agent: Option<&str>,
    as_json: bool,
) -> Result<Value, CliError> {
    let _ = as_json;
    let session = state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut approvals = Vec::new();
    if let (Some(session), Some(agents)) = (session, state.get("agents").and_then(Value::as_object))
    {
        let run_ws = crate::model::paths::canonical_run_workspace(workspace)
            .unwrap_or_else(|_| workspace.to_path_buf());
        // 0.5.x Phase 1d Batch 3: use the factory-resolved backend
        // so conpty teams get their scrollback from the shim rather
        // than a fake tmux capture that always returns empty. Tmux
        // teams take the same code path as before (byte-equivalent).
        let resolved = crate::transport_factory::resolve_read_only_transport(
            &run_ws,
            Some(state),
            crate::transport_factory::TransportPurpose::Status,
        );
        let backend: Box<dyn crate::transport::Transport> = match resolved {
            Ok(r) => r.backend,
            Err(_) => {
                // Read-path fallback: refused factory means we don't
                // try to inspect approval prompts. Empty vec = no
                // waiting approvals, honest.
                return Ok(json!({
                    "ok": true,
                    "waiting": false,
                    "waiting_count": 0,
                    "approvals": [],
                }));
            }
        };
        for (agent_id, agent_state) in agents {
            if agent.is_some_and(|wanted| wanted != agent_id) {
                continue;
            }
            let window = agent_window(agent_id, agent_state);
            let target = crate::transport::Target::SessionWindow {
                session: crate::transport::SessionName::new(session.to_string()),
                window: crate::transport::WindowName::new(window.clone()),
            };
            let Ok(captured) = backend.capture(&target, crate::transport::CaptureRange::Tail(120))
            else {
                continue;
            };
            if let Some(prompt) = crate::provider::extract_approval_prompt(agent_id, &captured.text)
            {
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
            let agent = approval
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let kind = approval
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
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
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let _ = as_json;
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut messages = store
        .inbox(agent, limit, owner_team_id)
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
    crate::state::repository::StateRepository::new(workspace)
        .load_workspace_if_exists_without_migrations()
        .ok()
        .flatten()
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
        OwnerTeamResolution::LegacyAlias {
            requested,
            canonical,
        } => {
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
        .find_map(|key| {
            agent_state
                .get(*key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(agent_id)
        .to_string()
}

/// 0.5.41 Slice 3 (fault-invisibility-locate.md §5): single-source
/// runtime freshness projection consumed by status/diagnose. Read-
/// only over existing sources (`coordinator_health`, heartbeat
/// sidecar, watch state); computes NO transport calls of its own.
#[derive(Default, Clone)]
pub(crate) struct RuntimeFreshness {
    pub coordinator_service_available: bool,
    pub host_boot_stale: bool,
    pub host_boot_recorded: Option<String>,
    pub host_boot_current: Option<String>,
    pub provider_exited_agents: std::collections::BTreeSet<String>,
}

impl RuntimeFreshness {
    fn host_boot_stale_reason(&self) -> Option<&'static str> {
        self.host_boot_stale.then_some("host_boot_mismatch")
    }
}

pub(crate) fn compute_runtime_freshness(
    workspace: &Path,
    state: &Value,
    health: &crate::coordinator::HealthReport,
) -> RuntimeFreshness {
    let workspace_path = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let heartbeat = crate::coordinator::read_coordinator_heartbeat(&workspace_path);
    let host_boot_recorded = heartbeat
        .as_ref()
        .and_then(|hb| hb.get("host_boot_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "unknown")
        .map(str::to_string);
    let host_boot_current = crate::coordinator::probe_host_boot_id();
    let host_boot_stale = match (host_boot_recorded.as_deref(), host_boot_current.as_deref()) {
        (Some(recorded), Some(current)) => recorded != current,
        _ => false,
    };
    // Collect worker-provider-exited agents from the coordinator
    // abnormal_exit_watch payload. Only the typed positive provider-exit
    // marker participates here: generic process/pane death diagnostics are
    // not proof that the provider exited. Read from top-level
    // `coordinator.abnormal_exit_watch`
    // OR the team-scoped mirror `teams.<key>.coordinator...`.
    let mut provider_exited_agents = std::collections::BTreeSet::new();
    for path in [
        "/coordinator/abnormal_exit_watch",
        &format!(
            "/teams/{}/coordinator/abnormal_exit_watch",
            state
                .get("active_team_key")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
    ] {
        if let Some(watch) = state.pointer(path).and_then(Value::as_object) {
            for (agent_id, entry) in watch {
                let exited =
                    entry.get("worker_provider_exited").and_then(Value::as_bool) == Some(true);
                if exited {
                    provider_exited_agents.insert(agent_id.clone());
                }
            }
        }
    }
    RuntimeFreshness {
        coordinator_service_available: health.service_available,
        host_boot_stale,
        host_boot_recorded,
        host_boot_current,
        provider_exited_agents,
    }
}

fn enrich_agents(
    workspace: &Path,
    state: &Value,
    tmux_session_present: bool,
    freshness: &RuntimeFreshness,
) -> Value {
    let agents = state.get("agents");
    let Some(Value::Object(input)) = agents else {
        return json!({});
    };
    let team_dir = state
        .get("team_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.to_path_buf());
    let mut out = Map::new();
    for (agent_id, value) in input {
        match value {
            Value::Object(obj) => {
                let mut enriched = obj.clone();
                apply_effective_role_projection(&team_dir, agent_id, &mut enriched);
                enriched.insert(
                    "interacted".to_string(),
                    Value::String(interacted_marker(obj.get("first_send_at"))),
                );
                // 0.5.41 Slice 3: order stale sources most-authoritative-first.
                // Host boot mismatch wins because it invalidates all cached
                // pane/pid/session facts. Provider-exit marker wins over
                // pane liveness because the wrapper leaves an interactive
                // shell live. Coordinator unavailability without stronger
                // live provider proof means DB agent_health rows are stale.
                // Tmux session missing keeps its existing legacy path so
                // pre-0.5.41 tests remain byte-identical when no new
                // signal fires.
                let has_pane_binding = agent_has_pane_fact(&Value::Object(obj.clone()));
                // 0.5.41 Slice 3 (fault-invisibility-locate.md §5 point 3
                // + §9 RED4 live-provider guard): the wrapper-era pane
                // liveness cannot prove provider liveness — but a state
                // `pane_current_command` that MATCHES the agent's provider
                // IS positive proof (the abnormal.rs classifier writes it
                // when the pane's foreground command is the provider CLI).
                // When that positive proof is present, no stale downgrade
                // fires here so the agent renders as working.
                let provider_command_positive_proof = provider_current_command_matches(obj);
                // 0.5.41 Slice 3 (0.5.35 R4 regression guard): when the
                // runtime classifier has already written canonical
                // `worker_state=UNKNOWN` / `activity.status=uncertain`,
                // that is the authoritative honest observation — do NOT
                // reclassify it as `coordinator_unavailable` stale (which
                // would land it in the Stopped bucket instead of Unknown).
                // Host-boot mismatch and provider-exited marker are
                // stronger, more specific signals and still win.
                let canonical_unknown = agent_canonical_worker_state_is_unknown(obj);
                let new_reason = if freshness.host_boot_stale && has_pane_binding {
                    freshness.host_boot_stale_reason()
                } else if freshness.provider_exited_agents.contains(agent_id) {
                    Some("worker_provider_exited")
                } else if !freshness.coordinator_service_available
                    && has_pane_binding
                    && !provider_command_positive_proof
                    && !canonical_unknown
                {
                    Some("coordinator_unavailable")
                } else {
                    None
                };
                let legacy_reason = if provider_command_positive_proof {
                    None
                } else {
                    stale_reason_for_agent(&Value::Object(obj.clone()), tmux_session_present)
                };
                let reason = new_reason.or(legacy_reason);
                if let Some(reason) = reason {
                    enriched.insert("stale".to_string(), Value::Bool(true));
                    enriched.insert(
                        "stale_reason".to_string(),
                        Value::String(reason.to_string()),
                    );
                    // Downgrade cached BUSY/working when the stale source is
                    // one of the new authoritative signals OR the pre-existing
                    // session-missing signal. Legacy code only downgraded on
                    // !tmux_session_present; that let host_boot / provider-
                    // exit / coord-unavailable stale rows keep raw=running.
                    let is_new_signal = matches!(
                        reason,
                        "host_boot_mismatch" | "worker_provider_exited" | "coordinator_unavailable"
                    );
                    if !tmux_session_present || is_new_signal {
                        downgrade_stale_agent(&mut enriched);
                    }
                }
                out.insert(agent_id.clone(), Value::Object(enriched));
            }
            _ => {
                out.insert(agent_id.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

fn apply_effective_role_projection(
    team_dir: &Path,
    agent_id: &str,
    agent: &mut Map<String, Value>,
) {
    let role_file = agent
        .get("dynamic_role_file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| team_dir.join("agents").join(format!("{agent_id}.md")));
    let Ok((meta, _)) = crate::compiler::read_front_matter(&role_file) else {
        return;
    };
    let Some(meta) = yaml_map(&meta) else {
        return;
    };
    if let Some(provider) = yaml_str(meta, "provider").filter(|value| !value.is_empty()) {
        agent.insert("provider".to_string(), Value::String(provider.to_string()));
        agent.insert(
            "provider_source".to_string(),
            Value::String("role".to_string()),
        );
    }
    if let Some(model) = yaml_str(meta, "model").filter(|value| !value.is_empty()) {
        if agent.get("model").and_then(Value::as_str) != Some(model) {
            agent.insert("model_stale".to_string(), Value::Bool(true));
        }
        agent.insert("model".to_string(), Value::String(model.to_string()));
        agent.insert(
            "model_source".to_string(),
            Value::String("role".to_string()),
        );
    }
}

fn yaml_map(
    value: &crate::model::yaml::Value,
) -> Option<&Vec<(String, crate::model::yaml::Value)>> {
    match value {
        crate::model::yaml::Value::Map(items) => Some(items),
        _ => None,
    }
}

fn yaml_str<'a>(items: &'a [(String, crate::model::yaml::Value)], key: &str) -> Option<&'a str> {
    items.iter().find_map(|(name, value)| {
        if name == key {
            match value {
                crate::model::yaml::Value::Str(value) => Some(value.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

fn downgrade_stale_agent(agent: &mut Map<String, Value>) {
    let raw = agent
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(raw.as_str(), "running" | "busy" | "working" | "idle") {
        agent.insert("status".to_string(), Value::String("stopped".to_string()));
    }
    let worker_state = agent
        .get("worker_state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(worker_state.as_str(), "RUNNING" | "BUSY" | "PROBABLY_IDLE") {
        agent.insert(
            "worker_state".to_string(),
            Value::String("DEAD".to_string()),
        );
    }
}

fn stale_reason_for_agent(agent: &Value, tmux_session_present: bool) -> Option<&'static str> {
    let pane_dead = !tmux_session_present && agent_has_pane_fact(agent);
    let process_dead =
        agent_process_dead(agent) || (!tmux_session_present && agent_has_process_fact(agent));
    match (pane_dead, process_dead) {
        (true, true) => Some("both"),
        (false, true) => Some("process_dead"),
        (true, false) => Some("pane_dead"),
        (false, false) => None,
    }
}

/// 0.5.41 Slice 3 (0.5.35 R4 regression guard): true when the agent
/// row carries the canonical `worker_state=UNKNOWN` OR
/// `activity.status=uncertain` observation the runtime classifier
/// writes. Used to skip the coordinator-unavailable stale mark
/// (see `enrich_agents`) so the pre-existing R4 rendering (UNKNOWN
/// beats WORKING) is preserved.
fn agent_canonical_worker_state_is_unknown(agent: &serde_json::Map<String, Value>) -> bool {
    let worker_state_unknown = agent
        .get("worker_state")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("UNKNOWN"));
    let activity_uncertain = agent
        .get("activity")
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("uncertain"));
    worker_state_unknown || activity_uncertain
}

/// 0.5.41 Slice 3 (fault-invisibility-locate.md §9 RED4 live-provider
/// guard): true when the agent row carries a `pane_current_command`
/// that matches the agent's provider CLI. This is positive proof
/// the provider is the pane's foreground process — the abnormal.rs
/// classifier writes this field after the marker/current-command
/// check clears. When true, stale-downgrade paths in
/// `enrich_agents` skip so the row keeps its BUSY/working state.
fn provider_current_command_matches(agent: &serde_json::Map<String, Value>) -> bool {
    let Some(command) = agent
        .get("pane_current_command")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let Some(provider_wire) = agent.get("provider").and_then(Value::as_str) else {
        return false;
    };
    let Some(provider) = crate::provider::wire::parse_provider(provider_wire) else {
        return false;
    };
    crate::leader::command_matches_provider(provider, command)
}

fn agent_has_pane_fact(agent: &Value) -> bool {
    ["pane_id", "window", "window_name"].iter().any(|key| {
        agent
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    })
}

fn agent_has_process_fact(agent: &Value) -> bool {
    agent.get("pid").and_then(Value::as_i64).is_some()
        || agent.get("process_started").and_then(Value::as_bool) == Some(true)
        || agent
            .get("provider_process_dead")
            .and_then(Value::as_bool)
            .is_some()
        || agent
            .get("process_liveness")
            .and_then(Value::as_str)
            .is_some()
}

fn agent_process_dead(agent: &Value) -> bool {
    if agent.get("provider_process_dead").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    ["process_liveness", "worker_state"].iter().any(|key| {
        agent
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(is_dead_process_state)
    })
}

fn is_dead_process_state(value: &str) -> bool {
    matches!(
        value,
        "dead" | "missing" | "stopped" | "exited" | "terminated"
    )
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

fn tmux_session_present(workspace: &Path, state: &Value, session_name: Option<&str>) -> bool {
    // Bug #7 (prerelease 0.4.0 gate review §6): probe the SAME endpoint
    // the runtime actually uses (state.tmux_endpoint / tmux_socket), not
    // the workspace-hash socket. When state has no persisted endpoint,
    // fall back to workspace — preserves legacy behavior. wait_readiness
    // formula unchanged per 不可改项; only the input signal is fixed.
    let Some(name) = session_name else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let run_ws = crate::model::paths::canonical_run_workspace(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf());
    // 0.5.x Phase 1d Batch 3: route through the factory so a
    // conpty team does NOT get its `has_session` probe served by a
    // tmux backend (which would always return false and drive the
    // reader into a false `tmux_session_missing` state — design
    // §Batch 3 Verification anchor). Tmux teams see byte-equivalent
    // behavior because factory Layer 3 (legacy tmux endpoint) uses
    // the same `tmux_backend_for_runtime_state_or_workspace` shape.
    let resolved = crate::transport_factory::resolve_read_only_transport(
        &run_ws,
        Some(state),
        crate::transport_factory::TransportPurpose::Status,
    );
    match resolved {
        Ok(r) => r
            .backend
            .has_session(&crate::transport::SessionName::new(name))
            .unwrap_or(false),
        Err(_) => {
            // Factory refused (e.g. explicit conpty without a
            // resolvable team_key). Honest: return false rather
            // than pretend a tmux session exists.
            false
        }
    }
}

fn message_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    status_counts(conn, "messages", owner_team_id)
}

fn result_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
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
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut rows = match owner_team_id {
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
    };
    let mut out = Map::new();
    while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
        let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
        let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
        out.insert(status, json!(count));
    }
    Ok(Value::Object(out))
}

fn result_status_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
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
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
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

/// 0.5.5 gate054 round-2: leader notifications that were refused with
/// `rebind_required` (status=failed, error=leader_not_attached) sit as
/// failed rows in the store; without a dedicated status field the
/// operator sees only `messages.failed=N` and cannot tell that the
/// notifications are waiting for a rebind. This field surfaces them
/// alongside `queued_messages` so `attach-leader` / `takeover` is
/// visibly the fix. Once the pane is rebound the requeue path flips
/// each row back to `status=accepted` and it drops out of this list.
fn pending_leader_notifications(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
    limit: usize,
) -> Result<Value, CliError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    // E6 (0.5.9 offline-mailbox §6.6): also surface `queued_until_leader_attach`
    // rows (third-party sends into the leader mailbox) so the target owner can
    // see them alongside the rebind-required failures. Channel wire label
    // distinguishes the two so the operator can tell the two shapes apart.
    let sql = match owner_team_id {
        Some(_) => {
            "select message_id, sender, status, error, created_at, delivery_attempts
                 from messages
                 where recipient = 'leader'
                   and owner_team_id = ?1
                   and (
                     (status = 'failed' and error = 'leader_not_attached')
                     or status = 'queued_until_leader_attach'
                   )
                 order by created_at desc
                 limit ?2"
        }
        None => {
            "select message_id, sender, status, error, created_at, delivery_attempts
                 from messages
                 where recipient = 'leader'
                   and (
                     (status = 'failed' and error = 'leader_not_attached')
                     or status = 'queued_until_leader_attach'
                   )
                 order by created_at desc
                 limit ?1"
        }
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let status: String = row.get(2)?;
        let channel = if status == "queued_until_leader_attach" {
            "leader_mailbox"
        } else {
            "rebind_required"
        };
        Ok(json!({
            "message_id": row.get::<_, String>(0)?,
            "sender": row.get::<_, Option<String>>(1)?,
            "status": status,
            "error": row.get::<_, Option<String>>(3)?,
            "created_at": row.get::<_, Option<String>>(4)?,
            "delivery_attempts": row.get::<_, i64>(5)?,
            "channel": channel,
            "action": "run team-agent attach-leader or team-agent takeover",
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

/// 0.4.x: slim default compact payload — exactly 7 top-level fields.
/// Diagnostic detail moves to `--detail`. Plan:
/// /Users/alauda/Documents/code/team-agent-public/.team/artifacts/status-compact-plan.md
fn compact_status(full: Value) -> Value {
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
fn compact_ready(full: &Value, not_ready: &Value) -> bool {
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
fn compact_not_ready(full: &Value) -> Value {
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

fn not_ready_reasons(full: &Value) -> Vec<String> {
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

/// 0.4.x: agent rows in the slim payload have exactly 4 fields. agent_id
/// is no longer copied in — the map key already carries it. Diagnostic
/// fields (model, tmux_window_present, session_id, captured_via,
/// attribution_confidence, display, interacted) move to `--detail`.
/// `activity` + `last_output_at` are preserved (RM-039-STAT-001).
fn compact_agent_state(_agent_id: &str, agent: &Value) -> Value {
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

fn compact_tasks(value: Option<&Value>) -> Value {
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
            let sql =
                format!("select count(*) from {table} where status = ?1 and owner_team_id = ?2");
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

fn agent_health(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
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
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
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
        insert_optional_string(
            &mut item,
            "last_output_at",
            row.get(2).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
        insert_optional_i64(
            &mut item,
            "context_usage_pct",
            row.get(3).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
        let current_task_id: Option<String> =
            row.get(4).map_err(|e| CliError::Runtime(e.to_string()))?;
        let has_current_task = current_task_id.is_some();
        insert_optional_string(&mut item, "current_task_id", current_task_id);
        let updated_at: String = row.get(5).map_err(|e| CliError::Runtime(e.to_string()))?;
        // Phase-DX E2 (plan §4 / CR supplement A): expose the last agent_health
        // observation timestamp as `health_updated_at` alongside the legacy
        // `updated_at` alias. Two names for one column keep old scrapers working
        // while surfacing the semantic (heartbeat, not row bookkeeping).
        item.insert("updated_at".to_string(), json!(updated_at.clone()));
        item.insert("health_updated_at".to_string(), json!(updated_at));
        // Phase-DX E2 (CR P0 red line #6, supplements A/B): current_task is a
        // best-effort *display* field until A1 makes task FSM authoritative.
        // The structured source/confidence markers stop downstream code from
        // treating agent_health.current_task_id as authority. `current_task_source`
        // records where the display value came from (only "health" today —
        // Phase-DX never merges state tasks into this projection); the
        // `current_task_confidence` enum stays "best_effort" for the whole
        // Phase-DX slice (A1 will later flip it to "authoritative" when the
        // task FSM lands). Field is written unconditionally so consumers can
        // switch on it even when `current_task_id` is null.
        item.insert(
            "current_task_source".to_string(),
            json!(if has_current_task { "health" } else { "none" }),
        );
        item.insert("current_task_confidence".to_string(), json!("best_effort"));
        insert_optional_string(
            &mut item,
            "owner_team_id",
            row.get(6).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
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
fn build_runtime_status_block(
    coordinator_running: bool,
    undelivered: i64,
    tmux_session_missing: bool,
    freshness: &RuntimeFreshness,
) -> Value {
    let mut runtime = serde_json::Map::new();
    runtime.insert(
        "coordinator".to_string(),
        json!({"ok": coordinator_running}),
    );
    runtime.insert("undelivered".to_string(), json!(undelivered));
    // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): expose
    // typed issues on the runtime block so `status --json --detail`
    // can be scanned for `runtime_bindings_stale_after_boot` the
    // same way `diagnose` surfaces it. Hint precedence: session-
    // missing > host-boot-stale > coordinator-not-running-with-
    // backlog — most-actionable-first.
    let mut issues: Vec<Value> = Vec::new();
    if freshness.host_boot_stale {
        issues.push(json!({
            "id": "runtime_bindings_stale_after_boot",
            "recorded_host_boot_id": freshness.host_boot_recorded,
            "current_host_boot_id": freshness.host_boot_current,
        }));
    }
    if !issues.is_empty() {
        runtime.insert("issues".to_string(), Value::Array(issues));
    }
    if tmux_session_missing {
        runtime.insert(
            "hint".to_string(),
            json!("tmux session missing — run team-agent restart"),
        );
    } else if freshness.host_boot_stale {
        runtime.insert(
            "hint".to_string(),
            json!("runtime_bindings_stale_after_boot — run team-agent restart"),
        );
    } else if !coordinator_running && undelivered > 0 {
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
    // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): runtime
    // service predicate is `service_available`, not any-`Running` pid.
    // A stale-pid daemon is CoordinatorHealthStatus::Stale (or Running
    // with metadata_ok=false); a service-compatible newer daemon is
    // Running + service_available=true — send/diagnose already use
    // this same truth source, and status must agree.
    health.service_available
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
    let expose_binary_drift = health.service_available && !health.metadata_ok;
    let binary_identity_relation = health.binary_identity_relation.as_str();
    let mut value = json!({
        "ok": health.ok,
        "status": coordinator_status_wire(health.status),
        "pid": health.pid.map(|p| p.get()),
        "metadata": health.metadata.map(|m| json!({
            "pid": m.pid.get(),
            "protocol_version": m.protocol_version,
            "message_store_schema_version": m.message_store_schema_version,
            "binary_path": m.binary_path,
            "binary_version": m.binary_version,
            "source": m.source,
            "updated_at": m.updated_at,
        })),
        "metadata_ok": health.metadata_ok,
        "metadata_mismatch_reason": health.metadata_mismatch_reason,
        "binary_path": health.current_binary_identity.binary_path,
        "binary_version": health.current_binary_identity.binary_version,
        "schema_ok": health.schema.ok,
        "schema_error": health.schema.error.map(|e| format!("{e:?}")),
        "schema": {
            "message_store_schema_version": health.schema.schema_version,
        },
        // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3):
        // `service_available` is now always exposed alongside `ok`
        // and `status` so status/diagnose consumers can share one
        // service-availability truth without keying on the legacy
        // `expose_binary_drift` conditional (that path only fired
        // for the newer-daemon-preserved shape). `binary_identity_
        // relation` moves to always-on for the same reason — RED3
        // scans the summary for `daemon_newer_than_caller`.
        "service_available": health.service_available,
        "binary_identity_relation": binary_identity_relation,
    });
    if expose_binary_drift {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("wire_metadata_ok".to_string(), Value::Bool(true));
            obj.insert("binary_identity_ok".to_string(), Value::Bool(false));
        }
    }
    value
}

fn coordinator_status_wire(status: crate::coordinator::CoordinatorHealthStatus) -> &'static str {
    match status {
        crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
        crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
        crate::coordinator::CoordinatorHealthStatus::Running => "running",
        crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health_fixture() -> crate::coordinator::HealthReport {
        crate::coordinator::HealthReport {
            ok: true,
            status: crate::coordinator::CoordinatorHealthStatus::Running,
            pid: Some(crate::coordinator::Pid::new(std::process::id())),
            metadata: None,
            metadata_ok: true,
            process_running: true,
            wire_metadata_ok: true,
            binary_identity_ok: true,
            binary_identity_relation: crate::coordinator::CoordinatorBinaryIdentityRelation::Same,
            service_available: true,
            metadata_mismatch_reason: None,
            current_binary_identity: crate::coordinator::CoordinatorBinaryIdentity {
                binary_path: "/test/team-agent".to_string(),
                binary_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            schema: crate::coordinator::SchemaHealth {
                ok: true,
                schema_version: crate::db::schema::SCHEMA_VERSION,
                error: None,
                action: None,
            },
        }
    }

    #[test]
    fn runtime_freshness_provider_exit_requires_typed_positive_fact() {
        let state = serde_json::json!({
            "coordinator": {
                "abnormal_exit_watch": {
                    "pane_only": {
                        "provider_process_dead": true,
                        "worker_provider_exited": false
                    },
                    "typed_exit": {
                        "provider_process_dead": true,
                        "worker_provider_exited": true
                    }
                }
            }
        });

        let freshness = compute_runtime_freshness(
            Path::new("/nonexistent/status-port-typed-provider-exit-test"),
            &state,
            &health_fixture(),
        );

        assert!(!freshness.provider_exited_agents.contains("pane_only"));
        assert!(freshness.provider_exited_agents.contains("typed_exit"));
    }
}
