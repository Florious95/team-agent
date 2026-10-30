#[path = "../../../tests/support/direct_fixture.rs"]
mod direct_fixture;

#[test]
fn requires_ack_for_target_leader_vs_worker() {
    assert!(!requires_ack_for_target(&MessageTarget::Single(
        "leader".to_string()
    )));
    assert!(!requires_ack_for_target(&MessageTarget::Single(
        "Leader".to_string()
    )));
    assert!(requires_ack_for_target(&MessageTarget::Single(
        "alice".to_string()
    )));
    // list: all-leader → false; any non-leader → true
    assert!(!requires_ack_for_target(&MessageTarget::Fanout(vec![
        "leader".to_string(),
        "Leader".to_string()
    ])));
    assert!(requires_ack_for_target(&MessageTarget::Fanout(vec![
        "leader".to_string(),
        "alice".to_string()
    ])));
}

// ════════════════════════════════════════════════════════════════════════
// is_worker_recipient — single str not in {"","*","leader","Leader"} (tools.py:22)
// ════════════════════════════════════════════════════════════════════════
#[test]
fn is_worker_recipient_classification() {
    assert!(is_worker_recipient(&MessageTarget::Single(
        "alice".to_string()
    )));
    assert!(!is_worker_recipient(&MessageTarget::Single("".to_string())));
    assert!(!is_worker_recipient(&MessageTarget::Single(
        "leader".to_string()
    )));
    assert!(!is_worker_recipient(&MessageTarget::Single(
        "Leader".to_string()
    )));
    // Broadcast "*" is NOT a worker recipient
    assert!(!is_worker_recipient(&MessageTarget::Broadcast));
    // Fanout list is NOT a worker recipient (not a single str)
    assert!(!is_worker_recipient(&MessageTarget::Fanout(vec![
        "alice".to_string()
    ])));
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
        warning: None,
    };
    let v = outcome.to_value();
    assert_eq!(
        keys(&v),
        vec!["status", "delivery_pending", "poll_via", "message_id"]
    );
    assert_eq!(
        s(&v),
        r#"{"status":"accepted","delivery_pending":true,"poll_via":"team-agent inbox 42","message_id":"42"}"#
    );
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
    assert!(
        v.get("delivery_pending").is_none(),
        "Direct is NOT the accepted envelope"
    );
}

// ════════════════════════════════════════════════════════════════════════
// CONTROL-PLANE: send_message worker recipient → WorkerAccepted (tools.py:135-183)
// ════════════════════════════════════════════════════════════════════════
fn seed_current_worker_state(tag: &str) -> std::path::PathBuf {
    let ws = unique_ws(tag);
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "active_team_key": "current",
            "agents": {
                "worker-1": {"status": "running", "agent_id": "worker-1", "window": "worker-1"},
            },
            "teams": {
                "current": {
                    "status": "alive",
                    "agents": {
                        "worker-1": {"status": "running", "agent_id": "worker-1", "window": "worker-1"},
                    },
                },
            },
        }),
    )
    .unwrap();
    ws
}

#[test]
fn send_message_worker_recipient_returns_physical_receipt_without_poll_hint() {
    let fixture = direct_fixture::DirectFixture::new();
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
    );
    let outcome = tools.send_message(
        &MessageTarget::Single("worker-1".into()), "do the thing", None, None, None,
    ).unwrap();
    assert!(matches!(outcome, SendOutcome::Direct(_)));
    let value = outcome.to_value();
    assert_eq!(value["status"], "sent");
    assert_eq!(value["delivery"], "physical");
    assert_eq!(value["accepted"], Value::Null);
    assert!(value.get("poll_via").is_none());
    assert!(value.get("delivery_pending").is_none());
    fixture.received("Team Agent message from leader:\n\ndo the thing");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    // M1 stores before submitting: the receipt names the durable inbox record.
    let inbox = crate::inbox_store::read(&fixture.root, "worker-1", 10, Some("current")).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["messages"][0]["message_id"], value["message_id"]);
}

#[test]
fn ordinary_send_shape_has_no_recovery_marker() {
    let fixture = direct_fixture::DirectFixture::new();
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
    );
    let sent = tools.send_message(
        &MessageTarget::Single("worker-1".into()), "ordinary send", None, None, None,
    ).unwrap().to_value();
    assert_eq!(sent["status"], "sent");
    assert!(sent.get("recovery").is_none());
    assert!(sent.get("acceptance_marker").is_none());
    fixture.received("Team Agent message from leader:\n\nordinary send");
}

#[test]
fn send_message_without_framework_identity_fails_closed() {
    let ws = seed_current_worker_state("missing-sender-identity");
    let tools = TeamOrchestratorTools::with_identity(&ws, None, Some(TeamKey::new("current")));
    let error = tools
        .send_message(
            &MessageTarget::Single("worker-1".to_string()),
            "must not be attributed to unknown",
            None,
            None,
            None,
        )
        .expect_err("missing framework identity must fail before persistence");
    assert_eq!(error.reason, ToolErrorReason::McpScopeRefused);
}

#[test]
fn send_message_worker_recipient_ignores_dead_coordinator() {
    let fixture = direct_fixture::DirectFixture::new();
    let coordinator_ws = crate::coordinator::WorkspacePath::new(fixture.root.clone());
    let stale_pid = crate::coordinator::Pid::new(99_999_999);
    crate::coordinator::write_coordinator_metadata(
        &coordinator_ws, stale_pid, crate::coordinator::MetadataSource::Boot,
    ).unwrap();
    std::fs::write(crate::coordinator::coordinator_pid_path(&coordinator_ws), stale_pid.to_string()).unwrap();
    let before = std::fs::read(fixture.root.join(".team/runtime/state.json")).unwrap();
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
    );
    let value = tools.send_message(
        &MessageTarget::Single("worker-1".into()), "no coordinator required", None, None, None,
    ).unwrap().to_value();
    assert_eq!(value["status"], "sent");
    assert_eq!(value["delivery"], "physical");
    assert!(value.get("delivery_pending").is_none());
    fixture.received("Team Agent message from leader:\n\nno coordinator required");
    assert_eq!(before, std::fs::read(fixture.root.join(".team/runtime/state.json")).unwrap());
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
fn send_message_leader_recipient_is_direct_not_accepted() {
    let ws = seed_state_ws("send-leader", &json!({"teams": {"teamA": {"agents": {"worker-1": {}}}}}));
    let tools = TeamOrchestratorTools::with_identity(
        &ws,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    let outcome = tools
        .send_message(
            &MessageTarget::Single("leader".to_string()),
            "status update",
            None,
            None,
            None,
        )
        .expect("leader send ok");
    assert!(
        matches!(outcome, SendOutcome::Direct(_)),
        "leader recipient → Direct (synchronous), not WorkerAccepted"
    );
}

#[test]
fn send_message_mailbox_is_durable_without_live_injection() {
    let ws = seed_state_ws("send-leader-mailbox", &json!({"teams": {"teamA": {"agents": {"worker-1": {}}}}}));
    let tools = TeamOrchestratorTools::with_identity(
        &ws,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    let mailbox = json!(true);
    let outcome = tools
        .send_message_with_presentation(
            &MessageTarget::Single("leader".to_string()),
            "stored update",
            None,
            None,
            None,
            Some(&mailbox),
            None,
        )
        .expect("mailbox send persists");
    let value = outcome.to_value();
    assert_eq!(value.get("status"), Some(&json!("stored")));
    // M1 receipt: a mailbox send is stored, not live, and never attempts submission.
    assert_eq!(value["live"], false);
    assert!(value.get("submission").is_none(), "{value}");
    let inbox = crate::inbox_store::read(&ws, "leader", 10, Some("teamA")).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["messages"][0]["message_id"], value["message_id"]);
    assert_eq!(inbox["messages"][0]["content"], "stored update");
    assert!(!ws.join(".team/runtime/team.db").exists());
}

// ════════════════════════════════════════════════════════════════════════
// CROSS-TEAM PRE-REFUSAL (C23) — refuse_cross_team_peer (tools.py:185-213)
// ════════════════════════════════════════════════════════════════════════
#[test]
fn refuse_cross_team_peer_blocks_unknown_peer_without_workspace_scope() {
    // owner_team set, target a peer NOT in scope, scope != workspace → PeerNotInScope.
    let tools = TeamOrchestratorTools::with_identity(
        &seed_state_ws("refuse_cross_team_peer_blocks_unknown_peer_without_workspace_scope", &json!({"teams": {"teamA": {"agents": {"worker-1": {}}}}})),
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    let refusal =
        tools.refuse_cross_team_peer(&MessageTarget::Single("other-team-bob".to_string()), None);
    let te = refusal.expect("cross-team peer must be refused");
    assert_eq!(te.reason, ToolErrorReason::PeerNotInScope);
    // hint preserved in extra (tools.py:208-213 status:"refused" + hint)
    let env = te.to_envelope();
    assert_eq!(env.get("status"), Some(&json!("refused")));
    assert_eq!(env.get("reason"), Some(&json!("peer_not_in_scope")));
    assert_eq!(
            env.get("hint"),
            Some(&json!("the requested peer is not part of your team; worker-origin MCP cannot widen team scope."))
        );
}

#[test]
fn refuse_cross_team_peer_rejects_workspace_scope_override_for_worker() {
    let tools = TeamOrchestratorTools::with_identity(
        Path::new("/tmp/ws"),
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    // scope="workspace" is not worker consent to cross team boundaries.
    let te = tools
        .refuse_cross_team_peer(
            &MessageTarget::Single("other-team-bob".to_string()),
            Some(Scope::Workspace),
        )
        .expect("workspace scope override must still be refused for worker-origin MCP");
    assert_eq!(te.reason, ToolErrorReason::McpScopeRefused);
}

#[test]
fn refuse_cross_team_peer_allows_leader_broadcast_and_self() {
    let tools = TeamOrchestratorTools::with_identity(
        &seed_state_ws("refuse_cross_team_peer_allows_leader_broadcast_and_self", &json!({"teams": {"teamA": {"agents": {"worker-1": {}}}}})),
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    // leader / "*" / "" / self are never refused (tools.py:190,195)
    assert!(tools
        .refuse_cross_team_peer(&MessageTarget::Single("leader".to_string()), None)
        .is_none());
    assert!(tools
        .refuse_cross_team_peer(&MessageTarget::Broadcast, None)
        .is_none());
    assert!(tools
        .refuse_cross_team_peer(&MessageTarget::Single("worker-1".to_string()), None)
        .is_none());
}

#[test]
fn refuse_cross_team_peer_no_owner_team_is_legacy_passthrough() {
    // A readable legacy snapshot without a teams map retains the legacy scope.
    let tools = TeamOrchestratorTools::with_identity(
        &seed_state_ws("legacy-single-team", &json!({"agents": {"worker-1": {}}})),
        Some(AgentId::new("worker-1")),
        None,
    );
    assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("anybody".into()), None).is_none());
}

#[test]
fn send_message_cross_team_peer_surfaces_peer_not_in_scope_error() {
    // End-to-end: send_message to an out-of-scope peer → Err(ToolError{PeerNotInScope})
    // BEFORE any runtime delivery (server-side guard, no peer-name leak).
    // A real published snapshot distinguishes unknown peers from an unreadable scope.
    let workspace = unique_ws("direct-peer-scope");
    std::fs::create_dir_all(workspace.join(".team/runtime")).unwrap();
    std::fs::write(workspace.join(".team/runtime/state.json"), serde_json::to_vec(&serde_json::json!({
        "active_team_key": "teamA",
        "teams": {"teamA": {"team_id": "teamA", "agents": {"worker-1": {}}}}
    })).unwrap()).unwrap();
    let tools = TeamOrchestratorTools::with_identity(
        &workspace,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    let err = tools
        .send_message(
            &MessageTarget::Single("other-team-bob".to_string()),
            "leak attempt",
            None,
            None,
            None,
        )
        .expect_err("out-of-scope peer must be refused");
    assert_eq!(err.reason, ToolErrorReason::PeerNotInScope);
    assert!(!workspace.join(".team/runtime/team.db").exists());
    std::fs::remove_dir_all(workspace).unwrap();
}

// ════════════════════════════════════════════════════════════════════════
// WORKER-ID INFERENCE FALLBACK (bug-085, C17) — report_result identity.
// explicit > env > "unknown"; task → "manual". NEVER treat worker as leader.
// ════════════════════════════════════════════════════════════════════════
