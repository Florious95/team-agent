//! Exact argv characterization: no helper may trim, escape or reorder caller values.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::model::enums::ProviderEffort;
use crate::provider::adapters::{claude, copilot, pi};

#[test]
fn command_helpers_keep_provider_argv_bytes_and_order() {
    let raw = " \"'\\\n ";
    let mcp = McpConfig { raw: serde_json::json!({}) };
    assert_eq!(claude::claude_base_command(
        &BasicProviderAdapter { provider: Provider::Claude }, AuthMode::Subscription,
        Some(&mcp), Some(raw), Some(""), false, false, Some(ProviderEffort::High),
    ).expect("claude"), ["claude", "--permission-mode", "default", "--model", "", "--effort", "high", "--append-system-prompt", raw, "--mcp-config", "{\"mcpServers\":{}}"]);

    assert_eq!(copilot::copilot_base_command(
        AuthMode::Subscription, Some(&mcp), Some(raw), Some(""), true,
    ), ["copilot", "--no-color", "--no-auto-update", "--no-remote", "--disable-builtin-mcps", "--allow-all", "--allow-tool", "team_orchestrator", "--model", "", "--additional-mcp-config", "{}"]);
    assert_eq!(pi::build_pi_command_argv(pi::PiCommandRequest {
        extension: std::path::Path::new("extension.ts"), model: Some("vendor/model"),
        effort: Some(ProviderEffort::XHigh), system_prompt: raw,
        session_dir: Some(std::path::Path::new("sessions")),
        session: pi::PiSessionSelector::Fresh, agent_id: "worker",
    }).expect("pi"), ["pi", "-e", "extension.ts", "--model", "vendor/model", "--thinking", "xhigh", "--append-system-prompt", raw, "--session-dir", "sessions", "--name", "worker"]);
}

#[test]
fn command_helpers_keep_absent_options_and_safe_defaults() {
    assert_eq!(claude::claude_base_command(
        &BasicProviderAdapter { provider: Provider::Claude }, AuthMode::Subscription,
        None, None, None, false, false, None,
    ).expect("claude"), ["claude", "--permission-mode", "default"]);
    assert_eq!(copilot::copilot_base_command(AuthMode::Subscription, None, None, None, false),
        ["copilot", "--no-color", "--no-auto-update", "--no-remote", "--disable-builtin-mcps", "--allow-tool", "team_orchestrator"]);
}
