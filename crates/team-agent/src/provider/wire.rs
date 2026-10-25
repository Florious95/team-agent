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

pub(crate) const RETIRED_PROVIDER_ERROR: &str =
    "retired provider cursor_agent/agent is unsupported; configure provider: cursor explicitly";

pub(crate) const LEGACY_CODEX_ERROR: &str =
    "legacy provider codex is retired; configure provider: codex with contract_runtime: true";

pub(crate) fn is_retired_provider(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "cursor_agent" | "agent"
    )
}

/// Old Codex spellings. The exact contract id `codex` is not in this set by
/// itself: admission depends on [`legacy_codex_refused`].
pub(crate) fn is_legacy_codex_key(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "codex" | "codex_cli" | "codex-cli"
    )
}

/// Refuse a legacy Codex key before any side effect.
///
/// Exact `codex` plus an already-decided contract opt-in stays on the contract
/// leaf. Every other spelling, including `CODEX` and `codex_cli`, is refused
/// even when the opt-in flag is set.
pub(crate) fn legacy_codex_refused(raw: &str, contract_opt_in: bool) -> bool {
    let trimmed = raw.trim();
    is_legacy_codex_key(trimmed) && !(trimmed == "codex" && contract_opt_in)
}

pub(crate) fn json_contract_opt_in(row: &serde_json::Value) -> bool {
    row.get("contract_runtime") == Some(&serde_json::Value::Bool(true))
        || row.get("runtime_family").and_then(serde_json::Value::as_str) == Some("contract")
}

/// Inspect declared provider keys even when malformed YAML would become plain text.
pub(crate) fn has_retired_provider_header(raw: &str) -> bool {
    let (meta, _) = crate::compiler::split_front_matter(raw);
    if let Some(provider) = meta.get("provider") {
        return provider.as_str().is_some_and(is_retired_provider);
    }
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let Some(header) = normalized.strip_prefix("---\n") else {
        return false;
    };
    header
        .lines()
        .take_while(|line| *line != "---")
        .any(|line| {
            line == line.trim_start()
                && line.split_once(':').is_some_and(|(key, value)| {
                    key.trim().trim_matches(['\'', '"']) == "provider"
                        && is_retired_provider(
                            value
                                .split('#')
                                .next()
                                .unwrap_or("")
                                .trim()
                                .trim_matches(['\'', '"']),
                        )
                })
        })
}

/// Top-level provider key only. A contract Codex role is not a legacy key.
/// Body text that mentions `provider: codex` does not match.
pub(crate) fn has_legacy_codex_header(raw: &str) -> bool {
    let (meta, _) = crate::compiler::split_front_matter(raw);
    if meta.get("provider").is_some() || meta.get("contract_runtime").is_some() {
        let provider = meta.get("provider").and_then(|value| value.as_str()).unwrap_or("");
        let opt_in = matches!(
            meta.get("contract_runtime"),
            Some(crate::model::yaml::Value::Bool(true))
        ) || meta.get("runtime_family").and_then(|value| value.as_str()) == Some("contract");
        return legacy_codex_refused(provider, opt_in);
    }
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let Some(header) = normalized.strip_prefix("---\n") else {
        return false;
    };
    let mut provider = None;
    let mut opt_in = false;
    for line in header.lines().take_while(|line| *line != "---") {
        if line != line.trim_start() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['\'', '"']);
        let value = value
            .split('#')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(['\'', '"']);
        match key {
            "provider" => provider = Some(value.to_string()),
            "contract_runtime" if matches!(value, "true" | "True" | "TRUE") => opt_in = true,
            "runtime_family" if value.eq_ignore_ascii_case("contract") => opt_in = true,
            _ => {}
        }
    }
    provider.is_some_and(|provider| legacy_codex_refused(&provider, opt_in))
}

pub(crate) fn require_active_state_providers(
    state: &serde_json::Value,
    target: Option<&str>,
) -> Result<(), &'static str> {
    if state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.iter().any(|(id, row)| {
                target.is_none_or(|target| target == id)
                    && row_refuses_provider(row)
            })
        })
    {
        return Err(if state_has_legacy_codex(state, target) {
            LEGACY_CODEX_ERROR
        } else {
            RETIRED_PROVIDER_ERROR
        });
    }
    Ok(())
}

fn row_refuses_provider(row: &serde_json::Value) -> bool {
    let Some(provider) = row.get("provider").and_then(serde_json::Value::as_str) else {
        return false;
    };
    is_retired_provider(provider) || legacy_codex_refused(provider, json_contract_opt_in(row))
}

fn state_has_legacy_codex(state: &serde_json::Value, target: Option<&str>) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.iter().any(|(id, row)| {
                target.is_none_or(|target| target == id)
                    && row
                        .get("provider")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|provider| {
                            legacy_codex_refused(provider, json_contract_opt_in(row))
                        })
            })
        })
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
/// older specs).
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
    fn retired_headers_check_top_level_without_rejecting_active_roles() {
        for provider in ["pi", "claude", "claude_code"] {
            let role = format!("---\nprovider: {provider}\nexample:\n  provider: agent\nnotes: |\n  provider: cursor_agent\n---\nprovider: agent\n");
            assert!(!has_retired_provider_header(&role));
        }
        assert!(!has_retired_provider_header("---\nexample:\n  provider: agent\n---\n"));
        for provider in ["cursor_agent", "agent"] {
            assert!(has_retired_provider_header(&format!("---\nprovider: {provider}\n---\n")));
            assert!(has_retired_provider_header(&format!("---\nprovider: {provider}\nmissing closing delimiter")));
            assert_eq!(parse_provider(provider), None);
        }
    }

    #[test]
    fn retired_state_guard_is_scoped_to_the_addressed_seat() {
        let state = serde_json::json!({"agents": {
            "pi": {"provider": "pi"},
            "claude": {"provider": "claude"},
            "retired": {"provider": "cursor_agent"}
        }});
        assert!(require_active_state_providers(&state, Some("pi")).is_ok());
        assert!(require_active_state_providers(&state, Some("claude")).is_ok());
        assert!(require_active_state_providers(&state, Some("retired")).is_err());
        assert!(require_active_state_providers(&state, None).is_err());
    }

    #[test]
    fn legacy_codex_refusal_is_case_insensitive_and_ignores_role_prose() {
        for key in ["codex", "CODEX", "Codex", "codex_cli", "codex-cli", " Codex "] {
            assert!(legacy_codex_refused(key, false), "{key}");
            assert!(legacy_codex_refused(key.trim(), key.trim() != "codex"), "{key}");
        }
        assert!(!legacy_codex_refused("codex", true));
        assert!(legacy_codex_refused("CODEX", true));
        assert!(!legacy_codex_refused("pi", false));
        assert!(!legacy_codex_refused("claude", true));
        assert!(!legacy_codex_refused("cursor", false));
        let pi = "---\nprovider: pi\nexample:\n  provider: codex\nnotes: |\n  provider: CODEX\n---\nprovider: codex\n";
        assert!(!has_legacy_codex_header(pi));
        assert!(!has_legacy_codex_header("---\nprovider: claude\n---\n"));
        assert!(!has_legacy_codex_header(
            "---\nprovider: codex\ncontract_runtime: true\n---\n"
        ));
        assert!(has_legacy_codex_header("---\nprovider: codex\n---\n"));
        assert!(has_legacy_codex_header("---\nprovider: CODEX\ncontract_runtime: true\n---\n"));
        assert!(has_legacy_codex_header("---\nprovider: codex_cli\n---\nmissing"));
        let state = serde_json::json!({"agents": {
            "pi": {"provider": "pi", "notes": "provider: codex"},
            "claude": {"provider": "claude"},
            "cursor": {"provider": "cursor", "contract_runtime": true},
            "contract_codex": {"provider": "codex", "contract_runtime": true},
            "legacy": {"provider": "CODEX"}
        }});
        assert!(require_active_state_providers(&state, Some("pi")).is_ok());
        assert!(require_active_state_providers(&state, Some("claude")).is_ok());
        assert!(require_active_state_providers(&state, Some("cursor")).is_ok());
        assert!(require_active_state_providers(&state, Some("contract_codex")).is_ok());
        assert_eq!(
            require_active_state_providers(&state, Some("legacy")),
            Err(LEGACY_CODEX_ERROR)
        );
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
