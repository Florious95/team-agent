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
//! → `native_mcp_config: false`; MCP reaches the worker via profile env only.

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
    // intentionally absent (native_mcp_config: false, MCP via profile env).
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
    if !tools.contains(&"execute_bash") {
        disallowed.push("Bash");
    }
    if !tools.contains(&"fs_read") {
        disallowed.push("Read");
    }
    if !tools.contains(&"fs_write") {
        disallowed.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
    }
    if !tools.contains(&"fs_list") {
        disallowed.extend(["Glob", "Grep"]);
    }
    disallowed
}
