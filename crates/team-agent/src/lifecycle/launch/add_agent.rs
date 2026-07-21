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
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "add-agent",
        team: Some(selected.team_key.as_str()),
        agent_id: Some(agent_id),
    })?;
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

/// `add_agent` with an injected transport — after the recompile+write, wires the new worker spawn
/// (via start_agent_with_transport) + start_coordinator (rt-host-a sweep: recompiled but never spawned).
pub fn add_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &run_workspace,
        operation: "add-agent",
        team,
        agent_id: Some(agent_id),
    })?;
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

pub fn add_agent_with_transport_force(
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
    let safety = effective_runtime_config(&spec)?;
    // E5 spec 迁移:重编译的 spec 原子写到 .team/runtime/<team_key>/(不落用户目录 team_dir)。
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    // E42 (0.3.24 P0): capture pre-write bytes for atomic rollback. If anything
    // downstream of write_spec_atomic + upsert_agent_state_from_role + spawn
    // fails, restore the prior bytes so the canonical spec / runtime state never
    // get a half-written row that disagrees with what remove-agent can see.
    let pre_spec_text = match std::fs::read_to_string(&spec_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(LifecycleError::StatePersist(format!("read spec: {e}"))),
    };
    let pre_runtime_state = crate::state::persist::load_runtime_state(run_workspace).ok();
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
        &safety,
    ) {
        rollback_add_agent_atomic(
            run_workspace,
            &spec_path,
            pre_spec_text.as_deref(),
            pre_runtime_state.as_ref(),
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
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                pre_runtime_state.as_ref(),
                agent_id,
                "start_agent_failed",
            );
            return Err(error);
        }
    };
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { env, .. } => (env, StartMode::Noop),
        StartAgentOutcome::Paused { .. } => {
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                pre_runtime_state.as_ref(),
                agent_id,
                "added_agent_paused",
            );
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
