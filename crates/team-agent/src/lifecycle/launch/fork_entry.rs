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
    let is_pi = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .and_then(|agent| agent.get("provider"))
        .and_then(serde_json::Value::as_str)
        == Some("pi");
    let transport: Box<dyn crate::transport::Transport> = if is_pi {
        #[cfg(test)]
        {
            test_fork_transport(&selected, source_agent_id).unwrap_or(Box::new(
                crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
                    &selected.run_workspace,
                    &selected.state,
                )?
                .backend,
            ))
        }
        #[cfg(not(test))]
        {
            Box::new(
                crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
                    &selected.run_workspace,
                    &selected.state,
                )?
                .backend,
            )
        }
    } else {
        Box::new(
            crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
                &selected.run_workspace,
                Some(selected.team_key.as_str()),
            )
            .unwrap_or_else(|_| {
                crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace)
            }),
        )
    };
    fork_agent_with_transport(
        workspace,
        source_agent_id,
        as_agent_id,
        label,
        open_display,
        team,
        transport.as_ref(),
    )
}

#[cfg(test)]
fn test_fork_transport(
    selected: &crate::state::selector::SelectedTeam,
    source_agent_id: &AgentId,
) -> Option<Box<dyn crate::transport::Transport>> {
    let marker = std::env::var_os("TEST_TMUX_TRANSPORT")?;
    let state = &selected.state;
    let session = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(crate::transport::SessionName::new)?;
    let source = state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()));
    let targets = source
        .and_then(|agent| agent.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
        .map(|pane| {
            let window = source
                .and_then(|agent| agent.get("window"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(source_agent_id.as_str());
            vec![crate::transport::PaneInfo {
                pane_id: crate::transport::PaneId::new(pane),
                session: session.clone(),
                window_index: None,
                window_name: Some(crate::transport::WindowName::new(window)),
                pane_index: None,
                tty: None,
                current_command: None,
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: std::collections::BTreeMap::new(),
            }]
        })
        .unwrap_or_default();
    let endpoint = state
        .get("tmux_endpoint")
        .or_else(|| state.get("tmux_socket"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| marker.to_string_lossy().into_owned());
    Some(Box::new(
        crate::transport::test_support::OfflineTransport::new()
            .with_session_present(true)
            .with_targets(targets)
            .with_tmux_endpoint(endpoint),
    ))
}
