//! unit-7 (Stage 3) — provider command-assembly boundary.
//!
//! The provider/adapter.rs monolith mixes three concerns:
//!   1. The `ProviderAdapter` TRAIT and facade dispatch (the public API).
//!   2. Per-provider COMMAND ASSEMBLY (argv plan, base/resume flags,
//!      --session-id injection — adapter.rs:409-508 region).
//!   3. SESSION SCANNING (capture polling + transcript scanning —
//!      adapter.rs:511-546, 969-1078).
//!
//! This module CREATES the dedicated home for #2 so future work can
//! migrate command-assembly fns here in small, behavior-neutral commits.
//! Today it only carries the documented intent and a small typed helper —
//! the bulk of the assembly fns are still in adapter.rs and reachable
//! through the existing pub(crate) surface.
//!
//! Why incremental: every command-assembly fn touches multiple internal
//! constants (`--session-id`, `--resume`, MCP-config-path conventions) and
//! per-provider quirks. Moving them blindly is high-risk. The boundary
//! exists so the next commit (or any contributor) has a documented target
//! for relocations.

use crate::provider::Provider;

/// Lightweight tag for command-plan branches in the existing adapter
/// (`Base` vs `Resume`). Distinct type so command-assembly code reads
/// clearly even when it lives next to adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPlanKind {
    /// Initial spawn — `--session-id <uuid>` predetermined.
    Base,
    /// Resume an existing session — `--resume <sid>`, no fresh
    /// `--session-id` injected.
    Resume,
}

/// Returns true when the provider's command plan honors `--session-id`
/// for fresh spawns. Mirrors the existing adapter logic but lives next to
/// where future command-assembly fns will land.
pub fn honors_session_id_for_base_spawn(provider: Provider) -> bool {
    matches!(
        provider,
        Provider::Claude | Provider::ClaudeCode | Provider::Copilot
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_provider_does_not_honor_session_id() {
        assert!(!honors_session_id_for_base_spawn(Provider::Fake));
    }

    #[test]
    fn provider_matrix_for_base_spawn_session_id_support() {
        let cases = [
            (Provider::Claude, true),
            (Provider::ClaudeCode, true),
            (Provider::Copilot, true),
            (Provider::Codex, false),
            (Provider::GeminiCli, false),
            (Provider::Fake, false),
        ];
        for (provider, expected) in cases {
            assert_eq!(
                honors_session_id_for_base_spawn(provider),
                expected,
                "{provider:?}"
            );
        }
    }

    #[test]
    fn command_plan_kind_is_copy() {
        let a = CommandPlanKind::Base;
        let b = a;
        assert_eq!(a, b);
    }
}
