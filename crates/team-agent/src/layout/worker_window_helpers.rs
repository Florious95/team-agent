//! 0.3.28 layout step 2 — small window grammar helper for layout-layer naming.
//!
//! Command/UI grammar, not provider identity parsing: leader window names
//! intentionally collapse `claude_code` to `claude` and use executable-style
//! labels such as `gemini`.

use crate::model::enums::Provider;

/// Window name used inside the leader session. Mirrors
/// Python's `leader_window_name = provider` (see
/// `runtime/0.2.11/src/team_agent/leader/__init__.py:114-131`).
pub fn provider_window_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "fake",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_window_names_are_lowercase_wire_strings() {
        assert_eq!(provider_window_name(Provider::Claude), "claude");
        assert_eq!(provider_window_name(Provider::ClaudeCode), "claude");
        assert_eq!(provider_window_name(Provider::Codex), "codex");
        assert_eq!(provider_window_name(Provider::Copilot), "copilot");
    }
}
