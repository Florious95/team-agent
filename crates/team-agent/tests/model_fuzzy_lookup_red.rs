//! Independent F-series contract tests for provider model discovery.
//!
//! These tests intentionally use only the public CLI. The expected results are fixed by
//! the fixture rows below; no production matcher is called to calculate an oracle.
//!
//! `models --provider codex|codex_cli|codex-cli|grok` now read the contract-leaf catalogs;
//! the legacy Codex catalog contracts (fuzzy search, visibility/priority projection, bounded
//! legacy reads and the Codex launch builders) left with that engine. `claude`/`claude_code`
//! and `pi` (the default) keep their legacy catalogs; the Claude catalog contracts below stay.

#[cfg(unix)]
mod unix_tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    const REQUEST_LINE: &str = "{\"type\":\"control_request\",\"request_id\":\"team-agent-model-catalog\",\"request\":{\"subtype\":\"initialize\",\"hooks\":{},\"agents\":{},\"skills\":[]}}\n";
    const REQUEST_ID: &str = "team-agent-model-catalog";
    const RAW_SENTINEL: &str = "RAW_CATALOG_SECRET_SENTINEL";
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
if [ -n "${FAKE_STDERR_FILE:-}" ] && [ -f "$FAKE_STDERR_FILE" ]; then
  /bin/cat "$FAKE_STDERR_FILE" >&2
fi
case "$name" in
  claude) source_file=$CLAUDE_CATALOG_FILE ;;
  pi) source_file=$PI_CATALOG_FILE ;;
  *) exit 64 ;;
esac
if [ -f "$source_file" ]; then /bin/cat "$source_file"; fi
exit "${FAKE_EXIT_CODE:-0}"
"#;

    // The codex_tui leaf runs the PATH-selected `codex` twice: the `--version`
    // probe, then the native `debug models` catalog in the workspace.
    const LEAF_CODEX: &str = r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$TRACE_DIR/codex.calls"
case "$*" in
  --version) printf '%s\n' 'codex-cli 0.160.1' ;;
  'debug models') /bin/cat "$CODEX_CATALOG_FILE" ;;
  *) exit 64 ;;
esac
"#;

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
            for executable in ["claude", "pi"] {
                fixture.write_executable(executable, FAKE_PROVIDER);
            }
            fixture.write_executable("codex", LEAF_CODEX);
            fixture.write_executable("agent", r#"#!/bin/sh
printf '%s\n' retired-entrypoint >> "$TRACE_DIR/agent.calls"
exit 77
"#);
            fixture.set_codex(&codex_leaf_catalog_text());
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

        fn set_claude(&self, text: &str) {
            fs::write(&self.claude_catalog, text).expect("write fake Claude catalog");
        }

        fn set_claude_bytes(&self, bytes: &[u8]) {
            fs::write(&self.claude_catalog, bytes).expect("write fake Claude catalog bytes");
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

    /// Native `codex debug models` rows: only `visibility: list` rows are launch IDs.
    fn codex_leaf_catalog_text() -> String {
        serde_json::to_string(&json!({ "models": [
            {
                "slug": "gpt-6-luna",
                "display_name": "GPT-6 Luna",
                "visibility": "list",
                "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]
            },
            {
                "slug": "hidden-canary-model",
                "display_name": RAW_SENTINEL,
                "visibility": "hide",
                "supported_reasoning_levels": []
            }
        ]}))
        .expect("serialize fixture")
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
    fn f07_codex_names_read_only_the_codex_tui_leaf_catalog() {
        for name in ["codex", "codex_cli", "codex-cli"] {
            let fixture = Fixture::new();
            let output = fixture.run(&["models", "--provider", name, "--json"]);
            assert!(
                output.status.success(),
                "{name}: expected the codex_tui leaf catalog, stdout={}, stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|error| panic!("{name}: invalid JSON stdout: {error}"));
            assert_eq!(value["schema_version"], "models.v1", "{name}");
            assert_eq!(value["ok"], true, "{name}");
            assert_eq!(value["provider"], "codex_tui", "{name}");
            assert_eq!(value["source"]["version"], "0.160.1", "{name}");
            assert_eq!(codex_ids(&value), ["gpt-6-luna"], "{name}: only list rows are launch IDs");
            assert!(
                value["models"]
                    .as_array()
                    .expect("models array")
                    .iter()
                    .all(|model| model["provider"] == "codex_tui"),
                "{name}: {value}"
            );
            assert_eq!(
                fs::read_to_string(fixture.trace.join("codex.calls")).unwrap_or_default(),
                "--version\ndebug models\n",
                "{name}: the PATH `codex` answers the version probe, then the native catalog"
            );
            assert_single_provider_call(&fixture, "codex", 2);
            assert!(
                !String::from_utf8_lossy(&output.stdout).contains(RAW_SENTINEL),
                "{name}: hidden rows are never projected"
            );
        }
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

    // Legacy Claude Code catalog contracts (restored with the legacy Claude worker runtime).
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

    fn write_claude_rows(fixture: &Fixture, rows: &[Value]) {
        fixture.set_claude(&claude_catalog_text(rows));
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
}
