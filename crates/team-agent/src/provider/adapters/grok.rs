//!
//! purpose: Grok CLI argv + permission deny 映射；MCP 不走 CLI flag
//! contract: launch 写 `<cwd>/.grok/config.toml`；未登录 / 目录未信任必须拒绝启动。
//!   未指定 model 时不注入 `--model`，由 Grok CLI 使用原生默认。
//!   未知工具映射记成 Unsupported，不发明 deny 名
//! mcp_injection: dir_scoped —— 每席必须独立工作目录；同 cwd 多席会静默串台，故拒绝。
//!   对照 claude/codex 是 argv 作用域（`--mcp-config`），同目录多席没有这个问题。
//!   本版 exclusive 检查不读 per-agent cwd，一个 workspace 只支持一个 grok 席。
//!   这是 Provider::Grok 的能力边界，不是框架对所有 provider 的限制。
//! boundary: 只服务 Provider::Grok。不改 claude/codex/copilot 路径
//!
//! Grok CLI provider-local command builders.
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
//!   effort  → `--effort <level>` (alias of `--reasoning-effort`)
//!             grok accepts low|medium|high|xhigh; CLI rejects `max`
//!   cwd     → `--cwd <CWD>` / `-w, --worktree [<WORKTREE>]`
//!
//! No native `--mcp-config` flag on the Grok CLI (`grok mcp` is a subcommand)
//! → `native_mcp_config: false`; MCP reaches the worker via launch-path
//! `<cwd>/.grok/config.toml` (`apply_grok_mcp_overlay`).

use crate::provider::adapter::next_session_token;
use crate::provider::ProviderError;
use crate::provider::command_helpers::{append_opt_pair, append_pair};

pub(crate) fn grok_launch_command(
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = grok_base_command(system_prompt, model, dangerously_skip_permissions, None)?;
    append_pair(&mut argv, "--session-id", next_session_token());
    Ok(argv)
}

pub(crate) fn grok_base_command(
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["grok".to_string()];
    if dangerously_skip_permissions {
        argv.push("--always-approve".to_string());
    }
    append_opt_pair(&mut argv, "--model", model.map(str::trim).filter(|value| !value.is_empty()));
    if let Some(effort) = effort {
        // Grok accepts `--effort` as alias for `--reasoning-effort`.
        argv.push("--effort".to_string());
        argv.push(effort.as_str().to_string());
    }
    append_opt_pair(&mut argv, "--rules", system_prompt);
    // Grok CLI has no `--mcp-config` flag — the claude inline-MCP block is
    // intentionally absent. Launch writes `<cwd>/.grok/config.toml`.
    Ok(argv)
}
