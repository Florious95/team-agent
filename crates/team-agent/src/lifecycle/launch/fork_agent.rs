//! ---
//! purpose: legacy 席位的 fork 入口；legacy 席位一律拒绝，新席位 fork 只走 contract 管道
//! contract:
//!   provides:
//!     - name: fork_agent_with_transport
//!       what: 持席位生命周期锁读取源席；升级前遗留的 claude/codex/grok/pi worker 行按退役拒绝，其余 legacy provider 报未验证
//!   depends:
//!     - crate::lifecycle::lock
//! boundary:
//!   - 新席位 fork 只由 contract_runtime::fork 管道完成，这里不注册新席位、不注入窗口
//!   - 不 spawn pane，不改 TEAM_AGENT_ID / MCP / 席位名
//!   - 未验证的 provider 不猜斜杠命令
//! maturity: wired
//! ---

use super::*;

/// ---
/// purpose: legacy 席位的 fork：只读源席后拒绝，不做任何副作用
/// params:
///   source_agent_id: 被 fork 的席位
///   as_agent_id: 新席位名
///   transport: 目标 team 实际使用的 transport（legacy 席位不再注入）
/// returns: 不返回成功；contract 席位在此之前已由 contract 管道处理
/// errors: 选不到 team 返回 TeamSelect，退役 provider 行与其余 legacy provider 返回 Provider
/// ---
/// The Grok in-window `/fork` was the last legacy fork; Claude Code, Codex,
/// Grok and Pi fork only into a new seat through the contract pipeline
/// (`contract_runtime::fork`).
pub fn fork_agent_with_transport(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let _ = (as_agent_id, label, open_display, transport);
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let lock_workspace = selected.run_workspace.clone();
    let lock_team_key = selected.team_key.clone();
    let _lifecycle_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &lock_workspace,
        operation: "fork-agent",
        team: Some(lock_team_key.as_str()),
        agent_id: Some(source_agent_id),
    })?;
    let selected = crate::state::selector::resolve_active_team(
        &lock_workspace,
        Some(lock_team_key.as_str()),
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let agent = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}"))
        })?;
    let provider_raw = agent
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    // A pre-upgrade Claude Code, Codex, Grok or Pi row has no legacy runtime
    // to fork from; contract seats fork through the contract pipeline before
    // this point.
    if crate::provider::wire::is_retired_provider(provider_raw) {
        return Err(LifecycleError::Provider(
            crate::provider::wire::retired_provider_error(provider_raw).into(),
        ));
    }
    Err(LifecycleError::Provider(format!(
        "{provider_raw} native session fork is unverified (未验证); no legacy fork or fresh replacement was performed"
    )))
}
