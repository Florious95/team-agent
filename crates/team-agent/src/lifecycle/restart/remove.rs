use super::*;
use super::agent::{resolve_team_scoped_state_or_refuse, start_agent_at_paths, stop_agent_at_paths};
use super::common::*;
use super::team_state::write_team_state;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

/// `remove_agent(workspace, agent_id, from_spec, force, team)`(`lifecycle/agents.py:22`)。
/// 从 spec/state/team_state/role-file/agent_health 原子摘除;`_RemoveRollback` 字节级快照
/// 回滚全部。未传 from_spec 确认 / 运行中未传 force → 拒绝。
pub fn remove_agent(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &paths.run_workspace,
        operation: "remove-agent",
        team,
        agent_id: Some(agent_id),
    })?;
    let transport =
        lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    remove_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        from_spec,
        force,
        team,
        &transport,
    )
}

pub fn remove_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &paths.run_workspace,
        operation: "remove-agent",
        team,
        agent_id: Some(agent_id),
    })?;
    remove_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        from_spec,
        force,
        team,
        transport,
    )
}

pub fn remove_agent_flag_requirements(
    workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
) -> Result<RemoveAgentFlagRequirements, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let transport =
        lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    Ok(remove_agent_preflight(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        team,
        &transport,
    )?
    .requirements)
}

struct RemoveAgentPreflight {
    state: serde_json::Value,
    spec: YamlValue,
    requirements: RemoveAgentFlagRequirements,
}

fn remove_agent_preflight(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentPreflight, LifecycleError> {
    // golden agents.py:34-41: resolve_team_scoped_state FIRST (surfaces the team_target_ambiguous /
    // team_target_unresolved refusal before the owner gate), THEN the owner gate, THEN load_spec +
    // _find_worker (unknown-worker raise). Mirror the stop/reset wiring so remove is byte-symmetric:
    // the team-scoped projection (not a raw load) drives the dynamic/running/from_spec decisions.
    let state = resolve_team_scoped_state_or_refuse(workspace, team)?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, Some(agent_id))?;
    let spec = load_team_spec(spec_workspace)?;
    let Some(spec_agent) = find_spec_agent(&spec, agent_id) else {
        return Err(unknown_worker(agent_id));
    };
    let dynamic_agent = is_dynamic_agent(&state, spec_agent, agent_id);
    let force_required = agent_is_running(&state, agent_id, transport);
    let has_session = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
        || state
            .get("agents")
            .and_then(|v| v.get(agent_id.as_str()))
            .is_some_and(agent_has_session);
    Ok(RemoveAgentPreflight {
        state,
        spec,
        requirements: RemoveAgentFlagRequirements {
            agent_id: agent_id.clone(),
            from_spec_required: !dynamic_agent,
            force_required,
            has_session,
        },
    })
}

fn agent_has_session(agent: &serde_json::Value) -> bool {
    ["session_id", "_pending_session_id", "rollout_path"]
        .iter()
        .any(|key| {
            agent
                .get(key)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
        })
}

fn remove_agent_at_paths(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let preflight = remove_agent_preflight(workspace, spec_workspace, agent_id, team, transport)?;
    let missing_from_spec = preflight.requirements.from_spec_required && !from_spec;
    let missing_force = preflight.requirements.force_required && !force;
    if missing_from_spec || missing_force {
        return Ok(if missing_from_spec && missing_force {
            RemoveAgentOutcome::RefusedRequiredFlags {
                agent_id: agent_id.clone(),
                from_spec_required: true,
                force_required: true,
            }
        } else if missing_from_spec {
            RemoveAgentOutcome::RefusedFromSpecConfirm {
                agent_id: agent_id.clone(),
            }
        } else {
            RemoveAgentOutcome::RefusedForceRequired {
                agent_id: agent_id.clone(),
            }
        });
    }
    let paths = LifecyclePathRefs {
        run_workspace: workspace,
        spec_workspace,
    };
    let mut rollback = RemoveRollback::capture(
        paths.run_workspace,
        paths.spec_workspace,
        &preflight.spec,
        &preflight.state,
        agent_id,
    )?;
    rollback.restore_running = force && preflight.requirements.force_required;
    let result = remove_agent_inner(
        &paths,
        agent_id,
        &preflight.spec,
        preflight.state,
        force,
        team,
        transport,
    );
    match result {
        Ok(success) => {
            // golden agents.py:135: _save_team_runtime_snapshot runs OUTSIDE the try/except, and
            // snapshot.py:19-21 returns None (no error) when session_name is falsy. Mirror that here:
            // only snapshot when session_name is present, and never let it roll the committed removal back.
            if success
                .removed_state
                .get("session_name")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
            {
                let _ = crate::lifecycle::helpers::save_team_runtime_snapshot(workspace, &success.removed_state)?;
            }
            write_remove_complete_event(
                paths.run_workspace,
                agent_id,
                from_spec,
                force,
                success.stopped,
                success.role_file_removed,
                success.cleared_locations,
            )?;
            Ok(success.outcome)
        }
        Err(error) => {
            // golden agents.py:110-133: restore is best-effort (collects per-artifact errors, restores ALL),
            // and the ORIGINAL operation error is ALWAYS re-raised, annotated with rollback_ok — a
            // restore-step failure only flips rollback_ok, it never replaces the surfaced cause.
            let restore_errors = rollback.restore(paths.run_workspace, paths.spec_workspace, team, transport);
            let rollback_ok = restore_errors.is_empty();
            let rollback_event = RemoveRollbackEvent {
                agent_id,
                workspace: paths.run_workspace,
                from_spec,
                force,
                stopped: rollback.restore_running,
                error: &error,
                rollback_ok,
                restore_errors: &restore_errors,
            };
            let _ = write_remove_rollback_events(rollback_event);
            Err(LifecycleError::StatePersist(format!(
                "remove-agent failed for {agent_id}: {error}; rollback_ok={rollback_ok}"
            )))
        }
    }
}

fn remove_agent_inner(
    paths: &LifecyclePathRefs<'_>,
    agent_id: &AgentId,
    spec: &YamlValue,
    state: serde_json::Value,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveSuccess, LifecycleError> {
    // golden agents.py:75-79: when force-stopping a running worker, RE-RESOLVE the team-scoped state
    // after the stop (stop_agent persisted it); otherwise the originally-resolved projection drives the
    // removal. Either way we operate on the PROJECTION, never a raw load_runtime_state.
    let mut working_state = state;
    let mut stopped = false;
    let mut cleared_locations = Vec::new();
    if force && agent_is_running(&working_state, agent_id, transport) {
        let stop = stop_agent_at_paths(paths.run_workspace, paths.spec_workspace, agent_id, team, transport)?;
        stopped = stop.stopped;
        write_remove_step_event(
            paths.run_workspace,
            agent_id,
            "stop",
            &stop.target,
            Some(stop.stopped),
        )?;
        working_state = resolve_team_scoped_state_or_refuse(paths.run_workspace, team)?;
    }
    let dynamic_role_path = dynamic_role_file_path(paths.run_workspace, &working_state, agent_id);
    let dynamic_role_required = has_recorded_dynamic_role_file(&working_state, agent_id);
    // golden agents.py:81-83: removed_state = deepcopy(state); pop the agent; save_team_scoped_state
    // (team projection) — NOT a raw save, so other teams in a multi-team workspace are preserved.
    let mut removed_state = working_state;
    remove_agent_from_state(&mut removed_state, agent_id)?;
    crate::state::projection::save_team_scoped_state_with_deleted_agents(
        paths.run_workspace,
        &removed_state,
        &[agent_id.as_str()],
    )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    cleared_locations.push(serde_json::json!("state.json:agents"));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "workspace_state",
        "state.json:agents",
        None,
    )?;

    let removed_spec = spec_without_agent(spec, agent_id);
    if should_validate_removed_spec(&removed_spec, paths) {
        crate::model::spec::validate_spec(&removed_spec, paths.run_workspace)
            .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    }
    // golden agents.py:96-100,157: state_file = the team_state.md path written from removed_spec/state.
    let team_state_path = write_team_state(paths.spec_workspace, &removed_spec, &removed_state)?;
    cleared_locations.push(serde_json::json!(team_state_path.to_string_lossy().to_string()));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "team_state",
        &team_state_path.to_string_lossy(),
        None,
    )?;
    std::fs::write(paths.spec_workspace.join("team.spec.yaml"), yaml::dumps(&removed_spec))
        .map_err(|e| LifecycleError::StatePersist(format!("write spec: {e}")))?;
    cleared_locations.push(serde_json::json!("team.spec.yaml"));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "spec",
        "team.spec.yaml",
        None,
    )?;
    let role_file_removed = remove_dynamic_role_file(&dynamic_role_path, dynamic_role_required)?;
    if role_file_removed {
        let resource = dynamic_role_path.to_string_lossy().to_string();
        cleared_locations.push(serde_json::json!(resource));
        write_remove_step_event(
            paths.run_workspace,
            agent_id,
            "role_file",
            &dynamic_role_path.to_string_lossy(),
            None,
        )?;
    }
    let agent_health_deleted = delete_agent_health(paths.run_workspace, agent_id)?;
    cleared_locations.push(serde_json::json!("agent_health"));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "agent_health",
        "agent_health",
        None,
    )?;
    maybe_fail_remove_after_agent_health_delete()?;
    Ok(RemoveSuccess {
        outcome: RemoveAgentOutcome::Removed {
            agent_id: agent_id.clone(),
            state_file: team_state_path,
            agent_health_deleted: agent_health_deleted || role_file_removed,
        },
        removed_state,
        stopped,
        role_file_removed,
        cleared_locations,
    })
}

fn should_validate_removed_spec(spec: &YamlValue, paths: &LifecyclePathRefs<'_>) -> bool {
    let agents_empty = spec
        .get("agents")
        .and_then(YamlValue::as_list)
        .is_none_or(|agents| agents.is_empty());
    !(agents_empty && paths.spec_workspace != paths.run_workspace)
}

struct RemoveSuccess {
    outcome: RemoveAgentOutcome,
    removed_state: serde_json::Value,
    stopped: bool,
    role_file_removed: bool,
    cleared_locations: Vec<serde_json::Value>,
}

fn write_remove_step_event(
    workspace: &Path,
    agent_id: &AgentId,
    step: &str,
    resource: &str,
    stopped: Option<bool>,
) -> Result<(), LifecycleError> {
    let mut payload = serde_json::Map::new();
    payload.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    payload.insert("step".to_string(), serde_json::json!(step));
    payload.insert("resource".to_string(), serde_json::json!(resource));
    if let Some(stopped) = stopped {
        payload.insert("stopped".to_string(), serde_json::json!(stopped));
    }
    crate::event_log::EventLog::new(workspace)
        .write(
            crate::lifecycle::types::event_names::REMOVE_STEP_COMPLETED,
            serde_json::Value::Object(payload),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn write_remove_complete_event(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    stopped: bool,
    role_file_removed: bool,
    cleared_locations: Vec<serde_json::Value>,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "remove_agent.complete",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "from_spec": from_spec,
                "force": force,
                "stopped": stopped,
                "role_file_removed": role_file_removed,
                "cleared_locations": cleared_locations,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

struct RemoveRollbackEvent<'a> {
    workspace: &'a Path,
    agent_id: &'a AgentId,
    from_spec: bool,
    force: bool,
    stopped: bool,
    error: &'a LifecycleError,
    rollback_ok: bool,
    restore_errors: &'a [String],
}

fn write_remove_rollback_events(event: RemoveRollbackEvent<'_>) -> Result<(), LifecycleError> {
    let log = crate::event_log::EventLog::new(event.workspace);
    let errors = event
        .restore_errors
        .iter()
        .map(|e| serde_json::json!(e))
        .collect::<Vec<_>>();
    log.write(
        "remove_agent.rollback",
        serde_json::json!({
            "agent_id": event.agent_id.as_str(),
            "from_spec": event.from_spec,
            "force": event.force,
            "stopped": event.stopped,
            "error": event.error.to_string(),
            "rollback_ok": event.rollback_ok,
            "errors": errors,
        }),
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    log.write(
        crate::lifecycle::types::event_names::REMOVE_ROLLED_BACK,
        serde_json::json!({
            "agent_id": event.agent_id.as_str(),
            "step": "rollback",
            "resource": "workspace",
            "rollback_ok": event.rollback_ok,
            "errors": errors,
        }),
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if !event.restore_errors.is_empty() {
        log.write(
            "remove_agent.rollback_failed",
            serde_json::json!({
                "agent_id": event.agent_id.as_str(),
                "errors": errors,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    Ok(())
}

fn remove_agent_from_state(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
) -> Result<(), LifecycleError> {
    if let Some(agents) = state.get_mut("agents").and_then(|v| v.as_object_mut()) {
        agents.remove(agent_id.as_str());
        Ok(())
    } else {
        Err(LifecycleError::StatePersist(
            "runtime state agents is not an object".to_string(),
        ))
    }
}

/// Build the persisted spec after removing one worker. Besides deleting the worker and startup entry,
/// prune routing references that would otherwise point at the removed worker.
fn spec_without_agent(spec: &YamlValue, agent_id: &AgentId) -> YamlValue {
    let YamlValue::Map(pairs) = spec else {
        return spec.clone();
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "agents" {
            let agents = value
                .as_list()
                .map(|items| {
                    items
                        .iter()
                        .filter(|agent| {
                            agent
                                .get("id")
                                .and_then(YamlValue::as_str)
                                .map(|id| id != agent_id.as_str())
                                .unwrap_or(true)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.push((key.clone(), YamlValue::List(agents)));
        } else if key == "runtime" {
            out.push((key.clone(), runtime_without_startup_agent(value, agent_id)));
        } else if key == "routing" {
            out.push((key.clone(), routing_without_agent(value, agent_id)));
        } else if key == "tasks" {
            out.push((key.clone(), tasks_without_agent_assignee(value, agent_id)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    YamlValue::Map(out)
}

fn runtime_without_startup_agent(runtime: &YamlValue, agent_id: &AgentId) -> YamlValue {
    let YamlValue::Map(pairs) = runtime else {
        return runtime.clone();
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "startup_order" {
            let order = value
                .as_list()
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| item.as_str().map(|id| id != agent_id.as_str()).unwrap_or(true))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.push((key.clone(), YamlValue::List(order)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    YamlValue::Map(out)
}

fn routing_without_agent(routing: &YamlValue, agent_id: &AgentId) -> YamlValue {
    let YamlValue::Map(pairs) = routing else {
        return routing.clone();
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "default_assignee"
            && value.as_str().is_some_and(|id| id == agent_id.as_str())
        {
            out.push((key.clone(), YamlValue::Str(String::new())));
        } else if key == "rules" {
            let rules = value
                .as_list()
                .map(|items| {
                    items
                        .iter()
                        .filter(|rule| {
                            rule
                                .get("assign_to")
                                .and_then(YamlValue::as_str)
                                .map(|id| id != agent_id.as_str())
                                .unwrap_or(true)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.push((key.clone(), YamlValue::List(rules)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    YamlValue::Map(out)
}

fn tasks_without_agent_assignee(tasks: &YamlValue, agent_id: &AgentId) -> YamlValue {
    let YamlValue::List(items) = tasks else {
        return tasks.clone();
    };
    YamlValue::List(
        items
            .iter()
            .map(|task| task_without_agent_assignee(task, agent_id))
            .collect(),
    )
}

fn task_without_agent_assignee(task: &YamlValue, agent_id: &AgentId) -> YamlValue {
    let YamlValue::Map(pairs) = task else {
        return task.clone();
    };
    YamlValue::Map(
        pairs
            .iter()
            .map(|(key, value)| {
                if key == "assignee"
                    && value
                        .as_str()
                        .is_some_and(|id| id == agent_id.as_str())
                {
                    (key.clone(), YamlValue::Str(String::new()))
                } else {
                    (key.clone(), value.clone())
                }
            })
            .collect(),
    )
}

fn remove_dynamic_role_file(
    path: &Path,
    required: bool,
) -> Result<bool, LifecycleError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && required => {
            Err(LifecycleError::StatePersist(format!(
                "dynamic role file missing: {}",
                path.display()
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(LifecycleError::StatePersist(format!(
            "remove role file {}: {e}",
            path.display()
        ))),
    }
}

fn dynamic_role_file_path(
    workspace: &Path,
    state: &serde_json::Value,
    agent_id: &AgentId,
) -> std::path::PathBuf {
    if let Some(raw) = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let path = std::path::PathBuf::from(raw);
        if path.is_absolute() {
            return path;
        }
        return workspace.join(path);
    }
    workspace
        .join(".team")
        .join("dynamic-role-files")
        .join(format!("{}.md", agent_id.as_str()))
}

fn has_recorded_dynamic_role_file(state: &serde_json::Value, agent_id: &AgentId) -> bool {
    state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

fn delete_agent_health(workspace: &Path, agent_id: &AgentId) -> Result<bool, LifecycleError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let changed = conn
        .execute("delete from agent_health where agent_id = ?1", [agent_id.as_str()])
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(changed > 0)
}

/// golden agents.py:185 `copy.deepcopy(store.agent_health().get(agent_id))` — read the row BEFORE delete
/// so the rollback can re-upsert it. Returns the captured health columns, or None if absent.
fn select_agent_health(
    workspace: &Path,
    agent_id: &AgentId,
) -> Result<Option<CapturedHealth>, LifecycleError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let row = conn
        .query_row(
            "select owner_team_id, status, last_output_at, context_usage_pct, current_task_id \
             from agent_health where agent_id = ?1",
            [agent_id.as_str()],
            |r| {
                Ok(CapturedHealth {
                    owner_team_id: r.get::<_, Option<String>>(0)?,
                    status: r.get::<_, Option<String>>(1)?,
                    last_output_at: r.get::<_, Option<String>>(2)?,
                    context_usage_pct: r.get::<_, Option<i64>>(3)?,
                    current_task_id: r.get::<_, Option<String>>(4)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// golden agents.py:268-278 `_restore_agent_health`: re-upsert the captured row (status||"IDLE"), or
/// delete the row when there was nothing to restore.
fn restore_agent_health(
    workspace: &Path,
    agent_id: &AgentId,
    row: &Option<CapturedHealth>,
) -> Result<(), LifecycleError> {
    let Some(row) = row else {
        delete_agent_health(workspace, agent_id)?;
        return Ok(());
    };
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let status = row.status.clone().unwrap_or_else(|| "IDLE".to_string());
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string();
    // The restore always follows a delete of this row, so a plain insert re-materializes the captured
    // health (golden _restore_agent_health re-upserts status||"IDLE" + the captured columns).
    conn.execute(
        "insert into agent_health (owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at) \
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            row.owner_team_id,
            agent_id.as_str(),
            status,
            row.last_output_at,
            row.context_usage_pct,
            row.current_task_id,
            now,
        ],
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

#[derive(Clone)]
struct CapturedHealth {
    owner_team_id: Option<String>,
    status: Option<String>,
    last_output_at: Option<String>,
    context_usage_pct: Option<i64>,
    current_task_id: Option<String>,
}

fn maybe_fail_remove_after_agent_health_delete() -> Result<(), LifecycleError> {
    let Ok(reason) = std::env::var("TEAM_AGENT_TEST_FAIL_REMOVE_AFTER_AGENT_HEALTH_DELETE") else {
        return Ok(());
    };
    if reason.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::StatePersist(format!(
        "injected remove failure after agent_health delete: {reason}"
    )))
}

struct RemoveRollback {
    agent_id: AgentId,
    spec_text: Option<String>,
    state: serde_json::Value,
    team_state_text: Option<String>,
    team_state_path: std::path::PathBuf,
    dynamic_role_bytes: Option<Vec<u8>>,
    dynamic_role_path: std::path::PathBuf,
    /// golden agents.py:185: the agent_health row captured BEFORE delete, re-upserted on rollback.
    health: Option<CapturedHealth>,
    restore_running: bool,
}

impl RemoveRollback {
    fn capture(
        workspace: &Path,
        spec_workspace: &Path,
        spec: &YamlValue,
        state: &serde_json::Value,
        agent_id: &AgentId,
    ) -> Result<Self, LifecycleError> {
        let spec_path = spec_workspace.join("team.spec.yaml");
        let spec_text = match std::fs::read_to_string(&spec_path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(LifecycleError::StatePersist(format!("read spec: {e}"))),
        };
        let team_state_path = spec_workspace.join(
            spec.get("context")
                .and_then(|v| v.get("state_file"))
                .and_then(YamlValue::as_str)
                .unwrap_or("team_state.md"),
        );
        let team_state_text = match std::fs::read_to_string(&team_state_path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(LifecycleError::StatePersist(format!("read team_state: {e}"))),
        };
        let dynamic_role_path = dynamic_role_file_path(workspace, state, agent_id);
        let dynamic_role_bytes = match std::fs::read(&dynamic_role_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(LifecycleError::StatePersist(format!("read role file: {e}"))),
        };
        let health = select_agent_health(workspace, agent_id)?;
        Ok(Self {
            agent_id: agent_id.clone(),
            spec_text,
            state: state.clone(),
            team_state_text,
            team_state_path,
            dynamic_role_bytes,
            dynamic_role_path,
            health,
            restore_running: false,
        })
    }

    /// golden agents.py:189-227 `_RemoveRollback.restore`: BEST-EFFORT — wrap EACH artifact restore
    /// (spec → workspace_state → team_state → role_file → agent_health) in its own try/except, append
    /// per-artifact failures to `errors`, and NEVER short-circuit on the first failure. The worker is
    /// only re-started when restore_running AND no errors. Returns the collected error strings (empty
    /// == ok); the caller re-raises the ORIGINAL operation error annotated with rollback_ok.
    fn restore(
        &self,
        workspace: &Path,
        spec_workspace: &Path,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Vec<String> {
        let mut errors: Vec<String> = Vec::new();

        // spec
        let spec_path = spec_workspace.join("team.spec.yaml");
        if let Some(text) = &self.spec_text {
            if let Err(e) = std::fs::write(&spec_path, text) {
                errors.push(format!("spec:{e}"));
            }
        }
        // workspace_state
        if let Err(e) = crate::state::persist::save_runtime_state(workspace, &self.state) {
            errors.push(format!("workspace_state:{e}"));
        }
        // team_state
        let team_state_result = match &self.team_state_text {
            Some(text) => {
                if let Some(parent) = self.team_state_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&self.team_state_path, text)
            }
            None => match std::fs::remove_file(&self.team_state_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = team_state_result {
            errors.push(format!("team_state:{e}"));
        }
        // role_file
        let role_file_result = match &self.dynamic_role_bytes {
            Some(bytes) => {
                if let Some(parent) = self.dynamic_role_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&self.dynamic_role_path, bytes)
            }
            None => match std::fs::remove_file(&self.dynamic_role_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = role_file_result {
            errors.push(format!("role_file:{e}"));
        }
        // agent_health
        if let Err(e) = restore_agent_health(workspace, &self.agent_id, &self.health) {
            errors.push(format!("agent_health:{e}"));
        }

        if self.restore_running && errors.is_empty() {
            if let Err(e) = start_agent_at_paths(
                workspace,
                spec_workspace,
                &self.agent_id,
                true,
                false,
                true,
                team,
                transport,
            ) {
                errors.push(format!("worker_restore:{e}"));
            }
        }
        errors
    }
}
