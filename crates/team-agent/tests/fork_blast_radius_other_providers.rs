//! ---
//! purpose: 钉死未验证 provider 不会被猜成 grok 的 /fork 窗口命令
//! contract:
//!   provides:
//!     - name: A9-other-providers-unchanged
//!       what: 只有 grok+subscription 返回 /fork；claude/codex/copilot/cursor/gemini 为 None
//! boundary:
//!   - 不把「函数存在」当通过
//!   - 不给未验证 provider 填推断斜杠命令
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use team_agent::lifecycle::launch::in_window_fork_command;
use team_agent::model::enums::{AuthMode, Provider};

#[test]
fn grok_subscription_uses_official_slash_fork() {
    assert_eq!(
        in_window_fork_command(Provider::Grok, AuthMode::Subscription),
        Some("/fork"),
        "grok subscription must use the official in-window /fork command"
    );
}

#[test]
fn grok_compatible_api_does_not_get_slash_fork() {
    assert_eq!(
        in_window_fork_command(Provider::Grok, AuthMode::CompatibleApi),
        None,
        "compatible_api must keep the refuse path, not guess /fork"
    );
}

#[test]
fn claude_does_not_get_in_window_fork_command() {
    for provider in [Provider::Claude, Provider::ClaudeCode] {
        for auth in [AuthMode::Subscription, AuthMode::CompatibleApi, AuthMode::OfficialApi] {
            assert_eq!(
                in_window_fork_command(provider, auth),
                None,
                "{provider:?} must not be given a guessed in-window fork command; auth={auth:?}"
            );
        }
    }
}

#[test]
fn codex_does_not_get_in_window_fork_command() {
    for auth in [AuthMode::Subscription, AuthMode::CompatibleApi] {
        assert_eq!(
            in_window_fork_command(Provider::Codex, auth),
            None,
            "codex is unverified — do not invent a slash fork; auth={auth:?}"
        );
    }
}

#[test]
fn copilot_cursor_gemini_stay_unverified_no_slash_fork() {
    for provider in [
        Provider::Copilot,
        Provider::CursorAgent,
        Provider::GeminiCli,
    ] {
        assert_eq!(
            in_window_fork_command(provider, AuthMode::Subscription),
            None,
            "{provider:?} is unverified — no guessed in-window fork command"
        );
    }
}
