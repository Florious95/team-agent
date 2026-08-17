//!
//! purpose: Grok CLI argv + permission deny 映射；MCP 不走 CLI flag
//! contract: launch 写 `<cwd>/.grok/config.toml`；同一 cwd 只允许一个 grok 席；
//!   未登录 / 目录未信任必须拒绝启动。未知工具映射记成 Unsupported，不发明 deny 名
//! boundary: 只服务 Provider::Grok。不改 claude/codex/copilot 路径
//!
//! Grok CLI provider-local command builders + permission helpers.
//!
//! Mirrors `adapters/claude.rs` (0.5.67 provider-adapter step). Pure
//! flag-name adaptation over the claude skeleton — no new abstraction
//! layers, no public-API changes outside the provider dispatch.
//!
//! Grok CLI flag map (亲核 attest 0.5.67 + grok-full-help.txt):
//!   bypass  → `--always-approve`  ("Auto-approve all tool executions")
//!   system  → `--rules <RULES>`   ("Extra rules to append to the system prompt")
//!   model   → `-m, --model <MODEL>`
//!   session → `-s, --session-id <SESSION_ID>` (fresh spawn)
//!   resume  → `-r, --resume [<ID_OR_TITLE>]` / `-c, --continue`
//!   fork    → `--fork-session` (with --resume)
//!   effort  → `--reasoning-effort <EFFORT>` (alias `--effort`)
//!   deny    → `--disallowed-tools` (compat alias `--disallowedTools`)
//!   cwd     → `--cwd <CWD>` / `-w, --worktree [<WORKTREE>]`
//!
//! No native `--mcp-config` flag on the Grok CLI (`grok mcp` is a subcommand)
//! → `native_mcp_config: false`; MCP reaches the worker via launch-path
//! `<cwd>/.grok/config.toml` (`apply_grok_mcp_overlay`).

use crate::model::enums::AuthMode;
use crate::provider::adapter::{next_session_token, BasicProviderAdapter};
use crate::provider::{McpConfig, ProviderError};

pub(crate) fn grok_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Result<Vec<String>, ProviderError> {
    let mut argv = grok_base_command(
        adapter,
        auth_mode,
        mcp_config,
        system_prompt,
        model,
        tools,
        false,
        None,
    )?;
    argv.push("--session-id".to_string());
    argv.push(next_session_token());
    Ok(argv)
}

pub(crate) fn grok_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
    managed_mcp_config: bool,
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["grok".to_string()];
    if grok_dangerous_auto_approve(tools) {
        argv.push("--always-approve".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(effort) = effort {
        // Grok accepts `--effort` as alias for `--reasoning-effort`.
        argv.push("--effort".to_string());
        argv.push(effort.as_str().to_string());
    }
    if let Some(prompt) = system_prompt {
        argv.push("--rules".to_string());
        argv.push(prompt.to_string());
    }
    // Grok CLI has no `--mcp-config` flag — the claude inline-MCP block is
    // intentionally absent. Launch writes `<cwd>/.grok/config.toml`.
    let _ = (adapter, auth_mode, mcp_config, managed_mcp_config);
    for tool in grok_disallowed_tools(tools) {
        argv.push("--disallowedTools".to_string());
        argv.push(tool.to_string());
    }
    Ok(argv)
}

pub(crate) fn grok_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}

pub(crate) fn grok_disallowed_tools(tools: &[&str]) -> Vec<&'static str> {
    let mut disallowed = Vec::new();
    for tool in [
        "execute_bash",
        "fs_read",
        "fs_write",
        "fs_list",
        "network",
        "git_diff",
        "mcp_team",
        "provider_builtin",
    ] {
        if tools.contains(&tool) {
            continue;
        }
        match grok_tool_mapping(tool) {
            GrokToolMapping::Deny(names) => disallowed.extend(names),
            GrokToolMapping::Unsupported | GrokToolMapping::Bypass => {}
        }
    }
    disallowed
}

/// Canonical tool → grok CLI deny names. Unknown tools stay Unsupported
/// (no invented `--disallowedTools` token).
pub(crate) fn grok_tool_mapping(tool: &str) -> GrokToolMapping {
    match tool {
        "execute_bash" => GrokToolMapping::Deny(&["Bash"]),
        "fs_read" => GrokToolMapping::Deny(&["Read"]),
        "fs_write" => GrokToolMapping::Deny(&["Edit", "Write", "MultiEdit", "NotebookEdit"]),
        "fs_list" => GrokToolMapping::Deny(&["Glob", "Grep"]),
        "dangerous_auto_approve" => GrokToolMapping::Bypass,
        "network" | "git_diff" | "mcp_team" | "provider_builtin" => GrokToolMapping::Unsupported,
        _ => GrokToolMapping::Unsupported,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokToolMapping {
    Deny(&'static [&'static str]),
    Bypass,
    Unsupported,
}
