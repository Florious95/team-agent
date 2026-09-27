//! Independent F01–F21 contract tests for provider model discovery.
//!
//! These tests intentionally use only the public CLI and public launch contracts. The
//! expected search results are fixed by the fixture rows below; no production matcher is
//! called to calculate an oracle. Run on the frozen baseline to establish product RED.

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
    use team_agent::compiler;
    use team_agent::model::enums::{AuthMode, Provider, ProviderEffort};
    use team_agent::model::yaml::Value as YamlValue;
    use team_agent::provider::{get_adapter, ProviderCommandContext};

    const REQUEST_ID: &str = "team-agent-model-catalog";
    const REQUEST_LINE: &str = "{\"type\":\"control_request\",\"request_id\":\"team-agent-model-catalog\",\"request\":{\"subtype\":\"initialize\",\"hooks\":{},\"agents\":{},\"skills\":[]}}\n";
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
  agent) source_file=$CURSOR_CATALOG_FILE ;;
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
        cursor_catalog: PathBuf,
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
                cursor_catalog: root.join("cursor-catalog.txt"),
                stderr_file: root.join("provider-stderr.txt"),
                root,
                bin,
                home,
                temp,
                workspace,
                trace,
            };
            for executable in ["codex", "claude", "pi", "agent"] {
                fixture.write_executable(executable, FAKE_PROVIDER);
            }
            fixture.set_codex(&codex_catalog_text());
            fixture.set_claude(&claude_catalog_text(&claude_rows()));
            fixture.set_pi("provider model\nopenai-codex gpt-5.6-sol\nopenai-codex gpt-5.6-luna\n");
            fixture.set_cursor(
                "Available models\ngpt-5.6-luna-high - GPT-5.6 Luna 1M High\nauto - Auto (default)\n",
            );
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

        fn set_claude_bytes(&self, bytes: &[u8]) {
            fs::write(&self.claude_catalog, bytes).expect("write fake Claude catalog bytes");
        }

        fn set_pi(&self, text: &str) {
            fs::write(&self.pi_catalog, text).expect("write fake Pi catalog");
        }

        fn set_cursor(&self, text: &str) {
            fs::write(&self.cursor_catalog, text).expect("write fake Cursor catalog");
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
                .env("CURSOR_CATALOG_FILE", &self.cursor_catalog)
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

    fn claude_ids(value: &Value) -> Vec<String> {
        codex_ids(value)
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

    fn write_claude_rows(fixture: &Fixture, rows: &[Value]) {
        fixture.set_claude(&claude_catalog_text(rows));
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
    fn f02_claude_spellings_use_one_exact_initialize_and_canonical_projection() {
        for provider in ["claude", "claude_code"] {
            let fixture = Fixture::new();
            write_claude_rows(&fixture, &claude_rows());
            let output = fixture.run(&["models", "--provider", provider, "--json"]);
            let value = successful_json(&output, &format!("F02 {provider} discovery"));
            assert_single_provider_call(&fixture, "claude", 1);
            assert_eq!(
                fixture.argv("claude"),
                [
                    "--print",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--no-session-persistence",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                    "--setting-sources",
                    "",
                    "--settings",
                    "{\"disableAllHooks\":true}",
                    "--tools",
                    "",
                    "--no-chrome",
                ]
            );
            assert_eq!(fixture.call_count("claude"), 1);
            assert_eq!(
                fs::read(fixture.trace.join("claude.stdin")).expect("captured initialize"),
                REQUEST_LINE.as_bytes(),
                "the initialize request is the only stdin payload"
            );
            assert_eq!(
                fs::read_to_string(fixture.trace.join("claudecode"))
                    .expect("env observation")
                    .trim(),
                "absent",
                "only the nested-session marker is removed for Claude"
            );
            assert_eq!(value["provider"], provider);
            assert_eq!(value["current_role_model"], Value::Null);
            let expected_ids = [
                "claude-opus-5-5[1m]",
                "claude-fable-5",
                "claude-sonnet-5",
                "claude-haiku-4-5-20251001",
                "claude-opus-5-5",
                "claude-sonnet-6",
            ];
            assert_eq!(claude_ids(&value), expected_ids);
            for item in value["models"].as_array().expect("model rows") {
                assert_eq!(item["provider"], provider);
                assert_eq!(item["vendor"], "anthropic");
                assert_eq!(item["model_id"], item["role_model"]);
                assert_eq!(item["current"], Value::Null);
                assert_eq!(item["aliases"].as_array().is_some(), true);
            }
            assert!(!String::from_utf8_lossy(&output.stdout).contains(RAW_SENTINEL));
        }
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
        let claude = invoke_search(&fixture, "claude", "claude-opus-5-5[1m]");
        let item = record(&claude, "claude-opus-5-5[1m]");
        assert_eq!(item["role_model"], "claude-opus-5-5[1m]");
        assert_eq!(item["model_id"], "claude-opus-5-5[1m]");
    }

    #[test]
    fn f04_ascii_case_perturbation_preserves_match_set_and_wire_bytes() {
        let fixture = Fixture::new();
        let lower = invoke_search(&fixture, "codex", "gpt luna");
        let mixed = invoke_search(&fixture, "codex", "GpT LuNa");
        assert_eq!(codex_ids(&lower), codex_ids(&mixed));
        assert_eq!(codex_ids(&mixed), ["gpt-5.6-luna", "gpt-5.6-luna@1m"]);
        assert_eq!(record(&mixed, "gpt-5.6-luna")["model_id"], "gpt-5.6-luna");

        let lower = invoke_search(&fixture, "claude", "sonnet");
        let mixed = invoke_search(&fixture, "claude", "SoNnEt");
        assert_eq!(claude_ids(&lower), claude_ids(&mixed));
        assert_eq!(claude_ids(&mixed), ["claude-sonnet-5", "claude-sonnet-6"]);
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
    fn f07_provider_isolation_and_claude_code_envelope_are_observable() {
        let codex = Fixture::new();
        let codex_value = successful_json(
            &codex.run(&["models", "--provider", "codex", "--json"]),
            "F07 Codex source isolation",
        );
        assert_eq!(codex_value["provider"], "codex");
        assert_single_provider_call(&codex, "codex", 1);

        let claude = Fixture::new();
        let claude_value = successful_json(
            &claude.run(&["models", "--provider", "claude_code", "--json"]),
            "F07 ClaudeCode source isolation",
        );
        assert_eq!(claude_value["provider"], "claude_code");
        assert!(claude_value["models"]
            .as_array()
            .expect("rows")
            .iter()
            .all(|row| row["provider"] == "claude_code"));
        assert_single_provider_call(&claude, "claude", 1);
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

        let malformed_claude = [
            ("empty stdout", Vec::new()),
            ("invalid UTF-8", vec![0xff]),
            ("invalid JSONL", format!("{RAW_SENTINEL}\n").into_bytes()),
            (
                "valid row followed by malformed frame",
                format!("{}\n{{bad json}}\n", claude_catalog_text(&claude_rows())).into_bytes(),
            ),
            (
                "success with empty models",
                claude_catalog_text(&[]).into_bytes(),
            ),
            (
                "missing models field",
                claude_catalog_text(&[
                    json!({"value":"sonnet", "resolvedModel":"claude-sonnet-5"}),
                ])
                .replace("\"models\":[{", "\"notModels\":[{")
                .into_bytes(),
            ),
            (
                "empty required field",
                claude_catalog_text(&[
                    json!({"value":"", "resolvedModel":"claude-sonnet-5", "displayName":"Sonnet"}),
                ])
                .into_bytes(),
            ),
        ];
        for (label, bytes) in malformed_claude {
            fixture.set_claude_bytes(&bytes);
            let output = fixture.run(&["models", "--provider", "claude", "--json"]);
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) if !output.status.success() => {
                    if value["ok"] != false
                        || value["models"] != json!([])
                        || value["error"]
                            .as_str()
                            .is_none_or(|text| text.contains("unsupported model provider"))
                    {
                        failures.push(format!(
                            "Claude {label}: unsafe/wrong failure projection {value}"
                        ));
                    }
                    let rendered = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    if rendered.contains(RAW_SENTINEL) {
                        failures.push(format!("Claude {label}: raw source leaked"));
                    }
                }
                other => failures.push(format!(
                    "Claude {label}: expected structured fail-closed error, got {other:?}"
                )),
            }
        }
        assert!(
            failures.is_empty(),
            "F09 catalog validation failures: {failures:#?}"
        );
    }

    #[test]
    fn f10_duplicate_source_id_fails_but_official_claude_aliases_normalize_only_by_resolved_model()
    {
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

        let duplicate_claude = [
            json!({"value":"sonnet", "resolvedModel":"claude-sonnet-5", "displayName":"Sonnet"}),
            json!({"value":"sonnet", "resolvedModel":"claude-sonnet-5", "displayName":"Sonnet"}),
        ];
        write_claude_rows(&fixture, &duplicate_claude);
        let output = fixture.run(&["models", "--provider", "claude", "--json"]);
        let error = failed_json(&output, "F10 duplicate Claude selector");
        assert!(!error["error"]
            .as_str()
            .unwrap()
            .contains("unsupported model provider"));

        write_claude_rows(&fixture, &claude_rows());
        let normalized = successful_json(
            &fixture.run(&["models", "--provider", "claude", "--json"]),
            "F10 distinct official selectors for one resolvedModel",
        );
        let opus = record(&normalized, "claude-opus-5-5[1m]");
        assert_eq!(opus["aliases"], json!(["default", "opus[1m]"]));
        assert_eq!(opus["display_name"], "Opus (1M context)");
        assert!(record(&normalized, "claude-sonnet-5").is_object());
        assert!(record(&normalized, "claude-sonnet-6").is_object());
        assert_eq!(
            normalized["models"].as_array().expect("models").len(),
            6,
            "equal display names do not collapse distinct canonical IDs"
        );
    }

    fn claude_response(request_id: &str, subtype: &str, body: Value) -> String {
        format!(
            "{}\n",
            json!({
                "type": "control_response",
                "request_id": request_id,
                "response": { "subtype": subtype, "response": body }
            })
        )
    }

    #[test]
    fn f11_claude_protocol_failures_and_metadata_projection_are_fail_closed() {
        let rows = claude_rows();
        let valid_response = json!({ "models": rows, "account": {"token": RAW_SENTINEL} });
        let mut cases = vec![
            (
                "wrong request id",
                format!(
                    "{}\n",
                    claude_response("wrong-request", "success", valid_response.clone())
                ),
            ),
            (
                "error response",
                claude_response(REQUEST_ID, "error", json!({"message": RAW_SENTINEL})),
            ),
            (
                "missing response",
                format!("{}\n", json!({"type":"system", "message":RAW_SENTINEL})),
            ),
            (
                "interactive control request",
                format!(
                    "{}\n",
                    json!({"type":"control_request", "request_id":"unexpected", "request":{"subtype":"permission"}})
                ),
            ),
            (
                "assistant frame",
                format!("{}\n", json!({"type":"assistant", "message":RAW_SENTINEL})),
            ),
            (
                "result frame",
                format!("{}\n", json!({"type":"result", "result":RAW_SENTINEL})),
            ),
            (
                "missing models",
                claude_response(REQUEST_ID, "success", json!({"account":RAW_SENTINEL})),
            ),
            (
                "wrong models type",
                claude_response(REQUEST_ID, "success", json!({"models":"not-array"})),
            ),
            (
                "resolvedModel missing",
                claude_response(
                    REQUEST_ID,
                    "success",
                    json!({"models":[{"value":"sonnet", "displayName":"Sonnet"}]}),
                ),
            ),
            (
                "resolvedModel wrong type",
                claude_response(
                    REQUEST_ID,
                    "success",
                    json!({"models":[{"value":"sonnet", "resolvedModel":4, "displayName":"Sonnet"}]}),
                ),
            ),
            (
                "duplicate success response",
                format!(
                    "{}\n{}\n",
                    claude_response(REQUEST_ID, "success", valid_response.clone()),
                    claude_response(REQUEST_ID, "success", valid_response.clone())
                ),
            ),
        ];
        cases.push((
            "success response followed by bad JSONL",
            format!(
                "{}\n{{bad}}\n",
                claude_response(REQUEST_ID, "success", valid_response)
            ),
        ));
        let mut failures = Vec::new();
        for (label, bytes) in cases {
            fixture_for_claude_case(&mut failures, label, bytes);
        }
        assert!(
            failures.is_empty(),
            "F11 Claude protocol failures: {failures:#?}"
        );
    }

    fn fixture_for_claude_case(failures: &mut Vec<String>, label: &str, bytes: String) {
        let fixture = Fixture::new();
        fixture.set_claude_bytes(bytes.as_bytes());
        let output = fixture.run(&["models", "--provider", "claude", "--json"]);
        match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(value) if !output.status.success() => {
                if value["ok"] != false
                    || value["models"] != json!([])
                    || value["error"]
                        .as_str()
                        .is_none_or(|text| text.contains("unsupported model provider"))
                {
                    failures.push(format!("{label}: wrong failure result {value}"));
                }
                let rendered = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if rendered.contains(RAW_SENTINEL) {
                    failures.push(format!("{label}: provider metadata leaked"));
                }
            }
            other => failures.push(format!("{label}: expected fail-closed JSON, got {other:?}")),
        }
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

        let claude = Fixture::new();
        fs::remove_file(claude.bin.join("claude")).expect("remove Claude for missing PATH case");
        let missing = failed_json(
            &claude.run(&["models", "--provider", "claude_code", "--json"]),
            "F12 ClaudeCode unavailable",
        );
        assert!(missing["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("claude"));
        assert!(!missing["action"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("repair pi"));
        assert_eq!(claude.calls_total(), 0);
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
    fn f14_default_provider_help_unsupported_aliases_and_models_v1_compatibility() {
        let fixture = Fixture::new();
        let default = successful_json(
            &fixture.run(&["models", "--json"]),
            "F14 default provider remains Pi",
        );
        assert_eq!(default["provider"], "pi");
        assert_eq!(fixture.call_count("pi"), 1);
        assert_eq!(fixture.call_count("codex"), 0);
        assert_eq!(fixture.argv("pi"), ["--list-models"]);

        let help = fixture.run(&["models", "--help"]);
        assert!(help.status.success(), "models help is available");
        let help = String::from_utf8_lossy(&help.stdout);
        for provider in ["pi", "cursor_agent", "codex", "claude", "claude_code"] {
            assert!(
                help.contains(provider),
                "F14 help must list {provider}: {help}"
            );
        }
        for alias in ["agent", "cloud"] {
            let out = fixture.run(&["models", "--provider", alias, "--json"]);
            let err = failed_json(&out, &format!("F14 unsupported legacy alias {alias}"));
            assert!(err["error"]
                .as_str()
                .unwrap()
                .contains("unsupported model provider"));
        }
        assert_eq!(
            fixture.calls_total(),
            1,
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
    fn f15_current_default_and_priority_semantics_are_provider_specific() {
        let pi = Fixture::new();
        let pi_value = successful_json(
            &pi.run_with(
                &["models", "--json"],
                &[("PI_PROVIDER", "openai-codex"), ("PI_MODEL", "gpt-5.6-sol")],
            ),
            "F15 Pi exact current environment",
        );
        assert_eq!(pi_value["current_role_model"], "openai-codex/gpt-5.6-sol");
        assert_eq!(
            record(&pi_value, "openai-codex/gpt-5.6-sol")["current"],
            true
        );
        let pi_filtered = successful_json(
            &pi.run_with(
                &["models", "--search", "luna", "--json"],
                &[("PI_PROVIDER", "openai-codex"), ("PI_MODEL", "gpt-5.6-sol")],
            ),
            "F15 Pi current derives from full catalog",
        );
        assert_eq!(
            pi_filtered["current_role_model"],
            "openai-codex/gpt-5.6-sol"
        );
        assert_eq!(pi_filtered["models"][0]["current"], false);

        let cursor = Fixture::new();
        let cursor_value = successful_json(
            &cursor.run(&["models", "--provider", "cursor_agent", "--json"]),
            "F15 Cursor catalog default",
        );
        assert_eq!(record(&cursor_value, "auto")["default"], true);
        assert_eq!(record(&cursor_value, "auto")["current"], Value::Null);
        assert_eq!(cursor_value["current_role_model"], Value::Null);

        let codex = Fixture::new();
        let codex_value = successful_json(
            &codex.run(&["models", "--provider", "codex", "--json"]),
            "F15 Codex priority is not default",
        );
        assert_eq!(record(&codex_value, "gpt-5.5")["default"], Value::Null);
        assert_eq!(codex_value["current_role_model"], Value::Null);

        let claude = Fixture::new();
        let claude_value = successful_json(
            &claude.run(&["models", "--provider", "claude", "--json"]),
            "F15 Claude official default selector",
        );
        assert_eq!(
            record(&claude_value, "claude-opus-5-5[1m]")["default"],
            true
        );
        assert_eq!(record(&claude_value, "claude-opus-5-5")["default"], false);
        assert_eq!(claude_value["current_role_model"], Value::Null);

        let no_default = Fixture::new();
        let rows = claude_rows()
            .into_iter()
            .filter(|row| row["value"] != "default")
            .collect::<Vec<_>>();
        write_claude_rows(&no_default, &rows);
        let value = successful_json(
            &no_default.run(&["models", "--provider", "claude", "--json"]),
            "F15 absent default remains unknown",
        );
        assert!(value["models"]
            .as_array()
            .expect("rows")
            .iter()
            .all(|row| row["default"] == Value::Null));
    }

    #[test]
    fn f16_models_are_read_only_and_claude_runs_isolated_without_session_or_tools() {
        let fixture = Fixture::new();
        let output = fixture.run_with(
            &["models", "--provider", "claude", "--json"],
            &[("CLAUDECODE", "outer-session-sentinel")],
        );
        successful_json(&output, "F16 isolated Claude lookup");
        assert_single_provider_call(&fixture, "claude", 1);
        let cwd = fs::read_to_string(fixture.trace.join("claude.pwd")).expect("Claude cwd capture");
        let cwd = PathBuf::from(cwd.trim());
        assert!(
            cwd.starts_with(&fixture.temp),
            "Claude cwd must be owned temp: {cwd:?}"
        );
        assert_ne!(cwd, fixture.workspace);
        assert!(
            !cwd.exists(),
            "owned Claude temp directory must be removed after completion"
        );
        assert_eq!(
            fs::read_to_string(fixture.trace.join("claudecode"))
                .expect("Claude env capture")
                .trim(),
            "absent"
        );
        assert!(!fixture.workspace.join(".team").exists());
        assert!(!fixture.home.join(".team-agent").exists());
        assert!(!fixture.home.join(".claude").exists());
        assert!(!fixture.root.join("coordinator.sqlite").exists());
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
    fn f17_launch_builders_keep_exact_unlisted_models_and_pi_preflight_boundary() {
        for provider in [
            Provider::Codex,
            Provider::Claude,
            Provider::ClaudeCode,
            Provider::CursorAgent,
        ] {
            let exact_unlisted = "manual-model-id-not-in-discovery-catalog";
            let plan = get_adapter(provider)
                .build_command_plan(context(exact_unlisted, None))
                .expect("catalog discovery must not become launch validation");
            assert!(
                contains_pair(&plan.argv, "--model", exact_unlisted)
                    || plan.argv.iter().any(|arg| arg == exact_unlisted),
                "F17 model passed through byte-for-byte for {provider:?}: {:?}",
                plan.argv
            );
        }

        let role = |provider: &str, model: &str| {
            YamlValue::Map(vec![
                ("provider".to_string(), YamlValue::Str(provider.to_string())),
                ("model".to_string(), YamlValue::Str(model.to_string())),
            ])
        };
        let calls = std::cell::Cell::new(0);
        assert!(compiler::preflight_pi_role_model_with(
            &role("pi", "openai-codex/exact-model"),
            |_| {
                calls.set(calls.get() + 1);
                Ok(Vec::new())
            }
        )
        .is_ok());
        assert_eq!(
            calls.get(),
            0,
            "qualified Pi model keeps the existing no-discovery path"
        );
        for request in ["gpt-5.6-luna", "openai-codex/gpt-*"] {
            let error = compiler::preflight_pi_role_model_with(&role("pi", request), |_| {
                Ok(vec!["openai-codex/gpt-5.6-luna".to_string()])
            })
            .expect_err(
                "unqualified/wildcard Pi request remains a suggestion, not a launch selection",
            );
            assert_eq!(error.candidates, ["openai-codex/gpt-5.6-luna"]);
        }
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
        let claude_model = "claude-custom-unlisted-model";
        let claude = get_adapter(Provider::Claude)
            .build_command_plan(context(claude_model, Some(ProviderEffort::Max)))
            .expect("Claude max effort launch plan");
        assert!(contains_pair(&claude.argv, "--model", claude_model));
        assert!(contains_pair(&claude.argv, "--effort", "max"));
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
    fn f19_claude_alias_default_and_display_names_share_and_search_without_guessing() {
        let fixture = Fixture::new();
        let sonnet = invoke_search(&fixture, "claude", "SoNNet");
        assert_eq!(claude_ids(&sonnet), ["claude-sonnet-5", "claude-sonnet-6"]);
        let opus = invoke_search(&fixture, "claude", "default opus");
        assert_eq!(claude_ids(&opus), ["claude-opus-5-5[1m]"]);
        let context = invoke_search(&fixture, "claude", "1M context");
        assert_eq!(claude_ids(&context), ["claude-opus-5-5[1m]"]);
        let haiku = invoke_search(&fixture, "claude_code", "HAIKU");
        assert_eq!(claude_ids(&haiku), ["claude-haiku-4-5-20251001"]);
        assert_eq!(
            record(&haiku, "claude-haiku-4-5-20251001")["aliases"],
            json!(["haiku"])
        );

        let no_match = fixture.run(&[
            "models",
            "--provider",
            "claude",
            "--search",
            "gpt-6-luna",
            "--json",
        ]);
        let value = successful_json(
            &no_match,
            "missing model is determined by current Claude catalog",
        );
        assert!(value["models"].as_array().expect("rows").is_empty());
    }

    #[test]
    fn f20_provider_capability_and_public_onboarding_contract_are_explicit() {
        let fixture = Fixture::new();
        let help = fixture.run(&["models", "--help"]);
        assert!(help.status.success());
        let help = String::from_utf8_lossy(&help.stdout);
        for provider in ["pi", "cursor_agent", "codex", "claude", "claude_code"] {
            assert!(
                help.contains(provider),
                "F20 registered provider absent from help: {provider}"
            );
        }
        let unsupported = failed_json(
            &fixture.run(&["models", "--provider", "grok", "--json"]),
            "F20 unsupported provider is explicit",
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

#[cfg(not(unix))]
#[test]
fn model_catalog_fuzzy_lookup_red_requires_unix_fake_provider_fixtures() {
    // The provider-path shims in the contract suite are Unix executables.
}

#[cfg(unix)]
#[test]
fn model_catalog_fuzzy_lookup_red_unix_suite_is_registered() {
    // The behavior tests live in unix_tests; this keeps a visible crate-level test count.
}
