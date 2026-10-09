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
        assert_eq!(provider_args(&argv), expected, "{id}");
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

#[test]
fn send_target_cases() {
    // (old case-id, targets, target, complete expected enum).
    let cases: &[(&str, Option<&str>, Option<&str>, MessageTarget)] = &[
        (
            "send_target_fanout_strips_and_filters_empty",
            Some("a, b ,,c"),
            None,
            MessageTarget::Fanout(vec!["a".to_string(), "b".to_string(), "c".to_string()]),
        ),
        (
            "send_target_single_target",
            None,
            Some("agent_x"),
            MessageTarget::Single("agent_x".to_string()),
        ),
        // A bare "*" means broadcast to the whole team.
        (
            "send_target_broadcast_star",
            None,
            Some("*"),
            MessageTarget::Broadcast,
        ),
        // targets="" is falsy in Python, so it falls through to args.target.
        (
            "send_target_empty_targets_falls_through_to_target",
            Some(""),
            Some("fallback"),
            MessageTarget::Single("fallback".to_string()),
        ),
    ];
    for (id, targets, target, expected) in cases {
        assert_eq!(send_target(*targets, *target), *expected, "{id}");
    }
}

// =========================================================================
// classify_agent_bucket / agent_summary_counts(commands.py:309-330)
// bug-071/077/085 铁律:unknown ≠ idle,无匹配态显式落 Unknown
// =========================================================================

#[test]
fn classify_agent_bucket_cases() {
    // (old case-id, all original (raw, health) inputs, expected bucket).
    const CASES: &[(&str, &[(&str, &str)], SummaryBucket)] = &[
        (
            "classify_failed_takes_priority",
            &[("failed", ""), ("error", ""), ("running", "error")],
            SummaryBucket::Failed,
        ),
        (
            "classify_stopped",
            &[("stopped", ""), ("done", ""), ("running", "done")],
            SummaryBucket::Stopped,
        ),
        (
            "classify_busy",
            &[("busy", ""), ("", "running"), ("", "working")],
            SummaryBucket::Busy,
        ),
        // hstatus==idle precedes the raw==running branch.
        (
            "classify_hstatus_idle_beats_raw_running",
            &[("running", "idle")],
            SummaryBucket::Idle,
        ),
        (
            "classify_pure_running",
            &[("running", "")],
            SummaryBucket::Running,
        ),
        // bug-071/077/085: unmatched and blocked/stuck/missing are Unknown, NEVER idle.
        (
            "classify_blocked_and_unmatched_are_unknown_never_idle",
            &[
                ("blocked", ""),
                ("stuck", ""),
                ("", "missing"),
                ("weird_value", ""),
                ("", ""),
            ],
            SummaryBucket::Unknown,
        ),
    ];
    for (id, inputs, expected) in CASES {
        for &(raw, health) in inputs.iter() {
            assert_eq!(
                classify_agent_bucket(raw, health),
                *expected,
                "{id}: raw={raw:?}, health={health:?}"
            );
        }
    }
}

#[test]
fn agent_summary_counts_mixed_golden() {
    // golden (empty health): a1 running->Running; a2 busy->Busy; a3 failed->Failed;
    //   a4 stopped->Stopped; a5 blocked->Unknown; a6 ""->Unknown; a7 weird->Unknown.
    //   => running=1 busy=1 idle=0 stopped=1 failed=1 unknown=3
    let agents = json!({
        "a1": {"status": "running"},
        "a2": {"status": "busy"},
        "a3": {"status": "failed"},
        "a4": {"status": "stopped"},
        "a5": {"status": "blocked"},
        "a6": {"status": ""},
        "a7": {"status": "weird_value"},
    });
    let got = agent_summary_counts(&agents, &json!({}));
    assert_eq!(
        got,
        SummaryCounts {
            running: 1,
            busy: 1,
            idle: 0,
            stopped: 1,
            failed: 1,
            unknown: 3
        }
    );
    assert_eq!(got.total(), 7);
}

#[test]
fn agent_summary_counts_none_agent_is_unknown() {
    // golden: {"x": None} -> unknown=1
    let got = agent_summary_counts(&json!({"x": Value::Null}), &json!({}));
    assert_eq!(
        got,
        SummaryCounts {
            unknown: 1,
            ..Default::default()
        }
    );
}

#[test]
fn agent_summary_counts_uppercase_status_lowercased() {
    // golden: {"x":{"status":"RUNNING"}} -> running=1 (str(...).lower())
    let got = agent_summary_counts(&json!({"x": {"status": "RUNNING"}}), &json!({}));
    assert_eq!(
        got,
        SummaryCounts {
            running: 1,
            ..Default::default()
        }
    );
}

// =========================================================================
// interaction_counts(commands.py:292-306):interacted 非空且≠"never"
// =========================================================================

#[test]
fn interaction_counts_mixed_golden() {
    // golden: a:"5m ago"->interacted; b:"never"->never; c:""->never; d:{}->never; e:None->never
    // result (1, 4)
    let agents = json!({
        "a": {"interacted": "5m ago"},
        "b": {"interacted": "never"},
        "c": {"interacted": ""},
        "d": {},
        "e": Value::Null,
    });
    let got = interaction_counts(&agents);
    assert_eq!(
        got,
        InteractionCounts {
            interacted: 1,
            never: 4
        }
    );
}

// =========================================================================
// format_status_summary(commands.py:263-289):五行 triage 字节锁(Gap 18a)
// =========================================================================

#[test]
fn format_status_summary_full_byte_lock() {
    // golden:
    // coordinator: running schema_ok=True tmux=True
    // receiver: %3 cmd=codex topology=external
    // agents: 2 — running=1 busy=1 idle=0 stopped=0 failed=0 unknown=0
    // queued: 2 mailbox messages awaiting delivery
    // latest result: a1 -> did the thing @ -
    let data = json!({
        "coordinator": {"status": "running", "schema_ok": true},
        "leader_receiver": {"pane_id": "%3", "pane_current_command": "codex"},
        "agents": {"a1": {"status": "running"}, "a2": {"status": "busy"}},
        "agent_health": {},
        "tmux_session_present": true,
        "queued_messages": [1, 2],
        "latest_results": [{"agent_id": "a1", "summary": "did the thing", "created_at": Value::Null}],
    });
    let got = format_status_summary(&data);
    let expected = "coordinator: running schema_ok=true tmux=true\n\
receiver: %3 cmd=codex topology=external\n\
agents: 2 — running=1 busy=1 idle=0 stopped=0 failed=0 unknown=0\n\
queued: 2 mailbox messages awaiting delivery\n\
latest result: a1 -> did the thing @ -";
    assert_eq!(got, expected);
}

#[test]
fn format_status_summary_empty_byte_lock() {
    // golden empty data: stopped/false/false, dashes, 0 counts, none latest.
    let got = format_status_summary(&json!({}));
    let expected = "coordinator: stopped schema_ok=false tmux=false\n\
receiver: - cmd=- topology=external\n\
agents: 0 — running=0 busy=0 idle=0 stopped=0 failed=0 unknown=0\n\
queued: 0 mailbox messages awaiting delivery\n\
latest result: none";
    assert_eq!(got, expected);
}

#[test]
fn format_status_summary_interacted_marker_appended() {
    // golden: when interacted>0, agents line gets " (1 interacted, 1 never)" suffix.
    let data = json!({
        "coordinator": {},
        "agents": {"a1": {"status": "running", "interacted": "3m"}, "a2": {"status": "idle"}},
        "agent_health": {"a2": {"status": "idle"}},
    });
    let got = format_status_summary(&data);
    let line2 = got.lines().nth(2).unwrap();
    assert_eq!(
        line2,
        "agents: 2 — running=1 busy=0 idle=1 stopped=0 failed=0 unknown=0 (1 interacted, 1 never)"
    );
}

#[test]
fn format_status_summary_no_interacted_marker_when_zero() {
    // Gap 18a contract: interacted==0 -> line[2] stays byte-identical with NO marker suffix.
    let data = json!({
        "agents": {"a1": {"status": "running"}},
        "agent_health": {},
    });
    let line2 = format_status_summary(&data)
        .lines()
        .nth(2)
        .unwrap()
        .to_string();
    assert_eq!(
        line2,
        "agents: 1 — running=1 busy=0 idle=0 stopped=0 failed=0 unknown=0"
    );
    assert!(
        !line2.contains("interacted"),
        "no marker when interacted==0"
    );
}

#[test]
fn format_status_csv_preserves_agent_order_and_collapses_errors() {
    let data = json!({
        "agents": {
            "zeta": {"status": "idle", "pane_id": "%1"},
            "alpha": {"status": "running", "pane_id": "%2"},
            "err_failed": {"status": "failed", "pane_id": "%3"},
            "err_missing_pane": {"status": "running"},
            "err_unknown": {"status": "mystery", "pane_id": "%4"},
            "err_stopped": {"status": "stopped", "pane_id": "%5"}
        },
        "agent_health": {
            "alpha": {"status": "working"}
        }
    });
    assert_eq!(
            format_status_csv(&data),
            "zeta,idle\nalpha,working\nerr_failed,error\nerr_missing_pane,error\nerr_unknown,error\nerr_stopped,error"
        );
}

#[test]
fn format_status_csv_zero_workers_is_empty() {
    assert_eq!(
        format_status_csv(&json!({"agents": {}, "agent_health": {}})),
        ""
    );
}

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

#[test]
fn emit_none_output_produces_nothing() {
    // passthrough/watch: CmdOutput::None never reaches emit -> None (no stdout line).
    assert_eq!(emit(&CmdOutput::None, false), None);
    assert_eq!(emit(&CmdOutput::None, true), None);
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
fn cli_error_payload_tmux_conflict_quick_start_enrichment() {
    // golden quick-start enrichment (exact bytes):
    let err = CliError::Runtime("tmux session already exists: my-team. Startup aborted".into());
    let payload = err.to_payload(Path::new("/tmp/cli-error-123.log"), "quick-start");
    assert_eq!(
        payload.reason.as_deref(),
        Some("tmux_session_name_conflict")
    );
    assert_eq!(payload.session_name.as_deref(), Some("my-team"));
    // E8 (N38): quick-start 撞已有 runtime 引导到 restart(resume);
    // context reset is only through restart --allow-fresh with explicit consent.
    assert_eq!(
            payload.action,
            "Terminal session `my-team` already exists and may belong to your existing team. Resume with team-agent restart; use --allow-fresh only after explicitly accepting loss of saved context. To create another team, change the name in TEAM.md before quick-start. Do not shut down an existing team to bypass this refusal."
        );
    let next = payload.next_actions.as_ref().unwrap();
    assert_eq!(next.len(), 2);
    assert!(next[0].contains("team-agent restart"));
    assert!(
        next[1].contains("TEAM.md") && next[1].contains("name") && next[1].contains("quick-start")
    );
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

#[test]
fn consume_inbox_missing_file_returns_none() {
    // helpers.py:30-31: inbox_path absent -> None (no crash).
    let ws = tmp_workspace();
    assert_eq!(consume_leader_inbox_summary(&ws, 500), None);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn consume_inbox_single_entry_summary_and_cursor_advance() {
    // golden _leader_inbox_summary single entry:
    //   "Leader inbox: 1 new fallback entry\n- Hello world message\nHint: team-agent inbox leader"
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    std::fs::write(&inbox, "[x fallback]\nHello world message").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("new entry -> Some");
    assert_eq!(
        summary,
        "Leader inbox: 1 new fallback entry\n- Hello world message\nHint: team-agent inbox leader"
    );
    // cursor advanced: a second call with no new bytes -> None (offset==size).
    assert_eq!(consume_leader_inbox_summary(&ws, 500), None);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn consume_inbox_two_entries_plural() {
    // golden two-entry summary uses plural "entries".
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    std::fs::write(&inbox, "[a fallback]\nFirst msg\n[b fallback]\nSecond msg").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("Some");
    assert_eq!(
            summary,
            "Leader inbox: 2 new fallback entries\n- First msg\n- Second msg\nHint: team-agent inbox leader"
        );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn consume_inbox_budget_truncation_footer() {
    // golden budget=200: header + 2 lines then truncation footer (exact bytes from Python).
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    let many: String = (0..20)
        .map(|i| format!("[e{i} fallback]\nMessage number {i} with some text padding here"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&inbox, &many).unwrap();
    let summary = consume_leader_inbox_summary(&ws, 200).expect("Some");
    let expected = "Leader inbox: 20 new fallback entries\n\
- Message number 0 with some text padding here\n\
- Message number 1 with some text padd ...\n\
Truncated: more fallback entries available; run team-agent inbox leader";
    assert_eq!(summary, expected);
    let _ = std::fs::remove_dir_all(&ws);
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
        "pi", "codex", "claude", "copilot", "grok", "agent", "cursor",
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
    for provider in ["pi", "codex", "claude", "copilot", "grok", "cursor"] {
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
fn cmd_leader_passthrough_maps_grok_and_cursor_not_claude() {
    assert_eq!(
        leader_passthrough_provider("grok"),
        Some(crate::model::enums::Provider::Grok)
    );
    assert_eq!(
        leader_passthrough_provider("cursor"),
        Some(crate::model::enums::Provider::CursorAgent)
    );
    assert_ne!(
        leader_passthrough_provider("cursor"),
        Some(crate::model::enums::Provider::ClaudeCode)
    );
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
    std::fs::write(&launcher, "#!/bin/sh\nprintf called > \"$0.called\"\nexit 0\n").unwrap();
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
        assert_eq!(crate::cli::run(&argv, &cwd), ExitCode::Error);
    }
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 5, "no tool invoked");
    assert!(env.registry_entries().is_empty());
}
