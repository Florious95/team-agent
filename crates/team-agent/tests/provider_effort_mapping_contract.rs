//! The existing six-level mapper remains the single parsing, wire and admission policy.
use team_agent::model::enums::{Provider, ProviderEffort};

const LEVELS: [(ProviderEffort, &str); 6] = [
    (ProviderEffort::Low, "low"), (ProviderEffort::Medium, "medium"),
    (ProviderEffort::High, "high"), (ProviderEffort::XHigh, "xhigh"),
    (ProviderEffort::Max, "max"), (ProviderEffort::Ultra, "ultra"),
];

#[test]
fn effort_mapping_keeps_all_six_wire_values_and_trim_only_parsing() {
    for (effort, wire) in LEVELS {
        assert_eq!(effort.as_str(), wire);
        assert_eq!(ProviderEffort::parse(wire), Some(effort));
        assert_eq!(ProviderEffort::parse(&format!(" \n{wire}\t")), Some(effort));
        assert_eq!(serde_json::to_string(&effort).unwrap(), format!("\"{wire}\""));
        assert_eq!(serde_json::from_str::<ProviderEffort>(&format!("\"{wire}\"")).unwrap(), effort);
        assert_eq!(ProviderEffort::parse(&wire.to_uppercase()), None);
    }
    for raw in ["", " ", "turbo", "x-high", "medium-high", "high/low"] {
        assert_eq!(ProviderEffort::parse(raw), None);
    }
}

#[test]
fn effort_mapping_keeps_nine_provider_by_six_level_admission_matrix() {
    for provider in [Provider::Claude, Provider::ClaudeCode, Provider::Codex,
        Provider::Copilot, Provider::GeminiCli, Provider::Grok,
        Provider::CursorAgent, Provider::Pi, Provider::Fake] {
        for (effort, _) in LEVELS {
            // This literal contract is deliberately independent of is_supported_by.
            let expected = match (provider, effort) {
                (Provider::CursorAgent, _) => Err("cursor_agent does not support effort; the Cursor CLI has no --effort flag"),
                (Provider::Codex, _) => Ok(Some(effort)),
                (_, ProviderEffort::Ultra) => Err("effort 'ultra' is only supported by codex"),
                (Provider::Claude | Provider::ClaudeCode | Provider::Pi, _) => Ok(Some(effort)),
                (_, ProviderEffort::Max) => Err("effort 'max' is only supported by claude/claude_code/codex/pi"),
                (Provider::Grok, _) => Ok(Some(effort)),
                _ => Ok(None),
            };
            assert_eq!(effort.resolve_for_provider(provider), expected, "{provider:?} {effort:?}");
            assert_eq!(effort.is_supported_by(provider), matches!(expected, Ok(Some(_))), "native support {provider:?} {effort:?}");
        }
    }
}
