use super::*;

// =====================================================================
// 8. idle-takeover 接线 — build_idle_nodes / leader_node(unimplemented → RED)
//    命门:rollout_path=None → Unknown → never idle;leader path/provider 缺 → 省略;
//    MUST-NOT-13:经 TurnStateClassifier mock,断言零 provider client 直连。
// =====================================================================

// build_idle_nodes:一个 worker 有 rollout_path(可读)→ classifier.classify 被调一次;
// 经注入分类器(零 provider client)。stopped/paused 跳过。
#[test]
fn build_idle_nodes_uses_injected_classifier_no_provider_client() {
    // 真实 session 文件,使 _read_session_tail 读到非空 → classifier 返回注入 state。
    let dir = std::env::temp_dir().join(format!("ta_rs_idle_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("w1.jsonl");
    std::fs::write(&log, b"{\"type\":\"turn_complete\"}\n").unwrap();
    let state = serde_json::json!({
        "agents": {
            "w1": {"provider": "codex", "rollout_path": log.to_string_lossy(), "status": "running"},
            "w_stopped": {"provider": "codex", "rollout_path": log.to_string_lossy(), "status": "stopped"},
        }
    });
    let clf = CountingClassifier::new(TurnState::Idle);
    let nodes = build_idle_nodes(&state, &clf).unwrap();
    // stopped 被跳过(__init__ wiring:29)→ 仅 w1。
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "w1");
    assert_eq!(nodes[0].role, NodeRole::Worker);
    assert_eq!(nodes[0].state, TurnState::Idle);
    // MUST-NOT-13:分类只经注入 classifier(此 mock 计数==1),零 provider client 直连。
    assert_eq!(clf.calls.get(), 1, "每个 live node 恰调一次注入 classify");
}

// bug-085:rollout_path=None → 读到空串 → Unknown(不当 idle)。
#[test]
fn build_idle_nodes_none_rollout_path_yields_unknown_not_idle() {
    let state = serde_json::json!({
        "agents": {
            "w1": {"provider": "codex", "status": "running"} // 无 rollout_path。
        }
    });
    // 即使 mock 默认想返 Idle,空 session-log → classifier 返 Unknown(见 mock 逻辑)。
    let clf = CountingClassifier::new(TurnState::Idle);
    let nodes = build_idle_nodes(&state, &clf).unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes[0].state,
        TurnState::Unknown,
        "None rollout_path → Unknown,绝不 idle"
    );
    assert!(
        !nodes[0].state.is_idle_for_takeover(),
        "unknown ≠ idle:不得对 takeover 放行"
    );
}

// _leader_node:leader path 或 provider 缺 → None(省略而非猜 idle)。
#[test]
fn leader_node_omitted_when_path_or_provider_missing() {
    let clf = CountingClassifier::new(TurnState::Idle);
    // 既无 leader.rollout_path 也无 receiver.rollout_path → None。
    let state_no_path = serde_json::json!({
        "leader": {"id": "leader", "provider": "codex"},
        "leader_receiver": {"provider": "codex"}
    });
    assert!(
        leader_node(&state_no_path, &clf).unwrap().is_none(),
        "缺 path → 省略 leader 节点"
    );
    // 有 path 但无 provider → None。
    let dir = std::env::temp_dir().join(format!("ta_rs_lnode_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("leader.jsonl");
    std::fs::write(&log, b"{\"type\":\"turn_open\"}\n").unwrap();
    let state_no_provider = serde_json::json!({
        "leader": {"id": "leader", "rollout_path": log.to_string_lossy()}
    });
    assert!(
        leader_node(&state_no_provider, &clf).unwrap().is_none(),
        "缺 provider → 省略 leader 节点(不猜 idle)"
    );
}

// _leader_node:path+provider 齐 → 经 classifier 产 role=leader 节点(C13)。
#[test]
fn leader_node_classified_via_injected_classifier() {
    let dir = std::env::temp_dir().join(format!("ta_rs_lnode2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("leader.jsonl");
    std::fs::write(&log, b"{\"type\":\"turn_complete\"}\n").unwrap();
    let state = serde_json::json!({
        "leader": {"id": "leader", "provider": "codex", "rollout_path": log.to_string_lossy()}
    });
    let clf = CountingClassifier::new(TurnState::Working);
    let node = leader_node(&state, &clf)
        .unwrap()
        .expect("path+provider 齐 → 有 leader 节点");
    assert_eq!(node.role, NodeRole::Leader);
    assert_eq!(node.node_id, "leader");
    assert_eq!(node.state, TurnState::Working);
    assert_eq!(
        clf.calls.get(),
        1,
        "leader 分类经注入 classifier 一次,零 provider client"
    );
}

// =====================================================================
// 9. classify_provider_turn_state 门面(unimplemented → RED)
//    unknown/abnormal 且有 event_sink → 写 idle_takeover.classify。
// =====================================================================

// 门面经注入 classifier 分类;空文本 → Unknown(本 mock),验证返回 TurnClassification。
#[test]
fn classify_provider_turn_state_returns_classification_via_injected_classifier() {
    let clf = CountingClassifier::new(TurnState::Idle);
    let c =
        classify_provider_turn_state(Provider::Codex, "{\"type\":\"turn_complete\"}", &clf, None)
            .unwrap();
    assert_eq!(c.state, TurnState::Idle);
    assert_eq!(clf.calls.get(), 1);
    // 空文本 → Unknown(unknown ≠ idle 命门下游)。
    let clf2 = CountingClassifier::new(TurnState::Idle);
    let c2 = classify_provider_turn_state(Provider::Codex, "", &clf2, None).unwrap();
    assert_eq!(c2.state, TurnState::Unknown);
    assert!(!c2.state.is_idle_for_takeover());
}

// event_sink + unknown/abnormal → 写 idle_takeover.classify(事件名字节锁)。
#[test]
fn classify_event_name_is_idle_takeover_classify() {
    assert_eq!(
        LeaderEvent::IdleTakeoverClassify.name(),
        "idle_takeover.classify"
    );
}

// =====================================================================
// 10. push_idle_reminder(unimplemented → RED):!should_ping → no-op。
// =====================================================================

#[test]
fn push_idle_reminder_noop_when_should_not_ping() {
    let ws = std::env::temp_dir().join(format!("ta_rs_push_{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let event_log = crate::event_log::EventLog::new(&ws);
    let state = serde_json::json!({"leader": {"id": "leader"}});
    let result = TakeoverReminderResult {
        should_ping: false,
        message: None,
        interrupted_nodes: vec![],
        reason: Some("not_armed_no_worker_turn".into()),
    };
    // should_ping=false → no-op,返回 Ok(())。现 unimplemented → RED。
    push_idle_reminder(&ws, &state, &event_log, &result).unwrap();
    // 强化:no-op 必须真的什么都不做 —— 不写 idle_takeover.reminder 事件(EventLog 无该事件)。
    let events = event_log.tail(50).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == serde_json::json!("idle_takeover.reminder")),
        "should_ping=false 时绝不写 reminder 事件"
    );
}

// #236 nag_removal (N35) — push_idle_reminder is now a no-op shim.
// [OLD] assertion: should_ping=true → writes idle_takeover.reminder event (with
// interrupted/reason byte-locked golden payload).
// [NEW] assertion: even when should_ping=true, push_idle_reminder writes NO event
// and emits NO leader-bound message; ownership/handover happens only via explicit
// `claim-leader` / `takeover` commands. The function signature is preserved so
// existing callers (coordinator/tick.rs, lifecycle wiring) still resolve.
#[test]
fn push_idle_reminder_is_silent_no_op_under_n35_even_when_should_ping_true() {
    let ws = std::env::temp_dir().join(format!("ta_rs_push2_{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let event_log = crate::event_log::EventLog::new(&ws);
    let state = serde_json::json!({"leader": {"id": "leader"}});
    let result = TakeoverReminderResult {
        should_ping: true,
        message: Some("neutral reminder body".into()),
        interrupted_nodes: vec!["w1".into()],
        reason: Some("armed_all_idle".into()),
    };
    push_idle_reminder(&ws, &state, &event_log, &result).unwrap();
    let events = event_log.tail(50).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == serde_json::json!("idle_takeover.reminder")),
        "#236 N35: push_idle_reminder must no longer emit the reminder nag event; got {events:?}"
    );
}

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
