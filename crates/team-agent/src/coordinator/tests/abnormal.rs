use super::*;

// ═════════════════════════════════════════════════════════════════════════
// GROUP E — abnormal_track classify / dedup (abnormal_track.py) — RED
//   provider-neutral:断言 ZERO adapter_for 调用(MUST-NOT-13)。
//   ASSUMPTION:传入 `records` 是 provider reader 已产出的结构化 fault facts
//   ({signature, turn_id?, kind?, raw}),即 read_fault_facts 的输出形;abnormal_track
//   只对它做 catch-bias + (signature, turn_id|fingerprint) 去重(error_lists 来自注入
//   registry,绝不经 adapter_for 触碰 provider client)。
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn abnormal_default_notify_for_structured_fault() {
    // abnormal_track.py:204 — C9 catch-bias: 无黑白名单命中 → notify_default.
    let reg = MockRegistry::new(&[], &[]);
    let records = vec![serde_json::json!({"raw": "session line"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1);
    assert_eq!(out.notifications[0].decision, AbnormalDecision::NotifyDefault);
    // §84:绝不触碰 provider adapter client。
    assert_eq!(reg.adapter_calls.get(), 0, "MUST NOT call adapter_for (provider client)");
}

#[test]
fn abnormal_blacklist_hit_yields_notify_blacklist() {
    // abnormal_track.py:203 — blacklist 命中 → notify_blacklist.
    let reg = MockRegistry::new(&[], &["boom"]);
    let records = vec![serde_json::json!({"raw": "kaboom: boom happened"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1);
    assert_eq!(out.notifications[0].decision, AbnormalDecision::NotifyBlacklist);
    assert_eq!(reg.adapter_calls.get(), 0);
}

#[test]
fn abnormal_whitelist_beats_blacklist_and_skips() {
    // abnormal_track.py:200-201 — whitelist > blacklist > default. 同命中 → skip, 0 notify.
    let reg = MockRegistry::new(&["boom"], &["boom"]);
    let records = vec![serde_json::json!({"raw": "boom transient"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 0, "whitelist wins → skip (probe confirmed)");
    assert_eq!(reg.adapter_calls.get(), 0);
}

#[test]
fn abnormal_approval_kind_maps_to_blocked_on_human() {
    // abnormal_track.py:74 — kind=="approval" → state blocked_on_human, else abnormal.
    let reg = MockRegistry::new(&[], &[]);
    let records = vec![serde_json::json!({"raw": "approve?", "kind": "approval"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1);
    assert_eq!(out.notifications[0].state, TurnState::BlockedOnHuman);
}

#[test]
fn abnormal_same_turn_id_folds_to_one_notification() {
    // abnormal_track.py:60-68 — C8 dedup: same (signature, turn_id) → one notify.
    let reg = MockRegistry::new(&[], &[]);
    let records = vec![
        serde_json::json!({"signature": "sig_a", "turn_id": "t1", "raw": "boom"}),
        serde_json::json!({"signature": "sig_a", "turn_id": "t1", "raw": "boom"}),
    ];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1, "retry-loop in SAME turn folds (probe: 1)");
    // seen 状态回存,key = "sig_a\0t1"。
    assert!(out.notification_state.seen.contains("sig_a\u{0}t1"));
}

#[test]
fn abnormal_missing_turn_id_distinct_faults_each_notify() {
    // abnormal_track.py:64 — turn_id None → per-record content fingerprint bucket:
    // 不同 fault 各自 notify(绝不折叠进全局桶,probe: 2)。
    let reg = MockRegistry::new(&[], &[]);
    let records = vec![
        serde_json::json!({"signature": "sig_b", "raw": "alpha"}),
        serde_json::json!({"signature": "sig_b", "raw": "beta"}),
    ];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 2, "distinct raw → 2 notifies (probe)");
}

#[test]
fn abnormal_missing_turn_id_identical_faults_fold() {
    // abnormal_track.py:64-68 — identical raw + missing turn_id → same fingerprint → fold (probe: 1).
    let reg = MockRegistry::new(&[], &[]);
    let records = vec![
        serde_json::json!({"signature": "sig_c", "raw": "same"}),
        serde_json::json!({"signature": "sig_c", "raw": "same"}),
    ];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1, "identical raw folds even w/o turn_id (probe)");
}

#[test]
fn abnormal_preseeded_seen_suppresses_duplicate_across_calls() {
    // abnormal_track.py:67 — key already in seen → skip notification (cross-tick dedup).
    let reg = MockRegistry::new(&[], &[]);
    let mut state = AbnormalNotificationState::default();
    state.seen.insert("sig_a\u{0}t1".to_string());
    let records = vec![serde_json::json!({"signature": "sig_a", "turn_id": "t1", "raw": "boom"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &state,
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 0, "pre-seeded seen suppresses (probe: 0)");
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP F — detect_whole_team_gone (abnormal_track.py:91) — clean vs unexpected — RED
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn whole_team_gone_alive_when_any_present() {
    // abnormal_track.py:117-127 — any alive → not whole_gone, silent.
    let snap = TeamPresenceSnapshot {
        coordinator_alive: true,
        leader_alive: false,
        provider_processes_alive: vec![false],
        tmux_sessions_present: false,
        clean_shutdown: false,
        restart_in_progress: false,
    };
    let mut ms = MapMarkerStore::ok();
    let r = detect_whole_team_gone(&snap, &mut ms);
    assert!(!r.whole_team_gone);
    assert_eq!(r.classification, WholeTeamGoneClass::Alive);
    assert!(!r.notify);
    assert!(!r.marker_written);
    assert!(ms.markers.is_empty());
}

#[test]
fn whole_team_gone_clean_shutdown_is_silent() {
    // abnormal_track.py:129-130 — clean_shutdown → silent, NO marker.
    let snap = TeamPresenceSnapshot {
        coordinator_alive: false,
        leader_alive: false,
        provider_processes_alive: vec![false],
        tmux_sessions_present: false,
        clean_shutdown: true,
        restart_in_progress: false,
    };
    let mut ms = MapMarkerStore::ok();
    let r = detect_whole_team_gone(&snap, &mut ms);
    assert!(r.whole_team_gone);
    assert_eq!(r.classification, WholeTeamGoneClass::CleanShutdown);
    assert!(!r.notify);
    assert!(!r.escalate_user_on_next_leader_command);
    assert!(!r.marker_written);
    assert!(ms.markers.is_empty(), "clean shutdown writes NO durable marker");
}

#[test]
fn whole_team_gone_restart_in_progress_is_silent() {
    // abnormal_track.py:131-132 — restart_in_progress → silent.
    let snap = TeamPresenceSnapshot {
        coordinator_alive: false,
        leader_alive: false,
        provider_processes_alive: vec![false],
        tmux_sessions_present: false,
        clean_shutdown: false,
        restart_in_progress: true,
    };
    let mut ms = MapMarkerStore::ok();
    let r = detect_whole_team_gone(&snap, &mut ms);
    assert!(r.whole_team_gone);
    assert_eq!(r.classification, WholeTeamGoneClass::RestartInProgress);
    assert!(!r.notify);
    assert!(!r.marker_written);
}

#[test]
fn whole_team_gone_unexpected_writes_marker_and_defers_escalation() {
    // abnormal_track.py:134-147 — 闪退: durable marker + deferred escalation, notify.
    let snap = TeamPresenceSnapshot {
        coordinator_alive: false,
        leader_alive: false,
        provider_processes_alive: vec![false, false],
        tmux_sessions_present: false,
        clean_shutdown: false,
        restart_in_progress: false,
    };
    let mut ms = MapMarkerStore::ok();
    let r = detect_whole_team_gone(&snap, &mut ms);
    assert!(r.whole_team_gone);
    assert_eq!(r.classification, WholeTeamGoneClass::UnexpectedExit);
    assert!(r.notify);
    assert!(r.escalate_user_on_next_leader_command, "defer to next leader command");
    assert!(r.marker_written);
    assert!(ms.markers.contains_key("whole_team_gone"), "durable marker named whole_team_gone");
}

#[test]
fn whole_team_gone_unexpected_marker_failure_reflected() {
    // abnormal_track.py:146 — marker_written = bool(_marker_set(...)); 落盘失败 → false,
    // 但仍 notify + escalate(分类不变)。
    let snap = TeamPresenceSnapshot {
        coordinator_alive: false,
        leader_alive: false,
        provider_processes_alive: vec![false],
        tmux_sessions_present: false,
        clean_shutdown: false,
        restart_in_progress: false,
    };
    let mut ms = MapMarkerStore::failing();
    let r = detect_whole_team_gone(&snap, &mut ms);
    assert_eq!(r.classification, WholeTeamGoneClass::UnexpectedExit);
    assert!(r.notify);
    assert!(!r.marker_written, "marker set failed → marker_written false");
}
