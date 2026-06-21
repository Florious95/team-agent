use super::*;

    // =====================================================================
    // 12. leader_identity_context — override / state uuid / derive 三源(unimplemented → RED)
    // =====================================================================

    // 无 override env、无 state record → derive(machine, ws_abspath, user, team)。
    // 现 unimplemented → 调用即 RED;锁住返回 LeaderIdentity 且 source==Derived。
    #[test]
    #[serial_test::serial(env)]
    fn leader_identity_context_derives_when_no_override_no_state() {
        let ws = std::env::temp_dir().join(format!("ta_rs_lic_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        // 空 state(无 team_owner/leader_receiver uuid)。
        let state = serde_json::json!({});
        let id = leader_identity_context(&ws, None, Some(&state)).unwrap();
        // 无 override → source 为 Derived(leader plan 侧;__init__.py:206)。
        assert_eq!(id.leader_session_uuid_source, LeaderSessionUuidSource::Derived);
        // uuid 是 32 hex(derive 形)。
        assert_eq!(id.leader_session_uuid.as_str().len(), 32);
    }

    // leader_identity:CLI 直出 dict(__init__.py:355-369)。unimplemented → RED。
    // 强化:uuid_prefix 必须 == leader_identity_context 派生 uuid 的前 12 hex(绑到真值,
    // 而非任意 12 字符);整个 dict 的 machine_fingerprint/os_user/team_id/source 必须与 context 一致。
    #[test]
    #[serial_test::serial(env)]
    fn leader_identity_dict_ties_prefix_and_fields_to_derived_context() {
        let ws = std::env::temp_dir().join(format!("ta_rs_lid_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        // 空 state → context 走 derive(无 override/无 state uuid)。
        let ctx = leader_identity_context(&ws, None, Some(&serde_json::json!({}))).unwrap();
        let expected_prefix = &ctx.leader_session_uuid.as_str()[..12];
        let v = leader_identity(&ws, None).unwrap();
        assert_eq!(v["ok"], serde_json::json!(true));
        // uuid_prefix 绑到派生真值的前 12 hex(错的 12 字符串会被抓)。
        assert_eq!(v["uuid_prefix"].as_str().unwrap(), expected_prefix);
        // 其余身份字段与 context 字节一致。
        assert_eq!(v["machine_fingerprint"].as_str().unwrap(), ctx.machine_fingerprint);
        assert_eq!(v["os_user"].as_str().unwrap(), ctx.os_user);
        assert_eq!(v["team_id"].as_str().unwrap(), ctx.team_id.as_str());
        // source == 派生侧 "derived"(无 override → 不是 "override"/"env")。
        assert_eq!(v["source"], serde_json::json!("derived"));
        assert_eq!(ctx.leader_session_uuid_source, LeaderSessionUuidSource::Derived);
        // CLI 直出形态:current_pane_id / last_seen_at 在无 env/无 receiver 时为 null。
        assert_eq!(v["last_seen_at"], serde_json::Value::Null);
    }

    // =====================================================================
    // 13. leader_start_plan(unimplemented → RED):钉 mode 选择 + leader_env 导出键。
    // =====================================================================

    // leader_start_plan(__init__.py:82-145)。强化:钉具体 mode + plan 内容,而非 provider 回声。
    // 确定性环境:在 TMUX 内 → exec_provider;不在 TMUX(且 tmux 可用)→ new_tmux_session,
    // session_name==leader_session_name(Fake,ws),leader_env 携带 5 个 TEAM_AGENT_* 导出键。
    // 注:`detached` 在 leader_start_plan 返回值里恒为 false(__init__.py:174 "detached": False);
    //     非 tty 的 `-d` 插入发生在 start_leader 调用者层(:74-78),不在本 plan 边界 → 不在此断言 detached。
    // unimplemented → RED。
    #[test]
    #[serial_test::serial(env)]
    fn leader_start_plan_pins_mode_and_leader_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TMUX", None), ("TMUX_PANE", None)]);
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let plan = leader_start_plan(Provider::Fake, &[], &ws, false, false, None, false).unwrap();
        assert_eq!(plan.provider, Provider::Fake);
        assert_eq!(plan.mode, LeaderStartMode::ManagedTmuxClient);
        // 0.3.28 Step 2: managed mode now uses the dedicated leader session
        // (`team-agent-leader-<provider>-<folder>-<sha1[:8]>`), not the worker
        // session `team-<team_id>`. Python parity.
        let session_name = plan.session_name.as_ref().map(SessionName::as_str).unwrap_or("");
        assert!(
            session_name.starts_with(crate::layout::sessions::LEADER_SESSION_PREFIX),
            "managed mode must use dedicated leader session prefix; got `{session_name}`"
        );
        // 0.3.28 Step 2: window name = provider wire (`fake`), not `leader`.
        assert_eq!(
            plan.leader_window.as_ref().map(WindowName::as_str),
            Some("fake")
        );
        assert!(!plan.is_external_leader);
        assert!(
            plan.argv.iter().any(|arg| arg == "attach-session"),
            "no-tmux managed launch attaches the user client to the leader window: {:?}",
            plan.argv
        );
        // plan 边界 detached 恒 false(`-d` 插入在 start_leader 层,非此处)。
        assert!(!plan.detached, "leader_start_plan 返回值 detached 恒 false");
        // leader_env 携带 5 个 TEAM_AGENT_* 导出键(_leader_provider_env)。
        for key in [
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_MACHINE_FINGERPRINT",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
        ] {
            assert!(plan.leader_env.contains_key(key), "leader_env 缺导出键 {key}");
        }
        assert_eq!(
            plan.leader_env.get("TEAM_AGENT_LEADER_PROVIDER").map(String::as_str),
            Some("fake")
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn leader_start_plan_external_leader_keeps_exec_provider_in_tmux() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TMUX", Some("/private/tmp/tmux-501/default,88432,187"))]);
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_external_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let provider_args = vec!["--".to_string(), "--model".to_string(), "opus".to_string()];
        let plan = leader_start_plan(Provider::Fake, &provider_args, &ws, false, false, None, true).unwrap();

        assert_eq!(plan.mode, LeaderStartMode::ExecProvider);
        assert!(plan.is_external_leader);
        assert_eq!(plan.leader_window, None);
        assert_eq!(
            plan.argv,
            vec!["fake".to_string(), "--model".to_string(), "opus".to_string()]
        );
        assert_eq!(
            plan.provider_argv,
            vec!["fake".to_string(), "--model".to_string(), "opus".to_string()]
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn managed_leader_uses_dedicated_leader_session_independent_of_state_session_name() {
        // 0.3.28 Step 2 amendment: the persisted `session_name` is the WORKER
        // session, not the leader session. Managed leader now ignores it and
        // always uses the dedicated `leader_session_name(provider, workspace)`.
        // This test pins the new behaviour and the regression guard that the
        // managed argv never gets a `team-team-*` double prefix.
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TMUX", None), ("TMUX_PANE", None)]);
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_existing_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({"session_name": "team-alpha"}),
        )
        .unwrap();

        let plan = leader_start_plan(Provider::Fake, &[], &ws, false, false, None, false).unwrap();

        assert_eq!(plan.mode, LeaderStartMode::ManagedTmuxClient);
        let session_name = plan.session_name.as_ref().map(SessionName::as_str).unwrap_or("");
        assert!(
            session_name.starts_with(crate::layout::sessions::LEADER_SESSION_PREFIX),
            "managed mode uses dedicated leader session prefix regardless of \
             persisted worker session_name; got `{session_name}`"
        );
        assert!(
            !plan.argv.iter().any(|arg| arg.contains("team-team-alpha")),
            "managed session name must not gain a second team- prefix: {:?}",
            plan.argv
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn in_tmux_default_leader_runs_provider_in_current_pane() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_switch_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let socket = crate::tmux_backend::socket_name_for_workspace(&ws);
        let endpoint = format!("/private/tmp/tmux-501/{socket},88432,187");
        let _e = EnvGuard::apply(&[("TMUX", Some(&endpoint)), ("TMUX_PANE", Some("%7"))]);

        let plan = leader_start_plan(Provider::Fake, &[], &ws, false, false, None, false).unwrap();

        assert_eq!(plan.mode, LeaderStartMode::ExecProvider);
        assert!(!plan.is_external_leader);
        assert!(plan.session_name.is_none());
        assert_eq!(plan.leader_window, None);
        assert_eq!(plan.argv, vec!["fake".to_string()]);
        assert!(
            !plan.argv.iter().any(|arg| arg == "switch-client" || arg == "attach-session"),
            "in-tmux default launch must not create or attach a background leader session: {:?}",
            plan.argv
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn in_tmux_default_leader_does_not_refuse_different_tmux_server() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("TMUX", Some("/private/tmp/tmux-501/default,88432,187")),
            ("TMUX_PANE", Some("%9")),
        ]);
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_refuse_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let plan = leader_start_plan(Provider::Fake, &[], &ws, false, false, None, false).unwrap();

        assert_eq!(plan.mode, LeaderStartMode::ExecProvider);
        assert!(!plan.is_external_leader);
        assert!(plan.session_name.is_none());
        assert_eq!(plan.argv, vec!["fake".to_string()]);
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_leader_env_disables_terminal_title_only_for_copilot() {
        let ws = std::env::temp_dir().join(format!("ta_rs_lsp_copilot_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let identity = leader_identity_context(&ws, None, Some(&serde_json::json!({}))).unwrap();

        let copilot = leader_env_for_identity(Provider::Copilot, &identity);
        assert_eq!(
            copilot.get("COPILOT_DISABLE_TERMINAL_TITLE").map(String::as_str),
            Some("1")
        );

        for provider in [Provider::Codex, Provider::ClaudeCode] {
            let leader_env = leader_env_for_identity(provider, &identity);
            assert!(
                !leader_env.contains_key("COPILOT_DISABLE_TERMINAL_TITLE"),
                "{provider:?} leader env must not include copilot title override"
            );
        }
    }

    // ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
    // Lock CORRECT Python v0.2.11 leader-identity behavior the contracts missed.
    // Golden re-probed via /tmp/probe_p2_leader.py vs team-agent-public @ 439bef8
    // (leader/__init__.py:_leader_identity_context / _identity_* / _detect_dual_state_divergence).

    #[test]
    #[serial_test::serial(env)]
    fn p2_leader_state_uuid_source_is_derived_not_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
            ("TEAM_AGENT_LEADER_SESSION_UUID", None),
        ]);
        let ws = p2_temp_ws("src");
        let state = serde_json::json!({"team_owner": {"leader_session_uuid": "STATEUUID123"}});
        let id = leader_identity_context(&ws, None, Some(&state)).unwrap();
        assert_eq!(id.leader_session_uuid_source, LeaderSessionUuidSource::Derived);
        assert_eq!(id.leader_session_uuid.as_str(), "STATEUUID123");
    }

    // P1 — operator override env var is TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE (the
    // _OVERRIDE suffix), per leader/__init__.py:197 — NOT the injected child-env var.
    #[test]
    #[serial_test::serial(env)]
    fn p2_leader_override_reads_override_suffixed_env_var() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", Some("OVERRIDE_X")),
            ("TEAM_AGENT_LEADER_SESSION_UUID", None),
        ]);
        let ws = p2_temp_ws("ovr");
        let id = leader_identity_context(&ws, None, None).unwrap();
        assert_eq!(id.leader_session_uuid_source, LeaderSessionUuidSource::Override);
        assert_eq!(id.leader_session_uuid.as_str(), "OVERRIDE_X");
    }

    // P1 — derived inputs read state: machine_fingerprint = state team_owner record first
    // (_identity_machine_fingerprint); team_id = team_state_key(state) from session_name
    // (default 'current', not a hardcoded 'default').
    #[test]
    #[serial_test::serial(env)]
    fn p2_leader_derived_inputs_read_state_record() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[("TEAM_AGENT_MACHINE_FINGERPRINT", None)]);
        let ws = p2_temp_ws("der");
        let state = serde_json::json!({
            "team_owner": {"machine_fingerprint": "RECORDED-FP-FROM-STATE"},
            "session_name": "team-agent-myteam"
        });
        let id = leader_identity_context(&ws, None, Some(&state)).unwrap();
        assert_eq!(id.machine_fingerprint, "RECORDED-FP-FROM-STATE", "state record fp beats env/hostname");
        assert_eq!(id.team_id.as_str(), "team-agent-myteam", "team_id from state.session_name");
    }

    // P1 — os_user fallback chain = USER or USERNAME or '' (_identity_os_user), NOT
    // USER or LOGNAME or 'unknown'. (USERNAME is the 2nd choice; empty-string fallback.)
    #[test]
    #[serial_test::serial(env)]
    fn p2_leader_os_user_honors_username_then_empty() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _e = EnvGuard::apply(&[
            ("USER", None),
            ("LOGNAME", None),
            ("USERNAME", Some("winuser")),
        ]);
        let ws = p2_temp_ws("usr");
        let id = leader_identity_context(&ws, None, None).unwrap();
        assert_eq!(id.os_user, "winuser", "USERNAME is the second choice (not LOGNAME)");
    }

    // P1 — detect_dual_state_divergence must catch an owner leader_session_uuid split even
    // when panes + epoch are identical (leader/__init__.py:574).
    #[test]
    #[serial_test::serial(env)]
    fn p2_leader_detect_divergence_catches_owner_uuid_split() {
        let ws = p2_temp_ws("div");
        let snap_dir = crate::model::paths::runtime_dir(&ws).join("teams").join("sess1");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let snap = serde_json::json!({
            "session_name":"sess1",
            "team_owner":{"pane_id":"%1","leader_session_uuid":"UUID_B","owner_epoch":5},
            "leader_receiver":{"pane_id":"%1"}
        });
        std::fs::write(snap_dir.join("state.json"), serde_json::to_string(&snap).unwrap()).unwrap();
        let state = serde_json::json!({
            "session_name":"sess1",
            "team_owner":{"pane_id":"%1","leader_session_uuid":"UUID_A","owner_epoch":5},
            "leader_receiver":{"pane_id":"%1"}
        });
        let div = detect_dual_state_divergence(&ws, &state).unwrap();
        assert!(div.is_some(), "owner uuid split (A vs B) with matching panes/epoch must be detected");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 14. WAVE-2 Lane B CONTRACT PASS — CLI-handler-facing byte-parity for the
    //     three verbs (claim-leader / takeover / identity) + their core lease
    //     machinery (_claim_lease_no_incident outcomes / _lease_refused shapes).
    //
    //     GOLDEN (re-probed @ team-agent-public, leader/__init__.py +
    //     runtime.py:721/791). Each test labels RED|LOCK honestly:
    //       RED  = drives an unimplemented!() body (claim_lease_no_incident /
    //              attach_leader_to_state) → panics today = correct RED-first.
    //       LOCK = drives an already-implemented stub/path → green today; pins
    //              the golden so a future port cannot regress it.
    //     Deferred to later adversarial rounds (Lane-A style): the ambiguous-
    //     incident claim arm (no_caller_pane / caller_not_candidate / dry_run /
    //     lost_race) which needs a seeded event-log incident + the broadcast
    //     requeue cross-lane; the strict-uuid attach refusal string (needs a
    //     live pane resolver). Those are #[ignore]/NOTE seams below.
    // ═══════════════════════════════════════════════════════════════════════

    // ── 14a. _claim_lease_no_incident OUTCOMES (golden __init__.py:598) ──────
    //
    // claim_lease_no_incident is unimplemented!() → every test here PANICS today
    // ═══════════════════════════════════════════════════════════════════════
    // unit-0 (Stage 0) characterization tests
    //
    // Pin two invariants that unit-1/3/4 will refactor behind typed
    // identity helpers. If the constant or the prefix shape changes, the
    // refactor must also update these tests.
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn unit0_leader_session_prefix_constant_is_stable() {
        // Pinned: the literal leader session prefix that gates "is this a
        // leader session" decisions everywhere in the runtime.
        assert_eq!(
            crate::leader::start::LEADER_SESSION_PREFIX,
            "team-agent-leader-"
        );
        // The layout module re-exports the same value (kept in sync via
        // const re-export).
        assert_eq!(
            crate::layout::sessions::LEADER_SESSION_PREFIX,
            crate::leader::start::LEADER_SESSION_PREFIX
        );
    }

    #[test]
    fn unit0_leader_prefixed_name_must_never_be_taken_as_worker_session() {
        // Pinned invariant fed into unit-1 typed identity: any session
        // name starting with `LEADER_SESSION_PREFIX` is a leader launcher
        // session, never a worker session. unit-1 will wrap this rule in
        // typed constructors (`WorkerSession::new` rejects the prefix,
        // `LeaderLauncherSession::new` requires it).
        let leader = "team-agent-leader-claude-x-aaaaaaaaaaaa";
        let worker = "team-real-worker";
        let prefix = crate::leader::start::LEADER_SESSION_PREFIX;
        assert!(leader.starts_with(prefix));
        assert!(!worker.starts_with(prefix));
        // Symmetry: the runtime today uses this exact check
        // (cli/mod.rs:261 `starts_with(LEADER_SESSION_PREFIX)`) to spare
        // leader sessions during shutdown.
        let leader_name = crate::transport::SessionName::new(leader);
        let worker_name = crate::transport::SessionName::new(worker);
        assert!(
            crate::layout::sessions::is_leader_session(&leader_name),
            "is_leader_session must accept a leader-prefixed session",
        );
        assert!(
            !crate::layout::sessions::is_leader_session(&worker_name),
            "is_leader_session must reject a non-prefixed session",
        );
    }
