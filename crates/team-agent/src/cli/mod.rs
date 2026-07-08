//! step 14b · cli — `team-agent <subcommand>` clap 命令面(真相源 `cli/`)。
//!
//! Card: `docs/phase0/subsystems/14-mcp_cli.md`(CLI 半边)。
//! Python 真相源(team-agent-public @ v0.2.11, 439bef8):
//!   - `cli/parser.py`     — argparse 顶层:`main(argv)`、`codex`/`claude` passthrough 早返回、
//!                            约 40 子命令注册、`func(args)` + 统一异常→`_emit_cli_error`+`SystemExit(1)`、
//!                            `consume_leader_inbox_summary` 命令后吐 leader fallback inbox 摘要、
//!                            `TeamAgentArgumentParser.error` 给 send 加顺序提示。
//!   - `cli/commands.py`   — 每子命令一个 `cmd_*`(薄壳),含逻辑的:`cmd_status`(--summary/--json/--detail
//!                            三态互斥 + 五行 summary 渲染)、`cmd_doctor`(gate/comms/fix-schema/cleanup-orphans 分派)。
//!   - `cli/helpers.py`    — `emit`(--json vs 人读)、`_emit_cli_error`/`_cli_error_payload`(错误落
//!                            `.team/logs/cli-error-<ts>.log` + tmux session 冲突富化)、`_provider_args`/
//!                            `_leader_launcher_args`(`--`/`--attach`/`--attach-session` 解析)、
//!                            `consume_leader_inbox_summary`(游标 + 字节预算截断的 fallback inbox 摘要)。
//!
//! 本子系统是"最薄的壳":几乎不拥有耐久数据,subcommand 全部委派给 step 5/6/7/11/12/13。
//! 自身只拥有:CLI 参数形状、`--json` 稳定输出形状、错误信封 + 退出码、五行 triage 渲染规则、
//! leader inbox 摘要游标 + 字节预算截断。
//!
//! §10/§12:本层是 bin 边界,**非** daemon/coordinator/lifecycle,故顶层**不**强加
//! `#![deny(unwrap/expect/panic)]`(leader 集成时不会给本文件加 deny)。CLI 顶层错误最终
//! 用 `anyhow`(bin main),但本 lib-side surface 用 `thiserror` 的 [`CliError`] 返回。
//!
//! 所有 fn body = `unimplemented!("step14b port: ...")`。RED 契约据此 NAME 类型 + CALL 真 fn。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/param/大 Err 暂未落地;P2 porter 实现时移除。
#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    clippy::result_large_err,
    clippy::doc_overindented_list_items,
    clippy::doc_lazy_continuation,
    clippy::io_other_error
)]
// §10:CLI 命令实现层禁 unwrap/expect/panic(unimplemented!() stub 不被拦);tests 子模块各自 allow。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;

// REUSE in-tree(只 import,不 redefine):
use crate::messaging::{self, AlertType, MessageTarget, SendOptions};
use crate::model::ids::{TaskId, TeamKey};

pub(crate) const COMMS_BOUNDARY_TEXT: &str = "validates live pane binding consistency and zero-token comms contracts. Does NOT perform live runtime message round-trip. (zero token, zero pollution)";
pub(crate) const QUICK_START_REMINDER: &str = "Reminder: Do not inspect raw worker terminal output during normal operation. Use team-agent status / inbox / collect instead. Wait for report_result.";
pub(crate) const SEND_REMINDER: &str = "Message delivered. Wait for the worker to report_result. Do not poll the worker terminal with capture-pane.";
pub(crate) const STATUS_REMINDER: &str = "To wait for results use --watch-result or team-agent collect. Do not capture-pane worker terminals.";

pub mod adapters;
pub mod attach_app_server_leader;
pub mod diagnose;
pub mod emit;
pub mod helpers;
pub mod leader;
pub mod leaders;
pub mod named_address;
pub mod profile;
pub mod send;
pub mod status;
pub mod types;

pub use adapters::*;
pub use attach_app_server_leader::*;
pub use diagnose::*;
pub use emit::*;
pub use leader::*;
pub use leaders::*;
pub use named_address::*;
pub use profile::*;
pub use send::*;
pub use status::*;
pub use types::*;

/// Public `attach-leader` CLI handler. It consumes the typed pane/provider args and
/// writes/returns a `leader_receiver` binding via the leader lease port.
pub fn cmd_attach_leader(args: &AttachLeaderArgs) -> Result<CmdResult, CliError> {
    let mut value = leader_port::attach_leader(
        &args.workspace,
        args.team.as_deref(),
        args.pane.as_ref(),
        args.provider,
        args.confirm,
    )?;
    if let Some(obj) = value.as_object_mut() {
        obj.entry("leader_receiver".to_string())
            .or_insert(Value::Null);
    }
    Ok(CmdResult::from_json(value, args.json))
}

pub(crate) use helpers::*;

#[cfg(test)]
mod tests;

// =============================================================================
// CROSS-LANE PLACEHOLDERS(sibling 14a-mcp / status / diagnose / step13-lifecycle
// 尚未落地;leader 集成时收口到真模块。本层只声明 CLI 调用面所需的最小占位,
// **不猜** sibling 内部命名 —— 见 cross_deps_or_placeholders)。
// =============================================================================

/// PLACEHOLDER → status lane(`status/queries.py`/`compact.py`)。`cmd_status`/`cmd_approvals`/
/// `cmd_inbox` 委派的只读投影面。返回 serde `Value`(稳定 JSON 形状由 status lane 拥有)。
pub mod status_port;

/// PLACEHOLDER → step13 lifecycle(`runtime.{quick_start,start_agent,add_agent,fork_agent,
/// remove_agent,start_agent,stop_agent,reset_agent,restart,shutdown,start_leader,acknowledge_idle}`)。
/// `quick_start.py` 物理在本子系统但实现属 step 13(card)。本层只声明委派面。
pub mod lifecycle_port {
    use super::*;
    use crate::model::enums::Provider;

    /// `runtime.quick_start`(`cmd_quick_start` 委派)。返回 `{ok, summary, ...}` 稳定形状。
    pub fn quick_start(
        workspace: &Path,
        agents_dir: &Path,
        name: Option<&str>,
        team_id: Option<&str>,
        yes: bool,
        open_display: bool,
        backend: Option<&str>,
    ) -> Result<Value, CliError> {
        match crate::lifecycle::quick_start_in_workspace_with_display_and_backend(
            workspace,
            agents_dir,
            name,
            yes,
            team_id,
            open_display,
            backend,
        ) {
            Ok(report) => Ok(quick_start_value(report)),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.start_leader`(`codex`/`claude` passthrough + `cmd_codex`/`cmd_claude`)。
    pub fn start_leader(
        provider: Provider,
        provider_args: &[String],
        cwd: &Path,
        attach: &LeaderLauncherArgs,
    ) -> Result<Value, CliError> {
        let attach_session = attach
            .attach_session
            .as_ref()
            .map(|name| crate::transport::SessionName::new(name.clone()));
        let plan = crate::leader::start::leader_start_plan(
            provider,
            provider_args,
            cwd,
            attach.attach_existing,
            attach.confirm_attach,
            attach_session.as_ref(),
            attach.external_leader,
        )
        .map_err(|e| CliError::Runtime(e.to_string()))?;
        let outcome = crate::leader::start::execute_leader_plan(&plan, cwd)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let ok = match outcome.status {
            crate::leader::LeaderLaunchStatus::Exited => outcome.exit_code == Some(0),
            crate::leader::LeaderLaunchStatus::Detached => true,
            crate::leader::LeaderLaunchStatus::NotStarted => false,
        };
        let leader_attach_command = leader_attach_command_for_plan(cwd, &plan);
        Ok(json!({
            "ok": ok,
            "provider": provider,
            "mode": plan.mode,
            "leader_topology": if plan.is_external_leader { "external" } else { "managed" },
            "is_external_leader": plan.is_external_leader,
            "leader_window": plan.leader_window.as_ref().map(|window| window.as_str().to_string()),
            "leader_attach_command": leader_attach_command,
            "status": outcome.status,
            "exit_code": outcome.exit_code,
            "reason": outcome.reason,
            "attach_existing": attach.attach_existing,
            "confirm_attach": attach.confirm_attach,
            "attach_session": attach.attach_session,
            "session_name": plan.session_name.as_ref().map(|session| session.as_str().to_string()),
        }))
    }

    pub(crate) fn leader_attach_command_for_plan(
        cwd: &Path,
        plan: &crate::leader::LeaderStartPlan,
    ) -> Option<String> {
        if plan.is_external_leader {
            return None;
        }
        let session = plan.session_name.as_ref()?;
        let window = plan.leader_window.as_ref()?;
        crate::tmux_backend::attach_command_for_workspace(cwd, session, window.as_str())
    }
    /// `runtime.shutdown`(`cmd_shutdown`)。
    ///
    /// 0.5.x Phase 1d Batch 5: the workspace-transport branch now routes
    /// through `transport_factory::resolve_read_only_transport` so a
    /// conpty team's shutdown reaches the shim rather than a
    /// no-op tmux backend. Legacy tmux endpoint literal in state still
    /// takes the `shutdown_transport_for_endpoint` tmux channel helper
    /// path (intentional — this is the "attached explicit tmux
    /// endpoint" branch and stays tmux-typed).
    ///
    /// `shutdown_with_transport_and_state` is generic over `&dyn
    /// Transport`, so we hand the boxed factory backend down as
    /// `&*box`. Factory refusal falls back to the workspace tmux
    /// backend byte-equivalent to today so daemon liveness paths
    /// don't crash.
    pub fn shutdown(
        workspace: &Path,
        keep_logs: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let run_ws = crate::model::paths::canonical_run_workspace(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let state = shutdown_state_for_team(&run_ws, team)?;
        if let Some(endpoint) = legacy_worker_tmux_endpoint(&state) {
            let transport = shutdown_transport_for_endpoint(endpoint);
            return shutdown_with_transport_and_state(
                workspace,
                keep_logs,
                team,
                &transport,
                Some(state),
            );
        }
        let boxed: Box<dyn crate::transport::Transport> =
            match crate::transport_factory::resolve_read_only_transport(
                &run_ws,
                Some(&state),
                crate::transport_factory::TransportPurpose::Shutdown,
            ) {
                Ok(r) => r.backend,
                Err(_) => Box::new(shutdown_workspace_transport(&run_ws)),
            };
        shutdown_with_transport_and_state(workspace, keep_logs, team, boxed.as_ref(), Some(state))
    }

    /// E12 ①:从 state 锚 pane_id(leader_receiver/team_owner,top+teams)映射到其所在 session
    /// (经同一帧 list_targets pane→session)。state 无任何锚 → 退命名判据 + spare_fallback event。
    fn anchor_sessions_from_state(
        state: &Value,
        pane_targets: &[crate::transport::PaneInfo],
        event_log: &crate::event_log::EventLog,
    ) -> std::collections::BTreeSet<String> {
        let anchor_pane_ids = collect_state_leader_anchor_pane_ids(state);
        if anchor_pane_ids.is_empty() {
            // 无锚(state 损坏/未记)→ 退纯命名前缀判据(下游 sessions_to_kill 仍 spare 前缀)。
            let _ = event_log.write(
                "shutdown.spare_fallback_to_naming",
                json!({"reason": "no leader_receiver/team_owner pane anchor in state"}),
            );
            return std::collections::BTreeSet::new();
        }
        pane_targets
            .iter()
            .filter(|pane| anchor_pane_ids.contains(pane.pane_id.as_str()))
            .map(|pane| pane.session.as_str().to_string())
            .collect()
    }

    fn socket_session_names_from_targets(
        pane_targets: &[crate::transport::PaneInfo],
    ) -> Vec<crate::transport::SessionName> {
        let mut seen = std::collections::BTreeSet::new();
        pane_targets
            .iter()
            .map(|pane| pane.session.clone())
            .filter(|session| seen.insert(session.as_str().to_string()))
            .collect()
    }

    /// E12 下沉纯函数:bare-shutdown socket 拆除决策。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum KillDecision {
        /// socket 独享(无 spare、无外来 session)→ 可整 server 拆除。
        KillServerExclusive,
        /// 有 spare(leader 锚/前缀)或非独享 → 逐 session kill,绝不 kill-server。
        KillIndividually {
            to_kill: Vec<crate::transport::SessionName>,
            spared: Vec<crate::transport::SessionName>,
        },
    }

    /// E12 纯决策(单测下沉):spare = `anchor_sessions` ∪ `team-agent-leader-*` 前缀(并集,锚优先)。
    /// 全部 session 都不 spare 且非空 → `KillServerExclusive`(独享 socket 兜底);否则逐 session
    /// kill 非 spare 的(共享 socket / leader 在 → 绝不整 server 拆)。空 session 集 → 逐 kill(no-op)。
    pub(crate) fn sessions_to_kill(
        sessions: &[crate::transport::SessionName],
        anchor_sessions: &std::collections::BTreeSet<String>,
    ) -> KillDecision {
        let is_spared = |s: &crate::transport::SessionName| {
            s.as_str().starts_with(crate::leader::LEADER_SESSION_PREFIX)
                || anchor_sessions.contains(s.as_str())
        };
        let spared: Vec<_> = sessions.iter().filter(|s| is_spared(s)).cloned().collect();
        let to_kill: Vec<_> = sessions.iter().filter(|s| !is_spared(s)).cloned().collect();
        // 独享 = 非空 + 无 spare(socket 上每个 session 都是要 kill 的我方 session)。
        if spared.is_empty() && !sessions.is_empty() {
            KillDecision::KillServerExclusive
        } else {
            KillDecision::KillIndividually { to_kill, spared }
        }
    }

    #[derive(Debug, Default)]
    struct ShutdownSocketCleanup {
        killed_sessions: Vec<crate::transport::SessionName>,
        spared_sessions: Vec<crate::transport::SessionName>,
        error: Option<String>,
    }

    #[derive(Debug)]
    struct OwnedShutdownEndpoint {
        endpoint: String,
        socket_file: Option<PathBuf>,
    }

    #[derive(Debug, Default)]
    struct OwnedEndpointCleanup {
        residual_files: Vec<String>,
        removed_files: Vec<String>,
        skipped_files: Vec<String>,
        remaining_sessions: Vec<String>,
        error: Option<String>,
    }

    fn push_unique_session(
        sessions: &mut Vec<crate::transport::SessionName>,
        session: crate::transport::SessionName,
    ) {
        if !sessions
            .iter()
            .any(|existing| existing.as_str() == session.as_str())
        {
            sessions.push(session);
        }
    }

    fn bare_shutdown_socket_cleanup(
        transport: &dyn crate::transport::Transport,
        state: &Value,
        event_log: &crate::event_log::EventLog,
    ) -> ShutdownSocketCleanup {
        // E12 (P0): the leader terminal lives on this socket by design. A bare shutdown must
        // NOT `kill-server` it away. spare = state-anchor sessions ∪ `team-agent-leader-*`
        // prefix sessions (union; cr E12 ①). kill_server only when the socket is exclusively
        // ours (no spare + no foreign session); shared socket → kill our sessions individually
        // (cr E12 ②). All spare derivation comes from ONE snapshot (list_targets + state) —
        // no independent ps/tmux re-derivation (N39).
        let pane_targets = transport.list_targets().unwrap_or_default();
        let sessions = socket_session_names_from_targets(&pane_targets);
        if !state_uses_external_leader(state) {
            return managed_leader_socket_cleanup(transport, state, &sessions, event_log);
        }
        let anchor_sessions = anchor_sessions_from_state(state, &pane_targets, event_log);
        match sessions_to_kill(&sessions, &anchor_sessions) {
            KillDecision::KillServerExclusive => {
                if state.get("tmux_socket_source").and_then(Value::as_str) == Some("leader_env") {
                    let _ = event_log.write(
                        "shutdown.kill_server_skipped_shared_socket",
                        json!({
                            "reason": "leader_env_tmux_socket",
                            "spared_sessions": [],
                            "killed_sessions": sessions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                        }),
                    );
                    let mut error = None;
                    for session in &sessions {
                        if let Err(err) = transport.kill_session(session) {
                            if !tmux_absent_error(&err.to_string()) {
                                error.get_or_insert_with(|| err.to_string());
                            }
                        }
                    }
                    return ShutdownSocketCleanup {
                        killed_sessions: sessions,
                        spared_sessions: Vec::new(),
                        error,
                    };
                }
                let error = transport.kill_server().err().map(|error| error.to_string());
                ShutdownSocketCleanup {
                    killed_sessions: sessions,
                    spared_sessions: Vec::new(),
                    error,
                }
            }
            KillDecision::KillIndividually { to_kill, spared } => {
                if !spared.is_empty() || to_kill.len() != sessions.len() {
                    // shared socket / leader spared → never whole-server teardown.
                    let _ = event_log.write(
                        "shutdown.kill_server_skipped_shared_socket",
                        json!({
                            "spared_sessions": spared.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                            "killed_sessions": to_kill.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                        }),
                    );
                }
                let mut error = None;
                for session in &to_kill {
                    if let Err(err) = transport.kill_session(session) {
                        if !tmux_absent_error(&err.to_string()) {
                            error.get_or_insert_with(|| err.to_string());
                        }
                    }
                }
                ShutdownSocketCleanup {
                    killed_sessions: to_kill,
                    spared_sessions: spared,
                    error,
                }
            }
        }
    }

    fn state_uses_external_leader(state: &Value) -> bool {
        crate::state::projection::state_is_external_leader(state)
    }

    fn managed_leader_socket_cleanup(
        transport: &dyn crate::transport::Transport,
        state: &Value,
        sessions: &[crate::transport::SessionName],
        event_log: &crate::event_log::EventLog,
    ) -> ShutdownSocketCleanup {
        let target = state
            .get("session_name")
            .and_then(Value::as_str)
            .filter(|session| !session.is_empty())
            .map(crate::transport::SessionName::new);
        // E49 (0.3.24 P0, shutdown kills leader CLI): for managed leader topology
        // the target session may carry the leader anchor pane. The pre-fix code
        // pushed it into `to_kill` and then issued `kill_session` — which ended
        // the leader pane (= leader CLI). When the target session has a live
        // anchor pane in `list_targets`, spare it instead.
        let leader_anchor_ids = collect_state_leader_anchor_pane_ids(state);
        let live_targets = transport.list_targets().unwrap_or_default();
        let mut to_kill = Vec::new();
        let mut target_spared_for_anchor: Option<crate::transport::SessionName> = None;
        if let Some(target) = target {
            let target_has_anchor = live_targets.iter().any(|t| {
                t.session.as_str() == target.as_str()
                    && leader_anchor_ids.contains(t.pane_id.as_str())
            });
            if target_has_anchor {
                target_spared_for_anchor = Some(target);
            } else if sessions.is_empty()
                || sessions
                    .iter()
                    .any(|session| session.as_str() == target.as_str())
            {
                to_kill.push(target);
            }
        }
        let mut spared = sessions
            .iter()
            .filter(|session| {
                !to_kill
                    .iter()
                    .any(|target| target.as_str() == session.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(anchored) = target_spared_for_anchor {
            if !spared.iter().any(|s| s.as_str() == anchored.as_str()) {
                spared.push(anchored);
            }
        }
        let _ = event_log.write(
            "shutdown.kill_server_skipped_managed_leader",
            json!({
                "reason": "managed_leader_topology",
                "spared_sessions": spared.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "killed_sessions": to_kill.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            }),
        );
        let mut error = None;
        for session in &to_kill {
            // 0.3.28 Step 6 (warn-only invariant): the kill list MUST NOT
            // contain a leader session. `sessions_to_kill` already excludes
            // leader-prefixed sessions, but the managed-leader cleanup path
            // (this function) computes its own `to_kill` from `target`. Any
            // leader-prefixed entry here is a topology violation introduced
            // somewhere upstream. Log loudly and skip the kill.
            if session
                .as_str()
                .starts_with(crate::leader::LEADER_SESSION_PREFIX)
            {
                eprintln!(
                    "team_agent::layout shutdown_invariant_violation kind=KillListContainsLeaderSession \
                     session=`{}` action=skipping_kill (post-Step-9 will hard-fail)",
                    session.as_str()
                );
                continue;
            }
            if let Err(err) = transport.kill_session(session) {
                if !tmux_absent_error(&err.to_string()) {
                    error.get_or_insert_with(|| err.to_string());
                }
            }
        }
        ShutdownSocketCleanup {
            killed_sessions: to_kill,
            spared_sessions: spared,
            error,
        }
    }

    fn owned_shutdown_endpoint(
        workspace: &Path,
        state: &Value,
        transport: &dyn crate::transport::Transport,
    ) -> Option<OwnedShutdownEndpoint> {
        if state_uses_external_leader(state) {
            return None;
        }
        if state.get("tmux_socket_source").and_then(Value::as_str) == Some("leader_env") {
            return None;
        }
        let endpoint = legacy_worker_tmux_endpoint(state)
            .map(str::to_string)
            .or_else(|| transport.tmux_endpoint())
            .unwrap_or_else(|| crate::tmux_backend::socket_name_for_workspace(workspace));
        if endpoint.is_empty() || endpoint == "default" {
            return None;
        }
        let workspace_socket = crate::tmux_backend::socket_name_for_workspace(workspace);
        let source = state.get("tmux_socket_source").and_then(Value::as_str);
        let owned = source == Some("workspace")
            || source.is_none()
                && endpoint_matches_workspace_socket(&endpoint, workspace, &workspace_socket);
        if !owned {
            return None;
        }
        let socket_file = socket_file_for_endpoint(&endpoint);
        Some(OwnedShutdownEndpoint {
            endpoint,
            socket_file,
        })
    }

    fn endpoint_matches_workspace_socket(
        endpoint: &str,
        workspace: &Path,
        workspace_socket: &str,
    ) -> bool {
        if endpoint == workspace_socket {
            return true;
        }
        let Some(workspace_path) = crate::tmux_backend::socket_path_for_workspace(workspace) else {
            return false;
        };
        Path::new(endpoint) == workspace_path
    }

    fn socket_file_for_endpoint(endpoint: &str) -> Option<PathBuf> {
        if Path::new(endpoint).is_absolute() {
            Some(PathBuf::from(endpoint))
        } else {
            crate::tmux_backend::socket_path_for_name(endpoint)
        }
    }

    fn cleanup_owned_empty_endpoint(
        transport: &dyn crate::transport::Transport,
        endpoint: &OwnedShutdownEndpoint,
        event_log: &crate::event_log::EventLog,
    ) -> OwnedEndpointCleanup {
        let mut cleanup = OwnedEndpointCleanup::default();
        cleanup.remaining_sessions = transport
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .map(|pane| pane.session.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if !cleanup.remaining_sessions.is_empty() {
            if let Some(path) = endpoint.socket_file.as_ref() {
                cleanup.skipped_files.push(path.display().to_string());
            }
            let _ = event_log.write(
                "shutdown.owned_endpoint_cleanup_skipped",
                json!({
                    "endpoint": endpoint.endpoint,
                    "reason": "sessions_remain",
                    "remaining_sessions": cleanup.remaining_sessions.clone(),
                    "skipped_files": cleanup.skipped_files.clone(),
                }),
            );
            return cleanup;
        }
        if let Err(error) = transport.kill_server() {
            cleanup.error = Some(error.to_string());
        }
        if let Some(path) = endpoint.socket_file.as_ref() {
            if path.exists() {
                match std::fs::remove_file(path) {
                    Ok(()) => cleanup.removed_files.push(path.display().to_string()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        cleanup.error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
            if path.exists() {
                cleanup.residual_files.push(path.display().to_string());
            }
        }
        let _ = event_log.write(
            "shutdown.owned_endpoint_cleanup",
            json!({
                "endpoint": endpoint.endpoint,
                "removed_files": cleanup.removed_files.clone(),
                "residual_files": cleanup.residual_files.clone(),
                "error": cleanup.error.clone(),
            }),
        );
        cleanup
    }

    pub fn shutdown_with_transport(
        workspace: &Path,
        keep_logs: bool,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
    ) -> Result<Value, CliError> {
        shutdown_with_transport_and_state(workspace, keep_logs, team, transport, None)
    }

    fn shutdown_with_transport_and_state(
        workspace: &Path,
        keep_logs: bool,
        team: Option<&str>,
        transport: &dyn crate::transport::Transport,
        state: Option<Value>,
    ) -> Result<Value, CliError> {
        crate::os_probe::clear_probe_timeout();
        let deadline = ShutdownDeadline::new(std::time::Duration::from_secs(20));
        let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let _started_event = crate::event_log::EventLog::new(&run_workspace)
            .write(
                "lifecycle.shutdown.started",
                json!({
                    "keep_logs": keep_logs,
                    "team": team,
                }),
            )
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut state = match state {
            Some(state) => state,
            None => shutdown_state_for_team(&run_workspace, team)?,
        };
        deadline.check("refresh_provider_sessions")?;
        let captured_missing_sessions =
            crate::lifecycle::restart::refresh_missing_provider_sessions(&mut state)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
        let session_name = state
            .get("session_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(crate::transport::SessionName::new);
        // PERF-6 C-①-1: ONE process-table snapshot for the whole happy path; the
        // protected / pgid / kill / wait sets all derive from it (N39 same-source).
        // A probe failure is observable, not a silent empty table (swallow batch 1).
        let mut probe_degraded = false;
        let entry_table = shutdown_table_snapshot(&run_workspace, &mut probe_degraded, "entry");
        let mut protected = shutdown_protection_set(&entry_table);
        extend_protection_with_leader_panes(&mut protected, transport, &state, &entry_table);
        let protected = protected;
        let reap_scope = if team.is_some() {
            ShutdownReapScope::ScopedTeam
        } else {
            ShutdownReapScope::Workspace
        };
        deadline.check("process_roots")?;
        let mut root_pids = state_process_roots(&state, reap_scope)
            .into_iter()
            .filter(|pid| !protected.contains_pid(*pid))
            .collect::<Vec<_>>();
        let pane_pids = session_name
            .as_ref()
            .map(|session| {
                pane_pids_for_session(transport, session)
                    .into_iter()
                    .filter(|pid| !protected.contains_pid(*pid))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        root_pids.extend(pane_pids);
        root_pids.sort_unstable();
        root_pids.dedup();
        let root_pgids = process_pgids(&root_pids, &protected, &entry_table);
        deadline.check("reap_process_tree")?;
        reap_process_tree(&root_pids, &protected, &entry_table);
        reap_process_groups(&root_pgids, &protected);
        let mut kill_error: Option<String> = None;
        let mut killed_sessions = Vec::new();
        let mut spared_sessions = Vec::new();
        deadline.check("kill_session")?;
        if let Some(session) = session_name.as_ref() {
            // E49 (0.3.24 P0, shutdown kills leader CLI): managed-leader topology
            // means the leader pane lives INSIDE state.session_name. The pre-fix
            // `transport.kill_session(state.session_name)` ended the leader pane —
            // i.e. terminated the user's own CLI. User truth: shutdown must never
            // close the leader CLI that launched it. Detect via state.is_external_leader
            // (state_is_managed_leader = !external) AND the presence of a leader
            // anchor pane id (leader_receiver / team_owner) that lives in this
            // session per `list_targets`. When managed-leader, switch to per-pane
            // cleanup: kill the workers individually and SPARE the leader's pane.
            //
            // External leader (is_external_leader=true) keeps the unconditional
            // kill_session — the team session is a disposable worker session in
            // that topology, the leader pane lives elsewhere.
            let leader_anchor_ids = collect_state_leader_anchor_pane_ids(&state);
            let live_targets_now = transport.list_targets().unwrap_or_default();
            let session_has_leader_anchor = live_targets_now.iter().any(|target| {
                target.session.as_str() == session.as_str()
                    && leader_anchor_ids.contains(target.pane_id.as_str())
            });
            if crate::state::projection::state_is_managed_leader(&state)
                && session_has_leader_anchor
            {
                let _ = event_log_write_session_spared(&run_workspace, session, &leader_anchor_ids);
                push_unique_session(&mut spared_sessions, session.clone());
                let worker_panes = collect_session_worker_panes(
                    &state,
                    session.as_str(),
                    &leader_anchor_ids,
                    &live_targets_now,
                );
                for pane in &worker_panes {
                    if let Err(error) = transport.kill_pane(pane) {
                        if !tmux_absent_error(&error.to_string()) {
                            kill_error.get_or_insert_with(|| error.to_string());
                        }
                    }
                }
                let _ =
                    crate::lifecycle::display::close_team_display_backends(&run_workspace, session);
            } else {
                push_unique_session(&mut killed_sessions, session.clone());
                if let Err(error) = transport.kill_session(session) {
                    if !tmux_absent_error(&error.to_string()) {
                        kill_error = Some(error.to_string());
                    }
                }
                let _ =
                    crate::lifecycle::display::close_team_display_backends(&run_workspace, session);
            }
        }
        deadline.check("reap_workspace_residuals")?;
        reap_workspace_process_residuals(
            &run_workspace,
            &state,
            &root_pids,
            &root_pgids,
            transport,
            reap_scope,
            &mut probe_degraded,
        );
        if team.is_none() {
            deadline.check("shared_socket_cleanup")?;
            let event_log = crate::event_log::EventLog::new(&run_workspace);
            let cleanup = bare_shutdown_socket_cleanup(transport, &state, &event_log);
            for session in cleanup.killed_sessions {
                push_unique_session(&mut killed_sessions, session);
            }
            spared_sessions = cleanup.spared_sessions;
            if let Some(error) = cleanup.error {
                kill_error.get_or_insert(error);
            }
        }
        deadline.check("session_residuals")?;
        let (session_residuals, session_residual_error) = session_residuals_after_reap_many(
            transport,
            &run_workspace,
            &killed_sessions,
            !captured_missing_sessions,
        );
        if let Some(error) = session_residual_error {
            kill_error.get_or_insert(error);
        }
        let event_log = crate::event_log::EventLog::new(&run_workspace);
        let owned_endpoint = owned_shutdown_endpoint(&run_workspace, &state, transport);
        let owned_cleanup = owned_endpoint
            .as_ref()
            .map(|endpoint| cleanup_owned_empty_endpoint(transport, endpoint, &event_log))
            .unwrap_or_default();
        if let Some(error) = owned_cleanup.error.as_ref() {
            kill_error.get_or_insert_with(|| error.clone());
        }
        let owned_file_residuals = owned_cleanup
            .residual_files
            .iter()
            .map(|path| json!({ "path": path }))
            .collect::<Vec<_>>();
        deadline.check("process_residuals")?;
        // C-①: the post-verify gets ONE fresh verification snapshot (reaps changed
        // the world; #248 post-verify facts must be current, not the entry view).
        let verify_table =
            shutdown_table_snapshot(&run_workspace, &mut probe_degraded, "post_verify");
        let process_residuals = process_residuals(
            &run_workspace,
            &state,
            &root_pids,
            &root_pgids,
            &protected,
            reap_scope,
            &verify_table,
        );
        deadline.check("stop_coordinator")?;
        let mut coordinator_timeout = false;
        let mut coordinator_post_stop = CoordinatorStopObservation::NotNeeded;
        let mut coordinator_pid_for_report = None;
        let stopped = if team.is_none() {
            let wp = crate::coordinator::WorkspacePath::new(run_workspace.clone());
            let coordinator_pid_before_stop = crate::coordinator::coordinator_health(&wp).pid;
            coordinator_pid_for_report = coordinator_pid_before_stop.map(|pid| pid.get());
            match stop_coordinator_bounded(wp, std::time::Duration::from_millis(900)) {
                Some(Ok(report)) => Some(report),
                Some(Err(error)) => {
                    kill_error.get_or_insert(error);
                    None
                }
                None => {
                    coordinator_timeout = true;
                    let wp = crate::coordinator::WorkspacePath::new(run_workspace.clone());
                    coordinator_post_stop =
                        coordinator_post_stop_observation(&wp, coordinator_pid_before_stop);
                    None
                }
            }
        } else {
            None
        };
        if let Some(stopped) = stopped.as_ref().filter(|stopped| !stopped.ok) {
            let wp = crate::coordinator::WorkspacePath::new(run_workspace.clone());
            coordinator_post_stop = coordinator_post_stop_observation(&wp, stopped.pid);
        }
        let probe_timeout = crate::os_probe::probe_timeout();
        let probe_timeout_kind = probe_timeout.as_ref().map(|timeout| timeout.probe);
        // swallow batch 1: a failed ps probe degrades cleanup truthfully — the
        // empty table must never read as a clean "no residual processes". A slow
        // per-process cwd probe is diagnostic only once session/process residuals
        // are otherwise clean.
        let cleanup_truth_degraded = probe_degraded || probe_timeout_kind == Some("ps_table");
        let diagnostic_probe_degraded = probe_timeout_kind == Some("lsof_cwd");
        let other_probe_timeout_degraded =
            probe_timeout.is_some() && !cleanup_truth_degraded && !diagnostic_probe_degraded;
        let verification_degraded =
            cleanup_truth_degraded || diagnostic_probe_degraded || other_probe_timeout_degraded;
        let session_killed = !killed_sessions.is_empty()
            && kill_error.is_none()
            && session_residuals.is_empty()
            && process_residuals.is_empty()
            && owned_file_residuals.is_empty();
        mark_agents_stopped(&mut state);
        deadline.check("save_state")?;
        if team.is_some() {
            crate::state::projection::save_team_scoped_state(&run_workspace, &state)?;
            promote_live_sibling_after_scoped_shutdown(&run_workspace, &state)?;
        } else {
            let _changed_keys =
                mark_matching_session_teams_stopped(&mut state, session_name.as_ref());
            crate::state::persist::save_runtime_state(&run_workspace, &state)?;
        }
        let coordinator_status = if coordinator_timeout {
            "timeout"
        } else {
            stopped
                .as_ref()
                .map(|stopped| stop_status_wire(stopped.status))
                .unwrap_or("not_stopped")
        };
        let coordinator_pid = stopped
            .as_ref()
            .and_then(|stopped| stopped.pid.map(|p| p.get()));
        let coordinator_pid = coordinator_pid.or(coordinator_pid_for_report);
        // unit-2 (Stage 1) false-green guard fact:
        //   target_session_spared = "we had a target worker session name AND
        //                            that exact name appears in spared_sessions"
        // Mirrors the "we asked to kill X but X is still alive" check the
        // 0.3.39 false-green bug missed. session_name is the configured
        // target; spared_sessions is the cleanup's "kept alive" list.
        let target_session_spared = match session_name.as_ref() {
            Some(target) => spared_sessions
                .iter()
                .any(|s| s.as_str() == target.as_str()),
            None => false,
        };
        let outcome = classify_shutdown_outcome(ShutdownOutcomeInput {
            kill_error: kill_error.is_some(),
            session_residuals: !session_residuals.is_empty(),
            process_residuals: !process_residuals.is_empty(),
            owned_file_residuals: !owned_file_residuals.is_empty(),
            cleanup_truth_degraded,
            coordinator_timeout,
            coordinator_stop_ok: stopped.as_ref().map(|stopped| stopped.ok),
            coordinator_post_stop,
            target_session_spared,
        });
        let ok = outcome.ok;
        let status = outcome.status;
        let phase = outcome.phase;
        let probe_timeout_value = probe_timeout.as_ref().map(|timeout| {
            json!({
                "probe": timeout.probe,
                "pid": timeout.pid,
                "timeout_ms": timeout.timeout_ms,
            })
        });
        let _event = crate::event_log::EventLog::new(&run_workspace)
            .write(
                "lifecycle.shutdown",
                json!({
                    "keep_logs": keep_logs,
                    "team": team,
                    "session_name": session_name.as_ref().map(|s| s.as_str().to_string()),
                    "session_killed": session_killed,
                    "killed_sessions": killed_sessions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    "spared_sessions": spared_sessions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    "coordinator_status": coordinator_status,
                    "status": status,
                    "phase": phase,
                    "verification_degraded": verification_degraded,
                    "probe_timeout_kind": probe_timeout_kind,
                    "probe_timeout": probe_timeout_value,
                    "owned_endpoint": owned_endpoint.as_ref().map(|endpoint| endpoint.endpoint.clone()),
                    "owned_files_removed": owned_cleanup.removed_files.clone(),
                    "owned_files_skipped": owned_cleanup.skipped_files.clone(),
                    "owned_file_residuals": owned_file_residuals.clone(),
                }),
            )
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        // 0.5.x Windows portability Batch 6 Option A (leader
        // constraint 3): if a ConPTY shim was recorded in
        // `state.transport.shim.pid`, terminate it explicitly via
        // `platform::process::terminate_pid`.
        //
        // Batch 7 refinement: the field is emitted into the shutdown
        // JSON only on Windows so Unix golden fixtures stay
        // byte-preserving. The Unix behavior is a no-op (no shim
        // concept there); adding a placeholder key would just noise
        // up the fixtures.
        let response = json!({
            "ok": ok,
            "status": status,
            "phase": phase,
            "verification_degraded": verification_degraded,
            "probe_degraded": probe_degraded,
            "probe_timeout_kind": probe_timeout_kind,
            "probe_timeout": probe_timeout_value,
            "keep_logs": keep_logs,
            "team": team,
            "session_name": session_name.map(|s| s.as_str().to_string()),
            "session_killed": session_killed,
            "killed_sessions": killed_sessions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "spared_sessions": spared_sessions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "residuals": {
                "sessions": session_residuals,
                "processes": process_residuals,
                "owned_files": owned_file_residuals,
            },
            "error": kill_error,
            "coordinator": {
                "status": coordinator_status,
                "pid": coordinator_pid,
            },
        });
        #[cfg(windows)]
        let mut response = response;
        #[cfg(windows)]
        {
            if let Some(obj) = response.as_object_mut() {
                obj.insert(
                    "conpty_shim".to_string(),
                    shutdown_conpty_shim(&run_workspace),
                );
            }
        }
        #[cfg(not(windows))]
        {
            let _ = &run_workspace;
        }
        // E7 unregister-after-success (host-leader-registry-design §14 step 6):
        // Only prune the derived registry entry when the canonical shutdown
        // succeeded and there was no dirty_state / verification degradation.
        // Failed/degraded shutdowns leave the entry STALE so operators can
        // decide.
        if ok && !verification_degraded && !probe_degraded {
            let _ = crate::cli::leader_port::unregister_after_shutdown_success(&run_workspace, team);
        }
        Ok(response)
    }

    /// 0.5.x Windows portability Batch 6 Option A: locate and
    /// terminate the ConPTY shim if `state.transport.shim.pid` is
    /// recorded. Returns a JSON blob for the shutdown response so
    /// operators can distinguish "no shim was ever launched" from
    /// "shim terminated cleanly" from "shim already gone".
    ///
    /// Windows-only; caller (`shutdown_with_transport_and_state`)
    /// only invokes on Windows. Unix has no shim concept.
    #[cfg(windows)]
    fn shutdown_conpty_shim(workspace: &Path) -> serde_json::Value {
        let Some(pid) = crate::coordinator::conpty_shim::recorded_shim_pid(workspace) else {
            return json!({ "action": "no_shim_recorded" });
        };
        // Prefer graceful termination — Windows maps this to
        // `TerminationOutcome::ForceOnly` (Batch 3 anchor: no
        // SIGTERM equivalent for non-console child), which
        // surfaces the honest audit signal in the JSON.
        let outcome = crate::platform::process::terminate_pid(
            pid,
            crate::platform::process::SignalKind::TerminateForce,
        );
        match outcome {
            Ok(crate::platform::process::TerminationOutcome::Requested) => {
                json!({ "pid": pid, "action": "terminated" })
            }
            Ok(crate::platform::process::TerminationOutcome::AlreadyGone) => {
                json!({ "pid": pid, "action": "already_gone" })
            }
            Ok(crate::platform::process::TerminationOutcome::ForceOnly { reason }) => {
                json!({ "pid": pid, "action": "terminated_force_only", "reason": reason })
            }
            Err(e) => {
                json!({ "pid": pid, "action": "terminate_failed", "reason": e.to_string() })
            }
        }
    }

    /// T5 (harvest §1 / A2): the bounded stop RETAINS the JoinHandle and reclaims the
    /// worker thread — on a timely result it joins immediately; on timeout it gives the
    /// thread one short grace join window instead of dropping it detached (repeated
    /// shutdowns no longer accumulate leaked threads racing the same workspace).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum CoordinatorStopObservation {
        NotNeeded,
        Gone,
        Running,
        Unknown,
    }

    pub(crate) struct ShutdownOutcome {
        pub(crate) ok: bool,
        pub(crate) status: &'static str,
        pub(crate) phase: Option<&'static str>,
    }

    pub(crate) struct ShutdownOutcomeInput {
        pub(crate) kill_error: bool,
        pub(crate) session_residuals: bool,
        pub(crate) process_residuals: bool,
        pub(crate) owned_file_residuals: bool,
        pub(crate) cleanup_truth_degraded: bool,
        pub(crate) coordinator_timeout: bool,
        pub(crate) coordinator_stop_ok: Option<bool>,
        pub(crate) coordinator_post_stop: CoordinatorStopObservation,
        /// unit-2 (Stage 1) false-green guard:
        /// `true` when the workspace had a target worker session name AND
        /// the cleanup did NOT confirm kill of that name (i.e. the target
        /// was spared, leaving the worker alive while ok/status:"ok" would
        /// otherwise be reported — the 0.3.39 shutdown false-green shape).
        ///
        /// Default `false` for callers that have no target (legacy bare
        /// shutdown on a not-yet-bound workspace) so the existing
        /// well-defined OK branch keeps working.
        pub(crate) target_session_spared: bool,
    }

    pub(crate) fn classify_shutdown_outcome(input: ShutdownOutcomeInput) -> ShutdownOutcome {
        let coordinator_clean = match input.coordinator_post_stop {
            CoordinatorStopObservation::Gone => true,
            CoordinatorStopObservation::Running | CoordinatorStopObservation::Unknown => false,
            CoordinatorStopObservation::NotNeeded => {
                !input.coordinator_timeout && input.coordinator_stop_ok.unwrap_or(true)
            }
        };
        let ok = coordinator_clean
            && !input.kill_error
            && !input.session_residuals
            && !input.process_residuals
            && !input.owned_file_residuals
            && !input.cleanup_truth_degraded
            // unit-2 false-green guard: refuse OK when the configured worker
            // session was spared (still alive). The user asked for shutdown,
            // not "shutdown but maybe keep the session" — make the disagreement
            // explicit instead of paint-it-green.
            && !input.target_session_spared;
        if ok {
            return ShutdownOutcome {
                ok,
                status: "ok",
                phase: None,
            };
        }
        let (status, phase) = if input.coordinator_timeout && !coordinator_clean {
            ("timeout", Some("stop_coordinator"))
        } else if input.cleanup_truth_degraded {
            ("partial", Some("os_probe"))
        } else if input.kill_error
            || input.session_residuals
            || input.process_residuals
            || input.owned_file_residuals
        {
            ("failed", None)
        } else if input.target_session_spared {
            // The worker session that was supposed to be killed survived
            // because the cleanup decision spared it (dirty topology — e.g.
            // 0.3.39 leader_receiver.session_name == worker session). Surface
            // a status that names the cause instead of fall-through "partial".
            ("dirty_state", Some("target_session_spared"))
        } else {
            ("partial", None)
        };
        ShutdownOutcome { ok, status, phase }
    }

    fn coordinator_post_stop_observation(
        workspace: &crate::coordinator::WorkspacePath,
        pid: Option<crate::coordinator::Pid>,
    ) -> CoordinatorStopObservation {
        if let Some(pid) = pid {
            match crate::coordinator::pid_is_running(pid) {
                Ok(true) => return CoordinatorStopObservation::Running,
                Ok(false) => return CoordinatorStopObservation::Gone,
                Err(_) => {}
            }
        }
        let health = crate::coordinator::coordinator_health(workspace);
        match health.status {
            crate::coordinator::CoordinatorHealthStatus::Running => {
                CoordinatorStopObservation::Running
            }
            crate::coordinator::CoordinatorHealthStatus::Missing
            | crate::coordinator::CoordinatorHealthStatus::InvalidPid
            | crate::coordinator::CoordinatorHealthStatus::Stale => {
                CoordinatorStopObservation::Gone
            }
        }
    }

    fn stop_coordinator_bounded(
        workspace: crate::coordinator::WorkspacePath,
        timeout: std::time::Duration,
    ) -> Option<Result<crate::coordinator::types::StopReport, String>> {
        stop_coordinator_bounded_with(workspace, timeout, |workspace| {
            crate::coordinator::stop_coordinator(workspace).map_err(|error| error.to_string())
        })
    }

    pub(crate) fn stop_coordinator_bounded_with<F>(
        workspace: crate::coordinator::WorkspacePath,
        timeout: std::time::Duration,
        stop: F,
    ) -> Option<Result<crate::coordinator::types::StopReport, String>>
    where
        F: FnOnce(
                &crate::coordinator::WorkspacePath,
            ) -> Result<crate::coordinator::types::StopReport, String>
            + Send
            + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = stop(&workspace);
            let _ = tx.send(result);
        });
        let outcome = rx.recv_timeout(timeout).ok();
        if outcome.is_some() {
            // The worker already sent its result; the join is immediate.
            let _ = handle.join();
            return outcome;
        }
        // Timeout: grant a short grace window for the worker to wind down, then join if
        // it finished; a still-stuck stop is reported as timeout either way (the grace
        // join keeps the common slightly-late case from leaking a detached thread).
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(late) => {
                let _ = handle.join();
                Some(late)
            }
            Err(_) => {
                if handle.is_finished() {
                    let _ = handle.join();
                }
                None
            }
        }
    }

    struct ShutdownDeadline {
        start: std::time::Instant,
        timeout: std::time::Duration,
    }

    impl ShutdownDeadline {
        fn new(timeout: std::time::Duration) -> Self {
            Self {
                start: std::time::Instant::now(),
                timeout,
            }
        }

        fn check(&self, phase: &'static str) -> Result<(), CliError> {
            if self.start.elapsed() >= self.timeout {
                return Err(CliError::Runtime(
                    json!({
                        "ok": false,
                        "status": "timeout",
                        "phase": phase,
                    })
                    .to_string(),
                ));
            }
            Ok(())
        }
    }

    fn shutdown_state_for_team(workspace: &Path, team: Option<&str>) -> Result<Value, CliError> {
        if let Some(team) = team {
            crate::state::projection::select_runtime_state(workspace, Some(team))
                .map_err(CliError::from)
        } else {
            crate::state::persist::load_runtime_state(workspace).map_err(CliError::from)
        }
    }

    fn shutdown_workspace_transport(workspace: &Path) -> crate::tmux_backend::TmuxBackend {
        crate::tmux_backend::TmuxBackend::for_workspace(workspace)
    }

    fn shutdown_transport_for_endpoint(endpoint: &str) -> crate::tmux_backend::TmuxBackend {
        if Path::new(endpoint).is_absolute() {
            crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint)
        } else {
            crate::tmux_backend::TmuxBackend::for_socket_name(endpoint)
        }
    }

    fn legacy_worker_tmux_endpoint(state: &Value) -> Option<&str> {
        state
            .get("tmux_endpoint")
            .and_then(Value::as_str)
            .or_else(|| state.get("tmux_socket").and_then(Value::as_str))
            .filter(|endpoint| !endpoint.is_empty())
    }

    fn pane_pids_for_session(
        transport: &dyn crate::transport::Transport,
        session: &crate::transport::SessionName,
    ) -> Vec<u32> {
        transport
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .filter(|pane| pane.session.as_str() == session.as_str())
            .filter_map(|pane| pane.pane_pid)
            .collect()
    }

    fn session_residuals_after_reap(
        transport: &dyn crate::transport::Transport,
        workspace: &Path,
        session: &crate::transport::SessionName,
        check_primary_transport: bool,
    ) -> (Vec<String>, Option<String>) {
        let mut residual = false;
        let mut error = None;
        if check_primary_transport {
            match transport.has_session(session) {
                Ok(true) => residual = true,
                Ok(false) => {}
                Err(err) if tmux_absent_error(&err.to_string()) => {}
                Err(err) => {
                    error = Some(err.to_string());
                    residual = true;
                }
            }
        }
        // 0.5.x Phase 1d Batch 5 (design §Batch 5 point 4): a ConPTY
        // team's residual check MUST NOT read the workspace tmux socket
        // + shared default tmux socket as evidence for/against ConPTY
        // residual. Those probes are meaningless for a shim-owned pane
        // universe: they will always return `has_session=false` (no
        // tmux session exists) which would look like "no residual" but
        // gives zero honest information about whether the shim / child
        // panes have actually been reaped. Skip the tmux fallback
        // probes when the primary transport is ConPTY; the primary
        // transport check above already covered the honest question
        // ("does the shim still have this session?").
        let primary_is_conpty = matches!(transport.kind(), crate::transport::BackendKind::ConPty);
        if !primary_is_conpty {
            let workspace_transport = shutdown_workspace_transport(workspace);
            match crate::transport::Transport::has_session(&workspace_transport, session) {
                Ok(true) => residual = true,
                Ok(false) => {}
                Err(err) if tmux_absent_error(&err.to_string()) => {}
                Err(err) => {
                    error.get_or_insert_with(|| err.to_string());
                    residual = true;
                }
            }
            let default_transport = crate::tmux_backend::TmuxBackend::new();
            match crate::transport::Transport::has_session(&default_transport, session) {
                Ok(true) => residual = true,
                Ok(false) => {}
                Err(err) if tmux_absent_error(&err.to_string()) => {}
                Err(err) => {
                    error.get_or_insert_with(|| err.to_string());
                    residual = true;
                }
            }
        }
        let sessions = if residual {
            vec![session.as_str().to_string()]
        } else {
            Vec::new()
        };
        (sessions, error)
    }

    fn session_residuals_after_reap_many(
        transport: &dyn crate::transport::Transport,
        workspace: &Path,
        sessions: &[crate::transport::SessionName],
        check_primary_transport: bool,
    ) -> (Vec<String>, Option<String>) {
        let mut residuals = Vec::new();
        let mut error = None;
        for session in sessions {
            let (session_residuals, session_error) = session_residuals_after_reap(
                transport,
                workspace,
                session,
                check_primary_transport,
            );
            for residual in session_residuals {
                if !residuals.iter().any(|seen| seen == &residual) {
                    residuals.push(residual);
                }
            }
            if let Some(session_error) = session_error {
                error.get_or_insert(session_error);
            }
        }
        (residuals, error)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ShutdownReapScope {
        Workspace,
        ScopedTeam,
    }

    fn state_process_roots(state: &Value, scope: ShutdownReapScope) -> Vec<u32> {
        let mut out = Vec::new();
        collect_agent_process_roots(state, &mut out);
        if scope == ShutdownReapScope::Workspace {
            if let Some(teams) = state.get("teams").and_then(Value::as_object) {
                for team in teams.values() {
                    collect_agent_process_roots(team, &mut out);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn collect_agent_process_roots(state: &Value, out: &mut Vec<u32>) {
        let Some(agents) = state.get("agents").and_then(Value::as_object) else {
            return;
        };
        for agent in agents.values() {
            for key in ["provider_pid", "process_id", "pid", "child_pid", "pane_pid"] {
                if let Some(pid) = agent.get(key).and_then(value_u32) {
                    out.push(pid);
                }
            }
        }
    }

    fn value_u32(value: &Value) -> Option<u32> {
        value
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .or_else(|| value.as_str().and_then(|pid| pid.parse::<u32>().ok()))
            .filter(|pid| *pid > 0)
    }

    /// PERF-6 C-② batched signals: the UNION of all root trees gets SIGTERM, shares ONE
    /// >=150ms grace window (no single pid's grace is shortened — the serial per-root
    /// chain is what's removed), then the union gets SIGKILL (noop for already-dead
    /// pids; Gap 37 escalation order TERM -> grace -> KILL preserved), then a single
    /// bounded wait for the whole union. kill/wait sets derive from the SAME snapshot
    /// as the protected set (N39).
    fn reap_process_tree(root_pids: &[u32], protected: &ShutdownProtection, table: &[ProcessInfo]) {
        let mut pids = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for root in root_pids {
            for pid in process_tree_from_table(*root, table) {
                if !protected.contains_pid(pid) && seen.insert(pid) {
                    pids.push(pid);
                }
            }
        }
        if pids.is_empty() {
            return;
        }
        for pid in pids.iter().rev() {
            let _ = crate::platform::process::terminate_pid(
                *pid,
                crate::platform::process::SignalKind::TerminateGraceful,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        for pid in pids.iter().rev() {
            let _ = crate::platform::process::terminate_pid(
                *pid,
                crate::platform::process::SignalKind::TerminateForce,
            );
        }
        wait_for_processes_gone(&pids, std::time::Duration::from_secs(1));
    }

    fn reap_process_groups(pgids: &[u32], protected: &ShutdownProtection) {
        // 0.5.x Windows portability Batch 3: routes group termination
        // through `platform::process::terminate_group`. Unix keeps
        // `kill(-pgid, SIGTERM|SIGKILL)` semantics byte-for-byte;
        // Windows returns AlreadyGone (no pgid concept — Job Object
        // teardown is the shim-side concern per design §Route B).
        // The `pgid_t <= 1 || protected.contains_pgid(...)` filter
        // stays in place so pgid 0/1 (kernel/init) and the caller's
        // own group are never targeted.
        for pgid in pgids {
            if *pgid <= 1 || protected.contains_pgid(*pgid) {
                continue;
            }
            let _ = crate::platform::process::terminate_group(
                *pgid,
                crate::platform::process::SignalKind::TerminateGraceful,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        for pgid in pgids {
            if *pgid <= 1 || protected.contains_pgid(*pgid) {
                continue;
            }
            let _ = crate::platform::process::terminate_group(
                *pgid,
                crate::platform::process::SignalKind::TerminateForce,
            );
        }
    }

    /// PERF-6 C-①-2 + C-②-5: every residual round fetches ONE fresh snapshot (reap
    /// changed the world) and re-derives the protected set from THAT snapshot; all
    /// in-round consumers (match + tree walks) reuse it.
    fn reap_workspace_process_residuals(
        workspace: &Path,
        state: &Value,
        root_pids: &[u32],
        root_pgids: &[u32],
        transport: &dyn crate::transport::Transport,
        scope: ShutdownReapScope,
        probe_degraded: &mut bool,
    ) {
        for _ in 0..5 {
            let round_table = shutdown_table_snapshot(workspace, probe_degraded, "residual_round");
            let mut protected = shutdown_protection_set(&round_table);
            extend_protection_with_leader_panes(&mut protected, transport, state, &round_table);
            let residuals = matched_processes(
                workspace,
                state,
                root_pids,
                root_pgids,
                &protected,
                scope,
                &round_table,
            );
            if residuals.is_empty() {
                return;
            }
            let residual_pids = residuals
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>();
            reap_process_tree(&residual_pids, &protected, &round_table);
            let pgids = residuals
                .iter()
                .filter_map(|process| process.pgid)
                .collect::<Vec<_>>();
            reap_process_groups(&pgids, &protected);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// swallow batch 1: the raw ps probe with an explicit error channel — a failed
    /// probe must never masquerade as "no processes" (CLAUDE.md §5).
    fn probed_process_table() -> Result<Vec<ProcessInfo>, String> {
        match crate::os_probe::bounded_command_output_with_probe(
            std::process::Command::new("ps").args(["-axo", "pid=,ppid=,pgid=,sess=,command="]),
            "ps_table",
            None,
        ) {
            Ok(output) if output.status.success() => Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(parse_process_info)
                .collect()),
            Ok(output) => Err(format!("ps exited with status {:?}", output.status.code())),
            Err(error) => Err(error.to_string()),
        }
    }

    fn process_table() -> Vec<ProcessInfo> {
        probed_process_table().unwrap_or_default()
    }

    /// PERF-6 C-①-1 / swallow batch 1: the shutdown-scope snapshot fetch. A probe
    /// failure writes a `shutdown.process_probe_failed` event (non-null error) and
    /// marks the run degraded instead of silently treating it as "no processes".
    fn shutdown_table_snapshot(
        workspace: &Path,
        probe_degraded: &mut bool,
        phase: &str,
    ) -> Vec<ProcessInfo> {
        match probed_process_table() {
            Ok(table) => table,
            Err(error) => {
                *probe_degraded = true;
                let _ = crate::event_log::EventLog::new(workspace).write(
                    "shutdown.process_probe_failed",
                    json!({
                        "phase": phase,
                        "probe": "ps_table",
                        "error": error,
                    }),
                );
                Vec::new()
            }
        }
    }

    fn parse_process_info(line: &str) -> Option<ProcessInfo> {
        let mut parts = line.split_whitespace();
        let pid = parts.next()?.parse::<u32>().ok()?;
        let ppid = parts.next()?.parse::<u32>().ok()?;
        let pgid = parts.next().and_then(|raw| raw.parse::<u32>().ok());
        let session = parts.next().and_then(|raw| raw.parse::<u32>().ok());
        let command = parts.collect::<Vec<_>>().join(" ");
        Some(ProcessInfo {
            pid,
            ppid,
            pgid,
            session,
            command,
        })
    }

    #[derive(Clone, Debug)]
    struct ProcessInfo {
        pid: u32,
        ppid: u32,
        pgid: Option<u32>,
        session: Option<u32>,
        command: String,
    }

    #[derive(Clone, Debug, Default)]
    struct ShutdownProtection {
        pids: std::collections::BTreeSet<u32>,
        pgids: std::collections::BTreeSet<u32>,
    }

    impl ShutdownProtection {
        fn contains_pid(&self, pid: u32) -> bool {
            self.pids.contains(&pid)
        }

        fn contains_pgid(&self, pgid: u32) -> bool {
            self.pgids.contains(&pgid)
        }

        fn contains_process(&self, process: &ProcessInfo) -> bool {
            self.pids.contains(&process.pid)
                || process.pgid.is_some_and(|pgid| self.pgids.contains(&pgid))
        }
    }

    /// E4 真机 grounded(任何 team 的 shutdown 都不杀任何 team 的 leader 锚 pane):
    /// 扫 state.json 收集所有 leader-anchor pane_id(top-level team_owner /
    /// leader_receiver + teams[<key>].* 嵌套形态)。返非空 BTreeSet 给
    /// `extend_protection_with_leader_panes` 第二来源用。
    ///
    /// 覆盖场景:
    /// - LeaderStartMode::ExecProvider:state.json team_owner.pane_id 指用户原 tmux
    ///   pane(非 leader 前缀)→ shutdown 不杀(E4 真机复发修法)
    /// - E4b team-in-team:子 team state 的 team_owner.pane_id 指父 team worker pane;
    ///   父 team state 的 teams.<child>.team_owner.pane_id 同义(若有该字段)
    ///   → 任一 team 的 shutdown 都不杀任何 team 的 leader 锚 pane
    pub fn collect_state_leader_anchor_pane_ids(
        state: &Value,
    ) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        push_anchor_pane_id(state, &mut out);
        if let Some(teams) = state.get("teams").and_then(Value::as_object) {
            for (_, team_state) in teams {
                push_anchor_pane_id(team_state, &mut out);
            }
        }
        out
    }

    /// E49 (0.3.24 P0, shutdown kills leader CLI): collect worker pane ids on the
    /// given `session` from state.agents + teams[*].agents, EXCLUDING any pane id
    /// in `leader_anchor_ids`. If an agent has no `pane_id` but its window appears
    /// in `live_targets` under the same session, fall back to the live `pane_id`
    /// for that window — so we still kill the worker pane via the structural
    /// addressing path rather than the session-level `kill_session` that would
    /// also end the leader pane (the E49 bug).
    fn collect_session_worker_panes(
        state: &Value,
        session: &str,
        leader_anchor_ids: &std::collections::BTreeSet<String>,
        live_targets: &[crate::transport::PaneInfo],
    ) -> Vec<crate::transport::PaneId> {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut out: Vec<crate::transport::PaneId> = Vec::new();
        collect_session_worker_panes_from_agents(
            state.get("agents"),
            session,
            leader_anchor_ids,
            live_targets,
            &mut seen,
            &mut out,
        );
        if let Some(teams) = state.get("teams").and_then(Value::as_object) {
            for (_, team_state) in teams {
                collect_session_worker_panes_from_agents(
                    team_state.get("agents"),
                    session,
                    leader_anchor_ids,
                    live_targets,
                    &mut seen,
                    &mut out,
                );
            }
        }
        out
    }

    fn collect_session_worker_panes_from_agents(
        agents: Option<&Value>,
        session: &str,
        leader_anchor_ids: &std::collections::BTreeSet<String>,
        live_targets: &[crate::transport::PaneInfo],
        seen: &mut std::collections::BTreeSet<String>,
        out: &mut Vec<crate::transport::PaneId>,
    ) {
        let Some(map) = agents.and_then(Value::as_object) else {
            return;
        };
        for (agent_id, agent) in map {
            let claimed_pane = agent
                .get("pane_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            // Try state-claimed pane id first; cross-check with live_targets to
            // skip dead state entries (a stale pane_id from a respawn).
            if let Some(pane_id) = claimed_pane {
                if leader_anchor_ids.contains(pane_id) {
                    continue;
                }
                let in_live = live_targets
                    .iter()
                    .any(|info| info.pane_id.as_str() == pane_id);
                if in_live && seen.insert(pane_id.to_string()) {
                    out.push(crate::transport::PaneId::new(pane_id));
                    continue;
                }
            }
            // Fallback: agent window + session match a live pane (E43 lineage:
            // tmux respawn-pane bumps the %id while the window stays). Use the
            // live pane id for that window.
            let window = agent
                .get("window")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(agent_id.as_str());
            if let Some(info) = live_targets.iter().find(|info| {
                info.session.as_str() == session
                    && info
                        .window_name
                        .as_ref()
                        .is_some_and(|name| name.as_str() == window)
            }) {
                let pane_str = info.pane_id.as_str();
                if leader_anchor_ids.contains(pane_str) {
                    continue;
                }
                if seen.insert(pane_str.to_string()) {
                    out.push(info.pane_id.clone());
                }
            }
        }
    }

    /// E49 (0.3.24 P0): emit a structured event documenting the session was
    /// spared because it carries a managed-leader anchor pane. Lets operators
    /// see why kill_session was skipped.
    fn event_log_write_session_spared(
        workspace: &Path,
        session: &crate::transport::SessionName,
        leader_anchor_ids: &std::collections::BTreeSet<String>,
    ) -> Result<(), CliError> {
        let event_log = crate::event_log::EventLog::new(workspace);
        let _ = event_log.write(
            "shutdown.session_spared_managed_leader",
            json!({
                "session": session.as_str(),
                "reason": "managed_leader_anchor_in_session",
                "leader_anchor_pane_ids": leader_anchor_ids.iter().collect::<Vec<_>>(),
            }),
        );
        Ok(())
    }

    /// 单帧扫 team_owner.pane_id + leader_receiver.pane_id → BTreeSet 累加。
    fn push_anchor_pane_id(state: &Value, out: &mut std::collections::BTreeSet<String>) {
        for key in &["team_owner", "leader_receiver"] {
            if let Some(pane_id) = state
                .get(*key)
                .and_then(|v| v.get("pane_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.insert(pane_id.to_string());
            }
        }
    }

    /// E4 真机 grounded(cross-socket):收 state.json 中所有记录的 tmux_socket
    /// endpoint(top-level + teams[<key>] 嵌套形态;team_owner / leader_receiver
    /// 任一字段)。owner_bind 在 claim 时把 leader pane 所在 socket 记进
    /// leader_receiver.tmux_socket(evidence:/测试rust版本/4 state.json),用作
    /// 跨 socket 查 leader pane → pane_pid 的真相源。
    fn collect_state_recorded_tmux_sockets(state: &Value) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        push_recorded_tmux_socket(state, &mut out);
        if let Some(teams) = state.get("teams").and_then(Value::as_object) {
            for (_, team_state) in teams {
                push_recorded_tmux_socket(team_state, &mut out);
            }
        }
        out
    }

    fn push_recorded_tmux_socket(state: &Value, out: &mut std::collections::BTreeSet<String>) {
        for key in &["team_owner", "leader_receiver"] {
            if let Some(socket) = state
                .get(*key)
                .and_then(|v| v.get("tmux_socket"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.insert(socket.to_string());
            }
        }
    }

    /// PERF-6 C-①-1/C-②-4 (N39): the protected set derives from the CALLER's snapshot —
    /// the same table the kill/wait sets derive from.
    fn shutdown_protection_set(table: &[ProcessInfo]) -> ShutdownProtection {
        let mut protected = ShutdownProtection::default();
        let current = std::process::id();
        protected.pids.insert(current);
        // 0.5.x Windows portability Batch 3: use platform primitive.
        // Windows returns None (no pgid concept) and the branch is
        // skipped honestly.
        if let Some(pgid) = crate::platform::process::current_process_group() {
            protected.pgids.insert(pgid);
        }
        let mut cursor = current;
        let mut seen = std::collections::BTreeSet::new();
        while seen.insert(cursor) {
            let Some(process) = table.iter().find(|process| process.pid == cursor) else {
                break;
            };
            protected.pids.insert(process.pid);
            if let Some(pgid) = process.pgid {
                protected.pgids.insert(pgid);
            }
            if process.ppid == 0 || process.ppid == process.pid {
                break;
            }
            cursor = process.ppid;
        }
        protected
    }

    /// B5/F2 + E4 真机 grounded(任何 team 的 shutdown 都不杀任何 team 的 leader 锚 pane):
    /// the leader terminal's pane process tree joins the protected set (same set, same
    /// mechanism as the invoker ancestry) so the workspace residual sweep's cmdline/cwd
    /// matching cannot reap the leader — including when ANOTHER team's bare shutdown
    /// runs, where the leader is never in the invoker's ancestry.
    ///
    /// 0.4.x (CR R3): leader shell wrapper interaction. The leader pane's
    /// controlling process is the `sh -lc "...; exec ${SHELL} -l"` that
    /// runs Claude as a CHILD. When Claude exits and the shell wrapper falls
    /// back via `exec ${SHELL} -l`, the controlling PID is REPLACED in-place
    /// by the interactive shell (same pane_pid). Because this function
    /// protects by `pane.pane_pid` (Source 1 & 2), the fallback interactive
    /// shell is already covered by the same protection set — shutdown will
    /// NOT treat the fallback shell as a stray process. Verified by the
    /// `leader_fallback_shell_protected_when_provider_exited` test.
    ///
    /// Two leader-pane sources(N39 双来源,真机 grounded):
    /// 1. **Session prefix**: tmux session starts with `team-agent-leader-`(契约 grounded;
    ///    覆盖 LeaderStartMode::NewTmuxSession / AttachExisting).
    /// 2. **State.json anchors**(E4 修法):state.team_owner.pane_id / state.leader_receiver.pane_id
    ///    在 top-level **和** teams[<key>].* 都扫(N39 任何 team 的 leader 锚 pane);
    ///    覆盖 LeaderStartMode::ExecProvider(用户 in_tmux 直接 exec,session 名是用户原
    ///    `main`/`0`/whatever,不带 leader 前缀 — 此前 B5 三犯保护集漏覆盖)+ E4b
    ///    team-in-team(子 team 的 leader 锚 = 父 team worker pane,window 名是 agent id
    ///    也不带 leader 前缀)。
    pub(crate) fn extend_protection_with_leader_panes(
        protected: &mut ShutdownProtection,
        transport: &dyn crate::transport::Transport,
        state: &Value,
        table: &[ProcessInfo],
    ) {
        let mut leader_pane_pids: Vec<u32> = Vec::new();
        let pane_targets = transport.list_targets().unwrap_or_default();
        // Source 1: session 前缀过滤(原 B5 实现)— per-workspace socket。
        leader_pane_pids.extend(
            pane_targets
                .iter()
                .filter(|pane| {
                    pane.session
                        .as_str()
                        .starts_with(crate::leader::LEADER_SESSION_PREFIX)
                })
                .filter_map(|pane| pane.pane_pid),
        );
        // Source 2: state.json team_owner / leader_receiver 真锚 pane_id(top-level +
        // teams[*]),per-workspace socket 命中。
        let anchor_pane_ids: std::collections::BTreeSet<String> =
            collect_state_leader_anchor_pane_ids(state);
        leader_pane_pids.extend(
            pane_targets
                .iter()
                .filter(|pane| anchor_pane_ids.contains(pane.pane_id.as_str()))
                .filter_map(|pane| pane.pane_pid),
        );
        // Source 3 (E4 真机 grounded · cross-socket):leader 锚 pane 可能在【别的
        // tmux socket】上 — LeaderStartMode::ExecProvider 真实场景里用户 in_tmux
        // 起 `team-agent claude`,leader pane 留在用户【默认 socket】,而 shutdown
        // 的 transport 走 per-workspace `ta-<hash>` socket,list_targets 看不见。
        // 从 state.json 读 leader_receiver/team_owner.tmux_socket(claim 时
        // owner_bind 记录,见 evidence /测试rust版本/4 state.json),查那个 socket
        // 的 list_targets 找 anchor pane_id → pane_pid → 进入 process_tree 保护。
        // 不在 state 中的 socket 不查(MUST-17 不撒宽 / 不主动枚举全机器 sockets)。
        for socket_endpoint in collect_state_recorded_tmux_sockets(state) {
            let cross_backend =
                crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&socket_endpoint);
            let cross_panes =
                <crate::tmux_backend::TmuxBackend as crate::transport::Transport>::list_targets(
                    &cross_backend,
                )
                .unwrap_or_default();
            leader_pane_pids.extend(
                cross_panes
                    .iter()
                    .filter(|pane| anchor_pane_ids.contains(pane.pane_id.as_str()))
                    .filter_map(|pane| pane.pane_pid),
            );
        }
        leader_pane_pids.sort_unstable();
        leader_pane_pids.dedup();
        if leader_pane_pids.is_empty() {
            return;
        }
        for root in &leader_pane_pids {
            for pid in process_tree_from_table(*root, table) {
                protected.pids.insert(pid);
                if let Some(pgid) = table
                    .iter()
                    .find(|process| process.pid == pid)
                    .and_then(|process| process.pgid)
                {
                    protected.pgids.insert(pgid);
                }
            }
        }
        // The tmux SERVER carrying the leader pane must survive too: its command line
        // contains the workspace path (it was started with the worker spawn command), so
        // the residual sweep matches it, and killing the server SIGHUPs every pane —
        // including the protected leader — bypassing per-pid protection. Protect the
        // server pid itself (NOT its tree: worker panes must still die).
        for pane_pid in &leader_pane_pids {
            if let Some(server) = table
                .iter()
                .find(|process| process.pid == *pane_pid)
                .and_then(|pane| table.iter().find(|process| process.pid == pane.ppid))
                .filter(|server| server.pid > 1)
            {
                protected.pids.insert(server.pid);
                if let Some(pgid) = server.pgid {
                    protected.pgids.insert(pgid);
                }
            }
        }
    }

    // 0.5.x Windows portability Batch 3: the four inline helpers
    // (send_process_signal, send_process_signal_group,
    // reap_child_if_possible, process_is_live) have been replaced
    // by direct calls through `crate::platform::process::*` at the
    // shutdown callsites above. The private helpers are removed so
    // there's a single source of truth for signal/waitpid/liveness
    // semantics; Unix behavior is byte-preserving (SignalKind maps
    // 1:1 to SIGTERM/SIGKILL, ProcessLiveness::Live == the previous
    // `kill(pid, 0) == 0 || EPERM` branch).

    fn wait_for_processes_gone(pids: &[u32], timeout: std::time::Duration) {
        let start = std::time::Instant::now();
        loop {
            for pid in pids {
                crate::platform::process::reap_child_if_possible(*pid);
            }
            if !pids
                .iter()
                .any(|pid| crate::platform::process::pid_is_alive(*pid))
                || start.elapsed() >= timeout
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn process_pgids(
        pids: &[u32],
        protected: &ShutdownProtection,
        table: &[ProcessInfo],
    ) -> Vec<u32> {
        let mut pgids = pids
            .iter()
            .filter_map(|pid| table.iter().find(|process| process.pid == *pid))
            .filter_map(|process| process.pgid)
            .filter(|pgid| {
                // 0.5.x Windows portability Batch 4: `libc::pid_t` was
                // used here as a signed-integer conversion gate to
                // reject values > INT_MAX. Replace with the equivalent
                // `i32::try_from` (pgid_t is `c_int` on every Unix we
                // support). Windows has no pgid concept so `pgids`
                // is empty in practice — the filter is dead code on
                // Windows but must still compile.
                i32::try_from(*pgid)
                    .map(|pgid_int| pgid_int > 1 && !protected.contains_pgid(*pgid))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        pgids.sort_unstable();
        pgids.dedup();
        pgids
    }

    fn process_residuals(
        workspace: &Path,
        state: &Value,
        root_pids: &[u32],
        root_pgids: &[u32],
        protected: &ShutdownProtection,
        scope: ShutdownReapScope,
        table: &[ProcessInfo],
    ) -> Vec<Value> {
        let mut residuals = matched_processes(
            workspace, state, root_pids, root_pgids, protected, scope, table,
        );
        let mut seen = residuals
            .iter()
            .map(|process| process.pid)
            .collect::<std::collections::BTreeSet<_>>();
        for pid in root_pids {
            if !protected.contains_pid(*pid)
                && crate::platform::process::pid_is_alive(*pid)
                && seen.insert(*pid)
            {
                residuals.push(ProcessInfo {
                    pid: *pid,
                    ppid: 0,
                    pgid: None,
                    session: None,
                    command: String::new(),
                });
            }
        }
        residuals
            .into_iter()
            .map(|process| {
                json!({
                    "pid": process.pid,
                    "ppid": process.ppid,
                    "pgid": process.pgid,
                    "session": process.session,
                    "command": process.command,
                })
            })
            .collect()
    }

    fn matched_processes(
        workspace: &Path,
        state: &Value,
        root_pids: &[u32],
        root_pgids: &[u32],
        protected: &ShutdownProtection,
        scope: ShutdownReapScope,
        table: &[ProcessInfo],
    ) -> Vec<ProcessInfo> {
        let root_tree = root_pids
            .iter()
            .flat_map(|pid| process_tree_from_table(*pid, table))
            .filter(|pid| !protected.contains_pid(*pid))
            .collect::<std::collections::BTreeSet<_>>();
        let root_pgids = root_pgids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let spawn_cwds = state_spawn_cwds(state, scope);
        let workspace_text = workspace.to_string_lossy().to_string();
        let mut cwd_probe_budget = 3_usize;
        let mut out = Vec::new();
        for process in table {
            if protected.contains_pid(process.pid) {
                continue;
            }
            let matches_workspace = scope == ShutdownReapScope::Workspace
                && process_matches_workspace(
                    process,
                    &workspace_text,
                    &spawn_cwds,
                    &mut cwd_probe_budget,
                );
            if matches_workspace
                || root_tree.contains(&process.pid)
                || process.pgid.is_some_and(|pgid| root_pgids.contains(&pgid))
            {
                out.push(process.clone());
            }
        }
        out
    }

    fn process_tree_from_table(root_pid: u32, table: &[ProcessInfo]) -> Vec<u32> {
        if root_pid == 0 {
            return Vec::new();
        }
        let mut out = vec![root_pid];
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(root_pid);
        let mut index = 0;
        while index < out.len() {
            let parent = out[index];
            for process in table {
                if process.ppid == parent && seen.insert(process.pid) {
                    out.push(process.pid);
                }
            }
            index += 1;
        }
        out
    }

    fn state_spawn_cwds(state: &Value, scope: ShutdownReapScope) -> Vec<PathBuf> {
        let mut out = Vec::new();
        collect_spawn_cwds(state, &mut out);
        if scope == ShutdownReapScope::Workspace {
            if let Some(teams) = state.get("teams").and_then(Value::as_object) {
                for team in teams.values() {
                    collect_spawn_cwds(team, &mut out);
                }
            }
        }
        out
    }

    fn collect_spawn_cwds(state: &Value, out: &mut Vec<PathBuf>) {
        let Some(agents) = state.get("agents").and_then(Value::as_object) else {
            return;
        };
        for agent in agents.values() {
            if let Some(spawn_cwd) = agent
                .get("spawn_cwd")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.is_empty())
            {
                out.push(PathBuf::from(spawn_cwd));
            }
        }
    }

    fn process_matches_workspace(
        process: &ProcessInfo,
        workspace_text: &str,
        spawn_cwds: &[PathBuf],
        cwd_probe_budget: &mut usize,
    ) -> bool {
        let command = process.command.as_str();
        if command.contains("mcp-server")
            && command.contains("--workspace")
            && command.contains(workspace_text)
        {
            return true;
        }
        if command.contains(workspace_text) {
            return true;
        }
        if spawn_cwds.is_empty() || *cwd_probe_budget == 0 {
            return false;
        }
        *cwd_probe_budget -= 1;
        let Some(cwd) = process_cwd(process.pid) else {
            return false;
        };
        spawn_cwds
            .iter()
            .any(|spawn_cwd| path_is_under(&cwd, spawn_cwd))
    }

    fn process_cwd(pid: u32) -> Option<PathBuf> {
        let proc_cwd = PathBuf::from(format!("/proc/{pid}/cwd"));
        if let Ok(path) = std::fs::read_link(proc_cwd) {
            return Some(path);
        }
        if crate::os_probe::probe_timed_out() {
            return None;
        }
        let output = crate::os_probe::bounded_command_output_with_probe(
            std::process::Command::new("lsof").args([
                "-a",
                "-p",
                &pid.to_string(),
                "-d",
                "cwd",
                "-Fn",
            ]),
            "lsof_cwd",
            Some(pid),
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix('n').map(PathBuf::from))
    }

    fn path_is_under(path: &Path, root: &Path) -> bool {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        path == root || path.starts_with(root)
    }
    /// `runtime.restart`(`cmd_restart`)。
    pub fn restart(
        workspace: &Path,
        allow_fresh: bool,
        team: Option<&str>,
        session_converge_deadline_ms: Option<u64>,
    ) -> Result<Value, CliError> {
        match crate::lifecycle::restart_with_session_convergence_deadline(
            workspace,
            allow_fresh,
            team,
            session_converge_deadline_ms,
        ) {
            Ok(report) => Ok(restart_value(report, team)),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.start_agent`(`cmd_start_agent`)。
    pub fn start_agent(
        workspace: &Path,
        agent: &str,
        force: bool,
        open_display: bool,
        allow_fresh: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let agent_id = crate::model::ids::AgentId::new(agent);
        match crate::lifecycle::start_agent(
            workspace,
            &agent_id,
            force,
            open_display,
            allow_fresh,
            team,
        ) {
            Ok(report) => {
                Ok(json!({"ok": true, "agent_id": agent, "report": format!("{report:?}")}))
            }
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.stop_agent`(`cmd_stop_agent`)。
    pub fn stop_agent(
        workspace: &Path,
        agent: &str,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let agent_id = crate::model::ids::AgentId::new(agent);
        match crate::lifecycle::stop_agent(workspace, &agent_id, team) {
            Ok(report) => Ok(json!({"ok": true, "agent_id": agent, "stopped": report.stopped})),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.reset_agent`(`cmd_reset_agent`;`--discard-session` 必需)。
    pub fn reset_agent(
        workspace: &Path,
        agent: &str,
        discard_session: bool,
        open_display: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let agent_id = crate::model::ids::AgentId::new(agent);
        match crate::lifecycle::reset_agent(
            workspace,
            &agent_id,
            discard_session,
            open_display,
            team,
        ) {
            Ok(crate::lifecycle::ResetAgentOutcome::Reset {
                env,
                start_mode,
                discarded_session_id,
                session_id,
                new_session_id,
            }) => Ok(json!({
                "ok": true,
                "agent_id": env.agent_id.as_str(),
                "status": "reset",
                "state_file": env.state_file.to_string_lossy().to_string(),
                "coordinator_started": env.coordinator_started,
                "start_mode": start_mode,
                "discarded_session_id": discarded_session_id.as_ref().map(|id| id.as_str()),
                "session_id": session_id.as_ref().map(|id| id.as_str()),
                "new_session_id": new_session_id.as_ref().map(|id| id.as_str()),
            })),
            Ok(crate::lifecycle::ResetAgentOutcome::Refused { reason }) => Ok(json!({
                "ok": false,
                "agent_id": agent,
                "status": "refused",
                "reason": match reason {
                    crate::lifecycle::ResetRefusal::DiscardSessionRequired => "discard_session_required",
                },
            })),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.add_agent`(`cmd_add_agent`;`--role-file` 必需)。
    pub fn add_agent(
        workspace: &Path,
        agent: &str,
        role_file: &str,
        open_display: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let agent_id = crate::model::ids::AgentId::new(agent);
        match crate::lifecycle::add_agent(
            workspace,
            &agent_id,
            Path::new(role_file),
            open_display,
            team,
        ) {
            Ok(report) => Ok(json!({
                "ok": true,
                "agent_id": agent,
                "role_file": report.role_file.to_string_lossy(),
            })),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.fork_agent`(`cmd_fork_agent`;`--as` 必需)。
    pub fn fork_agent(
        workspace: &Path,
        source_agent: &str,
        as_agent_id: &str,
        label: Option<&str>,
        open_display: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let source = crate::model::ids::AgentId::new(source_agent);
        let dest = crate::model::ids::AgentId::new(as_agent_id);
        match crate::lifecycle::fork_agent(workspace, &source, &dest, label, open_display, team) {
            Ok(report) => Ok(json!({
                "ok": true,
                "source_agent_id": report.source_agent_id.as_str(),
                "new_agent_id": report.new_agent_id.as_str(),
            })),
            Err(e) => Ok(error_value(e)),
        }
    }
    /// `runtime.remove_agent`(`cmd_remove_agent`;`--from-spec` 须配 `--confirm`)。
    pub fn remove_agent(
        workspace: &Path,
        agent: &str,
        from_spec: bool,
        confirm: bool,
        force: bool,
        team: Option<&str>,
    ) -> Result<Value, CliError> {
        let agent_id = crate::model::ids::AgentId::new(agent);
        match crate::lifecycle::remove_agent_flag_requirements(workspace, &agent_id, team) {
            Ok(requirements) => {
                if !remove_agent_missing_flags(from_spec, confirm, force, &requirements).is_empty() {
                    return Ok(remove_agent_flag_refusal(
                        workspace,
                        agent,
                        team,
                        from_spec,
                        confirm,
                        force,
                        &requirements,
                    ));
                }
            }
            Err(error) if confirm => return Ok(error_value(error)),
            Err(_) => {}
        }
        if !confirm {
            return Ok(
                json!({"ok": false, "agent_id": agent, "error": "remove-agent requires --confirm"}),
            );
        }
        match crate::lifecycle::remove_agent(workspace, &agent_id, from_spec, force, team) {
            Ok(report @ crate::lifecycle::RemoveAgentOutcome::Removed { .. }) => Ok(json!({
                "ok": true,
                "agent_id": agent,
                "status": "removed",
                "report": format!("{report:?}"),
            })),
            Ok(crate::lifecycle::RemoveAgentOutcome::RefusedFromSpecConfirm { .. }) => Ok(json!({
                "ok": false,
                "agent_id": agent,
                "status": "refused",
                "reason": "from_spec_confirm_required",
                "error": "remove-agent requires --from-spec --confirm for spec-defined agents",
                "action": "rerun with --from-spec --confirm, or omit --from-spec only for dynamic agents",
            })),
            Ok(crate::lifecycle::RemoveAgentOutcome::RefusedForceRequired { .. }) => Ok(json!({
                "ok": false,
                "agent_id": agent,
                "status": "refused",
                "reason": "force_required",
                "error": "agent is running; remove-agent requires --force",
                "action": "rerun with --force to stop and remove the running agent",
            })),
            Ok(crate::lifecycle::RemoveAgentOutcome::RefusedRequiredFlags { .. }) => Ok(json!({
                "ok": false,
                "agent_id": agent,
                "status": "refused",
                "reason": "remove_agent_flags_required",
                "error": "remove-agent required flags changed; rerun the command from the latest refusal",
            })),
            Err(e) => Ok(error_value(e)),
        }
    }

    fn remove_agent_flag_refusal(
        workspace: &Path,
        agent: &str,
        team: Option<&str>,
        from_spec: bool,
        confirm: bool,
        force: bool,
        requirements: &crate::lifecycle::RemoveAgentFlagRequirements,
    ) -> Value {
        let required_flags = remove_agent_required_flags(requirements);
        let missing_flags = remove_agent_missing_flags(from_spec, confirm, force, requirements);
        let reason = if missing_flags.len() == 1 {
            match missing_flags[0] {
                "--confirm" => "confirm_required",
                "--from-spec" => "from_spec_confirm_required",
                "--force" => "force_required",
                _ => "remove_agent_flags_required",
            }
        } else {
            "remove_agent_flags_required"
        };
        let command = remove_agent_command(workspace, agent, team, &required_flags);
        let required = required_flags.join(" ");
        json!({
            "ok": false,
            "agent_id": agent,
            "status": "refused",
            "reason": reason,
            "error": format!("remove-agent requires {required} for this agent"),
            "action": format!("rerun: {command}"),
            "command": command,
            "missing_flags": missing_flags,
            "required_flags": required_flags,
            "state": {
                "from_spec_required": requirements.from_spec_required,
                "running": requirements.force_required,
                "has_session": requirements.has_session,
            },
        })
    }

    fn remove_agent_missing_flags(
        from_spec: bool,
        confirm: bool,
        force: bool,
        requirements: &crate::lifecycle::RemoveAgentFlagRequirements,
    ) -> Vec<&'static str> {
        remove_agent_required_flags(requirements)
            .into_iter()
            .filter(|flag| match *flag {
                "--from-spec" => !from_spec,
                "--confirm" => !confirm,
                "--force" => !force,
                _ => false,
            })
            .collect()
    }

    fn remove_agent_required_flags(
        requirements: &crate::lifecycle::RemoveAgentFlagRequirements,
    ) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if requirements.from_spec_required {
            flags.push("--from-spec");
        }
        flags.push("--confirm");
        if requirements.force_required {
            flags.push("--force");
        }
        flags
    }

    fn remove_agent_command(
        workspace: &Path,
        agent: &str,
        team: Option<&str>,
        required_flags: &[&str],
    ) -> String {
        let mut parts = vec![
            "team-agent".to_string(),
            "remove-agent".to_string(),
            shell_arg(agent),
            "--workspace".to_string(),
            shell_arg(&workspace.to_string_lossy()),
        ];
        if let Some(team) = team {
            parts.push("--team".to_string());
            parts.push(shell_arg(team));
        }
        parts.extend(required_flags.iter().map(|flag| (*flag).to_string()));
        parts.join(" ")
    }

    fn shell_arg(raw: &str) -> String {
        if !raw.is_empty()
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b':'))
        {
            raw.to_string()
        } else {
            format!("'{}'", raw.replace('\'', "'\\''"))
        }
    }
    /// `runtime.acknowledge_idle`(`cmd_acknowledge_idle`)。
    pub fn acknowledge_idle(workspace: &Path, team: Option<&str>) -> Result<Value, CliError> {
        let mut state = crate::state::persist::load_runtime_state(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let team = team
            .map(ToString::to_string)
            .or_else(|| {
                state
                    .get("active_team_key")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .filter(|s| !s.is_empty())
            .or_else(|| {
                workspace
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "current".to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let ttl_seconds = 1800;
        let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds)).to_rfc3339();
        record_idle_acknowledged(&mut state, &team, &now, &expires_at, ttl_seconds);
        suppress_team_idle_fallbacks(&mut state, &team, &now, &expires_at, ttl_seconds);
        let agent_id = state
            .get("agents")
            .and_then(Value::as_object)
            .and_then(|agents| agents.keys().next().cloned())
            .map(Value::String)
            .unwrap_or(Value::Null);
        crate::state::persist::save_runtime_state(workspace, &state)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        crate::event_log::EventLog::new(workspace)
            .write(
                "coordinator.idle_acknowledged",
                json!({"team": team, "ttl_seconds": ttl_seconds}),
            )
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        Ok(json!({
            "ok": true,
            "team": team,
            "agent_id": agent_id,
            "acknowledged_at": now,
            "expires_at": expires_at,
            "ttl_seconds": ttl_seconds,
        }))
    }

    fn error_value(error: crate::lifecycle::LifecycleError) -> Value {
        let message = error.to_string();
        let mut payload = json!({"ok": false, "error": message});
        if let Some(next_action) = error_next_action(&message) {
            payload["next_action"] = json!(next_action);
        }
        payload
    }

    /// E8 (N38): 把"错路常犯"的运行时错误指到正确出路(纯文案,无语义变更)。
    /// 匹配 [`LifecycleError`] 的人读消息子串(`agent {id} not found` /
    /// `agent id already exists` / `unknown worker agent id`),给出下一步命令。
    fn error_next_action(message: &str) -> Option<&'static str> {
        // start-agent 撞"agent ... not found":start-agent 语义=启动 state 已有 agent;
        // 想新增角色应走 add-agent。
        if message.contains("not found") && message.contains("agent") {
            return Some(
                "start-agent only starts an agent that already exists in state. \
                 To add a NEW role at runtime use: team-agent add-agent <id> --role-file <path>",
            );
        }
        // add-agent / fork 撞"agent id already exists":id 已占用。
        if message.contains("agent id already exists") {
            return Some(
                "that agent id is already in the team. \
                 Use a different id, or start the existing one with: team-agent start-agent <id>",
            );
        }
        // stop/reset/fork 源撞"unknown worker agent id":拼写/团队选择错。
        if message.contains("unknown worker agent id") {
            return Some(
                "no such worker agent in this team. \
                 Run `team-agent status` to list agent ids (check --team if multiple teams)",
            );
        }
        None
    }

    fn record_idle_acknowledged(
        state: &mut Value,
        team: &str,
        acknowledged_at: &str,
        expires_at: &str,
        ttl_seconds: i64,
    ) {
        let Some(root) = state.as_object_mut() else {
            return;
        };
        let coordinator = root
            .entry("coordinator")
            .or_insert_with(|| json!({}))
            .as_object_mut();
        let Some(coordinator) = coordinator else {
            return;
        };
        let idle = coordinator
            .entry("idle_acknowledged")
            .or_insert_with(|| json!({}))
            .as_object_mut();
        let Some(idle) = idle else {
            return;
        };
        idle.insert(
            team.to_string(),
            json!({"acknowledged_at": acknowledged_at, "expires_at": expires_at, "ttl_seconds": ttl_seconds}),
        );
    }

    fn suppress_team_idle_fallbacks(
        state: &mut Value,
        team: &str,
        suppressed_at: &str,
        expires_at: &str,
        ttl_seconds: i64,
    ) {
        let agents = state
            .get("agents")
            .and_then(Value::as_object)
            .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for agent in agents {
            upsert_suppression(
                state,
                SuppressionRecord {
                    team,
                    agent_id: &agent,
                    alert_type: "idle_fallback",
                    suppressed_by: "manual_acknowledge",
                    suppressed_at,
                    expires_at,
                    ttl_seconds,
                },
            );
        }
    }

    struct SuppressionRecord<'a> {
        team: &'a str,
        agent_id: &'a str,
        alert_type: &'a str,
        suppressed_by: &'a str,
        suppressed_at: &'a str,
        expires_at: &'a str,
        ttl_seconds: i64,
    }

    fn upsert_suppression(state: &mut Value, record: SuppressionRecord<'_>) {
        let Some(root) = state.as_object_mut() else {
            return;
        };
        let Some(coordinator) = root
            .entry("coordinator")
            .or_insert_with(|| json!({}))
            .as_object_mut()
        else {
            return;
        };
        let Some(all) = coordinator
            .entry("suppressed_idle_alerts")
            .or_insert_with(|| json!({}))
            .as_object_mut()
        else {
            return;
        };
        let Some(team_map) = all
            .entry(record.team.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
        else {
            return;
        };
        let Some(agent_map) = team_map
            .entry(record.agent_id.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
        else {
            return;
        };
        agent_map.insert(
            record.alert_type.to_string(),
            json!({
                "suppressed_at": record.suppressed_at,
                "suppressed_by": record.suppressed_by,
                "manual_acknowledge": true,
                "expires_at": record.expires_at,
                "ttl_seconds": record.ttl_seconds,
            }),
        );
    }

    fn quick_start_value(report: crate::lifecycle::QuickStartReport) -> Value {
        match report {
            crate::lifecycle::QuickStartReport::Ready {
                session_name,
                launch,
                next_actions,
                attach_commands,
                display_backend,
                worker_readiness,
            } => {
                // BUG-7: never emit bare "ready" while worker tool-load is unverified.
                // The summary string + a structured `worker_readiness` block tell the
                // caller exactly which agents are unhealthy (Degraded) or that the
                // tool-set load has not been confirmed yet (PendingToolLoad).
                let incomplete_session_capture_agents =
                    launch.session_capture_incomplete_agents.clone();
                let all_spawned = !launch.started.is_empty();
                let leader_receiver_attached = launch.leader_receiver_attached;
                let all_resumable_have_session = incomplete_session_capture_agents.is_empty();
                let all_workers_spawned = all_spawned;
                let attached_receiver = leader_receiver_attached;
                let all_attached_receiver = leader_receiver_attached;
                let all_resumable_agents_have_sessions = all_resumable_have_session;
                let (summary, ok, readiness_json) = match &worker_readiness {
                    crate::lifecycle::QuickStartReadiness::Degraded { unhealthy_agents } => (
                        format!(
                            "quick-start degraded: {}; unhealthy: {}",
                            session_name.as_str(),
                            unhealthy_agents.join(",")
                        ),
                        false,
                        json!({
                            "all_spawned": all_spawned,
                            "all_workers_spawned": all_workers_spawned,
                            "all_attached_receiver": all_attached_receiver,
                            "attached_receiver": attached_receiver,
                            "leader_receiver_attached": leader_receiver_attached,
                            "all_resumable_have_session": all_resumable_have_session,
                            "all_resumable_agents_have_sessions": all_resumable_agents_have_sessions,
                            "ready": all_spawned && all_attached_receiver && all_resumable_have_session,
                            "state": "degraded",
                            "session_capture_complete": all_resumable_have_session,
                            "session_capture_incomplete": !all_resumable_have_session,
                            "incomplete_session_capture_agents": incomplete_session_capture_agents.clone(),
                            "pending_session_agent_ids": incomplete_session_capture_agents,
                            "unhealthy_agents": unhealthy_agents,
                        }),
                    ),
                    crate::lifecycle::QuickStartReadiness::PendingToolLoad => {
                        if !all_resumable_have_session {
                            (
                                format!(
                                    "quick-start pending: {}; provider session capture incomplete",
                                    session_name.as_str()
                                ),
                                false,
                                json!({
                                    "all_spawned": all_spawned,
                                    "all_workers_spawned": all_workers_spawned,
                                    "all_attached_receiver": all_attached_receiver,
                                    "attached_receiver": attached_receiver,
                                    "leader_receiver_attached": leader_receiver_attached,
                                    "all_resumable_have_session": all_resumable_have_session,
                                    "all_resumable_agents_have_sessions": all_resumable_agents_have_sessions,
                                    "ready": all_spawned && all_attached_receiver && all_resumable_have_session,
                                    "state": "session_capture_incomplete",
                                    "session_capture_complete": all_resumable_have_session,
                                    "session_capture_incomplete": !all_resumable_have_session,
                                    "incomplete_session_capture_agents": incomplete_session_capture_agents.clone(),
                                    "pending_session_agent_ids": incomplete_session_capture_agents,
                                    "reason": "provider session capture is incomplete; restart is not yet resume-safe",
                                }),
                            )
                        } else if launch.leader_receiver_attached {
                            (
                                format!(
                                    "quick-start launched (worker tool load unverified): {}",
                                    session_name.as_str()
                                ),
                                all_spawned && all_attached_receiver && all_resumable_have_session,
                                json!({
                                    "all_spawned": all_spawned,
                                    "all_workers_spawned": all_workers_spawned,
                                    "all_attached_receiver": all_attached_receiver,
                                    "attached_receiver": attached_receiver,
                                    "leader_receiver_attached": leader_receiver_attached,
                                    "all_resumable_have_session": all_resumable_have_session,
                                    "all_resumable_agents_have_sessions": all_resumable_agents_have_sessions,
                                    "ready": all_spawned && all_attached_receiver && all_resumable_have_session,
                                    "state": "pending_tool_load",
                                    "session_capture_complete": all_resumable_have_session,
                                    "session_capture_incomplete": !all_resumable_have_session,
                                    "incomplete_session_capture_agents": incomplete_session_capture_agents.clone(),
                                    "pending_session_agent_ids": incomplete_session_capture_agents,
                                    "reason": "worker MCP tool set load not yet confirmed; run `team-agent doctor` or wait for first worker turn",
                                }),
                            )
                        } else {
                            (
                                format!(
                                    "quick-start degraded: {}; leader receiver unbound",
                                    session_name.as_str()
                                ),
                                false,
                                json!({
                                    "all_spawned": all_spawned,
                                    "all_workers_spawned": all_workers_spawned,
                                    "all_attached_receiver": all_attached_receiver,
                                    "attached_receiver": attached_receiver,
                                    "leader_receiver_attached": leader_receiver_attached,
                                    "all_resumable_have_session": all_resumable_have_session,
                                    "all_resumable_agents_have_sessions": all_resumable_agents_have_sessions,
                                    "ready": all_spawned && all_attached_receiver && all_resumable_have_session,
                                    "state": "leader_receiver_unbound",
                                    "session_capture_complete": all_resumable_have_session,
                                    "session_capture_incomplete": !all_resumable_have_session,
                                    "incomplete_session_capture_agents": incomplete_session_capture_agents.clone(),
                                    "pending_session_agent_ids": incomplete_session_capture_agents,
                                    "reason": "launched team has no attached leader receiver",
                                    "next_action": "claim-leader",
                                }),
                            )
                        }
                    }
                };
                json!({
                    "ok": ok,
                    "summary": summary,
                    "status": readiness_json.get("state").cloned().unwrap_or(Value::Null),
                    "reason": readiness_json.get("reason").cloned().unwrap_or(Value::Null),
                    "ready": readiness_json.get("ready").cloned().unwrap_or(Value::Bool(false)),
                    "session_name": session_name.as_str(),
                    "dry_run": launch.dry_run,
                    "display_backend": display_backend,
                    "next_actions": next_actions,
                    "attach_commands": attach_commands,
                    "reminder": crate::cli::QUICK_START_REMINDER,
                    "readiness": readiness_json.clone(),
                    "worker_readiness": readiness_json,
                })
            }
            crate::lifecycle::QuickStartReport::ExistingRuntime {
                team,
                session_name,
                state_path,
                next_actions,
                attach_commands,
            } => json!({
                "ok": false,
                "summary": "existing runtime",
                "team": team,
                "session_name": session_name.map(|s| s.as_str().to_string()),
                "state_path": state_path.map(|p| p.to_string_lossy().to_string()),
                "next_actions": next_actions,
                "attach_commands": attach_commands,
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::QuickStartReport::PreflightBlocked {
                summary,
                blockers,
                next_actions,
                attach_commands,
            } => json!({
                "ok": false,
                "summary": summary,
                "blockers": blockers,
                "next_actions": next_actions,
                "attach_commands": attach_commands,
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
        }
    }

    #[cfg(test)]
    mod quick_start_value_tests {
        use super::*;

        #[test]
        fn existing_runtime_json_includes_attach_commands() {
            let value = quick_start_value(crate::lifecycle::QuickStartReport::ExistingRuntime {
                team: Some("teamA".to_string()),
                session_name: Some(crate::transport::SessionName::new("team-teamA")),
                state_path: Some(PathBuf::from("/tmp/state.json")),
                next_actions: vec!["restart".to_string()],
                attach_commands: vec![
                    "tmux -S /tmp/tmux-501/ta-test attach -t team-teamA:worker".to_string()
                ],
            });
            assert_eq!(
                value.pointer("/attach_commands/0").and_then(Value::as_str),
                Some("tmux -S /tmp/tmux-501/ta-test attach -t team-teamA:worker"),
                "B-2: ExistingRuntime JSON must preserve attach_commands instead of only next_actions; value={value}"
            );
            assert_eq!(
                value.get("reminder").and_then(Value::as_str),
                Some(crate::cli::QUICK_START_REMINDER)
            );
        }

        #[test]
        fn preflight_blocked_json_includes_empty_attach_commands() {
            let value = quick_start_value(crate::lifecycle::QuickStartReport::PreflightBlocked {
                summary: "blocked".to_string(),
                blockers: vec!["missing TEAM.md".to_string()],
                next_actions: vec!["fix preflight blockers".to_string()],
                attach_commands: Vec::new(),
            });
            assert_eq!(
                value.get("attach_commands").and_then(Value::as_array).map(Vec::len),
                Some(0),
                "B-2: PreflightBlocked JSON must include attach_commands: [] for schema parity with Ready/Restart; value={value}"
            );
        }

        #[test]
        fn restart_json_includes_harness_reminder() {
            let value = restart_value(
                crate::lifecycle::RestartReport::RefusedResumeAtomicity {
                    unresumable: Vec::new(),
                    allow_fresh: false,
                    error: "resume refused".to_string(),
                },
                None,
            );

            assert_eq!(
                value.get("reminder").and_then(Value::as_str),
                Some(crate::cli::QUICK_START_REMINDER)
            );
        }
    }

    fn restart_value(report: crate::lifecycle::RestartReport, team: Option<&str>) -> Value {
        match report {
            crate::lifecycle::RestartReport::Restarted {
                session_name,
                agents,
                coordinator_started,
                next_actions,
                attach_commands,
            } => json!({
                "ok": true,
                "status": "restarted",
                "session_name": session_name.as_str(),
                "agents": agents.iter().map(|a| a.agent_id.as_str()).collect::<Vec<_>>(),
                "coordinator_started": coordinator_started,
                "next_actions": next_actions,
                "attach_commands": attach_commands,
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::RestartReport::Partial {
                session_name,
                agents,
                failed_agents,
                coordinator_started,
                next_actions,
                attach_commands,
            } => json!({
                "ok": false,
                "status": "partial",
                "reason": "restart_agent_failed",
                "session_name": session_name.as_str(),
                "agents": agents.iter().map(|a| a.agent_id.as_str()).collect::<Vec<_>>(),
                "failed_agents": failed_agents.iter().map(|failure| json!({
                    "agent_id": failure.agent_id.as_str(),
                    "restart_mode": failure.restart_mode,
                    "decision": failure.decision,
                    "session_id": failure.session_id.as_ref().map(|session| session.as_str()),
                    "phase": failure.phase,
                    "error": failure.error,
                    "action": format!(
                        "inspect worker {} output, then restart that worker with `team-agent restart-agent {}` or rerun `team-agent restart --allow-fresh`",
                        failure.agent_id,
                        failure.agent_id
                    ),
                    "log": format!(
                        ".team/logs/coordinator.log and .team/runtime/state.json agent={}",
                        failure.agent_id
                    ),
                })).collect::<Vec<_>>(),
                "coordinator_started": coordinator_started,
                "next_actions": next_actions,
                "attach_commands": attach_commands,
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::RestartReport::Failed {
                session_name,
                failed_agents,
                next_actions,
                attach_commands,
            } => json!({
                "ok": false,
                "status": "failed",
                "reason": "restart_all_agents_failed",
                "session_name": session_name.as_str(),
                "agents": [],
                "failed_agents": failed_agents.iter().map(|failure| json!({
                    "agent_id": failure.agent_id.as_str(),
                    "restart_mode": failure.restart_mode,
                    "decision": failure.decision,
                    "session_id": failure.session_id.as_ref().map(|session| session.as_str()),
                    "phase": failure.phase,
                    "error": failure.error,
                    "action": format!(
                        "inspect worker {} output, then restart that worker with `team-agent restart-agent {}` or rerun `team-agent restart --allow-fresh`",
                        failure.agent_id,
                        failure.agent_id
                    ),
                    "log": format!(
                        ".team/logs/coordinator.log and .team/runtime/state.json agent={}",
                        failure.agent_id
                    ),
                })).collect::<Vec<_>>(),
                "next_actions": next_actions,
                "attach_commands": attach_commands,
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::RestartReport::RefusedResumeAtomicity {
                unresumable,
                allow_fresh,
                error,
            } => {
                // Unit 5 + Layer 2 wire-through (leader directive 2026-06-22):
                // `unresumable` JSON shape is now `[{agent_id, reason,
                // session_id?, checked_paths?, recovery_hint?}, ...]` so the
                // structured refusal class is visible to CLI consumers. The
                // legacy string-array shape (single agent_id per entry) is
                // available under `unresumable_ids` for tooling that still
                // wants the cheap list.
                let unresumable_detail: Vec<Value> = unresumable
                    .iter()
                    .map(|w| {
                        let mut entry = serde_json::Map::new();
                        entry.insert(
                            "agent_id".to_string(),
                            json!(w.agent_id.as_str()),
                        );
                        // Prefer the structured wire string when the
                        // ResumeRefusalReason enum is populated; fall back
                        // to the legacy free-form `reason` otherwise.
                        let reason_wire = w
                            .refusal_reason
                            .as_ref()
                            .map(|r| r.wire().to_string())
                            .unwrap_or_else(|| w.reason.clone());
                        entry.insert("reason".to_string(), json!(reason_wire));
                        if let Some(sid) = &w.session_id {
                            entry.insert(
                                "session_id".to_string(),
                                json!(sid.as_str()),
                            );
                        }
                        if let Some(reason) = &w.refusal_reason {
                            if let crate::provider::session::ResumeRefusalReason::SessionBackingStoreMissing {
                                checked_paths,
                                recovery_hint,
                            } = reason
                            {
                                if !checked_paths.is_empty() {
                                    entry.insert(
                                        "checked_paths".to_string(),
                                        json!(checked_paths
                                            .iter()
                                            .map(|p| p.to_string_lossy().into_owned())
                                            .collect::<Vec<_>>()),
                                    );
                                }
                                if let Some(hint) = recovery_hint {
                                    let mut h = serde_json::Map::new();
                                    h.insert(
                                        "provider".to_string(),
                                        json!(hint.provider),
                                    );
                                    if let Some(name) = &hint.provider_session_name_hint {
                                        h.insert("name".to_string(), json!(name));
                                    }
                                    if let Some(cwd) = &hint.spawn_cwd {
                                        h.insert(
                                            "spawn_cwd".to_string(),
                                            json!(cwd.to_string_lossy()),
                                        );
                                    }
                                    h.insert(
                                        "picker_hint".to_string(),
                                        json!(hint.picker_hint()),
                                    );
                                    entry.insert(
                                        "recovery_hint".to_string(),
                                        Value::Object(h),
                                    );
                                }
                            }
                            if let crate::provider::session::ResumeRefusalReason::SessionIdentityMismatch {
                                expected_agent_id,
                                embedded_agent_id,
                                session_id,
                                rollout_path,
                            } = reason
                            {
                                entry.insert(
                                    "expected_agent_id".to_string(),
                                    json!(expected_agent_id),
                                );
                                entry.insert(
                                    "embedded_agent_id".to_string(),
                                    json!(embedded_agent_id),
                                );
                                entry.insert(
                                    "poisoned_session_id".to_string(),
                                    json!(session_id),
                                );
                                if let Some(path) = rollout_path {
                                    entry.insert(
                                        "rollout_path".to_string(),
                                        json!(path.to_string_lossy()),
                                    );
                                }
                            }
                        }
                        Value::Object(entry)
                    })
                    .collect();
                let unresumable_ids: Vec<&str> =
                    unresumable.iter().map(|w| w.agent_id.as_str()).collect();
                // Layer 2 self-healing (leader follow-up 2026-06-22): when
                // EVERY unresumable worker shares the same structured
                // refusal_reason wire string, refine the top-level `status`
                // from the coarse `refused_resume_atomicity` to a class-
                // specific label and overlay an `error` that names the
                // specific cause + concrete recovery moves. The original
                // `refused_resume_atomicity` status is kept as
                // `status_class` so anything matching on the coarse label
                // still works.
                let distinct_wires: std::collections::BTreeSet<&'static str> = unresumable
                    .iter()
                    .filter_map(|w| w.refusal_reason.as_ref().map(|r| r.wire()))
                    .collect();
                let (status_str, error_str): (String, String) = if distinct_wires.len() == 1
                    && distinct_wires.len() == unresumable.len()
                {
                    // Every entry had a structured reason and they all
                    // agreed. Refine.
                    let wire = *distinct_wires.iter().next().unwrap_or(&"");
                    match wire {
                        "session_backing_store_missing" => {
                            // Collect a flat list of every checked path so
                            // operators can copy/paste-grep for files that
                            // moved. picker_hint per worker gives the
                            // alternate-recovery one-liner.
                            let mut probed: Vec<String> = Vec::new();
                            let mut picker_lines: Vec<String> = Vec::new();
                            for w in unresumable.iter() {
                                if let Some(
                                    crate::provider::session::ResumeRefusalReason::SessionBackingStoreMissing {
                                        checked_paths,
                                        recovery_hint,
                                    },
                                ) = w.refusal_reason.as_ref()
                                {
                                    for p in checked_paths {
                                        probed.push(p.to_string_lossy().into_owned());
                                    }
                                    if let Some(h) = recovery_hint {
                                        picker_lines.push(format!(
                                            "  {}: try `{}`",
                                            w.agent_id.as_str(),
                                            h.picker_hint(),
                                        ));
                                    }
                                }
                            }
                            probed.sort();
                            probed.dedup();
                            let mut msg = format!(
                                "restart refused: provider session backing store missing for {} worker(s) ({}). \
                                 Pass --allow-fresh to start a new session, or restore the backing files listed below.",
                                unresumable.len(),
                                unresumable_ids.join(", "),
                            );
                            if !probed.is_empty() {
                                msg.push_str("\nProbed paths (none contained the expected session):");
                                for p in &probed {
                                    msg.push_str("\n  ");
                                    msg.push_str(p);
                                }
                            }
                            if !picker_lines.is_empty() {
                                msg.push_str("\nProvider picker recovery hints:");
                                for line in &picker_lines {
                                    msg.push_str("\n");
                                    msg.push_str(line);
                                }
                            }
                            (
                                "refused_session_backing_missing".to_string(),
                                msg,
                            )
                        }
                        "no_persisted_session_id" => (
                            "refused_no_session_id".to_string(),
                            format!(
                                "restart refused: no persisted session_id for {} worker(s) ({}). Pass --allow-fresh to start fresh.",
                                unresumable.len(),
                                unresumable_ids.join(", "),
                            ),
                        ),
                        "provider_resume_unsupported" => (
                            "refused_provider_resume_unsupported".to_string(),
                            format!(
                                "restart refused: provider does not support resume for {} worker(s) ({}). Pass --allow-fresh to start fresh.",
                                unresumable.len(),
                                unresumable_ids.join(", "),
                            ),
                        ),
                        "session_identity_mismatch" => {
                            let mut lines = Vec::new();
                            for w in unresumable.iter() {
                                if let Some(
                                    crate::provider::session::ResumeRefusalReason::SessionIdentityMismatch {
                                        expected_agent_id,
                                        embedded_agent_id,
                                        session_id,
                                        rollout_path,
                                    },
                                ) = w.refusal_reason.as_ref()
                                {
                                    let path = rollout_path
                                        .as_ref()
                                        .map(|p| p.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "<unknown>".to_string());
                                    lines.push(format!(
                                        "  {}: session {} points to transcript identity {} at {} (expected {})",
                                        w.agent_id.as_str(),
                                        session_id,
                                        embedded_agent_id,
                                        path,
                                        expected_agent_id
                                    ));
                                }
                            }
                            let mut msg = format!(
                                "restart refused: provider session identity mismatch for {} worker(s) ({}). Pass --allow-fresh to discard the poisoned tuple and start fresh.",
                                unresumable.len(),
                                unresumable_ids.join(", "),
                            );
                            if !lines.is_empty() {
                                msg.push_str("\nMismatched sessions:");
                                for line in &lines {
                                    msg.push('\n');
                                    msg.push_str(line);
                                }
                            }
                            ("refused_session_identity_mismatch".to_string(), msg)
                        }
                        _ => ("refused_resume_atomicity".to_string(), error.clone()),
                    }
                } else {
                    // Mixed or unstructured reasons — keep the coarse
                    // label, but augment the error so it lists each
                    // worker's specific reason.
                    let per_worker = unresumable
                        .iter()
                        .map(|w| {
                            let reason = w
                                .refusal_reason
                                .as_ref()
                                .map(|r| r.wire().to_string())
                                .unwrap_or_else(|| w.reason.clone());
                            format!("{} ({})", w.agent_id.as_str(), reason)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        "refused_resume_atomicity".to_string(),
                        format!("{error} Per-worker: {per_worker}."),
                    )
                };
                json!({
                    "ok": false,
                    "status": status_str,
                    "status_class": "refused_resume_atomicity",
                    "allow_fresh": allow_fresh,
                    "error": error_str,
                    "unresumable": unresumable_detail,
                    "unresumable_ids": unresumable_ids,
                    "reminder": crate::cli::QUICK_START_REMINDER,
                })
            }
            crate::lifecycle::RestartReport::RefusedResumeNotReady {
                missing,
                allow_fresh,
                deadline,
                elapsed,
                error,
            } => json!({
                "ok": false,
                "kind": "resume_not_ready",
                "reason": "session_capture_incomplete",
                "status": "resume_not_ready",
                "allow_fresh": allow_fresh,
                "error": error,
                "pending_agents": missing.iter().map(|w| w.as_str()).collect::<Vec<_>>(),
                "missing": missing.iter().map(|w| w.as_str()).collect::<Vec<_>>(),
                "session_convergence": {
                    "complete": false,
                    "deadline_s": deadline.as_secs_f64(),
                    "deadline_ms": deadline.as_millis(),
                    "elapsed_ms": elapsed.as_millis(),
                    "pending_agent_ids": missing.iter().map(|w| w.as_str()).collect::<Vec<_>>(),
                },
                "next_action": "rerun restart after session capture completes, or pass --allow-fresh to deliberately discard missing context",
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::RestartReport::RefusedInvalidFirstSendAt {
                invalid,
                allow_fresh,
                error,
            } => json!({
                "ok": false,
                "status": "refused_invalid_first_send_at",
                "allow_fresh": allow_fresh,
                "error": error,
                "invalid": invalid.iter().map(|w| w.worker_id.as_str()).collect::<Vec<_>>(),
                "reminder": crate::cli::QUICK_START_REMINDER,
            }),
            crate::lifecycle::RestartReport::RefusedDirtyTopology {
                session_name,
                reason,
                error,
                issue_ids,
            } => {
                let repair_team = team
                    .filter(|team| !team.is_empty())
                    .unwrap_or(session_name.as_str());
                let claim =
                    format!("team-agent claim-leader --team {repair_team} --confirm --json");
                let takeover = format!("team-agent takeover --team {repair_team} --confirm --json");
                json!({
                    "ok": false,
                    "status": "refused_dirty_topology",
                    "reason": reason,
                    "session_name": session_name,
                    "error": error,
                    "issues": issue_ids
                        .iter()
                        .map(|id| json!({"id": id}))
                        .collect::<Vec<_>>(),
                    "next_actions": [
                        "team-agent diagnose --json",
                        claim,
                        takeover
                    ],
                    "reminder": crate::cli::QUICK_START_REMINDER,
                })
            }
        }
    }

    fn stop_status_wire(status: crate::coordinator::StopOutcome) -> &'static str {
        match status {
            crate::coordinator::StopOutcome::Missing => "missing",
            crate::coordinator::StopOutcome::InvalidPidRemoved => "invalid_pid_removed",
            crate::coordinator::StopOutcome::KillFailed => "kill_failed",
            crate::coordinator::StopOutcome::Stopped => "stopped",
        }
    }

    fn tmux_absent_error(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("no server running")
            || lower.contains("no such file")
            || lower.contains("can't find session")
            || lower.contains("can't find pane")
            || lower.contains("can't find window")
    }

    fn mark_agents_stopped(state: &mut Value) {
        let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
            return;
        };
        for agent in agents.values_mut() {
            if let Some(obj) = agent.as_object_mut() {
                obj.insert("status".to_string(), json!("stopped"));
            }
        }
    }

    fn mark_matching_session_teams_stopped(
        state: &mut Value,
        session_name: Option<&crate::transport::SessionName>,
    ) -> Vec<String> {
        let Some(session_name) = session_name.map(crate::transport::SessionName::as_str) else {
            return Vec::new();
        };
        let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (key, team) in teams.iter_mut() {
            let matches = team
                .get("session_name")
                .and_then(Value::as_str)
                .is_some_and(|session| session == session_name);
            if matches {
                mark_agents_stopped(team);
                out.push(key.clone());
            }
        }
        out
    }

    fn promote_live_sibling_after_scoped_shutdown(
        workspace: &Path,
        stopped_state: &Value,
    ) -> Result<(), CliError> {
        let stopped_key = stopped_state
            .get("active_team_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty());
        let Some(stopped_key) = stopped_key else {
            return Ok(());
        };
        let raw = crate::state::persist::load_runtime_state(workspace)?;
        let active = raw
            .get("active_team_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        if active != stopped_key {
            return Ok(());
        }
        let Some((next_key, _)) = raw
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|teams| {
                teams
                    .iter()
                    .find(|(key, team)| key.as_str() != stopped_key && team_has_running_agent(team))
            })
        else {
            return Ok(());
        };
        let promoted = crate::state::projection::project_top_level_view(&raw, next_key);
        crate::state::persist::save_runtime_state(workspace, &promoted)?;
        Ok(())
    }

    fn team_has_running_agent(team: &Value) -> bool {
        team.get("agents")
            .and_then(Value::as_object)
            .is_some_and(|agents| {
                agents
                    .values()
                    .any(|agent| agent.get("status").and_then(Value::as_str) == Some("running"))
            })
    }

    #[cfg(test)]
    mod e8_error_guidance_tests {
        use super::{error_next_action, error_value};

        #[test]
        fn start_agent_not_found_points_to_add_agent() {
            // LifecycleError::RequirementUnmet("agent {id} not found") 经 to_string():
            // "agent start requirement unmet: agent foo not found".
            let msg = "agent start requirement unmet: agent foo not found";
            let na = error_next_action(msg).expect("not-found must carry next_action");
            assert!(na.contains("add-agent"), "must steer to add-agent: {na}");
            assert!(
                na.contains("--role-file"),
                "must show the role-file flag: {na}"
            );
        }

        #[test]
        fn add_agent_already_exists_explains_way_out() {
            let msg = "agent start requirement unmet: agent id already exists: foo";
            let na = error_next_action(msg).expect("already-exists must carry next_action");
            assert!(na.contains("start-agent"), "must mention start-agent: {na}");
        }

        #[test]
        fn unknown_worker_points_to_status() {
            let msg = "agent start requirement unmet: unknown worker agent id: ghost";
            let na = error_next_action(msg).expect("unknown worker must carry next_action");
            assert!(na.contains("status"), "must steer to status: {na}");
        }

        #[test]
        fn unrelated_error_has_no_next_action() {
            assert_eq!(
                error_next_action("state persistence failed: disk full"),
                None
            );
        }

        #[test]
        fn error_value_attaches_next_action_field() {
            let err = crate::lifecycle::LifecycleError::RequirementUnmet(
                "agent foo not found".to_string(),
            );
            let v = error_value(err);
            assert_eq!(v["ok"], serde_json::json!(false));
            assert!(
                v["next_action"]
                    .as_str()
                    .unwrap_or("")
                    .contains("add-agent"),
                "error_value must attach the add-agent guidance: {v}"
            );
        }
    }
}

/// PLACEHOLDER → diagnose lane(`diagnose/health.py` `doctor`、`diagnose/comms.py`
/// `run_comms_selftest`、`diagnose/orphan_cleanup.py` `orphan_gate`/`cleanup_orphan_coordinators`、
/// `message_store/schema_migration.py` `schema_diagnosis`/`fix_schema_layout`)。
/// `cmd_doctor` 的所有分支委派点。返回 `Value`(稳定 JSON 形状由 diagnose lane 拥有)。
pub mod diagnose_port {
    use super::*;

    /// `runtime.doctor(spec)` + schema 注入(`cmd_doctor` 默认分支)。
    pub fn doctor(workspace: &Path, spec: Option<&Path>) -> Result<Value, CliError> {
        let tmux_path = which_path("tmux");
        let tmux_installed = tmux_path.is_some();
        let workspace_valid = workspace.is_dir();
        let team_context = workspace_valid && has_doctor_team_context(workspace, spec);
        let workspace_has_entries = workspace_valid && workspace_has_any_entry(workspace);
        // SMOKE-1 (locate.md §"Minimal Fix"):default doctor 不再隐式编译
        // `<workspace>/.team/current`(legacy 残留)作 profile_smoke 目标。
        // profile_smoke 是 team-scoped 体检,只在以下两种情形跑:
        //   ① 用户显式给了 spec / team dir;
        //   ② workspace 根本身就是 team dir(含 TEAM.md / team.spec.yaml)。
        // legacy `<workspace>/.team/current` 仅作降级诊断面(legacy_team_invalid),
        // 不再绑架整个 doctor 假死在 profile_smoke_failed 上。
        let explicit_team_target = explicit_doctor_team_dir(workspace, spec);
        let profile_smoke = explicit_team_target
            .as_ref()
            .map(|team| crate::cli::diagnose::build_profile_smoke_check_for_team(team))
            .transpose()?;
        let legacy_check = if explicit_team_target.is_none() {
            legacy_current_team_check(workspace)?
        } else {
            None
        };
        let profile_smoke_value = profile_smoke.unwrap_or_else(|| {
            legacy_check.clone().unwrap_or_else(|| {
                json!({
                    "name": "profile_smoke",
                    "ok": true,
                    "status": "not_required",
                    "checks": [],
                    "secret_values_printed": false,
                })
            })
        });
        let profile_smoke_ok = profile_smoke_value
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        // legacy 降级面(legacy_team_invalid)不下拉整体 ok —— 用户没显式让我们
        // 体检这个 team,失败是降级诊断信息,不是 install 自检失败。
        let legacy_only_failure = !profile_smoke_ok
            && profile_smoke_value.get("status").and_then(Value::as_str)
                == Some("legacy_team_invalid");
        let effective_smoke_ok = profile_smoke_ok || legacy_only_failure;
        let ok = workspace_valid && (team_context || workspace_has_entries) && effective_smoke_ok;
        let health = crate::coordinator::coordinator_health(
            &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
        );
        Ok(json!({
            "tmux": {
                "installed": tmux_installed,
                "path": tmux_path,
            },
            "workspace": workspace.to_string_lossy().to_string(),
            "workspace_is_git_repo": workspace.join(".git").exists(),
            "providers": {},
            "mcp": {
                "server_command": which_path("team_orchestrator"),
                "local_module": true,
            },
            "secret_scan": secret_scan(workspace),
            "profile_smoke": profile_smoke_value,
            "coordinator": coordinator_health_value(health),
            "ok": ok,
            "error": if ok {
                Value::Null
            } else if !profile_smoke_ok && !legacy_only_failure {
                json!("profile_smoke_failed")
            } else if workspace_valid {
                json!("workspace has no Team Agent spec or runtime context")
            } else {
                json!("invalid workspace")
            },
        }))
    }

    /// SMOKE-1: 仅当用户显式提供 spec/team dir,或 workspace 根本身是 team dir
    /// (含 TEAM.md / team.spec.yaml)时返 team_dir。legacy `<workspace>/.team/
    /// current` 不算 explicit target(走 legacy_current_team_check 降级面)。
    fn explicit_doctor_team_dir(workspace: &Path, spec: Option<&Path>) -> Option<PathBuf> {
        if let Some(spec) = spec {
            let candidate = if spec.is_absolute() {
                spec.to_path_buf()
            } else {
                workspace.join(spec)
            };
            if candidate.is_file() {
                return candidate.parent().map(Path::to_path_buf);
            }
            if candidate.join("team.spec.yaml").is_file() || candidate.join("TEAM.md").is_file() {
                return Some(candidate);
            }
        }
        if workspace.join("team.spec.yaml").is_file() || workspace.join("TEAM.md").is_file() {
            return Some(workspace.to_path_buf());
        }
        None
    }

    /// SMOKE-1: legacy `<workspace>/.team/current` 残留体检 — 降级诊断,**不**
    /// 当 install self-check 失败。如果 legacy 团有 spec/TEAM.md,尝试 compile,
    /// 失败返 `status=legacy_team_invalid` + team_dir + reason + next_action(N38
    /// 失败可解释性);compile 成功就不打扰用户(返 None,profile_smoke 走
    /// `not_required`)。无 legacy 团目录 → None。
    fn legacy_current_team_check(workspace: &Path) -> Result<Option<Value>, CliError> {
        let team = workspace.join(".team").join("current");
        let has_spec = team.join("team.spec.yaml").is_file();
        let has_team_md = team.join("TEAM.md").is_file();
        if !has_spec && !has_team_md {
            return Ok(None);
        }
        match crate::compiler::compile_team(&team) {
            Ok(_) => Ok(None),
            Err(error) => {
                let team_dir = team.to_string_lossy().to_string();
                Ok(Some(json!({
                    "name": "profile_smoke",
                    "ok": false,
                    "status": "legacy_team_invalid",
                    "team_dir": team_dir,
                    "reason": error.to_string(),
                    "next_action": format!(
                        "scope doctor to a real team: `team-agent doctor <team-dir>`, \
                         or repair/remove the legacy `{}` directory",
                        team.display()
                    ),
                    "checks": [],
                    "secret_values_printed": false,
                })))
            }
        }
    }

    fn doctor_team_dir(workspace: &Path, spec: Option<&Path>) -> Option<PathBuf> {
        if let Some(spec) = spec {
            let candidate = if spec.is_absolute() {
                spec.to_path_buf()
            } else {
                workspace.join(spec)
            };
            if candidate.is_file() {
                return candidate.parent().map(Path::to_path_buf);
            }
            if candidate.join("team.spec.yaml").is_file() || candidate.join("TEAM.md").is_file() {
                return Some(candidate);
            }
        }
        if workspace.join("team.spec.yaml").is_file() || workspace.join("TEAM.md").is_file() {
            return Some(workspace.to_path_buf());
        }
        let current = workspace.join(".team").join("current");
        if current.join("team.spec.yaml").is_file() || current.join("TEAM.md").is_file() {
            return Some(current);
        }
        None
    }

    fn has_doctor_team_context(workspace: &Path, spec: Option<&Path>) -> bool {
        if spec.is_some_and(|path| {
            let candidate = if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.join(path)
            };
            candidate.is_file()
        }) {
            return true;
        }
        [
            workspace.join("TEAM.md"),
            workspace.join("team.spec.yaml"),
            workspace.join(".team/current/TEAM.md"),
            workspace.join(".team/current/team.spec.yaml"),
            workspace.join(".team/runtime/state.json"),
            workspace.join(".team/runtime/team.db"),
        ]
        .into_iter()
        .any(|path| path.exists())
    }

    fn workspace_has_any_entry(workspace: &Path) -> bool {
        std::fs::read_dir(workspace)
            .ok()
            .and_then(|mut entries| entries.next())
            .is_some()
    }

    fn secret_scan(workspace: &Path) -> Value {
        let mut findings = Vec::new();
        let mut scanned = 0usize;
        scan_secret_dir(workspace, workspace, 0, &mut scanned, &mut findings);
        json!({
            "ok": findings.is_empty(),
            "findings": findings,
        })
    }

    const SECRET_SCAN_MAX_DEPTH: usize = 4;
    const SECRET_SCAN_MAX_ENTRIES: usize = 512;
    const SECRET_SCAN_MAX_FILE_BYTES: u64 = 128 * 1024;

    fn scan_secret_dir(
        root: &Path,
        dir: &Path,
        depth: usize,
        scanned: &mut usize,
        findings: &mut Vec<Value>,
    ) {
        if depth > SECRET_SCAN_MAX_DEPTH || *scanned >= SECRET_SCAN_MAX_ENTRIES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if *scanned >= SECRET_SCAN_MAX_ENTRIES {
                return;
            }
            *scanned = scanned.saturating_add(1);
            let path = entry.path();
            let name = path.file_name().map(|s| s.to_string_lossy());
            if name.as_deref() == Some(".team") || name.as_deref() == Some(".git") {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                scan_secret_dir(root, &path, depth.saturating_add(1), scanned, findings);
                continue;
            }
            if file_type.is_file() {
                scan_secret_file(root, &path, findings);
            }
        }
    }

    fn scan_secret_file(root: &Path, path: &Path, findings: &mut Vec<Value>) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() || metadata.len() > SECRET_SCAN_MAX_FILE_BYTES {
            return;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        let mut text = String::new();
        if std::io::Read::take(file, SECRET_SCAN_MAX_FILE_BYTES)
            .read_to_string(&mut text)
            .is_err()
        {
            return;
        }
        for (idx, line) in text.lines().enumerate() {
            if line.contains("OPENAI_API_KEY=") || line.contains("ANTHROPIC_API_KEY=") {
                let rel = path.strip_prefix(root).unwrap_or(path);
                findings.push(json!({
                    "path": rel.to_string_lossy().to_string(),
                    "line": idx.saturating_add(1),
                    "rule": "api_key_assignment",
                    "match_excerpt": line.trim(),
                }));
            }
        }
    }
    /// `run_comms_selftest`(`--comms`/`--gate comms`)。**纯 state-read,零 token**(MUST-NOT-13)。
    pub fn comms_selftest(
        workspace: &Path,
        team: Option<&str>,
        gate: Option<&str>,
    ) -> Result<Value, CliError> {
        crate::diagnose::comms::doctor_comms_json(workspace, team, gate)
    }

    /// `orphan_gate(fix, confirm)`(`--gate orphans`)。CI gate。
    pub fn orphan_gate(fix: bool, confirm: bool) -> Result<Value, CliError> {
        crate::diagnose::orphans::orphan_gate_json(fix, confirm)
    }
    /// `cleanup_orphan_coordinators(confirm)`(`--cleanup-orphans`;dry-run unless `--confirm`)。
    pub fn cleanup_orphans(confirm: bool) -> Result<Value, CliError> {
        crate::diagnose::orphans::cleanup_orphans_json(confirm)
    }
    /// `fix_schema_layout`(`--fix-schema`)/`schema_diagnosis`。
    pub fn fix_schema(workspace: &Path) -> Result<Value, CliError> {
        let db_path = workspace.join(".team").join("runtime").join("team.db");
        let result =
            crate::db::migration::fix_schema_layout(workspace, crate::db::schema::SCHEMA_VERSION)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
        match result {
            crate::db::migration::FixResult::Missing(diagnosis) => Ok(fix_schema_value(
                &db_path,
                diagnosis,
                false,
                Vec::new(),
                None,
                None,
            )),
            crate::db::migration::FixResult::Blocked { reason } => Ok(json!({
                "ok": false,
                "status": "blocked",
                "db_path": db_path.to_string_lossy().to_string(),
                "schema_version": crate::db::schema::SCHEMA_VERSION,
                "reason": reason,
                "fixed": false,
            })),
            crate::db::migration::FixResult::Fixed {
                diagnosis,
                rebuilds,
            } => {
                let backup = rebuilds
                    .first()
                    .map(|event| event.backup_path.clone())
                    .unwrap_or_else(|| backup_path_preview(&db_path, diagnosis.user_version));
                Ok(fix_schema_value(
                    &db_path,
                    diagnosis,
                    true,
                    rebuild_values(rebuilds),
                    Some(backup),
                    Some("none"),
                ))
            }
        }
    }

    fn run_id() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:012x}", now & 0xffffffffffff)
    }

    fn read_runtime_state(workspace: &Path) -> Value {
        let path = workspace.join(".team").join("runtime").join("state.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn which_path(binary: &str) -> Option<String> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
        None
    }

    fn backup_path_preview(db_path: &Path, user_version: i64) -> String {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        db_path
            .with_file_name(format!(
                "team.db.pre-migration-{stamp}-from-v{user_version}.bak"
            ))
            .to_string_lossy()
            .to_string()
    }

    fn rebuild_values(events: Vec<crate::db::migration::RebuildEvent>) -> Vec<Value> {
        events
            .into_iter()
            .map(|event| {
                json!({
                    "table": event.table,
                    "from_layout_columns": event.from_layout_columns,
                    "to_layout_columns": event.to_layout_columns,
                    "backup_path": event.backup_path,
                    "row_count_before": event.row_count_before,
                    "row_count_after": event.row_count_after,
                    "missing": event.missing,
                })
            })
            .collect()
    }

    fn fix_schema_value(
        db_path: &Path,
        diagnosis: crate::db::migration::Diagnosis,
        fixed: bool,
        rebuilds: Vec<Value>,
        backup: Option<String>,
        recommended_action: Option<&str>,
    ) -> Value {
        json!({
            "ok": diagnosis.ok,
            "status": diagnosis.status,
            "db_path": db_path.to_string_lossy().to_string(),
            "schema_version": crate::db::schema::SCHEMA_VERSION,
            "user_version": diagnosis.user_version,
            "layout_diffs": diagnosis.layout_diffs,
            "recommended_action": recommended_action.unwrap_or("none"),
            "would_backup_path": backup,
            "fixed": fixed,
            "rebuilds": rebuilds,
        })
    }

    fn coordinator_health_value(health: crate::coordinator::HealthReport) -> Value {
        json!({
            "ok": health.ok,
            "status": coordinator_status_wire(health.status),
            "pid": health.pid.map(|p| p.get()),
            "metadata": health.metadata.map(|m| json!({
                "pid": m.pid.get(),
                "protocol_version": m.protocol_version,
                "message_store_schema_version": m.message_store_schema_version,
                "source": m.source,
                "updated_at": m.updated_at,
            })),
            "metadata_ok": health.metadata_ok,
            "schema_ok": health.schema.ok,
            "schema_error": health.schema.error.map(|e| format!("{e:?}")),
            "schema": {
                "message_store_schema_version": health.schema.schema_version,
            },
        })
    }

    fn coordinator_status_wire(
        status: crate::coordinator::CoordinatorHealthStatus,
    ) -> &'static str {
        match status {
            crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
            crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
            crate::coordinator::CoordinatorHealthStatus::Running => "running",
            crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
        }
    }
}

/// PLACEHOLDER → leader lane(`runtime.{takeover,claim_leader,leader_identity}` 的 CLI 视图)。
/// leader.rs 已有 `claim_leader`/`leader_identity`(返 `LeaseResult`/`Value`);CLI 需 `takeover` +
/// 把 `LeaseResult` 投影成稳定 `--json` 形状。这两步由 leader 集成收口,本层仅声明 CLI 委派面。
pub mod leader_port {
    use super::*;

    /// `runtime.takeover(workspace, team, confirm)` 的 CLI `--json` 投影。
    pub fn takeover(
        workspace: &Path,
        team: Option<&str>,
        confirm: bool,
    ) -> Result<Value, CliError> {
        if !confirm && !positive_caller_pane_env_present() {
            return Ok(json!({
                "ok": false,
                "status": "refused",
                "reason": "confirm_required",
                "action": "rerun with --confirm to claim ownership of this team",
            }));
        }
        if !positive_caller_pane_env_present() {
            let state = crate::state::persist::load_runtime_state(workspace)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            let team_id = resolve_owner_team_id(&state, team)
                .unwrap_or_else(|| TeamKey::new(crate::state::projection::team_state_key(&state)));
            let bind = crate::leader::bind_owner_from_caller_pane(workspace, &team_id, None)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            return Ok(owner_bind_value(bind));
        }
        let result = crate::leader::claim_leader(workspace, team, true)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut value = lease_value(result);
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            emit_topology_convergence_event(workspace, team, "takeover", &value);
            register_after_binding_success(workspace, team, "takeover", &mut value);
        }
        Ok(value)
    }
    /// `runtime.claim_leader(...)` 的 CLI `--json` 投影(`cmd_claim_leader`;含 inbox_hint)。
    pub fn claim_leader(
        workspace: &Path,
        team: Option<&str>,
        confirm: bool,
    ) -> Result<Value, CliError> {
        let state = crate::state::persist::load_runtime_state(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let Some(team_id) = resolve_owner_team_id(&state, team) else {
            return Ok(json!({
                "ok": false,
                "status": "refused",
                "reason": "team_target_unresolved",
                "team": team.unwrap_or(""),
                "hint": "specify an active team id",
            }));
        };
        if !positive_caller_pane_env_present() {
            let bind = crate::leader::bind_owner_from_caller_pane(workspace, &team_id, None)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            if !bind.ok {
                return Ok(owner_bind_refusal_value(bind));
            }
            return Ok(owner_bind_value(bind));
        }
        let result = crate::leader::claim_leader(workspace, team, confirm)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut value = lease_value(result);
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            emit_topology_convergence_event(workspace, team, "claim-leader", &value);
            register_after_binding_success(workspace, team, "claim-leader", &mut value);
        }
        Ok(value)
    }

    /// `runtime.attach_leader(...)` 的 CLI `--json` 投影。
    pub fn attach_leader(
        workspace: &Path,
        team: Option<&str>,
        pane: Option<&crate::transport::PaneId>,
        provider: crate::provider::Provider,
        _confirm: bool,
    ) -> Result<Value, CliError> {
        let result = crate::leader::attach_leader(workspace, team, pane, provider)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let requeued =
            attach_requeued_exhausted_watchers(workspace, result.bound_pane_id.as_ref())?;
        let mut value = attach_lease_value(result, requeued);
        if let Some(obj) = value.as_object_mut() {
            if let Some(team) = team {
                obj.insert("team".to_string(), json!(team));
                obj.insert("team_key".to_string(), json!(team));
            }
        }
        // E7 register-after-success: canonical lease write already
        // finished inside `crate::leader::attach_leader`.
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            register_after_binding_success(workspace, team, "attach-leader", &mut value);
        }
        Ok(value)
    }

    /// `runtime.leader_identity(workspace, team)`(`cmd_identity`)。
    pub fn leader_identity(workspace: &Path, team: Option<&str>) -> Result<Value, CliError> {
        crate::leader::leader_identity(workspace, team)
            .map_err(|e| CliError::Runtime(e.to_string()))
    }

    /// E7 (0.5.9 host-leader-registry-design §14 step 3): after a
    /// canonical binding command succeeds, write a derived discovery
    /// entry to `~/.team-agent/leaders`. Registry write failure is
    /// **never** a binding failure — we only append `leader_registry`
    /// status to the JSON response so operators can see the degradation.
    ///
    /// The receiver payload is read from the JUST-persisted runtime
    /// state so we capture the canonical fields (pane_id, socket,
    /// owner_epoch) after the lease writers finished.
    fn register_after_binding_success(
        workspace: &Path,
        team: Option<&str>,
        source: &str,
        response: &mut Value,
    ) {
        let Ok(state) = crate::state::persist::load_runtime_state(workspace) else {
            return;
        };
        let team_key = match team.filter(|t| !t.is_empty()) {
            Some(t) => t.to_string(),
            None => crate::state::projection::team_state_key(&state),
        };
        let receiver = state
            .get("teams")
            .and_then(|v| v.as_object())
            .and_then(|teams| teams.get(&team_key))
            .and_then(|t| t.get("leader_receiver"))
            .or_else(|| state.get("leader_receiver"))
            .cloned();
        let Some(receiver) = receiver else {
            return;
        };
        let transport_kind = receiver
            .get("transport_kind")
            .and_then(Value::as_str)
            .unwrap_or("direct_tmux")
            .to_string();
        let owner_epoch = receiver
            .get("owner_epoch")
            .and_then(Value::as_u64)
            .or_else(|| {
                state
                    .get("teams")
                    .and_then(|v| v.as_object())
                    .and_then(|teams| teams.get(&team_key))
                    .and_then(|t| t.get("owner_epoch"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0);
        let entry = crate::leader::registry::build_entry(
            workspace,
            &team_key,
            &transport_kind,
            receiver,
            owner_epoch,
            source,
            chrono::Utc::now().to_rfc3339(),
        );
        let event_log = crate::event_log::EventLog::new(workspace);
        let write_result = crate::leader::registry::write_entry_best_effort(&entry);
        let registry_status = match &write_result {
            Some(path) => {
                let _ = event_log.write(
                    crate::leader::registry::EVENT_REGISTERED,
                    json!({
                        "path": path.display().to_string(),
                        "team_key": team_key,
                        "workspace_hash": entry.workspace_hash,
                        "source": source,
                        "owner_epoch": entry.owner_epoch,
                    }),
                );
                json!({"status": "registered", "path": path.display().to_string()})
            }
            None => {
                let _ = event_log.write(
                    crate::leader::registry::EVENT_WRITE_FAILED,
                    json!({
                        "team_key": team_key,
                        "workspace_hash": entry.workspace_hash,
                        "source": source,
                    }),
                );
                json!({"status": "write_failed"})
            }
        };
        if let Some(obj) = response.as_object_mut() {
            obj.insert("leader_registry".to_string(), registry_status);
        }
    }

    fn emit_topology_convergence_event(
        workspace: &Path,
        team: Option<&str>,
        source: &str,
        response: &Value,
    ) {
        let Some(convergence) = response.get("topology_convergence") else {
            return;
        };
        if convergence.get("status").and_then(Value::as_str) != Some("converged") {
            return;
        }
        let team_id = team
            .filter(|team| !team.is_empty())
            .map(str::to_string)
            .or_else(|| {
                crate::state::persist::load_runtime_state(workspace)
                    .ok()
                    .map(|state| crate::state::projection::team_state_key(&state))
            })
            .unwrap_or_else(|| "current".to_string());
        let event_log = crate::event_log::EventLog::new(workspace);
        let _ = event_log.write(
            "leader_receiver.tmux_endpoint_converged",
            json!({
                "team_id": team_id,
                "old_tmux_endpoint": convergence.get("old_tmux_endpoint").cloned().unwrap_or(Value::Null),
                "new_tmux_endpoint": convergence.get("new_tmux_endpoint").cloned().unwrap_or(Value::Null),
                "source": source,
                "reason": convergence.get("reason").and_then(Value::as_str).unwrap_or("old_endpoint_dead"),
                "owner_epoch": convergence.get("owner_epoch").cloned().unwrap_or(Value::Null),
                "persisted": convergence.get("persisted").cloned().unwrap_or(Value::Bool(false)),
                "checked_paths": convergence.get("checked_paths").cloned().unwrap_or_else(|| json!([])),
            }),
        );
    }

    /// E7 GC hook: called from shutdown/unbind success paths.
    pub(crate) fn unregister_after_shutdown_success(
        workspace: &Path,
        team: Option<&str>,
    ) -> Option<PathBuf> {
        let team_key = team.filter(|t| !t.is_empty()).map(str::to_string).or_else(|| {
            crate::state::persist::load_runtime_state(workspace)
                .ok()
                .map(|s| crate::state::projection::team_state_key(&s))
        })?;
        let path = crate::leader::registry::unregister_entry(workspace, &team_key)?;
        let event_log = crate::event_log::EventLog::new(workspace);
        let _ = event_log.write(
            crate::leader::registry::EVENT_UNREGISTERED,
            json!({
                "path": path.display().to_string(),
                "team_key": team_key,
            }),
        );
        Some(path)
    }

    fn owner_bind_value(result: crate::leader::OwnerBindResult) -> Value {
        json!({
            "ok": result.ok,
            "status": if result.ok { "claimed" } else { "refused" },
            "reason": result.reason.map(lease_reason_wire),
            "caller_pane_id": result.caller_pane_id.as_str(),
            "caller_current_command": result.caller_current_command,
            "team_id": result.team_id.as_str(),
            "hint": result.hint,
        })
    }

    fn owner_bind_refusal_value(result: crate::leader::OwnerBindResult) -> Value {
        json!({
            "ok": false,
            "status": "refused",
            "reason": result.reason.map(lease_reason_wire),
            "caller_pane_id": result.caller_pane_id.as_str(),
            "caller_current_command": result.caller_current_command,
            "hint": result.hint,
        })
    }

    fn resolve_owner_team_id(state: &Value, team: Option<&str>) -> Option<TeamKey> {
        match team.filter(|t| !t.is_empty()) {
            Some(team_id) => {
                let current = crate::state::projection::team_state_key(state);
                if current == team_id
                    || state
                        .get("teams")
                        .and_then(|teams| teams.get(team_id))
                        .is_some()
                {
                    Some(TeamKey::new(team_id))
                } else {
                    None
                }
            }
            None => Some(TeamKey::new(crate::state::projection::team_state_key(
                state,
            ))),
        }
    }

    fn positive_caller_pane_env_present() -> bool {
        std::env::var("TMUX_PANE")
            .ok()
            .is_some_and(|pane| !pane.is_empty())
            || std::env::var("TEAM_AGENT_LEADER_PANE_ID")
                .ok()
                .is_some_and(|pane| !pane.is_empty())
    }

    fn team_owner_value(state: &Value, team_id: &TeamKey) -> Option<Value> {
        // Stage 2 (identity-boundary unified plan, architect direction
        // 2026-06-23): route through the ownership repository so all owner
        // reads see the same precedence (teams.<key> > top-level when
        // team_state_key matches). The repository preserves the pre-Stage-2
        // semantics of this helper and adds an `OwnershipSource` tag that
        // diagnose/status surfaces can consume in later stages.
        crate::state::ownership::read_owner_value(state, team_id.as_str()).cloned()
    }

    fn family_a_owner_value(
        result: &crate::leader::OwnerBindResult,
        owner: &crate::leader::TeamOwner,
    ) -> Value {
        json!({
            "pane_id": owner.pane_id.as_str(),
            "leader_session_uuid": owner.leader_session_uuid.as_ref().map(|u| u.as_str()),
            "machine_fingerprint": owner.machine_fingerprint,
            "provider": crate::leader::owner_bind::owner_bind_provider_wire(&result.caller_current_command),
            "os_user": owner.os_user.as_deref().unwrap_or(""),
            "claimed_at": owner.claimed_at,
        })
    }

    fn lease_value(result: crate::leader::LeaseResult) -> Value {
        let mut out = serde_json::Map::new();
        out.insert("ok".to_string(), json!(result.ok));
        out.insert(
            "status".to_string(),
            json!(lease_status_wire(result.status)),
        );
        if let Some(reason) = result.reason {
            out.insert("reason".to_string(), json!(lease_reason_wire(reason)));
        }
        if let Some(action) = result.action {
            out.insert("action".to_string(), json!(action));
        }
        if let Some(epoch) = result.owner_epoch {
            out.insert("owner_epoch".to_string(), json!(epoch.0));
        }
        if let Some(pane) = result.bound_pane_id {
            out.insert("bound_pane_id".to_string(), json!(pane.as_str()));
        }
        if let Some(receiver) = result.receiver {
            out.insert(
                "leader_receiver".to_string(),
                serde_json::to_value(receiver).unwrap_or(Value::Null),
            );
        }
        if let Some(owner) = result.owner {
            out.insert(
                "team_owner".to_string(),
                serde_json::to_value(owner).unwrap_or(Value::Null),
            );
        }
        if let Some(topology_convergence) = result.topology_convergence {
            out.insert("topology_convergence".to_string(), topology_convergence);
        }
        Value::Object(out)
    }

    fn attach_lease_value(result: crate::leader::LeaseResult, requeued: Value) -> Value {
        json!({
            "ok": result.ok,
            "leader_receiver": result
                .receiver
                .map(|receiver| serde_json::to_value(receiver).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            "validation": {
                "ok": result.ok,
                "status": lease_status_wire(result.status),
                "reason": result.reason.map(lease_reason_wire),
                "action": result.action,
            },
            "requeued_exhausted_watchers": requeued,
        })
    }

    fn attach_requeued_exhausted_watchers(
        workspace: &Path,
        _pane_id: Option<&crate::transport::PaneId>,
    ) -> Result<Value, CliError> {
        let events = crate::event_log::EventLog::new(workspace)
            .tail(20)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let event_name = crate::leader::LeaderEvent::ReceiverRequeuedExhaustedWatchers.name();
        for event in events.iter().rev() {
            if event.get("event").and_then(Value::as_str) != Some(event_name) {
                continue;
            }
            return Ok(project_requeued_exhausted_watchers(event));
        }
        Ok(json!([]))
    }

    /// R8 D6 (decoupled for offline byte-lock — c-lite): project the requeued-exhausted event into the
    /// CLI `requeued_exhausted_watchers` return. golden (leader/__init__.py:56): the `watcher_ids`
    /// STRING list. (Current divergent body — the `requeued` Vec<WatcherNotice> objects — kept until
    /// porter-c ports; pinned RED in cli::tests asserts the golden string list.)
    pub(crate) fn project_requeued_exhausted_watchers(event: &Value) -> Value {
        event
            .get("watcher_ids")
            .cloned()
            .unwrap_or_else(|| json!([]))
    }

    fn lease_status_wire(status: crate::leader::LeaseStatus) -> &'static str {
        match status {
            crate::leader::LeaseStatus::AlreadyBound => "already_bound",
            crate::leader::LeaseStatus::Claimed => "claimed",
            crate::leader::LeaseStatus::Refused => "refused",
            crate::leader::LeaseStatus::DryRun => "dry_run",
        }
    }

    fn lease_reason_wire(reason: crate::leader::LeaseReason) -> &'static str {
        match reason {
            crate::leader::LeaseReason::VacantAcquired => "vacant_acquired",
            crate::leader::LeaseReason::PreviousOwnerPaneDead => "previous_owner_pane_dead",
            crate::leader::LeaseReason::PreviousOwnerAliveRefused => "previous_owner_alive_refused",
            crate::leader::LeaseReason::OwnerEpochAdvanced => "owner_epoch_advanced",
            crate::leader::LeaseReason::ForceConfirmRequired => "force_confirm_required",
            crate::leader::LeaseReason::CallerNotLeaderShaped => "caller_not_leader_shaped",
            crate::leader::LeaseReason::CallerPaneNotLive => "caller_pane_not_live",
            crate::leader::LeaseReason::CallerCwdMismatch => "caller_cwd_mismatch",
            crate::leader::LeaseReason::NotInTmuxPane => "not_in_tmux_pane",
            crate::leader::LeaseReason::CallerPaneMissing => "caller_pane_missing",
        }
    }
}
