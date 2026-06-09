use super::*;
use super::common::*;
use super::selection::classify_restart_plan;

// ── lifecycle::restart —— 整队 Route B resume-or-fresh 重建 ──────────────────

/// `restart(workspace, allow_fresh, team)`(`restart/orchestration.py:26`)。整队重建:
/// **先**算 resume 决策(Route B)+ `first_send_at` 严格校验(corrupt → hard refuse),
/// **再**做破坏性 teardown(关显示、建 session)、起后 leader rebind、adaptive 显示重建。
/// 每非 paused worker 必发一条 `restart.resume_decision`(Route B audit 契约)。
pub fn restart(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
) -> Result<RestartReport, LifecycleError> {
    restart_with_session_convergence_deadline(workspace, allow_fresh, team, None)
}

pub fn restart_with_session_convergence_deadline(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    session_converge_deadline_ms: Option<u64>,
) -> Result<RestartReport, LifecycleError> {
    let run_ws = lifecycle_run_workspace(workspace)?;
    restart_with_transport_with_session_convergence_deadline(
        workspace,
        allow_fresh,
        team,
        &crate::tmux_backend::TmuxBackend::for_workspace(&run_ws),
        session_converge_deadline_ms,
    )
}

/// `restart` with an injected transport (tests: recording mock; prod: real TmuxBackend). The Route-B
/// resume/fresh worker spawn + start_coordinator are wired here over `transport`. (rt-host-a sweep:
/// was a stub returning RequirementUnmet at the spawn boundary — never spawned/resumed/started coordinator.)
pub fn restart_with_transport(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RestartReport, LifecycleError> {
    match restart_with_transport_with_session_convergence_deadline(
        workspace,
        allow_fresh,
        team,
        transport,
        None,
    )? {
        RestartReport::RefusedResumeNotReady {
            missing,
            allow_fresh,
            error,
            ..
        } => Ok(RestartReport::RefusedResumeAtomicity {
            unresumable: missing
                .into_iter()
                .map(|agent_id| UnresumableWorker {
                    agent_id,
                    reason: "session_capture_incomplete".to_string(),
                    session_id: None,
                    first_send_at: None,
                })
                .collect(),
            allow_fresh,
            error,
        }),
        report => Ok(report),
    }
}

pub fn restart_with_transport_with_session_convergence_deadline(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
    session_converge_deadline_ms: Option<u64>,
) -> Result<RestartReport, LifecycleError> {
    if crate::lifecycle::restart::input_has_no_local_team_context(workspace) {
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec for restart: {}",
            workspace.join("team.spec.yaml").display()
        )));
    }
    let run_candidate = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if !workspace.join("team.spec.yaml").exists()
        && !crate::state::persist::runtime_state_path(&run_candidate).exists()
    {
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec for restart: {}",
            workspace.join("team.spec.yaml").display()
        )));
    }
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let mut state = selected.state;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, None)?;
    let spec_workspace = selected
        .spec_workspace
        .as_ref()
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec workspace not found".to_string()))?;
    let spec = load_team_spec(spec_workspace)?;
    let safety = crate::lifecycle::launch::effective_runtime_config(&spec)?;
    let mut convergence = converge_missing_provider_sessions(
        &mut state,
        session_convergence_deadline(session_converge_deadline_ms),
        session_convergence_poll_interval(),
        &selected.run_workspace,
        allow_fresh,
    )?;
    if convergence.converged && convergence.changed {
        crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    if repair_resume_sessions_from_event_log(&selected.run_workspace, &mut state)? {
        crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        let missing_after_repair = restart_required_missing_session_agent_ids(&state);
        convergence.changed = true;
        convergence.converged = missing_after_repair.is_empty();
        convergence.missing = missing_after_repair;
    }
    if !convergence.converged && !allow_fresh {
        return Ok(RestartReport::RefusedResumeNotReady {
            missing: convergence
                .missing
                .iter()
                .map(|agent_id| AgentId::new(agent_id.clone()))
                .collect(),
            allow_fresh,
            deadline: convergence.deadline,
            elapsed: convergence.elapsed,
            error: "resume_not_ready: session_capture_incomplete".to_string(),
        });
    }
    if !convergence.converged && convergence.changed {
        crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let forced_fresh_missing = if convergence.converged {
        std::collections::BTreeSet::new()
    } else {
        convergence.missing.iter().cloned().collect()
    };
    let forced_fresh_convergence = (!convergence.converged).then_some(convergence.clone());
    let plan = classify_restart_plan(&state, allow_fresh)?;
    write_restart_resume_decision_events(
        &selected.run_workspace,
        &state,
        allow_fresh,
        &plan.decisions,
        &forced_fresh_missing,
        forced_fresh_convergence.as_ref(),
    )?;
    if !plan.corrupt_entries.is_empty() {
        return Ok(RestartReport::RefusedInvalidFirstSendAt {
            invalid: plan.corrupt_entries,
            allow_fresh,
            error: "invalid first_send_at".to_string(),
        });
    }
    if !plan.unresumable.is_empty() {
        return Ok(RestartReport::RefusedResumeAtomicity {
            unresumable: plan.unresumable,
            allow_fresh,
            error: "restart requires resumable workers before live spawn".to_string(),
        });
    }
    let session_name = state_session_name(&state);
    if session_live_or_default(transport, &session_name, false) {
        transport
            .kill_session(&session_name)
            .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        mark_leader_receiver_rebind_required(&mut state, &session_name);
        mark_restart_targets_stopped_after_teardown(&mut state, &plan.decisions);
        crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let mut last_spawned: Option<AgentId> = None;
    for decision in &plan.decisions {
        let agent = state
            .get("agents")
            .and_then(|v| v.get(decision.agent_id.as_str()))
            .ok_or_else(|| {
                LifecycleError::RequirementUnmet(format!(
                    "agent {} not found for restart",
                    decision.agent_id
                ))
            })?
            .clone();
        let session_id = if matches!(decision.restart_mode, StartMode::Resumed) {
            decision.session_id.as_ref()
        } else {
            None
        };
        let session_live = session_live_or_default(transport, &session_name, false);
        if !session_live {
            if let Some(previous) = &last_spawned {
                return Err(LifecycleError::Transport(format!(
                    "session_disappeared_after_spawn: provider_resume_exited for {}; session {} disappeared before spawning {}",
                    previous,
                    session_name.as_str(),
                    decision.agent_id
                )));
            }
        }
        let spawn = spawn_agent_window(
            &selected.run_workspace,
            &session_name,
            &decision.agent_id,
            &agent,
            session_id,
            session_live,
            transport,
            Some(&safety),
            Some(spec_workspace),
        )?;
        verify_spawned_agent_live(&decision.agent_id, &spawn, transport)?;
        mark_agent_respawned(&mut state, &decision.agent_id, &spawn, transport, &safety)?;
        last_spawned = Some(decision.agent_id.clone());
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(decision.agent_id.as_str()))
            .and_then(serde_json::Value::as_object_mut)
        {
            persist_effective_approval_policy_for_restart(agent, &safety);
        }
    }
    crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let coordinator_started = start_coordinator_for_workspace(&selected.run_workspace)?;
    Ok(RestartReport::Restarted {
        session_name,
        agents: plan.decisions,
        coordinator_started,
    })
}

fn repair_resume_sessions_from_event_log(
    workspace: &Path,
    state: &mut serde_json::Value,
) -> Result<bool, LifecycleError> {
    let agent_ids = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| agents.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut changed = false;
    for agent_id in agent_ids {
        let previous = state
            .get("agents")
            .and_then(|agents| agents.get(&agent_id))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if previous
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| !session.is_empty())
        {
            continue;
        }
        let Some(provider) = previous
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_provider)
        else {
            continue;
        };
        let auth_mode = previous
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription);
        let exclude_session_ids = claimed_session_ids_except(state, &agent_id);
        let adapter = crate::provider::get_adapter(provider);
        let repaired = crate::session_capture::recover_resume_session_from_events(
            workspace,
            &agent_id,
            &previous,
            adapter.as_ref(),
            auth_mode,
            &exclude_session_ids,
        )
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        let Some(repaired) = repaired else {
            continue;
        };
        let old_session_id = previous
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|session| !session.is_empty())
            .map(str::to_string);
        let session_id = repaired
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|session| !session.is_empty())
            .map(str::to_string);
        let rollout_path = repaired
            .get("rollout_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(&agent_id))
        {
            *agent = repaired.clone();
        }
        crate::event_log::EventLog::new(workspace)
            .write(
                "resume.session_repaired",
                serde_json::json!({
                    "agent_id": agent_id,
                    "provider": provider_wire(provider),
                    "old_session_id": old_session_id,
                    "session_id": session_id,
                    "rollout_path": rollout_path,
                    "captured_via": "event_log_repair",
                    "attribution_confidence": repaired.get("attribution_confidence").cloned().unwrap_or(serde_json::Value::Null),
                }),
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        changed = true;
    }
    Ok(changed)
}

fn claimed_session_ids_except(
    state: &serde_json::Value,
    current_agent_id: &str,
) -> std::collections::BTreeSet<String> {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| {
            agents
                .iter()
                .filter(|(agent_id, _)| agent_id.as_str() != current_agent_id)
                .filter_map(|(_, agent)| {
                    agent
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|session| !session.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn session_convergence_deadline(requested_ms: Option<u64>) -> std::time::Duration {
    if let Some(ms) = requested_ms {
        return std::time::Duration::from_millis(ms);
    }
    env_duration_ms(
        &[
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_TIMEOUT_MS",
        ],
        crate::session_capture::RESTART_SESSION_CONVERGENCE_DEADLINE_MS,
    )
}

fn session_convergence_poll_interval() -> std::time::Duration {
    env_duration_ms(
        &[
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_POLL_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_RESTART_CAPTURE_POLL_MS",
            "TEAM_AGENT_SESSION_CAPTURE_POLL_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_POLL_MS",
        ],
        crate::session_capture::RESTART_SESSION_CONVERGENCE_POLL_MS,
    )
}

fn env_duration_ms(names: &[&str], default_ms: u64) -> std::time::Duration {
    let ms = names
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|value| parse_duration_value_ms(&value))
                .or_else(|| {
                    name.strip_suffix("_MS").and_then(|prefix| {
                        std::env::var(prefix)
                            .ok()
                            .and_then(|value| parse_duration_value_seconds_ms(&value))
                    })
                })
        })
        .unwrap_or(default_ms);
    std::time::Duration::from_millis(ms)
}

fn parse_duration_value_ms(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn parse_duration_value_seconds_ms(value: &str) -> Option<u64> {
    let seconds = value.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some((seconds * 1000.0).round() as u64)
    } else {
        None
    }
}

fn verify_spawned_agent_live(
    _agent_id: &AgentId,
    _spawn: &SpawnedAgentWindow,
    _transport: &dyn crate::transport::Transport,
) -> Result<(), LifecycleError> {
    Ok(())
}

fn mark_leader_receiver_rebind_required(state: &mut serde_json::Value, session_name: &SessionName) {
    let Some(receiver) = state
        .get_mut("leader_receiver")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let same_session = receiver
        .get("session_name")
        .and_then(|v| v.as_str())
        .map(|session| session == session_name.as_str())
        .unwrap_or(true);
    if !same_session {
        return;
    }
    if receiver
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| status == "attached")
    {
        receiver.insert(
            "status".to_string(),
            serde_json::json!("rebind_required"),
        );
    }
}

fn mark_restart_targets_stopped_after_teardown(
    state: &mut serde_json::Value,
    decisions: &[RestartedAgent],
) {
    let Some(agents) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for decision in decisions {
        let Some(agent) = agents
            .get_mut(decision.agent_id.as_str())
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        agent.insert("status".to_string(), serde_json::json!("stopped"));
        agent.remove("pane_id");
        agent.remove("pane_pid");
    }
}

fn mark_agent_respawned(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    spawn: &SpawnedAgentWindow,
    transport: &dyn crate::transport::Transport,
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(LifecycleError::StatePersist(format!(
            "agent {} state is not an object",
            agent_id
        )));
    };
    agent.insert("status".to_string(), serde_json::json!("running"));
    agent.insert(
        "pane_id".to_string(),
        serde_json::json!(spawn.spawn.pane_id.as_str()),
    );
    let pane_pid = spawn.spawn.child_pid.or_else(|| {
        transport
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .find(|pane| pane.pane_id == spawn.spawn.pane_id)
            .and_then(|pane| pane.pane_pid)
    });
    if let Some(pane_pid) = pane_pid {
        agent.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
    }
    crate::lifecycle::launch::persist_command_plan_state(
        agent,
        &spawn.plan,
        &spawn.profile_launch,
    );
    persist_effective_approval_policy_for_restart(agent, safety);
    agent.remove("startup_prompts");
    agent.remove("startup_prompt_status");
    Ok(())
}

fn write_restart_resume_decision_events(
    workspace: &Path,
    state: &serde_json::Value,
    allow_fresh: bool,
    decisions: &[RestartedAgent],
    forced_fresh_missing: &std::collections::BTreeSet<String>,
    forced_fresh_convergence: Option<&crate::session_capture::SessionConvergence>,
) -> Result<(), LifecycleError> {
    for decision in decisions {
        let agent = state
            .get("agents")
            .and_then(|v| v.get(decision.agent_id.as_str()))
            .unwrap_or(&serde_json::Value::Null);
        let first_send_at = agent
            .get("first_send_at")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let session_id = decision.session_id.as_ref().map(|s| s.as_str().to_string());
        let decision_wire = match decision.decision {
            ResumeDecision::Resume => "resume",
            ResumeDecision::FreshStart => "fresh_start",
            ResumeDecision::Refuse => "refuse",
        };
        write_restart_resume_decision_event(
            workspace,
            decision.agent_id.as_str(),
            first_send_at,
            session_id,
            allow_fresh,
            decision_wire,
            forced_fresh_missing.contains(decision.agent_id.as_str()),
            forced_fresh_convergence,
        )?;
    }
    Ok(())
}

fn write_restart_resume_decision_event(
    workspace: &Path,
    worker_id: &str,
    first_send_at: Option<String>,
    session_id: Option<String>,
    allow_fresh: bool,
    decision: &str,
    forced_fresh: bool,
    forced_fresh_convergence: Option<&crate::session_capture::SessionConvergence>,
) -> Result<(), LifecycleError> {
    use std::io::Write as _;

    let path = workspace.join(".team").join("logs").join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let mut event = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": crate::lifecycle::types::event_names::RESTART_RESUME_DECISION,
        "worker_id": worker_id,
        "has_first_send_at": first_send_at.is_some(),
        "has_session_id": session_id.is_some(),
        "allow_fresh": allow_fresh,
        "decision": decision,
        "first_send_at": first_send_at,
        "session_id": session_id,
    });
    if forced_fresh {
        if let Some(event) = event.as_object_mut() {
            event.insert("forced_fresh".to_string(), serde_json::json!(true));
            event.insert("reason".to_string(), serde_json::json!("resume_not_ready"));
            if let Some(convergence) = forced_fresh_convergence {
                event.insert(
                    "session_convergence".to_string(),
                    serde_json::json!({
                        "complete": false,
                        "deadline_s": convergence.deadline.as_secs_f64(),
                        "deadline_ms": convergence.deadline.as_millis(),
                        "elapsed_ms": convergence.elapsed.as_millis(),
                        "pending_agent_ids": convergence.missing.clone(),
                    }),
                );
            }
        }
    }
    let line = serde_json::to_string(&event)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    file.write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// `restart_candidates(workspace)`(`restart/selection.py:12`)。从 snapshot + active
/// state 收集可重启 team。
pub fn restart_candidates(workspace: &Path) -> Result<Vec<RestartCandidate>, LifecycleError> {
    let state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let alive = crate::state::projection::team_state_candidates(&state);
    if alive.is_empty() {
        if !state_has_restart_candidate_shape(&state) {
            return Ok(Vec::new());
        }
        let key = crate::state::projection::team_state_key(&state);
        return Ok(vec![restart_candidate_from_state(workspace, &key, &state)]);
    }
    Ok(alive
        .keys()
        .map(|key| {
            let projected = crate::state::projection::project_top_level_view(&state, key);
            restart_candidate_from_state(workspace, key, &projected)
        })
        .collect())
}

/// `select_restart_state(workspace, team)`(`restart/selection.py:49`)。按 `--team` 或
/// 唯一性选一个;歧义/未找到 → `TeamSelect`。
pub fn select_restart_state(
    workspace: &Path,
    team: Option<&str>,
) -> Result<RestartCandidate, LifecycleError> {
    let selected = crate::state::projection::select_runtime_state(workspace, team)
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    if !state_has_restart_candidate_shape(&selected) {
        let name = team.filter(|t| !t.is_empty()).unwrap_or("<default>");
        return Err(LifecycleError::TeamSelect(format!(
            "restart team {name:?} not found"
        )));
    }
    let key = selected
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or_else(|| crate::state::projection::team_state_key(&selected), str::to_string);
    Ok(restart_candidate_from_state(workspace, &key, &selected))
}

fn state_has_restart_candidate_shape(state: &serde_json::Value) -> bool {
    state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty())
        || state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| !agents.is_empty())
}

fn restart_candidate_from_state(
    workspace: &Path,
    team_name: &str,
    state: &serde_json::Value,
) -> RestartCandidate {
    let session_name = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .unwrap_or_else(|| SessionName::new(team_name));
    let mut agents = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|map| map.keys().map(AgentId::new).collect::<Vec<_>>())
        .unwrap_or_default();
    agents.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    RestartCandidate {
        session_name,
        team_name: team_name.to_string(),
        state_path: crate::state::persist::runtime_state_path(workspace),
        spec_path: restart_candidate_spec_path(workspace, state),
        has_context: restart_candidate_has_context(state),
        agents,
    }
}

fn restart_candidate_spec_path(workspace: &Path, state: &serde_json::Value) -> std::path::PathBuf {
    if let Some(path) = state
        .get("spec_path")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return std::path::PathBuf::from(path);
    }
    if let Some(team_dir) = state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return std::path::Path::new(team_dir).join("team.spec.yaml");
    }
    workspace.join("team.spec.yaml")
}

fn restart_candidate_has_context(state: &serde_json::Value) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                ["session_id", "rollout_path", "first_send_at"].iter().any(|key| {
                    agent
                        .get(*key)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                })
            })
        })
}
