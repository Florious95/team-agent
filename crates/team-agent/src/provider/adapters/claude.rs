//!
//! Claude / ClaudeCode provider-local command builders + permission helpers.
//!
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2). Pure
//! extraction — byte-identical to the original inline forms. Scope kept
//! small on purpose: base command + launch wrapper +
//! launch wrapper. Auth hints (`claude_auth_hint`) and the context-aware model
//! resolver (`claude_context_model`) stay in `adapter.rs`; capture scanning
//! helpers now live under `provider/session_scan/claude.rs`.

use crate::model::enums::{AuthMode, Provider};
use crate::provider::adapter::{next_session_token, prompt_needs_native_mcp, BasicProviderAdapter};
use crate::provider::{McpConfig, ProviderAdapter, ProviderError};
use crate::provider::command_helpers::{append_opt_pair, append_pair};

pub(crate) fn claude_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = claude_base_command(
        adapter,
        auth_mode,
        mcp_config,
        system_prompt,
        model,
        dangerously_skip_permissions,
        false,
        None,
    )?;
    append_pair(&mut argv, "--session-id", next_session_token());
    Ok(argv)
}

pub(crate) fn claude_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
    managed_mcp_config: bool,
    // 0.4.x provider effort MVP step 5: when Some, inject `--effort <level>`
    // immediately after the model (before prompt/MCP).
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["claude".to_string()];
    if dangerously_skip_permissions {
        // 0.5.66 bypass 单源:flag 由 provider_bypass_flag 表供给(单源)。
        let flag = crate::provider::bypass_flags::provider_bypass_flag(Provider::Claude)
            .expect("claude provider must define a bypass flag");
        argv.push(flag.to_string());
    } else {
        append_pair(&mut argv, "--permission-mode", "default");
    }
    append_opt_pair(&mut argv, "--model", model);
    if let Some(effort) = effort {
        argv.push("--effort".to_string());
        argv.push(effort.as_str().to_string());
    }
    append_opt_pair(&mut argv, "--append-system-prompt", system_prompt);
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
        append_pair(&mut argv, "--mcp-config", raw.to_string());
    }
    Ok(argv)
}
