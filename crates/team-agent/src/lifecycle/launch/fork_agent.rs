//! ---
//! purpose: legacy 席位的 fork 入口：Pi 以完整会话快照、Claude 以原生 --fork-session 创建独立席位；其余 legacy 席位拒绝
//! contract:
//!   provides:
//!     - name: fork_agent_with_transport
//!       what: 持席位生命周期锁读取源席；Pi 走锁内快照新席位；Claude 走锁内原生 fork 新席位，不再原地 /branch；升级前遗留的 codex/grok worker 行按退役拒绝，其余 legacy provider 报未验证
//!   depends:
//!     - crate::lifecycle::lock
//!     - crate::lifecycle::profile_launch
//! boundary:
//!   - 仅 Pi 与 Claude 的锁内专用管道注册并启动新席位；contract 席位只由 contract_runtime::fork 管道 fork
//!   - 不注入窗口，不改 TEAM_AGENT_ID / MCP / 席位名
//!   - 未验证的 provider 不猜斜杠命令
//! maturity: wired
//! ---

use super::*;
use crate::lifecycle::profile_launch::parse_provider;
use crate::model::enums::Provider;

/// ---
/// purpose: legacy 席位的 fork：Pi / Claude 在锁内创建独立新席位，其余只读源席后拒绝
/// params:
///   source_agent_id: 被 fork 的席位
///   as_agent_id: 新席位名
///   transport: 目标 team 实际使用的 transport
/// returns: Pi / Claude 的新席位 fork 报告；contract 席位在此之前已由 contract 管道处理
/// errors: 选不到 team 返回 TeamSelect，退役 provider 行与其余 legacy provider 返回 Provider
/// ---
/// Claude's in-window `/branch` and Grok's in-window `/fork` are retired:
/// Claude forks into a new seat through its native `--fork-session`
/// (restart::claude_fork), Pi through its full session snapshot
/// (restart::avatar); Codex and Grok fork only through the contract pipeline
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
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let lock_workspace = selected.run_workspace.clone();
    let lock_team_key = selected.team_key.clone();
    let _lifecycle_lock = acquire_seat_lifecycle_lock(LifecycleLockRequest {
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
    // A pre-upgrade Codex or Grok row has no legacy runtime to fork from;
    // contract seats fork through the contract pipeline before this point.
    if crate::provider::wire::is_retired_provider(provider_raw) {
        return Err(LifecycleError::Provider(
            crate::provider::wire::retired_provider_error(provider_raw).into(),
        ));
    }
    match parse_provider(provider_raw) {
        Some(Provider::Pi) => {
            return crate::lifecycle::restart::fork_pi_new_seat_locked(
                &selected,
                source_agent_id,
                as_agent_id,
                label,
                transport,
            );
        }
        Some(Provider::Claude | Provider::ClaudeCode) => {
            return crate::lifecycle::restart::fork_claude_new_seat_locked(
                &selected,
                source_agent_id,
                as_agent_id,
                label,
                open_display,
                transport,
            );
        }
        _ => {}
    }
    Err(LifecycleError::Provider(format!(
        "{provider_raw} native session fork is unverified (未验证); no legacy fork or fresh replacement was performed"
    )))
}
