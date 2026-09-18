//!
//! Copilot provider-local command builders + permission helpers.
//!
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2). Pure
//! extraction — byte-identical to the original inline forms. Scope kept
//! small: base command + resume + permission flags + MCP type→transport
//! translation. Auth hint (`copilot_auth_hint`) stays in `adapter.rs`;
//! session-store scanning lives under `provider/session_scan/copilot.rs`.

use crate::model::enums::AuthMode;
use crate::provider::McpConfig;

pub(crate) fn copilot_base_command(
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
) -> Vec<String> {
    let _ = (auth_mode, system_prompt);
    let mut argv = vec![
        "copilot".to_string(),
        // Noise control trio + disable remote control (防 GitHub web 远控 worker)
        "--no-color".to_string(),
        "--no-auto-update".to_string(),
        "--no-remote".to_string(),
        // P0: disable built-in github-mcp-server. Residual risk covered by
        // spawn-time `copilot mcp list` scan + per-name --disable-mcp-server.
        "--disable-builtin-mcps".to_string(),
    ];
    if dangerously_skip_permissions {
        // 0.5.66 bypass 单源:flag 由 provider_bypass_flag 表供给。
        let flag = crate::provider::bypass_flags::provider_bypass_flag(crate::model::enums::Provider::Copilot)
            .expect("copilot provider must define a bypass flag");
        argv.push(flag.to_string());
    }
    // mcp_team ∈ canonical → approval-free (whole-server pattern).
    argv.push("--allow-tool".to_string());
    argv.push("team_orchestrator".to_string());
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(config) = mcp_config {
        // Copilot mcp config schema field is `transport` (stdio|http|sse),
        // not the canonical `type`. McpConfig.raw is canonical; only copilot
        // translates type→transport for --additional-mcp-config.
        argv.push("--additional-mcp-config".to_string());
        argv.push(copilot_translate_mcp_config(&config.raw).to_string());
    }
    argv
}

/// Translate `McpConfig.raw` canonical schema (`type`) into the copilot
/// `mcp add`/`--additional-mcp-config` expected `transport` field
/// (stdio|http|sse). Shared by inline plans and lifecycle config files;
/// claude/codex paths leave canonical schema untouched.
pub(crate) fn copilot_translate_mcp_config(raw: &serde_json::Value) -> serde_json::Value {
    let Some(servers) = raw.as_object() else {
        return raw.clone();
    };
    let mut translated = serde_json::Map::new();
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            translated.insert(name.clone(), server.clone());
            continue;
        };
        let mut out = serde_json::Map::new();
        for (key, value) in obj {
            if key == "type" {
                out.insert("transport".to_string(), value.clone());
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
        translated.insert(name.clone(), serde_json::Value::Object(out));
    }
    serde_json::Value::Object(translated)
}

/// Resume path = base + `--resume <sid>` (drop --session-id); kept separate
/// to avoid the plan accidentally emitting --session-id and --resume in the
/// same frame.
pub(crate) fn copilot_base_command_resume(
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
) -> Vec<String> {
    copilot_base_command(auth_mode, mcp_config, system_prompt, model, dangerously_skip_permissions)
}
