//!
//! Codex provider-local command builder.
//!
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2). Pure
//! extraction — byte-identical to the original inline forms. Shared helper
//! `json_inline` stays in `adapter.rs` because it is also used by other
//! sites; this file reaches it via `super::*`.

use crate::model::enums::AuthMode;
use crate::provider::adapter::json_inline;
use crate::provider::{McpConfig, ProviderCommandOverrides};

pub(crate) fn codex_base_command(
    subcommand: Option<&str>,
    _auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
    overrides: Option<&ProviderCommandOverrides>,
    // 0.4.x provider effort MVP step 6: when Some, inject
    // `-c model_reasoning_effort=<level>` AFTER existing profile
    // codex_config overrides — explicit launch effort wins over profile.
    // Codex does not support `max`; the caller filters that case via
    // `ProviderEffort::is_supported_by` before reaching this point.
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Vec<String> {
    let mut argv = vec!["codex".to_string()];
    if let Some(subcommand) = subcommand {
        argv.push(subcommand.to_string());
    }
    argv.extend([
        "--no-alt-screen".to_string(),
        "--disable".to_string(),
        "shell_snapshot".to_string(),
        "--disable".to_string(),
        "apps".to_string(),
    ]);
    if let Some(profile) = overrides.and_then(|o| o.codex_profile.as_deref()) {
        argv.push("--profile".to_string());
        argv.push(profile.to_string());
    }
    if dangerously_skip_permissions {
        // 0.5.66 bypass 单源:flag 由 provider_bypass_flag 表供给。
        let flag = crate::provider::bypass_flags::provider_bypass_flag(crate::model::enums::Provider::Codex)
            .expect("codex provider must define a bypass flag");
        argv.push(flag.to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(overrides) = overrides {
        for config in &overrides.codex_config {
            argv.push("-c".to_string());
            argv.push(config.clone());
        }
    }
    if let Some(effort) = effort {
        argv.push("-c".to_string());
        argv.push(format!("model_reasoning_effort={}", effort.as_str()));
    }
    if let Some(prompt) = system_prompt {
        // codex.py:120 — escape order matters: backslash first, then quote, then newline.
        let escaped = prompt
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        argv.push("-c".to_string());
        argv.push(format!("developer_instructions=\"{escaped}\""));
    }
    if let Some(config) = mcp_config {
        append_codex_mcp_overrides(&mut argv, &config.raw);
    }
    argv
}

/// Render an `McpConfig::raw` ({ name: { type, command, args, env: {...} } }) into Codex
/// `-c mcp_servers.<name>.<field>=...` overrides. JSON values are stringified with serde
/// so arrays/objects survive (Codex parses the right-hand side as JSON; this is what the
/// Python golden + the live attached Codex panes do).
pub(crate) fn append_codex_mcp_overrides(argv: &mut Vec<String>, raw: &serde_json::Value) {
    let Some(servers) = raw.as_object() else {
        return;
    };
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            continue;
        };
        for (key, value) in obj {
            if key == "env" {
                if let Some(env) = value.as_object() {
                    for (env_key, env_value) in env {
                        argv.push("-c".to_string());
                        argv.push(format!(
                            "mcp_servers.{name}.env.{env_key}={}",
                            json_inline(env_value)
                        ));
                    }
                }
                continue;
            }
            argv.push("-c".to_string());
            argv.push(format!("mcp_servers.{name}.{key}={}", json_inline(value)));
        }
        // Every MCP server gets a 600s tool timeout so long-running
        // team_orchestrator calls (report_result etc.) survive the codex default.
        argv.push("-c".to_string());
        argv.push(format!("mcp_servers.{name}.tool_timeout_sec=600.0"));
    }
}
