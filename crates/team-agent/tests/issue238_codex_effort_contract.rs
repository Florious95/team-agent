//! Issue #238 independent contract tests.
//!
//! The new `ultra` literal is parsed at runtime rather than referenced as an
//! enum variant. This lets the test patch compile on the frozen product
//! baseline and produce a deterministic Initial RED.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::compiler::compile_role_agent;
use team_agent::model::enums::{Provider, ProviderEffort};
use team_agent::model::{spec::validate_spec, yaml, ModelError};

const EFFORT_WIRE: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "ultra"];

fn effort(raw: &str) -> ProviderEffort {
    ProviderEffort::parse(raw)
        .unwrap_or_else(|| panic!("Issue #238 effort literal must parse: {raw:?}"))
}

#[test]
fn r1_six_wire_values_round_trip_and_unknowns_are_strict() {
    for raw in EFFORT_WIRE {
        let parsed = effort(raw);
        assert_eq!(parsed.as_str(), raw);
        let wire = serde_json::to_string(&parsed).expect("serialize effort");
        assert_eq!(wire, format!("\"{raw}\""));
        assert_eq!(serde_json::from_str::<ProviderEffort>(&wire).unwrap(), parsed);
        assert_eq!(ProviderEffort::parse(&format!("  {raw}  ")), Some(parsed));
    }
    for invalid in ["", "Ultra", "MAX", "turbo"] {
        assert!(ProviderEffort::parse(invalid).is_none(), "{invalid:?}");
        assert!(serde_json::from_str::<ProviderEffort>(&format!("\"{invalid}\"")).is_err());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Accepted,
    Ignored,
    Rejected,
}

fn expected_admission(provider: Provider, raw: &str) -> Admission {
    match provider {
        Provider::Codex => Admission::Accepted,
        Provider::Claude | Provider::ClaudeCode | Provider::Pi => {
            if raw == "ultra" {
                Admission::Rejected
            } else {
                Admission::Accepted
            }
        }
        Provider::Grok => match raw {
            "low" | "medium" | "high" | "xhigh" => Admission::Accepted,
            _ => Admission::Rejected,
        },
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => match raw {
            "low" | "medium" | "high" | "xhigh" => Admission::Ignored,
            _ => Admission::Rejected,
        },
    }
}

#[test]
fn r2_eight_by_six_provider_admission_matrix_is_exact() {
    let providers = [
        Provider::Claude,
        Provider::ClaudeCode,
        Provider::Codex,
        Provider::Copilot,
        Provider::GeminiCli,
        Provider::Grok,
        Provider::Pi,
        Provider::Fake,
    ];
    assert_eq!(providers.len(), 8);
    for provider in providers {
        for raw in EFFORT_WIRE {
            let parsed = effort(raw);
            let expected = expected_admission(provider, raw);
            assert_eq!(
                parsed.is_supported_by(provider),
                expected == Admission::Accepted,
                "native support mismatch for {provider:?}/{raw}"
            );
            match (expected, parsed.resolve_for_provider(provider)) {
                (Admission::Accepted, Ok(Some(actual))) => assert_eq!(actual.as_str(), raw),
                (Admission::Ignored, Ok(None)) => {}
                (Admission::Rejected, Err(reason)) => {
                    let expected_reason = if raw == "ultra" {
                        "effort 'ultra' is only supported by codex"
                    } else {
                        "effort 'max' is only supported by claude/claude_code/codex/pi"
                    };
                    assert_eq!(reason, expected_reason, "rejection reason for {provider:?}/{raw}");
                }
                (expected, actual) => panic!(
                    "admission mismatch for {provider:?}/{raw}: expected {expected:?}, got {actual:?}"
                ),
            }
        }
    }
}

static ROLE_SEQ: AtomicU64 = AtomicU64::new(0);

fn role_path(provider: &str, model: Option<&str>, role_effort: Option<&str>) -> PathBuf {
    let id = ROLE_SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("issue238-role-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create isolated role directory");
    let mut text = format!(
        "---\nname: worker\nrole: worker\nprovider: {provider}\ndangerously_skip_permissions: false\n"
    );
    if let Some(model) = model {
        text.push_str(&format!("model: {model}\n"));
    }
    if let Some(effort) = role_effort {
        text.push_str(&format!("effort: {effort}\n"));
    }
    text.push_str("---\nworker\n");
    let path = root.join("worker.md");
    std::fs::write(&path, text).expect("write isolated role document");
    path
}

fn team_meta(provider_effort: Option<&str>) -> yaml::Value {
    match provider_effort {
        Some(value) => yaml::loads(&format!("provider_effort: {value}\n"))
            .expect("team front matter"),
        None => yaml::Value::Map(Vec::new()),
    }
}

fn compiled_effort(
    provider: &str,
    model: Option<&str>,
    role_effort: Option<&str>,
    team_effort: Option<&str>,
) -> Result<Option<String>, ModelError> {
    let path = role_path(provider, model, role_effort);
    let result = compile_role_agent(&path, &team_meta(team_effort), "/tmp/issue238-workspace")
        .map(|compiled| {
            compiled
                .agent
                .get("effort")
                .and_then(yaml::Value::as_str)
                .map(ToString::to_string)
        });
    let _ = std::fs::remove_dir_all(path.parent().expect("role parent"));
    result
}

#[test]
fn r3_compiler_role_wins_team_and_pi_does_not_inherit_team_effort() {
    assert_eq!(
        compiled_effort("codex", None, Some("max"), Some("high"))
            .expect("Codex role max"),
        Some("max".to_string())
    );
    let codex_team_only =
        compiled_effort("codex", None, None, Some("max")).expect("Codex TEAM max");
    if cfg!(unix) {
        // `provider: codex` compiles to the codex_tui contract leaf, which
        // admits only explicit role effort and never inherits TEAM effort.
        assert_eq!(codex_team_only, None, "codex_tui does not inherit TEAM effort");
    } else {
        assert_eq!(codex_team_only, Some("max".to_string()));
    }
    assert_eq!(
        compiled_effort("codex", None, Some("high"), Some("max"))
            .expect("role overrides TEAM"),
        Some("high".to_string())
    );
    assert_eq!(
        compiled_effort("codex", None, Some("ultra"), Some("max"))
            .expect("Codex role ultra"),
        Some("ultra".to_string())
    );

    let pi_model = Some("openai-codex/gpt-5.6-luna");
    assert_eq!(
        compiled_effort("pi", pi_model, Some("max"), Some("high"))
            .expect("Pi role max"),
        Some("max".to_string())
    );
    assert_eq!(
        compiled_effort("pi", pi_model, None, Some("ultra")).expect("Pi TEAM isolation"),
        None,
        "Pi does not inherit TEAM effort or inject a global default"
    );
    let error = compiled_effort("pi", pi_model, Some("ultra"), None)
        .expect_err("Pi ultra must be rejected")
        .to_string();
    assert!(error.contains("effort 'ultra' is only supported by codex"), "{error}");
}

fn direct_spec_text(
    provider: &str,
    model: &str,
    role_effort: Option<&str>,
    team_effort: Option<&str>,
) -> String {
    let mut text = include_str!("../src/model/testdata/team.spec.yaml").to_string();
    if let Some(effort) = team_effort {
        text = text.replacen(
            "  workspace: .\n",
            &format!("  workspace: .\n  provider_effort: {effort}\n"),
            1,
        );
    }
    text = text.replacen(
        "    provider: codex\n    model: null",
        &format!("    provider: {provider}\n    model: {model}"),
        1,
    );
    if let Some(effort) = role_effort {
        text = text.replacen(
            "    dangerously_skip_permissions: false\n",
            &format!("    dangerously_skip_permissions: false\n    effort: {effort}\n"),
            1,
        );
    }
    text
}

fn validate_direct_spec(text: &str) -> Result<(), String> {
    let loaded = yaml::loads(text).map_err(|error| format!("yaml parse: {error}"))?;
    validate_spec(&loaded, Path::new("/tmp/issue238-spec")).map_err(|error| error.to_string())
}

#[test]
fn r4_direct_spec_accepts_new_values_and_team_literal() {
    for (provider, value) in [("codex", "max"), ("codex", "ultra"), ("pi", "max")] {
        let result = validate_direct_spec(&direct_spec_text(provider, "null", Some(value), None));
        assert!(result.is_ok(), "direct spec {provider}/{value}: {result:?}");
    }
    let team_ultra = validate_direct_spec(&direct_spec_text("codex", "null", None, Some("ultra")));
    assert!(team_ultra.is_ok(), "TEAM ultra: {team_ultra:?}");
}

#[test]
fn r4_direct_spec_uses_shared_rejection_reasons_and_keeps_unknowns_unknown() {
    let pi_ultra = validate_direct_spec(&direct_spec_text(
        "pi",
        "openai-codex/gpt-5.6-luna",
        Some("ultra"),
        None,
    ))
    .expect_err("Pi ultra");
    assert!(pi_ultra.contains("effort 'ultra' is only supported by codex"));
    assert!(!pi_ultra.contains("unknown effort"));

    let grok_max = validate_direct_spec(&direct_spec_text("grok", "grok-4.6", Some("max"), None))
        .expect_err("Grok max");
    assert!(
        grok_max.contains("effort 'max' is only supported by claude/claude_code/codex/pi"),
        "{grok_max}"
    );

    let unknown = validate_direct_spec(&direct_spec_text("codex", "null", Some("turbo"), None))
        .expect_err("unknown effort");
    assert!(unknown.contains("/agents/0/effort: unknown effort 'turbo'"));
}

#[test]
fn r8_legacy_ignored_and_protected_provider_contracts_remain_intact() {
    for provider in [Provider::Copilot, Provider::GeminiCli, Provider::Fake] {
        for raw in ["low", "medium", "high", "xhigh"] {
            assert_eq!(effort(raw).resolve_for_provider(provider), Ok(None));
        }
        for raw in ["max", "ultra"] {
            assert!(effort(raw).resolve_for_provider(provider).is_err());
        }
    }
    assert_eq!(
        effort("max").resolve_for_provider(Provider::Claude),
        Ok(Some(effort("max")))
    );
}

#[test]
fn direct_fixture_is_repository_local_and_valid_without_effort() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/model/testdata/team.spec.yaml");
    assert!(fixture.is_file(), "missing fixture: {}", fixture.display());
    let text = direct_spec_text("codex", "null", None, None);
    let loaded = yaml::loads(&text).expect("fixture YAML");
    assert_eq!(loaded.get("version").and_then(yaml::Value::as_i64), Some(1));
    assert_eq!(loaded.get("agents").and_then(yaml::Value::as_list).map(|v| v.len()), Some(3));
    assert!(validate_direct_spec(&text).is_ok());
}

#[test]
fn codex_looking_pi_model_does_not_change_provider_semantics() {
    assert_eq!(
        compiled_effort(
            "pi",
            Some("openai-codex/gpt-5.6-luna"),
            Some("max"),
            None,
        )
        .expect("Pi max"),
        Some("max".to_string())
    );
    assert_eq!(
        effort("max").resolve_for_provider(Provider::Pi),
        Ok(Some(effort("max")))
    );
}

#[test]
fn unknown_wire_values_remain_fail_closed() {
    for raw in ["turbo", "Ultra", ""] {
        assert!(ProviderEffort::parse(raw).is_none());
    }
    assert!(validate_direct_spec(&direct_spec_text("codex", "null", Some("turbo"), None)).is_err());
}

#[test]
fn codex_accepts_all_six_values_at_shared_resolver_surface() {
    for raw in EFFORT_WIRE {
        let parsed = effort(raw);
        assert_eq!(
            parsed.resolve_for_provider(Provider::Codex),
            Ok(Some(parsed)),
            "Codex must admit {raw}"
        );
    }
}
