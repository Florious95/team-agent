//!
//! 0.5.66 bypass 单源统一 — provider → bypass argv flag 配置表。
//!
//! **唯一权威**:角色 md 的 `dangerously_skip_permissions: true` 决定是否加 bypass flag;
//! 本文件只回答"某个 provider 的 bypass 命令行参数是什么"。
//!
//! 数据源自旧 `lifecycle/launch/approval.rs::dangerous_leader_flags()` 表迁来:
//!   - claude/claude_code → `--dangerously-skip-permissions`
//!   - codex → `--dangerously-bypass-approvals-and-sandbox`
//!   - copilot → `--allow-all`(0.3.27 P1 E54 symptom 2 起)
//!   - grok → `--always-approve`
//!   - cursor_agent → `--force`
//!
//! 未定义(返回 None)的 provider:调用侧必须 fail-loud(见
//! `resolved_tool_strings_for_command`),不得静默 fallback。

use crate::model::enums::Provider;

/// 某 provider 的 bypass argv flag;未定义 → `None`。
pub(crate) fn provider_bypass_flag(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Claude | Provider::ClaudeCode => Some("--dangerously-skip-permissions"),
        Provider::Codex => Some("--dangerously-bypass-approvals-and-sandbox"),
        Provider::Copilot => Some("--allow-all"),
        Provider::Grok => Some("--always-approve"),
        Provider::CursorAgent => Some("--force"),
        // TODO: 查 gemini_cli 的等价 bypass 参数;未定义前 fail-loud。
        Provider::GeminiCli => None,
        Provider::Fake => None,
    }
}

/// 0.5.66 §3.2 跨 workspace 兼容警示用:扫 **team-agent 进程自己的** argv tokens
/// 找已知 bypass flag。**只做检测,不做行为决策**(0.6.0 删)。
///
/// 不认 `--force`:那是 team-agent 自己的 CLI 旗(`init --force` /
/// `remove-agent --confirm --force` / `restart --force`),与 cursor worker
/// 的 `--force` 撞名。本函数看不到「当前在看哪个 provider 的 argv」,
/// 认 `--force` 会把既有非 grok/cursor 路径误判成 bypass。
/// cursor worker 的 bypass 仍走 [`provider_bypass_flag`],不走这条检测面。
/// `--always-approve` 可以认:team-agent 自己没有这个旗。
///
/// TODO(0.6.0): remove
pub(crate) fn detect_bypass_flag_in_argv(argv_tokens: &[String]) -> Option<&'static str> {
    for token in argv_tokens {
        if token == "--dangerously-skip-permissions"
            || token == "--dangerously-skip-permission"
        {
            return Some("--dangerously-skip-permissions");
        }
        if token == "--dangerously-bypass-approvals-and-sandbox" {
            return Some("--dangerously-bypass-approvals-and-sandbox");
        }
        if token == "--allow-all" || token == "--yolo" {
            return Some("--allow-all");
        }
        if token == "--always-approve" {
            return Some("--always-approve");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_family_bypass_flag() {
        assert_eq!(
            provider_bypass_flag(Provider::Claude),
            Some("--dangerously-skip-permissions")
        );
        assert_eq!(
            provider_bypass_flag(Provider::ClaudeCode),
            Some("--dangerously-skip-permissions")
        );
    }

    #[test]
    fn codex_and_copilot_bypass_flags() {
        assert_eq!(
            provider_bypass_flag(Provider::Codex),
            Some("--dangerously-bypass-approvals-and-sandbox")
        );
        assert_eq!(provider_bypass_flag(Provider::Copilot), Some("--allow-all"));
    }

    #[test]
    fn undefined_providers_return_none() {
        assert_eq!(provider_bypass_flag(Provider::GeminiCli), None);
        assert_eq!(provider_bypass_flag(Provider::Fake), None);
    }

    #[test]
    fn grok_and_cursor_bypass_flags() {
        assert_eq!(
            provider_bypass_flag(Provider::Grok),
            Some("--always-approve")
        );
        assert_eq!(
            provider_bypass_flag(Provider::CursorAgent),
            Some("--force")
        );
    }

    #[test]
    fn detect_bypass_flag_ignores_team_agent_force() {
        let argv = [
            "team-agent".to_string(),
            "remove-agent".to_string(),
            "x".to_string(),
            "--confirm".to_string(),
            "--force".to_string(),
        ];
        assert_eq!(
            detect_bypass_flag_in_argv(&argv),
            None,
            "detect_bypass_flag_in_argv scans team-agent process argv, not worker argv; \
             `remove-agent --force` must not look like a provider bypass flag; got {:?}",
            detect_bypass_flag_in_argv(&argv)
        );
    }
}
