//! Provider wire-format helpers — the single source of truth for converting
//! `Provider` ↔ string forms used across state JSON, CLI args, log fields,
//! and the workers we shell out to.
//!
//! This module replaces 7 hand-rolled copies of these match arms that lived
//! in `leader/helpers.rs`, `coordinator/tick.rs` (×2), `lifecycle/restart/common.rs`,
//! `lifecycle/profile_launch.rs`, `lifecycle/launch.rs`, `provider/adapter.rs`,
//! and `lifecycle/restart/rebuild.rs`. Centralising them removes the kind of
//! drift that left `restart/rebuild.rs` with a `claude-code` alias the other
//! sites silently dropped, and shrinks the surface area when a new provider
//! is added.
//!
//! Wire-format invariants (do not change without a coordinated migration):
//!   * `provider_wire(Provider)` returns the canonical snake_case string used
//!     in state JSON. These values are persisted on disk and read by older
//!     runtimes — changing them is a breaking change.
//!   * `parse_provider(&str)` accepts the canonical form AND every historical
//!     alias we have ever written. New aliases go in `aliases()` and become
//!     a parse target automatically.
//!   * `command_name(Provider)` returns the binary name we exec for that
//!     provider. `Claude` and `ClaudeCode` both shell out to `claude` —
//!     they are distinct state values but share an executable.

use crate::model::enums::Provider;

/// Canonical snake_case wire string for `Provider`. Used for state JSON,
/// log fields, and any cross-process serialization. Stable: do not change
/// existing values.
pub(crate) fn provider_wire(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini_cli",
        Provider::Fake => "fake",
    }
}

/// Parse a wire string back to `Provider`. Accepts the canonical
/// `provider_wire` output AND every historical alias listed by `aliases()`.
/// Returns `None` for unknown strings.
pub(crate) fn parse_provider(raw: &str) -> Option<Provider> {
    for &provider in ALL_PROVIDERS {
        for alias in aliases(provider) {
            if *alias == raw {
                return Some(provider);
            }
        }
    }
    None
}

/// All aliases (including the canonical wire form) that parse to `provider`.
/// The canonical wire form is always the first entry, so callers that need
/// the "primary" string can take `aliases(p)[0]` — though most code should
/// use `provider_wire(p)` directly for clarity.
pub(crate) fn aliases(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Claude => &["claude"],
        // `claude-code` (kebab) is a historical alias that appeared in
        // restart/rebuild.rs:1186-1194 only. Keeping it parseable means
        // legacy state files still load.
        Provider::ClaudeCode => &["claude_code", "claude-code"],
        Provider::Codex => &["codex"],
        Provider::Copilot => &["copilot"],
        Provider::GeminiCli => &["gemini_cli"],
        Provider::Fake => &["fake"],
    }
}

/// Executable name we exec for the worker. `Claude` and `ClaudeCode` both
/// resolve to `claude` — the distinction is in state semantics
/// (`ClaudeCode` is the canonical CLI; `Claude` is a legacy alias kept for
/// older specs).
pub(crate) fn command_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "team-agent",
    }
}

const ALL_PROVIDERS: &[Provider] = &[
    Provider::Claude,
    Provider::ClaudeCode,
    Provider::Codex,
    Provider::Copilot,
    Provider::GeminiCli,
    Provider::Fake,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip_for_every_provider() {
        for &p in ALL_PROVIDERS {
            let wire = provider_wire(p);
            assert_eq!(parse_provider(wire), Some(p), "round-trip {p:?}");
        }
    }

    #[test]
    fn claude_code_kebab_alias_parses() {
        // Historical alias preserved from restart/rebuild.rs:1186-1194.
        assert_eq!(parse_provider("claude-code"), Some(Provider::ClaudeCode));
    }

    #[test]
    fn session_capture_uses_wire_parser_not_local_copy() {
        let capture_rs = include_str!("session/capture.rs");
        assert!(
            capture_rs.contains("provider::wire::parse_provider"),
            "session capture must import the canonical provider parser"
        );
        assert!(
            !capture_rs.contains("fn parse_provider("),
            "session capture must not carry a local provider parser copy"
        );
    }

    #[test]
    fn unknown_string_returns_none() {
        assert_eq!(parse_provider(""), None);
        assert_eq!(parse_provider("openai"), None);
        assert_eq!(parse_provider("CLAUDE"), None); // case-sensitive
    }

    #[test]
    fn aliases_first_entry_is_canonical_wire() {
        for &p in ALL_PROVIDERS {
            assert_eq!(aliases(p)[0], provider_wire(p), "canonical first for {p:?}");
        }
    }

    #[test]
    fn command_name_claude_and_claude_code_both_resolve_to_claude_binary() {
        assert_eq!(command_name(Provider::Claude), "claude");
        assert_eq!(command_name(Provider::ClaudeCode), "claude");
    }

    #[test]
    fn command_name_covers_every_provider() {
        for &p in ALL_PROVIDERS {
            assert!(!command_name(p).is_empty(), "{p:?} has no command name");
        }
    }
}
