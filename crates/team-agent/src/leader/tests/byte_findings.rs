use super::*;

    #[test]
    #[serial_test::serial(env)]
    fn d1_claim_team_owner_includes_os_user() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d1_owner_keys");
        let mut state = serde_json::json!({"session_name": "team-agent-x"});
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%5"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%5"])).unwrap();
        assert!(r.ok && r.status == LeaseStatus::Claimed, "precondition: vacant acquire");
        let owner = state["team_owner"].as_object().expect("team_owner object");
        assert!(owner.contains_key("os_user"),
            "golden claim-path team_owner carries os_user even when empty; keys={:?}", owner.keys().collect::<Vec<_>>());
        let keys: Vec<&str> = owner.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["pane_id","provider","machine_fingerprint","leader_session_uuid","owner_epoch","claimed_at","claimed_via","os_user"],
            "golden new_owner includes os_user");
    }

    // D2 [BLOCK] — claim-path leader_receiver is golden's 15 keys in golden order; NO
    // fingerprint/requested_provider/warning. Golden _receiver_from_claim_target (__init__.py:861-877).
    // Rust LeaderReceiver serializes all 17 (no skip_serializing_if) -> 3 always-null extras leak. RED.
    // (The POPULATED tmux values session_name/window_*/pane_* come from the caller-target scan — a
    // deferred real-tmux seam, see d2_receiver_populated_from_caller_target_seam; the KEY-SET + ORDER
    // locked here are unchanged by that scan.)
    #[test]
    #[serial_test::serial(env)]
    fn d2_claim_leader_receiver_is_fifteen_golden_keys_in_order_no_extras() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TMUX", Some("/tmp/tmux-501/default,123,0")),
        ]);
        let ws = p2_temp_ws("d2_recv_keys");
        let mut state = serde_json::json!({"session_name": "team-agent-x"});
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%5"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%5"])).unwrap();
        assert!(r.ok && r.status == LeaseStatus::Claimed, "precondition: vacant acquire");
        let recv = state["leader_receiver"].as_object().expect("leader_receiver object");
        for extra in ["fingerprint", "requested_provider", "warning"] {
            assert!(!recv.contains_key(extra),
                "golden leader_receiver has NO '{extra}' key (Rust must add skip_serializing_if); keys={:?}", recv.keys().collect::<Vec<_>>());
        }
        let keys: Vec<&str> = recv.keys().map(String::as_str).collect();
        assert_eq!(keys, vec![
            "mode","status","provider","pane_id","session_name","window_index","window_name",
            "pane_index","pane_tty","pane_current_command","tmux_socket","leader_session_uuid",
            "owner_epoch","attached_at","discovery",
        ], "golden _receiver_from_claim_target 15-key set + ORDER (__init__.py:861-877 + BUG-4 socket-qualified receiver)");
    }

    // D2 seam — the caller-target SCAN that fills session_name/window_index/window_name/pane_index/
    // pane_tty/pane_current_command from core_list_targets (golden _receiver_from_claim_target reads
    // target[...]). Rust make_receiver leaves them None (no scan). Real-tmux: needs core_list_targets.
    #[test]
    #[ignore = "real-tmux seam: leader_receiver session_name/window_*/pane_* are populated from the \
                caller target via core_list_targets (golden _receiver_from_claim_target); Rust has no \
                scan (values null). Porter wires the target scan; this asserts the populated values."]
    fn d2_receiver_populated_from_caller_target_seam() {
        // Golden contract: new_receiver.session_name == caller_target.session_name, etc. Needs a live
        // tmux target list (core_list_targets) — out of in-process scope.
    }

    // D3 [BLOCK] — bound_pane is RECEIVER-first (golden :624 receiver.pane_id OR owner.pane_id). PROBE-B
    // (receiver=%9, owner=%1, caller=%9): golden ok=True already_bound. Rust derives OWNER-first
    // (owner %1 != caller %9) -> owner live + !confirm -> refused force_confirm_required. RED.
    #[test]
    #[serial_test::serial(env)]
    fn d3_bound_pane_receiver_first_reclaim_from_receiver_pane_is_already_bound() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d3_receiver_first");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":3,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%9","owner_epoch":3,"leader_session_uuid":"U"},
        });
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%9"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%9","%1"])).unwrap();
        assert!(r.ok, "a re-claim from the bound RECEIVER pane (%9) is idempotent ok=true; got {r:?}");
        assert_eq!(r.status, LeaseStatus::AlreadyBound,
            "golden :624 receiver-first: bound_pane=receiver(%9)==caller(%9) -> already_bound, NOT force_confirm");
    }

    // D4 [WARN] — precheck_epoch uses Python truthiness: owner_epoch=0 is FALSY -> falls through to
    // receiver_epoch=5 (golden _lease_epoch :400 int(owner.epoch or recv.epoch or 0)). Rust
    // current_owner_epoch Some(0) stops the chain -> 0. Surfaces in the force_confirm refusal. RED.
    #[test]
    #[serial_test::serial(env)]
    fn d4_precheck_epoch_zero_is_falsy_falls_through_to_receiver() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d4_epoch_falsy");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":0,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%1","owner_epoch":5,"leader_session_uuid":"U"},
        });
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%2"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%1","%2"])).unwrap();
        assert_eq!(r.status, LeaseStatus::Refused, "precondition: live owner %1 + no confirm -> force_confirm");
        assert_eq!(r.owner_epoch, Some(OwnerEpoch(5)),
            "golden _lease_epoch: owner_epoch=0 is falsy -> receiver_epoch=5; Rust stops at Some(0)");
    }

    // D5 [BLOCK] — success path emits leader_receiver.rebind_applied + owner_epoch_advanced (golden
    // :712-729), NOT the incident-arm leader_receiver.claim_applied (:791). RED.
    #[test]
    #[serial_test::serial(env)]
    fn d5_success_emits_rebind_applied_and_owner_epoch_advanced_not_claim_applied() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d5_events");
        let mut state = serde_json::json!({"session_name": "team-agent-x"});
        let _ = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%5"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%5"])).unwrap();
        let names = event_names(&ws);
        assert!(names.iter().any(|n| n == "leader_receiver.rebind_applied"),
            "golden success emits leader_receiver.rebind_applied (:712); got {names:?}");
        assert!(names.iter().any(|n| n == "owner_epoch_advanced"),
            "golden success emits owner_epoch_advanced (:721); got {names:?}");
        assert!(!names.iter().any(|n| n == "leader_receiver.claim_applied"),
            "claim_applied is the INCIDENT-arm event, NEVER on the no-incident path; got {names:?}");
    }

    // D6 [WARN] — EVERY lease refusal writes a leader_receiver audit event (golden _emit_lease_refusal).
    // not_in_tmux_pane ∈ _LEASE_REBIND_REQUIRED_REASONS -> "leader_receiver.rebind_required" (:481).
    // Rust refused() takes no event_log -> writes nothing. RED.
    #[test]
    #[serial_test::serial(env)]
    fn d6_refusal_emits_leader_receiver_audit_event() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d6_refusal_event");
        let mut state = serde_json::json!({});
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new(""), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&[])).unwrap();
        assert_eq!(r.reason, Some(LeaseReason::NotInTmuxPane), "precondition: empty caller -> not_in_tmux_pane");
        let names = event_names(&ws);
        assert!(names.iter().any(|n| n == "leader_receiver.rebind_required"),
            "golden writes a leader_receiver.rebind_required audit on the not_in_tmux_pane refusal; Rust writes none. got {names:?}");
    }

    // D8 [WARN] — provider PRESERVED from the prior receiver on re-claim (golden
    // _receiver_from_claim_target provider=previous.provider or 'codex'; new_owner=new_recv.provider or
    // owner.provider or 'codex'). A dead-owner recover of a CLAUDE leader keeps provider='claude'. Rust
    // make_receiver/make_owner hardcode Provider::Codex. RED.
    #[test]
    #[serial_test::serial(env)]
    fn d8_provider_preserved_from_prior_receiver_on_reclaim() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("d8_provider");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"claude","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":3,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%1","provider":"claude","owner_epoch":3,"leader_session_uuid":"U"},
        });
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%5"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%5"])).unwrap();
        assert!(r.ok && r.status == LeaseStatus::Claimed, "precondition: dead-owner %1 recover");
        assert_eq!(state.pointer("/team_owner/provider").and_then(|v| v.as_str()), Some("claude"),
            "golden preserves the prior provider (claude) on re-claim; Rust hardcodes codex");
        assert_eq!(state.pointer("/leader_receiver/provider").and_then(|v| v.as_str()), Some("claude"),
            "the new receiver also preserves the prior provider (golden previous.provider)");
    }

    // TOCTOU CAS [BLOCK, flagged] — golden :654-671 re-reads owner_epoch INSIDE the lock and refuses
    // owner_epoch_advanced on a concurrent bump (no double-bind). Rust has NO locked re-read; the race
    // silently double-binds (LeaseReason::OwnerEpochAdvanced is unreachable). Simulated: on-disk
    // state.json (the locked-re-read source) carries epoch 5 while the passed precheck state is epoch 3.
    // RED: today Rust ignores disk and claims. Porter: re-read select_runtime_state under the lock and
    // refuse owner_epoch_advanced when locked_epoch != precheck_epoch.
    #[test]
    #[serial_test::serial(env)]
    fn toctou_locked_reread_refuses_owner_epoch_advanced_on_concurrent_bump() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("toctou_cas");
        // disk (the locked-re-read source): a concurrent claimer bumped owner_epoch to 5.
        crate::state::persist::save_runtime_state(&ws, &serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":5,"claimed_at":"t","claimed_via":"claim-leader"},
        })).unwrap();
        // precheck state passed to the claim: epoch 3, vacant (no owner pane) -> no force_confirm gate.
        let mut state = serde_json::json!({"session_name":"team-agent-x","leader_receiver":{"owner_epoch":3}});
        let r = claim_lease_no_incident(&ws, &mut state, None, &TeamKey::new("current"),
            &PaneId::new("%5"), false, &crate::event_log::EventLog::new(&ws), &seeded_liveness(&["%5"])).unwrap();
        assert_eq!(r.status, LeaseStatus::Refused,
            "golden TOCTOU CAS: locked epoch 5 != precheck 3 -> refused; Rust skips the re-read and claims. got {r:?}");
        assert_eq!(r.reason, Some(LeaseReason::OwnerEpochAdvanced),
            "the refusal reason is owner_epoch_advanced (:666)");
    }

    // attach_leader_to_state [BLOCK, flagged] — golden non-first-time path writes leader_receiver ONLY:
    // NO team_owner write, NO epoch advance (after the strict-uuid gate). Rust stub writes BOTH and
    // advances epoch (precheck+1), ignoring source/require_current. RED (locked before the autobind/launch
    // wiring lands, so it does not silently overwrite the owner + bump epoch on every attach).
    #[test]
    #[serial_test::serial(env)]
    fn attach_leader_to_state_nonfirst_does_not_overwrite_owner_or_advance_epoch() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("attach_nonfirst");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":3,"claimed_at":"t","claimed_via":"claim-leader"},
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        let _ = attach_leader_to_state(&ws, &mut state, Some(&PaneId::new("%2")),
            crate::provider::Provider::Codex, &event_log, LeaseSource::Manual, false);
        assert_eq!(state.pointer("/team_owner/pane_id").and_then(|v| v.as_str()), Some("%1"),
            "non-first-time attach must NOT overwrite team_owner.pane_id (golden writes leader_receiver only); Rust rebinds it to %2");
        assert_eq!(state.pointer("/team_owner/owner_epoch").and_then(|v| v.as_u64()), Some(3),
            "non-first-time attach must NOT advance owner_epoch (golden keeps 3); Rust bumps to 4");
    }

    // C3 [WARN] — leader_identity.current_pane_id honors TEAM_AGENT_LEADER_PANE_ID priority (golden
    // :366 = env(TEAM_AGENT_LEADER_PANE_ID) or env(TMUX_PANE) or None). Rust owner_bind.rs:32 reads
    // TMUX_PANE only. RED: with TEAM_AGENT_LEADER_PANE_ID set + TMUX_PANE unset, current_pane_id must be
    // the LEADER_PANE_ID. (Sibling gap, noted: last_seen_at = receiver.attached_at OR receiver.last_seen_at;
    // Rust reads attached_at only.)
    #[test]
    #[serial_test::serial(env)]
    fn c3_leader_identity_current_pane_honors_leader_pane_id_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TEAM_AGENT_LEADER_PANE_ID", Some("%foo")),
            ("TMUX_PANE", None),
        ]);
        let ws = p2_temp_ws("c3_pane_env");
        let v = leader_identity(&ws, None).unwrap();
        assert_eq!(v["current_pane_id"], serde_json::json!("%foo"),
            "golden :366 current_pane_id = TEAM_AGENT_LEADER_PANE_ID or TMUX_PANE or None; Rust reads TMUX_PANE only");
    }
