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

#[test]
fn abnormal_explicit_error_classifier_boundaries_stay_stable() {
    let claude_api = line(serde_json::json!({
        "type": "system",
        "subtype": "api_error",
        "level": "error",
        "sessionId": "sess-1"
    }));
    let claude_api_fact =
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &claude_api)
            .expect("Claude system api_error level=error counts as explicit");
    assert_eq!(claude_api_fact.signature.as_str(), "api_error");
    assert_eq!(
        claude_api_fact.turn_id.as_ref().map(|id| id.as_str()),
        Some("sess-1")
    );
    assert_eq!(claude_api_fact.api_error_status, None);
    assert_eq!(claude_api_fact.error.as_deref(), None);
    assert_eq!(claude_api_fact.request_id.as_deref(), None);
    assert_eq!(claude_api_fact.assistant_uuid.as_deref(), None);

    let claude_400 = line(serde_json::json!({
        "type": "assistant",
        "parentUuid": "parent-400",
        "uuid": "assistant-400",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "API Error: 400 Team Agent truthsource forced invalid_request_error"}
        ]},
        "error": "unknown",
        "isApiErrorMessage": true,
        "apiErrorStatus": 400,
        "sessionId": "session-400",
        "version": "2.1.181"
    }));
    let claude_400_fact =
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &claude_400)
            .expect("Claude assistant api error counts as explicit");
    assert_claude_api_error_fact(
        &claude_400_fact,
        "assistant-400",
        Some(400),
        Some("unknown"),
        None,
        Some("assistant-400"),
    );

    let claude_404 = line(serde_json::json!({
        "type": "assistant",
        "parentUuid": "parent-404",
        "uuid": "assistant-404",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "There's an issue with the selected model."}
        ]},
        "error": "model_not_found",
        "isApiErrorMessage": true,
        "apiErrorStatus": 404,
        "sessionId": "session-404",
        "version": "2.1.181"
    }));
    let claude_404_fact =
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &claude_404)
            .expect("Claude assistant model_not_found counts as explicit");
    assert_claude_api_error_fact(
        &claude_404_fact,
        "assistant-404",
        Some(404),
        Some("model_not_found"),
        None,
        Some("assistant-404"),
    );

    let claude_real_request = line(serde_json::json!({
        "type": "assistant",
        "parentUuid": "parent-real",
        "uuid": "assistant-real",
        "requestId": "req_011CceNfWj2aPY5gtCdakULt",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "There's an issue with the selected model."}
        ]},
        "error": "model_not_found",
        "isApiErrorMessage": true,
        "apiErrorStatus": 404,
        "sessionId": "session-real",
        "version": "2.1.181"
    }));
    let claude_real_fact =
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &claude_real_request)
            .expect("Claude assistant api error with requestId counts as explicit");
    assert_claude_api_error_fact(
        &claude_real_fact,
        "assistant-real",
        Some(404),
        Some("model_not_found"),
        Some("req_011CceNfWj2aPY5gtCdakULt"),
        Some("assistant-real"),
    );

    let claude_tool_result = line(serde_json::json!({
        "type": "user",
        "message": {
            "content": [
                {"type": "tool_result", "is_error": true, "content": "permission denied"}
            ]
        }
    }));
    assert!(
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &claude_tool_result)
            .is_none(),
        "Claude tool_result is_error is an abnormal fact, but not an explicit provider-exit error"
    );

    let ordinary_assistant_text = line(serde_json::json!({
        "type": "assistant",
        "uuid": "assistant-text",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "The command returned an error string."}
        ]},
        "sessionId": "session-text"
    }));
    assert!(
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &ordinary_assistant_text)
            .is_none(),
        "ordinary assistant text containing error is not a structured API error"
    );

    let interrupted = line(serde_json::json!({
        "type": "user",
        "uuid": "user-interrupt",
        "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}
    }));
    assert!(
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &interrupted).is_none(),
        "user interruption is not a provider API error"
    );

    let empty_api_error_shape = line(serde_json::json!({
        "type": "assistant",
        "uuid": "assistant-empty",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "API Error:"}
        ]},
        "isApiErrorMessage": true,
        "sessionId": "session-empty"
    }));
    assert!(
        crate::provider::latest_explicit_error_fact(Provider::ClaudeCode, &empty_api_error_shape)
            .is_none(),
        "isApiErrorMessage without status/error/requestId is not enough"
    );

    let codex_failed = line(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": {"turn": {"id": "turn-1", "status": "failed"}}
    }));
    let codex_fact = crate::provider::latest_explicit_error_fact(Provider::Codex, &codex_failed)
        .expect("Codex failed turn/completed counts as explicit");
    assert_eq!(codex_fact.signature.as_str(), "turn_failed");
    assert_eq!(
        codex_fact.turn_id.as_ref().map(|id| id.as_str()),
        Some("turn-1")
    );

    let non_error = line(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": {"turn": {"id": "turn-2", "status": "completed"}}
    }));
    assert!(
        crate::provider::latest_explicit_error_fact(Provider::Codex, &non_error).is_none(),
        "non-error tail must not count as latest explicit error"
    );
}

fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

fn assert_claude_api_error_fact(
    fact: &crate::provider::FaultFact,
    turn_id: &str,
    status: Option<i64>,
    error: Option<&str>,
    request_id: Option<&str>,
    assistant_uuid: Option<&str>,
) {
    assert_eq!(fact.signature.as_str(), "api_error");
    assert_eq!(fact.kind, crate::provider::FactKind::Error);
    assert_eq!(fact.turn_id.as_ref().map(|id| id.as_str()), Some(turn_id));
    assert_eq!(fact.api_error_status, status);
    assert_eq!(fact.error.as_deref(), error);
    assert_eq!(fact.request_id.as_deref(), request_id);
    assert_eq!(fact.assistant_uuid.as_deref(), assistant_uuid);
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
