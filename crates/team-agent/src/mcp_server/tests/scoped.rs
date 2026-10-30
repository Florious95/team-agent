/// Scoped retained-tool contracts exercise owner binding and team isolation.

    #[test]
    fn dispatch_send_message_worker_returns_physical_receipt_verbatim() {
        let fixture = direct_fixture::DirectFixture::new();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
        );
        let ok = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to": "worker-1", "content": "do it"
        })).expect("send ok");
        let value = serde_json::to_value(&ok).unwrap();
        assert_eq!(value["status"], "sent");
        assert_eq!(value["delivery"], "physical");
        assert_eq!(value["recipient"], "worker-1");
        assert_eq!(value["pane_id"], fixture.pane);
        assert_eq!(value["accepted"], Value::Null);
        // M1 stores before submitting, so the physical receipt names its inbox record.
        let inbox = crate::inbox_store::read(&fixture.root, "worker-1", 10, Some("current")).unwrap();
        assert_eq!(inbox["messages"][0]["message_id"], value["message_id"]);
        assert!(value.get("poll_via").is_none());
        fixture.received("Team Agent message from leader:\n\ndo it");
        assert!(!fixture.root.join(".team/runtime/team.db").exists());
    }

    // ── #36 report_result identity: neither envelope nor args can override capture ─
    #[test]
    fn report_result_rejects_envelope_and_argument_identity_mismatch() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-setdefault"),
            Some(AgentId::new("env-id")),
            None,
        );
        let error = tools
            .report_result(
                Some(&json!({
                    "agent_id": "env-agent", "task_id": "env-task",
                    "status": "blocked", "summary": "env summary"
                })),
                Some("ARG summary"), ResultStatus::Success,
                None, None, None, None, None,
                Some("ARG-task"), Some("ARG-agent"),
            )
            .expect_err("envelope and argument identities must be bound to the worker capture");
        assert_eq!(error.reason, ToolErrorReason::McpScopeRefused);
        assert!(error.message.contains("identity_mismatch"));
        assert_eq!(error.extra.get("expected_agent_id"), Some(&json!("env-id")));
    }

    // Decoupled reports never infer task attribution from coordinator state.
    #[test]
    fn report_result_defaults_to_manual_despite_latest_assigned_task() {
        // M1 binds every report to the seat's captured team; the coordinator task list
        // must still not leak into attribution.
        let cws = seed_state_ws("report-infer-task", &json!({
            "agents": {}, "active_team_key": "teamA",
            "teams": {"teamA": {"status": "alive", "agents": {"worker-7": {"status": "running"}}}},
            "tasks": [{"id": "t-42", "assignee": "worker-7", "status": "pending"}]
        }));
        let tools = TeamOrchestratorTools::with_identity(&cws, Some(AgentId::new("worker-7")), Some(TeamKey::new("teamA")));
        let ok = tools.report_result(
            None, Some("done it"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("task_id"), Some(&json!("manual")));
        let inbox = crate::inbox_store::read(&cws, "leader", 10, Some("teamA")).unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
        assert_eq!(inbox["messages"][0]["envelope"]["task_id"], "manual");
        assert_eq!(inbox["messages"][0]["content"], "done it");
        assert!(!cws.join(".team/runtime/team.db").exists());
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
