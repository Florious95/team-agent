// ════════════════════════════════════════════════════════════════════════
// GROUP A — serde 字节锁:verification / stage / backend 审计字符串
// (tmux_io.py + tmux_prompt.py 散布字符串穷尽枚举;09-transport.md 表 §38-42)。
// 这些是审计/事件 wire 值,改一个字节 = 下游 recognizer/事件断流。
// ════════════════════════════════════════════════════════════════════════
use super::*;

#[test]
fn backend_kind_serde_snake_case() {
    // BackendKind 诊断/事件用(transport-backend-design.md §1.3)。
    assert_eq!(
        serde_json::to_string(&BackendKind::Tmux).unwrap(),
        "\"tmux\""
    );
    assert_eq!(
        serde_json::to_string(&BackendKind::WezTerm).unwrap(),
        "\"wez_term\""
    );
    assert_eq!(
        serde_json::to_string(&BackendKind::ConPty).unwrap(),
        "\"con_pty\""
    );
}

#[test]
fn inject_stage_serde_kebab_case_byte_locked() {
    // 09-transport.md 表 §41:失败定位阶段,kebab-case 显式 rename。
    let pairs = [
        (InjectStage::SendKeys, "\"send-keys\""),
        (InjectStage::PrePasteCapture, "\"pre-paste-capture\""),
        (InjectStage::SetBuffer, "\"set-buffer\""),
        (InjectStage::LoadBuffer, "\"load-buffer\""),
        (InjectStage::PasteBuffer, "\"paste-buffer\""),
        (InjectStage::DeleteBuffer, "\"delete-buffer\""),
        (InjectStage::PrePastePaneState, "\"pre-paste-pane-state\""),
        (InjectStage::PaneModeCheck, "\"pane-mode-check\""),
        (InjectStage::Submit, "\"submit\""),
        (InjectStage::VisibleCheck, "\"visible-check\""),
    ];
    for (stage, wire) in pairs {
        assert_eq!(serde_json::to_string(&stage).unwrap(), wire, "{stage:?}");
    }
}





#[test]
fn pane_liveness_three_valued_reused_from_model() {
    // 裁决:Liveness{Live/Dead/Unknown} == model::enums::PaneLiveness(bug-085 穷尽三态)。
    // unknown ≠ dead ≠ live;serde 小写(与 state.py 对齐)。
    assert_eq!(
        serde_json::to_string(&PaneLiveness::Live).unwrap(),
        "\"live\""
    );
    assert_eq!(
        serde_json::to_string(&PaneLiveness::Dead).unwrap(),
        "\"dead\""
    );
    assert_eq!(
        serde_json::to_string(&PaneLiveness::Unknown).unwrap(),
        "\"unknown\""
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP B — id newtype 字节透明(== 裸字符串;沿用 model::ids 风格)
// ════════════════════════════════════════════════════════════════════════

#[test]
fn pane_id_transparent_bytes_equal_raw_string() {
    // tmux `%7` 字节级 == 裸字符串(serde transparent)。
    assert_eq!(serde_json::to_string(&PaneId::new("%7")).unwrap(), "\"%7\"");
    assert_eq!(PaneId::from("%12").as_str(), "%12");
    assert_eq!(PaneId::new("%3").to_string(), "%3");
    // 反序列化往返。
    let back: PaneId = serde_json::from_str("\"%99\"").unwrap();
    assert_eq!(back, PaneId::new("%99"));
}

#[test]
fn session_window_name_transparent_bytes() {
    assert_eq!(
        serde_json::to_string(&SessionName::new("team-sess")).unwrap(),
        "\"team-sess\""
    );
    assert_eq!(
        serde_json::to_string(&WindowName::new("win-1")).unwrap(),
        "\"win-1\""
    );
    assert_eq!(SessionName::from("s").as_str(), "s");
    assert_eq!(WindowName::from("w").to_string(), "w");
}

// ════════════════════════════════════════════════════════════════════════
// GROUP C — Target 寻址稳定性(禁混传:Pane vs SessionWindow 类型上区分)
// contracts-rust-native §2: spawn 返回的稳定 Target,后续 inject/capture 命中同进程。
// ════════════════════════════════════════════════════════════════════════

#[test]
fn spawn_first_returns_stable_addressable_target_then_reachable() {
    // contracts-rust-native: spawn_first 返回稳定可寻址 Target,且交回的 pane_id 能
    // 寻址回同一进程。RED:stub.spawn_first/capture unimplemented!()。
    let t = tmux();
    let env = BTreeMap::new();
    let spawned = t
        .spawn_first(
            &SessionName::new("team-sess"),
            &WindowName::new("win-1"),
            &["sh".into(), "-lc".into(), "echo hi".into()],
            Path::new("/tmp/ws"),
            &env,
        )
        .expect("spawn_first ok");
    // 身份正向登记:返回的 session/window 必须 == 请求的(不可凭空捏造别的)。
    assert_eq!(spawned.session, SessionName::new("team-sess"));
    assert_eq!(spawned.window, WindowName::new("win-1"));
    // 交回的 pane_id 必须能寻址回同一进程:capture 命中并能取到文本。
    let target = Target::Pane(spawned.pane_id.clone());
    let cap = t
        .capture(&target, CaptureRange::Tail(40))
        .expect("capture ok");
    assert_eq!(cap.range, CaptureRange::Tail(40));

    // 命令构造 golden(STEP-9 头号目标):first-spawn 必须是 new-session,
    // 不是 new-window(terminal.py:44-45 / runtime.py:1019-1020)。
    assert_eq!(
        tmux_spawn_argv(
            &SessionName::new("team-sess"),
            &WindowName::new("win-1"),
            "echo hi",
            true,
        ),
        vec![
            "tmux",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            "team-sess",
            "-n",
            "win-1",
            "sh",
            "-lc",
            "echo hi",
        ]
    );
}

#[test]
fn spawn_into_then_list_targets_enumerates_it() {
    // contracts-rust-native test_spawn_into_existing_session_parity。RED via stub。
    let t = tmux();
    let env = BTreeMap::new();
    let spawned = t
        .spawn_into(
            &SessionName::new("team-sess"),
            &WindowName::new("worker-2"),
            &["sh".into()],
            Path::new("/tmp/ws"),
            &env,
        )
        .expect("spawn_into ok");
    let listed = t.list_targets().expect("list_targets ok");
    // 枚举出的那条必须 == spawn 请求(porter 不能用同一捏造 pane_id 让 spawn_into
    // 与 list_targets 串通假绿):session/window_name/active 都要对上。
    let info = listed
        .iter()
        .find(|p| p.pane_id == spawned.pane_id)
        .expect("spawned worker must be enumerated by list_targets");
    assert_eq!(info.session, SessionName::new("team-sess"));
    assert_eq!(info.window_name, Some(WindowName::new("worker-2")));
    assert!(info.active, "freshly spawned worker pane is active");

    // 命令构造 golden:spawn_into 是 new-window(不是 new-session),挂到既有 session。
    assert_eq!(
        tmux_spawn_argv(
            &SessionName::new("team-sess"),
            &WindowName::new("worker-2"),
            "sh",
            false,
        ),
        // E53 (0.3.26): new-window now carries `-d` (detached, no focus steal).
        vec![
            "tmux",
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "team-sess",
            "-n",
            "worker-2",
            "sh",
            "-lc",
            "sh"
        ]
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP D — InjectPayload 空文本分流(trust turn-integrity 契约 §3,bug)
// text=="" 走纯 send submit-key,禁 set/load/paste-buffer 空串(tmux 拒空 buffer
// 会卡 trust prompt)。golden /tmp/transport_golden.py: empty_inject_*。
// ════════════════════════════════════════════════════════════════════════





// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model finding) ═══════════════
// P1 — inject must switch to `load-buffer -` (stdin) at TMUX_STDIN_BUFFER_THRESHOLD
// (16*1024 bytes); below it stays `set-buffer <text>`. The current seam has NO size
// param → always set-buffer → 16KiB+ prompts hit ARG_MAX/E2BIG.
// Golden: messaging/tmux_io.py:292-303 (`size >= TMUX_STDIN_BUFFER_THRESHOLD`),
// _tmux_load_buffer_stdin argv `["tmux","load-buffer","-b",<buf>,"-"]`; runtime.py:464
// TMUX_STDIN_BUFFER_THRESHOLD = 16 * 1024.

// ════════════════════════════════════════════════════════════════════════
// GROUP E — Key 枚举翻译(抽象 Key,各后端翻译,不透传 tmux 字面量)
// contracts-rust-native test_key_enum_translation_parity / cancel_mode_noop。
// ════════════════════════════════════════════════════════════════════════

#[test]
fn send_keys_sequence_routes_through_transport() {
    // send_keys 接受 &[Key];RED via stub。
    let t = tmux();
    t.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::Down, Key::Enter])
        .expect("send_keys ok");

    // 命令构造 golden(STEP-9):抽象 Key 翻译成 tmux 字面键名,不透传(§gap-5)。
    // [Down, Enter] → send-keys -t %7 Down Enter(codex.py:266)。
    assert_eq!(tmux_key_name(Key::Down), "Down");
    assert_eq!(tmux_key_name(Key::Enter), "Enter");
    assert_eq!(
        tmux_send_keys_argv(&PaneId::new("%7"), &[Key::Down, Key::Enter]),
        vec!["tmux", "send-keys", "-t", "%7", "Down", "Enter"]
    );
}

#[test]
fn cancel_mode_is_noop_on_non_tmux_backends() {
    // contracts-rust-native test_cancel_mode_noop_on_non_tmux_parity:
    // Key::CancelMode 在 WezTerm/ConPTY 上无害 no-op(无 copy-mode 概念),不报错不阻塞。
    // RED via stub(porter 让 wezterm/conpty 返回 Ok(()))。
    for be in [wezterm(), conpty()] {
        be.send_keys(&Target::Pane(PaneId::new("%1")), &[Key::CancelMode])
            .expect("CancelMode no-op must be Ok on non-tmux");
    }

    // tmux 侧:CancelMode 按 pane mode 分派退出键(命令构造 golden,tmux_io.py:419-426)。
    // copy→-X cancel,tree/view→q,client→d,unknown→-X cancel(+warn)。
    assert_eq!(
        tmux_cancel_mode_argv(&PaneId::new("%7"), PaneMode::Copy),
        vec!["tmux", "send-keys", "-t", "%7", "-X", "cancel"]
    );
    assert_eq!(
        tmux_cancel_mode_argv(&PaneId::new("%7"), PaneMode::Tree),
        vec!["tmux", "send-keys", "-t", "%7", "q"]
    );
    assert_eq!(
        tmux_cancel_mode_argv(&PaneId::new("%7"), PaneMode::View),
        vec!["tmux", "send-keys", "-t", "%7", "q"]
    );
    assert_eq!(
        tmux_cancel_mode_argv(&PaneId::new("%7"), PaneMode::Client),
        vec!["tmux", "send-keys", "-t", "%7", "d"]
    );
    assert_eq!(
        tmux_cancel_mode_argv(&PaneId::new("%7"), PaneMode::Unknown),
        vec!["tmux", "send-keys", "-t", "%7", "-X", "cancel"]
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP F — CaptureRange tail/full 语义 + capture 出口规范化收口
// tmux `-S -<N>`(Tail)/ `-S -`(Full);DELIVERY_CAPTURE_LINES=40 是默认 tail。
// ════════════════════════════════════════════════════════════════════════

#[test]
fn capture_tail_records_range_in_captured_text() {
    // CapturedText 携带它抓取的 range + 出口规范化文本。
    // contracts-rust-native test_capture_normalized_text_parity。RED via stub。
    let t = tmux();
    let cap = t
        .capture(&Target::Pane(PaneId::new("%7")), CaptureRange::Tail(40))
        .expect("capture ok");
    assert_eq!(cap.range, CaptureRange::Tail(40));

    // 命令构造 golden(STEP-9):Tail(40) → capture-pane -p -S -40 -t %7
    // (tmux_prompt.py:149 / tmux_io.py:410)。范围回显不够 —— 钉死 argv 映射。
    assert_eq!(
        tmux_capture_argv(&PaneId::new("%7"), CaptureRange::Tail(40)),
        vec!["tmux", "capture-pane", "-p", "-S", "-40", "-t", "%7"]
    );
    // 出口文本规范化 golden(§4b design line 399-400):逐行 rstrip + \xa0→空格。
    assert_eq!(
        normalize_capture("line one  \nbusy\u{a0}marker   \n  \n"),
        "line one\nbusy marker\n\n"
    );
}

#[test]
fn capture_full_returns_complete_history() {
    // contracts-rust-native test_capture_full_scrollback_parity(tmux -S -)。RED via stub。
    let t = tmux();
    let cap = t
        .capture(&Target::Pane(PaneId::new("%7")), CaptureRange::Full)
        .expect("capture full ok");
    assert_eq!(cap.range, CaptureRange::Full);

    // 命令构造 golden:Full → capture-pane -p -S - -t %7(全量 scrollback,runtime.py:519)。
    // 注意是 `-S -`(空 N)不是 `-S -0`。
    assert_eq!(
        tmux_capture_argv(&PaneId::new("%7"), CaptureRange::Full),
        vec!["tmux", "capture-pane", "-p", "-S", "-", "-t", "%7"]
    );
}
