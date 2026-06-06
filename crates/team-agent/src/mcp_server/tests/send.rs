    #[test]
    fn requires_ack_for_target_leader_vs_worker() {
        assert!(!requires_ack_for_target(&MessageTarget::Single("leader".to_string())));
        assert!(!requires_ack_for_target(&MessageTarget::Single("Leader".to_string())));
        assert!(requires_ack_for_target(&MessageTarget::Single("alice".to_string())));
        // list: all-leader → false; any non-leader → true
        assert!(!requires_ack_for_target(&MessageTarget::Fanout(vec![
            "leader".to_string(), "Leader".to_string()
        ])));
        assert!(requires_ack_for_target(&MessageTarget::Fanout(vec![
            "leader".to_string(), "alice".to_string()
        ])));
    }

    // ════════════════════════════════════════════════════════════════════════
    // is_worker_recipient — single str not in {"","*","leader","Leader"} (tools.py:22)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn is_worker_recipient_classification() {
        assert!(is_worker_recipient(&MessageTarget::Single("alice".to_string())));
        assert!(!is_worker_recipient(&MessageTarget::Single("".to_string())));
        assert!(!is_worker_recipient(&MessageTarget::Single("leader".to_string())));
        assert!(!is_worker_recipient(&MessageTarget::Single("Leader".to_string())));
        // Broadcast "*" is NOT a worker recipient
        assert!(!is_worker_recipient(&MessageTarget::Broadcast));
        // Fanout list is NOT a worker recipient (not a single str)
        assert!(!is_worker_recipient(&MessageTarget::Fanout(vec!["alice".to_string()])));
    }

    // ════════════════════════════════════════════════════════════════════════
    // merge_tasks_by_id — prefer wins, prefer-first insertion order (tools.py:30)
    // Golden: prefer t1(done),t2 + fallback t1(pending),t3,{no id},"notdict"
    //   → [t1(done), t2, t3]  (t1 from prefer wins; non-dict / no-id dropped)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn merge_tasks_by_id_prefer_wins_no_done_regression() {
        let prefer = vec![
            json!({"id": "t1", "status": "done"}),
            json!({"id": "t2", "status": "pending"}),
        ];
        let fallback = vec![
            json!({"id": "t1", "status": "pending"}), // must NOT regress t1
            json!({"id": "t3", "status": "ready"}),
            json!({"no": "id"}),                       // dropped (no id)
            json!("notdict"),                          // dropped (not object)
        ];
        let merged = merge_tasks_by_id(&prefer, &fallback);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0]["id"], json!("t1"));
        assert_eq!(merged[0]["status"], json!("done")); // prefer wins → no regression
        assert_eq!(merged[1]["id"], json!("t2"));
        assert_eq!(merged[2]["id"], json!("t3"));
    }

    // ════════════════════════════════════════════════════════════════════════
    // SendOutcome::to_value — worker-accepted async envelope (tools.py:177-182)
    // byte-stable: {status:"accepted",delivery_pending:true,
    //               poll_via:"team-agent inbox <id>",message_id:<id>}
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn send_outcome_worker_accepted_envelope_byte_stable() {
        let outcome = SendOutcome::WorkerAccepted {
            message_id: "42".to_string(),
            poll_via: "team-agent inbox 42".to_string(),
        };
        let v = outcome.to_value();
        assert_eq!(keys(&v), vec!["status", "delivery_pending", "poll_via", "message_id"]);
        assert_eq!(s(&v),
            r#"{"status":"accepted","delivery_pending":true,"poll_via":"team-agent inbox 42","message_id":"42"}"#);
    }

    #[test]
    fn send_outcome_direct_renders_compact_body() {
        // leader / * / broadcast path → compacted delegate body, not the accepted envelope.
        let ok = ToolOk {
            fields: {
                let mut m = serde_json::Map::new();
                m.insert("ok".to_string(), json!(true));
                m.insert("status".to_string(), json!("queued"));
                m
            },
        };
        let v = SendOutcome::Direct(ok).to_value();
        assert_eq!(v.get("status"), Some(&json!("queued")));
        assert!(v.get("delivery_pending").is_none(), "Direct is NOT the accepted envelope");
    }

    // ════════════════════════════════════════════════════════════════════════
    // CONTROL-PLANE: send_message worker recipient → WorkerAccepted (tools.py:135-183)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn send_message_worker_recipient_returns_accepted_with_poll_hint() {
        // A worker recipient w/ a delivered message_id → async accepted carrying the
        // byte-stable poll hint. Identity anchored on injected env (no candidate scan).
        // golden: a leader WITH owner_team_id on an unseeded ws would hit the C23 cross-team
        // refusal first (worker-1 not in visible peers) -> PeerNotInScope. owner_team_id=None
        // (legacy single-team) bypasses that, isolating the worker-recipient accepted path.
        // The cross-team refusal has its own tests (refuse_cross_team_peer_* / send_message_cross_team_*).
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("send-worker"),
            Some(AgentId::new("leader")),
            None,
        );
        let outcome = tools.send_message(
            &MessageTarget::Single("worker-1".to_string()),
            "do the thing",
            None, None, None, None,
        );
        match outcome {
            Ok(SendOutcome::WorkerAccepted { message_id, poll_via }) => {
                assert!(!message_id.is_empty());
                assert_eq!(poll_via, format!("team-agent inbox {message_id}"));
            }
            other => panic!("worker recipient must be WorkerAccepted, got {other:?}"),
        }
    }

    #[test]
    fn send_message_leader_recipient_is_direct_not_accepted() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("send-leader"),
            Some(AgentId::new("worker-1")),
            Some(TeamKey::new("teamA")),
        );
        let outcome = tools.send_message(
            &MessageTarget::Single("leader".to_string()),
            "status update",
            None, None, None, None,
        ).expect("leader send ok");
        assert!(matches!(outcome, SendOutcome::Direct(_)),
            "leader recipient → Direct (synchronous), not WorkerAccepted");
    }

    // ════════════════════════════════════════════════════════════════════════
    // CROSS-TEAM PRE-REFUSAL (C23) — refuse_cross_team_peer (tools.py:185-213)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn refuse_cross_team_peer_blocks_unknown_peer_without_workspace_scope() {
        // owner_team set, target a peer NOT in scope, scope != workspace → PeerNotInScope.
        let tools = TeamOrchestratorTools::with_identity(
            Path::new("/tmp/ws"),
            Some(AgentId::new("worker-1")),
            Some(TeamKey::new("teamA")),
        );
        let refusal = tools.refuse_cross_team_peer(
            &MessageTarget::Single("other-team-bob".to_string()),
            None,
        );
        let te = refusal.expect("cross-team peer must be refused");
        assert_eq!(te.reason, ToolErrorReason::PeerNotInScope);
        // hint preserved in extra (tools.py:208-213 status:"refused" + hint)
        let env = te.to_envelope();
        assert_eq!(env.get("status"), Some(&json!("refused")));
        assert_eq!(env.get("reason"), Some(&json!("peer_not_in_scope")));
        assert_eq!(
            env.get("hint"),
            Some(&json!("the requested peer is not part of your team. pass scope='workspace' to address peers in other teams."))
        );
    }

    #[test]
    fn refuse_cross_team_peer_allows_workspace_scope_optin() {
        let tools = TeamOrchestratorTools::with_identity(
            Path::new("/tmp/ws"),
            Some(AgentId::new("worker-1")),
            Some(TeamKey::new("teamA")),
        );
        // scope="workspace" → None (allowed to proceed)
        assert!(tools.refuse_cross_team_peer(
            &MessageTarget::Single("other-team-bob".to_string()),
            Some(Scope::Workspace),
        ).is_none(), "workspace scope opts in to cross-team addressing");
    }

    #[test]
    fn refuse_cross_team_peer_allows_leader_broadcast_and_self() {
        let tools = TeamOrchestratorTools::with_identity(
            Path::new("/tmp/ws"),
            Some(AgentId::new("worker-1")),
            Some(TeamKey::new("teamA")),
        );
        // leader / "*" / "" / self are never refused (tools.py:190,195)
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("leader".to_string()), None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Broadcast, None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-1".to_string()), None).is_none());
    }

    #[test]
    fn refuse_cross_team_peer_no_owner_team_is_legacy_passthrough() {
        // No owner_team_id (legacy single-team) → never refuse (tools.py:192).
        let tools = TeamOrchestratorTools::with_identity(
            Path::new("/tmp/ws"),
            Some(AgentId::new("worker-1")),
            None,
        );
        assert!(tools.refuse_cross_team_peer(
            &MessageTarget::Single("anybody".to_string()),
            None,
        ).is_none());
    }

    #[test]
    fn send_message_cross_team_peer_surfaces_peer_not_in_scope_error() {
        // End-to-end: send_message to an out-of-scope peer → Err(ToolError{PeerNotInScope})
        // BEFORE any runtime delivery (server-side guard, no peer-name leak).
        let tools = TeamOrchestratorTools::with_identity(
            Path::new("/tmp/ws"),
            Some(AgentId::new("worker-1")),
            Some(TeamKey::new("teamA")),
        );
        let err = tools.send_message(
            &MessageTarget::Single("other-team-bob".to_string()),
            "leak attempt",
            None, None, None, None,
        ).expect_err("out-of-scope peer must be refused");
        assert_eq!(err.reason, ToolErrorReason::PeerNotInScope);
    }

    // ════════════════════════════════════════════════════════════════════════
    // WORKER-ID INFERENCE FALLBACK (bug-085, C17) — report_result identity.
    // explicit > env > "unknown"; task → "manual". NEVER treat worker as leader.
    // ════════════════════════════════════════════════════════════════════════
