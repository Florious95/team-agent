//! 0.3.28 layout step 2 — small provider-wire helper for layout-layer window naming.
//!
//! Mirrors `leader::helpers::provider_wire` so the layout module does not
//! depend on the leader module's private helpers. Single source of truth
//! is `provider_command_name` from `crate::provider::adapter` (the
//! authoritative provider→string mapping).

use crate::model::enums::Provider;

/// Window name used inside the leader session, derived from the provider's
/// wire identifier (e.g. `claude`, `codex`, `copilot`, `fake`). Mirrors
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
