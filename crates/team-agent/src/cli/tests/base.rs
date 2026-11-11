use super::*;

// RED 契约 lane:argv→run 端到端、五行 summary 字节锁(Gap 18a)、
// classify_agent_bucket(unknown≠idle)、cli-error 信封字节锁、leader_launcher_args 解析、
// consume_leader_inbox_summary 游标+预算截断、send_target fanout 解析。
// Python golden 来源:cli/{parser,commands,helpers}.py @ v0.2.11(439bef8)。
// 所有期望值用 `PYTHONPATH=.../src python3` 实跑 Python 实现抓取的字节级 golden。
use super::*;
use serde_json::json;

// =========================================================================
// provider_args(helpers.py:190-193):values[0]=='--' ? values[1..] : values
// =========================================================================

#[test]
fn provider_args_cases() {
    // (old case-id, input, Python golden); values[1:] of a lone "--" is empty.
    const CASES: &[(&str, &[&str], &[&str])] = &[
        (
            "provider_args_strips_leading_dashdash",
            &["--", "-x"],
            &["-x"],
        ),
        (
            "provider_args_keeps_when_no_leading_dashdash",
            &["-x", "-y"],
            &["-x", "-y"],
        ),
        ("provider_args_empty_is_empty", &[], &[]),
        ("provider_args_lone_dashdash_yields_empty", &["--"], &[]),
    ];
    for (id, input, expected) in CASES {
        let argv: Vec<String> = input.iter().map(|arg| (*arg).to_string()).collect();
        let expected: Vec<String> = expected.iter().map(|arg| (*arg).to_string()).collect();
        assert_eq!(leader_launcher_args(&argv).unwrap().provider_args, expected, "{id}");
    }
}

// =========================================================================
// leader_launcher_args(helpers.py:196-226):attach 旗标解析 + 缺值 Err
// =========================================================================

#[test]
fn leader_launcher_args_success_cases() {
    // (old case-id, argv, complete expected structure), including default false flags.
    let cases: &[(&str, &[&str], LeaderLauncherArgs)] = &[
        (
            "leader_launcher_args_empty_all_default",
            &[],
            LeaderLauncherArgs::default(),
        ),
        (
            "leader_launcher_args_attach_and_confirm",
            &["--attach", "--confirm"],
            LeaderLauncherArgs {
                attach_existing: true,
                confirm_attach: true,
                ..Default::default()
            },
        ),
        // --attach-existing is an alias of --attach.
        (
            "leader_launcher_args_attach_existing_alias",
            &["--attach-existing"],
            LeaderLauncherArgs {
                attach_existing: true,
                ..Default::default()
            },
        ),
        (
            "leader_launcher_args_external_leader_opt_out",
            &["--external-leader", "--", "--model", "opus"],
            LeaderLauncherArgs {
                provider_args: vec!["--model".to_string(), "opus".to_string()],
                external_leader: true,
                ..Default::default()
            },
        ),
        (
            "leader_launcher_args_attach_session_spaced",
            &["--attach-session", "mysess"],
            LeaderLauncherArgs {
                attach_session: Some("mysess".to_string()),
                ..Default::default()
            },
        ),
        (
            "leader_launcher_args_attach_session_equals",
            &["--attach-session=mysess"],
            LeaderLauncherArgs {
                attach_session: Some("mysess".to_string()),
                ..Default::default()
            },
        ),
        // Known Team Agent launcher flags after `--` are rejected by the error table.
        (
            "leader_launcher_args_dashdash_passthrough_strips_separator",
            &["--attach", "--", "-x", "--provider-confirm"],
            LeaderLauncherArgs {
                provider_args: vec!["-x".to_string(), "--provider-confirm".to_string()],
                attach_existing: true,
                ..Default::default()
            },
        ),
        (
            "leader_launcher_args_unknown_tokens_collect_as_provider_args",
            &["foo", "--attach", "bar"],
            LeaderLauncherArgs {
                provider_args: vec!["foo".to_string(), "bar".to_string()],
                attach_existing: true,
                ..Default::default()
            },
        ),
    ];
    for (id, input, expected) in cases {
        let argv: Vec<String> = input.iter().map(|arg| (*arg).to_string()).collect();
        let got = leader_launcher_args(&argv).unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_eq!(got, *expected, "{id}");
    }
}

#[test]
fn leader_launcher_args_error_cases() {
    // Preserve the original error-substring contract, rather than requiring exact equality.
    const CASES: &[(&str, &[&str], &str)] = &[
        (
            "leader_launcher_args_external_leader_after_dashdash_errors",
            &["--", "--external-leader"],
            "Team Agent launcher flag --external-leader must appear before --",
        ),
        (
            "leader_launcher_args_attach_session_missing_value_errors",
            &["--attach-session"],
            "--attach-session requires a tmux session name",
        ),
    ];
    for (id, input, expected) in CASES {
        let argv: Vec<String> = input.iter().map(|arg| (*arg).to_string()).collect();
        let err = leader_launcher_args(&argv).expect_err(id);
        assert!(
            err.to_string().contains(*expected),
            "{id}: unexpected error: {err}"
        );
    }
}

// =========================================================================
// send_target(commands.py:181-184):--to split / target / None
// =========================================================================


// =========================================================================
// classify_agent_bucket / agent_summary_counts(commands.py:309-330)
// bug-071/077/085 铁律:unknown ≠ idle,无匹配态显式落 Unknown
// =========================================================================





// =========================================================================
// interaction_counts(commands.py:292-306):interacted 非空且≠"never"
// =========================================================================


// =========================================================================
// format_status_summary(commands.py:263-289):五行 triage 字节锁(Gap 18a)
// =========================================================================







// =========================================================================
// emit(helpers.py:12-23):--json sort_keys+indent=2 | dict 逐键 | 非 dict
// =========================================================================

#[test]
fn emit_json_sorted_indented() {
    // golden json.dumps(indent=2, sort_keys=True): keys sorted a,b,nested; nested list expanded.
    let out = emit(
        &CmdOutput::Json(json!({"b": 2, "a": 1, "nested": {"x": [1, 2]}})),
        true,
    )
    .expect("json emit returns Some");
    let expected = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"nested\": {\n    \"x\": [\n      1,\n      2\n    ]\n  }\n}";
    assert_eq!(out, expected);
}

#[test]
fn emit_dict_human_per_key() {
    // golden human dict: scalar -> "key: value"; dict/list -> compact json value.
    // KEY INSERTION ORDER preserved (NOT sorted) in non-json path.
    let out = emit(
        &CmdOutput::Json(json!({"key1": "val1", "nested": {"a": 1}, "lst": [1, 2]})),
        false,
    )
    .expect("dict human emit returns Some");
    let expected = "key1: val1\nnested: {\"a\": 1}\nlst: [1, 2]";
    assert_eq!(out, expected);
}

#[test]
fn emit_human_non_dict_passthrough() {
    // golden: non-dict (Human string) printed raw.
    let out =
        emit(&CmdOutput::Human("just a string".into()), false).expect("human emit returns Some");
    assert_eq!(out, "just a string");
}


// =========================================================================
// CliError::to_payload(helpers.py:137-187):稳定信封 + tmux 冲突富化
// =========================================================================

#[test]
fn cli_error_payload_plain_runtime() {
    // golden plain: ok=false, error=str(exc), action=generic, log=path, NO reason/session/next.
    let err = CliError::Runtime("some other error".into());
    let payload = err.to_payload(Path::new("/tmp/y.log"), "status");
    assert!(!payload.ok);
    assert_eq!(payload.error, "some other error");
    assert_eq!(
        payload.action,
        "Run team-agent doctor --workspace . for the selected team, or inspect the error log shown here."
    );
    assert_eq!(payload.log, "/tmp/y.log");
    assert_eq!(payload.reason, None);
    assert_eq!(payload.session_name, None);
    assert_eq!(payload.next_actions, None);
}

#[test]
fn cli_error_payload_tmux_conflict_non_quick_start_enrichment() {
    // golden non-quick-start (command="restart") enrichment uses generic startup wording.
    let err = CliError::Runtime("tmux session already exists: my-team. Startup aborted".into());
    let payload = err.to_payload(Path::new("/tmp/x.log"), "restart");
    assert_eq!(payload.session_name.as_deref(), Some("my-team"));
    assert_eq!(
        payload.action,
        "Terminal session `my-team` already exists and may belong to a running team. Do not shut it down; choose another team name before starting."
    );
    let next = payload.next_actions.as_ref().unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0].contains("team") && next[0].contains("name"));
}

#[test]
fn cli_error_payload_json_shape_serializes_optional_fields_skipped() {
    // skip_serializing_if for reason/session_name/next_actions: plain payload omits them.
    let err = CliError::Runtime("boom".into());
    let payload = err.to_payload(Path::new("/tmp/z.log"), "status");
    let v = serde_json::to_value(&payload).unwrap();
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("reason"), "reason omitted on plain error");
    assert!(!obj.contains_key("session_name"));
    assert!(!obj.contains_key("next_actions"));
    assert_eq!(obj.get("ok"), Some(&json!(false)));
}





// =========================================================================
// CmdResult::from_json(parser.py:507-508):ok is False -> ExitCode::Error
// =========================================================================

#[test]
fn cmd_result_from_json_ok_true_exits_ok() {
    let r = CmdResult::from_json(json!({"ok": true, "x": 1}), true);
    assert_eq!(r.exit, ExitCode::Ok);
    assert!(r.as_json);
    assert_eq!(r.output, CmdOutput::Json(json!({"ok": true, "x": 1})));
}

#[test]
fn cmd_result_from_json_ok_false_exits_error() {
    // parser.py:507: result.get("ok") is False -> SystemExit(1)
    let r = CmdResult::from_json(json!({"ok": false, "error": "x"}), false);
    assert_eq!(r.exit, ExitCode::Error);
    assert!(!r.as_json);
}

#[test]
fn cmd_result_from_json_missing_ok_exits_ok() {
    // result with NO "ok" key: `result.get("ok") is False` is False -> NOT an error (exit Ok).
    // None-vs-missing: absence of ok != ok:false.
    let r = CmdResult::from_json(json!({"summary": "fine"}), false);
    assert_eq!(r.exit, ExitCode::Ok);
}

#[test]
fn exit_code_numeric() {
    assert_eq!(ExitCode::Ok.code(), 0);
    assert_eq!(ExitCode::Error.code(), 1);
}

// =========================================================================
// cmd_doctor: --fix without --gate is a structured failure in both renderers.
// =========================================================================

#[test]
fn cmd_doctor_fix_without_gate_reports_usage_failure() {
    let mut args = DoctorArgs {
        spec: None,
        workspace: PathBuf::from("."),
        gate: None,
        comms: false,
        team: None,
        fix: true,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: false,
    };
    let human = cmd_doctor(&args).expect("structured usage failure");
    assert_eq!(human.exit, ExitCode::Error);
    let CmdOutput::Human(text) = human.output else {
        panic!("expected human triage");
    };
    assert!(text.contains("fix_requires_gate"), "{text}");
    assert!(text.contains("add --gate orphans|comms"), "{text}");

    args.json = true;
    let result = cmd_doctor(&args).expect("structured JSON usage failure");
    assert_eq!(result.exit, ExitCode::Error);
    let CmdOutput::Json(value) = result.output else {
        panic!("expected JSON usage failure");
    };
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "--fix requires --gate");
    assert_eq!(value["issues"], json!(["fix_requires_gate"]));
}

// =========================================================================
// cmd_status 三态互斥(commands.py:90-100)
// =========================================================================

#[test]
fn cmd_status_summary_with_json_is_mutually_exclusive() {
    // commands.py:92-93: --summary and --json -> TeamAgentError(mutually exclusive)
    let args = StatusArgs {
        agent: None,
        workspace: PathBuf::from("."),
        detail: false,
        summary: true,
        json: true,
        team: None,
    };
    let err = cmd_status(&args).unwrap_err();
    assert!(
        err.to_string()
            .contains("--summary and --json are mutually exclusive"),
        "got: {err}"
    );
}

#[test]
fn cmd_status_summary_with_agent_rejected() {
    // commands.py:94-95: --summary + agent -> TeamAgentError(does not accept an agent argument)
    let args = StatusArgs {
        agent: Some("a1".into()),
        workspace: PathBuf::from("."),
        detail: false,
        summary: true,
        json: false,
        team: None,
    };
    let err = cmd_status(&args).unwrap_err();
    assert!(
        err.to_string()
            .contains("status --summary does not accept an agent argument"),
        "got: {err}"
    );
}

// =========================================================================
// cmd_leader_passthrough(parser.py:515-522):-h/--help 早返回 CmdResult::none
// =========================================================================

#[test]
#[serial_test::serial(env)]
fn cmd_leader_passthrough_help_is_pure_human_guidance() {
    let env = crate::cli::hermetic_test_support::HermeticTestEnv::enter("launcher-help-unit");
    let cwd = env.workspace("empty");
    let tools = [
        "pi", "codex", "claude", "copilot", "grok",
    ];
    let bin = env.root().join("canaries");
    std::fs::create_dir(&bin).unwrap();
    #[cfg(unix)]
    let _path = {
        use std::os::unix::fs::PermissionsExt;
        for tool in tools {
            let path = bin.join(tool);
            std::fs::write(&path, "#!/bin/sh\nprintf called > \"$0.called\"\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        env.with_env("PATH", &bin.to_string_lossy())
    };
    for provider in ["pi", "codex", "claude", "copilot", "grok"] {
        for alias in ["-h", "--help"] {
            let result = cmd_leader_passthrough(provider, &[alias.into()], &cwd).unwrap();
            assert_eq!(result.exit, ExitCode::Ok);
            assert!(
                matches!(&result.output, CmdOutput::Human(text) if text.contains(provider) && text.contains("Usage") && text.contains("Examples") && text.contains("Next Action"))
            );
        }
    }
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    assert!(env.registry_entries().is_empty());
    for tool in tools {
        assert!(
            !bin.join(format!("{tool}.called")).exists(),
            "native tool invoked"
        );
    }
}

#[test]
fn cmd_leader_passthrough_maps_copilot_provider() {
    assert_eq!(
        leader_passthrough_provider("codex"),
        Some(crate::model::enums::Provider::Codex)
    );
    assert_eq!(
        leader_passthrough_provider("claude"),
        Some(crate::model::enums::Provider::ClaudeCode)
    );
    assert_eq!(
        leader_passthrough_provider("copilot"),
        Some(crate::model::enums::Provider::Copilot)
    );
}

#[test]
fn cmd_leader_passthrough_maps_grok_and_rejects_retired_cursor_names() {
    assert_eq!(
        leader_passthrough_provider("grok"),
        Some(crate::model::enums::Provider::Grok)
    );
    assert_eq!(leader_passthrough_provider("cursor"), None);
    assert_eq!(leader_passthrough_provider("agent"), None);
    assert_eq!(leader_passthrough_provider("cursor_agent"), None);
    assert_eq!(leader_passthrough_provider("not-a-passthrough"), None);
}

#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn kiro_doctor_inventory_is_read_only_and_does_not_claim_native_readiness() {
    use std::os::unix::fs::PermissionsExt;
    let env = crate::cli::hermetic_test_support::HermeticTestEnv::enter("kiro-inventory");
    let bin = env.workspace("bin");
    let _path = env.with_env("PATH", &bin.to_string_lossy());
    let inventory = provider_doctor_checks();
    assert_eq!(inventory["kiro"]["installed"], false);
    assert_eq!(inventory["codex"]["command"], "codex");
    let launcher = bin.join("kiro-cli");
    std::fs::write(
        &launcher,
        "#!/bin/sh\nprintf called > \"$0.called\"\nexit 0\n",
    )
    .unwrap();
    for (mode, installed) in [(0o600, false), (0o700, true)] {
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            provider_doctor_checks()["kiro"],
            json!({
                "auth": "unknown",
                "command": "kiro-cli",
                "installed": installed,
                "version": "unknown",
                "probe_status": "not_run",
                "provider_probe_status": "not_run",
            })
        );
    }
    assert!(!bin.join("kiro-cli.called").exists());
    assert!(env.registry_entries().is_empty());
}

#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn kiro_leader_entry_has_help_and_typed_refusal_without_native_or_fallback_effects() {
    use std::os::unix::fs::PermissionsExt;
    let env = crate::cli::hermetic_test_support::HermeticTestEnv::enter("kiro-leader-gate");
    let cwd = env.workspace("empty");
    let bin = env.workspace("bin");
    for tool in ["kiro-cli", "kiro-cli-chat", "codex", "pi", "tmux"] {
        let path = bin.join(tool);
        std::fs::write(&path, "#!/bin/sh\nprintf called > \"$0.called\"\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _path = env.with_env("PATH", &bin.to_string_lossy());
    assert!(!crate::cli::emit::is_leader_passthrough_command("kiro"));
    assert_eq!(leader_passthrough_provider("kiro"), None);
    let help = crate::cli::emit::command_help(Some("kiro"));
    assert!(help.contains("team-agent kiro [--json]"));
    assert!(help.contains("kiro_leader_not_admitted"));
    assert!(crate::cli::emit::default_help().contains("Check Kiro leader launch admission"));
    for alias in ["-h", "--help"] {
        assert_eq!(
            crate::cli::run(&["kiro".into(), alias.into()], &cwd),
            ExitCode::Ok
        );
    }
    let cases: &[&[&str]] = &[&[], &["--json"], &["--attach-existing", "--confirm"]];
    for values in cases {
        let args: Vec<String> = values.iter().map(|arg| (*arg).into()).collect();
        let result = cmd_kiro_leader(&args);
        assert_eq!(result.exit, ExitCode::Error);
        assert_eq!(result.as_json, values.contains(&"--json"));
        let CmdOutput::Json(value) = result.output else {
            panic!("expected structured capability refusal");
        };
        assert_eq!(value["schema_version"], "leader_launch.v1");
        assert_eq!(value["ok"], false);
        assert_eq!(value["provider"], "kiro");
        assert_eq!(value["capability"], "leader_launch");
        assert_eq!(value["reason"], "kiro_leader_not_admitted");
        let mut argv = vec!["kiro".into()];
        argv.extend(args);
        let expected = if values.contains(&"--attach-existing") {
            ExitCode::Usage // Unknown controls fail before capability dispatch.
        } else {
            ExitCode::Error // Supported syntax retains the typed refusal.
        };
        assert_eq!(crate::cli::run(&argv, &cwd), expected);
    }
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(&bin).unwrap().count(),
        5,
        "no tool invoked"
    );
    assert!(env.registry_entries().is_empty());
}
