    #[test]
    fn dispatch_send_message_worker_accepted_returned_verbatim() {
        // A-7: accepted requires a REAL stored message_id (no fabricated ids), so the
        // workspace seeds a running worker-1 the delivery layer can actually queue for.
        let ws = unique_ws("dispatch-accepted");
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "session_name": "team-x",
                "agents": {
                    "worker-1": {"status": "running", "agent_id": "worker-1", "window": "worker-1"},
                },
            }),
        )
        .unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("leader")), // legacy single-team bypasses cross-team refusal
            None,
        );
        let ok = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to": "worker-1", "content": "do it"
        })).expect("send ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(keys(&v), vec!["status", "delivery_pending", "poll_via", "message_id"],
            "worker-accepted dict returned verbatim (NOT re-compacted)");
        assert_eq!(v.get("status"), Some(&json!("accepted")));
        assert_eq!(v.get("delivery_pending"), Some(&json!(true)));
        let mid = v.get("message_id").and_then(Value::as_str).unwrap();
        assert_eq!(v.get("poll_via"), Some(&json!(format!("team-agent inbox {mid}"))));
    }

    // ── #32/#47 request_human key order ok,message_id,status (no compaction) ────
    // GOLDEN (probe_events_red.py REQUEST_HUMAN-KEYS): ['ok','message_id','status'].
    // Rust compact_tool_result reorders to ok,status,message_id.
    #[test]
    fn request_human_key_order_is_ok_message_id_status() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("reqhuman-order"),
            Some(AgentId::new("worker-3")),
            None,
        );
        let ok = tools.request_human("need approval", Some("task-1"), None).expect("request_human ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(keys(&v), vec!["ok", "message_id", "status"]);
        assert_eq!(v.get("status"), Some(&json!("needs_human")));
    }

    // ── #32 update_state returns RAW {ok, state_file} (NO compaction) ───────────
    // GOLDEN (tools.py:316-325 + probe_passthrough): update_state is NOT compacted;
    // state_file survives. Rust runs compact_tool_result whose ok-whitelist DROPS
    // state_file (not a golden whitelist key), so the key vanishes.
    #[test]
    fn update_state_state_file_survives_no_compaction() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("update-state-raw"),
            Some(AgentId::new("leader")),
            None,
        );
        let ok = tools.update_state("note").expect("update_state ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert!(v.get("state_file").and_then(Value::as_str).is_some(),
            "state_file must survive (update_state is not _compact_tool_result'd)");
        assert_eq!(keys(&v), vec!["ok", "state_file"]);
    }

    // ── #36 report_result setdefault: populated envelope keys WIN over args ─────
    // GOLDEN (probe_setdefault.py): envelope {agent_id:env-agent, task_id:env-task,...}
    // + explicit args agent_id=ARG-agent, task_id=ARG-task → returned dict keeps
    // env-agent / env-task (setdefault). Rust unconditionally insert-overrides.
    #[test]
    fn report_result_setdefault_envelope_wins_over_args() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-setdefault"),
            Some(AgentId::new("env-id")),
            None,
        );
        let ok = tools.report_result(
            Some(&json!({
                "agent_id": "env-agent", "task_id": "env-task",
                "status": "blocked", "summary": "env summary"
            })),
            Some("ARG summary"), ResultStatus::Success,
            None, None, None, None, None,
            Some("ARG-task"), Some("ARG-agent"),
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        // setdefault: the pre-populated envelope values win.
        assert_eq!(v.get("agent_id"), Some(&json!("env-agent")), "envelope agent_id wins (setdefault)");
        assert_eq!(v.get("task_id"), Some(&json!("env-task")), "envelope task_id wins (setdefault)");
    }

    // ── #44 report_result task_id inference from state (_latest_task_for_assignee)
    // GOLDEN (probe_report evidence): env agent worker-7, state tasks=[{id:t-42,
    // assignee:worker-7,status:pending}], report with NO task_id → task_id "t-42".
    // Rust has no _latest_task_for_assignee; hard-codes "manual".
    #[test]
    fn report_result_infers_task_id_from_latest_assigned_task() {
        let cws = seed_state_ws("report-infer-task", &json!({
            "agents": {}, "active_team_key": null,
            "tasks": [{"id": "t-42", "assignee": "worker-7", "status": "pending"}]
        }));
        let tools = TeamOrchestratorTools::with_identity(&cws, Some(AgentId::new("worker-7")), None);
        let ok = tools.report_result(
            None, Some("done it"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("task_id"), Some(&json!("t-42")),
            "task_id inferred from latest non-terminal assigned task, not 'manual'");
    }

    // ── #42 get_visible_peers reads seeded state (sorted live peers) ────────────
    // GOLDEN (probe_peers.py): teamA agents {worker-z(alive),worker-a(working),
    // worker-dead(DEAD),worker-stopped(Stopped),worker-no-status(dict no status),
    // worker-weird(non-dict)} → peers ["worker-a","worker-no-status","worker-weird",
    // "worker-z"] (sorted, dead/stopped filtered, non-dict & no-status INCLUDED);
    // sender_team_id "teamA", scope team. Rust stub returns empty peers.
    #[test]
    fn get_visible_peers_reads_state_sorted_live_filtered() {
        let cws = seed_state_ws("visible-peers", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {
                "teamA": {"status": "alive", "agents": {
                    "worker-z": {"status": "alive"},
                    "worker-a": {"status": "working"},
                    "worker-dead": {"status": "DEAD"},
                    "worker-stopped": {"status": "Stopped"},
                    "worker-no-status": {},
                    "worker-weird": "not-a-dict"
                }},
                "teamB": {"status": "alive", "agents": {"other-bob": {"status": "alive"}}}
            }
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        let vp = tools.get_visible_peers().expect("visible peers");
        let got: Vec<&str> = vp.peers.iter().map(AgentId::as_str).collect();
        assert_eq!(got, vec!["worker-a", "worker-no-status", "worker-weird", "worker-z"]);
        assert_eq!(vp.sender_team_id, Some(TeamKey::new("teamA")));
        assert_eq!(vp.scope, Scope::Team);
    }

    // ── #42 refuse_cross_team_peer ALLOWS a live in-team peer (visible bypass) ──
    // GOLDEN (probe_peers.py): with the same seeded state, refusing worker-a / worker-z
    // / worker-no-status / worker-weird → None (ALLOWED, they are visible peers), while
    // worker-dead / worker-stopped / other-bob → refused. Rust stub refuses ALL of them.
    #[test]
    fn refuse_cross_team_peer_allows_live_in_team_peer() {
        let cws = seed_state_ws("refuse-inteam", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {
                "teamA": {"status": "alive", "agents": {
                    "worker-z": {"status": "alive"},
                    "worker-a": {"status": "working"},
                    "worker-dead": {"status": "DEAD"},
                    "worker-no-status": {},
                    "worker-weird": "not-a-dict"
                }},
                "teamB": {"status": "alive", "agents": {"other-bob": {"status": "alive"}}}
            }
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        // live / no-status / non-dict in-team peers are ALLOWED (None).
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-a".to_string()), None).is_none(),
            "a live in-team peer must be allowed (visible-peer bypass)");
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-z".to_string()), None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-no-status".to_string()), None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-weird".to_string()), None).is_none());
        // dead / other-team peers are still refused.
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-dead".to_string()), None).is_some());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("other-bob".to_string()), None).is_some());
    }

    // ── #48 refuse_cross_team_peer writes mcp.send_message_refused EventLog ─────
    // GOLDEN (probe_events_red.py): a refusal appends an event with fields
    // event=mcp.send_message_refused, reason=peer_not_in_scope, scope=team,
    // sender_team_id=teamA, hint=<the cross-team hint>. Rust handler writes nothing.
    #[test]
    fn refuse_cross_team_peer_writes_send_message_refused_event() {
        let cws = seed_state_ws("refuse-event", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {"teamA": {"status": "alive", "agents": {"worker-1": {"status": "alive"}}}}
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        // out-of-scope peer → refusal must emit the audit event.
        let _ = tools.refuse_cross_team_peer(&MessageTarget::Single("other-bob".to_string()), None);
        let events = EventLog::new(&cws).tail(50).expect("read events");
        let refused = events.iter().find(|e| e["event"] == json!("mcp.send_message_refused"))
            .expect("mcp.send_message_refused must be written on refusal");
        assert_eq!(refused["reason"], json!("peer_not_in_scope"));
        assert_eq!(refused["scope"], json!("team"));
        assert_eq!(refused["sender_team_id"], json!("teamA"));
        assert_eq!(refused["hint"],
            json!("the requested peer is not part of your team; worker-origin MCP cannot widen team scope."));
    }
