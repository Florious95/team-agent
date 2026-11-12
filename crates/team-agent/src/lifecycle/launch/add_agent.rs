//! ---
//! purpose: 动态角色文档加一席，失败按快照回滚
//! contract:
//!   provides:
//!     - name: add_agent
//!       what: 编译角色进 runtime spec 并起席，失败回滚 spec 与 state
//!     - name: add_agent_with_transport_at_paths
//!       what: 加席的实体实现，含 owner 门、重名拒绝、原子写 spec 与起席
//!   depends:
//!     - crate::lifecycle::lock
//!     - crate::lifecycle::restart
//!     - crate::lifecycle::restart::remove
//!     - crate::compiler
//!     - crate::state::selector
//!     - crate::state::projection
//!     - crate::state::persist
//!     - crate::tmux_backend
//! boundary:
//!   - 不拷贝外部角色文件进 team 目录，就地读取编译
//!   - 起席一律走 restart 的 start_agent_at_paths，本文件不直接 spawn
//!   - 回滚只恢复 spec 字节与 runtime state，不回收已 spawn 的 pane
//! maturity: wired
//! ---
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::lifecycle::*;
use crate::model::ids::AgentId;
use crate::model::yaml::Value;
use crate::transport::Transport;

use crate::lifecycle::lock::{acquire_seat_lifecycle_lock, LifecycleLockRequest};

use super::*;

struct AgentReservation {
    workspace: PathBuf,
    team_key: String,
    agent_id: AgentId,
    owner: String,
    active: bool,
}

impl AgentReservation {
    fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for AgentReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let request = LifecycleLockRequest {
            workspace: &self.workspace,
            operation: "agent-reservation-rollback",
            team: Some(self.team_key.as_str()),
            agent_id: Some(&self.agent_id),
        };
        let Ok(_lock) = acquire_seat_lifecycle_lock(request) else {
            let _ = crate::event_log::EventLog::new(&self.workspace).write(
                "lifecycle.clone_reservation_rollback_failed",
                serde_json::json!({
                    "agent_id": self.agent_id.as_str(),
                    "reservation_owner": self.owner,
                    "reason": "lifecycle lock unavailable",
                }),
            );
            return;
        };
        let result =
            release_agent_reservation(&self.workspace, &self.team_key, &self.agent_id, &self.owner);
        let _ = crate::event_log::EventLog::new(&self.workspace).write(
            if result.is_ok() {
                "lifecycle.clone_reservation_rolled_back"
            } else {
                "lifecycle.clone_reservation_rollback_failed"
            },
            serde_json::json!({
                "agent_id": self.agent_id.as_str(),
                "reservation_owner": self.owner,
                "result": result.as_ref().err().map(ToString::to_string),
            }),
        );
    }
}

fn reserve_agent_slot(
    workspace: &Path,
    team: Option<&str>,
    agent_id: &AgentId,
) -> Result<AgentReservation, LifecycleError> {
    let state = crate::state::projection::select_runtime_state(workspace, team)
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let team_key = team
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| explicit_active_team_key(&state))
        .unwrap_or_else(|| crate::state::projection::team_state_key(&state));
    let owner = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| LifecycleError::StatePersist(error.to_string()))?
            .as_nanos()
    );
    let mut latest = crate::state::projection::select_runtime_state(workspace, Some(&team_key))
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    ensure_owner_allowed_for_state(&latest, Some(agent_id))?;
    if runtime_agent_exists(&latest, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    latest
        .as_object_mut()
        .ok_or_else(|| LifecycleError::StatePersist("runtime state root is not an object".into()))?
        .entry("agents")
        .or_insert_with(|| serde_json::json!({}));
    let agents = latest
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            LifecycleError::StatePersist("runtime state agents is not an object".into())
        })?;
    agents.insert(
        agent_id.as_str().to_string(),
        serde_json::json!({
            "agent_id": agent_id.as_str(),
            "status": "reserved",
            "reservation_owner": owner,
        }),
    );
    save_launched_team_state_for_key(
        workspace,
        &latest,
        Some(team_key.as_str()),
        Some(agent_id.as_str()),
    )?;
    let _ = crate::event_log::EventLog::new(workspace).write(
        "lifecycle.clone_reservation_acquired",
        serde_json::json!({
            "agent_id": agent_id.as_str(),
            "reservation_owner": owner,
            "team": team_key,
        }),
    );
    Ok(AgentReservation {
        workspace: workspace.to_path_buf(),
        team_key,
        agent_id: agent_id.clone(),
        owner,
        active: true,
    })
}

fn release_agent_reservation(
    workspace: &Path,
    team_key: &str,
    agent_id: &AgentId,
    owner: &str,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::projection::select_runtime_state(workspace, Some(team_key))
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let matches = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .and_then(|agent| agent.get("reservation_owner"))
        .and_then(serde_json::Value::as_str)
        == Some(owner);
    if !matches {
        return Ok(());
    }
    state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            LifecycleError::StatePersist("runtime state agents is not an object".into())
        })?
        .remove(agent_id.as_str());
    save_launched_team_state_for_key(workspace, &state, Some(team_key), Some(agent_id.as_str()))
}

/// ---
/// purpose: 加一席的默认入口，解析活跃 team、拿生命周期锁、路由到该 team 实际使用的 tmux socket
/// params:
///   role_file_path: 外部角色文档路径，就地读取不拷贝
///   open_display: 是否为新席开显示
/// returns: 新席的环境与启动模式
/// errors: 选不到 team 返回 TeamSelect，角色文件缺失或编译失败返回 Compile，重名返回 RequirementUnmet
/// contract_id: lifecycle.add_agent.entry
/// ---
/// `add_agent(workspace, agent_id, role_file_path, open_display, team)`
/// (`lifecycle/operations.py:143`)。动态 role doc 编译进 spec + 起 worker;失败**字节级回滚**
/// spec_yaml / workspace_state / **team_state.md** / role_file(Gap 15.11),每步发
/// `lifecycle.add_step_*` 事件(顺序被测试锁死)。
pub fn add_agent(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
) -> Result<AddAgentReport, LifecycleError> {
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        Ok(selected) => selected,
        Err(_) if workspace.join("TEAM.md").exists() => {
            // TEAM.md lives in the run workspace. `team_workspace()` always walks
            // to the parent and would spawn on a hash socket of the scratch parent.
            // Use the idempotent run-workspace so persisted `tmux_endpoint` wins.
            let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
                .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            let transport =
                crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
                    &run_workspace, team,
                )
                .unwrap_or_else(|_| {
                    crate::tmux_backend::TmuxBackend::for_workspace(&run_workspace)
                });
            return add_agent_with_transport(
                workspace,
                agent_id,
                role_file_path,
                open_display,
                team,
                &transport,
            );
        }
        Err(error) => return Err(LifecycleError::TeamSelect(error.to_string())),
    };
    let lifecycle_lock = acquire_seat_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "add-agent",
        team: Some(selected.team_key.as_str()),
        agent_id: Some(agent_id),
    })?;
    let reservation = reserve_agent_slot(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
        agent_id,
    )?;
    drop(lifecycle_lock);
    // E5 §3:compile_team 要角色定义目录(team_dir),不是 spec 落点(spec_workspace=runtime)。
    let team_dir = selected.team_dir;
    // **0.3.24 add-agent socket drift fix**: route to the live team's persisted
    // tmux endpoint (NOT the workspace-hash for_workspace socket). Without this,
    // `add-agent` spawns into an orphan socket (e.g. `ta-<hash>/termclaud`) while
    // the live team runs on its persisted default socket — the leader can't see
    // the new window, state never registers, and the orphaned `claude` process
    // floats forever (macmini repro: `demo-director` startup blocker).
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    add_agent_with_transport_at_paths_reserved(
        &selected.run_workspace,
        &team_dir,
        agent_id,
        role_file_path,
        open_display,
        Some(selected.team_key.as_str()),
        &transport,
        Some(reservation),
        false,
        None,
        None,
    )
}

/// ---
/// purpose: 带注入 transport 的加席入口，归一 workspace 并拿锁后转实体实现
/// returns: 新席的环境与启动模式
/// errors: 归一 workspace 失败返回 StatePersist，其余透传
/// contract_id: lifecycle.add_agent.entry
/// ---
/// `add_agent` with an injected transport — after the recompile+write, wires the new worker spawn
/// (via start_agent_with_transport) + start_coordinator (rt-host-a sweep: recompiled but never spawned).
pub(crate) fn add_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    // The seat lock lives in the seat's canonical team domain, never `default`.
    let canonical_team = crate::state::selector::resolve_active_team_readonly(
        &run_workspace,
        team,
        crate::state::selector::SelectorMode::RuntimeOnly,
    )
    .ok()
    .map(|selected| selected.team_key);
    let team = canonical_team.as_deref().or(team);
    let lifecycle_lock = acquire_seat_lifecycle_lock(LifecycleLockRequest {
        workspace: &run_workspace,
        operation: "add-agent",
        team,
        agent_id: Some(agent_id),
    })?;
    let reservation = reserve_agent_slot(&run_workspace, team, agent_id)?;
    drop(lifecycle_lock);
    add_agent_with_transport_at_paths_reserved(
        &run_workspace,
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
        Some(reservation),
        false,
        None,
        None,
    )
}


#[allow(clippy::too_many_arguments)]
pub(crate) fn add_agent_with_transport_at_paths_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
    resolved_agent: &crate::model::yaml::Value,
) -> Result<AddAgentReport, LifecycleError> {
    add_agent_with_transport_at_paths_reserved(
        run_workspace,
        team_dir,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
        None,
        true,
        None,
        Some(resolved_agent),
    )
}

/// fork-agent's new seat under the held lifecycle lock: the ordinary add-agent
/// transaction (and rollback), whose first spawn natively forks `parent`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_forked_agent_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
    parent: &crate::provider::SessionId,
) -> Result<AddAgentReport, LifecycleError> {
    add_agent_with_transport_at_paths_reserved(
        run_workspace,
        team_dir,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
        None,
        true,
        Some(parent),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_agent_with_transport_at_paths_reserved(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
    reservation: Option<AgentReservation>,
    lifecycle_lock_held: bool,
    fork_parent: Option<&crate::provider::SessionId>,
    resolved_agent: Option<&crate::model::yaml::Value>,
) -> Result<AddAgentReport, LifecycleError> {
    let runtime_state = crate::state::persist::load_runtime_state(run_workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let canonical_team_key = team
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| explicit_active_team_key(&runtime_state))
        .unwrap_or_else(|| crate::state::projection::team_state_key(&runtime_state));
    let owner_state =
        crate::state::projection::select_runtime_state(run_workspace, Some(&canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    ensure_owner_allowed_for_state(&owner_state, Some(agent_id))?;
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
    let reservation_owned = reservation.as_ref().is_some_and(|reservation| {
        owner_state
            .get("agents")
            .and_then(|agents| agents.get(agent_id.as_str()))
            .and_then(|agent| agent.get("reservation_owner"))
            .and_then(serde_json::Value::as_str)
            == Some(reservation.owner.as_str())
    });
    if runtime_agent_exists(&owner_state, agent_id) && !reservation_owned {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    // E5 Bug1:不再 copy role 文件进 <team_dir>/agents(自拷贝 O_TRUNC 截断反模式)。
    // 就地读外部 role 文档编译,注入 base team spec 的 agents/routing。role 文件留在原处。
    // Adding one seat validates that role only. Recompiling every sibling's
    // source lets an unrelated broken role veto this otherwise valid action.
    let base_spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    let mut spec = match std::fs::read_to_string(&base_spec_path) {
        Ok(text) => crate::model::yaml::loads(&text)
            .map_err(|e| LifecycleError::Compile(e.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            crate::compiler::compile_team(team_dir)
                .map_err(|e| LifecycleError::Compile(e.to_string()))?,
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    };
    override_spec_workspace(&mut spec, run_workspace);
    let workspace_s = spec
        .get("team")
        .and_then(|team| team.get("workspace"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| team_dir.to_str().unwrap_or_default())
        .to_string();
    let team_meta = crate::compiler::read_front_matter(&team_dir.join("TEAM.md"))
        .map(|(meta, _)| meta)
        .unwrap_or(Value::Null);
    let compiled = crate::compiler::compile_role_agent(role_file_path, &team_meta, &workspace_s)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    if compiled.id != agent_id.as_str() {
        return Err(LifecycleError::Compile(format!(
            "role file declares name '{}' but add-agent id is '{}'",
            compiled.id, agent_id
        )));
    }
    // The compiled spec carries a contract leaf's canonical id (`agent` → `cursor`).
    #[cfg(unix)]
    let contract_provider = compiled
        .agent
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| crate::contract_runtime::registry::descriptor(provider).is_some())
        .map(str::to_owned);
    let compiled_agent = compiled.agent.clone();
    inject_agent_into_spec(&mut spec, compiled_agent.clone(), &compiled.id)?;
    // E5 spec 迁移:重编译的 spec 原子写到 .team/runtime/<team_key>/(不落用户目录 team_dir)。
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    // E42 (0.3.24 P0): capture pre-write bytes for atomic rollback. If anything
    // downstream of write_spec_atomic + upsert_agent_state_from_role + spawn
    // fails, restore the prior bytes so the canonical spec / runtime state never
    // get a half-written row that disagrees with what remove-agent can see.
    let reservation_lock = if lifecycle_lock_held {
        None
    } else {
        Some(acquire_seat_lifecycle_lock(LifecycleLockRequest {
            workspace: run_workspace,
            operation: "add-agent",
            team,
            agent_id: Some(agent_id),
        })?)
    };
    let pre_spec_text = match std::fs::read_to_string(&spec_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(LifecycleError::StatePersist(format!("read spec: {e}"))),
    };
    // Merge against the latest runtime spec while holding only the short
    // filesystem critical section. Role compilation happened before this lock;
    // peer reservations therefore remain visible instead of being overwritten
    // by a stale base-spec snapshot.
    let mut latest_spec = pre_spec_text
        .as_deref()
        .and_then(|text| crate::model::yaml::loads(text).ok())
        .unwrap_or(spec);
    prepare_reserved_spec(&mut latest_spec, agent_id, compiled_agent, resolved_agent, run_workspace)?;
    write_spec_atomic(&spec_path, &latest_spec)?;
    let written_spec_text = crate::model::yaml::dumps(&latest_spec);
    drop(reservation_lock);
    #[cfg(unix)]
    if let Some(provider) = contract_provider {
        return add_contract_seat(
            run_workspace,
            team_dir,
            &spec_path,
            pre_spec_text.as_deref(),
            Some(&written_spec_text),
            &canonical_team_key,
            agent_id,
            &provider,
            role_file_path,
            reservation,
        );
    }
    let (meta, _) = crate::compiler::read_front_matter(role_file_path)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    // upsert writes status="starting" (E42) — start_agent_at_paths::mark_agent_started
    // promotes to "running" on Ok. If anything fails between here and the Ok
    // return below, rollback restores the captured pre-bytes.
    if let Err(error) = upsert_agent_state_from_role(
        run_workspace,
        &canonical_team_key,
        agent_id,
        &meta,
        role_file_path,
        fork_parent,
    ) {
        let skipped = rollback_add_agent_atomic(
            run_workspace,
            &spec_path,
            pre_spec_text.as_deref(),
            Some(&written_spec_text),
            None,
            Some(&canonical_team_key),
            agent_id,
            "state_upsert_failed",
        );
        return Err(with_spec_rollback_warning(error, skipped));
    }
    let started = match crate::lifecycle::restart::start_agent_at_paths(
        run_workspace,
        spec_path.parent().unwrap_or(team_dir),
        agent_id,
        false,
        open_display,
        true,
        Some(&canonical_team_key),
        transport,
    ) {
        Ok(started) => started,
        Err(error) => {
            let skipped = rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                Some(&written_spec_text),
                None,
                Some(&canonical_team_key),
                agent_id,
                "start_agent_failed",
            );
            return Err(with_spec_rollback_warning(error, skipped));
        }
    };
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { .. } => {
            let skipped = rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                Some(&written_spec_text),
                None,
                Some(&canonical_team_key),
                agent_id,
                "start_agent_noop",
            );
            return Err(with_spec_rollback_warning(LifecycleError::RequirementUnmet(format!(
                "start_agent.noop: newly added agent {agent_id} must spawn, not reuse a live window"
            )), skipped));
        }
        StartAgentOutcome::Paused { .. } => {
            let skipped = rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                Some(&written_spec_text),
                None,
                Some(&canonical_team_key),
                agent_id,
                "added_agent_paused",
            );
            return Err(with_spec_rollback_warning(LifecycleError::RequirementUnmet(format!(
                "added agent {agent_id} is paused"
            )), skipped));
        }
    };
    if let Some(reservation) = reservation {
        reservation.commit();
    }
    Ok(AddAgentReport {
        env,
        start_mode,
        role_file: role_file_path.to_path_buf(),
    })
}

/// A contract seat starts in the native framework, never the legacy transport:
/// the seeded row is what the framework's start/publish path projects onto.
/// Its role must live in `<team_dir>/agents`, where the contract config reads.
// Canonical start supplies the already-resolved row; legacy callers remain
// append-only. Validate the complete reservation before its first spec write.
pub(crate) fn prepare_reserved_spec(
    spec: &mut Value,
    agent_id: &AgentId,
    compiled: Value,
    resolved: Option<&Value>,
    workspace: &Path,
) -> Result<(), LifecycleError> {
    if let Some(resolved) = resolved {
        if spec.get("agents").and_then(Value::as_list).is_some_and(|rows|
            rows.iter().any(|row| row.get("id").and_then(Value::as_str) == Some(agent_id.as_str()))) {
            crate::lifecycle::role_config::replace_spec_agent(spec, agent_id, resolved)?;
        }
    }
    inject_agent_into_spec(spec, compiled, agent_id.as_str())?;
    if resolved.is_some() {
        crate::model::spec::validate_spec(spec, workspace)
            .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    }
    Ok(())
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn add_contract_seat(
    run_workspace: &Path,
    team_dir: &Path,
    spec_path: &Path,
    pre_spec_text: Option<&str>,
    written_spec_text: Option<&str>,
    team_key: &str,
    agent_id: &AgentId,
    provider: &str,
    role_file_path: &Path,
    reservation: Option<AgentReservation>,
) -> Result<AddAgentReport, LifecycleError> {
    let rollback = |reason: &str| {
        rollback_add_agent_atomic(
            run_workspace,
            spec_path,
            pre_spec_text,
            written_spec_text,
            None,
            Some(team_key),
            agent_id,
            reason,
        )
    };
    let seed = serde_json::json!({
        "agent_id": agent_id.as_str(), "provider": provider, "runtime_family": "contract",
        "status": "starting", "contract_instance": null, "pane_id": null,
    });
    if let Err(error) = upsert_agent_entry(run_workspace, team_key, agent_id, seed) {
        let skipped = rollback("state_upsert_failed");
        return Err(with_spec_rollback_warning(error, skipped));
    }
    let started = team_agent_contract::contract::types::SeatId::new(agent_id.as_str())
        .map_err(|error| error.to_string())
        .and_then(|seat| {
            crate::contract_runtime::framework::start_new_seat(
                run_workspace,
                team_dir,
                team_key,
                &seat,
            )
            .map_err(|error| error.to_string())
        });
    if let Err(error) = started {
        let skipped = rollback("start_agent_failed");
        return Err(with_spec_rollback_warning(LifecycleError::Provider(error), skipped));
    }
    if let Some(reservation) = reservation {
        reservation.commit();
    }
    Ok(AddAgentReport {
        env: AgentActionEnvelope {
            agent_id: agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(run_workspace),
            coordinator_started: false,
        },
        start_mode: StartMode::Fresh,
        role_file: role_file_path.to_path_buf(),
    })
}
