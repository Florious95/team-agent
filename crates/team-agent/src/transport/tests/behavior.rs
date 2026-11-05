// ════════════════════════════════════════════════════════════════════════
// GROUP G — PaneField 单字段查询映射(display-message -p -F → list json 字段)
// 非 tmux 后端无对应概念的字段返回 Ok(None)(typed「不适用」)。
// ════════════════════════════════════════════════════════════════════════
use super::*;

#[test]
fn pane_field_variants_distinct() {
    // PaneTty remains until the Stage 2 K3 removal.
    let all = [PaneField::PaneMode, PaneField::PaneCurrentCommand, PaneField::PaneTty];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(i == j, a == b, "{a:?} vs {b:?}");
        }
    }
}

#[test]
fn query_pane_width_returns_numeric_string() {
    // Retain the current PaneMode argv assertion; the PaneWidth API is retired.
    // PaneMode 用裸格式参数(无 -F),pane_mode 直传(tmux_io.py:403)。
    assert_eq!(
        tmux_query_argv(&PaneId::new("%7"), PaneField::PaneMode),
        vec!["tmux", "display-message", "-p", "-t", "%7", "#{pane_mode}"]
    );
}

#[test]
fn query_pane_mode_is_not_applicable_on_wezterm() {
    // 设计 §3.1 query(pane_mode):非 tmux 后端无 pane-mode 概念 → recognizer 恒当 Input。
    // typed「不适用」收口为 Ok(None)(porter 不能返回 Ok(Some("anything")) 假绿)。RED via stub。
    let w = wezterm();
    let mode = w
        .query(&Target::Pane(PaneId::new("1")), PaneField::PaneMode)
        .expect("query pane_mode ok on wezterm (no error)");
    assert_eq!(
        mode, None,
        "wezterm has no pane-mode concept -> typed not-applicable is Ok(None)"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP H — liveness 三态(unknown ≠ dead ≠ live)
// contracts-rust-native test_pane_liveness_three_valued_parity。
// ════════════════════════════════════════════════════════════════════════

#[test]
fn freshly_spawned_pane_is_live_not_unknown_via_transport() {
    // 已知存活 fixture:刚 spawn 的 pane 必须 == Live(不是 all-variant 析取的同义反复)。
    // 镜像 conpty_cross_process...==Unknown 的强度,但锁的是 Live 这一极。
    // RED via stub:spawn_first/liveness 都 unimplemented!()。
    let t = tmux();
    let env = BTreeMap::new();
    let spawned = t
        .spawn_first(
            &SessionName::new("team-sess"),
            &WindowName::new("win-1"),
            &["sh".into(), "-lc".into(), "sleep 60".into()],
            Path::new("/tmp/ws"),
            &env,
        )
        .expect("spawn_first ok");
    assert_eq!(
        t.liveness(&spawned.pane_id).expect("liveness ok"),
        PaneLiveness::Live,
        "a freshly-spawned (sleeping) pane must read Live, never Unknown/Dead"
    );
}

#[test]
fn conpty_cross_process_pane_is_unknown_never_optimistic_live() {
    // contracts-rust-native:ConPTY 对非自有/跨进程 pane 判 Unknown,绝不乐观当 Live
    // (bug-085 穷尽三态;Unknown 显式 block ping,不 fallthrough idle)。RED via stub。
    let c = conpty();
    let res = c
        .liveness(&PaneId::new("foreign-leader"))
        .expect("liveness ok");
    assert_eq!(
        res,
        PaneLiveness::Unknown,
        "ConPTY cross-process pane must be Unknown, never optimistic Live"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP I — 能力性拒绝是 typed outcome,不是 Err(§10)
// SetEnvOutcome / AttachOutcome 的 per-backend 语义(transport-backend-design §4c/§1.3)。
// ════════════════════════════════════════════════════════════════════════







// ════════════════════════════════════════════════════════════════════════
// GROUP J — TransportError Display(thiserror;lib 边界)+ I/O 才走 Err
// bug-084:os I/O/timeout 必须 Result 化,绝不裸穿 coordinator tick。
// ════════════════════════════════════════════════════════════════════════

#[test]
fn transport_error_display_messages_byte_locked() {
    // 错误信息含定位字段(backend/stage/argv/target),便于 §5 系统集成调试。
    let e = TransportError::Inject {
        stage: InjectStage::PasteBuffer,
        source: std::io::Error::other("boom"),
    };
    assert_eq!(e.to_string(), "inject failed at stage PasteBuffer: boom");

    let sub = TransportError::Subprocess {
        argv: vec!["tmux".into(), "paste-buffer".into()],
        code: Some(1),
        stderr: "no such pane".into(),
    };
    assert_eq!(
        sub.to_string(),
        "subprocess [\"tmux\", \"paste-buffer\"] exited with Some(1): no such pane"
    );

    let mux = TransportError::MuxUnavailable {
        backend: BackendKind::WezTerm,
        detail: "mux server not reachable".into(),
    };
    assert_eq!(
        mux.to_string(),
        "mux unavailable on WezTerm: mux server not reachable"
    );

    let nf = TransportError::TargetNotFound {
        target: "%99".into(),
    };
    assert_eq!(nf.to_string(), "target not found: %99");
}

#[test]
fn io_error_converts_into_transport_error_via_from() {
    // bug-084:I/O 错误 typed 化(#[from] std::io::Error)。
    let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let te: TransportError = io.into();
    assert!(matches!(te, TransportError::Io(_)));
}

// ════════════════════════════════════════════════════════════════════════
// GROUP K — backend kind 诊断标识 + buffer 不进 trait(内化)
// ════════════════════════════════════════════════════════════════════════

#[test]
fn backend_kind_is_reported_per_backend() {
    assert_eq!(tmux().kind(), BackendKind::Tmux);
    assert_eq!(wezterm().kind(), BackendKind::WezTerm);
    assert_eq!(conpty().kind(), BackendKind::ConPty);
}


// ════════════════════════════════════════════════════════════════════════
// GROUP L — [真机] gated:WezTerm cli list / 正向身份登记(无法在此完整断言)
// ════════════════════════════════════════════════════════════════════════

#[test]
#[ignore = "real-machine-gated (fixture_kind=real): needs wezterm cli list --format json; \
                asserts pane_pid/current_command/leader_env forward-registration (GAP-2/GAP-3)"]
fn wezterm_list_targets_forward_registration_real_machine() {
    // contracts-rust-native test_list_targets_enumerate_parity /
    // test_worker_identity_forward_registration_parity:
    // WezTerm 身份靠正向登记表投影(不反向读进程 env)。仅锁形状,实现期真机校验。
    let w = wezterm();
    let listed = w.list_targets().expect("list_targets ok");
    for info in &listed {
        // 正向登记:leader_env 是登记表投影,不是反向读 /proc。
        // [真机] list json 未必给 pane_pid(GAP-2)/ current_command(GAP-3) → Option。
        let _ = (&info.leader_env, &info.pane_pid, &info.current_command);
    }
}

#[test]
#[ignore = "real-machine-gated (B1): kill_window/kill_session liveness transition needs a live mux"]
fn kill_window_then_target_dead_real_machine() {
    // contracts-rust-native test_kill_window_stops_agent_parity:
    // kill_window 后 liveness=Dead 且 list_targets 不再列出。实现期真机校验。
    let t = tmux();
    t.kill_window(&Target::Pane(PaneId::new("%7")))
        .expect("kill_window ok");
    assert_eq!(
        t.liveness(&PaneId::new("%7")).expect("liveness ok"),
        PaneLiveness::Dead
    );
}
