use super::*;

    // =========================================================================
    // status_compact_flag (commands.py:99): compact = !detail — CLI 独占不变量
    // gate: 'detail=false => compact=true mapping, the one byte-level invariant CLI owns'.
    // =========================================================================

    #[test]
    fn status_compact_flag_default_is_compact() {
        // golden: cmd_status without --detail -> runtime.status(compact=not(False)) == compact=True.
        assert!(status_compact_flag(false), "detail=false MUST map to compact=true (commands.py:99)");
    }

    #[test]
    fn status_compact_flag_detail_is_full() {
        // golden: cmd_status --detail -> runtime.status(compact=not(True)) == compact=False.
        assert!(!status_compact_flag(true), "detail=true MUST map to compact=false (full projection)");
    }

    // =========================================================================
    // status_port::status — REAL caller against SEEDED fixture (gate: 'zero callers').
    // Asserts the --json projection shape that the compact-vs-detail wiring selects.
    // RED: status_port::status is unimplemented!() so the call panics until ported.
    // =========================================================================

    #[test]
    fn status_port_status_compact_json_shape_against_seeded_fixture() {
        // cmd_status json branch (detail=false) delegates status_port::status(compact=true).
        // compact_status (status/compact.py:8-37) projects to a STABLE key set; assert the
        // load-bearing keys survive and `last_events` is bounded (compact truncates events).
        let ws = seed_status_workspace();
        let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
            .expect("seeded fixture status should project a value");
        let obj = v.as_object().expect("--json status is a dict");
        // compact_status's exact top-level key set (compact.py:9-37):
        for key in [
            "team",
            "session_name",
            "leader_topology",
            "is_external_leader",
            "leader_attach_command",
            "leader_client",
            "tmux_session_present",
            "leader_receiver",
            "agents",
            "agent_health",
            "tasks",
            "messages",
            "queued_messages",
            "results",
            "latest_results",
            "coordinator",
            "last_events",
        ] {
            assert!(obj.contains_key(key), "compact status missing key `{key}`");
        }
        // seeded agent surfaces through the projection.
        assert!(
            obj["agents"].as_object().unwrap().contains_key("a1"),
            "seeded agent a1 must appear in compact agents projection"
        );
        // compact bounds: queued_messages[:8] and latest_results[:5] -> arrays.
        assert!(obj["queued_messages"].is_array());
        assert!(obj["latest_results"].as_array().unwrap().len() <= 5);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn status_port_status_reports_managed_leader_topology_and_attach_command() {
        let ws = seed_status_workspace();
        let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
        if let Some(obj) = state.as_object_mut() {
            obj.insert("is_external_leader".to_string(), json!(false));
            obj.insert("session_name".to_string(), json!("team-current"));
        }
        crate::state::persist::save_runtime_state(&ws, &state).unwrap();

        let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");

        assert_eq!(v["leader_topology"], json!("managed"));
        assert_eq!(v["is_external_leader"], json!(false));
        let attach = v["leader_attach_command"]
            .as_str()
            .expect("managed status includes attach command");
        assert!(attach.contains("attach -t team-current:leader"), "{attach}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn status_port_status_detail_full_keeps_uncompacted_events() {
        // cmd_status --json --detail -> status_port::status(compact=false): the FULL dict
        // (queries.py:65-79) is returned WITHOUT compact_status truncation. Distinguishing
        // invariant: full result preserves `tasks` rows verbatim (not compact_task-projected),
        // so the seeded task's full row (incl. fields compact would drop) survives.
        let ws = seed_status_workspace();
        let full = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true)
            .expect("seeded fixture full status should project a value");
        let compact = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
            .expect("seeded fixture compact status should project a value");
        // The CLI-owned invariant: detail=true => compact=false; the two projections MUST
        // differ in shape (full carries `messages`/`results` count maps + verbatim tasks).
        assert_ne!(full, compact, "detail (full) and default (compact) projections must differ");
        let full_obj = full.as_object().expect("full status is a dict");
        assert!(full_obj.contains_key("last_events"), "full status keeps last_events");
        assert_eq!(full_obj["agents"].as_object().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // =========================================================================
    // send_options_from_args (commands.py:170-177): SendArgs->SendOptions 旗标取反
    // gate: 'no_ack:true => requires_ack:false and no_wait:true => wait_visible:false';
    //        watch_result flag maps into SendOptions.
    // RED: send_options_from_args is unimplemented!() until ported.
    // =========================================================================

    fn send_args_fixture() -> SendArgs {
        SendArgs {
            target: Some("alice".into()),
            message: vec!["hello".into(), "world".into(), "foo".into()],
            targets: None,
            workspace: PathBuf::from("."),
            team: Some("teamA".into()),
            task: Some("t-1".into()),
            sender: "leader".into(),
            no_ack: true,
            no_wait: true,
            watch_result: true,
            timeout: 12.5,
            confirm_human: false,
            json: false,
            message_id: None,
        }
    }

    #[test]
    fn send_options_negates_no_ack_and_no_wait_and_carries_watch() {
        // golden (commands.py:172,174,176): requires_ack=not no_ack; wait_visible=not no_wait;
        //   watch_result passthrough. With no_ack=true,no_wait=true,watch_result=true:
        //   requires_ack=false, wait_visible=false, watch_result=true.
        let opts = send_options_from_args(&send_args_fixture());
        assert!(!opts.requires_ack, "no_ack:true MUST map to requires_ack:false (off-by-inversion guard)");
        assert!(!opts.wait_visible, "no_wait:true MUST map to wait_visible:false");
        assert!(opts.watch_result, "watch_result flag MUST pass through into SendOptions");
        assert!(!opts.confirm_human);
        assert_eq!(opts.sender, "leader");
        assert_eq!(opts.timeout, 12.5);
    }

    #[test]
    fn send_options_default_flags_are_acked_and_waited() {
        // golden: no_ack=false,no_wait=false,watch_result=false ->
        //   requires_ack=true, wait_visible=true, watch_result=false (Python defaults inverted back).
        let args = SendArgs {
            no_ack: false,
            no_wait: false,
            watch_result: false,
            ..send_args_fixture()
        };
        let opts = send_options_from_args(&args);
        assert!(opts.requires_ack, "no_ack:false MUST map to requires_ack:true");
        assert!(opts.wait_visible, "no_wait:false MUST map to wait_visible:true");
        assert!(!opts.watch_result);
    }

    // =========================================================================
    // cmd_send — REAL caller (gate: 'cmd_send has NO test beyond send_target').
    // Asserts (1) message Vec joined by single space surfaces to send_message,
    // (2) the registered-watcher notice ({status:'registered',...} -> result['watch'],
    //     send.py:326-337) survives into CmdResult Json output,
    // (3) DeliveryOutcome->exit-code derivation (ok=true -> ExitCode::Ok).
    // RED: cmd_send is unimplemented!() so it panics until ported.
    // =========================================================================

    #[test]
    fn cmd_send_joins_message_with_single_space() {
        // golden (commands.py:169): " ".join(["hello","world","foo"]) == "hello world foo".
        // Drive cmd_send; the joined content must reach send_message (RED until ported).
        let r = cmd_send(&send_args_fixture()).expect("cmd_send returns CmdResult");
        // The delegate's DeliveryOutcome -> Json must carry an `ok` key feeding exit-code.
        match r.output {
            CmdOutput::Json(ref v) => {
                assert!(v.get("ok").is_some(), "send result Json must carry `ok`");
            }
            other => panic!("cmd_send must emit Json DeliveryOutcome, got {other:?}"),
        }
    }

    #[test]
    fn cmd_send_watch_result_surfaces_registered_notice() {
        // gate CRITICAL: --watch-result -> SendOptions.watch_result=true -> send_message attaches
        // result['watch']={status:'registered', watcher_id, task_id, agent_id, notice}
        // (send.py:322-337). That dict MUST survive verbatim into CmdResult Json output.
        let r = cmd_send(&send_args_fixture()).expect("cmd_send returns CmdResult");
        let v = match r.output {
            CmdOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        let watch = v
            .get("watch")
            .expect("watch_result:true MUST attach result['watch'] (send.py:326)");
        assert_eq!(
            watch.get("status").and_then(|s| s.as_str()),
            Some("registered"),
            "registered-watcher notice status must be exactly 'registered'"
        );
        assert!(watch.get("watcher_id").is_some(), "watch notice carries watcher_id");
        assert_eq!(
            watch.get("agent_id").and_then(|s| s.as_str()),
            Some("alice"),
            "watch notice agent_id == the send target"
        );
        // non-queued notice golden bytes (send.py:335):
        assert_eq!(
            watch.get("notice").and_then(|s| s.as_str()),
            Some("Team Agent will collect the result and notify the leader when this task reports completion.")
        );
    }

    #[test]
    fn cmd_send_failed_outcome_yields_error_exit() {
        // DeliveryOutcome ok=false (e.g. refused) -> from_json -> ExitCode::Error (parser.py:507).
        // A failed send to a target must propagate non-zero exit reporting through CmdResult.
        let args = SendArgs {
            target: Some("nonexistent".into()),
            no_ack: false,
            no_wait: false,
            watch_result: false,
            ..send_args_fixture()
        };
        let r = cmd_send(&args).expect("cmd_send returns CmdResult even on delivery failure");
        if let CmdOutput::Json(ref v) = r.output {
            if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
                assert_eq!(
                    r.exit,
                    ExitCode::Error,
                    "ok:false DeliveryOutcome MUST derive ExitCode::Error (non-zero exit)"
                );
            }
        }
    }


// ═══════════════════════════════════════════════════════════════════════════
// coordinator.ok — non-compact status carries the FULL coordinator_health (incl. `ok`); compact
// strips to {status,pid,metadata_ok,schema_ok} (golden queries.py:77 + compact.py:35; ok =
// running∧metadata_ok∧schema_ok, coordinator/lifecycle.py:26-46). Deterministic missing-coordinator
// fixture (no pid → ok:false, status:"missing"). Dual-assertion catches both branch directions.
// ═══════════════════════════════════════════════════════════════════════════
#[test]
fn status_noncompact_coordinator_includes_ok() {
    let ws = seed_status_workspace();
    let v = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status");
    let coord = v.get("coordinator").and_then(|c| c.as_object()).expect("coordinator object");
    assert!(
        coord.contains_key("ok"),
        "non-compact coordinator MUST carry `ok` (golden queries.py:77 full coordinator_health); got keys {:?}",
        coord.keys().collect::<Vec<_>>()
    );
    for key in ["ok", "status", "pid", "metadata", "metadata_ok", "schema_ok"] {
        assert!(coord.contains_key(key), "non-compact coordinator missing `{key}`");
    }
    assert_eq!(
        coord.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "missing-coordinator fixture → ok:false (running∧metadata_ok∧schema_ok)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_compact_coordinator_omits_ok() {
    let ws = seed_status_workspace();
    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");
    let coord = v.get("coordinator").and_then(|c| c.as_object()).expect("coordinator object");
    let keys: std::collections::BTreeSet<&str> = coord.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> =
        ["metadata_ok", "pid", "schema_ok", "status"].into_iter().collect();
    assert_eq!(
        keys, expected,
        "compact coordinator key set must be EXACTLY {{metadata_ok,pid,schema_ok,status}} (no ok/metadata)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════════════════════════
// P0 (b) — CLI `send --task <unknown>` (route_task_id=true default = routing) MUST surface the
// golden error envelope {ok:false, error:"unknown task id:<id>", action, log} + exit 1 — NOT a
// silent 0-byte swallow (rt-host-b), and NO "validation:" prefix. Lock.
// ═══════════════════════════════════════════════════════════════════════════
// OLD seed: flat `{session_name, agents:{w1}, tasks:[]}`.
// NEW seed (Bug 1/2 — team-in-team state scope, see tests/team_in_team_state_scope_red.rs):
//   cmd_send projects state through the active team_key before reaching the unknown-task
//   gate, so agents/tasks must live under `teams[<key>].*` to be visible at projection
//   time. The "unknown task -> golden envelope" behavior being asserted is unchanged.
#[test]
fn cmd_send_unknown_task_surfaces_golden_error_envelope_not_silent() {
    let ws = std::env::temp_dir().join(format!(
        "ta-cli-sendunk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec_pretty(&json!({
            "session_name": "team-x",
            "active_team_key": "current",
            "teams": {"current": {
                "session_name": "team-x",
                "agents": { "w1": { "status": "running" } },
                "tasks": []
            }}
        })).unwrap(),
    ).unwrap();
    let _ = crate::message_store::MessageStore::open(&ws);
    let args = SendArgs {
        target: Some("w1".into()),
        targets: None,
        task: Some("t-unknown".into()),
        message: vec!["go".into()],
        workspace: ws.clone(),
        team: None,
        watch_result: false,
        json: true,
        ..send_args_fixture()
    };
    // route_task_id defaults true (CLI routing path) → the error MUST surface, not silently swallow.
    let err = cmd_send(&args).expect_err(
        "CLI send --task <unknown> must surface an error (route_task_id=true routing), not a silent 0-byte send"
    );
    let payload = err.to_payload(std::path::Path::new("/tmp/ta-cli-err.log"), "send");
    assert!(!payload.ok, "error envelope ok must be false");
    assert_eq!(
        payload.error, "unknown task id: t-unknown",
        "CLI error field == golden bare message (golden runtime.py:1032 str(exc)); NO 'validation:' prefix"
    );
    assert_eq!(payload.action, "run `team-agent doctor` or inspect the log path shown here");
    let _ = std::fs::remove_dir_all(&ws);
}

// P0 (b') — the SWALLOW guard: `run()` (the CLI process entry) MUST RENDER the send error, not
// discard Err(CliError) via unwrap_or (advisor %7 root cause). Proxy: emit_cli_error WRITES a
// `.team/logs/cli-error-*.log` (and prints the compact envelope) — if run() swallowed, neither
// happens. So a cli-error log containing the BARE "unknown task id: <id>" (no "validation:" prefix)
// + ExitCode::Error proves run() rendered. Drives the real argv→(exit,render) path.
// OLD/NEW: same Bug 1/2 seed sync as cmd_send_unknown_task_*; the render-vs-swallow
// behavior under test is unchanged.
#[test]
fn run_send_unknown_task_renders_error_not_silent_swallow() {
    let ws = std::env::temp_dir().join(format!(
        "ta-run-sendunk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec_pretty(&json!({
            "session_name": "team-x",
            "active_team_key": "current",
            "teams": {"current": {
                "session_name": "team-x",
                "agents": { "w1": { "status": "running" } },
                "tasks": []
            }}
        })).unwrap(),
    ).unwrap();
    let _ = crate::message_store::MessageStore::open(&ws);
    let argv: Vec<String> = ["send", "w1", "--task", "t-unknown", "go", "--json"]
        .iter().map(ToString::to_string).collect();
    let code = run(&argv, &ws);
    assert_eq!(code, ExitCode::Error, "run(send --task <unknown>) must exit Error, not Ok");
    // run() must have RENDERED (emit_cli_error wrote the cli-error log); a swallow leaves none.
    let logs_dir = ws.join(".team").join("logs");
    let mut found = String::new();
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("cli-error-") {
                found = std::fs::read_to_string(entry.path()).unwrap_or_default();
                break;
            }
        }
    }
    assert!(
        found.contains("unknown task id: t-unknown"),
        "run() must RENDER the send error (cli-error log written with the bare message) — a silent \
         swallow (unwrap_or discards Err) leaves no log. got log body: {found:?}"
    );
    assert!(
        !found.contains("validation:"),
        "rendered error must be the bare golden message, NO 'validation:' prefix; got {found:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// R8 D6 (c-lite offline byte-lock): the CLI requeued_exhausted_watchers return projection, extracted
// into a pure helper, must project the golden event's watcher_ids STRING list (leader/__init__.py:56) —
// NOT the Rust `requeued` Vec<WatcherNotice> objects.
#[test]
fn r8_project_requeued_exhausted_watchers_golden_string_list() {
    // golden attach event shape (what D4 emits): {watcher_ids:[str], count, trigger}.
    let golden_event = serde_json::json!({"watcher_ids": ["w1", "w2"], "count": 2, "trigger": "attach_leader"});
    let projected = crate::cli::leader_port::project_requeued_exhausted_watchers(&golden_event);
    let list = projected.as_array().expect("requeued_exhausted_watchers must be a JSON array");
    let ids: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(ids, vec!["w1", "w2"],
        "D6: CLI requeued_exhausted_watchers must project the golden watcher_ids STRING list \
         (leader/__init__.py:56), not the `requeued` Vec<WatcherNotice> objects; got {projected:?}");
}
