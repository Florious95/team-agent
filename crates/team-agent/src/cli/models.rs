//! Read-only Pi model catalog discovery for `team-agent models`.
use super::{CliError, CmdOutput, CmdResult, ExitCode, ModelsArgs};
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::BTreeMap;
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
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

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
    fn real_dispatcher_isolated_success_json_human_and_no_match() {
        let pi = fixture("printf 'provider model\\nopenai-codex gpt-5.6-sol\\n'");
        let dir = pi.parent().unwrap().to_path_buf();
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
        cleanup_fixture(&pi);
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
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "team-agent-pi-fixture-{}-{}-{sequence}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("pi");
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
    fn cleanup_fixture(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    fn native_timeout_fixture() -> std::path::PathBuf {
        let path = std::path::PathBuf::from(env!("TEAM_AGENT_MODELS_TIMEOUT_FIXTURE"));
        assert!(path.is_file(), "prebuilt models timeout fixture is missing");
        path
    }

    #[cfg(unix)]
    fn native_timeout_receipt() -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::path::PathBuf::from("/tmp").join(format!(
            "team-agent-models-timeout-{}-{stamp}.receipt",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn native_timeout_stage_receipt(receipt: &Path) -> std::path::PathBuf {
        receipt.with_extension("stage")
    }

    #[cfg(unix)]
    fn parse_native_timeout_receipt(path: &Path) -> Option<BTreeMap<String, String>> {
        let mut fields = BTreeMap::new();
        for line in std::fs::read_to_string(path).ok()?.lines() {
            let (key, value) = line.split_once('=')?;
            if fields.insert(key.to_string(), value.to_string()).is_some() {
                return None;
            }
        }
        let keys = fields.keys().map(String::as_str).collect::<Vec<_>>();
        (keys
            == vec![
                "child_argv_exact",
                "child_pid",
                "child_spawn_attempts",
                "descendant_pid",
                "descendant_started",
                "descendant_stdout_inherited",
                "parent_argv_exact",
                "parent_pid",
                "parent_started_once",
            ])
        .then_some(fields)
    }

    #[cfg(unix)]
    fn wait_native_timeout_receipt(path: &Path) -> BTreeMap<String, String> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Some(fields) = parse_native_timeout_receipt(path) {
                return fields;
            }
            if Instant::now() >= deadline {
                panic!("native timeout receipt did not become complete after runner return");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[cfg(unix)]
    fn with_native_mode<T>(
        mode: &str,
        action: impl FnOnce() -> T,
    ) -> (T, crate::lifecycle::launch::pi_mcp::PiCatalogTestObservation) {
        let receipt = native_timeout_receipt();
        assert!(!receipt.exists());
        let result = crate::lifecycle::launch::pi_mcp::with_pi_catalog_test_observation(
            &receipt,
            Some(mode),
            action,
        );
        assert!(!receipt.exists());
        let _ = std::fs::remove_file(format!("{}.sock", receipt.display()));
        result
    }

    #[cfg(unix)]
    fn safe_runner_observation(
        elapsed: Duration,
        observation: &crate::lifecycle::launch::pi_mcp::PiCatalogTestObservation,
    ) -> String {
        format!(
            "elapsed_ms={} spawn_count={} argv={:?} parent_pid={:?} parent_exit_success={:?} parent_exit_code={:?} reader_timeout={}",
            elapsed.as_millis(),
            observation.spawn_count,
            observation.argv,
            observation.parent_pid,
            observation.parent_exit_success,
            observation.parent_exit_code,
            observation.reader_timeout,
        )
    }

    #[cfg(unix)]
    fn safe_models_result_class(result: &CmdResult) -> &'static str {
        let CmdOutput::Human(text) = &result.output else {
            return "non_human_output";
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return "non_json_output";
        };
        match value.get("ok").and_then(Value::as_bool) {
            Some(true) => "success_json",
            Some(false) => match value.get("error").and_then(Value::as_str) {
                Some(error) if error.contains("unavailable") => "runner_unavailable",
                Some(error) if error.contains("timed out") => "runner_timeout",
                Some(error) if error.contains("command failed") => "runner_exit",
                Some(error) if error.contains("could not be read") => "reader_error",
                Some(error) if error.contains("exceeds") => "reader_oversize",
                Some(error) if error.contains("catalog") => "parser_catalog_error",
                Some(_) => "failure_json",
                None => "failure_json_missing_error",
            },
            None => "json_missing_ok",
        }
    }

    #[cfg(unix)]
    fn safe_models_observation(
        elapsed: Duration,
        result: &CmdResult,
        observation: &crate::lifecycle::launch::pi_mcp::PiCatalogTestObservation,
    ) -> String {
        format!(
            "class={} elapsed_ms={} parent_pid={:?} parent_exit_success={:?} parent_exit_code={:?} reader_timeout={}",
            safe_models_result_class(result),
            elapsed.as_millis(),
            observation.parent_pid,
            observation.parent_exit_success,
            observation.parent_exit_code,
            observation.reader_timeout,
        )
    }

    #[cfg(unix)]
    #[test]
    fn runner_invokes_exact_argv_once_and_drains() {
        let path = fixture(
            "test \"$1\" = --list-models && echo provider model && echo openai-codex gpt-5.6-sol",
        );
        let receipt = native_timeout_receipt();
        assert!(!receipt.exists());
        let started = Instant::now();
        let (result, observation) =
            crate::lifecycle::launch::pi_mcp::with_pi_catalog_test_observation(
                &receipt,
                None,
                || run_catalog(&path, Duration::from_secs(1), 1024),
            );
        let elapsed = started.elapsed();
        assert!(
            result
                .as_ref()
                .is_ok_and(|bytes| bytes.starts_with(b"provider model")),
            "runner observation: {}",
            safe_runner_observation(elapsed, &observation)
        );
        let _ = result.unwrap();
        let _ = std::fs::remove_file(&receipt);
        let _ = std::fs::remove_file(format!("{}.sock", receipt.display()));
        assert_eq!(
            std::fs::read_to_string(path.with_extension("count")).unwrap(),
            "1:--list-models\n"
        );
        cleanup_fixture(&path);
    }

    #[cfg(unix)]
    const NATIVE_STAGE_KEYS: [&str; 5] = [
        "parent_entry_ms",
        "parent_before_child_spawn_ms",
        "parent_after_child_spawn_ms",
        "parent_before_exit_ms",
        "test_spawn_return_ms",
    ];

    #[cfg(unix)]
    // Match the native helper's UNIX epoch millisecond clock exactly.
    fn append_native_stage(path: &Path, key: &str) {
        if !NATIVE_STAGE_KEYS.contains(&key) {
            return;
        }
        let Some(milliseconds) = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis())
        else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        else {
            return;
        };
        let _ = writeln!(file, "{key}={milliseconds}");
    }

    #[cfg(unix)]
    fn safe_native_stage(path: &Path) -> String {
        let (read_state, text) = match std::fs::read_to_string(path) {
            Ok(text) => ("ok", Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ("missing", None),
            Err(_) => ("unavailable", None),
        };
        let mut fields = BTreeMap::new();
        if let Some(text) = text {
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                if NATIVE_STAGE_KEYS.contains(&key)
                    && value.len() <= 20
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    fields.entry(key).or_insert(value);
                }
            }
        }
        let field = |key: &str| fields.get(key).copied().unwrap_or("unknown");
        format!(
            "stage_read={read_state} parent_entry_ms={} parent_before_child_spawn_ms={} parent_after_child_spawn_ms={} parent_before_exit_ms={} test_spawn_return_ms={}",
            field("parent_entry_ms"),
            field("parent_before_child_spawn_ms"),
            field("parent_after_child_spawn_ms"),
            field("parent_before_exit_ms"),
            field("test_spawn_return_ms"),
        )
    }

    #[cfg(unix)]
    fn wait_for_native_parent_exit(
        child: &mut std::process::Child,
        stage: &Path,
    ) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(None) => {
                    let before_kill = safe_native_stage(stage);
                    let _ = child.kill();
                    let _ = child.wait();
                    let after_wait = safe_native_stage(stage);
                    panic!(
                        "native parent did not exit before test setup deadline; stage_before_kill={before_kill}; stage_after_wait={after_wait}"
                    );
                }
                Err(_) => {
                    let before_kill = safe_native_stage(stage);
                    let _ = child.kill();
                    let _ = child.wait();
                    let after_wait = safe_native_stage(stage);
                    panic!(
                        "native parent status could not be observed; stage_before_kill={before_kill}; stage_after_wait={after_wait}"
                    );
                }
            }
        }
    }

    /// Read/drain seam only: public `run_catalog` spawn coverage remains in the
    /// normal success and fail-closed tests.
    #[cfg(unix)]
    #[test]
    fn reader_deadline_is_bounded_after_parent_exit_with_descendant_pipe() {
        let path = native_timeout_fixture();
        let receipt = native_timeout_receipt();
        let stage = native_timeout_stage_receipt(&receipt);
        assert!(!receipt.exists());
        assert!(!stage.exists());
        let mut parent = std::process::Command::new(&path)
            .arg("--list-models")
            .env("TEAM_AGENT_MODELS_TIMEOUT_RECEIPT", &receipt)
            .env("TEAM_AGENT_MODELS_TIMEOUT_STAGE_RECEIPT", &stage)
            .env("TEAM_AGENT_MODELS_TIMEOUT_MODE", "descendant")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        append_native_stage(&stage, "test_spawn_return_ms");
        let parent_pid = parent.id();
        let parent_status = wait_for_native_parent_exit(&mut parent, &stage);
        let stdout = parent.stdout.take().expect("native parent stdout pipe");
        let started = Instant::now();
        let (result, observation) =
            crate::lifecycle::launch::pi_mcp::with_pi_catalog_test_observation(
                &receipt,
                Some("descendant"),
                || {
                    crate::lifecycle::launch::pi_mcp::run_pi_catalog_after_parent_exit_for_test(
                        stdout,
                        parent_pid,
                        parent_status,
                        Duration::from_millis(40),
                        1024,
                    )
                },
            );
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "runner observation: {}",
            safe_runner_observation(elapsed, &observation)
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert_eq!(
            observation.parent_exit_success,
            Some(true),
            "runner observation: {}",
            safe_runner_observation(elapsed, &observation)
        );
        assert_eq!(observation.parent_exit_code, Some(0));
        assert!(observation.reader_timeout);

        let fields = wait_native_timeout_receipt(&receipt);
        assert_eq!(fields["parent_started_once"], "1");
        assert_eq!(fields["parent_argv_exact"], "1");
        assert_eq!(fields["child_spawn_attempts"], "1");
        assert_eq!(fields["child_argv_exact"], "1");
        assert_eq!(fields["descendant_started"], "1");
        assert_eq!(fields["descendant_stdout_inherited"], "1");
        assert_eq!(
            fields["parent_pid"],
            observation.parent_pid.unwrap().to_string()
        );
        assert_eq!(fields["child_pid"], fields["descendant_pid"]);
        let _ = std::fs::remove_file(&receipt);
        let _ = std::fs::remove_file(&stage);
        let _ = std::fs::remove_file(format!("{}.sock", receipt.display()));
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
        let receipt = native_timeout_receipt();
        assert!(!receipt.exists());
        let started = Instant::now();
        let (result, observation) =
            crate::lifecycle::launch::pi_mcp::with_pi_catalog_test_observation(
                &receipt,
                None,
                || cmd_models_with(&args, &path, Duration::from_secs(1), 1024),
            );
        let elapsed = started.elapsed();
        let result = match result {
            Ok(result) => result,
            Err(_) => panic!(
                "command boundary returned a CLI error; class=cli_error elapsed_ms={} parent_pid={:?} parent_exit_success={:?} parent_exit_code={:?} reader_timeout={}",
                elapsed.as_millis(),
                observation.parent_pid,
                observation.parent_exit_success,
                observation.parent_exit_code,
                observation.reader_timeout,
            ),
        };
        let CmdOutput::Human(text) = &result.output else {
            panic!(
                "expected JSON projection; {}",
                safe_models_observation(elapsed, &result, &observation)
            )
        };
        let value: Value = serde_json::from_str(text).unwrap_or_else(|_| {
            panic!(
                "expected valid JSON projection; {}",
                safe_models_observation(elapsed, &result, &observation)
            )
        });
        assert_eq!(
            value["schema_version"],
            "models.v1",
            "{}",
            safe_models_observation(elapsed, &result, &observation)
        );
        assert_eq!(
            value["models"][0]["role_model"],
            "openai-codex/gpt-5.6-sol",
            "{}",
            safe_models_observation(elapsed, &result, &observation)
        );
        assert_eq!(
            value["models"][0]["current"],
            false,
            "{}",
            safe_models_observation(elapsed, &result, &observation)
        );
        assert_eq!(
            value["auth"],
            "ok",
            "{}",
            safe_models_observation(elapsed, &result, &observation)
        );
        let model_count = value["models"].as_array().map(|models| models.len());
        assert_eq!(
            model_count,
            Some(1),
            "{}",
            safe_models_observation(elapsed, &result, &observation)
        );
        let _ = std::fs::remove_file(&receipt);
        let _ = std::fs::remove_file(format!("{}.sock", receipt.display()));
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
        cleanup_fixture(&path);
    }

    #[cfg(unix)]
    #[test]
    fn runner_fails_closed_for_nonzero_oversize_timeout_and_unavailable() {
        let fail = native_timeout_fixture();
        let started = Instant::now();
        let (failed_result, fail_observation) = with_native_mode("exit7", || {
            run_catalog(&fail, Duration::from_secs(1), 1024)
        });
        let elapsed = started.elapsed();
        assert_eq!(
            failed_result.unwrap_err(),
            "Pi model catalog command failed",
            "runner observation: {}",
            safe_runner_observation(elapsed, &fail_observation)
        );
        assert_eq!(fail_observation.spawn_count, 1);
        assert_eq!(fail_observation.argv, vec!["--list-models"]);
        assert_eq!(fail_observation.parent_exit_success, Some(false));
        assert_eq!(fail_observation.parent_exit_code, Some(7));
        assert!(!fail_observation.reader_timeout);
        let (failed, failed_observation) = with_native_mode("exit7", || {
            cmd_models_with(
                &ModelsArgs {
                    provider: "pi".into(),
                    search: None,
                    json: false,
                },
                &fail,
                Duration::from_secs(1),
                1024,
            )
        });
        let failed = failed.unwrap();
        assert_eq!(failed_observation.spawn_count, 1);
        assert_eq!(failed_observation.argv, vec!["--list-models"]);
        assert_eq!(failed_observation.parent_exit_success, Some(false));
        assert_eq!(failed_observation.parent_exit_code, Some(7));
        assert!(!failed_observation.reader_timeout);
        let CmdOutput::Human(text) = failed.output else {
            panic!("expected failure projection")
        };
        assert_eq!(failed.exit, ExitCode::Error);
        assert!(!text.contains("sensitive-token"));
        let big = native_timeout_fixture();
        let (big_result, big_observation) = with_native_mode("oversize", || {
            run_catalog(&big, Duration::from_secs(1), 8)
        });
        assert!(big_result.unwrap_err().contains("bounded output"));
        assert_eq!(big_observation.spawn_count, 1);
        assert_eq!(big_observation.argv, vec!["--list-models"]);
        assert_eq!(big_observation.parent_exit_success, Some(true));
        assert_eq!(big_observation.parent_exit_code, Some(0));
        assert!(!big_observation.reader_timeout);
        let slow = native_timeout_fixture();
        let (slow_result, slow_observation) = with_native_mode("sleep", || {
            run_catalog(&slow, Duration::from_millis(20), 1024)
        });
        assert!(slow_result.unwrap_err().contains("timed out"));
        assert_eq!(slow_observation.spawn_count, 1);
        assert_eq!(slow_observation.argv, vec!["--list-models"]);
        assert_eq!(slow_observation.parent_exit_success, None);
        assert!(!slow_observation.reader_timeout);
        assert!(run_catalog(
            std::path::Path::new("/does/not/exist"),
            Duration::from_secs(1),
            1024
        )
        .unwrap_err()
        .contains("unavailable"));
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
