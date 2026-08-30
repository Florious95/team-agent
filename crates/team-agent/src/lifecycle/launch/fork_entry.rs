//! ---
//! purpose: fork 一席的入口，解析活跃 team 并绑定它实际使用的 tmux socket
//! contract:
//!   provides:
//!     - name: fork_agent
//!       what: 解析 team 与 transport 后转 fork_agent_with_transport
//!   depends:
//!     - crate::state::selector
//!     - crate::lifecycle::restart
//!     - crate::tmux_backend
//! boundary:
//!   - 不自己做窗口注入，注入在 fork_agent.rs
//!   - 不用 workspace 哈希兜底 socket 覆盖已持久化的 endpoint
//! maturity: wired
//! ---
//!
use super::*;

/// ---
/// purpose: fork 一席的对外入口
/// params:
///   source_agent_id: 源席位
///   as_agent_id: 新席位名
///   label: 新席位的角色标签
/// returns: fork 报告
/// errors: 选不到活跃 team 返回 TeamSelect，其余透传 fork_agent_with_transport
/// ---
/// `fork_agent(workspace, source_agent_id, as_agent_id, ...)`(`lifecycle/operations.py:284`)。
/// native session fork(provider 须 supports_session_fork ∧ auth_mode!=compatible_api);
/// 失败回滚,每条失败臂 `adapter.cleanup_mcp`。
pub fn fork_agent(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
) -> Result<ForkAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // Fork-agent routes to the selected live team's persisted endpoint, not
    // the workspace-hash fallback socket.
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    fork_agent_with_transport(
        workspace,
        source_agent_id,
        as_agent_id,
        label,
        open_display,
        team,
        &transport,
    )
}
