//! Codex provider-local command builder + permission/sandbox helpers.
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
    tools: &[&str],
    overrides: Option<&ProviderCommandOverrides>,
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
    if codex_dangerous_auto_approve(tools) {
        argv.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    } else {
        argv.push("--sandbox".to_string());
        argv.push(codex_sandbox_mode(tools).to_string());
        argv.push("--ask-for-approval".to_string());
        argv.push("on-request".to_string());
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

pub(crate) fn codex_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}

pub(crate) fn codex_sandbox_mode(tools: &[&str]) -> &'static str {
    if tools
        .iter()
        .any(|tool| matches!(*tool, "fs_write" | "execute_bash"))
    {
        "workspace-write"
    } else {
        "read-only"
    }
}
