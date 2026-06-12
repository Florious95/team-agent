use super::*;

    // =====================================================================
    // 1. 锁名 / 闭枚举 / serde 字节对齐(纯数据,不依赖 unimplemented body)
    // =====================================================================

    // LEADER_OWNERSHIP_LOCK = "send"(__init__.py:393):attach/claim/takeover/autobind 共享同一临界区。
    #[test]
    fn ownership_lock_is_send() {
        assert_eq!(LEADER_OWNERSHIP_LOCK, "send");
    }

    // LeaseReason 闭枚举的 serde 字节必须与 _LEASE_REASON_ENUM ∪ {caller_pane_missing} 一致。
    // golden(probe_leader.py):enum 8 值(snake)+ binding 的 caller_pane_missing。
    #[test]
    fn lease_reason_serializes_to_exact_python_strings() {
        let cases = [
            (LeaseReason::VacantAcquired, "\"vacant_acquired\""),
            (LeaseReason::PreviousOwnerPaneDead, "\"previous_owner_pane_dead\""),
            (LeaseReason::PreviousOwnerAliveRefused, "\"previous_owner_alive_refused\""),
            (LeaseReason::OwnerEpochAdvanced, "\"owner_epoch_advanced\""),
            (LeaseReason::ForceConfirmRequired, "\"force_confirm_required\""),
            (LeaseReason::CallerNotLeaderShaped, "\"caller_not_leader_shaped\""),
            (LeaseReason::CallerCwdMismatch, "\"caller_cwd_mismatch\""),
            (LeaseReason::NotInTmuxPane, "\"not_in_tmux_pane\""),
            (LeaseReason::CallerPaneMissing, "\"caller_pane_missing\""),
        ];
        for (r, want) in cases {
            assert_eq!(serde_json::to_string(&r).unwrap(), want, "{r:?}");
        }
    }

    // _LEASE_REBIND_REQUIRED_REASONS = {not_in_tmux_pane, caller_not_leader_shaped, caller_cwd_mismatch}
    // (__init__.py:384-386)→ 决定 refusal 事件名 rebind_required vs claim_refused。
    // is_rebind_required 是 unimplemented!() → 调用即 panic = RED。
    #[test]
    fn lease_reason_rebind_required_membership_matches_python() {
        // 三个 rebind-required(golden probe_leader.py rebind_required 集)。
        assert!(LeaseReason::NotInTmuxPane.is_rebind_required());
        assert!(LeaseReason::CallerNotLeaderShaped.is_rebind_required());
        assert!(LeaseReason::CallerCwdMismatch.is_rebind_required());
        // 其余一律 claim_refused(非 rebind-required)。
        assert!(!LeaseReason::VacantAcquired.is_rebind_required());
        assert!(!LeaseReason::PreviousOwnerPaneDead.is_rebind_required());
        assert!(!LeaseReason::PreviousOwnerAliveRefused.is_rebind_required());
        assert!(!LeaseReason::OwnerEpochAdvanced.is_rebind_required());
        assert!(!LeaseReason::ForceConfirmRequired.is_rebind_required());
        assert!(!LeaseReason::CallerPaneMissing.is_rebind_required());
    }

    // LeaseStatus serde(probe_leader3.py):already_bound / claimed / refused / dry_run。
    #[test]
    fn lease_status_serializes_to_exact_python_strings() {
        assert_eq!(serde_json::to_string(&LeaseStatus::AlreadyBound).unwrap(), "\"already_bound\"");
        assert_eq!(serde_json::to_string(&LeaseStatus::Claimed).unwrap(), "\"claimed\"");
        assert_eq!(serde_json::to_string(&LeaseStatus::Refused).unwrap(), "\"refused\"");
        assert_eq!(serde_json::to_string(&LeaseStatus::DryRun).unwrap(), "\"dry_run\"");
    }

    // Discovery serde(probe_leader3.py):attach_readopt/claim_leader/env_pane/explicit_pane/current_pane。
    #[test]
    fn discovery_serializes_to_exact_python_strings() {
        assert_eq!(serde_json::to_string(&Discovery::AttachReadopt).unwrap(), "\"attach_readopt\"");
        assert_eq!(serde_json::to_string(&Discovery::ClaimLeader).unwrap(), "\"claim_leader\"");
        assert_eq!(serde_json::to_string(&Discovery::EnvPane).unwrap(), "\"env_pane\"");
        assert_eq!(serde_json::to_string(&Discovery::ExplicitPane).unwrap(), "\"explicit_pane\"");
        assert_eq!(serde_json::to_string(&Discovery::CurrentPane).unwrap(), "\"current_pane\"");
    }

    // ClaimedVia 是 kebab-case(__init__.py:545/693/876 等写 "attach-leader"/"claim-leader")。
    #[test]
    fn claimed_via_serializes_kebab_case() {
        assert_eq!(serde_json::to_string(&ClaimedVia::ClaimLeader).unwrap(), "\"claim-leader\"");
        assert_eq!(serde_json::to_string(&ClaimedVia::AttachLeader).unwrap(), "\"attach-leader\"");
    }

    // LeaseSource serde:launch/quick_start/restart/manual(__init__.py source 取值)。
    #[test]
    fn lease_source_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&LeaseSource::Launch).unwrap(), "\"launch\"");
        assert_eq!(serde_json::to_string(&LeaseSource::QuickStart).unwrap(), "\"quick_start\"");
        assert_eq!(serde_json::to_string(&LeaseSource::Restart).unwrap(), "\"restart\"");
        assert_eq!(serde_json::to_string(&LeaseSource::Manual).unwrap(), "\"manual\"");
    }

    // ReceiverMode/Status:direct_tmux / attached(__init__.py:283-284)。
    #[test]
    fn receiver_mode_and_status_serialize_to_python_strings() {
        assert_eq!(serde_json::to_string(&ReceiverMode::DirectTmux).unwrap(), "\"direct_tmux\"");
        assert_eq!(serde_json::to_string(&ReceiverStatus::Attached).unwrap(), "\"attached\"");
    }

    // LeaderStartMode:exec_provider/new_tmux_session/attach_existing(leader_start_plan)。
    #[test]
    fn leader_start_mode_serializes_to_python_strings() {
        assert_eq!(serde_json::to_string(&LeaderStartMode::ExecProvider).unwrap(), "\"exec_provider\"");
        assert_eq!(serde_json::to_string(&LeaderStartMode::ManagedTmuxClient).unwrap(), "\"managed_tmux_client\"");
        assert_eq!(serde_json::to_string(&LeaderStartMode::NewTmuxSession).unwrap(), "\"new_tmux_session\"");
        assert_eq!(serde_json::to_string(&LeaderStartMode::AttachExisting).unwrap(), "\"attach_existing\"");
    }

    // NodeRole:worker/leader(build_idle_nodes role 字段)。
    #[test]
    fn node_role_serializes_to_python_strings() {
        assert_eq!(serde_json::to_string(&NodeRole::Worker).unwrap(), "\"worker\"");
        assert_eq!(serde_json::to_string(&NodeRole::Leader).unwrap(), "\"leader\"");
    }

    // RereadReason:wake.py 五值(probe_leader.py)。
    #[test]
    fn reread_reason_serializes_to_python_strings() {
        assert_eq!(serde_json::to_string(&RereadReason::NoFile).unwrap(), "\"no_file\"");
        assert_eq!(serde_json::to_string(&RereadReason::NeverClassified).unwrap(), "\"never_classified\"");
        assert_eq!(serde_json::to_string(&RereadReason::FileChanged).unwrap(), "\"file_changed\"");
        assert_eq!(
            serde_json::to_string(&RereadReason::QuiescentAlreadyClassified).unwrap(),
            "\"quiescent_already_classified\""
        );
        assert_eq!(serde_json::to_string(&RereadReason::Unchanged).unwrap(), "\"unchanged\"");
    }

    // =====================================================================
    // 2. LeaderSessionUuidSource — 漂移 NOTE(card §168-179)
    // =====================================================================

    // leader plan 侧 _leader_identity_context(__init__.py:206)只产 "override"/"derived";
    // identity lane(state.py:332)用 "explicit-override"/"env"/"derived"。
    // 此 enum 的 serde 字节当对齐 LEADER 侧:Override→"override"。Env→"env" 也在闭集。
    #[test]
    fn leader_session_uuid_source_serializes_leader_plan_strings() {
        assert_eq!(serde_json::to_string(&LeaderSessionUuidSource::Derived).unwrap(), "\"derived\"");
        // NOTE(drift):leader plan 写裸 "override"(非 identity lane 的 "explicit-override")。
        assert_eq!(serde_json::to_string(&LeaderSessionUuidSource::Override).unwrap(), "\"override\"");
        assert_eq!(serde_json::to_string(&LeaderSessionUuidSource::Env).unwrap(), "\"env\"");
    }

    // 漂移可观测:identity lane 的 caller dict 用 "explicit-override",二者不可互换。
    // 这是 leader 集成时必须 reconcile 的 NOTE —— 锁死 leader-plan 侧字节,防被误统一成
    // identity 串。serde override≠"explicit-override"。
    #[test]
    fn leader_plan_override_is_not_identity_explicit_override_string() {
        let leader_plan = serde_json::to_string(&LeaderSessionUuidSource::Override).unwrap();
        assert_eq!(leader_plan, "\"override\"");
        assert_ne!(leader_plan, "\"explicit-override\"", "drift NOTE: leader plan 用 'override',不可写成 identity 的 'explicit-override'");
    }

    // =====================================================================
    // 3. LeaderEvent::name() — §40 字节级一致(unimplemented → RED)
    // =====================================================================

    // 全部 leader 审计事件名(probe_leader2.py FOUND 验证;ping 来自 idle_predicate 跨 lane,亦 golden)。
    // 注:ReceiverClaimLeaderNotification 在 v0.2.11 任何 source 都查无 "leader_receiver.claim_leader_notification"
    // 字面 → 见 deferred,此处不钉该串(只钉可验证的 23 个)。
    #[test]
    fn leader_event_names_match_python_byte_for_byte() {
        let cases = [
            (LeaderEvent::ReceiverAttached, "leader_receiver.attached"),
            (LeaderEvent::ReceiverRebindApplied, "leader_receiver.rebind_applied"),
            (LeaderEvent::ReceiverClaimApplied, "leader_receiver.claim_applied"),
            (LeaderEvent::ReceiverClaimRefused, "leader_receiver.claim_refused"),
            (LeaderEvent::ReceiverRebindRequired, "leader_receiver.rebind_required"),
            (LeaderEvent::ReceiverAttachFailed, "leader_receiver.attach_failed"),
            (LeaderEvent::ReceiverStateDivergenceRepaired, "leader_receiver.state_divergence_repaired"),
            (LeaderEvent::ReceiverFirstTimeEnvSeeded, "leader_receiver.first_time_env_seeded"),
            (LeaderEvent::ReceiverAutobindSkipped, "leader_receiver.autobind_skipped"),
            (LeaderEvent::ReceiverRequeuedExhaustedWatchers, "leader_receiver.requeued_exhausted_watchers"),
            (LeaderEvent::ReceiverAmbiguousCandidates, "leader_receiver.ambiguous_candidates"),
            (LeaderEvent::ReceiverClaimRequeue, "leader_receiver.claim_requeue"),
            (LeaderEvent::OwnerAdoptedOnRestart, "owner.adopted_on_restart"),
            (LeaderEvent::OwnerBoundFromCallerPane, "owner.bound_from_caller_pane"),
            (LeaderEvent::OwnerBindRefused, "owner.bind_refused"),
            (LeaderEvent::OwnerEpochAdvanced, "owner_epoch_advanced"),
            (LeaderEvent::LeaderSessionUuidOverride, "leader_session_uuid.override"),
            (LeaderEvent::LeaderStart, "leader.start"),
            (LeaderEvent::ResultWatcherRequeued, "result_watcher.requeued"),
            (LeaderEvent::IdleTakeoverClassify, "idle_takeover.classify"),
            (LeaderEvent::IdleTakeoverPing, "idle_takeover.ping"),
            (LeaderEvent::IdleTakeoverReminder, "idle_takeover.reminder"),
            (LeaderEvent::IdleTakeoverPushFailed, "idle_takeover.push_failed"),
        ];
        for (ev, want) in cases {
            assert_eq!(ev.name(), want, "{ev:?}");
        }
    }
