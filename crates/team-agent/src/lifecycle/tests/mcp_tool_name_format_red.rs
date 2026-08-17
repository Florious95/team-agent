//! purpose: worker 契约里的 MCP 工具名必须按 provider 实测书写形式渲染
//! contract:
//!   claude → mcp__team_orchestrator__{send_message,report_result}
//!   grok   → team_orchestrator__{send_message,report_result}
//!   已验证 provider 的渲染结果不得再含 `team_orchestrator.` 点号
//! boundary: 只钉 compile_worker_system_prompt 字面；未验证 provider 不在此猜形式

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::lifecycle::worker_command_context::{compile_worker_system_prompt, WorkerCommandAgent};
use crate::model::enums::Provider;

fn compiled_prompt(provider: Provider) -> String {
    let agent = WorkerCommandAgent::from_json(
        &serde_json::json!({
            "id": "w",
            "role": "developer",
            "tools": ["mcp_team"],
            "output_contract": {"format": "result_envelope_v1"},
        }),
        Some("w"),
        provider,
    )
    .expect("test agent");
    compile_worker_system_prompt(&agent).expect("compile prompt")
}

#[test]
fn claude_runtime_contract_pins_mcp_double_underscore_literals() {
    for provider in [Provider::Claude, Provider::ClaudeCode] {
        let prompt = compiled_prompt(provider);
        assert!(
            prompt.contains("mcp__team_orchestrator__report_result"),
            "{provider:?} must render the claude transcript literal mcp__team_orchestrator__report_result; prompt={prompt}"
        );
        assert!(
            prompt.contains("mcp__team_orchestrator__send_message"),
            "{provider:?} must render the claude transcript literal mcp__team_orchestrator__send_message; prompt={prompt}"
        );
    }
}

#[test]
fn grok_runtime_contract_pins_server_double_underscore_literals() {
    let prompt = compiled_prompt(Provider::Grok);
    assert!(
        prompt.contains("team_orchestrator__report_result"),
        "grok must render team_orchestrator__report_result; prompt={prompt}"
    );
    assert!(
        prompt.contains("team_orchestrator__send_message"),
        "grok must render team_orchestrator__send_message; prompt={prompt}"
    );
    assert!(
        !prompt.contains("mcp__team_orchestrator"),
        "grok must not use the claude mcp__ prefix; prompt={prompt}"
    );
}

#[test]
fn verified_provider_contracts_do_not_teach_dotted_tool_names() {
    for provider in [Provider::Claude, Provider::ClaudeCode, Provider::Grok] {
        let prompt = compiled_prompt(provider);
        assert!(
            !prompt.contains("team_orchestrator."),
            "{provider:?} contract must not contain the dotted form team_orchestrator.; prompt={prompt}"
        );
    }
}
