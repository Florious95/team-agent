//!
//! purpose: Cursor `agent` CLI argv；role 走 workspace rules 文件，不入 argv
//! contract: MCP 未实现。收到 mcp_config 必须 CapabilityUnsupported，不许静默丢弃
//!   （否则会起出能收信、没有 send_message/report_result 的席位）
//! boundary: 只服务 Provider::CursorAgent。不改 claude/codex/copilot/grok 路径
//!
//! Cursor `agent` CLI provider-local command builders + permission helpers.
//!
//! Mirrors `adapters/claude.rs` skeleton (0.5.67 provider-adapter step), with
//! the append-system-prompt mechanism swapped for the Cursor workspace-rules
//! file (方案 1 变体, smoke PASS 0.5.67-cursor-rules-smoke.md).
//!
//! Cursor `agent` CLI flag map (亲核 attest 0.5.67 + cursor-agent-full-help.txt):
//!   bypass  → `--force`        ("Force allow commands unless explicitly denied"; alias `--yolo`)
//!   model   → `--model <model>` (e.g. sonnet-4-thinking)
//!   resume  → `--resume [chatId]` / `--continue`
//!   workspace → `--workspace <path-or-name>` (defaults to cwd)
//!   prompt  → positional (first arg)
//!
//! **No** `--session-id` / `--fork-session` / `--mcp-config` / `--disallowedTools`
//! flags on the CLI → fresh spawn cannot pre-bind a session id (capture grabs
//! chatId from the first transcript) and `native_mcp_config: false`.
//!
//! TODO(0.5.67): Cursor CLI 用 global-agent 库,只接受 http:// 协议的 proxy env。
//! Team Agent runtime 注入 HTTPS_PROXY=https://... 会 crash。
//! spawn 时必须 unset: HTTPS_PROXY/HTTP_PROXY/ALL_PROXY/NO_PROXY (大小写各一份)。
//! 或走 profile 里 PROXY_MODE=direct 让 profile.rs 兜底(未验证是否覆盖 subscription 路径)。
//! 冒烟证据: .team/artifacts/0.5.67-cursor-rules-smoke.md
//! (本 adapter 只管 argv;env 处理归 launch 路径 profile_env_unset,见 profile_launch.rs。)

use crate::model::enums::AuthMode;
use crate::provider::adapter::BasicProviderAdapter;
use crate::provider::{McpConfig, ProviderError};

pub(crate) fn cursor_agent_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Result<Vec<String>, ProviderError> {
    cursor_agent_base_command(
        adapter,
        auth_mode,
        mcp_config,
        system_prompt,
        model,
        tools,
        false,
        None,
    )
}

pub(crate) fn cursor_agent_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
    managed_mcp_config: bool,
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["agent".to_string()];
    if cursor_agent_dangerous_auto_approve(tools) {
        argv.push("--force".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    // Cursor `--effort` flag does not exist; the framework drops effort at the
    // caller (warning event emitted before construct). Never invent a flag.
    let _ = effort;
    // append_system_prompt 方案 1 变体:system_prompt **不入 argv**,而是经 launch
    // 路径写 `<workspace>/.cursor/rules/team-agent-role-<agent_id>.mdc`
    // (worker_env::apply_cursor_agent_rules_overlay, 同 copilot AGENTS.md 机制)。
    // argv 只带 `--workspace {workspace}`(placeholder 由 fill_spawn_placeholders 替换)。
    if mcp_config.is_some() {
        return Err(ProviderError::CapabilityUnsupported(
            "cursor_agent MCP is not implemented; starting a cursor seat would receive mail with no send_message/report_result. action: use grok/claude/codex/copilot, or wait for a cursor MCP overlay"
                .to_string(),
        ));
    }
    let _ = (adapter, auth_mode, managed_mcp_config, system_prompt);
    argv.push("--workspace".to_string());
    argv.push("{workspace}".to_string());
    Ok(argv)
}

pub(crate) fn cursor_agent_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}
