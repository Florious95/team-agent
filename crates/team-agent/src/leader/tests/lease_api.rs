use super::*;

    // =====================================================================
    // 7. 五条 lease 路径签名 + 返回 LeaseResult 形态(unimplemented → RED)
    // =====================================================================

    // attach_leader:手动 CLI attach(__init__.py:19-58 → attach_leader_to_state:276 →
    // _resolve_leader_pane)。在无 live tmux 的测试环境,指定一个不存在的 pane %1 →
    // _resolve_leader_pane raise RuntimeError("tmux pane not found: %1")(_legacy_pane_discovery.py:153),
    // 映射到 LeaderError::Validation。golden(probe_attach.py 已验:真跑即 raise)。
    // 强化:钉具体的 Err 形态 + 错误串含 pane id;并断言失败时绝不留下半绑定 state(无 team_owner)。
    // unimplemented → RED(unimplemented panic ≠ 期望的 Validation,且后续 is_err 断言不会被求值)。
    #[test]
    fn attach_leader_errors_when_pane_not_resolvable() {
        let ws = std::env::temp_dir().join(format!("ta_rs_attach_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let r = attach_leader(&ws, None, Some(&PaneId::new("%1")), Provider::Codex);
        // 不可解析 pane → Err(Validation),错误串提及 pane not found。
        match r {
            Err(LeaderError::Validation(msg)) => {
                assert!(msg.contains("%1"), "Validation 错误须含目标 pane id,got {msg}");
                assert!(msg.contains("not found"), "须是 pane-not-found 形态,got {msg}");
            }
            Err(other) => panic!("期望 Validation(pane not found),got {other:?}"),
            Ok(v) => panic!("无 live tmux pane 时不该成功 attach,got {v:?}"),
        }
        // 失败不留半绑定:state.json 无 team_owner。
        let st = crate::state::persist::load_runtime_state(&ws).unwrap();
        assert!(st.get("team_owner").is_none(), "resolve 失败不得落 team_owner");
    }

    // attach_leader 成功 post-state(需 live tmux pane + cross-lane _resolve_leader_pane):
    // vacant acquire → status=Claimed、reason=vacant_acquired、owner_epoch 0→1、owner/receiver 绑同 pane、
    // 且 workspace state.json 真被持久化。golden(_claim_lease_no_incident:81/102/139-143)。
    // real-machine-gated(无 live tmux 无法驱动 pane resolver)。
    #[test]
    #[ignore = "needs a live tmux pane + cross-lane _resolve_leader_pane (step 9/11)"]
    fn attach_leader_binds_pane_advances_epoch_and_persists() {
        let ws = std::env::temp_dir().join(format!("ta_rs_attach_ok_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let pane = PaneId::new("%1");
        let r = attach_leader(&ws, None, Some(&pane), Provider::Codex).unwrap();
        assert!(r.ok);
        assert_eq!(r.status, LeaseStatus::Claimed);
        let owner = r.owner.as_ref().expect("attach 成功必带 owner");
        assert_eq!(owner.pane_id, pane);
        assert_eq!(owner.owner_epoch, OwnerEpoch(1), "vacant acquire 后 epoch=1");
        assert_eq!(owner.provider, Provider::Codex);
        let receiver = r.receiver.as_ref().expect("attach 成功必带 receiver");
        assert_eq!(receiver.pane_id, pane);
        assert_eq!(r.reason, Some(LeaseReason::VacantAcquired));
        let persisted = crate::state::persist::load_runtime_state(&ws).unwrap();
        assert_eq!(persisted["team_owner"]["pane_id"], serde_json::json!("%1"));
        assert_eq!(persisted["team_owner"]["owner_epoch"], serde_json::json!(1));
    }

    // autobind:$TMUX_PANE 缺 → Ok(None)(__init__.py:885-887,锁前直接返回,不开锁)。
    // 强化:断言这是 lock-not-acquired 早退 —— 不写 state.json(无 receiver/owner 落盘),
    // 不发任何 leader_receiver.* 事件(锁前 return,连 EventLog 都不构造)。
    #[test]
    fn autobind_returns_none_when_tmux_pane_missing() {
        if std::env::var_os("TMUX_PANE").is_some() {
            return; // 在 tmux 内:走绑定路径,本用例只验缺失分支。
        }
        let ws = std::env::temp_dir().join(format!("ta_rs_auto_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let r = autobind_leader_receiver_from_env(&ws, Provider::Codex, LeaseSource::Restart).unwrap();
        assert!(r.is_none(), "$TMUX_PANE 缺 → autobind 返回 Ok(None)");
        // lock-not-acquired 早退:state.json 未被写(load 兜底成空 state,无 team_owner/receiver)。
        let st = crate::state::persist::load_runtime_state(&ws).unwrap();
        assert!(st.get("leader_receiver").is_none(), "skip 路径不应落 receiver");
        assert!(st.get("team_owner").is_none(), "skip 路径不应落 owner");
        // 早退发生在 EventLog 构造前 → 无任何事件文件。
        let events = crate::event_log::EventLog::new(&ws).tail(50).unwrap();
        assert!(events.is_empty(), "skip 路径绝不写审计事件");
    }

    // autobind 成功支:$TMUX_PANE 命中 → 锁内 attach_leader_to_state → Ok(Some(receiver))。
    // receiver.pane_id == 注入 pane,discovery==EnvPane($TMUX_PANE 直接命中)。
    // env 变更进程全局且与并行 test race,且依赖跨 lane 的 live pane resolver,故 real-machine-gated。
    #[test]
    #[ignore = "needs live $TMUX_PANE + cross-lane _resolve_leader_pane; env mutation races parallel tests"]
    fn autobind_binds_env_pane_on_success() {
        let ws = std::env::temp_dir().join(format!("ta_rs_auto_ok_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        // SAFETY: #[ignore]d,单独 `--ignored --test-threads=1` 跑,不与并行 test race。
        unsafe { std::env::set_var("TMUX_PANE", "%42") };
        let r = autobind_leader_receiver_from_env(&ws, Provider::Codex, LeaseSource::Restart).unwrap();
        unsafe { std::env::remove_var("TMUX_PANE") };
        let receiver = r.expect("$TMUX_PANE 命中 → Ok(Some(receiver))");
        assert_eq!(receiver.pane_id, PaneId::new("%42"));
        assert_eq!(receiver.discovery, Some(Discovery::EnvPane), "$TMUX_PANE 命中 → discovery=env_pane");
    }

    // claim_leader:无 ambiguous incident → 走 claim_lease_no_incident 直接 acquire/CAS。
    // 现 unimplemented → RED;锁住返回 LeaseResult。
    #[test]
    fn claim_leader_returns_lease_result() {
        let ws = std::env::temp_dir().join(format!("ta_rs_claim_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let r = claim_leader(&ws, None, false).unwrap();
        // 无 caller pane(测试进程无 TMUX_PANE)→ refused not_in_tmux_pane(__init__.py:616-618)。
        if std::env::var_os("TMUX_PANE").is_none() {
            assert!(!r.ok);
            assert_eq!(r.status, LeaseStatus::Refused);
            assert_eq!(r.reason, Some(LeaseReason::NotInTmuxPane));
            assert!(r.action.is_some());
        }
    }

    // write_lease_dual_state:同一锁内双写(C17,__init__.py:588-596);unimplemented → RED。
    // 强化:带 session_name 时必须落 BOTH —— workspace state.json + team/<session> snapshot,
    // 两份 team_owner.pane_id / owner_epoch 必须一致(永不分叉)。空 body Ok(()) 会被这里抓。
    #[test]
    fn write_lease_dual_state_persists_both_locations_without_divergence() {
        let ws = std::env::temp_dir().join(format!("ta_rs_dual_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let state = serde_json::json!({
            "session_name": "team-sess",
            "team_owner": {"pane_id": "%1", "owner_epoch": 2, "leader_session_uuid": "uuuu"},
            "leader_receiver": {"pane_id": "%1", "owner_epoch": 2},
        });
        write_lease_dual_state(&ws, &state).unwrap();
        // (1) workspace state.json(<ws>/.team/runtime/state.json)被写,team_owner.pane_id==%1。
        let ws_path = crate::state::persist::runtime_state_path(&ws);
        let ws_state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ws_path).expect("workspace state.json 必须存在")).unwrap();
        assert_eq!(ws_state["team_owner"]["pane_id"], serde_json::json!("%1"));
        assert_eq!(ws_state["team_owner"]["owner_epoch"], serde_json::json!(2));
        // (2) team-level snapshot(<ws>/.team/runtime/teams/<session>/state.json)被写。
        let snap_path = crate::model::paths::runtime_dir(&ws)
            .join("teams")
            .join("team-sess")
            .join("state.json");
        let snap_state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&snap_path).expect("team snapshot state.json 必须存在")).unwrap();
        // (3) 两份永不分叉:owner pane_id / owner_epoch 必须相等(C17 核心不变量)。
        assert_eq!(
            ws_state["team_owner"]["pane_id"], snap_state["team_owner"]["pane_id"],
            "workspace 与 team snapshot 的 owner pane 不得分叉"
        );
        assert_eq!(
            ws_state["team_owner"]["owner_epoch"], snap_state["team_owner"]["owner_epoch"],
            "workspace 与 team snapshot 的 owner_epoch 不得分叉"
        );
    }

    #[test]
    fn lease_divergence_reads_restart_snapshot_for_special_session_names() {
        let ws = std::env::temp_dir().join(format!("ta_rs_dual_special_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let state = serde_json::json!({
            "session_name": "team:proj-中文",
            "team_owner": {"pane_id": "%1", "owner_epoch": 2, "leader_session_uuid": "uuuu"},
            "leader_receiver": {"pane_id": "%1", "owner_epoch": 2},
        });
        write_lease_dual_state(&ws, &state).unwrap();
        let safe_path = crate::lifecycle::helpers::team_snapshot_path(&ws, "team:proj-中文");
        let raw_path = crate::model::paths::runtime_dir(&ws)
            .join("teams")
            .join("team:proj-中文")
            .join("state.json");
        assert!(
            safe_path.exists(),
            "lease snapshot must use the restart-safe path: {}",
            safe_path.display()
        );
        assert!(
            !raw_path.exists(),
            "new lease writes must not create the legacy raw session path: {}",
            raw_path.display()
        );

        let restart_state = serde_json::json!({
            "session_name": "team:proj-中文",
            "team_owner": {"pane_id": "%9", "owner_epoch": 2, "leader_session_uuid": "uuuu"},
            "leader_receiver": {"pane_id": "%9", "owner_epoch": 2},
        });
        let restart_path = crate::lifecycle::save_team_runtime_snapshot(&ws, &restart_state).unwrap();
        assert_eq!(restart_path, safe_path);
        assert!(
            restart_path.exists(),
            "restart snapshot must be written to a real file: {}",
            restart_path.display()
        );

        let d = detect_dual_state_divergence(&ws, &state)
            .unwrap()
            .expect("divergence detector must read the restart-written snapshot");
        assert_eq!(d["workspace_owner_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_owner_pane"], serde_json::json!("%9"));
        assert_eq!(d["workspace_receiver_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_receiver_pane"], serde_json::json!("%9"));
    }

    #[test]
    fn detect_dual_state_divergence_reads_legacy_raw_snapshot_when_safe_absent() {
        let ws = std::env::temp_dir().join(format!("ta_rs_dual_legacy_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let state = serde_json::json!({
            "session_name": "team:legacy-中文",
            "team_owner": {"pane_id": "%1", "leader_session_uuid": "uuuu", "owner_epoch": 2},
            "leader_receiver": {"pane_id": "%1", "owner_epoch": 2},
        });
        let safe_path = crate::lifecycle::helpers::team_snapshot_path(&ws, "team:legacy-中文");
        assert!(!safe_path.exists(), "test setup requires safe path absent");
        let raw_dir = crate::model::paths::runtime_dir(&ws)
            .join("teams")
            .join("team:legacy-中文");
        std::fs::create_dir_all(&raw_dir).unwrap();
        let snap = serde_json::json!({
            "session_name": "team:legacy-中文",
            "team_owner": {"pane_id": "%8", "leader_session_uuid": "uuuu", "owner_epoch": 2},
            "leader_receiver": {"pane_id": "%8", "owner_epoch": 2},
        });
        std::fs::write(raw_dir.join("state.json"), serde_json::to_string(&snap).unwrap()).unwrap();

        let d = detect_dual_state_divergence(&ws, &state)
            .unwrap()
            .expect("legacy raw snapshot fallback must preserve old teams");
        assert_eq!(d["workspace_owner_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_owner_pane"], serde_json::json!("%8"));
        assert_eq!(d["workspace_receiver_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_receiver_pane"], serde_json::json!("%8"));
    }

    // detect_dual_state_divergence:无 session_name → None(__init__.py:560-561);unimplemented → RED。
    #[test]
    fn detect_dual_state_divergence_none_without_session_name() {
        let ws = std::env::temp_dir().join(format!("ta_rs_div_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let state = serde_json::json!({"team_owner": {"pane_id": "%1"}}); // 无 session_name。
        let d = detect_dual_state_divergence(&ws, &state).unwrap();
        assert!(d.is_none(), "无 session_name → 无 snapshot 可比 → None");
    }

    // C18 核心:workspace state.json 与 team snapshot 在 owner pane 上分叉 → Some(具体分叉字段)。
    // golden(probe_leader_strengthen.py):workspace owner=%1 / snapshot owner=%9 →
    // {workspace_owner_pane:%1, team_owner_pane:%9, workspace_receiver_pane:%1, team_receiver_pane:%9}。
    // unimplemented → RED。
    #[test]
    fn detect_dual_state_divergence_reports_diverging_panes() {
        let ws = std::env::temp_dir().join(format!("ta_rs_divx_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let state = serde_json::json!({
            "session_name": "team-sess",
            "team_owner": {"pane_id": "%1", "leader_session_uuid": "uuuu", "owner_epoch": 2},
            "leader_receiver": {"pane_id": "%1", "owner_epoch": 2},
        });
        // 写一份分叉 snapshot:owner/receiver pane 都是 %9(不同于 workspace 的 %1)。
        let snap_dir = crate::model::paths::runtime_dir(&ws).join("teams").join("team-sess");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let snap = serde_json::json!({
            "session_name": "team-sess",
            "team_owner": {"pane_id": "%9", "leader_session_uuid": "uuuu", "owner_epoch": 2},
            "leader_receiver": {"pane_id": "%9", "owner_epoch": 2},
        });
        std::fs::write(snap_dir.join("state.json"), serde_json::to_string(&snap).unwrap()).unwrap();
        let d = detect_dual_state_divergence(&ws, &state).unwrap().expect("分叉 → Some(details)");
        assert_eq!(d["workspace_owner_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_owner_pane"], serde_json::json!("%9"));
        assert_eq!(d["workspace_receiver_pane"], serde_json::json!("%1"));
        assert_eq!(d["team_receiver_pane"], serde_json::json!("%9"));

        // 匹配 snapshot(与 workspace 一致)→ None(无分叉)。
        std::fs::write(snap_dir.join("state.json"), serde_json::to_string(&state).unwrap()).unwrap();
        assert!(
            detect_dual_state_divergence(&ws, &state).unwrap().is_none(),
            "两份一致 → 无分叉 → None"
        );
    }

// R8 D4 (c-lite offline byte-lock): the leader_receiver.requeued_exhausted_watchers payload-build,
// extracted from the real-tmux attach flow into a pure helper, must produce golden shape.
// golden leader/__init__.py:39-44: EXACTLY {watcher_ids, count, trigger:"attach_leader"}.
#[test]
fn r8_requeued_exhausted_watchers_event_payload_golden_shape() {
    let notices = vec![crate::messaging::WatcherNotice {
        watcher_id: "w1".to_string(),
        result_id: Some("r1".to_string()),
        ok: true,
        status: Some("notify_failed".to_string()),
        notified_message_id: None,
        primary_watcher_id: None,
        prior_state: Some("delivery_exhausted".to_string()),
        error: None,
    }];
    let payload = crate::leader::lease::requeued_exhausted_watchers_event_payload(
        &crate::transport::PaneId::new("%leader"),
        &crate::model::ids::TeamKey::new("team-a"),
        &notices,
    );
    let keys: std::collections::BTreeSet<&str> =
        payload.as_object().unwrap().keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> = ["watcher_ids", "count", "trigger"].into_iter().collect();
    assert_eq!(keys, expected,
        "D4: leader_receiver.requeued_exhausted_watchers payload must be golden {{watcher_ids, count, trigger}} \
         (leader/__init__.py:39-44), not the Rust {{pane_id, team_id, watcher_ids, requeued}}; got {keys:?}");
    assert_eq!(
        payload.get("watcher_ids").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()),
        Some(vec!["w1"]), "watcher_ids must be the string list of requeued ids");
    assert_eq!(payload.get("count").and_then(|v| v.as_u64()), Some(1), "count == number of requeued watchers");
    assert_eq!(payload.get("trigger").and_then(|v| v.as_str()), Some("attach_leader"));
}

// 0.5.x Windows portability Batch 5: `FakeAppServer` uses UNIX domain
// sockets to fake the Codex app-server. Codex app-server client is
// Unix-only (Windows returns typed `SocketUnreachable`), so these
// tests are Unix-only too.
#[cfg(unix)]
#[test]
fn app_server_attach_writes_transport_kind_tuple_and_advances_epoch() {
    let ws = std::env::temp_dir().join(format!(
        "ta_rs_app_attach_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&ws).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "active_team_key": "team-a",
            "teams": {"team-a": {"team_key": "team-a", "agents": {}}},
        }),
    )
    .unwrap();
    let fake = crate::app_server_test_support::FakeAppServer::start(
        "attach-ok",
        crate::app_server_test_support::FakeAppServerScript::happy(
            "thread-live",
            "session-live",
            ws.to_str().unwrap(),
        ),
    );

    let out = attach_app_server_leader(&ws, Some("team-a"), fake.endpoint(), "thread-live")
        .expect("app-server attach should succeed");

    assert_eq!(out["ok"], serde_json::json!(true));
    assert_eq!(out["leader_receiver"]["transport_kind"], serde_json::json!("codex_app_server"));
    assert_eq!(out["leader_receiver"]["mode"], serde_json::json!("codex_app_server"));
    assert_eq!(
        out["leader_receiver"]["app_server"]["thread_id"],
        serde_json::json!("thread-live")
    );
    assert_eq!(
        out["leader_receiver"]["app_server"]["session_id"],
        serde_json::json!("session-live")
    );
    assert_eq!(
        out["leader_receiver"]["app_server"]["cwd"],
        serde_json::json!(ws.to_str().unwrap())
    );

    let saved = crate::state::projection::select_runtime_state(&ws, Some("team-a")).unwrap();
    assert_eq!(
        saved["leader_receiver"]["app_server"]["thread_id"],
        serde_json::json!("thread-live")
    );
    assert_eq!(saved["leader_receiver"].get("pane_id"), None);
    assert_eq!(saved["team_owner"]["transport_kind"], serde_json::json!("codex_app_server"));
    assert_eq!(saved["owner_epoch"], serde_json::json!(1));
}

#[cfg(unix)]
#[test]
fn app_server_attach_rejects_world_writable_socket_without_state_write() {
    let ws = std::env::temp_dir().join(format!(
        "ta_rs_app_attach_badmode_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&ws).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"active_team_key": "team-a", "teams": {"team-a": {"agents": {}}}}),
    )
    .unwrap();
    let fake = crate::app_server_test_support::FakeAppServer::start(
        "attach-world",
        crate::app_server_test_support::FakeAppServerScript::happy(
            "thread-live",
            "session-live",
            ws.to_str().unwrap(),
        ),
    );
    let mut perms = std::fs::metadata(fake.path()).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o777);
    std::fs::set_permissions(fake.path(), perms).unwrap();

    let err = attach_app_server_leader(&ws, Some("team-a"), fake.endpoint(), "thread-live")
        .expect_err("world-writable app-server socket must be rejected");
    assert!(
        err.to_string().contains("socket_ownership_invalid"),
        "unexpected error: {err}"
    );
    let saved = crate::state::projection::select_runtime_state(&ws, Some("team-a")).unwrap();
    assert!(
        saved.get("leader_receiver").is_none(),
        "failed attach must not write leader_receiver: {saved}"
    );
}

#[cfg(unix)]
#[test]
fn app_server_attach_rejects_missing_user_agent_without_state_write() {
    let ws = std::env::temp_dir().join(format!(
        "ta_rs_app_attach_no_ua_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&ws).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"active_team_key": "team-a", "teams": {"team-a": {"agents": {}}}}),
    )
    .unwrap();
    let mut script = crate::app_server_test_support::FakeAppServerScript::happy(
        "thread-live",
        "session-live",
        ws.to_str().unwrap(),
    );
    script.user_agent = None;
    let fake = crate::app_server_test_support::FakeAppServer::start("attach-no-ua", script);

    let err = attach_app_server_leader(&ws, Some("team-a"), fake.endpoint(), "thread-live")
        .expect_err("missing initialize.userAgent must fail closed");
    assert!(
        err.to_string()
            .contains("protocol_mismatch_missing_user_agent"),
        "unexpected error: {err}"
    );
    let saved = crate::state::projection::select_runtime_state(&ws, Some("team-a")).unwrap();
    assert!(
        saved.get("leader_receiver").is_none(),
        "failed attach must not write leader_receiver: {saved}"
    );
}

#[test]
fn app_server_delivery_paths_are_read_only_and_binding_entry_is_explicit() {
    let delivery = include_str!("../../messaging/delivery.rs");
    assert!(
        !delivery.contains("write_owner(")
            && !delivery.contains("with_leader_receiver(")
            && !delivery.contains("with_team_owner("),
        "MUST-12/I-RN-1: delivery may read typed leader_receiver fields but must not write ownership"
    );
    let lease = include_str!("../lease.rs");
    assert!(
        lease.contains("fn write_leader_receiver_transport("),
        "C-5: mode and transport_kind must be stamped by a single receiver transport helper"
    );
    let cli = include_str!("../../cli/attach_app_server_leader.rs");
    assert!(
        cli.contains("attach_app_server_leader("),
        "I-RN-2: app-server ownership mutation must be reachable through the explicit CLI entry"
    );
}

#[test]
fn messaging_leader_receiver_module_exports_no_ownership_writer() {
    let leader_receiver = include_str!("../../messaging/leader_receiver.rs");
    for forbidden in [
        "pub fn claim_leader_receiver",
        "write_owner(",
        "with_leader_receiver(",
        "with_team_owner(",
        "save_runtime_state",
    ] {
        assert!(
            !leader_receiver.contains(forbidden),
            "MUST-12/I-RN-1: messaging/leader_receiver.rs may read receiver fields but must not expose ownership writes; forbidden={forbidden}"
        );
    }

    let messaging_mod = include_str!("../../messaging/mod.rs");
    assert!(
        !messaging_mod.contains("claim_leader_receiver"),
        "MUST-12/I-RN-2: messaging/mod.rs must not re-export ownership writer APIs"
    );
}
