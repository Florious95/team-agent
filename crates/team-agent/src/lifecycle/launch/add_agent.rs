//! ---
//! purpose: 动态角色文档加一席，失败按快照回滚；force 变体先摘旧席再加
//! contract:
//!   provides:
//!     - name: add_agent
//!       what: 编译角色进 runtime spec 并起席，失败回滚 spec 与 state
//!     - name: add_agent_force
//!       what: 先快照并摘除同名旧席，再走正常加席，失败按快照恢复
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
//!   - start path 按 exact pane receipt 回收已 spawn pane；本文件恢复 spec/state，并删除新 Pi 席位的 exact wrapper
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

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
        let Ok(_lock) = acquire_agent_lifecycle_lock(request) else {
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

fn discover_pi_model_candidates(requested: &str) -> Result<Vec<String>, ()> {
    crate::lifecycle::launch::pi_mcp::pi_model_candidates(requested).map_err(|_| ())
}

fn preflight_pi_role_model_with(
    role_file_path: &Path,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<(), LifecycleError> {
    let (meta, _) = crate::compiler::read_front_matter(role_file_path)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    crate::compiler::preflight_pi_role_model_with(&meta, discover).map_err(|error| {
        LifecycleError::PiModelPreflight {
            requested: error.requested,
            candidates: error.candidates,
            action: error.action,
            not_ready: error.not_ready,
        }
    })
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
    let mut discover = discover_pi_model_candidates;
    add_agent_with_pi_preflight(
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        &mut discover,
    )
}

fn add_agent_with_pi_preflight(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<AddAgentReport, LifecycleError> {
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        Ok(selected) => selected,
        Err(_) if workspace.join("TEAM.md").exists() => {
            // The MCP server already passes the canonical run workspace here.
            // Reapplying team_workspace() would strip the scratch root and bind
            // the first dynamic worker to the parent workspace's tmux server.
            let transport =
                crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
                    workspace, team,
                )
                .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(workspace));
            return add_agent_with_transport_pi_preflight(
                workspace,
                agent_id,
                role_file_path,
                open_display,
                team,
                &transport,
                discover,
            );
        }
        Err(error) => return Err(LifecycleError::TeamSelect(error.to_string())),
    };
    let lifecycle_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "add-agent",
        team: Some(selected.team_key.as_str()),
        agent_id: Some(agent_id),
    })?;
    preflight_pi_role_model_with(role_file_path, discover)?;
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
    )
}

/// ---
/// purpose: 强制重建一席，先快照旧席再摘除再加回
/// params:
///   force: 为假时直接退回普通 add_agent
/// returns: 新席的环境与启动模式
/// errors: 任一步失败都按快照恢复，恢复本身再出错时错误里附 rollback_errors
/// contract_id: lifecycle.add_agent.force_entry
/// ---
/// Reconcile a single existing/inconsistent seat, then reuse the normal add
/// path. The external role source is preserved by remove-agent ownership
/// checks, so this is a one-command force-recreate rather than a team restart.
pub fn add_agent_force(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    force: bool,
) -> Result<AddAgentReport, LifecycleError> {
    let mut discover = discover_pi_model_candidates;
    add_agent_force_with_pi_preflight(
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        force,
        &mut discover,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_agent_force_with_pi_preflight(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    force: bool,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<AddAgentReport, LifecycleError> {
    if !force {
        return add_agent_with_pi_preflight(
            workspace,
            agent_id,
            role_file_path,
            open_display,
            team,
            discover,
        );
    }
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let canonical_team_key = selected.team_key.clone();
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "add-agent-force",
        team: Some(canonical_team_key.as_str()),
        agent_id: Some(agent_id),
    })?;
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(canonical_team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    force_recreate_with_transport_locked(
        &selected.run_workspace,
        &selected.team_dir,
        agent_id,
        role_file_path,
        open_display,
        Some(canonical_team_key.as_str()),
        &transport,
        discover,
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
    let mut discover = discover_pi_model_candidates;
    add_agent_with_transport_pi_preflight(
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
        &mut discover,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_agent_with_transport_pi_preflight(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<AddAgentReport, LifecycleError> {
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let lifecycle_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &run_workspace,
        operation: "add-agent",
        team,
        agent_id: Some(agent_id),
    })?;
    preflight_pi_role_model_with(role_file_path, discover)?;
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
    )
}

/// ---
/// purpose: 带注入 transport 的强制重建入口
/// params:
///   force: 为假时退回普通 add_agent_with_transport
/// returns: 新席的环境与启动模式
/// errors: 归一 workspace 失败返回 StatePersist，其余透传
/// contract_id: lifecycle.add_agent.force_entry
/// ---
pub(crate) fn add_agent_with_transport_force(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    force: bool,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let mut discover = discover_pi_model_candidates;
    add_agent_with_transport_force_pi_preflight(
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        force,
        transport,
        &mut discover,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_agent_with_transport_force_pi_preflight(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    force: bool,
    transport: &dyn Transport,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<AddAgentReport, LifecycleError> {
    if !force {
        return add_agent_with_transport_pi_preflight(
            workspace,
            agent_id,
            role_file_path,
            open_display,
            team,
            transport,
            discover,
        );
    }
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &run_workspace,
        operation: "add-agent-force",
        team,
        agent_id: Some(agent_id),
    })?;
    force_recreate_with_transport_locked(
        &run_workspace,
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
        discover,
    )
}

/// ---
/// purpose: 已持锁状态下的强制重建，先校验替换源可用再消费旧席
/// returns: 新席报告；成功后还要过快照的一致性校验
/// errors: 角色文件不存在先行返回 Compile；摘除、加回或一致性校验失败时按快照恢复并返回错误
/// ---
#[allow(clippy::too_many_arguments)]
pub(super) fn force_recreate_with_transport_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<AddAgentReport, LifecycleError> {
    // Reject an unusable replacement source before consuming the old seat.
    // Deeper compile/spawn failures remain covered by the transaction snapshot.
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
    preflight_pi_role_model_with(role_file_path, discover)?;
    let snapshot = crate::lifecycle::restart::remove::ForceRecreateSnapshot::capture(
        run_workspace,
        agent_id,
        team,
        transport,
    )?;
    let remove = crate::lifecycle::restart::remove::remove_agent_with_transport_locked(
        run_workspace,
        agent_id,
        true,
        true,
        team,
        transport,
    );
    if let Err(error) = remove {
        let restore_errors = snapshot.restore(team, transport);
        return force_recreate_rollback_error(agent_id, error, restore_errors);
    }
    let operation = add_agent_with_transport_at_paths_locked(
        run_workspace,
        team_dir,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
    )
    .and_then(|report| {
        maybe_fail_force_recreate_after_spawn()?;
        Ok(report)
    })
    .and_then(|report| {
        snapshot.require_coherent(agent_id, team, transport)?;
        Ok(report)
    });
    match operation {
        Ok(report) => Ok(report),
        Err(error) => {
            let restore_errors = snapshot.restore_after_consumption(transport);
            force_recreate_rollback_error(agent_id, error, restore_errors)
        }
    }
}

/// ---
/// purpose: 把原始错误与回滚过程中的错误合成一个对外错误
/// returns: 回滚干净时原样返回原始错误；否则包成 StatePersist 并附 rollback_errors
/// ---
pub(super) fn force_recreate_rollback_error<T>(
    agent_id: &AgentId,
    error: LifecycleError,
    restore_errors: Vec<String>,
) -> Result<T, LifecycleError> {
    if restore_errors.is_empty() {
        Err(error)
    } else {
        Err(LifecycleError::StatePersist(format!(
            "force-recreate failed for {agent_id}: {error}; rollback_errors={}",
            restore_errors.join("|")
        )))
    }
}

/// ---
/// purpose: 测试用注入点，按环境变量在 spawn 之后制造一次失败
/// returns: 环境变量未设或为空时直接成功
/// errors: 设了非空值时返回 StatePersist
/// ---
pub(super) fn maybe_fail_force_recreate_after_spawn() -> Result<(), LifecycleError> {
    let Ok(reason) = std::env::var("TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN") else {
        return Ok(());
    };
    if reason.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::StatePersist(format!(
        "injected force-recreate failure after spawn: {reason}"
    )))
}

fn rollback_added_pi_wrapper(
    run_workspace: &Path,
    team_key: &str,
    agent_id: &AgentId,
    role_meta: &Value,
) -> Result<(), LifecycleError> {
    if role_meta.get("provider").and_then(Value::as_str) != Some("pi") {
        return Ok(());
    }
    let wrapper =
        crate::lifecycle::launch::pi_mcp::pi_seat_paths(run_workspace, team_key, agent_id.as_str())
            .wrapper;
    match std::fs::remove_file(&wrapper) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LifecycleError::StatePersist(format!(
            "failed to roll back added Pi wrapper {}: {error}",
            wrapper.display()
        ))),
    }
}

/// ---
/// purpose: 加席的实体实现，含 owner 门、重名拒绝、重编译 spec 原子写、席位 state upsert 与起席
/// params:
///   run_workspace: 已归一的 run workspace
///   team_dir: 角色定义所在目录，编译 team 用它
/// returns: 新席的环境与启动模式
/// errors: owner 门不过或重名返回 RequirementUnmet，角色文件缺失或编译不一致返回 Compile，state 与 spawn 失败先回滚再透传原错
/// ---
pub(super) fn add_agent_with_transport_at_paths(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
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
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_agent_with_transport_at_paths_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
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
    let mut spec = crate::compiler::compile_team(team_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
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
        Some(acquire_agent_lifecycle_lock(LifecycleLockRequest {
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
    inject_agent_into_spec(&mut latest_spec, compiled_agent, &compiled.id)?;
    write_spec_atomic(&spec_path, &latest_spec)?;
    drop(reservation_lock);
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
    ) {
        rollback_add_agent_atomic(
            run_workspace,
            &spec_path,
            pre_spec_text.as_deref(),
            None,
            agent_id,
            "state_upsert_failed",
        );
        return Err(error);
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
            let wrapper_rollback =
                rollback_added_pi_wrapper(run_workspace, &canonical_team_key, agent_id, &meta);
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                None,
                agent_id,
                "start_agent_failed",
            );
            return match wrapper_rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(LifecycleError::StatePersist(format!(
                    "{error}; {rollback_error}"
                ))),
            };
        }
    };
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { .. } => {
            let wrapper_rollback =
                rollback_added_pi_wrapper(run_workspace, &canonical_team_key, agent_id, &meta);
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                None,
                agent_id,
                "added_agent_noop",
            );
            let error = LifecycleError::RequirementUnmet(format!(
                "newly added agent {agent_id} returned start_agent.noop"
            ));
            return match wrapper_rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(LifecycleError::StatePersist(format!(
                    "{error}; {rollback_error}"
                ))),
            };
        }
        StartAgentOutcome::Paused { .. } => {
            let wrapper_rollback =
                rollback_added_pi_wrapper(run_workspace, &canonical_team_key, agent_id, &meta);
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                None,
                agent_id,
                "added_agent_paused",
            );
            let error =
                LifecycleError::RequirementUnmet(format!("added agent {agent_id} is paused"));
            return match wrapper_rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(LifecycleError::StatePersist(format!(
                    "{error}; {rollback_error}"
                ))),
            };
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
