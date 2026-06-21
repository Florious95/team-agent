//! unit-7 (Stage 3) — provider session-scan boundary.
//!
//! Dedicated home for provider transcript/backing scanning concerns
//! (today co-located in adapter.rs around lines 511-546 and 969-1078).
//! Created as part of the unit-7 boundary establishment; full relocation
//! of the scanning fns is staged into follow-up commits so each diff
//! stays reviewable.
//!
//! See sibling [`crate::provider::session::capture`] for the existing
//! polling layer (moved under provider/session in unit-6).

/// Marker label for which scan strategy a provider uses. The Codex jsonl
/// scan and the Claude transcript scan share the same outer "decide what
/// to call" hook in adapter.rs; this enum exists to name the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScanStrategy {
    /// Codex rollout jsonl scan (the existing
    /// `agent_rollout_path`-based check).
    CodexJsonl,
    /// Claude transcript scan (per-cwd `.claude/projects/<cwd>/*.jsonl`).
    ClaudeTranscript,
    /// Copilot scan (different home; provider-specific layout).
    CopilotHome,
    /// Provider does not require a backing scan to decide resume — the
    /// session-id alone is the authority.
    NotRequired,
}

impl SessionScanStrategy {
    pub fn for_provider(provider: crate::provider::Provider) -> Self {
        match provider {
            crate::provider::Provider::Codex => Self::CodexJsonl,
            crate::provider::Provider::Claude | crate::provider::Provider::ClaudeCode => {
                Self::ClaudeTranscript
            }
            crate::provider::Provider::Copilot => Self::CopilotHome,
            crate::provider::Provider::GeminiCli => Self::NotRequired,
            crate::provider::Provider::Fake => Self::NotRequired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;

    #[test]
    fn strategy_per_provider() {
        assert_eq!(
            SessionScanStrategy::for_provider(Provider::Codex),
            SessionScanStrategy::CodexJsonl
        );
        assert_eq!(
            SessionScanStrategy::for_provider(Provider::Claude),
            SessionScanStrategy::ClaudeTranscript
        );
        assert_eq!(
            SessionScanStrategy::for_provider(Provider::Fake),
            SessionScanStrategy::NotRequired
        );
    }
}
