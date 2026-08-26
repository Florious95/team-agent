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
//!   - 回滚只恢复 spec 字节与 runtime state，不回收已 spawn 的 pane
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

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
            // **0.3.24 add-agent socket drift fix**: even on the TEAM.md fallback
            // path (no spec yet), prefer the state-aware resolver. It reads the
            // team's persisted `tmux_endpoint` (set at `team-agent launch` time)
            // and routes the new agent's spawn to the SAME tmux socket the live
            // team uses. Cold workspaces / first-agent paths safely fall back to
            // `TmuxBackend::for_workspace(team_workspace)` inside the resolver.
            let team_ws = team_workspace(workspace);
            let transport =
                crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
                    &team_ws, team,
                )
                .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&team_ws));
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
    add_agent_with_transport_at_paths(
        &selected.run_workspace,
        &team_dir,
        agent_id,
        role_file_path,
        open_display,
        Some(selected.team_key.as_str()),
        &transport,
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
    if !force {
        return add_agent(workspace, agent_id, role_file_path, open_display, team);
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
    add_agent_with_transport_at_paths(
        &run_workspace,
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
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
    if !force {
        return add_agent_with_transport(
            workspace,
            agent_id,
            role_file_path,
            open_display,
            team,
            transport,
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
    )
}

/// ---
/// purpose: 已持锁状态下的强制重建，先校验替换源可用再消费旧席
/// returns: 新席报告；成功后还要过快照的一致性校验
/// errors: 角色文件不存在先行返回 Compile；摘除、加回或一致性校验失败时按快照恢复并返回错误
/// ---
pub(super) fn force_recreate_with_transport_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    // Reject an unusable replacement source before consuming the old seat.
    // Deeper compile/spawn failures remain covered by the transaction snapshot.
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
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
    let reservation = reserve_agent_with_transport_at_paths(
        run_workspace,
        team_dir,
        agent_id,
        role_file_path,
        team,
    )?;
    start_reserved_agent(reservation, open_display, transport)
}

/// The force-recreate path already owns the lifecycle lock while it consumes
/// and restores the old seat. Keep its historical lock-held implementation
/// separate from the normal add/clone transaction.
fn add_agent_with_transport_at_paths_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let reservation =
        reserve_agent_locked(run_workspace, team_dir, agent_id, role_file_path, team)?;
    start_reserved_agent(reservation, open_display, transport)
}

struct AgentReservation {
    run_workspace: PathBuf,
    team_key: String,
    spec_path: PathBuf,
    agent_id: AgentId,
    token: String,
    role_file: PathBuf,
    session_id: Option<String>,
    backing_path: Option<PathBuf>,
}

fn reserve_agent_with_transport_at_paths(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    team: Option<&str>,
) -> Result<AgentReservation, LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: run_workspace,
        operation: "add-agent-reserve",
        team,
        agent_id: Some(agent_id),
    })?;
    reserve_agent_locked(run_workspace, team_dir, agent_id, role_file_path, team)
}

fn reserve_agent_locked(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    team: Option<&str>,
) -> Result<AgentReservation, LifecycleError> {
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
    if runtime_agent_exists(&owner_state, agent_id) {
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
    inject_agent_into_spec(&mut spec, compiled.agent, &compiled.id)?;
    // E5 spec 迁移:重编译的 spec 原子写到 .team/runtime/<team_key>/(不落用户目录 team_dir)。
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    write_spec_atomic(&spec_path, &spec)?;
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
        let _ = remove_reserved_agent(
            run_workspace,
            &spec_path,
            &canonical_team_key,
            agent_id,
            "state_upsert_failed",
            None,
        );
        return Err(error);
    }
    let token = reservation_token(agent_id);
    let mut state =
        crate::state::projection::select_runtime_state(run_workspace, Some(&canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(LifecycleError::StatePersist(format!(
            "reservation row missing for agent {agent_id}"
        )));
    };
    agent.insert(
        "_lifecycle_reservation_token".to_string(),
        serde_json::Value::String(token.clone()),
    );
    if let Err(error) = save_launched_team_state_for_key(
        run_workspace,
        &state,
        Some(&canonical_team_key),
        Some(agent_id.as_str()),
    ) {
        return Err(error);
    }
    if let Err(error) = write_reservation_registry(run_workspace, agent_id, &token) {
        let _ = remove_reserved_agent(
            run_workspace,
            &spec_path,
            &canonical_team_key,
            agent_id,
            "reservation_registry_failed",
            None,
        );
        return Err(error);
    }
    Ok(AgentReservation {
        run_workspace: run_workspace.to_path_buf(),
        team_key: canonical_team_key,
        spec_path,
        agent_id: agent_id.clone(),
        token,
        role_file: role_file_path.to_path_buf(),
        session_id: None,
        backing_path: None,
    })
}

fn reservation_token(agent_id: &AgentId) -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        agent_id.as_str()
    )
}

fn start_reserved_agent(
    mut reservation: AgentReservation,
    open_display: bool,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let mut attempts = 0;
    let started = loop {
        match crate::lifecycle::restart::start_agent_at_paths(
            &reservation.run_workspace,
            reservation
                .spec_path
                .parent()
                .unwrap_or_else(|| Path::new(".")),
            &reservation.agent_id,
            false,
            open_display,
            true,
            Some(&reservation.team_key),
            transport,
        ) {
            Ok(started) => break started,
            Err(error) if is_state_save_conflict(&error) && attempts < 20 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => {
                let _ = finalize_agent_reservation(&reservation, true);
                return Err(error);
            }
        }
    };
    let identity = match &started {
        StartAgentOutcome::Running {
            new_session_id,
            rollout_path,
            ..
        } => new_session_id.as_ref().map(|session| {
            (
                session.as_str().to_string(),
                rollout_path
                    .as_ref()
                    .map(|path| path.as_path().to_path_buf()),
            )
        }),
        StartAgentOutcome::Noop { .. } => {
            let session = expected_session_from_spawn_event(
                &reservation.run_workspace,
                &reservation.agent_id,
            )
            .unwrap_or_else(|| reservation.token.clone());
            let backing = find_claude_backing(&reservation.run_workspace, &session);
            Some((session, backing))
        }
        StartAgentOutcome::Paused { .. } => None,
    };
    let (session_id, backing_path) = identity.unwrap_or_else(|| {
        let session = reservation.token.clone();
        let backing = find_claude_backing(&reservation.run_workspace, &session);
        (session, backing)
    });
    let backing_path = backing_path.or_else(|| {
        let path = reservation
            .run_workspace
            .join(".team/runtime/clone-backings")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, b"{\"type\":\"clone-backing\"}\n").ok()?;
        Some(path)
    });
    reservation.session_id = Some(session_id.clone());
    reservation.backing_path = backing_path.clone();
    persist_spawn_identity(&reservation, &session_id, backing_path.as_deref())?;
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { env, .. } => (env, StartMode::Noop),
        StartAgentOutcome::Paused { .. } => {
            let _ = finalize_agent_reservation(&reservation, true);
            return Err(LifecycleError::RequirementUnmet(format!(
                "added agent {} is paused",
                reservation.agent_id
            )));
        }
    };
    finalize_agent_reservation(&reservation, false)?;
    Ok(AddAgentReport {
        env,
        start_mode,
        role_file: reservation.role_file,
    })
}

fn expected_session_from_spawn_event(workspace: &Path, agent_id: &AgentId) -> Option<String> {
    let path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .rev()
        .find_map(|line| {
            let event = serde_json::from_str::<serde_json::Value>(line).ok()?;
            (event.get("agent_id").and_then(serde_json::Value::as_str) == Some(agent_id.as_str()))
                .then(|| {
                    event
                        .get("expected_session_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|session| !session.is_empty())
                        .map(str::to_string)
                })
                .flatten()
        })
}

fn find_claude_backing(workspace: &Path, session_id: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".claude").join("projects"))
    {
        let mut pending = vec![root.clone()];
        while let Some(path) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in entries.flatten() {
                let child = entry.path();
                if child.file_name().and_then(|name| name.to_str())
                    == Some(&format!("{session_id}.jsonl"))
                {
                    return child.is_file().then_some(child);
                }
                if child.is_dir() {
                    pending.push(child);
                }
            }
        }
        let slug = workspace
            .to_string_lossy()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect::<String>();
        let candidate = root.join(slug).join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let fallback = workspace
        .join(".team")
        .join("runtime")
        .join("clone-backings")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(fallback.parent()?).ok()?;
    std::fs::write(&fallback, b"{\"type\":\"clone-backing\"}\n").ok()?;
    Some(fallback)
}

fn persist_spawn_identity(
    reservation: &AgentReservation,
    session_id: &str,
    backing_path: Option<&Path>,
) -> Result<(), LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &reservation.run_workspace,
        operation: "add-agent-identity",
        team: Some(&reservation.team_key),
        agent_id: Some(&reservation.agent_id),
    })?;
    if read_reservation_registry(&reservation.run_workspace, &reservation.agent_id)?.as_deref()
        != Some(reservation.token.as_str())
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "reservation owner mismatch for agent {}",
            reservation.agent_id
        )));
    }
    let mut state = crate::state::projection::select_runtime_state(
        &reservation.run_workspace,
        Some(&reservation.team_key),
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(reservation.agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(LifecycleError::StatePersist(format!(
            "reservation row missing for agent {}",
            reservation.agent_id
        )));
    };
    agent.insert("session_id".to_string(), serde_json::json!(session_id));
    if let Some(path) = backing_path {
        agent.insert(
            "rollout_path".to_string(),
            serde_json::json!(path.to_string_lossy().to_string()),
        );
    }
    write_identity_registry(
        &reservation.run_workspace,
        &reservation.agent_id,
        session_id,
        backing_path,
    )?;
    agent.insert(
        "captured_via".to_string(),
        serde_json::json!("clone-reservation"),
    );
    save_launched_team_state_for_key(
        &reservation.run_workspace,
        &state,
        Some(&reservation.team_key),
        Some(reservation.agent_id.as_str()),
    )
}

fn is_state_save_conflict(error: &LifecycleError) -> bool {
    matches!(error, LifecycleError::StatePersist(message) if message.contains("state save conflict"))
}

fn finalize_agent_reservation(
    reservation: &AgentReservation,
    rollback: bool,
) -> Result<(), LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &reservation.run_workspace,
        operation: if rollback {
            "add-agent-rollback"
        } else {
            "add-agent-commit"
        },
        team: Some(&reservation.team_key),
        agent_id: Some(&reservation.agent_id),
    })?;
    if rollback {
        remove_reserved_agent(
            &reservation.run_workspace,
            &reservation.spec_path,
            &reservation.team_key,
            &reservation.agent_id,
            "start_agent_failed",
            Some(&reservation.token),
        )
    } else {
        let mut state = crate::state::projection::select_runtime_state(
            &reservation.run_workspace,
            Some(&reservation.team_key),
        )
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
        let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(reservation.agent_id.as_str()))
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Err(LifecycleError::StatePersist(format!(
                "reservation row missing at commit for agent {}",
                reservation.agent_id
            )));
        };
        if read_reservation_registry(&reservation.run_workspace, &reservation.agent_id)?.as_deref()
            != Some(reservation.token.as_str())
        {
            return Err(LifecycleError::RequirementUnmet(format!(
                "reservation owner mismatch for agent {}",
                reservation.agent_id
            )));
        }
        if agent.get("_lifecycle_reservation_token")
            == Some(&serde_json::Value::String(reservation.token.clone()))
        {
            agent.remove("_lifecycle_reservation_token");
        }
        if let Some(session_id) = reservation.session_id.as_deref() {
            agent.insert("session_id".to_string(), serde_json::json!(session_id));
        }
        if let Some(backing_path) = reservation.backing_path.as_ref() {
            agent.insert(
                "rollout_path".to_string(),
                serde_json::json!(backing_path.to_string_lossy().to_string()),
            );
        }
        apply_identity_registry(&mut state, &reservation.run_workspace);
        save_launched_team_state_for_key(
            &reservation.run_workspace,
            &state,
            Some(&reservation.team_key),
            Some(reservation.agent_id.as_str()),
        )?;
        clear_reservation_registry(&reservation.run_workspace, &reservation.agent_id)
    }
}

fn remove_reserved_agent(
    run_workspace: &Path,
    spec_path: &Path,
    team_key: &str,
    agent_id: &AgentId,
    reason: &str,
    expected_token: Option<&str>,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::projection::select_runtime_state(run_workspace, Some(team_key))
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let reserved = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get(agent_id.as_str()));
    let owned = match expected_token {
        None => reserved.is_some(),
        Some(expected) => reserved
            .and_then(|agent| agent.get("_lifecycle_reservation_token"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|token| token == expected),
    };
    if !owned {
        return Err(LifecycleError::RequirementUnmet(format!(
            "reservation owner missing for agent {agent_id}"
        )));
    }
    if let Some(agents) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
    {
        agents.remove(agent_id.as_str());
    }
    remove_agent_from_spec(spec_path, agent_id)?;
    save_launched_team_state_for_key(run_workspace, &state, Some(team_key), None)?;
    if expected_token.is_some() {
        clear_reservation_registry(run_workspace, agent_id)?;
    }
    let _ = crate::event_log::EventLog::new(run_workspace).write(
        "add_agent.rollback",
        serde_json::json!({"agent_id": agent_id.as_str(), "reason": reason, "owner_scoped": true}),
    );
    Ok(())
}

fn reservation_registry_path(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join("agent-reservations.json")
}

fn identity_registry_path(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join("agent-identities.json")
}

fn write_identity_registry(
    workspace: &Path,
    agent_id: &AgentId,
    session_id: &str,
    backing_path: Option<&Path>,
) -> Result<(), LifecycleError> {
    let path = identity_registry_path(workspace);
    let mut registry = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
            .map_err(|error| {
                LifecycleError::StatePersist(format!("read identity registry: {error}"))
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    };
    registry.insert(
        agent_id.as_str().to_string(),
        serde_json::json!({
            "session_id": session_id,
            "backing_path": backing_path.map(|path| path.to_string_lossy().to_string()),
        }),
    );
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&registry).map_err(|error| {
            LifecycleError::StatePersist(format!("encode identity registry: {error}"))
        })?,
    )
    .map_err(|error| LifecycleError::StatePersist(format!("write identity registry: {error}")))
}

fn apply_identity_registry(state: &mut serde_json::Value, workspace: &Path) {
    let Ok(text) = std::fs::read_to_string(identity_registry_path(workspace)) else {
        return;
    };
    let Ok(registry) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
    else {
        return;
    };
    let Some(agents) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for (agent_id, identity) in registry {
        let Some(agent) = agents
            .get_mut(&agent_id)
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        if let Some(session_id) = identity.get("session_id") {
            agent.insert("session_id".to_string(), session_id.clone());
        }
        if let Some(backing_path) = identity.get("backing_path").filter(|path| !path.is_null()) {
            agent.insert("rollout_path".to_string(), backing_path.clone());
        }
    }
}

fn read_reservation_registry(
    workspace: &Path,
    agent_id: &AgentId,
) -> Result<Option<String>, LifecycleError> {
    let path = reservation_registry_path(workspace);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    };
    let registry: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text)
        .map_err(|error| {
            LifecycleError::StatePersist(format!("read reservation registry: {error}"))
        })?;
    Ok(registry
        .get(agent_id.as_str())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

fn write_reservation_registry(
    workspace: &Path,
    agent_id: &AgentId,
    token: &str,
) -> Result<(), LifecycleError> {
    let path = reservation_registry_path(workspace);
    let mut registry = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
            .map_err(|error| {
                LifecycleError::StatePersist(format!("read reservation registry: {error}"))
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    };
    registry.insert(
        agent_id.as_str().to_string(),
        serde_json::Value::String(token.to_string()),
    );
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&registry).map_err(|error| {
            LifecycleError::StatePersist(format!("encode reservation registry: {error}"))
        })?,
    )
    .map_err(|error| LifecycleError::StatePersist(format!("write reservation registry: {error}")))
}

fn clear_reservation_registry(workspace: &Path, agent_id: &AgentId) -> Result<(), LifecycleError> {
    let path = reservation_registry_path(workspace);
    let mut registry = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
            .map_err(|error| {
                LifecycleError::StatePersist(format!("read reservation registry: {error}"))
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    };
    registry.remove(agent_id.as_str());
    if registry.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&registry).map_err(|error| {
                LifecycleError::StatePersist(format!("encode reservation registry: {error}"))
            })?,
        )
        .map_err(|error| {
            LifecycleError::StatePersist(format!("write reservation registry: {error}"))
        })?;
    }
    Ok(())
}

fn remove_agent_from_spec(spec_path: &Path, agent_id: &AgentId) -> Result<(), LifecycleError> {
    let text = std::fs::read_to_string(spec_path)
        .map_err(|e| LifecycleError::StatePersist(format!("read spec for rollback: {e}")))?;
    let mut spec = crate::model::yaml::loads(&text)
        .map_err(|e| LifecycleError::StatePersist(format!("parse spec for rollback: {e}")))?;
    if let Value::Map(pairs) = &mut spec {
        if let Some((_, Value::List(agents))) = pairs.iter_mut().find(|(key, _)| key == "agents") {
            agents.retain(|agent| yaml_agent_id(agent) != Some(agent_id.as_str()));
        }
        if let Some((_, Value::Map(routing))) = pairs.iter_mut().find(|(key, _)| key == "routing") {
            if let Some((_, Value::List(rules))) =
                routing.iter_mut().find(|(key, _)| key == "rules")
            {
                rules.retain(|rule| yaml_route_assigns_to(rule) != Some(agent_id.as_str()));
            }
        }
    }
    write_spec_atomic(spec_path, &spec)
}
