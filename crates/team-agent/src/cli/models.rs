//! Read-only Pi model catalog discovery for `team-agent models`.
use super::{CliError, CmdOutput, CmdResult, ExitCode, ModelsArgs};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

pub fn cmd_models(args: &ModelsArgs) -> Result<CmdResult, CliError> {
    cmd_models_with(args, Path::new("pi"), CATALOG_TIMEOUT, MAX_CATALOG_BYTES)
}

fn cmd_models_with(
    args: &ModelsArgs,
    program: &Path,
    timeout: Duration,
    max_bytes: u64,
) -> Result<CmdResult, CliError> {
    let bytes = match run_catalog(program, timeout, max_bytes) {
        Ok(bytes) => bytes,
        Err(message) => return Ok(failure(args, &message)),
    };
    let all = match crate::lifecycle::launch::pi_mcp::parse_pi_list_models_table(&bytes) {
        Ok(models) => models,
        Err(error) => return Ok(failure(args, &safe_catalog_error(error))),
    };
    let visible = args.search.as_deref().map_or_else(
        || all.clone(),
        |search| {
            all.iter()
                .filter(|model| model.contains(search))
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let current = current_role_model(&all);
    let entries: Vec<Value> = visible
        .iter()
        .map(|model| {
            json!({
                "role_model": model,
                "current": current.as_deref() == Some(model.as_str()),
            })
        })
        .collect();
    let value = json!({
        "schema_version": "models.v1", "ok": true, "provider": "pi", "models": entries,
        "auth": "ok", "auth_basis": "catalog_visibility", "current_role_model": current, "search": args.search,
    });
    if args.json {
        let text = serde_json::to_string_pretty(&value)?;
        Ok(CmdResult {
            output: CmdOutput::Human(text),
            exit: ExitCode::Ok,
            as_json: false,
            preserve_json_order: true,
        })
    } else {
        let mut lines = vec!["models.v1 | Pi models (copyable role_model):".to_string()];
        lines.extend(visible.iter().map(|model| {
            format!(
                "role_model: {model} current={}",
                current.as_deref() == Some(model.as_str())
            )
        }));
        lines.push(format!(
            "auth: ok (catalog_visibility); {} model(s)",
            visible.len()
        ));
        if visible.is_empty() {
            lines.push("No models matched --search.".to_string());
        }
        Ok(CmdResult::human(lines.join("\n")))
    }
}

fn failure(args: &ModelsArgs, message: &str) -> CmdResult {
    let value = json!({ "schema_version": "models.v1", "ok": false, "provider": "pi", "auth": "not_ready", "auth_basis": "catalog_visibility", "models": Value::Array(Vec::new()), "current_role_model": Value::Null, "error": message, "action": "install or repair the PATH-first `pi` executable, then retry `team-agent models --provider pi`" });
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
        CmdResult { output: CmdOutput::Human(format!("models.v1 | error: {message}\naction: install or repair the PATH-first `pi` executable, then retry `team-agent models --provider pi`\nauth: not_ready (catalog_visibility)")), exit: ExitCode::Error, as_json: false, preserve_json_order: false }
    }
}

fn safe_catalog_error(error: crate::provider::ProviderError) -> String {
    let text = error.to_string();
    if text.contains("not UTF-8") {
        "Pi model catalog is not valid UTF-8".into()
    } else if text.contains("duplicate") {
        "Pi model catalog contains duplicate model ids".into()
    } else if text.contains("header") {
        "Pi model catalog has an unexpected header".into()
    } else if text.contains("row") {
        "Pi model catalog contains a malformed row".into()
    } else if text.contains("empty") || text.contains("no models") {
        "Pi model catalog is empty".into()
    } else {
        "Pi model catalog is invalid".into()
    }
}
fn current_role_model(models: &[String]) -> Option<String> {
    current_role_model_from(
        models,
        std::env::var("PI_PROVIDER").ok().as_deref(),
        std::env::var("PI_MODEL").ok().as_deref(),
    )
}
fn current_role_model_from(
    models: &[String],
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let exact = format!("{}/{}", provider?, model?);
    models
        .iter()
        .any(|candidate| candidate == &exact)
        .then_some(exact)
}

/// Injectable executable, deadline, and byte-limit boundary for deterministic tests.
fn run_catalog(program: &Path, timeout: Duration, max_bytes: u64) -> Result<Vec<u8>, String> {
    crate::lifecycle::launch::pi_mcp::run_pi_catalog(program, timeout, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

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
    fn real_dispatcher_isolated_success_json_human_and_no_match() {
        let path = fixture("printf 'provider model\\nopenai-codex gpt-5.6-sol\\n'");
        let dir = path.parent().unwrap().to_path_buf();
        let pi = dir.join("pi");
        std::fs::rename(&path, &pi).unwrap();
        let run_child = |args: &[&str], model: &str| {
            std::process::Command::new(std::env::current_exe().unwrap())
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
                .env("PATH", &dir)
                .env("PI_PROVIDER", "openai-codex")
                .env("PI_MODEL", model)
                .output()
                .unwrap()
        };
        let json = run_child(&["models", "--provider", "pi", "--json"], "gpt-5.6-sol");
        assert!(json.status.success());
        let output = String::from_utf8(json.stdout).unwrap();
        let cli_output = output
            .split("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n")
            .nth(1)
            .unwrap();
        let value: Value = serde_json::from_str(cli_output.trim()).unwrap();
        assert_eq!(value["schema_version"], "models.v1");
        assert_eq!(value["models"][0]["role_model"], "openai-codex/gpt-5.6-sol");
        assert_eq!(value["models"][0]["current"], true);
        let human = run_child(
            &["models", "--provider", "pi", "--search", "absent"],
            "missing",
        );
        assert!(human.status.success());
        let human_output = String::from_utf8(human.stdout).unwrap();
        let text = human_output
            .split("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n")
            .nth(1)
            .unwrap();
        assert!(text.contains("No models matched --search") && text.contains("auth: ok"));
        let null = run_child(&["models", "--provider", "pi", "--json"], "missing");
        let null_output = String::from_utf8(null.stdout).unwrap();
        let null_cli = null_output
            .split("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n")
            .nth(1)
            .unwrap();
        let null_value: Value = serde_json::from_str(null_cli.trim()).unwrap();
        assert!(null.status.success() && null_value["current_role_model"].is_null());
        std::fs::write(&pi, "#!/bin/sh\nprintf 'sensitive-stderr' >&2\nexit 7\n").unwrap();
        let failed = run_child(&["models", "--provider", "pi", "--json"], "missing");
        assert!(!failed.status.success());
        let failed_output = String::from_utf8(failed.stdout).unwrap();
        let failed_cli = failed_output
            .split("__TEAM_AGENT_MODELS_CLI_OUTPUT_v1__\n")
            .nth(1)
            .unwrap();
        assert!(!failed_cli.contains("sensitive-stderr"));
        assert_eq!(
            std::fs::read_to_string(pi.with_extension("count"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        let _ = std::fs::remove_file(pi.with_extension("count"));
        let _ = std::fs::remove_file(pi);
    }

    #[test]
    fn current_model_is_deterministic() {
        let models = vec!["openai-codex/gpt-5.6-sol".into()];
        assert_eq!(
            current_role_model_from(&models, Some("openai-codex"), Some("gpt-5.6-sol")),
            Some("openai-codex/gpt-5.6-sol".into())
        );
        assert_eq!(
            current_role_model_from(&models, Some("openai-codex"), Some("other")),
            None
        );
        assert_eq!(
            current_role_model_from(&models, None, Some("gpt-5.6-sol")),
            None
        );
    }
    #[cfg(unix)]
    fn fixture(body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "team-agent-pi-fixture-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$#:$1\" >> \"$0.count\"\n{body}\n"),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn runner_invokes_exact_argv_once_and_drains() {
        let path = fixture(
            "test \"$1\" = --list-models && echo provider model && echo openai-codex gpt-5.6-sol",
        );
        assert!(run_catalog(&path, Duration::from_secs(1), 1024)
            .unwrap()
            .starts_with(b"provider model"));
        assert_eq!(
            std::fs::read_to_string(path.with_extension("count")).unwrap(),
            "1:--list-models\n"
        );
        let _ = std::fs::remove_file(path.with_extension("count"));
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn runner_timeout_is_bounded_when_descendant_keeps_stdout() {
        let path = fixture("sleep 2 & exit 0");
        let started = Instant::now();
        let result = run_catalog(&path, Duration::from_millis(40), 1024);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(result.unwrap_err().contains("timed out"));
        let _ = std::fs::remove_file(path.with_extension("count"));
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn command_boundary_connects_runner_parser_search_and_contract() {
        let path = fixture(
            "printf 'provider model\\nopenai-codex gpt-5.6-sol\\nopenai-codex gpt-5.6-luna\\n'",
        );
        let args = ModelsArgs {
            provider: "pi".into(),
            search: Some("sol".into()),
            json: true,
        };
        let result = cmd_models_with(&args, &path, Duration::from_secs(1), 1024).unwrap();
        let CmdOutput::Human(text) = result.output else {
            panic!("expected JSON projection")
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], "models.v1");
        assert_eq!(value["models"][0]["role_model"], "openai-codex/gpt-5.6-sol");
        assert_eq!(value["models"][0]["current"], false);
        assert_eq!(value["auth"], "ok");
        assert_eq!(value["models"].as_array().unwrap().len(), 1);
        let human = cmd_models_with(
            &ModelsArgs {
                provider: "pi".into(),
                search: Some("absent".into()),
                json: false,
            },
            &path,
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let CmdOutput::Human(text) = human.output else {
            panic!("expected human projection")
        };
        assert!(text.contains("No models matched --search") && text.contains("auth: ok"));
        let refusal = crate::cli::emit::__test_dispatch(
            "models",
            &["--provider".into(), "codex".into()],
            Path::new("/tmp"),
        );
        match refusal {
            Err(CliError::Usage(message)) => {
                assert_eq!(message, "models supports only --provider pi, got \"codex\"")
            }
            other => panic!("expected provider refusal, got {other:?}"),
        }
        let _ = std::fs::remove_file(path.with_extension("count"));
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn runner_fails_closed_for_nonzero_oversize_timeout_and_unavailable() {
        let fail = fixture("echo sensitive-token >&2; exit 7");
        assert_eq!(
            run_catalog(&fail, Duration::from_secs(1), 1024).unwrap_err(),
            "Pi model catalog command failed"
        );
        let failed = cmd_models_with(
            &ModelsArgs {
                provider: "pi".into(),
                search: None,
                json: false,
            },
            &fail,
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        let CmdOutput::Human(text) = failed.output else {
            panic!("expected failure projection")
        };
        assert_eq!(failed.exit, ExitCode::Error);
        assert!(!text.contains("sensitive-token"));
        let big = fixture("head -c 64 /dev/zero");
        assert!(run_catalog(&big, Duration::from_secs(1), 8)
            .unwrap_err()
            .contains("bounded output"));
        let slow = fixture("sleep 2");
        assert!(run_catalog(&slow, Duration::from_millis(20), 1024)
            .unwrap_err()
            .contains("timed out"));
        assert!(run_catalog(
            std::path::Path::new("/does/not/exist"),
            Duration::from_secs(1),
            1024
        )
        .unwrap_err()
        .contains("unavailable"));
        for path in [fail, big, slow] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn public_failure_and_help_contracts_are_versioned() {
        let args = ModelsArgs {
            provider: "pi".into(),
            search: None,
            json: true,
        };
        let result = failure(&args, "safe failure");
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
                &["models".into(), "--provider".into(), "codex".into()],
                Path::new("/tmp")
            ),
            ExitCode::Error
        );
        assert_eq!(
            crate::cli::spec::command_spec("models").unwrap().usage,
            "usage: team-agent models --provider pi [--search TEXT] [--json]"
        );
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
