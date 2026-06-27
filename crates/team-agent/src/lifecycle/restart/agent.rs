use super::*;
use super::common::*;
use super::selection::decide_start_mode;
use super::team_state::write_team_state;

/// `start_agent(workspace, agent_id, force, open_display, allow_fresh, team)`
/// (`lifecycle/start.py:72`)。`_runtime_lock("start-agent")` 下串行:resume-or-fresh
/// 决策、resume 窗口退出回退 fresh、起后投递 pending message、起 coordinator。
/// bug-085:`(session_id, rollout_path)` 四象限穷尽 match,缺 rollout 的 codex 仅在
/// allow_fresh 时回退 fresh。
pub fn start_agent(
    workspace: &Path,
    agent_id: &AgentId,
    force: bool,
    open_display: bool,
    allow_fresh: bool,
    team: Option<&str>,
) -> Result<StartAgentOutcome, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let transport =
        lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    start_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        force,
        open_display,
        allow_fresh,
        team,
        &transport,
    )
}

/// `start_agent` with an injected transport — wires the single-worker resume/fresh spawn +
/// start_coordinator (rt-host-a sweep: was a stub returning RequirementUnmet at the spawn boundary).
#[allow(clippy::too_many_arguments)]
pub fn start_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    force: bool,
    open_display: bool,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<StartAgentOutcome, LifecycleError> {
    start_agent_at_paths(
        workspace,
        workspace,
        agent_id,
        force,
        open_display,
        allow_fresh,
        team,
        transport,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_agent_at_paths(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    force: bool,
    open_display: bool,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<StartAgentOutcome, LifecycleError> {
    let _ = open_display;
    let mut state = if team.is_some() {
        resolve_team_scoped_state_or_refuse(workspace, team)?
    } else {
        crate::state::persist::load_runtime_state(workspace)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?
    };
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, Some(agent_id))?;
    let agent = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .ok_or_else(|| LifecycleError::RequirementUnmet(format!("agent {agent_id} not found")))?
        .clone();
    if agent
        .get("paused")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(StartAgentOutcome::Paused { agent_id: agent_id.clone() });
    }
    let session_name = state_session_name(&state);
    let window = agent_window(&agent, agent_id);
    let adaptive_layout =
        open_display && crate::lifecycle::launch::state_uses_adaptive_layout(&state);
    let agent_live = if adaptive_layout {
        agent_pane_live(transport, &agent)
    } else {
        window_exists(transport, &session_name, &window)
    };
    // 0.3.28 Step 9: E51 self-heal converted to topology assertion. After
    // Step 2 the leader lives in its own session (`team-agent-leader-*`),
    // so `agent.pane_id == leader_receiver.pane_id` is STRUCTURALLY
    // impossible. We keep the check as a runtime guard: if it ever fires,
    // emit a topology_invariant_violation event AND still force a fresh
    // spawn (defensive). The check itself remains a no-op on healthy
    // state — assert_topology_invariants from Step 1 catches the
    // upstream corruption.
    let has_collision = pane_conflicts_with_leader_or_other(&state, agent_id, &agent);
    if has_collision {
        eprintln!(
            "team_agent::layout e51_collision_post_step2 agent_id=`{agent_id}` \
             action=forcing_fresh_spawn \
             (should be impossible after Step 2 leader/worker session separation; \
              investigate upstream state corruption)"
        );
    }
    let agent_live = agent_live && !has_collision;
    if !force && agent_live {
        mark_agent_running_noop(&mut state, agent_id, &session_name, &window)?;
        let team_key = restart_projection_team_key(&state, team);
        save_restart_projected_state(workspace, &mut state, &team_key)?;
        if let Ok(spec) = load_team_spec(spec_workspace) {
            write_team_state(spec_workspace, &spec, &state)?;
        }
        let coordinator_started = start_coordinator_for_workspace(workspace)?;
        let target = format!("{}:{window}", session_name.as_str());
        write_start_agent_noop_event(workspace, agent_id, &target, coordinator_started)?;
        return Ok(StartAgentOutcome::Noop {
            env: AgentActionEnvelope {
                agent_id: agent_id.clone(),
                state_file: crate::state::persist::runtime_state_path(workspace),
                coordinator_started,
            },
            target,
        });
    }
    let provider = agent_provider(&agent);
    let session_id = agent_session_id(&agent);
    let rollout_path = agent_rollout_path(&agent);
    let resume_backing_exists = session_id
        .as_ref()
        .map(|session| {
            resume_backing_exists_for_agent(
                workspace,
                agent_id,
                &agent,
                provider,
                session,
                rollout_path.as_ref(),
            )
        })
        .unwrap_or(false);
    let start_mode = decide_start_mode(
        provider_wire(provider),
        session_id.as_ref(),
        rollout_path.as_ref(),
        resume_backing_exists,
        allow_fresh,
    );
    if matches!(start_mode, StartMode::Noop) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "resume_not_ready: session backing store missing for agent {agent_id}; rerun with --allow-fresh to start fresh"
        )));
    }
    let spawn_session_id = if matches!(start_mode, StartMode::Resumed) {
        session_id.as_ref()
    } else {
        None
    };
    let into_existing_session =
        session_live_or_default(transport, &session_name, session_name_present(&state));
    let safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn()?;
    let layout_placement = if adaptive_layout {
        crate::lifecycle::launch::adaptive_existing_placement_for_agent(
            &state,
            transport,
            &session_name,
            agent_id,
        )
        .or_else(|| {
            crate::lifecycle::launch::adaptive_placement_for_agent(
                &state,
                transport,
                &session_name,
                agent_id,
            )
        })
    } else {
        None
    };
    let spawn_window = layout_placement
        .as_ref()
        .map(|placement| placement.layout_window.as_str().to_string())
        .unwrap_or_else(|| window.clone());
    // Issue 2 (Round 3b gate review §6): pass the explicit team_key so the
    // worker MCP env carries it through restart-agent path too. The
    // `restart_projection_team_key` helper consolidates the same resolution
    // used for save_restart_projected_state below.
    let resolved_team_key = restart_projection_team_key(&state, team);
    let spawn = spawn_agent_window(
        workspace,
        &session_name,
        agent_id,
        &agent,
        spawn_session_id,
        into_existing_session,
        transport,
        Some(&safety),
        layout_placement.as_ref(),
        None,
        Some(resolved_team_key.as_str()),
    )?;
    mark_agent_started(&mut state, agent_id, &spawn_window, &spawn, transport, &safety, start_mode)?;
    // **0.3.24 add-agent socket drift fix**: keep `state.tmux_endpoint` /
    // `state.tmux_socket` synchronized with the transport actually used for the
    // spawn. Without this, add-agent / fork-agent could spawn to a socket that
    // never gets persisted, and the next coordinator tick would re-resolve to
    // the workspace-hash socket and lose the new pane. Annotation runs inside
    // the same `save_restart_projected_state` window — no parallel "annotate
    // after spawn" race with coordinator and no double source of truth.
    crate::lifecycle::launch::annotate_runtime_tmux_endpoint(&mut state, transport, workspace);
    let team_key = restart_projection_team_key(&state, team);
    save_restart_projected_state(workspace, &mut state, &team_key)?;
    // **0.3.24 reachability gate (调整 4)** — strict, non-capture verification
    // that the spawn actually produced an addressable window/pane on the
    // transport's socket. Catches the macmini假绿 (`ok=true` returned by
    // `add_agent` while the spawned `claude` process orphaned on a different
    // socket and the leader never registered it). We use `has_pane` /
    // `liveness` / `list_targets` (structural addressing only) — never
    // `capture` — to avoid contention with E31's pane-readback gate timing.
    if !spawned_pane_is_reachable(transport, &spawn.spawn.pane_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "add-agent spawn unreachable: pane {} not addressable on transport \
             socket {:?} (likely socket drift — see 0.3.24 fix); the agent \
             process may have orphaned on a different tmux socket. Re-run after \
             confirming the team's tmux_endpoint persisted via `team-agent status`.",
            spawn.spawn.pane_id.as_str(),
            transport.tmux_endpoint().unwrap_or_default()
        )));
    }
    write_start_agent_start_event(
        workspace,
        agent_id,
        &agent,
        provider,
        start_mode,
        &session_name,
        &spawn_window,
        spawn_session_id,
        tmux_start_mode_for_spawn(&spawn, into_existing_session),
    )?;
    let coordinator_started = start_coordinator_for_workspace(workspace)?;
    Ok(StartAgentOutcome::Running {
        env: AgentActionEnvelope {
            agent_id: agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(workspace),
            coordinator_started,
        },
        start_mode,
        target: spawn.spawn.pane_id.as_str().to_string(),
        session_id,
        new_session_id: spawn.plan.expected_session_id.clone(),
        rollout_path,
    })
}

/// **0.3.24 strict-non-capture reachability gate**. Returns `true` IFF the spawn's
/// pane is currently addressable on the transport's tmux socket. Uses structural
/// addressing only (`has_pane` → `liveness` → `list_targets`) — **never**
/// `capture` — so it does not race with E31's pane-readback gate timing
/// (E31 reads pane contents after submit; running our own capture here would
/// either consume the first-paste tokens E31 looks for, or vice versa).
///
/// The gate fires after `mark_agent_started` + `save_restart_projected_state`,
/// so an unreachable pane returns a structured `LifecycleError` that the CLI
/// surface translates to `ok=false`, replacing the macmini假绿
/// (`ok=true` with orphaned-on-wrong-socket spawn).
fn spawned_pane_is_reachable(
    transport: &dyn crate::transport::Transport,
    pane: &crate::transport::PaneId,
) -> bool {
    // (a) authoritative: has_pane Some(true) → addressable.
    if matches!(transport.has_pane(pane), Ok(Some(true))) {
        return true;
    }
    // (b) liveness probe: not-Dead → addressable. Dead → unreachable.
    if matches!(
        transport.liveness(pane),
        Ok(crate::model::enums::PaneLiveness::Live)
    ) {
        return true;
    }
    // (c) last resort: enumeration. If the pane appears in `list_targets()`
    // (cross-session enumeration on the socket), it is addressable.
    if let Ok(targets) = transport.list_targets() {
        if targets.iter().any(|t| t.pane_id.as_str() == pane.as_str()) {
            return true;
        }
    }
    false
}

/// E51 (0.3.26 P0, restart self-heal): returns `true` when the agent's pane_id
/// is the same as the leader_receiver/team_owner pane_id OR is owned by a
/// different agent in the state. In both cases `start_agent` must NOT treat the
/// pane as "this agent's live pane" (it should spawn fresh).
fn pane_conflicts_with_leader_or_other(
    state: &serde_json::Value,
    agent_id: &crate::model::ids::AgentId,
    agent: &serde_json::Value,
) -> bool {
    let Some(pane_id) = agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let state_socket = runtime_tmux_socket(state);
    let agent_socket = tmux_socket_field(agent).or(state_socket);
    // Check leader anchor.
    for key in ["leader_receiver", "team_owner"] {
        if state.get(key).and_then(pane_socket_binding).is_some_and(|leader| {
            pane_conflicts_on_same_socket(pane_id, agent_socket, leader)
        }) {
            return true;
        }
    }
    // Check other agents.
    if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
        for (id, other) in agents {
            if id == agent_id.as_str() {
                continue;
            }
            let other_socket = tmux_socket_field(other).or(state_socket);
            if other
                .get("pane_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|op| {
                    op == pane_id && !tmux_sockets_known_different(agent_socket, other_socket)
                })
            {
                return true;
            }
        }
    }
    false
}

#[derive(Clone, Copy)]
struct PaneSocketBinding<'a> {
    pane_id: &'a str,
    tmux_socket: Option<&'a str>,
}

fn pane_conflicts_on_same_socket(
    pane_id: &str,
    agent_socket: Option<&str>,
    other: PaneSocketBinding<'_>,
) -> bool {
    other.pane_id == pane_id && !tmux_sockets_known_different(agent_socket, other.tmux_socket)
}

fn tmux_sockets_known_different(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    if left == right {
        return false;
    }
    std::path::Path::new(left).is_absolute() && std::path::Path::new(right).is_absolute()
}

fn runtime_tmux_socket(state: &serde_json::Value) -> Option<&str> {
    tmux_socket_field(state)
}

fn tmux_socket_field(value: &serde_json::Value) -> Option<&str> {
    value
        .get("tmux_endpoint")
        .or_else(|| value.get("tmux_socket"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

fn pane_socket_binding(value: &serde_json::Value) -> Option<PaneSocketBinding<'_>> {
    let pane_id = value
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty() && *s != "__team_agent_unbound__")?;
    Some(PaneSocketBinding {
        pane_id,
        tmux_socket: tmux_socket_field(value),
    })
}

fn agent_pane_live(
    transport: &dyn crate::transport::Transport,
    agent: &serde_json::Value,
) -> bool {
    let Some(pane) = agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
        .map(crate::transport::PaneId::new)
    else {
        return false;
    };
    agent_pane_live_by_id(transport, &pane)
}

fn agent_pane_live_by_id(
    transport: &dyn crate::transport::Transport,
    pane: &crate::transport::PaneId,
) -> bool {
    match transport.has_pane(&pane) {
        Ok(Some(live)) => live,
        Ok(None) | Err(_) => !matches!(
            transport.liveness(&pane),
            Ok(crate::model::enums::PaneLiveness::Dead)
        ),
    }
}

fn tmux_start_mode_for_spawn(
    spawn: &SpawnedAgentWindow,
    into_existing_session: bool,
) -> &'static str {
    if let Some(placement) = spawn.layout_placement.as_ref() {
        if placement.starts_window {
            if into_existing_session {
                "new-window"
            } else {
                "new-session"
            }
        } else {
            "split-window"
        }
    } else if into_existing_session {
        "new-window"
    } else {
        "new-session"
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 0.4.6 Stage 1: reset/start clean-boundary helpers
// ═══════════════════════════════════════════════════════════════════════

/// Bounded polling: wait for the old pane to become unreachable AND, when
/// possible, for the old pane_pid to exit. Returns a JSON evidence blob
/// recording the observed transition for events.
pub(super) fn drain_old_pane_and_pid(
    transport: &dyn crate::transport::Transport,
    old_pane: Option<&crate::transport::PaneId>,
    old_pid: Option<u32>,
) -> serde_json::Value {
    const DRAIN_MAX_MS: u64 = 1500;
    const DRAIN_POLL_MS: u64 = 50;
    let start = std::time::Instant::now();
    let mut pane_dead = old_pane.is_none();
    let mut pid_dead = old_pid.is_none();
    while start.elapsed().as_millis() < DRAIN_MAX_MS as u128 {
        if !pane_dead {
            if let Some(pane) = old_pane {
                if !agent_pane_live_by_id(transport, pane) {
                    pane_dead = true;
                }
            }
        }
        if !pid_dead {
            if let Some(pid) = old_pid {
                if !pid_is_alive(pid) {
                    pid_dead = true;
                }
            }
        }
        if pane_dead && pid_dead {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(DRAIN_POLL_MS));
    }
    serde_json::json!({
        "old_pane_id": old_pane.map(|p| p.as_str()),
        "old_pane_pid": old_pid,
        "old_pane_dead": pane_dead,
        "old_pid_dead": pid_dead,
        "drain_elapsed_ms": start.elapsed().as_millis() as u64,
    })
}

/// Best-effort liveness probe for a pid. Returns true if the pid exists
/// on Unix (signal 0). On other platforms, returns true conservatively.
pub(super) fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) checks if pid exists without sending a signal.
        let ret = unsafe { libc::kill(pid as i32, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM also means the process exists (we just can't signal it).
        let err = std::io::Error::last_os_error();
        matches!(err.raw_os_error(), Some(libc::EPERM))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Read state.agents[agent_id].pane_pid (u32) from the runtime state.
pub(super) fn state_pane_pid(state: &serde_json::Value, agent_id: &AgentId) -> Option<u32> {
    state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("pane_pid"))
        .and_then(serde_json::Value::as_u64)
        .map(|p| p as u32)
}

/// Read + increment state.agents[agent_id].spawn_epoch. The epoch is a
/// monotonic counter persisted with the agent row that uniquely tags each
/// fresh-start / reset / restart cycle. Subsequent capture / event /
/// status logic dispatches on `(team_key, agent_id, spawn_epoch,
/// pane_pid, expected_session_id)` to avoid attributing a stale prior
/// fresh attempt to the current process.
pub(super) fn next_spawn_epoch(state: &serde_json::Value, agent_id: &AgentId) -> u64 {
    let current = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("spawn_epoch"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    current.saturating_add(1)
}

/// `stop_agent(workspace, agent_id, team)`(`lifecycle/operations.py:62`)。
/// owner-gate → kill window → **同时关显示** → 写 state。
pub fn stop_agent(
    workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
) -> Result<StopAgentReport, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let transport =
        lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    stop_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        team,
        &transport,
    )
}

pub fn stop_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<StopAgentReport, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    stop_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        team,
        transport,
    )
}

pub(super) fn stop_agent_at_paths(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<StopAgentReport, LifecycleError> {
    // golden operations.py:64-66: resolve_team_scoped_state -> owner gate, BEFORE the unknown-worker raise.
    let mut state = resolve_team_scoped_state_or_refuse(workspace, team)?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, Some(agent_id))?;
    let spec = load_team_spec(spec_workspace)?;
    let agent = find_spec_agent(&spec, agent_id)
        .ok_or_else(|| unknown_worker(agent_id))?;
    let session_name = state_session_name_from_spec(&state, &spec);
    let window = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("window"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agent_id.as_str())
        .to_string();
    let pane_id = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("pane_id"))
        .and_then(|v| v.as_str())
        .filter(|pane| !pane.is_empty())
        .map(crate::transport::PaneId::new);
    let target_str = pane_id
        .as_ref()
        .map(|pane| pane.as_str().to_string())
        .unwrap_or_else(|| format!("{}:{window}", session_name.as_str()));
    let stopped = pane_id
        .as_ref()
        .map(|pane| agent_pane_live_by_id(transport, pane))
        .unwrap_or_else(|| window_exists(transport, &session_name, &window));
    if stopped {
        // golden operations.py:84-86: a non-zero kill-window raises
        // RuntimeError(f"failed to stop agent {agent_id}: {proc.stderr.strip()}").
        let kill_result = if let Some(pane) = pane_id.as_ref() {
            transport.kill_pane(pane)
        } else {
            let target = Target::SessionWindow {
                session: session_name.clone(),
                window: WindowName::new(&window),
            };
            transport.kill_window(&target)
        };
        if let Err(e) = kill_result {
            let stderr = match &e {
                crate::transport::TransportError::Subprocess { stderr, .. } => stderr.trim().to_string(),
                other => other.to_string(),
            };
            let _ = write_stop_window_failed_event(workspace, agent_id, &target_str, &stderr);
            return Err(LifecycleError::Transport(format!("failed to stop agent {agent_id}: {stderr}")));
        }
        // 0.4.6 Stage 1: drain-and-prove. The pre-fix path trusted `kill_pane`'s
        // success return without waiting for the pane / pid to actually exit.
        // This created a window where reset/start spawned a new worker while
        // the old Claude process + tty + provider state were still alive,
        // leading to the macmini "running but not capturable" failure mode.
        //
        // Poll bounded time for old pane to become unreachable. If it stays
        // reachable after the budget, emit an event but do NOT block — the
        // subsequent spawn boundary check (new pane_id != old pane_id) is
        // the final safety net.
        let drain_evidence = drain_old_pane_and_pid(
            transport,
            pane_id.as_ref(),
            state_pane_pid(&state, agent_id),
        );
        let _ = write_stop_drain_event(workspace, agent_id, &target_str, &drain_evidence);
    }
    close_agent_display(&mut state, agent_id);
    mark_agent_stopped(&mut state, agent_id, agent, &window)?;
    // golden operations.py:95: save_team_scoped_state (team projection) — NOT a raw save, so a
    // multi-team workspace keeps the other teams' persisted runtime state instead of being clobbered.
    crate::state::projection::save_team_scoped_state(workspace, &state)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    // golden operations.py:96-99: snapshot (side-effect), then state_file = write_team_state path.
    // snapshot.py:19-21 returns None silently when session_name is falsy — mirror that no-op here so
    // a workspace whose persisted state lacks session_name proceeds to write team_state and return ok
    // (do NOT swallow OTHER snapshot errors). The stop/reset/remove-force paths all inherit this guard.
    if session_name_present(&state) {
        crate::lifecycle::helpers::save_team_runtime_snapshot(workspace, &state)?;
    }
    let state_file = write_team_state(spec_workspace, &spec, &state)?;
    write_stop_complete_event(workspace, agent_id, &target_str, stopped)?;
    Ok(StopAgentReport {
        agent_id: agent_id.clone(),
        target: target_str,
        stopped,
        display_closed: true,
        state_file,
    })
}

/// golden `resolve_team_scoped_state` (state.py:243): returns the team-scoped projected state, or
/// surfaces the refusal dict (`team_target_ambiguous` / `team_target_unresolved`) as a typed error
/// BEFORE the owner gate / unknown-worker raise (operations.py:64-66). The lifecycle return types are
/// typed structs with no refusal-Value variant, so the observable refusal is carried in
/// `LifecycleError::TeamSelect`'s message (reason + error), which is the closest byte-faithful surface.
pub(super) fn resolve_team_scoped_state_or_refuse(
    workspace: &Path,
    team: Option<&str>,
) -> Result<serde_json::Value, LifecycleError> {
    let (state, refusal) = crate::state::projection::resolve_team_scoped_state(workspace, team)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) = refusal {
        let reason = refusal
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("team_target_unresolved");
        let detail = refusal
            .get("error")
            .or_else(|| refusal.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(LifecycleError::TeamSelect(format!("{reason}: {detail}")));
    }
    state.ok_or_else(|| LifecycleError::StatePersist("resolve_team_scoped_state returned no state".to_string()))
}

fn mark_agent_started(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    window: &str,
    spawn: &SpawnedAgentWindow,
    transport: &dyn crate::transport::Transport,
    safety: &DangerousApproval,
    start_mode: StartMode,
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
    // S1-CAPTURE-001 (0.4.8, CR M3 provider-agnostic): on a Fresh /
    // FreshAfterMissingRollout start, the prior session's authoritative
    // capture tuple MUST be cleared before persist_command_plan_state
    // writes the new _pending_session_id. Otherwise old session_id +
    // rollout_path coexist with new _pending_session_id and
    // agent_session_complete returns true on the stale tuple — capture
    // never re-binds to the new process, and any delivered token lands
    // in the old transcript (the leader/unassigned mis-attribution seen
    // in the gate evidence). This applies to all providers that resume:
    // codex, claude, copilot. Reset_agent --discard-session already does
    // this at common.rs:1144-1188; here we mirror it for start-agent /
    // restart-agent fresh paths so the fresh-tuple invariant is global.
    if matches!(start_mode, StartMode::Fresh | StartMode::FreshAfterMissingRollout) {
        for field in [
            "session_id",
            "rollout_path",
            "captured_at",
            "captured_via",
            "attribution_confidence",
            "capture_state",
            "attribution_ambiguous",
        ] {
            agent.remove(field);
        }
    }
    agent.insert("status".to_string(), serde_json::json!("running"));
    agent.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    agent.insert("window".to_string(), serde_json::json!(window));
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
    } else {
        agent.remove("pane_pid");
    }
    agent.insert(
        "spawned_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn.spawn_cwd.to_string_lossy().to_string()),
    );
    // 0.4.6 Stage 1+2: ensure spawn_epoch is present after every start.
    // reset_agent_at_paths bumps it before start; non-reset starts
    // initialise to 1 (or preserve existing) so capture/event paths can
    // always dispatch on (team_key, agent_id, spawn_epoch, ...).
    let preserved_epoch = agent
        .get("spawn_epoch")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0);
    agent.insert(
        "spawn_epoch".to_string(),
        serde_json::json!(preserved_epoch.unwrap_or(1)),
    );
    // Issue 2 (Round 3b gate review §6): persist the resolved owner_team_id
    // so future restart/start cycles read it directly from the agent row.
    if let Some(ref team_id) = spawn.owner_team_id {
        if !team_id.is_empty() {
            agent.insert(
                "owner_team_id".to_string(),
                serde_json::json!(team_id),
            );
        }
    }
    crate::lifecycle::launch::persist_command_plan_state(
        agent,
        &spawn.plan,
        &spawn.profile_launch,
    );
    crate::lifecycle::launch::persist_effective_approval_policy(agent, safety);
    if let Some(placement) = spawn.layout_placement.as_ref() {
        agent.insert(
            "layout_window".to_string(),
            serde_json::json!(placement.layout_window.as_str()),
        );
        agent.insert(
            "layout_index".to_string(),
            serde_json::json!(placement.layout_index),
        );
        agent.insert("pane_index".to_string(), serde_json::json!(placement.pane_index));
        agent.insert(
            "display".to_string(),
            serde_json::json!({
                "backend": "adaptive",
                "status": "opened",
                "window": placement.layout_window.as_str(),
                "workspace_window": null,
                "pane_id": spawn.spawn.pane_id.as_str(),
                "pane_title": agent_id.as_str(),
                "target": spawn.spawn.pane_id.as_str(),
                "target_worker_session": spawn.spawn.session.as_str(),
                "linked_session": null,
                "leader_session": spawn.spawn.session.as_str(),
                "display_session": null,
                "fallback": null,
            }),
        );
    }
    agent.remove("startup_prompts");
    agent.remove("startup_prompt_status");
    agent.remove("startup_prompt_probe_epoch");
    agent.remove("startup_prompt_probe_disabled_at");
    Ok(())
}

/// `reset_agent(workspace, agent_id, discard_session, open_display, team)`
/// (`lifecycle/operations.py:102`)。discard + 重起;**未传 discard_session → 拒绝**。
pub fn reset_agent(
    workspace: &Path,
    agent_id: &AgentId,
    discard_session: bool,
    open_display: bool,
    team: Option<&str>,
) -> Result<ResetAgentOutcome, LifecycleError> {
    if !discard_session {
        return Ok(ResetAgentOutcome::Refused {
            reason: ResetRefusal::DiscardSessionRequired,
        });
    }
    let paths = lifecycle_paths(workspace, team)?;
    let transport =
        lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    reset_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        discard_session,
        open_display,
        team,
        &transport,
    )
}

pub fn reset_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    discard_session: bool,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<ResetAgentOutcome, LifecycleError> {
    if !discard_session {
        return Ok(ResetAgentOutcome::Refused {
            reason: ResetRefusal::DiscardSessionRequired,
        });
    }
    let paths = lifecycle_paths(workspace, team)?;
    reset_agent_at_paths(
        &paths.run_workspace,
        &paths.spec_workspace,
        agent_id,
        discard_session,
        open_display,
        team,
        transport,
    )
}

fn reset_agent_at_paths(
    workspace: &Path,
    spec_workspace: &Path,
    agent_id: &AgentId,
    discard_session: bool,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<ResetAgentOutcome, LifecycleError> {
    if !discard_session {
        return Ok(ResetAgentOutcome::Refused {
            reason: ResetRefusal::DiscardSessionRequired,
        });
    }
    // golden operations.py:105-110: team-scope resolve + owner gate BEFORE the nested stop.
    let state_before_stop = resolve_team_scoped_state_or_refuse(workspace, team)?;
    let discarded_session_id = state_before_stop
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|session| !session.is_empty())
        .map(SessionId::new);
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state_before_stop, Some(agent_id))?;
    let stop = stop_agent_at_paths(workspace, spec_workspace, agent_id, team, transport)?;
    let mut state = resolve_team_scoped_state_or_refuse(workspace, team)?;
    let spec = load_team_spec(spec_workspace)?;
    discard_agent_session_fields(&mut state, agent_id)?;
    // 0.4.6 Stage 1: bump spawn_epoch on every reset. The new epoch is
    // visible to subsequent capture/event/status paths so they can
    // attribute observations to the current process cohort (not a stale
    // prior fresh attempt).
    let new_epoch = next_spawn_epoch(&state, agent_id);
    if let Some(obj) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    {
        obj.insert("spawn_epoch".to_string(), serde_json::json!(new_epoch));
    }
    let team_key = restart_projection_team_key(&state, team);
    sync_restart_team_projections(&mut state, &team_key);
    // golden operations.py (reset): save_team_scoped_state on the team projection — same multi-team
    // preservation as stop, not a raw save_runtime_state.
    crate::state::projection::save_team_scoped_state_with_tombstoned_agents(
        workspace,
        &state,
        &[agent_id.as_str()],
    )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    // golden operations.py:125: write_team_state after the discard-save (the intermediate stopped snapshot).
    write_team_state(spec_workspace, &spec, &state)?;
    write_reset_tombstone_event(
        workspace,
        agent_id,
        discarded_session_id
            .as_ref()
            .map(SessionId::as_str)
            .unwrap_or(""),
    )?;
    let start = start_agent_at_paths(
        workspace,
        spec_workspace,
        agent_id,
        true,
        open_display,
        true,
        team,
        transport,
    )?;
    let started = matches!(start, StartAgentOutcome::Running { .. });
    write_reset_complete_event(workspace, agent_id, stop.stopped, started)?;
    match start {
        StartAgentOutcome::Running {
            env,
            start_mode,
            session_id,
            new_session_id,
            ..
        } => {
            let output_session_id = if matches!(
                start_mode,
                StartMode::Fresh | StartMode::FreshAfterMissingRollout
            ) {
                new_session_id.clone().or(session_id)
            } else {
                session_id
            };
            Ok(ResetAgentOutcome::Reset {
                env,
                start_mode,
                discarded_session_id,
                session_id: output_session_id,
                new_session_id,
            })
        }
        StartAgentOutcome::Noop { env, .. } => Ok(ResetAgentOutcome::Reset {
            env,
            start_mode: StartMode::Noop,
            discarded_session_id,
            session_id: None,
            new_session_id: None,
        }),
        StartAgentOutcome::Paused { .. } => Ok(ResetAgentOutcome::Reset {
            env: AgentActionEnvelope {
                agent_id: agent_id.clone(),
                state_file: crate::state::persist::runtime_state_path(workspace),
                coordinator_started: false,
            },
            start_mode: StartMode::Noop,
            discarded_session_id,
            session_id: None,
            new_session_id: None,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_start_agent_start_event(
    workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    provider: crate::provider::Provider,
    start_mode: StartMode,
    session_name: &SessionName,
    window: &str,
    session_id: Option<&SessionId>,
    tmux_start_mode: &'static str,
) -> Result<(), LifecycleError> {
    let auth_mode = agent_auth_mode(agent);
    let model = agent.get("model").and_then(|v| v.as_str());
    let adapter = crate::provider::get_adapter(provider);
    // Contract C / F6.4: event log must record the same context-aware argv that the
    // actual spawn used — so the role/tools/MCP context appears in `start_agent.agent_start`.
    let safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn()?;
    let command_agent =
        crate::lifecycle::worker_command_context::WorkerCommandAgent::from_json(
            agent,
            Some(agent_id.as_str()),
            provider,
        );
    let system_prompt =
        crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
    let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
        &command_agent,
        provider,
        &safety,
    )?;
    let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let mcp_config = adapter
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let team_id = agent
        .get("owner_team_id")
        .and_then(|v| v.as_str());
    let mcp_config = crate::lifecycle::launch::resolve_mcp_config(
        mcp_config,
        workspace,
        agent_id.as_str(),
        team_id.unwrap_or(""),
    );
    let mcp_config_path =
        crate::lifecycle::launch::write_worker_mcp_config(workspace, agent_id.as_str(), &mcp_config)?;
    let profile_launch =
        crate::lifecycle::profile_launch::prepare_provider_profile_launch_from_json(
            workspace,
            agent_id.as_str(),
            agent,
            Some(&mcp_config),
        )?;
    let command_model = profile_launch
        .command_overrides
        .model
        .as_deref()
        .or(model);
    let context = crate::provider::ProviderCommandContext {
        auth_mode,
        mcp_config: Some(&mcp_config),
        system_prompt: Some(system_prompt.as_str()),
        model: command_model,
        tools: &resolved_tool_refs,
        profile_launch: Some(&profile_launch),
        agent_id_hint: Some(agent_id.as_str()),
    };
    let mut plan = match session_id {
        Some(session_id) => adapter
            .build_resume_command_plan(Some(session_id), context)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
        None => adapter
            .build_command_plan(context)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
    };
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        crate::lifecycle::launch::point_native_mcp_config_at_file(
            &mut plan.argv,
            provider,
            &mcp_config_path,
        );
    }
    crate::lifecycle::launch::fill_spawn_placeholders_full(
        &mut plan.argv,
        workspace,
        agent_id.as_str(),
        team_id,
    );
    crate::event_log::EventLog::new(workspace)
        .write(
            "start_agent.agent_start",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "provider": provider_wire(provider),
                "start_mode": start_mode,
                "session_id": session_id.map(|s| s.as_str()),
                "session": session_name.as_str(),
                "window": window,
                "tmux_start_mode": tmux_start_mode,
                "command": plan.argv,
                "mcp_config": agent.get("mcp_config").cloned().unwrap_or(serde_json::Value::Null),
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn write_stop_complete_event(
    workspace: &Path,
    agent_id: &AgentId,
    target: &str,
    stopped: bool,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "stop_agent.complete",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "target": target,
                "stopped": stopped,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

/// 0.4.6 Stage 1: drain evidence for the old pane+pid after stop. Fires
/// during reset/stop so operators can see whether the prior worker really
/// went away or stuck around inside the resp budget.
pub(super) fn write_stop_drain_event(
    workspace: &Path,
    agent_id: &AgentId,
    target: &str,
    drain: &serde_json::Value,
) -> Result<(), LifecycleError> {
    let mut payload = serde_json::Map::new();
    payload.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    payload.insert("target".to_string(), serde_json::json!(target));
    if let Some(obj) = drain.as_object() {
        for (k, v) in obj {
            payload.insert(k.clone(), v.clone());
        }
    }
    crate::event_log::EventLog::new(workspace)
        .write(
            "stop_agent.drain",
            serde_json::Value::Object(payload),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn write_stop_window_failed_event(
    workspace: &Path,
    agent_id: &AgentId,
    target: &str,
    stderr: &str,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "stop_agent.window_stop_failed",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "target": target,
                "stderr": stderr,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn write_reset_tombstone_event(
    workspace: &Path,
    agent_id: &AgentId,
    discarded_session_id: &str,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "discard.session_tombstone",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "discarded_session_id": discarded_session_id,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn write_reset_complete_event(
    workspace: &Path,
    agent_id: &AgentId,
    stopped: bool,
    started: bool,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "reset_agent.complete",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "stopped": stopped,
                "started": started,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e51_restart_allows_leader_same_pane_id_on_different_tmux_sockets() {
        let state = serde_json::json!({
            "tmux_endpoint": "/private/tmp/tmux-501/ta-worker",
            "leader_receiver": {
                "pane_id": "%0",
                "tmux_socket": "/private/tmp/tmux-501/default"
            },
            "agents": {
                "architect": {
                    "pane_id": "%0",
                    "tmux_socket": "/private/tmp/tmux-501/ta-worker"
                }
            }
        });
        let agent_id = AgentId::new("architect");
        let agent = state["agents"]["architect"].clone();

        assert!(
            !pane_conflicts_with_leader_or_other(&state, &agent_id, &agent),
            "same pane id on different tmux sockets must not force a fresh restart"
        );
    }

    #[test]
    fn e51_restart_keeps_leader_conflict_on_same_tmux_socket() {
        let socket = "/private/tmp/tmux-501/default";
        let state = serde_json::json!({
            "tmux_endpoint": socket,
            "leader_receiver": {
                "pane_id": "%0",
                "tmux_socket": socket
            },
            "agents": {
                "architect": {
                    "pane_id": "%0",
                    "tmux_socket": socket
                }
            }
        });
        let agent_id = AgentId::new("architect");
        let agent = state["agents"]["architect"].clone();

        assert!(
            pane_conflicts_with_leader_or_other(&state, &agent_id, &agent),
            "same pane id on the same tmux socket must keep the E51 guard"
        );
    }

    #[test]
    fn e51_restart_allows_other_agent_same_pane_id_on_different_tmux_sockets() {
        let state = serde_json::json!({
            "tmux_endpoint": "/private/tmp/tmux-501/ta-worker",
            "agents": {
                "architect": {
                    "pane_id": "%0",
                    "tmux_socket": "/private/tmp/tmux-501/ta-worker"
                },
                "reviewer": {
                    "pane_id": "%0",
                    "tmux_socket": "/private/tmp/tmux-501/default"
                }
            }
        });
        let agent_id = AgentId::new("architect");
        let agent = state["agents"]["architect"].clone();

        assert!(
            !pane_conflicts_with_leader_or_other(&state, &agent_id, &agent),
            "other-agent collision checks must also include tmux_socket"
        );
    }
}
