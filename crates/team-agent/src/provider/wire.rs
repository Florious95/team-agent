//!
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

/// A state row written by a release that still ran a legacy worker engine:
/// Cursor's `cursor_agent`/`agent` or Codex's `codex`/`codex_cli`.
/// No legacy worker runtime exists for it now; it must never fall back
/// to another provider. Role and CLI declarations of these names select the
/// contract leaf, whose state row carries the leaf id instead. A leader is not
/// an `agents` row, so its own provider key is unaffected. Claude Code
/// (`claude`/`claude_code`) and Pi (`pi`) rows keep their legacy runtimes.
pub(crate) const RETIRED_PROVIDER_ERROR: &str =
    "legacy cursor_agent seat has no runtime in this release; stop or remove it, then add it again (provider: cursor)";
const RETIRED_CODEX_ERROR: &str =
    "legacy codex seat has no runtime in this release; stop or remove it, then add it again (provider: codex)";

pub(crate) fn is_retired_provider(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "cursor_agent" | "agent" | "codex" | "codex_cli" | "codex-cli"
    )
}

/// The refusal for a retired state row, naming the provider to re-add it with.
pub(crate) fn retired_provider_error(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "codex" | "codex_cli" | "codex-cli" => RETIRED_CODEX_ERROR,
        _ => RETIRED_PROVIDER_ERROR,
    }
}

pub(crate) fn require_active_state_providers(
    state: &serde_json::Value,
    target: Option<&str>,
) -> Result<(), &'static str> {
    let retired = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| {
            agents.iter().find_map(|(id, row)| {
                row.get("provider")
                    .and_then(serde_json::Value::as_str)
                    .filter(|provider| {
                        target.is_none_or(|target| target == id) && is_retired_provider(provider)
                    })
            })
        });
    match retired {
        Some(provider) => Err(retired_provider_error(provider)),
        None => Ok(()),
    }
}

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
        Provider::Grok => "grok",
        Provider::Pi => "pi",
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

/// Parse only canonical persisted provider wire strings.
///
/// Use this for spec/schema surfaces that historically rejected aliases such as
/// `claude-code`; use [`parse_provider`] when reading state/log fields where
/// historical aliases must remain accepted.
pub(crate) fn parse_canonical_provider(raw: &str) -> Option<Provider> {
    all_providers()
        .iter()
        .copied()
        .find(|provider| provider_wire(*provider) == raw)
}

pub(crate) fn all_providers() -> &'static [Provider] {
    ALL_PROVIDERS
}

pub(crate) fn is_claude_family(provider: Provider) -> bool {
    matches!(provider, Provider::Claude | Provider::ClaudeCode)
}

pub(crate) fn requires_resume_backing(provider: Provider) -> bool {
    matches!(
        provider,
        Provider::Codex
            | Provider::Claude
            | Provider::ClaudeCode
            | Provider::Copilot
            | Provider::Grok
            | Provider::Pi
    )
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
        Provider::Grok => &["grok"],
        Provider::Pi => &["pi"],
        Provider::Fake => &["fake"],
    }
}

/// Executable name we exec for the worker. `Claude` and `ClaudeCode` both
/// resolve to `claude` — the distinction is in state semantics
/// (`ClaudeCode` is the canonical CLI; `Claude` is a legacy alias kept for
/// older specs). Spawn this public name. Do not replace it with a
/// canonicalized deep binary; that skips the user's wrapper.
pub(crate) fn command_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini",
        Provider::Grok => "grok",
        Provider::Pi => "pi",
        Provider::Fake => "team-agent",
    }
}

const ALL_PROVIDERS: &[Provider] = &[
    Provider::Claude,
    Provider::ClaudeCode,
    Provider::Codex,
    Provider::Copilot,
    Provider::GeminiCli,
    Provider::Grok,
    Provider::Pi,
    Provider::Fake,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_wire_round_trip_all_providers() {
        for &p in ALL_PROVIDERS {
            let wire = provider_wire(p);
            assert_eq!(parse_provider(wire), Some(p), "round-trip {p:?}");
            assert_eq!(
                parse_canonical_provider(wire),
                Some(p),
                "canonical parse {p:?}"
            );
            for alias in aliases(p) {
                assert_eq!(parse_provider(alias), Some(p), "alias {alias:?} -> {p:?}");
            }
        }
    }

    #[test]
    fn claude_code_kebab_alias_parses() {
        // Historical alias preserved from restart/rebuild.rs:1186-1194.
        assert_eq!(parse_provider("claude-code"), Some(Provider::ClaudeCode));
    }

    #[test]
    fn provider_wire_migrated_sites_compile_without_local_provider_clones() {
        let capture_rs = include_str!("session/capture.rs");
        let restart_selection_rs = include_str!("../lifecycle/restart/selection.rs");
        let model_spec_rs = include_str!("../model/spec.rs");
        let compiler_rs = include_str!("../compiler.rs");
        let delivery_rs = include_str!("../messaging/delivery.rs");
        let rediscover_rs = include_str!("../leader/rediscover.rs");
        let profile_smoke_rs = include_str!("../lifecycle/profile_smoke.rs");
        let diagnose_rs = include_str!("../cli/diagnose.rs");

        assert!(
            capture_rs.contains("provider::wire") && capture_rs.contains("parse_provider"),
            "session capture must import the canonical provider parser"
        );
        assert!(
            !capture_rs.contains("fn parse_provider("),
            "session capture must not carry a local provider parser copy"
        );
        assert!(
            capture_rs.contains("is_claude_family"),
            "session capture must use typed Claude-family detection"
        );
        assert!(
            restart_selection_rs.contains("parse_canonical_provider")
                && restart_selection_rs.contains("requires_resume_backing"),
            "restart selection must route resumable backing through typed provider helpers"
        );
        assert!(
            !restart_selection_rs.contains(r#""codex" | "claude" | "claude_code" | "copilot""#),
            "restart selection must not keep a local resumable provider string match"
        );
        assert!(
            !model_spec_rs.contains("SUPPORTED_PROVIDERS"),
            "model spec must not keep a local provider whitelist"
        );
        assert!(
            model_spec_rs.contains("parse_canonical_provider")
                && model_spec_rs.contains("resolve_for_provider"),
            "model spec must validate providers through typed helpers"
        );
        assert!(
            compiler_rs.contains("parse_canonical_provider"),
            "compiler must validate providers through typed helpers"
        );
        assert!(
            delivery_rs.contains("parse_canonical_provider")
                && delivery_rs.contains("parse_provider")
                && delivery_rs.contains("provider_wire"),
            "message delivery provider dispatch must use wire helpers"
        );
        for (path, source) in [
            ("leader/rediscover.rs", rediscover_rs),
            ("lifecycle/profile_smoke.rs", profile_smoke_rs),
            ("cli/diagnose.rs", diagnose_rs),
        ] {
            assert!(
                !source.contains("fn provider_wire("),
                "{path} must not carry a local provider_wire clone"
            );
            assert!(
                source.contains("provider::wire"),
                "{path} must import provider wire helpers"
            );
        }
    }

    #[test]
    fn provider_wire_command_grammar_sites_are_named_helpers_not_provider_parse() {
        let provider_attribution_rs = include_str!("../leader/provider_attribution.rs");
        let worker_window_helpers_rs = include_str!("../layout/worker_window_helpers.rs");
        let runtime_detectors_rs = include_str!("../coordinator/runtime_detectors.rs");
        let emit_rs = include_str!("../cli/emit.rs");
        let messaging_helpers_rs = include_str!("../messaging/helpers.rs");

        assert!(
            provider_attribution_rs.contains("COMMAND_GRAMMAR_PROVIDER_COMMANDS")
                && provider_attribution_rs.contains("provider_from_command_text"),
            "leader provider attribution must name command grammar explicitly"
        );
        assert!(
            worker_window_helpers_rs.contains("provider_window_name")
                && worker_window_helpers_rs.contains("Command/UI grammar"),
            "worker window helpers must keep a named window grammar helper"
        );
        assert!(
            runtime_detectors_rs.contains("runtime_event_provider_name")
                && runtime_detectors_rs.contains("Event-field grammar"),
            "runtime detectors must name event provider grammar explicitly"
        );
        assert!(
            emit_rs.contains("LEADER_PASSTHROUGH_COMMANDS")
                && emit_rs.contains("is_leader_passthrough_command"),
            "CLI emit must name passthrough command grammar explicitly"
        );
        assert!(
            messaging_helpers_rs.contains("non_provider_command")
                && messaging_helpers_rs.contains("Activity command grammar"),
            "messaging helpers must name activity command grammar explicitly"
        );
        for (path, source) in [
            ("leader/provider_attribution.rs", provider_attribution_rs),
            ("layout/worker_window_helpers.rs", worker_window_helpers_rs),
            ("coordinator/runtime_detectors.rs", runtime_detectors_rs),
            ("cli/emit.rs", emit_rs),
            ("messaging/helpers.rs", messaging_helpers_rs),
        ] {
            assert!(
                !source.contains("parse_canonical_provider"),
                "{path} is command/UI grammar and must not reuse provider identity parsing"
            );
        }
    }

    #[test]
    fn unknown_string_returns_none() {
        assert_eq!(parse_provider(""), None);
        assert_eq!(parse_provider("openai"), None);
        assert_eq!(parse_provider("CLAUDE"), None); // case-sensitive
    }

    #[test]
    fn retired_state_guard_is_scoped_to_the_addressed_seat() {
        let state = serde_json::json!({"agents": {
            "pi": {"provider": "pi"},
            "claude": {"provider": "claude"},
            "claude_code": {"provider": "claude_code"},
            "copilot": {"provider": "copilot"},
            "retired": {"provider": "cursor_agent"},
            "codex": {"provider": "codex_tui"},
            "grok": {"provider": "grok_build"},
            "legacy_codex": {"provider": "codex"},
            "legacy_codex_cli": {"provider": "codex-cli"},
            "legacy_grok": {"provider": "grok"}
        }});
        assert!(require_active_state_providers(&state, Some("pi")).is_ok());
        assert!(require_active_state_providers(&state, Some("claude")).is_ok());
        assert!(require_active_state_providers(&state, Some("claude_code")).is_ok());
        assert!(require_active_state_providers(&state, Some("copilot")).is_ok());
        assert!(require_active_state_providers(&state, Some("codex")).is_ok());
        assert!(require_active_state_providers(&state, Some("grok")).is_ok());
        assert_eq!(require_active_state_providers(&state, Some("retired")), Err(RETIRED_PROVIDER_ERROR));
        assert_eq!(require_active_state_providers(&state, Some("legacy_codex")), Err(RETIRED_CODEX_ERROR));
        assert_eq!(require_active_state_providers(&state, Some("legacy_codex_cli")), Err(RETIRED_CODEX_ERROR));
        assert!(require_active_state_providers(&state, Some("legacy_grok")).is_ok());
        assert!(!is_retired_provider("grok"));
        assert!(require_active_state_providers(&state, None).is_err());
        for retired in ["cursor_agent", "agent"] {
            assert_eq!(parse_provider(retired), None, "no legacy engine to parse into");
        }
        // The leader keys still parse: a leader is never an `agents` row.
        for leader in ["codex"] {
            assert!(is_retired_provider(leader), "{leader}");
            assert!(parse_provider(leader).is_some(), "{leader}");
        }
        // Claude Code and Pi keep their legacy worker runtimes.
        for legacy in ["claude", "claude_code", "claude-code", "pi"] {
            assert!(!is_retired_provider(legacy), "{legacy}");
            assert!(parse_provider(legacy).is_some(), "{legacy}");
        }
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
