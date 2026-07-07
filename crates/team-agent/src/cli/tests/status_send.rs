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
            activity.pointer("/status").and_then(serde_json::Value::as_str),
            Some("working"),
            "compact activity.status must survive the projection"
        );
        assert_eq!(
            activity.pointer("/rationale").and_then(serde_json::Value::as_str),
            Some("provider_jsonl:open_turn"),
            "compact activity.rationale must survive the projection"
        );
        // last_output_at is the timestamp the classifier advances when
        // scrollback digest changes; operators read it alongside activity.
        assert_eq!(
            agent.get("last_output_at").and_then(serde_json::Value::as_str),
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
        let got_activity = agent
            .get("activity")
            .expect("RM-039-STAT-001 second-round: activity must reach the compact projection \
                     even when active_team_key disagrees with team_dir basename");
        assert_eq!(
            got_activity.pointer("/status").and_then(serde_json::Value::as_str),
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
        // 0.4.x compact slim: exactly 7 top-level fields; diagnostics moved
        // to --detail. Plan: .team/artifacts/status-compact-plan.md.
        let ws = seed_status_workspace();
        let v = status_port::status(&ws, /*compact=*/ true, /*detail=*/ false)
            .expect("seeded fixture status should project a value");
        let obj = v.as_object().expect("--json status is a dict");
        // Exactly these 7 keys, no more.
        let expected: std::collections::BTreeSet<&str> = [
            "ok",
            "team",
            "session_name",
            "leader_attach_command",
            "ready",
            "not_ready",
            "agents",
        ]
        .iter()
        .copied()
        .collect();
        let actual: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            actual, expected,
            "0.4.x compact must expose exactly 7 keys; got {actual:?}"
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
    fn cmd_status_human_appends_harness_reminder() {
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

        assert!(text.ends_with(crate::cli::STATUS_REMINDER), "{text}");
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

        let detail = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status detail");
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
        assert!(attach.contains("attach -t team-current:claude_code"), "{attach}");
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

        let detail = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status detail");
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
        assert_ne!(full, compact, "detail (full) and default (compact) projections must differ");
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
            pane: None,
            to_name: None,
        to_leader: None,
        }
    }

    fn queued_send_args_fixture(json: bool) -> SendArgs {
        let ws = deleg_uniq_dir("send-human");
        let _ = crate::message_store::MessageStore::open(&ws).unwrap();
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "active_team_key": "current",
                "teams": {"current": {"agents": {"w1": {"provider": "codex"}}}}
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
        let args = SendArgs {
            json: true,
            ..send_args_fixture()
        };
        let r = cmd_send(&args).expect("cmd_send returns CmdResult");
        // The delegate's DeliveryOutcome -> Json must carry an `ok` key feeding exit-code.
        match r.output {
            CmdOutput::Json(ref v) => {
                assert!(v.get("ok").is_some(), "send result Json must carry `ok`");
                if v.get("ok").and_then(|ok| ok.as_bool()) == Some(true) {
                    assert_eq!(
                        v.get("reminder").and_then(|reminder| reminder.as_str()),
                        Some(crate::cli::SEND_REMINDER)
                    );
                }
            }
            other => panic!("cmd_send must emit Json DeliveryOutcome, got {other:?}"),
        }
    }

    #[test]
    fn cmd_send_default_human_output_is_one_line_without_false_delivered() {
        let r = cmd_send(&queued_send_args_fixture(false)).expect("cmd_send returns CmdResult");
        assert!(!r.as_json);
        let text = emit(&r.output, r.as_json).expect("send should render human text");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "default send output must be one line: {text}");
        assert!(
            lines[0].contains("ok:")
                && lines[0].contains("status:")
                && lines[0].contains("message_id:")
                && lines[0].contains("target:"),
            "default send output must keep only the core fields; got {text}"
        );
        assert!(
            !text.contains("delivered"),
            "queued send output must not claim or mention delivered; got {text}"
        );
        for hidden in [
            "agent_id:",
            "sender:",
            "message_status:",
            "verification:",
            "stage:",
            "reason:",
            "channel:",
            "reminder:",
        ] {
            assert!(
                !text.contains(hidden),
                "default send output should hide {hidden} unless needed; got {text}"
            );
        }
    }

    #[test]
    fn cmd_send_json_shape_keeps_056_fields() {
        let args = queued_send_args_fixture(true);
        let r = cmd_send(&args).expect("cmd_send returns CmdResult");
        let v = match r.output {
            CmdOutput::Json(v) => v,
            other => panic!("--json send must emit Json, got {other:?}"),
        };
        let obj = v.as_object().expect("--json send output must be object");
        for key in [
            "ok",
            "status",
            "delivery_status",
            "delivered",
            "target",
            "agent_id",
            "content_length_bytes",
            "sender",
            "message_id",
            "message_status",
            "verification",
            "stage",
            "reason",
            "channel",
            "reminder",
        ] {
            assert!(obj.contains_key(key), "--json send shape lost {key}: {v}");
        }
        assert_eq!(v.get("verification"), Some(&serde_json::Value::Null));
        assert_eq!(v.get("stage"), Some(&serde_json::Value::Null));
        assert_eq!(v.get("reason"), Some(&serde_json::Value::Null));
        assert_eq!(v.get("channel"), Some(&serde_json::Value::Null));
        assert_eq!(v.get("delivered").and_then(|d| d.as_bool()), Some(false));
        assert!(
            !v.get("reminder")
                .and_then(|reminder| reminder.as_str())
                .unwrap_or_default()
                .contains("Message delivered."),
            "queued JSON reminder must not contradict delivered:false: {v}"
        );
    }

    #[test]
    fn cmd_send_watch_result_does_not_register_before_delivery() {
        // 0.5.x send contract: --watch-result may only advertise a watcher after
        // initial worker delivery is physically proven.
        let args = SendArgs {
            json: true,
            ..send_args_fixture()
        };
        let r = cmd_send(&args).expect("cmd_send returns CmdResult");
        let v = match r.output {
            CmdOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(
            v.get("delivery_status").and_then(|s| s.as_str()).is_some(),
            "send output must expose delivery_status; got {v}"
        );
        assert_eq!(
            v.get("delivered").and_then(|s| s.as_bool()),
            Some(false),
            "undelivered send outcome must not look delivered"
        );
        assert!(
            !v.as_object().unwrap().contains_key("watch"),
            "watch_result:true must not attach result['watch'] before delivery; got {v}"
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
    let golden_event = serde_json::json!({"watcher_ids": ["w1", "w2"], "count": 2, "trigger": "attach_leader"});
    let projected = crate::cli::leader_port::project_requeued_exhausted_watchers(&golden_event);
    let list = projected.as_array().expect("requeued_exhausted_watchers must be a JSON array");
    let ids: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(ids, vec!["w1", "w2"],
        "D6: CLI requeued_exhausted_watchers must project the golden watcher_ids STRING list \
         (leader/__init__.py:56), not the `requeued` Vec<WatcherNotice> objects; got {projected:?}");
}
