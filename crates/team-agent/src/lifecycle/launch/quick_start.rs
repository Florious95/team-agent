//! ---
//! purpose: 零配置一键起队入口，编译角色目录、起全队、判嵌套层级并给出 attach 指引
//! contract:
//!   provides:
//!     - name: quick_start
//!       what: 由角色目录一键起队
//!     - name: quick_start_in_workspace_with_display_and_backend
//!       what: 带显示开关与后端选择的起队入口
//!     - name: quick_start_with_transport_in_workspace_with_display
//!       what: 起队的实体实现，含 leader pane 校验、层级门与已有 runtime 的早退
//!   depends:
//!     - crate::compiler
//!     - crate::state::persist
//!     - crate::transport_factory
//!     - crate::lifecycle::launch::identity
//!     - crate::lifecycle::launch::quick_start_transport
//! boundary:
//!   - 只管初次起队，已有 runtime 时给出 restart 指引而不接管
//!   - 显式指定非 tmux 后端时不静默退回 tmux，不可用就如实报错
//!   - 团队嵌套超过两层直接拒绝
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
use crate::transport::{PaneField, PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

trait FreshQuickStartLeaderBindingOps {
    fn caller_pane(&mut self) -> Option<String>;
    fn explicit_provider(&mut self) -> Option<String>;
    fn observe_command(&mut self, pane: &PaneId) -> Option<String>;
    fn live_binding_elsewhere(&mut self, pane: &PaneId, workspace: &Path, team_key: &str) -> bool;
    fn attach(
        &mut self,
        workspace: &Path,
        state: &mut serde_json::Value,
        pane: &PaneId,
        provider: crate::provider::Provider,
    ) -> bool;
    fn register(&mut self, workspace: &Path, team_key: &str) -> bool;
    fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool;
}

struct RuntimeFreshQuickStartLeaderBindingOps<'a> {
    transport: &'a dyn Transport,
}

impl FreshQuickStartLeaderBindingOps for RuntimeFreshQuickStartLeaderBindingOps<'_> {
    fn caller_pane(&mut self) -> Option<String> {
        std::env::var("TMUX_PANE")
            .ok()
            .filter(|pane| !pane.is_empty())
    }

    fn explicit_provider(&mut self) -> Option<String> {
        std::env::var("TEAM_AGENT_LEADER_PROVIDER")
            .ok()
            .filter(|provider| !provider.is_empty())
    }

    fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
        self.transport
            .query(&Target::Pane(pane.clone()), PaneField::PaneCurrentCommand)
            .ok()
            .flatten()
            .filter(|command| !command.trim().is_empty())
    }

    fn live_binding_elsewhere(&mut self, pane: &PaneId, workspace: &Path, team_key: &str) -> bool {
        crate::leader::registry::live_same_pane_binding_elsewhere(
            pane.as_str(),
            workspace,
            team_key,
        )
    }

    fn attach(
        &mut self,
        workspace: &Path,
        state: &mut serde_json::Value,
        pane: &PaneId,
        provider: crate::provider::Provider,
    ) -> bool {
        let event_log = crate::event_log::EventLog::new(workspace);
        crate::leader::attach_leader_to_state(
            workspace,
            state,
            Some(pane),
            provider,
            &event_log,
            crate::leader::LeaseSource::QuickStart,
            true,
        )
        .is_ok()
    }

    fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
        crate::leader::registry::register_binding_from_state_best_effort(
            workspace,
            Some(team_key),
            "quick-start",
        )
        .is_some_and(|outcome| outcome.status == "registered" && outcome.path.is_some())
    }

    fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
        launched_team_receiver_is_attached(workspace, team_key)
    }
}

fn bind_fresh_quick_start_leader_with<O: FreshQuickStartLeaderBindingOps>(
    workspace: &Path,
    team_key: &str,
    ops: &mut O,
) -> bool {
    let Ok(resolved) = crate::state::projection::resolve_runtime_team_scope(
        workspace,
        Some(team_key),
    ) else {
        return false;
    };
    if resolved.canonical_team_key != team_key {
        return false;
    }
    let mut state = resolved.state;
    if ["team_owner", "leader_receiver"]
        .iter()
        .any(|key| state.get(*key).is_some_and(|value| !value.is_null()))
    {
        return false;
    }
    let Some(pane) = ops.caller_pane().filter(|pane| !pane.is_empty()) else {
        return false;
    };
    let pane = PaneId::new(pane);
    let Some(command) = ops
        .observe_command(&pane)
        .filter(|command| !command.trim().is_empty())
    else {
        return false;
    };
    let explicit_provider = ops.explicit_provider();
    let Some(provider) = crate::leader::owner_bind::strict_owner_bind_provider(
        explicit_provider.as_deref(),
        &command,
    ) else {
        return false;
    };
    if ops.live_binding_elsewhere(&pane, workspace, team_key) {
        return false;
    }
    if !ops.attach(workspace, &mut state, &pane, provider) {
        return false;
    }
    if !ops.register(workspace, team_key) {
        return false;
    }
    ops.canonical_readback(workspace, team_key)
}

fn bind_fresh_quick_start_leader(
    workspace: &Path,
    team_key: &str,
    transport: &dyn Transport,
) -> bool {
    bind_fresh_quick_start_leader_with(
        workspace,
        team_key,
        &mut RuntimeFreshQuickStartLeaderBindingOps { transport },
    )
}

/// ---
/// purpose: 由角色目录推出 workspace 后一键起队
/// params:
///   agents_dir: 角色定义目录
///   name: 请求的团队名
///   yes: 免确认标志
///   team_id: 显式团队键，优先于 name
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
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

/// ---
/// purpose: 在指定 workspace 起队，transport 优先复用调用方所在 tmux socket
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_in_workspace(
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

/// ---
/// purpose: 带显示开关与后端字面量的起队入口
/// params:
///   open_display: 为假时把 spec 的显示后端改成 none
///   backend: 未给或为 tmux 时走既有 tmux 路径，其余字面量经 transport 工厂解析
/// returns: 起队报告
/// errors: 后端字面量不认识时返回 TeamSelect；工厂拒绝或后端不可用时如实报错，不退回 tmux
/// ---
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

/// ---
/// purpose: 带注入 transport 的起队入口，由角色目录推 workspace
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_with_transport(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_with_transport_in_workspace(&workspace, agents_dir, name, yes, team_id, transport)
}

/// ---
/// purpose: 带注入 transport 与显式 workspace 的起队入口，默认开显示
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_with_transport_in_workspace(
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

/// ---
/// purpose: 起队的实体实现，校验 leader pane、编译 spec、定团队键、判层级、必要时早退，然后起队并给 attach 指引
/// params:
///   open_display: 为假时把显示后端改成 none
/// returns: 起队报告，含 session 名与 attach 命令
/// errors: leader pane 环境无效返回 RequirementUnmet；角色目录不存在或编译失败返回 Compile；嵌套层级超限返回 RequirementUnmet；读 state 失败返回 StatePersist
/// ---
pub(crate) fn quick_start_with_transport_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    let mut discover = |requested: &str| {
        crate::lifecycle::launch::pi_mcp::pi_model_candidates(requested).map_err(|_| ())
    };
    quick_start_with_transport_in_workspace_with_display_pi_preflight(
        workspace,
        agents_dir,
        name,
        yes,
        team_id,
        transport,
        open_display,
        &mut discover,
    )
}

pub(crate) fn quick_start_with_transport_in_workspace_with_display_pi_preflight(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<QuickStartReport, LifecycleError> {
    if !agents_dir.exists() {
        return Err(LifecycleError::Compile(format!(
            "agents dir not found: {}",
            agents_dir.display()
        )));
    }
    crate::compiler::preflight_pi_models_in_team_with(agents_dir, discover).map_err(|error| {
        LifecycleError::PiModelPreflight {
            requested: error.requested,
            candidates: error.candidates,
            action: error.action,
            not_ready: error.not_ready,
        }
    })?;
    let workspace = workspace.to_path_buf();
    let mut spec = crate::compiler::compile_team(agents_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    // B-7 / 036b N38 三行 fail-fast — TEAM_AGENT_LEADER_PANE_ID 主动路径在 quick-start
    // 入口验活;死/缺(Dead)的 pane 必须明确报错,不可 silent bind 到 spawner /
    // owner_bind / lease / display 任一消费点。被动路径(display/seed 等)各自走
    // 降级+event,不在这里挡。错误三行式:error(含 pane id 字面)/action(unset
    // 或修 env)/log(env var 名)。 Role/schema and Pi model admission above is pure and
    // must finish before this validation can emit a warning event.
    let team_workspace = team_workspace(agents_dir);
    let warning_workspaces = [workspace.as_path(), team_workspace.as_path()];
    validate_active_leader_pane_env_with_workspaces(transport, &warning_workspaces)?;
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
    // Fresh initialization owns this one fail-closed bind attempt. It is
    // independent of display layout, so --no-display never suppresses receiver
    // binding. Readiness receives true only after canonical registry readback.
    launch.leader_receiver_attached =
        bind_fresh_quick_start_leader(&workspace, &state_team_key, transport);
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

#[cfg(test)]
mod fresh_quick_start_leader_binding_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn workspace(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ta-quick-start-bind-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        crate::state::persist::save_runtime_state(
            &path,
            &json!({
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}}
            }),
        )
        .unwrap();
        path
    }

    struct MockOps {
        pane: Option<String>,
        explicit_provider: Option<String>,
        command: Option<String>,
        live_elsewhere: bool,
        attach_ok: bool,
        register_ok: bool,
        readback_ok: bool,
        attach_calls: usize,
        register_calls: usize,
        readback_calls: usize,
        attached_provider: Option<crate::provider::Provider>,
    }

    impl Default for MockOps {
        fn default() -> Self {
            Self {
                pane: Some("%42".to_string()),
                explicit_provider: Some("pi".to_string()),
                command: Some("pi".to_string()),
                live_elsewhere: false,
                attach_ok: true,
                register_ok: true,
                readback_ok: true,
                attach_calls: 0,
                register_calls: 0,
                readback_calls: 0,
                attached_provider: None,
            }
        }
    }

    impl FreshQuickStartLeaderBindingOps for MockOps {
        fn caller_pane(&mut self) -> Option<String> {
            self.pane.clone()
        }

        fn explicit_provider(&mut self) -> Option<String> {
            self.explicit_provider.clone()
        }

        fn observe_command(&mut self, _pane: &PaneId) -> Option<String> {
            self.command.clone()
        }

        fn live_binding_elsewhere(
            &mut self,
            _pane: &PaneId,
            _workspace: &Path,
            _team_key: &str,
        ) -> bool {
            self.live_elsewhere
        }

        fn attach(
            &mut self,
            workspace: &Path,
            state: &mut serde_json::Value,
            pane: &PaneId,
            provider: crate::provider::Provider,
        ) -> bool {
            self.attach_calls += 1;
            self.attached_provider = Some(provider);
            if !self.attach_ok {
                return false;
            }
            state["team_owner"] = json!({"pane_id": pane.as_str()});
            state["leader_receiver"] = json!({
                "pane_id": pane.as_str(),
                "status": "attached",
                "transport_kind": "direct_tmux"
            });
            crate::state::persist::save_runtime_state(workspace, state).is_ok()
        }

        fn register(&mut self, workspace: &Path, _team_key: &str) -> bool {
            self.register_calls += 1;
            let persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            assert_eq!(
                persisted
                    .pointer("/leader_receiver/pane_id")
                    .and_then(serde_json::Value::as_str),
                Some("%42")
            );
            self.register_ok
        }

        fn canonical_readback(&mut self, workspace: &Path, _team_key: &str) -> bool {
            self.readback_calls += 1;
            let persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            self.readback_ok
                && persisted
                    .get("leader_receiver")
                    .is_some_and(|receiver| !receiver.is_null())
        }
    }

    #[test]
    fn fresh_binding_persists_then_registers_then_requires_canonical_readback() {
        let workspace = workspace("positive");
        let mut ops = MockOps::default();
        assert!(bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            &mut ops
        ));
        assert_eq!(ops.attached_provider, Some(crate::provider::Provider::Pi));
        assert_eq!(ops.attach_calls, 1);
        assert_eq!(ops.register_calls, 1);
        assert_eq!(ops.readback_calls, 1);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(
            state
                .pointer("/leader_receiver/pane_id")
                .and_then(serde_json::Value::as_str),
            Some("%42")
        );
    }

    #[test]
    fn every_pre_attach_refusal_is_byte_preserving_and_never_registers() {
        for case in [
            "missing_pane",
            "unverifiable_pane",
            "empty_command",
            "unknown_provider",
            "unknown_explicit_provider",
            "team_mismatch",
            "existing_owner",
            "existing_receiver",
            "live_other_scope",
        ] {
            let workspace = workspace(case);
            let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
            let mut ops = MockOps::default();
            let team_key = if case == "team_mismatch" {
                "other"
            } else {
                "fresh"
            };
            match case {
                "missing_pane" => ops.pane = None,
                "unverifiable_pane" => ops.command = None,
                "empty_command" => ops.command = Some("  ".to_string()),
                "unknown_provider" => {
                    ops.explicit_provider = None;
                    ops.command = Some("node".to_string());
                }
                "unknown_explicit_provider" => {
                    ops.explicit_provider = Some("unknown".to_string());
                    ops.command = Some("codex".to_string());
                }
                "existing_owner" => state["team_owner"] = json!({"pane_id": "%old"}),
                "existing_receiver" => {
                    state["leader_receiver"] = json!({"pane_id": "%old"})
                }
                "live_other_scope" => ops.live_elsewhere = true,
                "team_mismatch" => {}
                _ => unreachable!(),
            }
            crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
            let before = std::fs::read(crate::state::persist::runtime_state_path(&workspace))
                .unwrap();
            assert!(
                !bind_fresh_quick_start_leader_with(&workspace, team_key, &mut ops),
                "{case} must refuse"
            );
            assert_eq!(
                before,
                std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap(),
                "{case} changed state bytes"
            );
            assert_eq!(ops.attach_calls, 0, "{case} reached attach");
            assert_eq!(ops.register_calls, 0, "{case} reached registry write");
            assert_eq!(ops.readback_calls, 0, "{case} reached readback");
        }
    }

    #[test]
    fn attach_registry_or_readback_failure_never_reports_bound() {
        let attach_workspace = workspace("attach-failure");
        let mut attach_ops = MockOps {
            attach_ok: false,
            ..MockOps::default()
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &attach_workspace,
            "fresh",
            &mut attach_ops
        ));
        assert_eq!(attach_ops.attach_calls, 1);
        assert_eq!(attach_ops.register_calls, 0);
        assert_eq!(attach_ops.readback_calls, 0);

        let registry_workspace = workspace("registry-failure");
        let mut registry_ops = MockOps {
            register_ok: false,
            ..MockOps::default()
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &registry_workspace,
            "fresh",
            &mut registry_ops
        ));
        assert_eq!(registry_ops.attach_calls, 1);
        assert_eq!(registry_ops.register_calls, 1);
        assert_eq!(registry_ops.readback_calls, 0);

        let readback_workspace = workspace("readback-failure");
        let mut readback_ops = MockOps {
            readback_ok: false,
            ..MockOps::default()
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &readback_workspace,
            "fresh",
            &mut readback_ops
        ));
        assert_eq!(readback_ops.attach_calls, 1);
        assert_eq!(readback_ops.register_calls, 1);
        assert_eq!(readback_ops.readback_calls, 1);
    }
}
