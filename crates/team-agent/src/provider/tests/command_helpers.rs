//! Exact argv characterization: no helper may trim, escape or reorder caller values.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::model::enums::ProviderEffort;
use crate::provider::adapters::{claude, codex, copilot, cursor_agent, grok, pi};
use crate::provider::ProviderCommandOverrides;

#[test]
fn command_helpers_keep_provider_argv_bytes_and_order() {
    let raw = " \"'\\\n ";
    let mcp = McpConfig { raw: serde_json::json!({}) };
    assert_eq!(claude::claude_base_command(
        &BasicProviderAdapter { provider: Provider::Claude }, AuthMode::Subscription,
        Some(&mcp), Some(raw), Some(""), false, false, Some(ProviderEffort::High),
    ).expect("claude"), ["claude", "--permission-mode", "default", "--model", "", "--effort", "high", "--append-system-prompt", raw, "--mcp-config", "{\"mcpServers\":{}}"]);

    let overrides = ProviderCommandOverrides {
        codex_profile: Some("".to_string()),
        codex_config: vec!["model_reasoning_effort=low".to_string()],
        ..Default::default()
    };
    assert_eq!(codex::codex_base_command(
        Some("resume"), AuthMode::Subscription, Some(&mcp), Some(raw), Some(""),
        true, Some(&overrides), Some(ProviderEffort::Max),
    ), ["codex", "resume", "--no-alt-screen", "--disable", "shell_snapshot", "--disable", "apps", "--profile", "", "--dangerously-bypass-approvals-and-sandbox", "--model", "", "-c", "model_reasoning_effort=low", "-c", "model_reasoning_effort=max", "-c", "developer_instructions=\" \\\"'\\\\\\n \""]);

    assert_eq!(copilot::copilot_base_command(
        AuthMode::Subscription, Some(&mcp), Some(raw), Some(""), true,
    ), ["copilot", "--no-color", "--no-auto-update", "--no-remote", "--disable-builtin-mcps", "--allow-all", "--allow-tool", "team_orchestrator", "--model", "", "--additional-mcp-config", "{}"]);
    assert_eq!(cursor_agent::cursor_agent_base_command(Some(raw), true).expect("cursor"),
        ["agent", "--trust", "--sandbox", "disabled", "--force", "--model", raw, "--workspace", "{workspace}"]);
    assert_eq!(grok::grok_base_command(Some(raw), Some("  model-id  "), true, Some(ProviderEffort::Medium)).expect("grok"),
        ["grok", "--always-approve", "--model", "model-id", "--effort", "medium", "--rules", raw]);
    assert_eq!(grok::grok_base_command(Some(""), Some("  "), false, None).expect("grok empty"), ["grok", "--rules", ""]);
    assert_eq!(pi::build_pi_command_argv(pi::PiCommandRequest {
        extension: std::path::Path::new("extension.ts"), model: Some("vendor/model"),
        effort: Some(ProviderEffort::XHigh), system_prompt: raw,
        session_dir: Some(std::path::Path::new("sessions")),
        session: pi::PiSessionSelector::Fresh { session_id: "session-id" }, agent_id: "worker",
    }).expect("pi"), ["pi", "-e", "extension.ts", "--model", "vendor/model", "--thinking", "xhigh", "--append-system-prompt", raw, "--session-dir", "sessions", "--session-id", "session-id", "--name", "worker"]);
}

#[test]
fn command_helpers_keep_absent_options_and_safe_defaults() {
    assert_eq!(claude::claude_base_command(
        &BasicProviderAdapter { provider: Provider::Claude }, AuthMode::Subscription,
        None, None, None, false, false, None,
    ).expect("claude"), ["claude", "--permission-mode", "default"]);
    assert_eq!(codex::codex_base_command(None, AuthMode::Subscription, None, None, None, false, None, None),
        ["codex", "--no-alt-screen", "--disable", "shell_snapshot", "--disable", "apps"]);
    assert_eq!(copilot::copilot_base_command(AuthMode::Subscription, None, None, None, false),
        ["copilot", "--no-color", "--no-auto-update", "--no-remote", "--disable-builtin-mcps", "--allow-tool", "team_orchestrator"]);
    assert_eq!(cursor_agent::cursor_agent_base_command(None, false).expect("cursor"), ["agent", "--workspace", "{workspace}"]);
    assert_eq!(grok::grok_base_command(None, None, false, None).expect("grok"), ["grok"]);
}
