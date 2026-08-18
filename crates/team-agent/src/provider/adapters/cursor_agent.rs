//! ---
//! purpose: Cursor agent CLI argv；role 走 .cursor/rules，MCP 走 mcp.json overlay
//! contract:
//!   provides:
//!     - name: cursor_agent_base_command
//!       what: 只组已实测文档化 flag；mcp_config 不入 argv，由 launch overlay 写盘
//! boundary:
//!   - 不调未文档化 flag（--system-prompt / --allowed-tools）
//!   - 不写 grok 那种 cwd 独占闸
//!   - 不打印代理值
//! maturity: wired
//! ---
//!
//! Cursor `agent` CLI（与 `cursor-agent` 同二进制）。主路径与
//! `.team/scripts/cursor_seat.sh` 实测一致：
//!   `--trust --sandbox disabled --workspace <物理路径> [--force] [--model]`
//! Role 不入 argv，写 `<workspace>/.cursor/rules/*.mdc` + `alwaysApply: true`。
//! MCP 无 `--mcp-config`；身份必须写进 `.cursor/mcp.json` 的 env 表
//! （cursor 不把父进程 TEAM_AGENT_* 传给 MCP 子进程）。
//!
//! 隐藏 flag `--system-prompt` / `--allowed-tools` 实测存在但 help 未列，
//! 随时可能消失。可以在注释里记录，代码里不许调。
//! `--allowed-tools` 正反行为未验证，不写「支持工具白名单」。

use crate::model::enums::AuthMode;
use crate::provider::adapter::BasicProviderAdapter;
use crate::provider::{McpConfig, ProviderError};

pub(crate) fn cursor_agent_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Result<Vec<String>, ProviderError> {
    cursor_agent_base_command(
        adapter,
        auth_mode,
        mcp_config,
        system_prompt,
        model,
        tools,
        false,
        None,
    )
}

pub(crate) fn cursor_agent_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
    managed_mcp_config: bool,
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["agent".to_string()];
    // --trust 跳过 Workspace Trust 闸（已实测）。无此 flag 会停在 Do you trust。
    argv.push("--trust".to_string());
    if cursor_agent_dangerous_auto_approve(tools) {
        argv.push("--force".to_string());
    }
    // 与 cursor_seat.sh 主路径一致。沙箱实际隔离面未再拆，但 flag 本身已实测。
    argv.push("--sandbox".to_string());
    argv.push("disabled".to_string());
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    // Cursor `--effort` flag 不存在；框架在调用方丢掉 effort。绝不发明 flag。
    let _ = effort;
    // system_prompt 不入 argv（help 无 --rules / --append-system-prompt）。
    // launch 写 `<workspace>/.cursor/rules/team-agent-role-<agent_id>.mdc`。
    // mcp_config 也不入 argv（无 --mcp-config）。launch 写 `.cursor/mcp.json`
    // 并 `agent mcp enable team_orchestrator`。这里收下以免静默丢弃。
    let _ = (adapter, auth_mode, mcp_config, managed_mcp_config, system_prompt);
    argv.push("--workspace".to_string());
    argv.push("{workspace}".to_string());
    Ok(argv)
}

pub(crate) fn cursor_agent_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}
