use super::agent::{resolve_team_scoped_state_or_refuse, start_agent_at_paths};
use super::common::*;
use super::team_state::write_team_state;
use super::*;
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
    let transport = lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
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

pub(crate) fn remove_agent_with_transport_locked(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
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

pub(crate) struct ForceRecreateSnapshot {
    rollback: RemoveRollback,
    run_workspace: std::path::PathBuf,
    spec_workspace: std::path::PathBuf,
    before_physical: Option<crate::transport::PaneInfo>,
}

impl ForceRecreateSnapshot {
    pub(crate) fn capture(
        workspace: &Path,
        agent_id: &AgentId,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Result<Self, LifecycleError> {
        let paths = lifecycle_paths(workspace, team)?;
        let seat = resolve_seat(
            &paths.run_workspace,
            &paths.spec_workspace,
            agent_id,
            team,
            transport,
        )?;
        let mut rollback = RemoveRollback::capture(
            &paths.run_workspace,
            &paths.spec_workspace,
            &seat.spec,
            &seat.state,
            &seat.team_key,
            agent_id,
        )?;
        rollback.restore_running = seat.physical.is_some();
        Ok(Self {
            rollback,
            run_workspace: paths.run_workspace,
            spec_workspace: paths.spec_workspace,
            before_physical: seat.physical,
        })
    }

    pub(crate) fn restore(
        &self,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Vec<String> {
        self.rollback
            .restore(&self.run_workspace, &self.spec_workspace, team, transport)
    }

    /// The old pane has already been consumed. Any exact pane now resolved for
    /// this seat belongs to this force-recreate transaction and must be removed
    /// before the logical snapshot is restored, otherwise rollback can leave a
    /// duplicate worker behind.
    pub(crate) fn restore_after_consumption(
        &self,
        transport: &dyn crate::transport::Transport,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        match resolve_seat(
            &self.run_workspace,
            &self.spec_workspace,
            &self.rollback.agent_id,
            Some(self.rollback.team_key.as_str()),
            transport,
        ) {
            Ok(after) => {
                if let Some(pane) = after.physical {
                    if let Err(error) = transport.kill_pane(&pane.pane_id) {
                        errors.push(format!(
                            "transaction_pane:{}:{error}",
                            pane.pane_id.as_str()
                        ));
                    }
                }
            }
            Err(error) => errors.push(format!("transaction_resolve:{error}")),
        }
        errors.extend(self.rollback.restore(
            &self.run_workspace,
            &self.spec_workspace,
            Some(self.rollback.team_key.as_str()),
            transport,
        ));
        if errors.is_empty() {
            if let Some(before) = &self.before_physical {
                match resolve_seat(
                    &self.run_workspace,
                    &self.spec_workspace,
                    &self.rollback.agent_id,
                    Some(self.rollback.team_key.as_str()),
                    transport,
                ) {
                    Ok(after)
                        if after.physical.as_ref().is_some_and(|pane| {
                            pane.session == before.session && pane.window_name == before.window_name
                        }) => {}
                    Ok(after) => errors.push(format!(
                        "worker_restore:before physical tuple not restored: {:?}",
                        after.consistency
                    )),
                    Err(error) => errors.push(format!("worker_restore_resolve:{error}")),
                }
            }
        }
        errors
    }

    pub(crate) fn require_coherent(
        &self,
        agent_id: &AgentId,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Result<(), LifecycleError> {
        let after = resolve_seat(
            &self.run_workspace,
            &self.spec_workspace,
            agent_id,
            team,
            transport,
        )?;
        if after.consistency == SeatConsistency::Coherent {
            Ok(())
        } else {
            Err(LifecycleError::StatePersist(format!(
                "force-recreate post-resolve for {agent_id} is {:?}",
                after.consistency
            )))
        }
    }
}

pub fn remove_agent_flag_requirements(
    workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
) -> Result<RemoveAgentFlagRequirements, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let transport = lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
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
    seat: ResolvedSeat,
    requirements: RemoveAgentFlagRequirements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeatConsistency {
    Absent,
    Coherent,
    StateOnly,
    SpecOnly,
    PhysicalOnly,
    Mixed,
}

pub(super) struct ResolvedSeat {
    pub(super) state: serde_json::Value,
    pub(super) spec: YamlValue,
    pub(super) team_key: String,
    pub(super) session: crate::transport::SessionName,
    pub(super) window: String,
    pub(super) physical: Option<crate::transport::PaneInfo>,
    pub(super) state_present: bool,
    pub(super) spec_present: bool,
    pub(super) consistency: SeatConsistency,
}

/// Resolve one seat from the selected team's desired, persisted and physical
/// sources. The transport is already bound to the selected team's endpoint;
/// physical identity is then narrowed to exactly one `(session, window, pane)`
/// tuple. A global window-name match is never accepted as identity.
pub(super) fn resolve_seat(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<ResolvedSeat, LifecycleError> {
    let state = resolve_team_scoped_state_or_refuse(workspace, team)?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, Some(agent_id))?;
    let spec = load_team_spec(spec_workspace)?;
    let team_key = state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::state::projection::team_state_key(&state));
    let state_present = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .is_some();
    let spec_present = find_spec_agent(&spec, agent_id).is_some();
    let session = state_session_name_from_spec(&state, &spec);
    let window = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .and_then(|agent| agent.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|window| !window.is_empty())
        .unwrap_or_else(|| agent_id.as_str())
        .to_string();
    let stored_pane_id = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .and_then(|agent| agent.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
        .map(crate::transport::PaneId::new);
    let mut physical = transport
        .list_targets()
        .map_err(|error| LifecycleError::Transport(format!("resolve seat targets: {error}")))?
        .into_iter()
        .filter(|pane| {
            pane.session == session
                && pane
                    .window_name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == window)
                && stored_pane_id
                    .as_ref()
                    .is_none_or(|stored| pane.pane_id == *stored)
        });
    let mut first = physical.next();
    if physical.next().is_some() {
        return Err(LifecycleError::RequirementUnmet(format!(
            "seat identity ambiguous for {agent_id}: session={} window={window}",
            session.as_str()
        )));
    }
    // Some backends can positively probe an exact pane id even when their
    // global target snapshot is temporarily empty (for example an attached
    // explicit tmux socket during reset). The persisted tuple is scoped by the
    // selected transport endpoint and session; a positive exact-pane probe is
    // therefore safe, unlike a reverse window-name scan.
    if first.is_none() {
        if let Some(pane_id) = stored_pane_id
            .as_ref()
            .filter(|pane| transport.has_pane(pane).ok().flatten() == Some(true))
        {
            first = Some(crate::transport::PaneInfo {
                pane_id: pane_id.clone(),
                session: session.clone(),
                window_index: None,
                window_name: Some(crate::transport::WindowName::new(&window)),
                pane_index: None,
                tty: None,
                current_command: None,
                current_path: None,
                active: true,
                pane_pid: state
                    .get("agents")
                    .and_then(|agents| agents.get(agent_id.as_str()))
                    .and_then(|agent| agent.get("pane_pid"))
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok()),
                leader_env: std::collections::BTreeMap::new(),
            });
        }
    }
    let physical_present = first.is_some();
    let consistency = match (spec_present, state_present, physical_present) {
        (false, false, false) => SeatConsistency::Absent,
        (true, true, true) => SeatConsistency::Coherent,
        (false, true, false) => SeatConsistency::StateOnly,
        (true, false, false) => SeatConsistency::SpecOnly,
        (false, false, true) => SeatConsistency::PhysicalOnly,
        _ => SeatConsistency::Mixed,
    };
    Ok(ResolvedSeat {
        state,
        spec,
        team_key,
        session,
        window,
        physical: first,
        state_present,
        spec_present,
        consistency,
    })
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
    let seat = resolve_seat(workspace, spec_workspace, agent_id, team, transport)?;
    let spec_agent = find_spec_agent(&seat.spec, agent_id);
    // A persisted-only seat is still known and force-removable.
    let dynamic_agent =
        spec_agent.is_none_or(|agent| is_dynamic_agent(&seat.state, agent, agent_id));
    let force_required =
        seat.physical.is_some() || agent_is_running(&seat.state, agent_id, transport);
    let has_session = seat
        .state
        .get("session_name")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
        || seat
            .state
            .get("agents")
            .and_then(|v| v.get(agent_id.as_str()))
            .is_some_and(agent_has_session);
    Ok(RemoveAgentPreflight {
        seat,
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
    if preflight.seat.consistency == SeatConsistency::Absent && !force {
        return Err(unknown_worker(agent_id));
    }
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
        &preflight.seat.spec,
        &preflight.seat.state,
        &preflight.seat.team_key,
        agent_id,
    )?;
    rollback.restore_running = force && preflight.seat.physical.is_some();
    let result = remove_agent_inner(
        &paths,
        agent_id,
        &preflight.seat.spec,
        preflight.seat.state,
        preflight.seat.physical,
        &preflight.seat.team_key,
        force,
        team,
        transport,
    )
    .and_then(|success| {
        let after = resolve_seat(workspace, spec_workspace, agent_id, team, transport)?;
        if after.consistency != SeatConsistency::Absent {
            return Err(LifecycleError::StatePersist(format!(
                "remove-agent post-resolve for {agent_id} is {:?}",
                after.consistency
            )));
        }
        Ok(success)
    });
    match result {
        Ok(success) => {
            // Foundation-0 F0-2: the historical dual-write to the legacy
            // per-session snapshot has been retired
            // (`.team/artifacts/foundation-0-slice-design.md` §§4-5).
            // Root/projection is the sole runtime authority; the
            // snapshot writer stayed in `lifecycle::helpers` only for
            // diagnostic/migration/test callers.
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
            let restore_errors =
                rollback.restore(paths.run_workspace, paths.spec_workspace, team, transport);
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
    physical: Option<crate::transport::PaneInfo>,
    team_key: &str,
    force: bool,
    _team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveSuccess, LifecycleError> {
    // golden agents.py:75-79: when force-stopping a running worker, RE-RESOLVE the team-scoped state
    // after the stop (stop_agent persisted it); otherwise the originally-resolved projection drives the
    // removal. Either way we operate on the PROJECTION, never a raw load_runtime_state.
    let working_state = state;
    let mut stopped = false;
    let mut cleared_locations = Vec::new();
    if force {
        if let Some(pane) = physical {
            transport.kill_pane(&pane.pane_id).map_err(|error| {
                LifecycleError::Transport(format!(
                    "failed to stop exact seat pane {} for {agent_id}: {error}",
                    pane.pane_id.as_str()
                ))
            })?;
            stopped = true;
            let target = pane.pane_id.as_str().to_string();
            write_remove_step_event(paths.run_workspace, agent_id, "stop", &target, Some(true))?;
        }
    }
    let dynamic_role_path =
        managed_dynamic_role_file_path(paths.run_workspace, &working_state, agent_id)?;
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
    cleared_locations.push(serde_json::json!(team_state_path
        .to_string_lossy()
        .to_string()));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "team_state",
        &team_state_path.to_string_lossy(),
        None,
    )?;
    std::fs::write(
        paths.spec_workspace.join("team.spec.yaml"),
        yaml::dumps(&removed_spec),
    )
    .map_err(|e| LifecycleError::StatePersist(format!("write spec: {e}")))?;
    cleared_locations.push(serde_json::json!("team.spec.yaml"));
    write_remove_step_event(
        paths.run_workspace,
        agent_id,
        "spec",
        "team.spec.yaml",
        None,
    )?;
    let role_file_removed = match dynamic_role_path.as_deref() {
        Some(path) => remove_dynamic_role_file(path, dynamic_role_required)?,
        None => false,
    };
    if role_file_removed {
        let dynamic_role_path = dynamic_role_path.as_deref().expect("managed role path");
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
    let agent_health_deleted = delete_agent_health(paths.run_workspace, team_key, agent_id)?;
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
pub(crate) fn spec_without_agent(spec: &YamlValue, agent_id: &AgentId) -> YamlValue {
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
                        .filter(|item| {
                            item.as_str()
                                .map(|id| id != agent_id.as_str())
                                .unwrap_or(true)
                        })
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
        if key == "default_assignee" && value.as_str().is_some_and(|id| id == agent_id.as_str()) {
            out.push((key.clone(), YamlValue::Str(String::new())));
        } else if key == "rules" {
            let rules = value
                .as_list()
                .map(|items| {
                    items
                        .iter()
                        .filter(|rule| {
                            rule.get("assign_to")
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
                if key == "assignee" && value.as_str().is_some_and(|id| id == agent_id.as_str()) {
                    (key.clone(), YamlValue::Str(String::new()))
                } else {
                    (key.clone(), value.clone())
                }
            })
            .collect(),
    )
}

fn remove_dynamic_role_file(path: &Path, required: bool) -> Result<bool, LifecycleError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && required => Err(
            LifecycleError::StatePersist(format!("dynamic role file missing: {}", path.display())),
        ),
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

/// Resolve a deletable role artifact. `dynamic_role_file` may point at an
/// external `--role-file`; only canonical children of the runtime-managed
/// directory belong to remove/rollback. A symlink escape is external.
fn managed_dynamic_role_file_path(
    workspace: &Path,
    state: &serde_json::Value,
    agent_id: &AgentId,
) -> Result<Option<std::path::PathBuf>, LifecycleError> {
    let path = dynamic_role_file_path(workspace, state, agent_id);
    let managed_root = workspace.join(".team").join("dynamic-role-files");
    let canonical_root = match std::fs::canonicalize(&managed_root) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.starts_with(&managed_root).then_some(path));
        }
        Err(error) => {
            return Err(LifecycleError::StatePersist(format!(
                "resolve managed role root {}: {error}",
                managed_root.display()
            )))
        }
    };
    let canonical_path = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.starts_with(&managed_root).then_some(path));
        }
        Err(error) => {
            return Err(LifecycleError::StatePersist(format!(
                "resolve role file {}: {error}",
                path.display()
            )))
        }
    };
    Ok(canonical_path
        .starts_with(&canonical_root)
        .then_some(canonical_path))
}

fn has_recorded_dynamic_role_file(state: &serde_json::Value, agent_id: &AgentId) -> bool {
    state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

fn delete_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
) -> Result<bool, LifecycleError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let changed = conn
        .execute(
            "delete from agent_health where owner_team_id = ?1 and agent_id = ?2",
            rusqlite::params![owner_team_id, agent_id.as_str()],
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(changed > 0)
}

// Phase-DX E2: `select_agent_health` / `restore_agent_health` / `CapturedHealth` moved to
// `db::agent_health_capture` so the SQL column references (agent_health backup columns)
// live in the persistence layer (whitelisted by the E2 grep guard) rather than lifecycle
// policy code. The wrappers below preserve the existing `LifecycleError` surface.
use crate::db::agent_health_capture::{
    restore_agent_health as capture_restore_agent_health,
    select_agent_health as capture_select_agent_health, CapturedHealth,
};

fn select_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
) -> Result<Option<CapturedHealth>, LifecycleError> {
    capture_select_agent_health(workspace, owner_team_id, agent_id)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn restore_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
    row: &Option<CapturedHealth>,
) -> Result<(), LifecycleError> {
    capture_restore_agent_health(workspace, owner_team_id, agent_id, row)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
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
    team_key: String,
    spec_text: Option<String>,
    state: serde_json::Value,
    team_state_text: Option<String>,
    team_state_path: std::path::PathBuf,
    dynamic_role_bytes: Option<Vec<u8>>,
    dynamic_role_path: Option<std::path::PathBuf>,
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
        team_key: &str,
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
            Err(e) => {
                return Err(LifecycleError::StatePersist(format!(
                    "read team_state: {e}"
                )))
            }
        };
        let dynamic_role_path = managed_dynamic_role_file_path(workspace, state, agent_id)?;
        let dynamic_role_bytes = match dynamic_role_path.as_deref() {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(LifecycleError::StatePersist(format!("read role file: {e}"))),
            },
            None => None,
        };
        let health = select_agent_health(workspace, team_key, agent_id)?;
        Ok(Self {
            agent_id: agent_id.clone(),
            team_key: team_key.to_string(),
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
        if let Err(e) = crate::state::repository::StateRepository::new(workspace).save(
            crate::state::repository::StateWriteIntent::ForceRecreateRollback {
                team_key: &self.team_key,
                agent_id: self.agent_id.as_str(),
            },
            &self.state,
        ) {
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
        let role_file_result = match (&self.dynamic_role_path, &self.dynamic_role_bytes) {
            (Some(path), Some(bytes)) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(path, bytes)
            }
            (Some(path), None) => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            },
            (None, _) => Ok(()),
        };
        if let Err(e) = role_file_result {
            errors.push(format!("role_file:{e}"));
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
        // Starting a replacement cohort intentionally clears stale health, so
        // rollback must restore the captured row after the old seat is back.
        if let Err(e) =
            restore_agent_health(workspace, &self.team_key, &self.agent_id, &self.health)
        {
            errors.push(format!("agent_health:{e}"));
        }
        errors
    }
}
