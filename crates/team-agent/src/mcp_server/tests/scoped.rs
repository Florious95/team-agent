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

    #[test]
    fn leader_send_resolves_teams_created_after_mcp_launch() {
        let fixture = direct_fixture::DirectFixture::new();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
        );
        // The MCP server is already running when the leader creates a team.
        let mut state = json!({"active_team_key":"release-review", "teams":{
            "current":{"team_id":"current", "agents":{}},
            "release-review":{"team_id":"release-review", "tmux_socket":fixture.socket,
                "agents":{"reviewer":{"pane_id":fixture.pane, "status":"running"}}}
        }});
        for target in ["reviewer", "release-review/reviewer"] {
            // Also cover a launch team that has since disappeared.
            if target.contains('/') { state["teams"].as_object_mut().unwrap().remove("current"); }
            fixture.set_state(&state);
            let content = format!("review via {target}");
            let value = dispatch_tool(&tools, McpTool::SendMessage, &json!({
                "to":target, "content":content
            })).expect("leader uses native dynamic send resolution");
            let value = serde_json::to_value(value).unwrap();
            assert_eq!(value["delivery"], "physical", "{value}");
            fixture.received(&format!("Team Agent message from leader:\n\n{content}"));
        }
        let inbox = crate::inbox_store::read(&fixture.root, "reviewer", 10, Some("release-review")).unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
        assert!(inbox["messages"].as_array().unwrap().iter().all(|row|
            row["sender"] == "leader" && row["provenance"]["authority"] == "operator"));
    }

    #[test]
    fn leader_send_ambiguous_name_fails_closed_but_qualified_target_delivers() {
        let fixture = direct_fixture::DirectFixture::new();
        fixture.set_state(&json!({"active_team_key":"current", "teams":{
            "current":{"team_id":"current", "tmux_socket":fixture.socket,
                "agents":{"reviewer":{"pane_id":fixture.pane, "status":"running"}}},
            "release-review":{"team_id":"release-review", "tmux_socket":fixture.socket,
                "agents":{"reviewer":{"pane_id":fixture.pane, "status":"running"}}}
        }}));
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
        );
        let before = fixture.inbox_bytes();
        let error = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to":"reviewer", "content":"ambiguous must not deliver"
        })).unwrap_err();
        assert_eq!(error.reason, ToolErrorReason::NameAmbiguous, "{error:?}");
        assert_eq!(fixture.inbox_bytes(), before);
        let value = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to":"release-review/reviewer", "content":"qualified delivery"
        })).unwrap();
        let value = serde_json::to_value(value).unwrap();
        assert_eq!(value["delivery"], "physical");
        fixture.received("Team Agent message from leader:\n\nqualified delivery");
    }

    #[test]
    fn worker_send_cannot_follow_leader_into_other_teams_or_impersonate_leader() {
        let workspace = seed_state_ws("worker-send-scope", &json!({"active_team_key":"release-review", "teams":{
            "current":{"team_id":"current", "agents":{"worker-1":{"status":"running"}}},
            "release-review":{"team_id":"release-review", "agents":{"reviewer":{"status":"running"}}}
        }}));
        let tools = TeamOrchestratorTools::with_identity(
            &workspace, Some(AgentId::new("worker-1")), Some(TeamKey::new("current")),
        );
        for target in ["reviewer", "release-review/reviewer"] {
            dispatch_tool(&tools, McpTool::SendMessage, &json!({
                "to":target, "content":"must not escape", "mailbox":true
            })).expect_err("workers remain bound to their captured team");
        }
        let error = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to":"release-review/reviewer", "content":"must not impersonate", "agent_id":"leader"
        })).unwrap_err();
        assert!(error.message.contains("identity_mismatch"));
        assert!(!workspace.join(".team/inbox").exists());
        std::fs::remove_dir_all(workspace).unwrap();
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
            .report_result_with_presentation(
                Some(&json!({
                    "agent_id": "env-agent", "task_id": "env-task",
                    "status": "blocked", "summary": "env summary"
                })),
                Some("ARG summary"), Some("success"),
                None, None, None, None, None,
                Some("ARG-task"), Some("ARG-agent"), None)
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
        let ok = tools.report_result_with_presentation(
            None, Some("done it"), Some("success"),
            None, None, None, None, None,
            None, None, None).expect("report ok");
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
        // Durable ingress admits in-team peers regardless of their physical liveness.
        for recipient in ["worker-a", "worker-z", "worker-no-status", "worker-weird", "worker-dead"] {
            let outcome = tools.send_message_with_presentation(
                &MessageTarget::Single(recipient.into()), "scope probe",
                None, None, None, Some(&json!(true)), None,
            ).expect("in-team durable ingress");
            assert_eq!(outcome.to_value()["ok"], true, "{recipient}");
            assert_eq!(outcome.to_value()["status"], "stored", "{recipient}");
        }
        let error = tools.send_message_with_presentation(
            &MessageTarget::Single("teamB/other-bob".into()), "scope probe",
            None, None, None, Some(&json!(true)), None,
        ).expect_err("foreign team remains outside the captured worker scope");
        assert_eq!(error.reason, ToolErrorReason::PeerNotInScope);
    }

    // ── #48 refuse_cross_team_peer writes mcp.send_message_refused EventLog ─────
    // GOLDEN (probe_events_red.py): a refusal appends an event with fields
    // event=mcp.send_message_refused, reason=peer_not_in_scope, scope=team,
    // sender_team_id=teamA, hint=<the cross-team hint>. Rust handler writes nothing.
