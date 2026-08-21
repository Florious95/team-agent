//! ---
//! purpose: 摘掉一席，含双确认门、六态一致性判定、原子摘除与字节级回滚快照
//! contract:
//!   provides:
//!     - name: remove_agent
//!       what: 从 spec、state、team_state 与 role 文件原子摘除一席
//!     - name: remove_agent_flag_requirements
//!       what: 只做前置判定，告诉调用方这次需要哪些确认标志
//!     - name: ForceRecreateSnapshot
//!       what: 强制重建用的快照，可在失败后恢复席位并清掉本事务新起的 pane
//!     - name: resolve_seat
//!       what: 由期望、持久与物理三源定出席位的唯一身份与一致性态
//!     - name: spec_without_agent
//!       what: 生成摘掉该席位后的 spec，并清掉指向它的路由与启动项
//!   depends:
//!     - crate::lifecycle::lock
//!     - crate::lifecycle::restart::agent
//!     - crate::lifecycle::restart::team_state
//!     - crate::state::projection
//!     - crate::transport::Transport
//! boundary:
//!   - 未给 from_spec 确认或运行中未给 force 时拒绝，不擅自摘除
//!   - 物理身份必须收敛到唯一的 session 加 window 加 pane，全局同名窗口不算身份
//!   - 只删托管目录下的角色副本，用户自带的 role 文件不删
//! maturity: wired
//! ---
use super::agent::{resolve_team_scoped_state_or_refuse, start_agent_at_paths};
use super::common::*;
use super::team_state::write_team_state;
use super::*;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

/// ---
/// purpose: 摘掉一席的对外入口，取生命周期锁后走实体实现
/// params:
///   from_spec: 确认同时从 spec 里摘掉
///   force: 席位仍在运行时必须给出
/// returns: 摘除结果
/// errors: 选不到 team 返回 TeamSelect；确认标志不足或一致性态不允许时返回 RequirementUnmet；写盘失败返回 StatePersist
/// contract_id: lifecycle.remove_agent.entry
/// ---
/// `remove_agent(workspace, agent_id, from_spec, force, team)`(`lifecycle/agents.py:22`)。
/// 从 spec/state/team_state/agent_health 原子摘除。
/// 托管目录 `.team/dynamic-role-files/` 下的物化副本随席位清掉；托管目录之外的
/// `--role-file` 仍是用户资产，不删。A-28 对托管文件「默认保留」的承诺已由
/// `ledger.seat-supply-prereq` 推翻。
/// `_RemoveRollback` 字节级快照回滚全部运行时变更。未传 from_spec 确认 / 运行中未传 force → 拒绝。
pub fn remove_agent(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let canonical_team = paths.canonical_team(team).map(str::to_string);
    let team = canonical_team.as_deref();
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &paths.run_workspace,
        operation: "remove-agent",
        team,
        agent_id: Some(agent_id),
    })?;
    let transport = paths.tmux_backend()?;
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

/// ---
/// purpose: 带注入 transport 的摘席入口，自行取锁
/// returns: 摘除结果
/// errors: 同 remove_agent
/// contract_id: lifecycle.remove_agent.entry
/// ---
pub(crate) fn remove_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let canonical_team = paths.canonical_team(team).map(str::to_string);
    let team = canonical_team.as_deref();
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

/// ---
/// purpose: 调用方已持有生命周期锁时的摘席入口，本函数不再取锁
/// returns: 摘除结果
/// errors: 同 remove_agent
/// contract_id: lifecycle.remove_agent.entry
/// ---
pub(crate) fn remove_agent_with_transport_locked(
    workspace: &Path,
    agent_id: &AgentId,
    from_spec: bool,
    force: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RemoveAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let canonical_team = paths.canonical_team(team).map(str::to_string);
    let team = canonical_team.as_deref();
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
/// ---
/// purpose: 为强制重建拍一份可回滚的快照
/// returns: 含逻辑回滚数据与摘除前物理 pane 身份的快照
/// errors: 选不到 team 或席位解析失败时返回 LifecycleError
/// ---
    pub(crate) fn capture(
        workspace: &Path,
        agent_id: &AgentId,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Result<Self, LifecycleError> {
        let paths = lifecycle_paths(workspace, team)?;
        let canonical_team = paths.canonical_team(team).map(str::to_string);
        let team = canonical_team.as_deref();
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

/// ---
/// purpose: 按快照恢复席位的逻辑状态
/// returns: 恢复过程中的错误描述列表，空表示恢复干净
/// ---
    pub(crate) fn restore(
        &self,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Vec<String> {
        self.rollback
            .restore(&self.run_workspace, &self.spec_workspace, team, transport)
    }

/// ---
/// purpose: 旧 pane 已被消费后的恢复，先杀掉本次事务新起的 pane 再恢复逻辑快照
/// returns: 错误描述列表；恢复干净时还会校验物理身份是否回到摘除前的 session 与窗口
/// ---
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

/// ---
/// purpose: 强制重建之后要求席位处于一致态
/// returns: 一致时返回空值
/// errors: 解析失败透传；解析出的一致性态不是 Coherent 时返回 StatePersist
/// ---
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

/// ---
/// purpose: 只做前置判定，给出这次摘席需要哪些确认标志
/// returns: 标志要求；不做任何摘除动作
/// errors: 选不到 team 或席位解析失败时返回 LifecycleError
/// ---
pub fn remove_agent_flag_requirements(
    workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
) -> Result<RemoveAgentFlagRequirements, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let canonical_team = paths.canonical_team(team).map(str::to_string);
    let team = canonical_team.as_deref();
    let transport = paths.tmux_backend()?;
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

/// ---
/// purpose: 由 spec、runtime state 与物理 pane 三源定出席位身份与一致性态
/// params:
///   transport: 已绑定该 team endpoint 的 transport
/// returns: 席位解析结果，含所在 session、窗口、物理 pane 与六态之一的一致性判定
/// errors: 团队作用域 state 取不到或 owner 门不过时返回 LifecycleError
/// ---
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
    let recorded_role_file = recorded_dynamic_role_file(&working_state, agent_id);
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
    // golden agents.py:81-83: removed_state = deepcopy(state); pop the agent; save_team_scoped_state
    // (team projection) — NOT a raw save, so other teams in a multi-team workspace are preserved.
    let mut removed_state = working_state;
    remove_agent_from_state(&mut removed_state, agent_id)?;
    mark_agent_retired_in_state(&mut removed_state, agent_id)?;
    crate::state::repository::StateRepository::new(paths.run_workspace)
        .save(
            crate::state::repository::StateWriteIntent::RemoveAgent {
                team_key,
                agent_id: agent_id.as_str(),
            },
            &removed_state,
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
    // Managed copies under `.team/dynamic-role-files/` are framework residue
    // and must not block the next same-id clone. External --role-file paths
    // stay user-owned. Classify by the path's directory (do not follow a
    // last-component symlink); unlink with remove_file so only the link dies.
    let role_file_removed = clear_managed_role_residue(
        paths.run_workspace,
        agent_id,
        recorded_role_file.as_deref(),
        &mut cleared_locations,
    )?;
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

fn recorded_dynamic_role_file(
    state: &serde_json::Value,
    agent_id: &AgentId,
) -> Option<std::path::PathBuf> {
    state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

fn resolve_role_path(workspace: &Path, role_file: &Path) -> std::path::PathBuf {
    if role_file.is_absolute() {
        role_file.to_path_buf()
    } else {
        workspace.join(role_file)
    }
}

fn default_managed_role_file(workspace: &Path, agent_id: &AgentId) -> std::path::PathBuf {
    workspace
        .join(".team")
        .join("dynamic-role-files")
        .join(format!("{}.md", agent_id.as_str()))
}

/// Same prefix rule as `role_source_ownership`, but canonicalize the parent
/// only. Following the last component would classify a managed symlink whose
/// target lives outside the managed dir as external, and leave residue.
fn role_path_is_managed(workspace: &Path, role_file: &Path) -> bool {
    let managed_root = workspace.join(".team").join("dynamic-role-files");
    let Ok(root) = std::fs::canonicalize(&managed_root) else {
        return false;
    };
    let abs = resolve_role_path(workspace, role_file);
    let Some(parent) = abs.parent() else {
        return false;
    };
    match std::fs::canonicalize(parent) {
        Ok(parent_canon) => parent_canon.starts_with(&root),
        Err(_) => false,
    }
}

fn unlink_role_path(path: &Path) -> Result<bool, LifecycleError> {
    match std::fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(LifecycleError::StatePersist(format!(
            "inspect managed role file {}: {err}",
            path.display()
        ))),
        Ok(_) => std::fs::remove_file(path).map(|_| true).map_err(|err| {
            LifecycleError::StatePersist(format!(
                "remove managed role file {}: {err}",
                path.display()
            ))
        }),
    }
}

fn clear_managed_role_residue(
    workspace: &Path,
    agent_id: &AgentId,
    recorded: Option<&Path>,
    cleared_locations: &mut Vec<serde_json::Value>,
) -> Result<bool, LifecycleError> {
    let target = match recorded {
        Some(recorded) => {
            let abs = resolve_role_path(workspace, recorded);
            if !role_path_is_managed(workspace, &abs) {
                return Ok(false);
            }
            abs
        }
        None => default_managed_role_file(workspace, agent_id),
    };
    if !unlink_role_path(&target)? {
        return Ok(false);
    }
    let resource = target.to_string_lossy().into_owned();
    write_remove_step_event(workspace, agent_id, "role_file", &resource, None)?;
    cleared_locations.push(serde_json::json!(resource));
    Ok(true)
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

/// ---
/// purpose: 在 state 里给该席位打上退役标记
/// params:
///   state: 就地写 agent_lifecycle 下该席位的状态、时间与原因
/// returns: 已是退役态时幂等返回
/// errors: state 根、agent_lifecycle 或该条目不是对象时返回 StatePersist
/// ---
pub(crate) fn mark_agent_retired_in_state(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
) -> Result<(), LifecycleError> {
    let root = state.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("runtime state root is not an object".to_string())
    })?;
    let lifecycle = root
        .entry("agent_lifecycle".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let lifecycle = lifecycle.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("runtime state agent_lifecycle is not an object".to_string())
    })?;
    let entry = lifecycle
        .entry(agent_id.as_str().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if entry.get("state").and_then(serde_json::Value::as_str) == Some("retired") {
        return Ok(());
    }
    let entry = entry.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist(format!(
            "runtime state agent_lifecycle.{} is not an object",
            agent_id.as_str()
        ))
    })?;
    entry.insert("state".to_string(), serde_json::json!("retired"));
    entry.insert(
        "changed_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    entry.insert("reason".to_string(), serde_json::json!("remove-agent"));
    Ok(())
}

/// ---
/// purpose: 清掉该席位的退役标记
/// params:
///   state: 就地删除；只有当前确为退役态才删
/// ---
pub(crate) fn clear_agent_retirement_in_state(state: &mut serde_json::Value, agent_id: &AgentId) {
    let Some(lifecycle) = state
        .get_mut("agent_lifecycle")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let is_retired = lifecycle
        .get(agent_id.as_str())
        .and_then(|entry| entry.get("state"))
        .and_then(serde_json::Value::as_str)
        == Some("retired");
    if is_retired {
        lifecycle.remove(agent_id.as_str());
    }
}

/// ---
/// purpose: 生成摘掉该席位后的 spec
/// returns: 去掉该 agent、去掉它的启动项、并清掉指向它的路由引用后的 spec；spec 不是 map 时原样返回
/// ---
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
        let health = select_agent_health(workspace, team_key, agent_id)?;
        Ok(Self {
            agent_id: agent_id.clone(),
            team_key: team_key.to_string(),
            spec_text,
            state: state.clone(),
            team_state_text,
            team_state_path,
            health,
            restore_running: false,
        })
    }

    /// golden agents.py:189-227 `_RemoveRollback.restore`: BEST-EFFORT — wrap EACH artifact restore
    /// (spec → workspace_state → team_state → agent_health) in its own try/except, append
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
