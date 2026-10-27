//! Read-only provider model catalog discovery for `team-agent models`.
use super::{CliError, CmdOutput, CmdResult, ExitCode, ModelsArgs};
use serde_json::{json, Value};
#[cfg(test)]
use std::path::Path;
type ModelRecord = crate::provider::model_catalog::ModelRecord;

pub fn cmd_models(args: &ModelsArgs) -> Result<CmdResult, CliError> {
    #[cfg(unix)]
    if let Some(descriptor) = crate::contract_runtime::registry::declared(&args.provider) {
        // A role's provider names its catalog: `agent` or `cursor-agent` is the
        // Cursor leaf; `claude`/`claude_code`, `codex`, `grok` and `pi` are the
        // Claude Code, Codex, Grok Build and Pi leaves.
        let args = ModelsArgs { provider: descriptor.identity.id.into(), ..args.clone() };
        return crate::contract_runtime::discovery::models(&args);
    }
    let records = match crate::provider::model_catalog::discover_model_catalog(&args.provider) {
        Ok(records) => records,
        Err(error) => {
            let message = error.message(&args.provider);
            let action = error.action(&args.provider);
            return Ok(failure_with_action(args, &message, &action));
        }
    };
    render_catalog(args, records)
}

fn render_catalog(args: &ModelsArgs, all: Vec<ModelRecord>) -> Result<CmdResult, CliError> {
    let visible = args.search.as_deref().map_or_else(
        || all.clone(),
        |search| {
            all.iter()
                .filter(|model| crate::provider::model_catalog::model_matches(model, search))
                .cloned()
                .collect()
        },
    );
    let entries: Vec<Value> = visible.iter().map(|model| json!({
        "role_model": model.id,
        "current": Value::Null,
        "provider": model.provider,
        "vendor": model.vendor,
        "model_id": model.id,
        "display_name": model.display_name,
        "default": model.default,
        "aliases": model.aliases,
    })).collect();
    let provider = args.provider.as_str();
    let value = json!({
        "schema_version": "models.v1", "ok": true, "provider": provider, "models": entries,
        "auth": "ok", "auth_basis": "catalog_visibility", "current_role_model": Value::Null, "search": args.search,
    });
    if args.json {
        Ok(CmdResult {
            output: CmdOutput::Human(serde_json::to_string_pretty(&value)?),
            exit: ExitCode::Ok,
            as_json: false,
            preserve_json_order: true,
        })
    } else {
        let mut lines = vec![format!(
            "models.v1 | {provider} models (copyable model_id):"
        )];
        lines.push("PROVIDER VENDOR MODEL_ID DISPLAY_NAME CURRENT DEFAULT".to_string());
        lines.extend(visible.iter().map(|model| {
            format!(
                "{} {} {} {} {} {}",
                model.provider,
                model.vendor,
                model.id,
                model.display_name,
                "—",
                model
                    .default
                    .map_or_else(|| "—".to_string(), |value| value.to_string()),
            )
        }));
        lines.push(format!(
            "auth: ok (catalog_visibility); {} model(s)",
            visible.len()
        ));
        if visible.is_empty() {
            lines.push("No models matched --search/query; rerun without a search to list the full catalog.".to_string());
        }
        Ok(CmdResult::human(lines.join("\n")))
    }
}

fn failure_with_action(args: &ModelsArgs, message: &str, action: &str) -> CmdResult {
    let value = json!({ "schema_version": "models.v1", "ok": false, "provider": args.provider, "auth": "not_ready", "auth_basis": "catalog_visibility", "models": Value::Array(Vec::new()), "current_role_model": Value::Null, "error": message, "action": action });
    if args.json {
        let text = serde_json::to_string_pretty(&value)
            .unwrap_or_else(|_| "{\"schema_version\":\"models.v1\",\"ok\":false}".to_string());
        CmdResult {
            output: CmdOutput::Human(text),
            exit: ExitCode::Error,
            as_json: false,
            preserve_json_order: true,
        }
    } else {
        CmdResult { output: CmdOutput::Human(format!("models.v1 | error: {message}\naction: {action}\nauth: not_ready (catalog_visibility)")), exit: ExitCode::Error, as_json: false, preserve_json_order: false }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    #[test]
    fn real_dispatcher_child_helper() {
        if std::env::var_os("TEAM_AGENT_MODELS_CHILD").is_none() {
            return;
        }
        let args: Vec<String> =
            serde_json::from_str(&std::env::var("TEAM_AGENT_MODELS_ARGS").unwrap()).unwrap();
        print!("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        let exit = crate::cli::emit::run(&args, Path::new("/tmp"));
        std::process::exit(exit.code());
    }

    #[cfg(unix)]
    #[test]
    fn cursor_contract_catalog_uses_public_launcher_metadata_then_exact_ids() {
        let program = fixture_program("cursor-agent", r#"
test "$#" = 1 || exit 9
case "$1" in
  --version) printf '%s\n' '2026.10.01-a1b2c3' ;;
  --list-models) exec /bin/cat "$TEAM_AGENT_MODELS_FIXTURE" ;;
  *) exit 9 ;;
esac"#);
        let mut cursor = CliFixture::new(program);
        let response = cursor.program.with_extension("stdout");
        std::fs::write(&response, include_bytes!("../../../team-agent-contract/tests/fixtures/cursor-2026.10.01-models.txt")).unwrap();
        cursor.response = Some(response);
        let output = run_public_models_cli(&cursor.program, cursor.response.as_deref(),
            &["models", "--provider", "cursor", "--search", "gpt-5.6-luna-xhigh", "--json"]);
        let text = public_cli_text(&output);
        let calls = std::fs::read_to_string(cursor.program.with_extension("count")).unwrap();
        assert!(output.status.success(), "{text}");
        assert_eq!(calls, "1:--version\n1:--list-models\n");
        let value: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["provider"], "cursor");
        assert_eq!(value["auth"], "unknown");
        assert_eq!(value["source"]["schema"], "cursor-list-models-text-v1");
        assert_eq!(value["source"]["identity_scope"], "catalog_only");
        assert!(value["models"].as_array().unwrap().iter().any(|row| row["model_id"] == "gpt-5.6-luna-xhigh"));
        assert!(value["models"].as_array().unwrap().iter().all(|row| row["role_model"] == row["model_id"]));
        assert!(value["current_role_model"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn grok_build_contract_catalog_runs_the_public_grok_found_on_path() {
        // The public `grok` provider name declares the Grok Build leaf; both
        // names read the same contract catalog.
        for name in ["grok_build", "grok"] {
            let program = fixture_program("grok", r#"
test "$#" = 1 || exit 9
case "$1" in
  --version) printf '%s\n' 'grok 1.0.52 (0920c8169e27)' ;;
  models) exec /bin/cat "$TEAM_AGENT_MODELS_FIXTURE" ;;
  *) exit 9 ;;
esac"#);
            let mut grok = CliFixture::new(program);
            let response = grok.program.with_extension("stdout");
            std::fs::write(&response, include_bytes!("../provider/testdata/grok-models-authenticated.stdout")).unwrap();
            grok.response = Some(response);
            let output = run_public_models_cli(&grok.program, grok.response.as_deref(),
                &["models", "--provider", name, "--json"]);
            let text = public_cli_text(&output);
            let calls = std::fs::read_to_string(grok.program.with_extension("count")).unwrap_or_default();
            assert!(output.status.success(), "{name}: {text}");
            // Whatever `grok` PATH selects answers every probe; no install path is assumed.
            assert_eq!(calls, "1:--version\n1:models\n", "{name}");
            let value: Value = serde_json::from_str(text.trim()).unwrap();
            assert_eq!(value["provider"], "grok_build", "{name}");
            assert_eq!(value["source"]["executable"], grok.program.to_str().unwrap(), "{name}");
            assert!(value["models"].as_array().unwrap().iter().any(|row| row["model_id"] == "grok-4.7-build-fast"), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn every_cursor_provider_name_reads_the_contract_catalog_through_cursor_agent() {
        use std::os::unix::fs::PermissionsExt;
        for name in ["cursor_agent", "cursor-agent", "agent"] {
            let mut cursor = CliFixture::new(fixture_program("cursor-agent", r#"
test "$#" = 1 || exit 9
case "$1" in
  --version) printf '%s\n' '2026.10.01-a1b2c3' ;;
  --list-models) exec /bin/cat "$TEAM_AGENT_MODELS_FIXTURE" ;;
  *) exit 9 ;;
esac"#));
            let response = cursor.program.with_extension("stdout");
            std::fs::write(&response, include_bytes!("../../../team-agent-contract/tests/fixtures/cursor-2026.10.01-models.txt")).unwrap();
            cursor.response = Some(response);
            // The legacy binary name on PATH is never what runs.
            let legacy = cursor.program.parent().unwrap().join("agent");
            std::fs::write(&legacy, "#!/bin/sh\nprintf 'called' > \"$0.called\"\nexit 9\n").unwrap();
            std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = run_public_models_cli(&cursor.program, cursor.response.as_deref(),
                &["models", "--provider", name, "--json"]);
            let text = public_cli_text(&output);
            assert!(output.status.success(), "{name}: {text}");
            assert_eq!(std::fs::read_to_string(cursor.program.with_extension("count")).unwrap(),
                "1:--version\n1:--list-models\n", "{name}");
            assert!(!legacy.with_extension("called").exists(), "{name}");
            let value: Value = serde_json::from_str(text.trim()).unwrap();
            assert_eq!(value["provider"], "cursor", "{name}");
            assert!(value["models"].as_array().unwrap().iter().any(|row| row["model_id"] == "gpt-5.6-luna-xhigh"), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cursor_contract_bad_version_has_no_legacy_entrypoint_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let cursor = CliFixture::new(fixture_program("cursor-agent", "printf '%s\\n' 'Cursor release unknown'"));
        let legacy = cursor.program.parent().unwrap().join("agent");
        std::fs::write(&legacy, "#!/bin/sh\nprintf 'called' > \"$0.called\"\nexit 9\n").unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = run_public_models_cli(&cursor.program, None,
            &["models", "--provider", "cursor", "--json"]);
        let text = public_cli_text(&output);
        assert!(!output.status.success(), "{text}");
        assert_eq!(std::fs::read_to_string(cursor.program.with_extension("count")).unwrap(), "1:--version\n");
        assert!(!legacy.with_extension("called").exists());
        let value: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["provider"], "cursor");
        assert_eq!(value["ok"], false);
        assert_eq!(value["models"], json!([]));
        assert!(value["error"].as_str().unwrap().contains("Cursor release unknown"));
    }

    #[cfg(unix)]
    #[test]
    fn grok_failure_help_and_unsupported_action_are_publicly_diagnostic() {
        let missing = CliFixture::new(fixture_program("not-grok", "exit 0"));
        let output = run_public_models_cli(
            &missing.program,
            None,
            &["models", "--provider", "grok", "--json"],
        );
        let text = public_cli_text(&output);
        let value: Value = serde_json::from_str(text.trim()).unwrap();
        // `grok` reads the Grok Build leaf catalog; a missing PATH `grok` fails
        // closed there with no legacy catalog fallback.
        assert_eq!(value["ok"], false);
        assert_eq!(value["provider"], "grok_build", "{text}");
        assert_eq!(value["models"], json!([]), "{text}");
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("grok"),
            "{text}"
        );
        assert!(crate::cli::emit::default_help().contains("grok"));
        assert!(crate::cli::spec::command_spec("models")
            .unwrap()
            .usage
            .contains("grok"));

        let unsupported = run_public_models_cli(
            &missing.program,
            None,
            &["models", "--provider", "cloud", "--json"],
        );
        let unsupported_text = public_cli_text(&unsupported);
        let unsupported_value: Value = serde_json::from_str(unsupported_text.trim()).unwrap();
        drop(missing);
        assert!(!output.status.success());
        assert!(!unsupported.status.success());
        assert!(
            unsupported_value["action"]
                .as_str()
                .unwrap()
                .contains("grok"),
            "{unsupported_text}"
        );
    }

    #[cfg(unix)]
    fn fixture_program(program: &str, body: &str) -> std::path::PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "team-agent-{program}-fixture-{}-{}-{sequence}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(program);
        install_executable(
            &path,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$#:$1\" >> \"$0.count\"\n{body}\n"),
        );
        path
    }

    /// Linux refuses to exec an inode still open for writing anywhere (ETXTBSY),
    /// and a parallel test's fork can briefly inherit our write fd. Only a
    /// short-lived `cp` ever writes the executable, so no test thread holds it.
    #[cfg(unix)]
    fn install_executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let staged = path.with_extension("staged");
        std::fs::write(&staged, script).unwrap();
        let copied = std::process::Command::new("/bin/cp")
            .arg(&staged)
            .arg(path)
            .status()
            .unwrap();
        assert!(
            copied.success(),
            "install fixture executable {}",
            path.display()
        );
        std::fs::remove_file(&staged).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    struct CliFixture {
        program: std::path::PathBuf,
        response: Option<std::path::PathBuf>,
    }

    #[cfg(unix)]
    impl CliFixture {
        fn new(program: std::path::PathBuf) -> Self {
            Self {
                program,
                response: None,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for CliFixture {
        fn drop(&mut self) {
            cleanup_fixture(&self.program);
        }
    }

    #[cfg(unix)]
    fn run_public_models_cli(
        path: &Path,
        response: Option<&Path>,
        args: &[&str],
    ) -> std::process::Output {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cli::models::tests::real_dispatcher_child_helper",
                "--nocapture",
            ])
            .env("TEAM_AGENT_MODELS_CHILD", "1")
            .env(
                "TEAM_AGENT_MODELS_ARGS",
                serde_json::to_string(args).unwrap(),
            )
            .env("PATH", path.parent().unwrap())
            .env_remove("PI_PROVIDER")
            .env_remove("PI_MODEL")
            .env_remove("TEAM_AGENT_MODELS_FIXTURE");
        if let Some(response) = response {
            command.env("TEAM_AGENT_MODELS_FIXTURE", response);
        }
        command.output().unwrap()
    }

    #[cfg(unix)]
    fn public_cli_text(output: &std::process::Output) -> String {
        String::from_utf8(output.stdout.clone())
            .unwrap()
            .split("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n")
            .nth(1)
            .expect("public CLI child output marker")
            .to_string()
    }

    #[cfg(unix)]
    fn cleanup_fixture(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn public_failure_and_help_contracts_are_versioned() {
        let args = ModelsArgs {
            provider: "codex".into(),
            search: None,
            json: true,
        };
        let result = failure_with_action(&args, "safe failure", "safe action");
        let CmdOutput::Human(text) = result.output else {
            panic!("json contract must render")
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], "models.v1");
        assert_eq!(value["models"], Value::Array(vec![]));
        assert_eq!(result.exit, ExitCode::Error);
        assert!(crate::cli::emit::default_help().contains("models"));
        assert_eq!(
            crate::cli::run(&["models".into(), "--help".into()], Path::new("/tmp")),
            ExitCode::Ok
        );
        assert_eq!(
            crate::cli::run(
                &["models".into(), "--provider".into(), "cloud".into()],
                Path::new("/tmp")
            ),
            ExitCode::Error
        );
        let usage = "team-agent models [--provider pi|cursor|codex|claude|claude_code|grok|kiro] [QUERY|--search TEXT] [--json]";
        assert_eq!(crate::cli::spec::command_spec("models").unwrap().usage, usage);
        assert!(crate::cli::emit::command_help(Some("models")).contains(usage));
    }

    #[test]
    fn parser_contract_fixtures() {
        let parse =
            |input: &[u8]| crate::lifecycle::launch::pi_mcp::parse_pi_list_models_table(input);
        assert_eq!(
            parse(b"provider model\r\nopenai-codex gpt-5.6-sol\r\n").unwrap(),
            vec!["openai-codex/gpt-5.6-sol"]
        );
        assert!(parse(b"provider model\n").is_err());
        assert!(parse(b"provider model\na b\na b\n").is_err());
        assert!(parse(b"wrong model\na b\n").is_err());
        assert!(parse(b"provider model\na\n").is_err());
        assert!(parse(&[b'p', b'r', 0xff]).is_err());
    }
}
