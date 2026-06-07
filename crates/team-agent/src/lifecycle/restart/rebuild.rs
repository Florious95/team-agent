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
    let run_ws = lifecycle_run_workspace(workspace)?;
    restart_with_transport(
        workspace,
        allow_fresh,
        team,
        &crate::tmux_backend::TmuxBackend::for_workspace(&run_ws),
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
    if refresh_missing_provider_sessions(&mut state)? {
        crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let plan = classify_restart_plan(&state, allow_fresh)?;
    write_restart_resume_decision_events(&selected.run_workspace, &state, allow_fresh, &plan.decisions)?;
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
    }
    for (idx, decision) in plan.decisions.iter().enumerate() {
        let agent = state
            .get("agents")
            .and_then(|v| v.get(decision.agent_id.as_str()))
            .ok_or_else(|| {
                LifecycleError::RequirementUnmet(format!(
                    "agent {} not found for restart",
                    decision.agent_id
                ))
            })?;
        let session_id = if matches!(decision.restart_mode, StartMode::Resumed) {
            decision.session_id.as_ref()
        } else {
            None
        };
        let _ = spawn_agent_window(
            &selected.run_workspace,
            &session_name,
            &decision.agent_id,
            agent,
            session_id,
            idx > 0,
            transport,
            Some(&safety),
        )?;
    }
    let coordinator_started = start_coordinator_for_workspace(&selected.run_workspace)?;
    Ok(RestartReport::Restarted {
        session_name,
        agents: plan.decisions,
        coordinator_started,
    })
}

fn write_restart_resume_decision_events(
    workspace: &Path,
    state: &serde_json::Value,
    allow_fresh: bool,
    decisions: &[RestartedAgent],
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
) -> Result<(), LifecycleError> {
    use std::io::Write as _;

    let path = workspace.join(".team").join("logs").join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let event = serde_json::json!({
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
