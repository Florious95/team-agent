//! Independent F01–F21 contract tests for provider model discovery.
//!
//! These tests intentionally use only the public CLI and public launch contracts. The
//! expected search results are fixed by the fixture rows below; no production matcher is
//! called to calculate an oracle. Run on the frozen baseline to establish product RED.
//!
//! `models --provider claude|claude_code|pi` (and the default `pi`) now read the Claude Code
//! and Pi contract-leaf catalogs; the legacy Claude/Pi catalog contracts left with those
//! engines. The fake `claude`/`pi` shims stay on PATH only to prove Codex isolation.

#[cfg(unix)]
mod unix_tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use team_agent::model::enums::{AuthMode, Provider, ProviderEffort};
    use team_agent::provider::{get_adapter, ProviderCommandContext};

    const REQUEST_ID: &str = "team-agent-model-catalog";
    const RAW_SENTINEL: &str = "RAW_CATALOG_SECRET_SENTINEL";
    const MAX_CATALOG_BYTES: usize = 1024 * 1024;
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    const FAKE_PROVIDER: &str = r#"#!/bin/sh
set -eu
name=${0##*/}
printf '%s\n' "$name" >> "$TRACE_DIR/$name.calls"
printf '%s\0' "$@" > "$TRACE_DIR/$name.argv"
printf '%s\n' "$PWD" > "$TRACE_DIR/$name.pwd"
if [ "$name" = claude ]; then
  if [ "${CLAUDECODE+x}" = x ]; then printf 'present\n'; else printf 'absent\n'; fi > "$TRACE_DIR/claudecode"
  /bin/cat > "$TRACE_DIR/claude.stdin"
fi
if [ "$name" = codex ] && [ -n "${FAKE_CODEX_SLEEP:-}" ]; then
  printf '%s\n' "$$" > "$TRACE_DIR/codex.pid"
  exec /bin/sleep "$FAKE_CODEX_SLEEP"
fi
if [ "$name" = codex ] && [ -n "${FAKE_LATE_PIPE_SLEEP:-}" ]; then
  /bin/sleep "$FAKE_LATE_PIPE_SLEEP" &
  printf '%s\n' "$!" > "$TRACE_DIR/descendant.pid"
  exit 0
fi
if [ -n "${FAKE_STDERR_FILE:-}" ] && [ -f "$FAKE_STDERR_FILE" ]; then
  /bin/cat "$FAKE_STDERR_FILE" >&2
fi
case "$name" in
  codex) source_file=$CODEX_CATALOG_FILE ;;
  claude) source_file=$CLAUDE_CATALOG_FILE ;;
  pi) source_file=$PI_CATALOG_FILE ;;
  *) exit 64 ;;
esac
if [ -f "$source_file" ]; then /bin/cat "$source_file"; fi
exit "${FAKE_EXIT_CODE:-0}"
"#;

    const VISIBLE_CODEX_ROWS: &[(&str, &str)] = &[
        ("gpt-6-astra", "GPT-6 Astra"),
        ("gpt-5.6-sol", "GPT-5.6 Sol"),
        ("gpt-5.6-terra", "GPT-5.6 Terra"),
        ("gpt-5.6-luna", "GPT-5.6 Luna"),
        ("gpt-5.5", "GPT-5.5"),
        ("gpt-5.6-luna@1m", "GPT-5.6 Luna 1M"),
        ("gpt-5.6-astra-eval", "Shared Preview"),
        ("gpt-5.6-astra-lab", "Shared Preview"),
        ("GPT-5.6-Astra+Case", "Case-preserving Astra"),
    ];

    // HermeticTestEnv-equivalent boundary: each fixture owns a process-unique root; CLI
    // children receive env_clear plus fixture-owned HOME/PATH/TMPDIR/workspace, providers
    // are local stubs, and Drop removes only this exact root (no tmux/coordinator access).
    struct Fixture {
        root: PathBuf,
        bin: PathBuf,
        home: PathBuf,
        temp: PathBuf,
        workspace: PathBuf,
        trace: PathBuf,
        codex_catalog: PathBuf,
        claude_catalog: PathBuf,
        pi_catalog: PathBuf,
        stderr_file: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "model-fuzzy-lookup-red-{}-{sequence}",
                std::process::id()
            ));
            let bin = root.join("bin");
            let home = root.join("home");
            let temp = root.join("tmp");
            let workspace = root.join("workspace");
            let trace = root.join("trace");
            for directory in [&bin, &home, &temp, &workspace, &trace] {
                fs::create_dir_all(directory).expect("create isolated fixture directory");
            }
            let fixture = Self {
                codex_catalog: root.join("codex-catalog.json"),
                claude_catalog: root.join("claude-catalog.jsonl"),
                pi_catalog: root.join("pi-catalog.txt"),
                stderr_file: root.join("provider-stderr.txt"),
                root,
                bin,
                home,
                temp,
                workspace,
                trace,
            };
            for executable in ["codex", "claude", "pi"] {
                fixture.write_executable(executable, FAKE_PROVIDER);
            }
            fixture.write_executable("agent", r#"#!/bin/sh
printf '%s\n' retired-entrypoint >> "$TRACE_DIR/agent.calls"
exit 77
"#);
            fixture.set_codex(&codex_catalog_text());
            fixture.set_claude(&claude_catalog_text(&claude_rows()));
            fixture.set_pi("provider model\nopenai-codex gpt-5.6-sol\nopenai-codex gpt-5.6-luna\n");
            fixture
        }

        fn write_executable(&self, name: &str, body: &str) {
            let path = self.bin.join(name);
            fs::write(&path, body).expect("write fake provider executable");
            let mut permissions = fs::metadata(&path)
                .expect("stat fake provider executable")
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions).expect("chmod fake provider executable");
        }

        fn set_codex(&self, text: &str) {
            fs::write(&self.codex_catalog, text).expect("write fake Codex catalog");
        }

        fn set_codex_bytes(&self, bytes: &[u8]) {
            fs::write(&self.codex_catalog, bytes).expect("write fake Codex catalog bytes");
        }

        fn set_claude(&self, text: &str) {
            fs::write(&self.claude_catalog, text).expect("write fake Claude catalog");
        }

        fn set_pi(&self, text: &str) {
            fs::write(&self.pi_catalog, text).expect("write fake Pi catalog");
        }

        fn run(&self, args: &[&str]) -> Output {
            self.run_with(args, &[])
        }

        fn run_with(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
            let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
            command
                .args(args)
                .current_dir(&self.workspace)
                .env_clear()
                .env("PATH", &self.bin)
                .env("HOME", &self.home)
                .env("TMPDIR", &self.temp)
                .env("TRACE_DIR", &self.trace)
                .env("CODEX_CATALOG_FILE", &self.codex_catalog)
                .env("CLAUDE_CATALOG_FILE", &self.claude_catalog)
                .env("PI_CATALOG_FILE", &self.pi_catalog)
                .env("FAKE_STDERR_FILE", &self.stderr_file)
                // The child must remove this inherited marker only for Claude.
                .env("CLAUDECODE", "outer-session-sentinel");
            for (key, value) in extra_env {
                command.env(key, value);
            }
            command.output().expect("run isolated team-agent CLI")
        }

        fn call_count(&self, executable: &str) -> usize {
            fs::read_to_string(self.trace.join(format!("{executable}.calls")))
                .map(|value| value.lines().count())
                .unwrap_or(0)
        }

        fn argv(&self, executable: &str) -> Vec<String> {
            let bytes = fs::read(self.trace.join(format!("{executable}.argv"))).unwrap_or_default();
            let mut parts = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
            if parts.last().is_some_and(|part| part.is_empty()) {
                parts.pop();
            }
            parts
                .into_iter()
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect()
        }

        fn calls_total(&self) -> usize {
            ["codex", "claude", "pi", "agent"]
                .iter()
                .map(|provider| self.call_count(provider))
                .sum()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn codex_row(slug: &str, display_name: &str, visibility: &str) -> Value {
        json!({
            "slug": slug,
            "display_name": display_name,
            "visibility": visibility,
            "supported_in_api": slug != "gpt-5.6-terra",
            "priority": if slug == "gpt-5.5" { 1 } else { 9 },
            "reasoning_levels": ["low", "high"]
        })
    }

    fn codex_rows() -> Vec<Value> {
        let mut rows = VISIBLE_CODEX_ROWS
            .iter()
            .map(|(slug, display_name)| codex_row(slug, display_name, "list"))
            .collect::<Vec<_>>();
        rows.push(codex_row("hidden-canary-model", RAW_SENTINEL, "hide"));
        rows.push(codex_row("none-canary-model", "None-only model", "none"));
        rows
    }

    fn codex_catalog_text() -> String {
        serde_json::to_string(&json!({ "models": codex_rows() })).expect("serialize fixture")
    }

    fn claude_rows() -> Vec<Value> {
        vec![
            json!({
                "value": "default",
                "resolvedModel": "claude-opus-5-5[1m]",
                "displayName": "Default (recommended)"
            }),
            json!({
                "value": "opus[1m]",
                "resolvedModel": "claude-opus-5-5[1m]",
                "displayName": "Opus (1M context)"
            }),
            json!({
                "value": "claude-fable-5[1m]",
                "resolvedModel": "claude-fable-5",
                "displayName": "Fable"
            }),
            json!({
                "value": "sonnet",
                "resolvedModel": "claude-sonnet-5",
                "displayName": "Sonnet"
            }),
            json!({
                "value": "haiku",
                "resolvedModel": "claude-haiku-4-5-20251001",
                "displayName": "Haiku"
            }),
            json!({
                "value": "opus",
                "resolvedModel": "claude-opus-5-5",
                "displayName": "Opus"
            }),
            json!({
                "value": "sonnet-latest",
                "resolvedModel": "claude-sonnet-6",
                "displayName": "Sonnet"
            }),
        ]
    }

    fn claude_catalog_text(rows: &[Value]) -> String {
        let system = json!({
            "type": "system",
            "account": { "email": RAW_SENTINEL, "opaque": "must-not-project" }
        });
        let response = json!({
            "type": "control_response",
            "request_id": REQUEST_ID,
            "response": {
                "subtype": "success",
                "response": {
                    "models": rows,
                    "account": { "email": RAW_SENTINEL },
                    "commands": [{ "name": RAW_SENTINEL }]
                }
            }
        });
        format!("{}\n{}\n", system, response)
    }

    fn codex_ids(value: &Value) -> Vec<String> {
        value["models"]
            .as_array()
            .expect("models array")
            .iter()
            .map(|model| model["model_id"].as_str().expect("model_id").to_string())
            .collect()
    }

    fn successful_json(output: &Output, label: &str) -> Value {
        assert!(
            output.status.success(),
            "{label}: expected successful CLI result, status={:?}, stdout={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{label}: invalid JSON stdout: {error}"));
        assert_eq!(value["schema_version"], "models.v1", "{label}");
        assert_eq!(value["ok"], true, "{label}");
        assert_eq!(value["auth"], "ok", "{label}");
        value
    }

    fn failed_json(output: &Output, label: &str) -> Value {
        assert!(
            !output.status.success(),
            "{label}: expected non-zero CLI result, stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{label}: expected models.v1 JSON error: {error}; stdout={}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        assert_eq!(value["schema_version"], "models.v1", "{label}");
        assert_eq!(value["ok"], false, "{label}");
        assert_eq!(value["auth"], "not_ready", "{label}");
        assert_eq!(
            value["models"],
            json!([]),
            "{label}: partial rows forbidden"
        );
        assert!(
            value["error"].as_str().is_some_and(|text| !text.is_empty()),
            "{label}"
        );
        assert!(
            value["action"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{label}"
        );
        let rendered = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !rendered.contains(RAW_SENTINEL),
            "{label}: raw provider payload leaked"
        );
        value
    }

    fn invoke_search(fixture: &Fixture, provider: &str, query: &str) -> Value {
        let output = fixture.run(&[
            "models",
            "--provider",
            provider,
            "--search",
            query,
            "--json",
        ]);
        successful_json(&output, &format!("{provider} search {query:?}"))
    }

    fn record<'a>(value: &'a Value, id: &str) -> &'a Value {
        value["models"]
            .as_array()
            .expect("models array")
            .iter()
            .find(|model| model["model_id"] == id)
            .unwrap_or_else(|| panic!("missing canonical record {id:?}: {value}"))
    }

    fn assert_single_provider_call(fixture: &Fixture, target: &str, expected: usize) {
        for name in ["codex", "claude", "pi", "agent"] {
            assert_eq!(
                fixture.call_count(name),
                if name == target { expected } else { 0 },
                "only the selected source may spawn: target={target}, source={name}"
            );
        }
    }

    #[test]
    fn f01_codex_discovers_once_and_projects_visible_wire_ids() {
        let fixture = Fixture::new();
        let output = fixture.run(&["models", "--provider", "codex", "--json"]);
        let value = successful_json(&output, "F01 Codex discovery");
        assert_single_provider_call(&fixture, "codex", 1);
        assert_eq!(fixture.argv("codex"), ["debug", "models"]);
        assert_eq!(value["provider"], "codex");
        let expected = VISIBLE_CODEX_ROWS
            .iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        assert_eq!(codex_ids(&value), expected);
        assert_eq!(value["current_role_model"], Value::Null);
        for (slug, display_name) in VISIBLE_CODEX_ROWS {
            let item = record(&value, slug);
            assert_eq!(item["provider"], "codex");
            assert_eq!(item["vendor"], "openai");
            assert_eq!(item["model_id"], *slug);
            assert_eq!(item["role_model"], *slug);
            assert_eq!(item["display_name"], *display_name);
            assert_eq!(item["default"], Value::Null);
            assert_eq!(item["current"], Value::Null);
            assert_eq!(item["aliases"], json!([]));
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains(RAW_SENTINEL));
    }

    #[test]
    fn f03_full_ids_round_trip_without_normalizing_case_prefix_or_suffix() {
        let fixture = Fixture::new();
        let all = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F03 Codex full list",
        );
        for (slug, _) in VISIBLE_CODEX_ROWS {
            let result = invoke_search(&fixture, "codex", slug);
            let item = record(&result, slug);
            assert_eq!(item["model_id"], *slug);
            assert_eq!(item["role_model"], *slug);
        }
        assert_eq!(
            codex_ids(&all),
            VISIBLE_CODEX_ROWS
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn f04_ascii_case_perturbation_preserves_match_set_and_wire_bytes() {
        let fixture = Fixture::new();
        let lower = invoke_search(&fixture, "codex", "gpt luna");
        let mixed = invoke_search(&fixture, "codex", "GpT LuNa");
        assert_eq!(codex_ids(&lower), codex_ids(&mixed));
        assert_eq!(codex_ids(&mixed), ["gpt-5.6-luna", "gpt-5.6-luna@1m"]);
        assert_eq!(record(&mixed, "gpt-5.6-luna")["model_id"], "gpt-5.6-luna");
    }

    #[test]
    fn f05_query_tokens_are_independent_order_invariant_and_and_composed() {
        let fixture = Fixture::new();
        let cases = ["gpt luna", "LUNA gpt", "gpt gpt luna", "gpt\u{2003}luna"];
        let mut observed = Vec::new();
        for query in cases {
            observed.push(codex_ids(&invoke_search(&fixture, "codex", query)));
        }
        assert!(observed.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(observed[0], ["gpt-5.6-luna", "gpt-5.6-luna@1m"]);
        let one_token = codex_ids(&invoke_search(&fixture, "codex", "gpt"));
        let two_tokens = codex_ids(&invoke_search(&fixture, "codex", "gpt terra"));
        assert_eq!(two_tokens, ["gpt-5.6-terra"]);
        assert!(two_tokens.len() <= one_token.len());
        assert!(codex_ids(&invoke_search(&fixture, "codex", "astra nonexistent")).is_empty());
        assert_eq!(
            codex_ids(&invoke_search(&fixture, "codex", "codex luna")),
            observed[0]
        );
    }

    #[test]
    fn f06_distinct_matching_ids_and_same_display_name_remain_all_candidates_in_source_order() {
        let fixture = Fixture::new();
        let value = invoke_search(&fixture, "codex", "shared preview");
        assert_eq!(
            codex_ids(&value),
            ["gpt-5.6-astra-eval", "gpt-5.6-astra-lab"]
        );
        assert_eq!(
            record(&value, "gpt-5.6-astra-eval")["display_name"],
            record(&value, "gpt-5.6-astra-lab")["display_name"]
        );
        let broad = invoke_search(&fixture, "codex", "gpt-5.6");
        assert_eq!(
            codex_ids(&broad),
            VISIBLE_CODEX_ROWS
                .iter()
                .filter(|(id, _)| id.to_lowercase().contains("gpt-5.6"))
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
        );
        assert!(broad["models"].as_array().expect("models").len() > 5);
    }

    #[test]
    fn f07_provider_isolation_is_observable() {
        let codex = Fixture::new();
        let codex_value = successful_json(
            &codex.run(&["models", "--provider", "codex", "--json"]),
            "F07 Codex source isolation",
        );
        assert_eq!(codex_value["provider"], "codex");
        assert_single_provider_call(&codex, "codex", 1);
    }

    #[test]
    fn f08_zero_match_empty_query_and_query_argument_compatibility() {
        let fixture = Fixture::new();
        let no_match = fixture.run(&[
            "models",
            "--provider",
            "codex",
            "--search",
            "definitely-not-a-model",
            "--json",
        ]);
        let empty = successful_json(&no_match, "F08 valid no-match");
        assert!(empty["models"].as_array().expect("models").is_empty());
        assert_eq!(empty["auth"], "ok");
        assert!(!String::from_utf8_lossy(&no_match.stdout).contains(RAW_SENTINEL));
        let human = fixture.run(&["models", "--provider", "codex", "--search", "missing-item"]);
        assert!(human.status.success(), "human no-match remains successful");
        let human = String::from_utf8_lossy(&human.stdout);
        assert!(
            human.contains("No models matched"),
            "friendly empty result: {human}"
        );
        assert!(human.contains("without"), "friendly re-run hint: {human}");

        let all = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--search", "", "--json"]),
            "F08 empty string is no filter",
        );
        let whitespace = successful_json(
            &fixture.run(&[
                "models",
                "--provider",
                "codex",
                "--search",
                " \t\n ",
                "--json",
            ]),
            "F08 whitespace-only is no filter",
        );
        let unfiltered = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F08 omitted search is no filter",
        );
        assert_eq!(codex_ids(&all), codex_ids(&unfiltered));
        assert_eq!(codex_ids(&whitespace), codex_ids(&unfiltered));
        let conflict = fixture.run(&[
            "models",
            "--provider",
            "codex",
            "gpt",
            "--search",
            "luna",
            "--json",
        ]);
        assert!(
            !conflict.status.success(),
            "positional query and --search remain exclusive"
        );
        assert_eq!(
            fixture.call_count("codex"),
            5,
            "the usage error must not spawn a provider"
        );
    }

    #[test]
    fn f09_invalid_or_empty_catalogs_fail_as_a_whole_without_partial_rows() {
        let fixture = Fixture::new();
        let mut failures = Vec::new();
        let mut codex_cases = vec![
            ("empty source", br#"{"models":[]}"#.to_vec()),
            ("missing models", br#"{"other":[]}"#.to_vec()),
            ("wrong models type", br#"{"models":"wrong"}"#.to_vec()),
            ("invalid JSON", format!("{RAW_SENTINEL}").into_bytes()),
            ("wrong root", br#"[]"#.to_vec()),
            (
                "valid prefix then malformed row",
                serde_json::to_vec(&json!({ "models": [codex_row("gpt-5.5", "valid row", "list"), {"display_name":"missing slug", "visibility":"list"}] })).expect("serialize"),
            ),
            (
                "whitespace slug",
                serde_json::to_vec(&json!({ "models": [{"slug":"gpt 5", "display_name":"bad id", "visibility":"list"}] })).expect("serialize"),
            ),
            (
                "control in display name",
                serde_json::to_vec(&json!({ "models": [{"slug":"gpt-5", "display_name":"bad\nname", "visibility":"list"}] })).expect("serialize"),
            ),
        ];
        codex_cases.push(("bad UTF-8", vec![0xff, 0xfe]));
        for (label, bytes) in codex_cases {
            fixture.set_codex_bytes(&bytes);
            let output = fixture.run(&["models", "--provider", "codex", "--json"]);
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) if !output.status.success() => {
                    if value["ok"] != false
                        || value["models"] != json!([])
                        || value["error"]
                            .as_str()
                            .is_none_or(|text| text.contains("unsupported model provider"))
                    {
                        failures.push(format!(
                            "Codex {label}: unsafe/wrong failure projection {value}"
                        ));
                    }
                    let rendered = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    if rendered.contains(RAW_SENTINEL) {
                        failures.push(format!("Codex {label}: raw source leaked"));
                    }
                }
                other => failures.push(format!(
                    "Codex {label}: expected structured fail-closed error, got {other:?}"
                )),
            }
        }
        assert!(
            failures.is_empty(),
            "F09 catalog validation failures: {failures:#?}"
        );
    }

    #[test]
    fn f10_duplicate_source_id_fails_as_a_whole() {
        let fixture = Fixture::new();
        let duplicate_codex = serde_json::to_string(&json!({
            "models": [codex_row("duplicate-id", "first", "list"), codex_row("duplicate-id", "second", "list")]
        })).expect("serialize duplicate Codex source");
        fixture.set_codex(&duplicate_codex);
        let codex_error = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F10 duplicate Codex slug",
        );
        assert!(!codex_error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));
    }

    #[test]
    fn f12_missing_non_executable_and_failed_children_report_the_selected_provider() {
        let fixture = Fixture::new();
        let codex_path = fixture.bin.join("codex");
        fs::remove_file(&codex_path).expect("remove fake Codex for PATH-missing case");
        let missing = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F12 Codex unavailable",
        );
        assert!(missing["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("codex"));
        assert!(!missing["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("repair pi"));

        fixture.write_executable("codex", FAKE_PROVIDER);
        let mut permissions = fs::metadata(&codex_path)
            .expect("stat Codex shim")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&codex_path, permissions).expect("remove executable bit");
        let non_executable = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F12 Codex non-executable PATH entry",
        );
        assert!(non_executable["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("codex"));

        fixture.write_executable("codex", FAKE_PROVIDER);
        let failed = failed_json(
            &fixture.run_with(
                &["models", "--provider", "codex", "--json"],
                &[("FAKE_EXIT_CODE", "23")],
            ),
            "F12 nonzero exit after valid JSON",
        );
        assert!(failed["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("codex"));
        assert_eq!(
            fixture.call_count("codex"),
            1,
            "no retries after source failure"
        );
    }

    #[test]
    fn f13_exact_mebibyte_is_accepted_and_one_extra_byte_is_rejected() {
        let fixture = Fixture::new();
        let exact = padded_codex_catalog(MAX_CATALOG_BYTES);
        assert_eq!(exact.len(), MAX_CATALOG_BYTES);
        fixture.set_codex_bytes(&exact);
        let output = fixture.run(&["models", "--provider", "codex", "--json"]);
        let value = successful_json(&output, "F13 exact 1 MiB boundary");
        assert_eq!(codex_ids(&value), ["boundary-model"]);

        let over = padded_codex_catalog(MAX_CATALOG_BYTES + 1);
        assert_eq!(over.len(), MAX_CATALOG_BYTES + 1);
        fixture.set_codex_bytes(&over);
        let error = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F13 one byte over catalog limit",
        );
        assert!(!error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));
        assert_eq!(fixture.call_count("codex"), 2);
    }

    fn padded_codex_catalog(target_bytes: usize) -> Vec<u8> {
        let base = json!({
            "models": [codex_row("boundary-model", "Boundary Model", "list")],
            "padding": ""
        });
        let base_len = serde_json::to_vec(&base)
            .expect("serialize base catalog")
            .len();
        let padding = target_bytes
            .checked_sub(base_len)
            .expect("limit exceeds base catalog");
        let value = json!({
            "models": [codex_row("boundary-model", "Boundary Model", "list")],
            "padding": "x".repeat(padding)
        });
        let bytes = serde_json::to_vec(&value).expect("serialize padded catalog");
        assert_eq!(bytes.len(), target_bytes);
        bytes
    }

    #[test]
    fn f13_real_deadline_and_late_inherited_stdout_are_bounded_without_retry_or_leak() {
        let timeout = Fixture::new();
        let started = Instant::now();
        let output = timeout.run_with(
            &["models", "--provider", "codex", "--json"],
            &[("FAKE_CODEX_SLEEP", "15")],
        );
        let elapsed = started.elapsed();
        let value = failed_json(&output, "F13 hung provider is bounded");
        assert!(
            elapsed < Duration::from_secs(12),
            "catalog exceeded the 10s deadline: {elapsed:?}"
        );
        assert_eq!(timeout.call_count("codex"), 1, "timeout must not retry");
        assert!(!value["error"].as_str().unwrap().contains(RAW_SENTINEL));
        if let Ok(pid) = fs::read_to_string(timeout.trace.join("codex.pid")) {
            terminate_owned_pid(pid.trim());
        }

        let late = Fixture::new();
        let started = Instant::now();
        let output = late.run_with(
            &["models", "--provider", "codex", "--json"],
            &[("FAKE_LATE_PIPE_SLEEP", "15")],
        );
        let elapsed = started.elapsed();
        let value = failed_json(&output, "F13 parent exit with descendant-held stdout");
        assert!(
            elapsed < Duration::from_secs(12),
            "late stdout reader exceeded deadline: {elapsed:?}"
        );
        assert_eq!(
            late.call_count("codex"),
            1,
            "late-pipe timeout must not retry"
        );
        assert!(!value["error"].as_str().unwrap().contains(RAW_SENTINEL));
        if let Ok(pid) = fs::read_to_string(late.trace.join("descendant.pid")) {
            terminate_owned_pid(pid.trim());
        }
    }

    #[test]
    fn f13_large_stderr_is_discarded_and_never_deadlocks_or_leaks() {
        let fixture = Fixture::new();
        let mut stderr = vec![b'x'; 256 * 1024];
        stderr[..RAW_SENTINEL.len()].copy_from_slice(RAW_SENTINEL.as_bytes());
        fs::write(&fixture.stderr_file, stderr).expect("write stderr flood fixture");
        let output = fixture.run(&["models", "--provider", "codex", "--json"]);
        let value = successful_json(&output, "F13 stderr flood is drained/discarded");
        assert_eq!(
            codex_ids(&value),
            VISIBLE_CODEX_ROWS
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(RAW_SENTINEL));
        assert_eq!(fixture.call_count("codex"), 1);
    }

    #[test]
    fn f14_help_unsupported_aliases_and_models_v1_compatibility() {
        let fixture = Fixture::new();
        let help = fixture.run(&["models", "--help"]);
        assert!(help.status.success(), "models help is available");
        let help = String::from_utf8_lossy(&help.stdout);
        for provider in ["pi", "cursor", "codex", "claude", "claude_code"] {
            assert!(
                help.contains(provider),
                "F14 help must list {provider}: {help}"
            );
        }
        // Every Cursor name reads the contract catalog through `cursor-agent`
        // (absent here), never the retired `agent` entrypoint.
        for alias in ["cursor_agent", "cursor-agent", "agent"] {
            let out = fixture.run(&["models", "--provider", alias, "--json"]);
            let err = failed_json(&out, &format!("F14 Cursor name {alias}"));
            assert_eq!(err["provider"], "cursor", "{alias}");
        }
        let out = fixture.run(&["models", "--provider", "cloud", "--json"]);
        let err = failed_json(&out, "F14 unsupported alias cloud");
        assert!(err["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));
        assert_eq!(
            fixture.calls_total(),
            0,
            "help and unsupported providers never spawn sources"
        );

        let codex = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F14 models.v1 compatibility fields",
        );
        assert_eq!(codex["schema_version"], "models.v1");
        assert!(codex.get("auth_basis").is_some());
        assert!(codex.get("current_role_model").is_some());
        let item = &codex["models"][0];
        for field in [
            "provider",
            "vendor",
            "model_id",
            "role_model",
            "display_name",
            "current",
            "default",
        ] {
            assert!(item.get(field).is_some(), "F14 preserve old field {field}");
        }
    }

    #[test]
    fn f15_codex_priority_is_neither_default_nor_current() {
        let codex = Fixture::new();
        let codex_value = successful_json(
            &codex.run(&["models", "--provider", "codex", "--json"]),
            "F15 Codex priority is not default",
        );
        assert_eq!(record(&codex_value, "gpt-5.5")["default"], Value::Null);
        assert_eq!(codex_value["current_role_model"], Value::Null);
    }

    fn context(model: &str, effort: Option<ProviderEffort>) -> ProviderCommandContext<'_> {
        ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: None,
            model: Some(model),
            dangerously_skip_permissions: false,
            profile_launch: None,
            agent_id_hint: None,
            effort,
        }
    }

    fn contains_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2)
            .any(|window| window[0] == flag && window[1] == value)
    }

    #[test]
    fn f17_launch_builders_keep_exact_unlisted_models() {
        let exact_unlisted = "manual-model-id-not-in-discovery-catalog";
        let plan = get_adapter(Provider::Codex)
            .build_command_plan(context(exact_unlisted, None))
            .expect("catalog discovery must not become launch validation");
        assert!(
            contains_pair(&plan.argv, "--model", exact_unlisted)
                || plan.argv.iter().any(|arg| arg == exact_unlisted),
            "F17 model passed through byte-for-byte for Codex: {:?}",
            plan.argv
        );
    }

    #[test]
    fn f17_codex_max_and_ultra_effort_and_custom_model_remain_verbatim() {
        let model = "codex-custom-unlisted-model";
        for effort in [ProviderEffort::Max, ProviderEffort::Ultra] {
            let plan = get_adapter(Provider::Codex)
                .build_command_plan(context(model, Some(effort)))
                .expect("Codex effort launch plan");
            assert!(contains_pair(&plan.argv, "--model", model));
            let wire_effort = format!("model_reasoning_effort={}", effort.as_str());
            assert!(
                contains_pair(&plan.argv, "-c", &wire_effort),
                "effort must remain native: {:?}",
                plan.argv
            );
        }
    }

    #[test]
    fn f18_codex_visibility_is_catalog_fact_and_hidden_rows_are_still_validated() {
        let fixture = Fixture::new();
        let value = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F18 visible list",
        );
        assert!(codex_ids(&value).contains(&"gpt-5.6-terra".to_string()));
        assert!(!codex_ids(&value).contains(&"hidden-canary-model".to_string()));
        assert!(!codex_ids(&value).contains(&"none-canary-model".to_string()));

        let mut hidden_bad = codex_row("bad hidden", "hidden invalid row", "hide");
        hidden_bad
            .as_object_mut()
            .expect("row object")
            .remove("display_name");
        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("visible", "Visible", "list"), hidden_bad]
            }))
            .expect("serialize invalid hidden row"),
        );
        let error = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F18 malformed hidden row rejects whole catalog",
        );
        assert!(!error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));

        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("unknown-visibility", "Unknown visibility", "preview")]
            }))
            .expect("serialize unknown visibility"),
        );
        let error = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F18 unknown visibility must not be guessed",
        );
        assert!(!error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));

        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("supported-false", "Listed but unavailable in API", "list")]
            }))
            .expect("serialize subscription visible row"),
        );
        let mut rows: Value = serde_json::from_str(
            &fs::read_to_string(&fixture.codex_catalog).expect("read catalog"),
        )
        .expect("parse fixture");
        rows["models"][0]["supported_in_api"] = json!(false);
        fixture.set_codex(&serde_json::to_string(&rows).expect("serialize supported=false"));
        let value = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F18 supported_in_api false does not hide list visibility",
        );
        assert_eq!(codex_ids(&value), ["supported-false"]);

        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("hidden-only", "Hidden only", "hide")]
            }))
            .expect("serialize hidden-only"),
        );
        let error = failed_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F18 no visible list rows is empty catalog, not query no-match",
        );
        assert!(!error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));
    }

    #[test]
    fn f20_provider_capability_and_public_onboarding_contract_are_explicit() {
        let fixture = Fixture::new();
        let help = fixture.run(&["models", "--help"]);
        assert!(help.status.success());
        let help = String::from_utf8_lossy(&help.stdout);
        for provider in ["pi", "cursor", "codex", "claude", "claude_code", "grok"] {
            assert!(
                help.contains(provider),
                "F20 registered provider absent from help: {provider}"
            );
        }
        let unsupported = failed_json(
            &fixture.run(&["models", "--provider", "cloud", "--json"]),
            "F20 unknown provider is explicit",
        );
        assert!(unsupported["action"].as_str().unwrap().contains("pi"));
        assert!(unsupported["action"].as_str().unwrap().contains("codex"));
        assert!(unsupported["action"]
            .as_str()
            .unwrap()
            .contains("claude_code"));
        assert_eq!(
            fixture.calls_total(),
            0,
            "unsupported source must fail without spawn"
        );

        // Capability exhaustiveness and a single shared matcher are review obligations;
        // this assertion covers the public onboarding artifact without source-code grep.
        let doc = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/reference/provider-model-discovery.md");
        assert!(
            doc.is_file(),
            "F20 required reusable Provider discovery standard is absent: {doc:?}"
        );
        assert!(fs::metadata(doc).expect("stat onboarding doc").len() > 0);
    }

    #[test]
    fn f21_each_call_observes_fresh_catalog_and_failed_first_call_cannot_fallback() {
        let fixture = Fixture::new();
        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("gpt-5.6-luna", "Old fixture model", "list")]
            }))
            .expect("serialize first catalog"),
        );
        let first_error = failed_json(
            &fixture.run_with(
                &["models", "--provider", "codex", "--json"],
                &[("FAKE_EXIT_CODE", "23")],
            ),
            "F21 valid output followed by failure must not become success",
        );
        assert!(!first_error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));
        assert_eq!(fixture.call_count("codex"), 1);

        fixture.set_codex(
            &serde_json::to_string(&json!({
                "models": [codex_row("fresh-custom-id", "Fresh catalog row", "list")]
            }))
            .expect("serialize changed catalog"),
        );
        let second = successful_json(
            &fixture.run(&["models", "--provider", "codex", "--json"]),
            "F21 second observation sees current source",
        );
        assert_eq!(codex_ids(&second), ["fresh-custom-id"]);
        assert_eq!(fixture.call_count("codex"), 2);
    }

    fn terminate_owned_pid(pid: &str) {
        if pid.parse::<u32>().is_ok() {
            let _ = Command::new("/bin/kill").args(["-TERM", pid]).status();
        }
    }
}
