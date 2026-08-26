//! ---
//! purpose: 动态角色文档以 canonical owner reservation 加一席；force 变体先摘旧席再加
//! contract:
//!   provides:
//!     - name: add_agent
//!       what: 短锁预留 canonical row，锁外起席，再短锁提交或按 owner 回滚
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
//!     - crate::state::repository
//!     - crate::tmux_backend
//! boundary:
//!   - 不拷贝外部角色文件进 team 目录，就地读取编译
//!   - canonical agents 行上的单一 owner token 是唯一 reservation 真相
//!   - compile 在锁外；reserve/finalize/rollback 各自只持短 lifecycle lock
//!   - 起席一律走 restart 的 start_reserved_agent_at_paths，本文件不直接 spawn
//!   - 回滚只移除 token owner 的 row/spec delta，并复用既有 lifecycle tombstone 挡住 stale resurrection
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
/// purpose: 加一席的默认入口，解析活跃 team 并路由到该 team 实际使用的 tmux socket
/// params:
///   role_file_path: 外部角色文档路径，就地读取不拷贝
///   open_display: 是否为新席开显示
/// returns: 新席的环境与启动模式
/// errors: 选不到 team 返回 TeamSelect，角色文件缺失或编译失败返回 Compile，重名返回 RequirementUnmet
/// contract_id: lifecycle.add_agent.entry
/// ---
/// `add_agent(workspace, agent_id, role_file_path, open_display, team)`
/// (`lifecycle/operations.py:143`)。动态 role doc 在锁外编译，短锁写 canonical
/// reservation，锁外起 worker，再短锁 finalize 或按 owner 精确回滚。
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
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(canonical_team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    force_recreate_with_transport(
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
    force_recreate_with_transport(
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
/// purpose: 强制重建，短锁消费旧席后释放，再走 canonical reservation 加席
/// returns: 新席报告；成功后还要过快照的一致性校验
/// errors: 角色文件不存在先行返回 Compile；摘除、加回或一致性校验失败时按快照恢复并返回错误
/// ---
pub(super) fn force_recreate_with_transport(
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
    let (snapshot, remove) = {
        let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
            workspace: run_workspace,
            operation: "add-agent-force-remove",
            team,
            agent_id: Some(agent_id),
        })?;
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
        (snapshot, remove)
    };
    if let Err(error) = remove {
        let restore_errors = snapshot.restore(team, transport);
        return force_recreate_rollback_error(agent_id, error, restore_errors);
    }
    let operation = add_agent_with_transport_at_paths(
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
    let runtime_state = crate::state::persist::load_runtime_state(run_workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let canonical_team_key = team
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| explicit_active_team_key(&runtime_state))
        .unwrap_or_else(|| crate::state::projection::team_state_key(&runtime_state));
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
    // Compile and materialize the role outside the workspace lifecycle lock.
    // The exact source bytes are checked again while reserving so a concurrent
    // role rewrite cannot commit a stale compilation.
    let mut base_spec = crate::compiler::compile_team(team_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    override_spec_workspace(&mut base_spec, run_workspace);
    let workspace_s = base_spec
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
    let role_bytes = std::fs::read(role_file_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", role_file_path.display())))?;
    let (meta, _) = crate::compiler::read_front_matter(role_file_path)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    let reservation_token = new_reservation_token(&canonical_team_key, agent_id);

    let reserve_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: run_workspace,
        operation: "add-agent-reserve",
        team: Some(&canonical_team_key),
        agent_id: Some(agent_id),
    })?;
    let owner_state =
        crate::state::projection::select_runtime_state(run_workspace, Some(&canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    ensure_owner_allowed_for_state(&owner_state, Some(agent_id))?;
    if runtime_agent_exists(&owner_state, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    let current_role_bytes = std::fs::read(role_file_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", role_file_path.display())))?;
    if current_role_bytes != role_bytes {
        return Err(LifecycleError::Compile(format!(
            "role file changed while preparing add-agent: {}",
            role_file_path.display()
        )));
    }
    let mut spec = match std::fs::read_to_string(&spec_path) {
        Ok(text) => yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => base_spec,
        Err(error) => return Err(LifecycleError::StatePersist(format!("read spec: {error}"))),
    };
    let reservation_added_spec = !spec_agent_exists(&spec, agent_id.as_str());
    inject_agent_into_spec(&mut spec, compiled.agent, &compiled.id)?;
    write_spec_atomic(&spec_path, &spec)?;
    if let Err(error) = upsert_agent_state_from_role(
        run_workspace,
        &canonical_team_key,
        agent_id,
        &meta,
        role_file_path,
        &reservation_token,
    ) {
        let _ = rollback_reserved_agent_locked(
            run_workspace,
            &spec_path,
            &canonical_team_key,
            agent_id,
            &reservation_token,
            true,
            reservation_added_spec,
            "state_upsert_failed",
        );
        return Err(error);
    }
    drop(reserve_lock);

    if let Err(error) = after_reserve_test_gate(agent_id) {
        rollback_reserved_agent(
            run_workspace,
            &spec_path,
            &canonical_team_key,
            agent_id,
            &reservation_token,
            false,
            reservation_added_spec,
            "after_reserve_test_gate",
        )?;
        return Err(error);
    }

    let started = match crate::lifecycle::restart::start_reserved_agent_at_paths(
        run_workspace,
        spec_path.parent().unwrap_or(team_dir),
        agent_id,
        false,
        open_display,
        true,
        Some(&canonical_team_key),
        transport,
        &reservation_token,
    ) {
        Ok(started) => started,
        Err(error) => {
            let rollback = rollback_reserved_agent(
                run_workspace,
                &spec_path,
                &canonical_team_key,
                agent_id,
                &reservation_token,
                false,
                reservation_added_spec,
                "start_agent_failed",
            );
            if let Err(rollback_error) = rollback {
                return Err(LifecycleError::StatePersist(format!(
                    "{error}; owner rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
    };
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { env, .. } => (env, StartMode::Noop),
        StartAgentOutcome::Paused { .. } => {
            rollback_reserved_agent(
                run_workspace,
                &spec_path,
                &canonical_team_key,
                agent_id,
                &reservation_token,
                false,
                reservation_added_spec,
                "added_agent_paused",
            )?;
            return Err(LifecycleError::RequirementUnmet(format!(
                "added agent {agent_id} is paused"
            )));
        }
    };
    Ok(AddAgentReport {
        env,
        start_mode,
        role_file: role_file_path.to_path_buf(),
    })
}

static RESERVATION_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn new_reservation_token(team_key: &str, agent_id: &AgentId) -> String {
    let sequence = RESERVATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{}:{}:{}:{nanos}:{sequence}",
        team_key,
        agent_id.as_str(),
        std::process::id()
    )
}

fn after_reserve_test_gate(agent_id: &AgentId) -> Result<(), LifecycleError> {
    if std::env::var("TEAM_AGENT_TEST_FAIL_AFTER_RESERVE")
        .ok()
        .is_some_and(|target| target == agent_id.as_str())
    {
        return Err(LifecycleError::StatePersist(format!(
            "injected failure after reserve for {agent_id}"
        )));
    }
    if std::env::var("TEAM_AGENT_TEST_PAUSE_AFTER_RESERVE_AGENT")
        .ok()
        .is_none_or(|target| target != agent_id.as_str())
    {
        return Ok(());
    }
    let ready = std::env::var_os("TEAM_AGENT_TEST_RESERVATION_READY_FILE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            LifecycleError::StatePersist("reservation ready file not configured".to_string())
        })?;
    let proceed = std::env::var_os("TEAM_AGENT_TEST_RESERVATION_CONTINUE_FILE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            LifecycleError::StatePersist("reservation continue file not configured".to_string())
        })?;
    std::fs::write(&ready, agent_id.as_str())
        .map_err(|e| LifecycleError::StatePersist(format!("write reserve test gate: {e}")))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !proceed.exists() {
        if std::time::Instant::now() >= deadline {
            return Err(LifecycleError::StatePersist(format!(
                "reservation test gate timed out for {agent_id}"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

fn rollback_reserved_agent(
    run_workspace: &Path,
    spec_path: &Path,
    team_key: &str,
    agent_id: &AgentId,
    reservation_token: &str,
    remove_spec_if_unowned: bool,
    reservation_added_spec: bool,
    reason: &str,
) -> Result<(), LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: run_workspace,
        operation: "add-agent-rollback",
        team: Some(team_key),
        agent_id: Some(agent_id),
    })?;
    rollback_reserved_agent_locked(
        run_workspace,
        spec_path,
        team_key,
        agent_id,
        reservation_token,
        remove_spec_if_unowned,
        reservation_added_spec,
        reason,
    )
}

fn rollback_reserved_agent_locked(
    run_workspace: &Path,
    spec_path: &Path,
    team_key: &str,
    agent_id: &AgentId,
    reservation_token: &str,
    remove_spec_if_unowned: bool,
    reservation_added_spec: bool,
    reason: &str,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::projection::select_runtime_state(run_workspace, Some(team_key))
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let row = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()));
    let owned = if let Some(row) = row {
        let owner = row
            .get(LIFECYCLE_RESERVATION_TOKEN)
            .and_then(serde_json::Value::as_str);
        if owner != Some(reservation_token) {
            return Err(LifecycleError::RequirementUnmet(format!(
                "reservation owner mismatch for {agent_id}"
            )));
        }
        if let Some(agents) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
        {
            agents.remove(agent_id.as_str());
        }
        crate::lifecycle::restart::remove::mark_agent_retired_in_state(&mut state, agent_id)?;
        if let Some(entry) = state
            .get_mut("agent_lifecycle")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|entries| entries.get_mut(agent_id.as_str()))
            .and_then(serde_json::Value::as_object_mut)
        {
            entry.insert(
                "reason".to_string(),
                serde_json::json!("reservation-rollback"),
            );
        }
        crate::state::repository::StateRepository::new(run_workspace)
            .save(
                crate::state::repository::StateWriteIntent::AgentRollback {
                    team_key: Some(team_key),
                    agent_id: agent_id.as_str(),
                },
                &state,
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        true
    } else {
        false
    };
    if (owned || remove_spec_if_unowned) && reservation_added_spec && spec_path.exists() {
        let text = std::fs::read_to_string(spec_path)
            .map_err(|e| LifecycleError::StatePersist(format!("read spec: {e}")))?;
        let mut spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
        remove_agent_from_spec(&mut spec, agent_id.as_str());
        write_spec_atomic(spec_path, &spec)?;
    }
    if owned {
        let instructions = run_workspace
            .join(".team/runtime/copilot-instructions")
            .join(agent_id.as_str());
        match std::fs::remove_dir_all(&instructions) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LifecycleError::StatePersist(format!(
                    "remove reserved instructions: {error}"
                )))
            }
        }
    }
    let _ = crate::event_log::EventLog::new(run_workspace).write(
        "add_agent.rollback",
        serde_json::json!({
            "agent_id": agent_id.as_str(),
            "reason": reason,
            "owner_scoped": true,
        }),
    );
    Ok(())
}

fn spec_agent_exists(spec: &Value, agent_id: &str) -> bool {
    let Value::Map(pairs) = spec else {
        return false;
    };
    pairs
        .iter()
        .find(|(key, _)| key == "agents")
        .and_then(|(_, agents)| agents.as_list())
        .is_some_and(|agents| {
            agents
                .iter()
                .any(|agent| yaml_agent_id(agent) == Some(agent_id))
        })
}
