use super::*;

// idle_takeover.reminder / push_failed 事件名字节锁。
#[test]
fn idle_takeover_event_names_byte_locked() {
    assert_eq!(
        LeaderEvent::IdleTakeoverReminder.name(),
        "idle_takeover.reminder"
    );
    assert_eq!(
        LeaderEvent::IdleTakeoverPushFailed.name(),
        "idle_takeover.push_failed"
    );
    assert_eq!(LeaderEvent::IdleTakeoverPing.name(), "idle_takeover.ping");
}

// =====================================================================
// 11. struct 构造 / 序列化形态 + key 插入序证据(纯数据,不依赖 body)
// =====================================================================

// LeaderReceiver:所有可选字段 Option(bug-085 半状态合法);序列化保字段名。
#[test]
fn leader_receiver_struct_serializes_with_python_field_names() {
    let recv = LeaderReceiver {
        mode: ReceiverMode::DirectTmux,
        status: ReceiverStatus::Attached,
        provider: Provider::ClaudeCode,
        pane_id: PaneId::new("%648"),
        session_name: Some(SessionName::new("S")),
        window_index: Some("1".into()),
        window_name: Some(WindowName::new("W")),
        pane_index: Some("2".into()),
        pane_tty: Some("/dev/ttys001".into()),
        pane_current_command: Some("claude".into()),
        tmux_socket: None,
        scope_authority: None,
        authorized_team_workspace: None,
        binding_nonce: None,
        fingerprint: Some("fp".into()),
        leader_session_uuid: Some(uuid("fp", "/ws", "u", "default")),
        owner_epoch: Some(OwnerEpoch(3)),
        attached_at: Some("2026-06-02T00:00:00+00:00".into()),
        discovery: Some(Discovery::ClaimLeader),
        requested_provider: None,
        warning: None,
    };
    let v = serde_json::to_value(&recv).unwrap();
    assert_eq!(v["mode"], serde_json::json!("direct_tmux"));
    assert_eq!(v["status"], serde_json::json!("attached"));
    assert_eq!(v["provider"], serde_json::json!("claude_code"));
    assert_eq!(v["pane_id"], serde_json::json!("%648"));
    assert_eq!(v["owner_epoch"], serde_json::json!(3));
    assert_eq!(v["discovery"], serde_json::json!("claim_leader"));
    // bug-085:None 字段序列化为 null(半状态合法,不崩)。
    assert_eq!(v["requested_provider"], serde_json::Value::Null);
    assert_eq!(v["warning"], serde_json::Value::Null);
}

// TeamOwner:claimed_via kebab + owner_epoch int;os_user Option(Family A 才写)。
#[test]
fn team_owner_struct_serializes_with_python_shape() {
    let owner = TeamOwner {
        pane_id: PaneId::new("%9"),
        provider: Provider::Codex,
        machine_fingerprint: "fp".into(),
        leader_session_uuid: Some(uuid("fp", "/ws", "u", "default")),
        owner_epoch: OwnerEpoch(1),
        claimed_at: "2026-06-02T00:00:00+00:00".into(),
        claimed_via: ClaimedVia::ClaimLeader,
        os_user: Some("alice".into()),
    };
    let v = serde_json::to_value(&owner).unwrap();
    assert_eq!(v["claimed_via"], serde_json::json!("claim-leader"));
    assert_eq!(v["owner_epoch"], serde_json::json!(1));
    assert_eq!(v["provider"], serde_json::json!("codex"));
    assert_eq!(v["os_user"], serde_json::json!("alice"));
}

// LeaderIdentity:source 用 leader-plan 枚举值(Override→"override");team_id 透明串。
#[test]
fn leader_identity_struct_serializes_with_leader_plan_source() {
    let id = LeaderIdentity {
        leader_session_uuid: uuid("fp", "/ws", "u", "default"),
        leader_session_uuid_source: LeaderSessionUuidSource::Override,
        machine_fingerprint: "fp".into(),
        workspace_abspath: std::path::PathBuf::from("/ws"),
        os_user: "u".into(),
        team_id: TeamKey::new("default"),
    };
    let v = serde_json::to_value(&id).unwrap();
    assert_eq!(
        v["leader_session_uuid_source"],
        serde_json::json!("override")
    );
    assert_eq!(v["team_id"], serde_json::json!("default"));
}

// IdleNode:bug-085 rollout_path Option;state 是 TurnState(穷尽,Unknown 非 idle)。
#[test]
fn idle_node_unknown_state_is_not_idle() {
    let n = IdleNode {
        node_id: "w1".into(),
        role: NodeRole::Worker,
        state: TurnState::Unknown,
        turn_id: None,
        annotations: vec![],
        provider: Some(Provider::Codex),
        auth_mode: None,
        rollout_path: None, // bug-085:None 合法 → 该 node Unknown。
    };
    assert!(!n.state.is_idle_for_takeover(), "Unknown 不当 idle");
    assert!(n.rollout_path.is_none());
}
