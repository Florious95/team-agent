use super::*;

    // = RED. The porter must reproduce the EXACT LeaseResult shape per outcome.
    // We seed state.json directly and inject a PaneLivenessProbe so no real tmux
    // is needed for the no-pane / live-owner / dead-owner gate decisions.

    /// Injectable liveness probe (`state::owner_gate::PaneLivenessProbe`): a pane
    /// in the seeded set is `Live`, otherwise `Dead`. The porter's real probe
    /// (step 9 tmux) replaces it; here it drives the no-incident lease gate
    /// (bound-pane liveness + caller eligibility) deterministically with no tmux.
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_vacant_acquire_advances_epoch_and_dual_writes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_vacant");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        // empty (vacant) state with a session so dual-state writes both files.
        let mut state = serde_json::json!({"session_name": "team-agent-x"});
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&["%5"]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();
        assert!(r.ok);
        assert_eq!(r.status, LeaseStatus::Claimed);
        assert_eq!(r.reason, Some(LeaseReason::VacantAcquired));
        assert_eq!(r.owner_epoch, Some(OwnerEpoch(1)), "vacant acquire 0→1");
        let owner = r.owner.as_ref().expect("claimed → owner");
        assert_eq!(owner.pane_id, caller);
        assert_eq!(owner.owner_epoch, OwnerEpoch(1));
        assert_eq!(owner.claimed_via, ClaimedVia::ClaimLeader);
        let receiver = r.receiver.as_ref().expect("claimed → receiver");
        assert_eq!(receiver.pane_id, caller);
        assert_eq!(receiver.discovery, Some(Discovery::ClaimLeader));
        // Stage 3d (identity-boundary unified plan, architect direction
        // 2026-06-23): canonical owner now lives at teams.<team_key>; the
        // top-level dual-write was dropped. Read through the ownership
        // repository which preserves the legacy precedence for callers
        // that still consult top-level on migration states.
        let ws_path = crate::state::persist::runtime_state_path(&ws);
        let ws_state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ws_path).unwrap()).unwrap();
        // The canonical team_key is what `team_state_key(state)` derives
        // (here: state's session_name "team-agent-x" since no explicit
        // team_key/team_dir/spec_path was seeded). The test's `team_id`
        // parameter is the LOOKUP key but `claim_lease_no_incident` uses
        // the state-derived key for canonical writes. Stage 5 will unify
        // these via TeamRuntimePaths.
        let canonical_key = crate::state::projection::team_state_key(&ws_state);
        let ws_owner =
            crate::state::ownership::read_owner_value(&ws_state, &canonical_key)
                .expect("Stage 3d: vacant acquire writes canonical owner");
        assert_eq!(ws_owner["pane_id"], serde_json::json!("%5"));
        assert_eq!(ws_owner["owner_epoch"], serde_json::json!(1));
        // team_id parameter is currently unused by the canonical write
        // (architect Stage 5 will close this gap); silence the warning.
        let _ = &team_id;
        let snap_path = crate::model::paths::runtime_dir(&ws)
            .join("teams").join("team-agent-x").join("state.json");
        let snap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&snap_path).unwrap()).unwrap();
        // Snapshot is per-session; team_state_key on the snapshot derives
        // from its session_name. Read top-level first (legacy snapshot
        // layout), fall back to derived team key.
        let snap_owner = snap
            .get("team_owner")
            .or_else(|| {
                let key = crate::state::projection::team_state_key(&snap);
                snap.get("teams").and_then(|t| t.get(key)).and_then(|e| e.get("team_owner"))
            })
            .expect("dual-state snapshot owner");
        assert_eq!(snap_owner["pane_id"], serde_json::json!("%5"), "dual-state snapshot owner");
        assert_eq!(snap_owner["owner_epoch"], serde_json::json!(1));
    }

    // RED — DEAD-OWNER RECOVER: bound pane %1 absent from live set, caller %5
    // eligible, confirm=false → still acquires (dead owner never blocks),
    // reason=previous_owner_pane_dead, epoch 3→4. golden /tmp/probe_confirm.py.
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_dead_owner_recovers_without_confirm() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_dead");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"OLDUUID","owner_epoch":3,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%1","owner_epoch":3,"leader_session_uuid":"OLDUUID"},
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        // %1 (the recorded owner) is NOT live; only the caller %5 is live.
        let live = seeded_liveness(&["%5"]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();
        assert!(r.ok);
        assert_eq!(r.status, LeaseStatus::Claimed);
        assert_eq!(r.reason, Some(LeaseReason::PreviousOwnerPaneDead));
        assert_eq!(r.owner_epoch, Some(OwnerEpoch(4)), "dead-owner recover 3→4");
        assert_eq!(r.owner.as_ref().unwrap().pane_id, caller);
    }

    // RED — LIVE-OWNER REFUSAL (no --confirm): bound pane %1 live, caller %5,
    // confirm=false → refused force_confirm_required + action + bound_pane_id=%1
    // + owner_epoch=2 (precheck epoch, NOT advanced). golden /tmp/probe_confirm.py.
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_live_owner_refuses_without_confirm() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_live");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%1","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"OWNERUUID","owner_epoch":2,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%1","owner_epoch":2,"leader_session_uuid":"OWNERUUID"},
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        // both %1 (live owner, matching uuid) and %5 (eligible caller) live.
        let live = seeded_liveness(&["%1", "%5"]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();
        assert!(!r.ok);
        assert_eq!(r.status, LeaseStatus::Refused);
        assert_eq!(r.reason, Some(LeaseReason::ForceConfirmRequired));
        assert_eq!(
            r.action.as_deref(),
            Some("rerun with --confirm to take over the live leader pane")
        );
        assert_eq!(r.bound_pane_id, Some(PaneId::new("%1")), "refusal carries bound pane");
        assert_eq!(r.owner_epoch, Some(OwnerEpoch(2)), "refused → precheck epoch, NOT advanced");
    }

    // RED — ALREADY-BOUND: caller pane == bound pane → status=AlreadyBound,
    // ok=true, owner_epoch unchanged (no advance, no rewrite). golden probe_claimed.py.
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_already_bound_when_caller_is_bound_pane() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_bound");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {"pane_id":"%5","provider":"codex","machine_fingerprint":"fp","leader_session_uuid":"U","owner_epoch":1,"claimed_at":"t","claimed_via":"claim-leader"},
            "leader_receiver": {"pane_id":"%5","owner_epoch":1,"leader_session_uuid":"U"},
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&["%5"]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();
        assert!(r.ok);
        assert_eq!(r.status, LeaseStatus::AlreadyBound);
        assert_eq!(r.owner_epoch, Some(OwnerEpoch(1)), "already-bound → epoch unchanged");
        assert!(r.reason.is_none(), "already-bound path carries no acquire reason");
    }

    #[test]
    #[serial_test::serial(env)]
    fn claim_leader_persists_full_tmux_endpoint_when_tmux_tmpdir_differs_from_coordinator() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let leader_socket = "/tmp/ta-leader-root/tmux-501/dl2f";
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TMUX", Some("/tmp/ta-leader-root/tmux-501/dl2f,12345,0")),
            ("TMUX_TMPDIR", Some("/tmp/ta-coordinator-root")),
        ]);
        let ws = p2_temp_ws("claim_full_endpoint");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({"session_name": "team-agent-x"});
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&["%5"]);

        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();

        assert!(r.ok);
        assert_eq!(r.status, LeaseStatus::Claimed);
        assert_eq!(
            r.receiver.as_ref().and_then(|receiver| receiver.tmux_socket.as_deref()),
            Some(leader_socket),
            "claim-leader must persist the full $TMUX socket path, not only the -L short name"
        );
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(crate::state::persist::runtime_state_path(&ws)).unwrap(),
        )
        .unwrap();
        // Stage 3d: canonical receiver at teams.<team_key>.leader_receiver.
        let team_key = crate::state::projection::team_state_key(&persisted);
        let receiver = &persisted["teams"][&team_key]["leader_receiver"];
        assert_eq!(
            receiver["tmux_socket"],
            serde_json::json!(leader_socket),
            "state.json must carry enough endpoint information for later delivery to use tmux -S"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn claim_leader_already_bound_requires_same_delivery_endpoint_not_only_same_pane_id() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let current_socket = "/tmp/ta-current-leader-root/tmux-501/dl2f";
        let stale_socket = "/tmp/ta-stale-leader-root/tmux-501/dl2f";
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TMUX", Some("/tmp/ta-current-leader-root/tmux-501/dl2f,12345,0")),
            ("TMUX_TMPDIR", Some("/tmp/ta-coordinator-root")),
        ]);
        let ws = p2_temp_ws("claim_bound_endpoint");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {
                "pane_id":"%5",
                "provider":"codex",
                "machine_fingerprint":"fp",
                "leader_session_uuid":"U",
                "owner_epoch":1,
                "claimed_at":"t",
                "claimed_via":"claim-leader",
                "tmux_socket": stale_socket
            },
            "leader_receiver": {
                "pane_id":"%5",
                "owner_epoch":1,
                "leader_session_uuid":"U",
                "tmux_socket": stale_socket
            },
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&["%5"]);

        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();

        assert_ne!(
            r.status,
            LeaseStatus::AlreadyBound,
            "already_bound must verify the same delivery endpoint is reachable; matching bare pane \
             ids are not sufficient because different tmux socket roots can both have %5"
        );
        assert_eq!(
            r.receiver.as_ref().and_then(|receiver| receiver.tmux_socket.as_deref()),
            Some(current_socket),
            "claim should refresh the receiver to the caller's current full tmux endpoint"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn claim_leader_already_bound_normalizes_short_tmux_endpoint_to_full_path() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let current_socket = "/tmp/ta-current-leader-root/tmux-501/dl9aa40c88";
        let short_socket = "dl9aa40c88";
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TMUX", Some("/tmp/ta-current-leader-root/tmux-501/dl9aa40c88,12345,0")),
            ("TMUX_TMPDIR", Some("/tmp/ta-coordinator-root")),
        ]);
        let ws = p2_temp_ws("claim_bound_short_endpoint");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%5");
        let mut state = serde_json::json!({
            "session_name": "team-agent-x",
            "team_owner": {
                "pane_id":"%5",
                "provider":"codex",
                "machine_fingerprint":"fp",
                "leader_session_uuid":"U",
                "owner_epoch":1,
                "claimed_at":"t",
                "claimed_via":"claim-leader",
                "tmux_socket": short_socket
            },
            "leader_receiver": {
                "pane_id":"%5",
                "owner_epoch":1,
                "leader_session_uuid":"U",
                "tmux_socket": short_socket
            },
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&["%5"]);

        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();

        assert_ne!(
            r.status,
            LeaseStatus::AlreadyBound,
            "claim already_bound must not treat a stored short socket name as equivalent to the \
             caller's full $TMUX endpoint by basename; it must refresh/normalize the receiver"
        );
        assert_eq!(
            r.receiver.as_ref().and_then(|receiver| receiver.tmux_socket.as_deref()),
            Some(current_socket),
            "claim should rewrite any legacy short leader_receiver.tmux_socket to the full physical endpoint"
        );
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(crate::state::persist::runtime_state_path(&ws)).unwrap(),
        )
        .unwrap();
        // Stage 3d: canonical receiver at teams.<team_key>.leader_receiver.
        let team_key = crate::state::projection::team_state_key(&persisted);
        let receiver = &persisted["teams"][&team_key]["leader_receiver"];
        assert_eq!(
            receiver["tmux_socket"],
            serde_json::json!(current_socket),
            "state must not preserve a short endpoint after explicit claim; state={persisted}"
        );
    }

    // RED — NOT-IN-TMUX-PANE: empty caller pane → refused not_in_tmux_pane with
    // the EXACT golden action string (differs from the current claim_leader stub
    // string). golden /tmp/probe_claim.py _lease_refused("not_in_tmux_pane",...).
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_empty_caller_refuses_not_in_tmux_pane() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_nopane");
        let team_id = TeamKey::new("current");
        let empty = PaneId::new("");
        let mut state = serde_json::json!({});
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = seeded_liveness(&[]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &empty, false, &event_log, &live,
        )
        .unwrap();
        assert!(!r.ok);
        assert_eq!(r.status, LeaseStatus::Refused);
        assert_eq!(r.reason, Some(LeaseReason::NotInTmuxPane));
        assert_eq!(
            r.action.as_deref(),
            Some("run team-agent claim-leader from the leader's tmux pane"),
            "byte-parity: not_in_tmux_pane action string from _lease_refused (probe_claim.py)"
        );
    }

    // OLD (Python parity, pre-Bug-3): caller pane present but NOT in the live
    // leader-shape set → refused with CallerNotLeaderShaped + eligibility action
    // ("from a leader (claude/codex) tmux pane").
    // NEW (Bug 3 / I-RN-3 / tests/explicit_claim_takeover_any_live_pane_red):
    // the hard leader-shape gate is removed by design. claim_lease_no_incident
    // now accepts any LIVE caller pane (Codex / Claude / any worker shape). The
    // only remaining caller refusal on this no-incident path is "caller pane is
    // not live at all"; with seeded_liveness(&[]) the caller %7 is therefore
    // refused as CallerPaneNotLive (CallerNotLeaderShaped is no longer produced
    // on this code path).
    #[test]
    #[serial_test::serial(env)]
    fn claim_lease_no_incident_caller_not_live_refused_under_n_rn3_any_live_pane() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("claim_notleader");
        let team_id = TeamKey::new("current");
        let caller = PaneId::new("%7");
        let mut state = serde_json::json!({});
        let event_log = crate::event_log::EventLog::new(&ws);
        // Empty live-set → caller %7 is not a live pane at all.
        let live = seeded_liveness(&[]);
        let r = claim_lease_no_incident(
            &ws, &mut state, None, &team_id, &caller, false, &event_log, &live,
        )
        .unwrap();
        assert!(!r.ok);
        assert_eq!(r.status, LeaseStatus::Refused);
        assert_eq!(r.reason, Some(LeaseReason::CallerPaneNotLive));
    }

    // ── 14b. leader_identity dict — workspace_abspath key (golden __init__.py:363) ──
    //
    // LOCK→RED: leader_identity IS implemented (owner_bind.rs:22) but its dict
    // OMITS the golden `workspace_abspath` key (probe_lid.py shows it sits between
    // machine_fingerprint and os_user). This pins the missing key so the porter
    // must add it. Fails today (key absent → null) = RED against the omission.
    #[test]
    #[serial_test::serial(env)]
    fn leader_identity_dict_includes_workspace_abspath_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("lid_wsabs");
        let v = leader_identity(&ws, None).unwrap();
        let ctx = leader_identity_context(&ws, None, Some(&serde_json::json!({}))).unwrap();
        // golden key present + equals the resolved workspace abspath from context.
        assert!(
            v.get("workspace_abspath").is_some(),
            "golden leader_identity dict carries 'workspace_abspath' (probe_lid.py)"
        );
        assert_eq!(
            v["workspace_abspath"].as_str().unwrap(),
            ctx.workspace_abspath.to_string_lossy(),
            "workspace_abspath must equal the context's resolved abspath"
        );
    }

    // LOCK — leader_identity key ORDER & full key set (golden probe_lid.py):
    // [ok, uuid_prefix, machine_fingerprint, workspace_abspath, os_user, team_id,
    //  current_pane_id, last_seen_at, source]. Pins the 9-key surface CLI emits.
    #[test]
    #[serial_test::serial(env)]
    fn leader_identity_dict_has_exact_nine_golden_keys() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
        let ws = p2_temp_ws("lid_keys");
        let v = leader_identity(&ws, None).unwrap();
        let obj = v.as_object().expect("leader_identity → JSON object");
        for key in [
            "ok", "uuid_prefix", "machine_fingerprint", "workspace_abspath",
            "os_user", "team_id", "current_pane_id", "last_seen_at", "source",
        ] {
            assert!(obj.contains_key(key), "golden leader_identity dict must carry '{key}'");
        }
    }

    // ═════════════════════════════════════════════════════════════════════════
    // WAVE-2 Lane B — leader-lease DIVERGENCE round (adversarial review @ 2cd71ce).
    // The lease gate is green but NOT byte-parity. Golden: leader/__init__.py
    // (_claim_lease_no_incident:598, _receiver_from_claim_target:861, new_owner:686,
    // _lease_epoch:400, _emit_lease_refusal:469, audit :712-729). These REDs lock the EXACT
    // golden shape per divergence; driven via claim_lease_no_incident(+seeded_liveness), OS-safe.
    // ═════════════════════════════════════════════════════════════════════════
