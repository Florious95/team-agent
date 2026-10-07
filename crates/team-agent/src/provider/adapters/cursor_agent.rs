//! ---
//! purpose: Cursor agent CLI argv；role 走 .cursor/rules，MCP 走 mcp.json overlay
//! contract:
//!   provides:
//!     - name: cursor_agent_base_command
//!       what: 只组已实测文档化 flag；MCP 由 launch overlay 写盘
//! boundary:
//!   - 不调未文档化 flag（--system-prompt / --allowed-tools）
//!   - 不写 grok 那种 cwd 独占闸
//!   - 不打印代理值
//! maturity: wired
//! ---
//!
//! Cursor `agent` CLI（与 `cursor-agent` 同二进制）。默认不跳过信任或沙箱设置。
//! 显式 bypass 时才使用 `.team/scripts/cursor_seat.sh` 的 trusted/unsandboxed 选项：
//!   `--workspace <物理路径> [--trust --sandbox disabled --force] [--model]`
//! Role 不入 argv，写 `<workspace>/.cursor/rules/*.mdc` + `alwaysApply: true`。
//! MCP 无 `--mcp-config`；身份必须写进 `.cursor/mcp.json` 的 env 表
//! （cursor 不把父进程 TEAM_AGENT_* 传给 MCP 子进程）。
//!
//! 隐藏 flag `--system-prompt` / `--allowed-tools` 实测存在但 help 未列，
//! 随时可能消失。可以在注释里记录，代码里不许调。
//! `--allowed-tools` 正反行为未验证，不写「支持工具白名单」。

use crate::provider::ProviderError;

pub(crate) fn cursor_agent_base_command(
    model: Option<&str>,
    dangerously_skip_permissions: bool,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["agent".to_string()];
    if dangerously_skip_permissions {
        // Explicit bypass also opts into Cursor's trusted, unsandboxed launch.
        argv.push("--trust".to_string());
        argv.push("--sandbox".to_string());
        argv.push("disabled".to_string());
        argv.push("--force".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    // Cursor has no `--effort`, system-prompt, or MCP config flag.
    // Launch materializes role rules and the `.cursor/mcp.json` overlay.
    argv.push("--workspace".to_string());
    argv.push("{workspace}".to_string());
    Ok(argv)
}
