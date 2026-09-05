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
    fn tmux_endpoint(&mut self) -> Option<String>;
    fn observe_command(&mut self, pane: &PaneId) -> Option<String>;
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

    fn tmux_endpoint(&mut self) -> Option<String> {
        self.transport.tmux_endpoint()
    }

    fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
        self.transport
            .query(&Target::Pane(pane.clone()), PaneField::PaneCurrentCommand)
            .ok()
            .flatten()
            .filter(|command| !command.trim().is_empty())
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

struct BindingFileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl BindingFileSnapshot {
    fn capture(path: PathBuf) -> Self {
        let bytes = std::fs::read(&path).ok();
        Self { path, bytes }
    }

    fn restore(&self) {
        match &self.bytes {
            Some(bytes) => {
                let tmp = self.path.with_extension(format!(
                    "rollback-{}",
                    std::process::id()
                ));
                if std::fs::write(&tmp, bytes).is_ok()
                    && std::fs::rename(&tmp, &self.path).is_err()
                {
                    let _ = std::fs::remove_file(tmp);
                }
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn binding_snapshots(workspace: &Path, team_key: &str) -> Vec<BindingFileSnapshot> {
    let mut snapshots = vec![BindingFileSnapshot::capture(
        crate::state::persist::runtime_state_path(workspace),
    )];
    if let Some(dir) = crate::leader::registry::registry_dir() {
        snapshots.push(BindingFileSnapshot::capture(dir.join(format!(
            "{}__{team_key}.json",
            crate::leader::registry::workspace_hash(workspace)
        ))));
    }
    snapshots
}

fn same_workspace(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf())
        == std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf())
}

fn persisted_binding_matches_verified_pane(
    state: &serde_json::Value,
    workspace: &Path,
    pane: &PaneId,
    provider: crate::provider::Provider,
    endpoint: Option<&str>,
) -> bool {
    let Some(endpoint) = endpoint.filter(|value| !value.is_empty()) else {
        return false;
    };
    if state
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| !same_workspace(Path::new(value), workspace))
    {
        return false;
    }
    let Some(owner) = state.get("team_owner").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let Some(receiver) = state
        .get("leader_receiver")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let provider = serde_json::to_value(provider)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string));
    let Some(provider) = provider.as_deref() else {
        return false;
    };
    let owner_epoch = owner.get("owner_epoch").and_then(serde_json::Value::as_u64);
    let receiver_epoch = receiver
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64);
    let owner_uuid = owner
        .get("leader_session_uuid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    let receiver_uuid = receiver
        .get("leader_session_uuid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    receiver.get("status").and_then(serde_json::Value::as_str) == Some("attached")
        && receiver.get("mode").and_then(serde_json::Value::as_str) == Some("direct_tmux")
        && owner.get("pane_id").and_then(serde_json::Value::as_str) == Some(pane.as_str())
        && receiver.get("pane_id").and_then(serde_json::Value::as_str) == Some(pane.as_str())
        && owner.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
        && receiver.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
        && owner_epoch.is_some_and(|epoch| epoch > 0)
        && owner_epoch == receiver_epoch
        && owner_uuid.is_some()
        && owner_uuid == receiver_uuid
        && receiver
            .get("tmux_socket")
            .and_then(serde_json::Value::as_str)
            == Some(endpoint)
        && receiver
            .get("authorized_team_workspace")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .is_none_or(|value| same_workspace(Path::new(value), workspace))
}

fn bind_fresh_quick_start_leader_with<O: FreshQuickStartLeaderBindingOps>(
    workspace: &Path,
    team_key: &str,
    seeded_owner: Option<&serde_json::Value>,
    ops: &mut O,
) -> Result<bool, LifecycleError> {
    let Ok(resolved) = crate::state::projection::resolve_runtime_team_scope(
        workspace,
        Some(team_key),
    ) else {
        return Ok(false);
    };
    if resolved.canonical_team_key != team_key {
        return Ok(false);
    }
    let mut state = resolved.state;
    let Some(pane) = ops.caller_pane().filter(|pane| !pane.is_empty()) else {
        return Ok(false);
    };
    let pane = PaneId::new(pane);
    let Some(command) = ops
        .observe_command(&pane)
        .filter(|command| !command.trim().is_empty())
    else {
        return Ok(false);
    };
    let explicit_provider = ops.explicit_provider();
    let Some(provider) = crate::leader::owner_bind::strict_owner_bind_provider(
        explicit_provider.as_deref(),
        &command,
    ) else {
        return Ok(false);
    };
    let endpoint = ops.tmux_endpoint();
    let has_persisted_binding = ["team_owner", "leader_receiver"]
        .iter()
        .any(|key| state.get(*key).is_some_and(|value| !value.is_null()));
    // `claimed_via` is historical state, not proof that this invocation seeded
    // the owner. Cleanup is allowed only when the caller supplies the exact
    // in-memory seed created immediately before this bind attempt and the
    // projected state still contains that seed.
    let fresh_seeded_binding = seeded_owner.is_some_and(|seed| {
        state
            .get("team_owner")
            .is_some_and(|current| current == seed)
    });
    // Same-Team owner collision remains fail-closed. Cross-Team registry rows
    // are intentionally irrelevant: pane ids are scoped to their tmux server,
    // and independent endpoints may reuse the same numeric id.
    if has_persisted_binding
        && !persisted_binding_matches_verified_pane(
            &state,
            workspace,
            &pane,
            provider,
            endpoint.as_deref(),
        )
    {
        return Ok(false);
    }
    // Registry publication is the commit point. Restore persisted surfaces on
    // failure, except for the caller-seeded owner that must be cleared.
    let snapshots = binding_snapshots(workspace, team_key);
    let attached = has_persisted_binding || ops.attach(workspace, &mut state, &pane, provider);
    let committed = attached
        && ops.register(workspace, team_key)
        && ops.canonical_readback(workspace, team_key);
    if !committed {
        if fresh_seeded_binding
            && seeded_owner.is_some_and(|seed| {
                state
                    .get("team_owner")
                    .is_some_and(|current| current == seed)
            })
        {
            // Restore only the registry surface. The initial runtime state may
            // already contain a caller-seeded owner; restoring it after a failed
            // commit would make a later claim falsely report already_bound.
            for snapshot in snapshots.iter().skip(1).rev() {
                snapshot.restore();
            }
            clear_fresh_binding_on_refusal(workspace, &mut state, team_key)?;
        } else {
            // No caller-seeded owner was ours to clean up, so restore all
            // surfaces byte-for-byte after an attach/register/readback failure.
            for snapshot in snapshots.iter().rev() {
                snapshot.restore();
            }
        }
    }
    Ok(committed)
}

fn should_emit_workspace_socket_missing_hint(
    selected_source: Option<&str>,
    workspace_socket_missing: bool,
) -> bool {
    workspace_socket_missing && selected_source != Some("leader_env")
}

fn clear_fresh_binding_on_refusal(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
) -> Result<(), LifecycleError> {
    // A projected team state carries the seeded owner both at the root and in
    // `teams.<key>`. The repository deliberately preserves a newer owner from
    // disk, so make the selected team tombstone newer before removing the
    // candidate; otherwise a failed fresh bind can be resurrected by the
    // lock-held merge and later look already_bound.
    let current_epoch = [
        state.get("owner_epoch"),
        state
            .get("team_owner")
            .and_then(|owner| owner.get("owner_epoch")),
        state
            .get("leader_receiver")
            .and_then(|receiver| receiver.get("owner_epoch")),
        state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .and_then(|teams| teams.get(team_key))
            .and_then(serde_json::Value::as_object)
            .and_then(|team| team.get("owner_epoch")),
    ]
    .into_iter()
    .flatten()
    .filter_map(serde_json::Value::as_u64)
    .max()
    .unwrap_or(0);
    let next_epoch = current_epoch.saturating_add(1);
    if let Some(obj) = state.as_object_mut() {
        obj.remove("leader_receiver");
        obj.remove("team_owner");
        obj.remove("owner_epoch");
    }
    if let Some(team) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_key))
        .and_then(serde_json::Value::as_object_mut)
    {
        team.insert("leader_receiver".to_string(), serde_json::Value::Null);
        team.insert("team_owner".to_string(), serde_json::Value::Null);
        team.insert("owner_epoch".to_string(), serde_json::json!(next_epoch));
    }
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::ClaimLeader { team_key },
            state,
        )
        .map_err(|error| LifecycleError::StatePersist(format!(
            "quick-start binding cleanup failed: {error}"
        )))
}

fn bind_fresh_quick_start_leader(
    workspace: &Path,
    team_key: &str,
    seeded_owner: Option<&serde_json::Value>,
    transport: &dyn Transport,
) -> Result<bool, LifecycleError> {
    bind_fresh_quick_start_leader_with(
        workspace,
        team_key,
        seeded_owner,
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
                if should_emit_workspace_socket_missing_hint(
                    state
                        .get("tmux_socket_source")
                        .and_then(serde_json::Value::as_str),
                    crate::tmux_backend::socket_probe_missing_for_workspace(&workspace),
                ) {
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
    // Keep this attempt-local seed separate from persisted `claimed_via`;
    // historical quick-start rows are never sufficient cleanup authority.
    let seeded_owner = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(&state_team_key))
        .and_then(|team| team.get("team_owner"))
        .filter(|owner| {
            owner
                .get("claimed_via")
                .and_then(serde_json::Value::as_str)
                == Some("quick-start")
        })
        .cloned();
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
    launch.leader_receiver_attached = bind_fresh_quick_start_leader(
        &workspace,
        &state_team_key,
        seeded_owner.as_ref(),
        transport,
    )?;
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
    if should_emit_workspace_socket_missing_hint(
        selected_tmux_socket_source(transport, &workspace),
        crate::tmux_backend::socket_probe_missing_for_workspace(&workspace),
    ) {
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
        endpoint: Option<String>,
        command: Option<String>,
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
                endpoint: Some("/private/tmp/tmux-test/default".to_string()),
                command: Some("pi".to_string()),
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

        fn tmux_endpoint(&mut self) -> Option<String> {
            self.endpoint.clone()
        }

        fn observe_command(&mut self, _pane: &PaneId) -> Option<String> {
            self.command.clone()
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
                state["failed_attach_mutation"] = json!(true);
                let _ = crate::state::persist::save_runtime_state(workspace, state);
                return false;
            }
            state["leader_receiver"] = json!({
                "mode": "direct_tmux",
                "pane_id": pane.as_str(),
                "status": "attached",
                "provider": "pi",
                "leader_session_uuid": "uuid-fresh",
                "owner_epoch": 1,
                "tmux_socket": self.endpoint
            });
            state["team_owner"] = json!({
                "pane_id": pane.as_str(),
                "provider": "pi",
                "leader_session_uuid": "uuid-fresh",
                "owner_epoch": 1
            });
            crate::state::persist::save_runtime_state(workspace, state).is_ok()
        }

        fn register(&mut self, workspace: &Path, _team_key: &str) -> bool {
            self.register_calls += 1;
            let mut persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            assert_eq!(
                persisted
                    .pointer("/leader_receiver/pane_id")
                    .and_then(serde_json::Value::as_str),
                Some("%42")
            );
            if !self.register_ok {
                persisted["failed_registry_mutation"] = json!(true);
                crate::state::persist::save_runtime_state(workspace, &persisted).unwrap();
            }
            self.register_ok
        }

        fn canonical_readback(&mut self, workspace: &Path, _team_key: &str) -> bool {
            self.readback_calls += 1;
            let mut persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            if !self.readback_ok {
                persisted["failed_readback_mutation"] = json!(true);
                crate::state::persist::save_runtime_state(workspace, &persisted).unwrap();
            }
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
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
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
            "provider_mismatch",
            "socket_mismatch",
            "workspace_mismatch",
            "dual_state_mismatch",
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
                "provider_mismatch" | "socket_mismatch" | "workspace_mismatch"
                | "dual_state_mismatch" => {
                    state["workspace"] = json!(workspace);
                    state["team_owner"] = json!({
                        "pane_id": "%42", "provider": "pi",
                        "leader_session_uuid": "uuid-fresh", "owner_epoch": 1
                    });
                    state["leader_receiver"] = json!({
                        "mode": "direct_tmux", "status": "attached", "pane_id": "%42",
                        "provider": "pi", "leader_session_uuid": "uuid-fresh", "owner_epoch": 1,
                        "tmux_socket": "/private/tmp/tmux-test/default"
                    });
                    match case {
                        "provider_mismatch" => state["leader_receiver"]["provider"] = json!("codex"),
                        "socket_mismatch" => state["leader_receiver"]["tmux_socket"] = json!("/tmp/other"),
                        "workspace_mismatch" => state["workspace"] = json!(workspace.join("other")),
                        "dual_state_mismatch" => state["team_owner"]["owner_epoch"] = json!(2),
                        _ => unreachable!(),
                    }
                }
                "team_mismatch" => {}
                _ => unreachable!(),
            }
            crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
            let before = std::fs::read(crate::state::persist::runtime_state_path(&workspace))
                .unwrap();
            assert!(
                !bind_fresh_quick_start_leader_with(&workspace, team_key, None, &mut ops)
                    .unwrap(),
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

    struct HomeGuard(Option<std::ffi::OsString>);

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self(old)
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                std::env::set_var("HOME", value);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn ambient_dual_state_workspace(tag: &str) -> PathBuf {
        let workspace = workspace(tag);
        let ambient = "/private/tmp/tmux-test/default";
        crate::state::persist::save_runtime_state(
            &workspace,
            &json!({
                "workspace": workspace,
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}},
                "teams": {
                    "current": {
                        "team_key": "current",
                        "session_name": "team-parent",
                        "agents": {}
                    },
                    "fresh": {
                        "workspace": workspace,
                        "team_key": "fresh",
                        "session_name": "team-fresh",
                        "agents": {"sol": {"status": "running", "provider": "pi"}},
                        "team_owner": {
                            "pane_id": "%42",
                            "provider": "pi",
                            "leader_session_uuid": "uuid-fresh",
                            "owner_epoch": 1
                        },
                        "leader_receiver": {
                            "mode": "direct_tmux",
                            "status": "attached",
                            "pane_id": "%42",
                            "provider": "pi",
                            "leader_session_uuid": "uuid-fresh",
                            "owner_epoch": 1,
                            "tmux_socket": ambient
                        },
                        "owner_epoch": 1
                    }
                }
            }),
        )
        .unwrap();
        workspace
    }

    struct AmbientRegistryOps {
        readback_ok: bool,
        attach_calls: usize,
    }

    impl FreshQuickStartLeaderBindingOps for AmbientRegistryOps {
        fn caller_pane(&mut self) -> Option<String> {
            Some("%42".to_string())
        }

        fn explicit_provider(&mut self) -> Option<String> {
            Some("pi".to_string())
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            Some("/private/tmp/tmux-test/default".to_string())
        }

        fn observe_command(&mut self, _pane: &PaneId) -> Option<String> {
            Some("pi".to_string())
        }

        fn attach(
            &mut self,
            _workspace: &Path,
            _state: &mut serde_json::Value,
            _pane: &PaneId,
            _provider: crate::provider::Provider,
        ) -> bool {
            self.attach_calls += 1;
            false
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
            self.readback_ok && launched_team_receiver_is_attached(workspace, team_key)
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn ambient_dual_state_registers_and_reads_back_when_workspace_socket_is_missing() {
        let workspace = ambient_dual_state_workspace("ambient-dual-state");
        let home = workspace.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = HomeGuard::set(&home);
        assert!(crate::tmux_backend::socket_probe_missing_for_workspace(
            &workspace
        ));
        let mut ops = AmbientRegistryOps {
            readback_ok: true,
            attach_calls: 0,
        };
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(ops.attach_calls, 0, "preseeded dual state must not reattach");
        let path = crate::leader::registry::registry_dir().unwrap().join(format!(
            "{}__fresh.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        let entry: crate::leader::registry::LeaderRegistryEntry =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            entry.channel.get("tmux_socket").and_then(serde_json::Value::as_str),
            Some("/private/tmp/tmux-test/default")
        );
    }

    #[test]
    fn historical_quick_start_owner_is_not_cleanup_authority() {
        let workspace = workspace("historical-quick-start-owner");
        let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        state["workspace"] = json!(workspace);
        state["team_owner"] = json!({
            "pane_id": "%old",
            "provider": "pi",
            "leader_session_uuid": "uuid-old",
            "owner_epoch": 4,
            "claimed_via": "quick-start"
        });
        state["leader_receiver"] = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%old",
            "provider": "pi",
            "leader_session_uuid": "uuid-old",
            "owner_epoch": 4,
            "tmux_socket": "/private/tmp/tmux-test/old"
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let before = std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap();
        let mut ops = MockOps::default();

        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(
            before,
            std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap(),
            "historical claimed_via alone must not authorize cleanup"
        );
    }

    #[test]
    fn attach_registry_or_readback_failure_rolls_back_exact_state_bytes() {
        for (case, mut ops) in [
            (
                "attach-failure",
                MockOps {
                    attach_ok: false,
                    ..MockOps::default()
                },
            ),
            (
                "registry-failure",
                MockOps {
                    register_ok: false,
                    ..MockOps::default()
                },
            ),
            (
                "readback-failure",
                MockOps {
                    readback_ok: false,
                    ..MockOps::default()
                },
            ),
        ] {
            let workspace = workspace(case);
            let state_path = crate::state::persist::runtime_state_path(&workspace);
            let before = std::fs::read(&state_path).unwrap();
            assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
                .unwrap());
            assert_eq!(
                std::fs::read(&state_path).unwrap(),
                before,
                "{case} left partial state bytes"
            );
            match case {
                "attach-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 0);
                    assert_eq!(ops.readback_calls, 0);
                }
                "registry-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 1);
                    assert_eq!(ops.readback_calls, 0);
                }
                "readback-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 1);
                    assert_eq!(ops.readback_calls, 1);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn socket_missing_hint_is_suppressed_only_for_selected_leader_env_transport() {
        assert!(!should_emit_workspace_socket_missing_hint(
            Some("leader_env"),
            true
        ));
        assert!(should_emit_workspace_socket_missing_hint(
            Some("workspace"),
            true
        ));
        assert!(should_emit_workspace_socket_missing_hint(None, true));
        assert!(!should_emit_workspace_socket_missing_hint(
            Some("workspace"),
            false
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn failed_canonical_readback_removes_new_registry_bytes_and_preserves_dual_state_bytes() {
        let workspace = ambient_dual_state_workspace("ambient-readback-rollback");
        let home = workspace.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = HomeGuard::set(&home);
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        let before = std::fs::read(&state_path).unwrap();
        let registry_path = crate::leader::registry::registry_dir().unwrap().join(format!(
            "{}__fresh.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        let mut ops = AmbientRegistryOps {
            readback_ok: false,
            attach_calls: 0,
        };
        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(std::fs::read(state_path).unwrap(), before);
        assert!(!registry_path.exists(), "failed readback left registry bytes");
    }
}
