#[path = "../../../tests/support/direct_fixture.rs"]
mod direct_fixture;


// ════════════════════════════════════════════════════════════════════════
// is_worker_recipient — single str not in {"","*","leader","Leader"} (tools.py:22)
// ════════════════════════════════════════════════════════════════════════

// ════════════════════════════════════════════════════════════════════════
// SendOutcome::to_value — worker-accepted async envelope (tools.py:177-182)
// byte-stable: {status:"accepted",delivery_pending:true,
//               poll_via:"team-agent inbox <id>",message_id:<id>}
// ════════════════════════════════════════════════════════════════════════


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
    let outcome = tools.send_message_with_presentation(
        &MessageTarget::Single("worker-1".into()), "do the thing", None, None, None, None, None).unwrap();
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
    let sent = tools.send_message_with_presentation(
        &MessageTarget::Single("worker-1".into()), "ordinary send", None, None, None, None, None).unwrap().to_value();
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
        .send_message_with_presentation(
            &MessageTarget::Single("worker-1".to_string()),
            "must not be attributed to unknown",
            None,
            None,
            None, None, None)
        .expect_err("missing framework identity must fail before persistence");
    assert_eq!(error.reason, ToolErrorReason::McpScopeRefused);
}

#[test]
fn send_message_worker_recipient_ignores_dead_coordinator() {
    let fixture = direct_fixture::DirectFixture::new();
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root, Some(AgentId::new("leader")), Some(TeamKey::new("current")),
    );
    // cfd9eb90: establish the legitimate first-send timestamp before the no-repair oracle.
    let primed = tools.send_message_with_presentation(
        &MessageTarget::Single("worker-1".into()), "establish first send", None, None, None, None, None).unwrap().to_value();
    assert_eq!(primed["delivery"], "physical");
    fixture.received("Team Agent message from leader:\n\nestablish first send");
    let coordinator_ws = crate::coordinator::WorkspacePath::new(fixture.root.clone());
    // A real, synchronously reaped child replaces an arbitrary host PID constant.
    let exited = std::process::Command::new("/bin/sh").env_clear().current_dir(&fixture.root)
        .args(["-c", "printf '%s\\n' \"$$\""]).output().unwrap();
    assert!(exited.status.success());
    let stale_pid = crate::coordinator::Pid::new(String::from_utf8(exited.stdout).unwrap().trim().parse().unwrap());
    crate::coordinator::write_coordinator_metadata(
        &coordinator_ws, stale_pid, crate::coordinator::MetadataSource::Boot,
    ).unwrap();
    std::fs::write(crate::coordinator::coordinator_pid_path(&coordinator_ws), stale_pid.to_string()).unwrap();
    let before = std::fs::read(fixture.root.join(".team/runtime/state.json")).unwrap();
    let value = tools.send_message_with_presentation(
        &MessageTarget::Single("worker-1".into()), "no coordinator required", None, None, None, None, None).unwrap().to_value();
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
        .send_message_with_presentation(
            &MessageTarget::Single("leader".to_string()),
            "status update",
            None,
            None,
            None, None, None)
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
    let workspace = seed_state_ws("refuse-unknown-current", &json!({
        "teams": {"teamA": {"agents": {"worker-1": {}}}}
    }));
    let tools = TeamOrchestratorTools::with_identity(
        &workspace, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
    );
    let error = tools.send_message_with_presentation(
        &MessageTarget::Single("other-team-bob".into()), "scope probe",
        None, None, None, Some(&json!(true)), None,
    ).expect_err("an unknown peer cannot be admitted");
    // Current admission distinguishes an unknown name from a real foreign peer.
    assert_eq!(error.reason, ToolErrorReason::NameNotResolvable);
    let envelope = error.to_envelope();
    assert_eq!(envelope["status"], "refused");
    assert_eq!(envelope["reason"], "name_not_resolvable");
    assert!(envelope["hint"].as_str().is_some_and(|hint| !hint.is_empty()));
    let inbox = crate::inbox_store::read(&workspace, "other-team-bob", 10, Some("teamA")).unwrap();
    assert!(inbox["messages"].as_array().unwrap().is_empty());
}

#[test]
fn refuse_cross_team_peer_rejects_workspace_scope_override_for_worker() {
    let tools = TeamOrchestratorTools::with_identity(
        Path::new("/tmp/ws"), Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
    );
    let error = tools.send_message_with_presentation(
        &MessageTarget::Single("other-team-bob".into()), "scope probe",
        None, None, Some(Scope::Workspace), Some(&json!(true)), None,
    ).expect_err("workspace scope cannot widen a captured worker identity");
    assert_eq!(error.reason, ToolErrorReason::McpScopeRefused);
}

#[test]
fn refuse_cross_team_peer_allows_leader_broadcast_and_self() {
    let workspace = seed_state_ws("refuse-special-targets-current", &json!({
        "teams": {"teamA": {"agents": {"worker-1": {}, "worker-2": {}}}}
    }));
    let tools = TeamOrchestratorTools::with_identity(
        &workspace, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
    );
    for target in [MessageTarget::Single("leader".into()), MessageTarget::Broadcast] {
        let outcome = tools.send_message_with_presentation(
            &target, "scope probe", None, None, None, Some(&json!(true)), None,
        ).expect("leader and nonempty broadcast are admitted in the bound team");
        assert_eq!(outcome.to_value()["ok"], true);
        assert_eq!(outcome.to_value()["status"], "stored");
    }
    let error = tools.send_message_with_presentation(
        &MessageTarget::Single("worker-1".into()), "scope probe",
        None, None, None, Some(&json!(true)), None,
    ).expect_err("current public admission refuses an explicit self target");
    assert_eq!(error.reason, ToolErrorReason::SelfTarget);
    let inbox = crate::inbox_store::read(&workspace, "worker-1", 10, Some("teamA")).unwrap();
    assert!(inbox["messages"].as_array().unwrap().is_empty());
}



#[test]
fn send_message_cross_team_peer_surfaces_peer_not_in_scope_error() {
    // End-to-end: send_message to an out-of-scope peer → Err(ToolError{PeerNotInScope})
    // BEFORE any runtime delivery (server-side guard, no peer-name leak).
    // M5 (homomorphic send, admission ruling 1b): a name that exists nowhere is
    // name_not_resolvable exactly as on the CLI; peer_not_in_scope is kept for a real
    // peer in another team. Neither refusal names the other team's roster.
    let workspace = unique_ws("direct-peer-scope");
    std::fs::create_dir_all(workspace.join(".team/runtime")).unwrap();
    std::fs::write(workspace.join(".team/runtime/state.json"), serde_json::to_vec(&serde_json::json!({
        "active_team_key": "teamA",
        "teams": {
            "teamA": {"team_id": "teamA", "agents": {"worker-1": {}}},
            "teamB": {"team_id": "teamB", "agents": {"bob": {}}}
        }
    })).unwrap()).unwrap();
    let tools = TeamOrchestratorTools::with_identity(
        &workspace,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("teamA")),
    );
    for (target, reason) in [
        ("other-team-bob", ToolErrorReason::NameNotResolvable),
        ("teamB/bob", ToolErrorReason::PeerNotInScope),
    ] {
        let err = tools
            .send_message_with_presentation(&MessageTarget::Single(target.to_string()), "leak attempt", None, None, None, None, None)
            .expect_err("out-of-scope peer must be refused");
        assert_eq!(err.reason, reason, "{target}: {err:?}");
        let candidates = err.extra.get("candidates").and_then(|value| value.as_array()).cloned().unwrap_or_default();
        assert!(candidates.iter().all(|c| c["team_key"] == "teamA"), "{target} leaked the other roster: {err:?}");
    }
    assert!(!workspace.join(".team/inbox").exists(), "a refused send stores nothing");
    assert!(!workspace.join(".team/runtime/team.db").exists());
    std::fs::remove_dir_all(workspace).unwrap();
}

// ════════════════════════════════════════════════════════════════════════
// WORKER-ID INFERENCE FALLBACK (bug-085, C17) — report_result identity.
// explicit > env > "unknown"; task → "manual". NEVER treat worker as leader.
// ════════════════════════════════════════════════════════════════════════
