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

/// `quick_start(agents_dir, name, yes, team_id)`(`diagnose/quick_start.py:18`)。
/// 面向用户的零配置入口:编译 team_dir → `launch` → autobind leader receiver → 起
/// coordinator → `wait_ready` 轮询就绪。归入 lifecycle module(不与 diagnose 混)。
pub fn quick_start(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_in_workspace(&workspace, agents_dir, name, yes, team_id)
}

pub fn quick_start_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    let transport = quick_start_tmux_backend(&workspace);
    quick_start_with_transport_in_workspace(&workspace, agents_dir, name, yes, team_id, &transport)
}

pub fn quick_start_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    // Legacy entrypoint: no `--backend` override. Preserves the exact
    // tmux behavior we shipped in Phase 1c (byte-equivalent tmux path
    // when no explicit backend is asked for).
    quick_start_in_workspace_with_display_and_backend(
        workspace,
        agents_dir,
        name,
        yes,
        team_id,
        open_display,
        None,
    )
}

/// 0.5.x Phase 1d Batch 2: quick-start with an optional
/// `--backend <tmux|conpty>` override. When `backend` is `None` or
/// `Some("tmux")` the transport is the same one the legacy entrypoint
/// built (byte-equivalent to Phase 1c), so tmux users see no
/// behavioral change.
///
/// When `backend` is `Some("conpty")`, this call routes through the
/// factory. On a host without a live shim client (i.e. every
/// non-Windows host today), the resulting `ConPtyBackend` degrades
/// its spawn/inject/capture calls to `TransportError::MuxUnavailable`
/// honestly — MUST-NOT-13 + CR C-1 ①. Users see a real "conpty
/// unavailable" error rather than a silent tmux fallback.
pub fn quick_start_in_workspace_with_display_and_backend(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    open_display: bool,
    backend: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    // Default / `tmux` literal: preserve the existing tmux path
    // BYTE-FOR-BYTE. This is the CR §Batch 2 Verification anchor:
    // `quick-start` without `--backend` produces byte-equivalent tmux
    // behavior.
    let literal = backend.map(str::trim);
    let is_tmux_or_default = matches!(literal, None | Some("") | Some("tmux") | Some("TMUX"));
    if is_tmux_or_default {
        let transport = quick_start_tmux_backend(&workspace);
        return quick_start_with_transport_in_workspace_with_display(
            &workspace,
            agents_dir,
            name,
            yes,
            team_id,
            &transport,
            open_display,
        );
    }
    // Explicit non-tmux backend: route through the factory. Parse the
    // literal, then delegate. On unsupported literals or refused
    // preconditions we return `LifecycleError` — never silently pick
    // tmux (CR C-1 ①/②).
    let requested = crate::transport_factory::RequestedTransportBackend::parse_literal(
        literal.unwrap_or_default(),
    )
    .ok_or_else(|| {
        LifecycleError::TeamSelect(format!(
            "unsupported --backend literal {literal:?}; expected `tmux` or `conpty` \
             (Phase 1d does not auto-map `pty` to conpty — CR C-1 ②)"
        ))
    })?;
    let team_key_for_factory = team_id;
    // 0.5.x Windows portability Batch 8 F7 (leader msg_590b4dce0f68):
    // shim ownership moves to the coordinator. quick-start no longer
    // calls `spawn_shim_and_handshake` directly. Instead we ensure
    // the coordinator daemon is running; the coordinator's boot
    // path calls `conpty_shim::ensure_shim_running` (idempotent —
    // spawns if no live shim, reconnects if one is recorded).
    //
    // Rationale (from Batch 7 gate report F7): quick-start is a
    // one-shot process; if it owns the shim, the shim dies when
    // quick-start exits. Moving ownership to the coord daemon
    // gives us the "coord can die, shim survives" invariant the
    // design's §Shim Lifecycle chapter requires.
    //
    // The `ensure_coordinator_running` call is idempotent per
    // `coordinator/health.rs::start_coordinator`. On non-Windows
    // this whole block is cfg'd out.
    #[cfg(windows)]
    if matches!(
        requested,
        crate::transport_factory::RequestedTransportBackend::ConPty
    ) {
        let team_key_str = team_key_for_factory.ok_or_else(|| {
            LifecycleError::TeamSelect(
                "team_key required for --backend conpty on Windows (Batch 9 F8)".to_string(),
            )
        })?;
        // 0.5.x Windows portability Batch 9 F8 (leader msg_2a4cc1fa54c0):
        // Batch 8's seed-state pattern (writing active_team_key +
        // transport.kind to state.json before start_coordinator)
        // caused downstream launch code to see "existing runtime, use
        // restart" and skip spec compile. F8 fix: pass `--team`
        // directly to the coord daemon via `start_coordinator_with_team`
        // so state doesn't need pre-seeding.
        //
        // The coord daemon's `run_daemon_with_coordinator_and_boot_tmux`
        // then calls `ensure_shim_running` with the CLI-supplied
        // team_key. When quick-start's downstream code runs, state.json
        // still has whatever it had before (empty on fresh launch), so
        // the spec-compile + worker-spawn path runs normally.
        let run_ws = crate::coordinator::WorkspacePath::new(workspace.clone());
        let start_report =
            crate::coordinator::health::start_coordinator_with_team(&run_ws, Some(team_key_str))
                .map_err(|e| LifecycleError::TeamSelect(format!("coordinator start: {e}")))?;
        if !start_report.ok {
            return Err(LifecycleError::TeamSelect(format!(
                "coordinator start failed: schema_error={:?}, action={:?}",
                start_report.schema_error, start_report.action
            )));
        }
        // Give the coordinator a beat to write its transport.shim
        // block so the factory's `pipe_ready` gate opens on the
        // next resolve. The coordinator's `run_daemon` calls
        // `ensure_shim_running` inside its boot code path (see
        // `coordinator::backoff::run_daemon`).
        std::thread::sleep(std::time::Duration::from_millis(2500));
    }
    let input = crate::transport_factory::TransportFactoryInput::new(
        &workspace,
        crate::transport_factory::TransportPurpose::Launch,
    )
    .with_team_key(team_key_for_factory)
    .with_explicit_backend(Some(requested));
    // On Windows the factory needs to SEE the freshly-persisted
    // `state.transport.shim.pipe_ready = true` marker so its
    // `conpty_pipe_ready` gate opens.
    #[cfg(windows)]
    let state_value = crate::state::repository::StateRepository::new(&workspace)
        .load_workspace_if_exists_without_migrations()
        .ok()
        .flatten();
    #[cfg(windows)]
    let input = match state_value.as_ref() {
        Some(v) => input.with_state(Some(v)),
        None => input,
    };
    let resolved = crate::transport_factory::resolve_transport(input)
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // Hand the boxed backend down as `&dyn Transport`.
    quick_start_with_transport_in_workspace_with_display(
        &workspace,
        agents_dir,
        name,
        yes,
        team_id,
        &*resolved.backend,
        open_display,
    )
}

pub fn quick_start_with_transport(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_with_transport_in_workspace(&workspace, agents_dir, name, yes, team_id, transport)
}

pub fn quick_start_with_transport_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    quick_start_with_transport_in_workspace_with_display(
        workspace, agents_dir, name, yes, team_id, transport, true,
    )
}

pub fn quick_start_with_transport_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    // B-7 / 036b N38 三行 fail-fast — TEAM_AGENT_LEADER_PANE_ID 主动路径在 quick-start
    // 入口验活;死/缺(Dead)的 pane 必须明确报错,不可 silent bind 到 spawner /
    // owner_bind / lease / display 任一消费点。被动路径(display/seed 等)各自走
    // 降级+event,不在这里挡。错误三行式:error(含 pane id 字面)/action(unset
    // 或修 env)/log(env var 名)。
    let team_workspace = team_workspace(agents_dir);
    let warning_workspaces = [workspace, team_workspace.as_path()];
    validate_active_leader_pane_env_with_workspaces(transport, &warning_workspaces)?;
    if !agents_dir.exists() {
        return Err(LifecycleError::Compile(format!(
            "agents dir not found: {}",
            agents_dir.display()
        )));
    }
    let workspace = workspace.to_path_buf();
    let mut spec = crate::compiler::compile_team(agents_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    override_spec_workspace(&mut spec, &workspace);
    if !open_display {
        override_spec_display_backend(&mut spec, "none");
    }
    let explicit_team_key = quick_start_requested_team_key(team_id, name).map(str::to_string);
    let canonical_team_key = explicit_team_key
        .clone()
        .or_else(|| spec_team_id(&spec).filter(|team| !team.is_empty()))
        .unwrap_or_else(|| {
            runtime_team_key_for_spec(
                &agents_dir.join("team.spec.yaml"),
                &spec,
                &spec_session_name(&spec),
            )
        });
    let requested_team = Some(canonical_team_key.clone());
    let team_depth = quick_start_depth_guard(
        &workspace,
        agents_dir,
        requested_team.as_deref(),
        matches!(transport.kind(), crate::transport::BackendKind::Tmux),
    )?;
    if team_depth.team_depth > 2 {
        let parent = team_depth.parent_team_key.as_deref().unwrap_or("");
        return Err(LifecycleError::RequirementUnmet(format!(
            "team nesting depth limit exceeded: parent_team_key={parent} parent_depth={} max_depth=2",
            team_depth.team_depth.saturating_sub(1)
        )));
    }
    let state_path = crate::state::persist::runtime_state_path(&workspace);
    if state_path.exists() {
        let state = crate::state::persist::load_runtime_state(&workspace)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        if requested_team
            .as_deref()
            .is_none_or(|team| runtime_state_has_quick_start_team(&state, team))
        {
            let session_name = state
                .get("session_name")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(SessionName::new);
            let attach_commands = session_name
                .as_ref()
                .map(|session| {
                    let windows = quick_start_attach_window_names(&state);
                    attach_commands_for_runtime_windows(
                        state
                            .get("tmux_endpoint")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| {
                                state.get("tmux_socket").and_then(serde_json::Value::as_str)
                            }),
                        &workspace,
                        session,
                        windows.iter().map(String::as_str),
                    )
                })
                .unwrap_or_default();
            // Stage QR (design doc .team/artifacts/quickstart-restart-separation-design.md):
            // quick-start is initial-creation-only. When the team
            // already has runtime state, do NOT mention `--fresh`
            // (it's been removed); steer the operator to the
            // restart flow which owns resume + reset semantics.
            let mut next_actions = vec![
                "this team already has runtime state — use `team-agent restart` \
                 to resume it (quick-start is for first-time creation only). \
                 If recovery is impossible and the operator EXPLICITLY \
                 accepts losing context, restart accepts `--allow-fresh`."
                    .to_string(),
            ];
            if session_name.is_some() {
                if crate::tmux_backend::socket_probe_missing_for_workspace(&workspace) {
                    next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(
                        &workspace,
                    ));
                }
                next_actions.extend(attach_commands.iter().cloned());
            }
            return Ok(QuickStartReport::ExistingRuntime {
                team: requested_team.clone(),
                session_name,
                state_path: Some(state_path),
                next_actions,
                attach_commands,
            });
        }
    }
    // CR-040/042: repeated quick-start from one template with distinct --team-id/--name
    // must NOT collide on the template-derived tmux session. Override the compiled
    // spec's runtime.session_name with one derived from the REQUESTED team identity
    // so launch_with_transport (which reads runtime.session_name) spawns into an
    // isolated session per requested team.
    if let Some(requested) = explicit_team_key.as_deref() {
        override_spec_session_name(&mut spec, &format!("team-{requested}"));
    }
    let session_name = spec_session_name(&spec);
    // team_key 已在入口由显式 id/name 或 compiled spec identity 单次确定。
    let state_team_key = canonical_team_key;
    warn_ignored_owner_team_id(workspace.as_path(), agents_dir, &state_team_key);
    // E5 spec 迁移:spec 写到 .team/runtime/<team_key>/(中间产物,绝不落用户目录 agents_dir)。
    // Bug2:原子写(tmp+rename),避免半截 spec。
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, &state_team_key);
    write_spec_atomic(&spec_path, &spec)?;
    let _store = crate::message_store::MessageStore::open(&workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let resolved_spec_path =
        std::fs::canonicalize(&spec_path).unwrap_or_else(|_| spec_path.clone());
    let mut state = initial_runtime_state(
        &spec,
        &resolved_spec_path,
        &workspace,
        agents_dir,
        &state_team_key,
    );
    // 0.5.x Phase 1d hot-path 接线(裁决1 msg_76e1d98202b8): use the
    // generic annotator that writes `state.transport = { kind, source }`
    // for every backend AND (for tmux) preserves the existing
    // `tmux_endpoint`/`tmux_socket`/`tmux_socket_source` fields via the
    // inner `annotate_runtime_tmux_endpoint` call. Source is threaded
    // from the caller-side factory selection when available; the
    // legacy quick-start entrypoint currently passes `None` (defaults
    // to "unknown" in the payload) — Phase 2 refactor will thread
    // ResolvedTransport.source through the launch signature.
    annotate_runtime_transport(&mut state, transport, &workspace, None);
    save_launched_team_state_for_key(&workspace, &state, Some(&state_team_key), None)?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    // FIX (rt-host-a real-machine finding): dry_run=false so launch_with_transport calls spawn_agents
    // and really creates the tmux session + worker windows (was hardcoded true → never spawned, which
    // also starved the coordinator: no session → first tick TmuxSessionMissing → run_daemon loop exits).
    // 0.5.38 (`.team/artifacts/startup-latency-locate.md` §5): quick-start
    // owns the outer `launch.phase` timer so `coordinator_start` /
    // `readiness_wait` / `completed` fire monotonically after the inner
    // launch_with_transport's own `compile_spec` / `spawn_all` events.
    let quick_start_phase_timer = crate::lifecycle::restart::RestartPhaseTimer::start();
    let mut launch =
        launch_with_transport_in_workspace(&workspace, &spec_path, false, yes, true, transport)?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    launch.leader_receiver_attached =
        launched_team_receiver_is_attached(&workspace, &state_team_key);
    launch.session_capture_incomplete_agents =
        quick_start_session_capture_incomplete_agents(&workspace, &state_team_key);
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.clone());
    let coordinator_started = crate::coordinator::start_coordinator(&coordinator_workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    quick_start_phase_timer.emit(&workspace, "launch.phase", "coordinator_start");
    let coordinator_action = if coordinator_started {
        "coordinator started"
    } else {
        "coordinator not started"
    };
    // BUG-7: build an honest readiness verdict from the post-spawn runtime state.
    // - If persist_spawn_agent_state (BUG-2 fix) marked any agent non-running, the
    //   team is observably Degraded.
    // - Otherwise the framework cannot itself verify that the worker's MCP tool set
    //   loaded successfully (provider-side codex/claude schema rejections happen
    //   asynchronously after spawn), so the verdict is PendingToolLoad — never
    //   bare Ready.
    quick_start_phase_timer.emit(&workspace, "launch.phase", "readiness_wait");
    let worker_readiness = quick_start_worker_readiness(&workspace, &state_team_key);
    let attach_windows = load_runtime_state(&workspace)
        .ok()
        .map(|state| {
            attach_window_names_with_managed_leader(
                &state,
                started_attach_window_names(&launch.started),
            )
        })
        .unwrap_or_else(|| started_attach_window_names(&launch.started));
    let attach_commands = attach_commands_for_runtime_windows(
        launch.tmux_endpoint.as_deref(),
        &workspace,
        &session_name,
        attach_windows.iter().map(String::as_str),
    );
    let mut next_actions = vec![format!(
        "team compiled; real spawn is behind the transport/provider boundary; {coordinator_action}"
    )];
    // Stage QR (design doc
    // .team/artifacts/quickstart-restart-separation-design.md):
    // quick-start is initial-creation-only. After success, remind the
    // operator that subsequent starts must go through restart so the
    // user's context (sessions, ownership, tasks) survives.
    next_actions.push(
        "quick-start initialized this team; for all subsequent starts use \
         `team-agent restart` to resume context (quick-start refuses on \
         already-initialised teams)"
            .to_string(),
    );
    if crate::tmux_backend::socket_probe_missing_for_workspace(&workspace) {
        next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(
            &workspace,
        ));
    }
    next_actions.extend(attach_commands.iter().cloned());
    let display_backend = state
        .get("display_backend")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none")
        .to_string();
    quick_start_phase_timer.emit(&workspace, "launch.phase", "completed");
    Ok(QuickStartReport::Ready {
        session_name,
        launch: Box::new(launch),
        next_actions,
        attach_commands,
        display_backend,
        worker_readiness,
    })
}
