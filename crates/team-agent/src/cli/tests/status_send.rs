use super::*;

// =========================================================================
// status_compact_flag (commands.py:99): compact = !detail — CLI 独占不变量
// gate: 'detail=false => compact=true mapping, the one byte-level invariant CLI owns'.
// =========================================================================

#[test]
fn status_compact_flag_default_is_compact() {
    // golden: cmd_status without --detail -> runtime.status(compact=not(False)) == compact=True.
    assert!(
        status_compact_flag(false),
        "detail=false MUST map to compact=true (commands.py:99)"
    );
}

#[test]
fn status_compact_flag_detail_is_full() {
    // golden: cmd_status --detail -> runtime.status(compact=not(True)) == compact=False.
    assert!(
        !status_compact_flag(true),
        "detail=true MUST map to compact=false (full projection)"
    );
}

// =========================================================================
// status_port::status — REAL caller against SEEDED fixture (gate: 'zero callers').
// Asserts the --json projection shape that the compact-vs-detail wiring selects.
// RED: status_port::status is unimplemented!() so the call panics until ported.
// =========================================================================

// =========================================================================
// RM-039-STAT-001 regression guard (real-machine evidence 2026-06-22).
//
// Architect verdict (bugs-stat001-sess001-architecture-analysis.md §root-cause):
// the coordinator-tick activity classifier writes
// `activity {status, confidence, rationale}` to the top-level
// `agents.<id>` slot of state.json (T1 invariant 60/61); the compact
// `status --json` projection MUST preserve it, last_output_at, and
// the enrich_agents-injected `interacted` marker. Lifecycle `status`
// and turn `activity.status` are separate fields per T1; the
// projection MUST NOT collapse them.
// =========================================================================
#[test]
fn rm039_stat001_compact_status_preserves_activity_and_last_output() {
    let ws = seed_status_workspace();
    let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
    if let Some(agents) = state
        .pointer_mut("/agents")
        .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(agent) = agents
            .get_mut("a1")
            .and_then(serde_json::Value::as_object_mut)
        {
            agent.insert(
                "activity".to_string(),
                json!({
                    "status": "working",
                    "confidence": 0.95,
                    "rationale": "provider_jsonl:open_turn",
                }),
            );
            agent.insert(
                "last_output_at".to_string(),
                json!("2026-06-22T02:52:30+00:00"),
            );
            // first_send_at is already set by seed_status_workspace so
            // enrich_agents will inject `interacted` with the same ISO value.
        }
    }
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();

    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
        .expect("compact status should project a value");
    let agent = v
        .pointer("/agents/a1")
        .and_then(serde_json::Value::as_object)
        .expect("seeded agent a1 must appear in compact projection");

    // T1 split: lifecycle `status` stays unchanged.
    assert_eq!(
        agent.get("status").and_then(serde_json::Value::as_str),
        Some("running"),
        "RM-039-STAT-001: compact projection must NOT collapse `status` into `activity.status`"
    );
    // Turn activity preserved (the field that was historically dropped).
    let activity = agent
        .get("activity")
        .expect("RM-039-STAT-001: compact projection must preserve `activity`");
    assert_eq!(
        activity
            .pointer("/status")
            .and_then(serde_json::Value::as_str),
        Some("working"),
        "compact activity.status must survive the projection"
    );
    assert_eq!(
        activity
            .pointer("/rationale")
            .and_then(serde_json::Value::as_str),
        Some("provider_jsonl:open_turn"),
        "compact activity.rationale must survive the projection"
    );
    // last_output_at is the timestamp the classifier advances when
    // scrollback digest changes; operators read it alongside activity.
    assert_eq!(
        agent
            .get("last_output_at")
            .and_then(serde_json::Value::as_str),
        Some("2026-06-22T02:52:30+00:00"),
        "compact projection must preserve `last_output_at` for the \"is something moving\" view"
    );
    // 0.4.x compact slim: `interacted` moves to --detail; the 4-field
    // compact agent row keeps only status/provider/activity/last_output_at.
    assert!(
        agent.get("interacted").is_none(),
        "0.4.x: compact projection drops `interacted` (moved to --detail)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// =========================================================================
// RM-039-STAT-001 second-round regression (architect verdict 2026-06-22).
//
// Real failure shape: tick wrote `activity` to root .agents.coder and
// .teams.current.agents.coder because `team_state_key` cascaded to the
// `team_dir = "./.team/current"` basename. But status's selector reads
// the `active_team_key = "rm039-status-working-891"` projection, which
// was stale. The compact whitelist alone cannot fix this — by the time
// compact_agent_state runs, the selected team slot already lacks activity.
//
// Fix expected at the projection/state-key layer: when load_runtime_state
// sees `active_team_key` naming an existing teams entry and no root
// `team_key`, it must promote `team_key = active_team_key` so subsequent
// tick writes and status reads agree on which teams entry to use. The
// assertion here drives the real CLI path through `status_port::status`.
// =========================================================================
#[test]
fn rm039_stat001_status_resolves_active_team_when_root_team_key_missing() {
    let ws = seed_status_workspace();
    // Seed the exact dirty shape from the evidence: active_team_key
    // disagrees with team_dir basename; root team_key absent; the
    // teams.<active> entry is stale (no activity); root + teams.current
    // carry activity that the tick had already written there.
    let active = "rm039-status-working-891";
    let activity = json!({
        "status": "working",
        "confidence": 0.95,
        "rationale": "provider_jsonl:open_turn",
    });
    let state = json!({
        "session_name": "team-rm039-status-working",
        "team_dir": "./.team/current",
        "active_team_key": active,
        // intentionally no top-level "team_key" — that is the bug shape.
        "leader": {"id": "leader"},
        "leader_receiver": {"pane_id": "%3", "status": "running"},
        "agents": {
            "coder": {
                "status": "running",
                "first_send_at": "2026-01-01T00:00:00Z",
                "activity": activity.clone(),
                "last_output_at": "2026-06-22T02:52:30+00:00",
            }
        },
        "teams": {
            "current": {
                "active_team_key": active,
                "session_name": "team-rm039-status-working",
                "agents": {
                    "coder": {
                        "status": "running",
                        "first_send_at": "2026-01-01T00:00:00Z",
                        "activity": activity.clone(),
                    }
                }
            },
            active: {
                "active_team_key": active,
                "session_name": "team-rm039-status-working",
                "agents": {
                    "coder": {
                        "status": "running",
                        "first_send_at": "2026-01-01T00:00:00Z",
                        // NO `activity` here — this is the stale entry
                        // that the selector landed on pre-fix.
                    }
                }
            }
        }
    });
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
        .expect("compact status should project a value");
    let agent = v
        .pointer("/agents/coder")
        .and_then(serde_json::Value::as_object)
        .expect("coder agent must appear in compact projection");
    let got_activity = agent.get("activity").expect(
        "RM-039-STAT-001 second-round: activity must reach the compact projection \
                     even when active_team_key disagrees with team_dir basename",
    );
    assert_eq!(
        got_activity
            .pointer("/status")
            .and_then(serde_json::Value::as_str),
        Some("working"),
        "RM-039-STAT-001 second-round: compact status.agents.coder.activity.status must \
             be `working` when state.active_team_key names an existing teams entry, \
             regardless of team_dir basename"
    );
    // Lifecycle status unchanged — T1 split invariant.
    assert_eq!(
        agent.get("status").and_then(serde_json::Value::as_str),
        Some("running"),
        "lifecycle status must NOT collapse into activity.status"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_port_status_compact_json_shape_against_seeded_fixture() {
    // cmd_status json branch (detail=false) delegates status_port::status(compact=true).
    // Compact slim is the original 7 keys plus `grok_slot` and
    // `grok_identity_slot_ok`. `ok` is command success (always true here).
    let ws = seed_status_workspace();
    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
        .expect("seeded fixture status should project a value");
    let obj = v.as_object().expect("--json status is a dict");
    let expected: std::collections::BTreeSet<&str> = [
        "ok",
        "team",
        "session_name",
        "leader_attach_command",
        "ready",
        "not_ready",
        "agents",
        "grok_slot",
        "grok_identity_slot_ok",
    ]
    .iter()
    .copied()
    .collect();
    let actual: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        actual, expected,
        "compact must expose the original 7 keys plus grok_slot and grok_identity_slot_ok (9); got {actual:?}"
    );
    let grok_slot = obj
        .get("grok_slot")
        .and_then(serde_json::Value::as_object)
        .expect("compact must carry grok_slot");
    let grok_ok = grok_slot
        .get("consistent")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && grok_slot
            .get("readable")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    assert_eq!(
        obj.get("ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "compact ok is command success and must stay true; grok_slot={grok_slot:?}"
    );
    assert_eq!(
        obj.get("grok_identity_slot_ok")
            .and_then(serde_json::Value::as_bool),
        Some(grok_ok),
        "grok_identity_slot_ok must follow readable∧consistent; grok_slot={grok_slot:?}"
    );
    // Diagnostic keys must NOT leak into the default compact payload.
    for forbidden in [
        "leader_topology",
        "is_external_leader",
        "leader_client",
        "tmux_session_present",
        "leader_receiver",
        "agent_health",
        "tasks",
        "messages",
        "queued_messages",
        "results",
        "latest_results",
        "coordinator",
        "readiness",
        "reminder",
        "last_events",
    ] {
        assert!(
            !obj.contains_key(forbidden),
            "0.4.x: compact must NOT contain diagnostic key `{forbidden}` (--detail only)"
        );
    }
    // seeded agent surfaces through the projection.
    assert!(
        obj["agents"].as_object().unwrap().contains_key("a1"),
        "seeded agent a1 must appear in compact agents projection"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_compact_reports_identity_slot_health_on_grok_identity_slot_ok() {
    // 已废除的行为：旧实现看不见 grok 身份槽。此墓碑留下槽健康断言。
    // 2026-08-18 裁定：`ok` 是命令成功标志，成功投影必须恒真；身份槽健康
    // 走顶层 `grok_identity_slot_ok`，不再重载 `ok`。
    // 2026-08-18 身份串台：judge-g 的产出从 developer-d61 的通道报出并占掉
    // 后者 exactly-once report_result。那时若完全不报槽健康才是一句谎。
    let ws = seed_status_workspace();
    let grok = ws.join(".grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(
        grok.join("config.toml"),
        "[mcp_servers.team_orchestrator.env]\nTEAM_AGENT_ID = \"impostor-seat\"\n",
    )
    .unwrap();

    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
        .expect("mismatched grok slot must still project compact status");
    assert_eq!(
        v.get("ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "successful status must keep ok=true; slot health is a sibling key; got {v}"
    );
    assert_eq!(
        v.get("grok_identity_slot_ok")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "identity-slot mismatch must force grok_identity_slot_ok=false; got {v}"
    );
    assert_eq!(
        v.pointer("/grok_slot/consistent")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "fixture must actually be a slot mismatch; got {v}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn cmd_status_human_is_nine_field_brief_without_diagnostics() {
    let ws = seed_status_workspace();
    let args = StatusArgs {
        agent: None,
        workspace: ws.clone(),
        detail: false,
        summary: false,
        json: false,
        team: None,
    };

    let r = cmd_status_for_team(&args, None).expect("status");
    let text = match r.output {
        CmdOutput::Human(text) => text,
        other => panic!("expected human status output, got {other:?}"),
    };

    assert!(
        text.contains(
            "name: a1 provider: unknown model: null effort: null runtime_status: unknown activity: unknown health: unknown session_name: null tmux_command: null"
        ),
        "{text}"
    );
    assert!(!text.contains(crate::cli::STATUS_REMINDER));
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

    // 0.4.x: leader_topology / is_external_leader moved to --detail.
    // leader_attach_command stays in the slim compact payload.
    let slim = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");
    let attach = slim["leader_attach_command"]
        .as_str()
        .expect("compact still includes leader_attach_command");
    assert!(attach.contains("attach -t team-current:leader"), "{attach}");

    let detail =
        status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status detail");
    assert_eq!(detail["leader_topology"], json!("managed"));
    assert_eq!(detail["is_external_leader"], json!(false));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_port_managed_attach_command_uses_receiver_window_name() {
    let ws = seed_status_workspace();
    let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
    if let Some(obj) = state.as_object_mut() {
        obj.insert("is_external_leader".to_string(), json!(false));
        obj.insert("session_name".to_string(), json!("team-current"));
        obj.insert(
            "leader_receiver".to_string(),
            json!({"pane_id": "%3", "window_name": "claude_code", "status": "attached"}),
        );
    }
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();

    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");

    let attach = v["leader_attach_command"]
        .as_str()
        .expect("managed status includes attach command");
    assert!(
        attach.contains("attach -t team-current:claude_code"),
        "{attach}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn leader_attach_command_for_plan_uses_plan_leader_window() {
    let ws = tmp_workspace();
    let plan = crate::leader::LeaderStartPlan {
        mode: crate::leader::LeaderStartMode::ManagedTmuxClient,
        provider: crate::provider::Provider::ClaudeCode,
        workspace: ws.clone(),
        socket: crate::leader::LeaderLaunchSocket::Workspace,
        session_name: Some(crate::transport::SessionName::new(
            "team-agent-leader-claude_code-demo".to_string(),
        )),
        argv: Vec::new(),
        provider_argv: Vec::new(),
        leader_prompt: None,
        session_capture: None,
        leader_window: Some(crate::transport::WindowName::new("claude_code")),
        is_external_leader: false,
        leader_env: std::collections::BTreeMap::new(),
        identity: None,
        detached: false,
    };

    let attach = lifecycle_port::leader_attach_command_for_plan(&ws, &plan)
        .expect("managed plan has attach command");

    assert!(
        attach.contains("attach -t team-agent-leader-claude_code-demo:claude_code"),
        "{attach}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_port_missing_topology_marker_defaults_to_managed() {
    let ws = seed_status_workspace();
    let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
    if let Some(obj) = state.as_object_mut() {
        obj.remove("is_external_leader");
        obj.insert("session_name".to_string(), json!("team-current"));
    }
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();

    // 0.4.x: topology/external markers moved to --detail; compact keeps attach.
    let slim = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");
    let attach = slim["leader_attach_command"]
        .as_str()
        .expect("missing marker defaults to managed attach command");
    assert!(attach.contains("attach -t team-current:leader"), "{attach}");

    let detail =
        status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status detail");
    assert_eq!(detail["leader_topology"], json!("managed"));
    assert_eq!(detail["is_external_leader"], json!(false));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_port_status_detail_full_keeps_uncompacted_events() {
    // 0.4.x: --detail (compact=false) preserves ALL diagnostic fields the
    // compact slim payload drops. Pin the must-keep set so future
    // refactors can't accidentally strip detail diagnostics.
    let ws = seed_status_workspace();
    let full = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true)
        .expect("seeded fixture full status should project a value");
    let compact = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
        .expect("seeded fixture compact status should project a value");
    assert_ne!(
        full, compact,
        "detail (full) and default (compact) projections must differ"
    );
    let full_obj = full.as_object().expect("full status is a dict");
    for key in [
        "coordinator",
        "readiness",
        "leader_receiver",
        "agent_health",
        "tasks",
        "messages",
        "queued_messages",
        "results",
        "latest_results",
        "last_events",
    ] {
        assert!(
            full_obj.contains_key(key),
            "0.4.x: --detail must preserve `{key}` (compact slimming escape hatch)"
        );
    }
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
    let workspace = seed_send_workspace();
    // M4: no SQLite message store fixture (MessageStore::open is retired).
    SendArgs {
        target: Some("alice".into()),
        message: vec!["hello".into(), "world".into(), "foo".into()],
        targets: None,
        workspace,
        team: Some("teamA".into()),
        task: Some("t-1".into()),
        sender: TrustedSender::leader(),
        no_ack: true,
        no_wait: true,
        watch_result: true,
        timeout: 12.5,
        confirm_human: false,
        json: false,
        message_id: None,
        presentation: crate::messaging::presentation::PresentationRequest { sink: crate::messaging::presentation::PresentationSink::Silent, ..Default::default() },
        pane: None,
        to_name: None,
        to_leader: None,
    }
}

fn queued_send_args_fixture(json: bool) -> SendArgs {
    let ws = deleg_uniq_dir("send-human");
    // M4: no SQLite message store fixture (MessageStore::open is retired).
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "current",
            "teams": {"current": {"agents": {"w1": {"provider": "copilot", "status": "running"}}}}
        }),
    )
    .unwrap();
    SendArgs {
        workspace: ws,
        target: Some("w1".into()),
        team: None,
        task: None,
        watch_result: false,
        json,
        ..send_args_fixture()
    }
}

#[test]
fn send_options_negates_no_ack_and_no_wait_and_carries_watch() {
    // golden (commands.py:172,174,176): requires_ack=not no_ack; wait_visible=not no_wait;
    //   watch_result passthrough. With no_ack=true,no_wait=true,watch_result=true:
    //   requires_ack=false, wait_visible=false, watch_result=true.
    let opts = send_options_from_args(&send_args_fixture());
    assert!(
        !opts.requires_ack,
        "no_ack:true MUST map to requires_ack:false (off-by-inversion guard)"
    );
    assert!(
        !opts.wait_visible,
        "no_wait:true MUST map to wait_visible:false"
    );
    assert!(
        opts.watch_result,
        "watch_result flag MUST pass through into SendOptions"
    );
    assert!(!opts.confirm_human);
    assert_eq!(opts.sender.as_str(), "leader");
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
    assert!(
        opts.requires_ack,
        "no_ack:false MUST map to requires_ack:true"
    );
    assert!(
        opts.wait_visible,
        "no_wait:false MUST map to wait_visible:true"
    );
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
fn cmd_send_mailbox_output_names_storage_without_claiming_delivery() {
    let args = queued_send_args_fixture(false);
    let result = cmd_send(&args).expect("mailbox append without a live pane");
    let text = emit(&result.output, result.as_json).unwrap();
    assert!(text.contains("stored") && text.contains("w1"), "{text}");
    assert!(!text.contains("Delivered") && !text.contains("parked") && !text.contains("still pending"), "{text}");
}

#[test]
#[cfg(unix)]
fn issue372_send_never_reports_a_handshake_parking_gate() {
    let args = queued_send_args_fixture(true);
    let result = cmd_send(&args).unwrap();
    let CmdOutput::Json(value) = result.output else { panic!("JSON result expected") };
    assert_eq!(value["status"], "stored");
    assert!(value.get("deferred_reason").is_none());
    let direct = SendArgs { presentation: crate::messaging::presentation::PresentationRequest::default(), ..args };
    // M1 stores first; a missing pane is reported on the receipt, not as an error.
    let CmdOutput::Json(direct) = cmd_send(&direct).unwrap().output else { panic!("JSON result expected") };
    let text = direct.to_string();
    assert_eq!(direct["submission"], "not_attempted", "{text}");
    assert!(direct["delivery_error"].as_str().is_some_and(|e| e.contains("no tmux pane")), "{text}");
    assert!(!text.contains("mcp_handshake") && !text.contains("parked"), "{text}");
}

#[test]
fn cmd_send_json_shape_describes_the_decoupled_operation() {
    let args = queued_send_args_fixture(true);
    let result = cmd_send(&args).unwrap();
    let CmdOutput::Json(value) = result.output else { panic!("JSON result expected") };
    let inbox = crate::inbox_store::read(&args.workspace, "w1", 10, None).unwrap();
    assert_eq!(inbox["messages"][0]["message_id"], value["message_id"]);
    // M1 receipt: durable storage facts only; a mailbox send never attempts submission.
    let mut keys = value.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["accepted", "anonymous_dedupe_window", "dedupe_key", "duplicate", "live", "message_id", "ok", "recipient", "record_ref", "status", "stored"], "{value}");
    assert_eq!((value["ok"].clone(), value["status"].clone(), value["stored"].clone(), value["live"].clone(), value["recipient"].clone()),
        (json!(true), json!("stored"), json!(true), json!(false), json!("w1")), "{value}");
    for key in ["delivery_attempts", "message_status", "verification", "watch", "deferred_reason", "submission"] {
        assert!(value.get(key).is_none(), "no coordinator delivery fiction: {value}");
    }
}


#[test]
fn cmd_send_unknown_recipient_is_an_immediate_error_without_queueing() {
    let args = SendArgs { target: Some("absent-worker".into()), json: true, ..send_args_fixture() };
    let refusal = cmd_send(&args).unwrap();
    assert_eq!(refusal.exit, ExitCode::Error);
    let CmdOutput::Json(value) = refusal.output else { panic!("JSON refusal expected") };
    assert_eq!(value["ok"], false);
    assert!(value.to_string().contains("absent-worker"), "{value}");
    assert!(crate::inbox_store::read(&args.workspace, "absent-worker", 3, None).unwrap()["messages"].as_array().unwrap().is_empty());
}

#[test]
fn cmd_send_canonicalizes_legacy_session_alias_before_membership_projection() {
    let ws = deleg_uniq_dir("send-team-alias");
    // M4: no SQLite message store fixture (MessageStore::open is retired).
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "teamdir",
            "team_key": "teamdir",
            "teams": {
                "teamdir": {
                    "status": "alive",
                    "session_name": "team-displayname",
                    "agents": {"w1": {"provider": "copilot", "status": "running"}}
                }
            }
        }),
    )
    .unwrap();
    let args = SendArgs {
        workspace: ws,
        target: Some("w1".into()),
        team: Some("displayname".into()),
        task: None,
        no_wait: true,
        watch_result: false,
        json: true,
        ..send_args_fixture()
    };

    let result = cmd_send(&args).expect("alias selection should reach canonical team send");
    let value = match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON send outcome, got {other:?}"),
    };
    assert_ne!(
        value.get("reason").and_then(serde_json::Value::as_str),
        Some("target_not_in_team"),
        "selector accepted the alias, so membership must use its canonical team key: {value}"
    );
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
    let coord = v
        .get("coordinator")
        .and_then(|c| c.as_object())
        .expect("coordinator object");
    assert!(
        coord.contains_key("ok"),
        "non-compact coordinator MUST carry `ok` (golden queries.py:77 full coordinator_health); got keys {:?}",
        coord.keys().collect::<Vec<_>>()
    );
    for key in [
        "ok",
        "status",
        "pid",
        "metadata",
        "metadata_ok",
        "schema_ok",
    ] {
        assert!(
            coord.contains_key(key),
            "non-compact coordinator missing `{key}`"
        );
    }
    assert_eq!(
        coord.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "missing-coordinator fixture → ok:false (running∧metadata_ok∧schema_ok)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn status_compact_omits_coordinator_entirely() {
    // 0.4.x compact slim: the entire `coordinator` block moved to --detail.
    // Compact folds coordinator health into the top-level `ready`/`not_ready`
    // synthesis (`coordinator_not_running` / `coordinator_schema_not_ok`).
    let ws = seed_status_workspace();
    let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false).expect("status");
    assert!(
        v.as_object().unwrap().get("coordinator").is_none(),
        "0.4.x: compact must NOT include `coordinator`; reasons fold into not_ready"
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
fn cmd_send_legacy_task_flag_does_not_require_a_coordinator_task() {
    let args = SendArgs { task: Some("t-unknown".into()), ..send_args_fixture() };
    let result = cmd_send(&args).expect("legacy task metadata cannot gate physical/inbox delivery");
    assert_eq!(result.exit, ExitCode::Ok);
    let inbox = crate::inbox_store::read(&args.workspace, "alice", 3, None).unwrap();
    assert_eq!(inbox["messages"][0]["content"], "hello world foo");
    // M1: the task id is an advisory label kept for result attribution (red_m1_r12).
    assert_eq!(inbox["messages"][0]["task_id"], "t-unknown");
}


#[test]
fn run_leader_passthrough_flag_after_dashdash_renders_error_not_silent_swallow() {
    let ws = std::env::temp_dir().join(format!(
        "ta-run-leaderflag-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&ws).unwrap();
    let argv: Vec<String> = ["codex", "--", "--external-leader"]
        .iter()
        .map(ToString::to_string)
        .collect();

    let code = run(&argv, &ws);

    assert_eq!(
        code,
        ExitCode::Error,
        "misplaced leader flag must fail before provider exec"
    );
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
        found.contains("Team Agent launcher flag --external-leader must appear before --"),
        "leader passthrough run() must render the misplaced flag error; a silent swallow leaves no \
         cli-error log. got log body: {found:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// R8 D6 (c-lite offline byte-lock): the CLI requeued_exhausted_watchers return projection, extracted
// into a pure helper, must project the golden event's watcher_ids STRING list (leader/__init__.py:56) —
// NOT the Rust `requeued` Vec<WatcherNotice> objects.
#[test]
fn r8_project_requeued_exhausted_watchers_golden_string_list() {
    // golden attach event shape (what D4 emits): {watcher_ids:[str], count, trigger}.
    let golden_event =
        serde_json::json!({"watcher_ids": ["w1", "w2"], "count": 2, "trigger": "attach_leader"});
    let projected = crate::cli::leader_port::project_requeued_exhausted_watchers(&golden_event);
    let list = projected
        .as_array()
        .expect("requeued_exhausted_watchers must be a JSON array");
    let ids: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        ids,
        vec!["w1", "w2"],
        "D6: CLI requeued_exhausted_watchers must project the golden watcher_ids STRING list \
         (leader/__init__.py:56), not the `requeued` Vec<WatcherNotice> objects; got {projected:?}"
    );
}
