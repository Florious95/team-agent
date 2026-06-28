//! Claude / ClaudeCode provider-local command builders + permission helpers.
//!
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2). Pure
//! extraction — byte-identical to the original inline forms. Scope kept
//! small on purpose: base command + permission/disallowed-tool mapping +
//! launch wrapper. Auth hints (`claude_auth_hint`), capture-related
//! helpers (`claude_projects_dir_for_cwd`, `encode_claude_projects_dir`,
//! `rollout_path_has_claude_leader_marker`,
//! `claude_records_have_leader_marker`), and the context-aware model
//! resolver (`claude_context_model`) stay in `adapter.rs` because they
//! depend on adapter-private utilities (capture scanning, `command_on_path`,
//! `ProfileLaunchContext`). A later step can move those into a session
//! store / auth descriptor hook.

use crate::model::enums::AuthMode;
use crate::provider::adapter::{
    next_session_token, prompt_needs_native_mcp, BasicProviderAdapter,
};
use crate::provider::{McpConfig, ProviderAdapter, ProviderError};

pub(crate) fn claude_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Result<Vec<String>, ProviderError> {
    let mut argv = claude_base_command(
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

pub(crate) fn claude_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
    managed_mcp_config: bool,
    // 0.4.x provider effort MVP step 5: when Some, inject `--effort <level>`
    // immediately after the model (before prompt/MCP).
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["claude".to_string()];
    if claude_dangerous_auto_approve(tools) {
        argv.push("--dangerously-skip-permissions".to_string());
    } else {
        argv.push("--permission-mode".to_string());
        argv.push("default".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(effort) = effort {
        argv.push("--effort".to_string());
        argv.push(effort.as_str().to_string());
    }
    if let Some(prompt) = system_prompt {
        argv.push("--append-system-prompt".to_string());
        argv.push(prompt.to_string());
    }
    if !managed_mcp_config
        && (mcp_config.is_some()
            || auth_mode == AuthMode::CompatibleApi
            || system_prompt.is_some_and(prompt_needs_native_mcp))
    {
        let raw = if let Some(config) = mcp_config {
            serde_json::json!({"mcpServers": config.raw.clone()})
        } else {
            serde_json::json!({"mcpServers": adapter.mcp_config(auth_mode)?.raw})
        };
        argv.push("--mcp-config".to_string());
        argv.push(raw.to_string());
    }
    for tool in claude_disallowed_tools(tools) {
        argv.push("--disallowedTools".to_string());
        argv.push(tool.to_string());
    }
    Ok(argv)
}

pub(crate) fn claude_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}

pub(crate) fn claude_disallowed_tools(tools: &[&str]) -> Vec<&'static str> {
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
