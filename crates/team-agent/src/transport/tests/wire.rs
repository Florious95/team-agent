    // ════════════════════════════════════════════════════════════════════════
    // GROUP A — serde 字节锁:verification / stage / backend 审计字符串
    // (tmux_io.py + tmux_prompt.py 散布字符串穷尽枚举;09-transport.md 表 §38-42)。
    // 这些是审计/事件 wire 值,改一个字节 = 下游 recognizer/事件断流。
    // ════════════════════════════════════════════════════════════════════════
    use super::*;

    #[test]
    fn backend_kind_serde_snake_case() {
        // BackendKind 诊断/事件用(transport-backend-design.md §1.3)。
        assert_eq!(serde_json::to_string(&BackendKind::Tmux).unwrap(), "\"tmux\"");
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
    fn inject_verification_serde_snake_case_byte_locked() {
        // 09-transport.md 表 §38。注意 EmptyTextSendKeys = 空文本走纯 send-keys 的验证。
        let pairs = [
            (
                InjectVerification::CaptureContainsToken,
                "\"capture_contains_token\"",
            ),
            (
                InjectVerification::CaptureContainsMessageFragment,
                "\"capture_contains_message_fragment\"",
            ),
            (
                InjectVerification::CaptureContainsNewPastedContentPrompt,
                "\"capture_contains_new_pasted_content_prompt\"",
            ),
            (InjectVerification::NoToken, "\"no_token\""),
            (
                InjectVerification::CaptureMissingToken,
                "\"capture_missing_token\"",
            ),
            (
                InjectVerification::EmptyTextSendKeys,
                "\"empty_text_send_keys\"",
            ),
        ];
        for (v, wire) in pairs {
            assert_eq!(serde_json::to_string(&v).unwrap(), wire, "{v:?}");
        }
    }

    #[test]
    fn turn_verification_serde_snake_case_byte_locked() {
        // 09-transport.md 表 §40;Gap42:not_yet_observed 也算成功,绝非投递闸门。
        let pairs = [
            (
                TurnVerification::LeaderNewTurnBoundaryVerified,
                "\"leader_new_turn_boundary_verified\"",
            ),
            (
                TurnVerification::LeaderNewTurnBoundaryMissing,
                "\"leader_new_turn_boundary_missing\"",
            ),
            (TurnVerification::NotYetObserved, "\"not_yet_observed\""),
            (TurnVerification::NotRequired, "\"not_required\""),
        ];
        for (v, wire) in pairs {
            assert_eq!(serde_json::to_string(&v).unwrap(), wire, "{v:?}");
        }
    }

    #[test]
    fn submit_verification_wire_strings_byte_locked() {
        // 09-transport.md 表 §39 + tmux_io.py:64/215-221 + tmux_prompt.py:304/313。
        // SubmitVerification 携 Key 模板,Python 是散字符串 → 这里钉死 to-wire 映射。
        // RED via stub:submit_verification_wire unimplemented!()。
        assert_eq!(
            submit_verification_wire(SubmitVerification::EnterSentWithoutPlaceholderCheck),
            "enter_sent_without_placeholder_check"
        );
        assert_eq!(
            submit_verification_wire(SubmitVerification::PastedContentPromptAbsentAfterSubmit),
            "pasted_content_prompt_absent_after_submit"
        );
        assert_eq!(
            submit_verification_wire(SubmitVerification::PastedContentPromptStillPresentAfterSubmit),
            "pasted_content_prompt_still_present_after_submit"
        );
        assert_eq!(
            submit_verification_wire(SubmitVerification::SendKeysFailed),
            "send_keys_failed"
        );
        // 模板 `{key}_sent_after_visible_token`:key 取 tmux 字面键名(submit_key)。
        // golden:Enter → "Enter_sent_after_visible_token"(tmux_io.py:218,submit_key 字面)。
        assert_eq!(
            submit_verification_wire(SubmitVerification::KeySentAfterVisibleToken {
                key: Key::Enter
            }),
            "Enter_sent_after_visible_token"
        );
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
    fn target_pane_and_session_window_are_distinct_addressings() {
        // 两种合法寻址不可混传;同名 PaneId vs SessionWindow 不相等。
        let p = Target::Pane(PaneId::new("%7"));
        let sw = Target::SessionWindow {
            session: SessionName::new("team-sess"),
            window: WindowName::new("%7"),
        };
        assert_ne!(p, sw);
        // Pane 寻址按 PaneId 字节相等。
        assert_eq!(
            Target::Pane(PaneId::new("%7")),
            Target::Pane(PaneId::new("%7"))
        );
        // SessionWindow 按 (session,window) 对相等;任一不同即不等。
        let a = Target::SessionWindow {
            session: SessionName::new("s"),
            window: WindowName::new("w"),
        };
        let b = Target::SessionWindow {
            session: SessionName::new("s"),
            window: WindowName::new("w2"),
        };
        assert_ne!(a, b);
    }

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
        let cap = t.capture(&target, CaptureRange::Tail(40)).expect("capture ok");
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
                "tmux", "new-session", "-d", "-s", "team-sess", "-n", "win-1", "sh", "-lc",
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
            vec!["tmux", "new-window", "-t", "team-sess", "-n", "worker-2", "sh", "-lc", "sh"]
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    // GROUP D — InjectPayload 空文本分流(trust turn-integrity 契约 §3,bug)
    // text=="" 走纯 send submit-key,禁 set/load/paste-buffer 空串(tmux 拒空 buffer
    // 会卡 trust prompt)。golden /tmp/transport_golden.py: empty_inject_*。
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn inject_payload_empty_is_typed_distinct_from_empty_text() {
        // 类型上把空文本与含字符文本分流(InjectPayload::Empty != Text("..."))。
        assert_ne!(InjectPayload::Empty, InjectPayload::Text(String::new()));
        assert_eq!(
            InjectPayload::Text("hi".into()),
            InjectPayload::Text("hi".into())
        );
    }

    #[test]
    fn inject_empty_payload_reports_empty_text_send_keys_and_turn_not_required() {
        // golden empty_inject_report:
        //   verification=empty_text_send_keys, turn_verification=not_required,
        //   stage=submitted(Rust:stage_reached=Submit), submitted=true, attempts=1。
        // 空文本禁走 buffer:只发一个 submit-key(golden empty_inject_calls 仅 1 条 send-keys)。
        // RED:stub.inject unimplemented!()。
        let t = tmux();
        let report = t
            .inject(
                &Target::Pane(PaneId::new("%7")),
                &InjectPayload::Empty,
                Key::Enter,
                false,
            )
            .expect("inject empty ok");
        assert_eq!(report.inject_verification, InjectVerification::EmptyTextSendKeys);
        assert_eq!(report.turn_verification, TurnVerification::NotRequired);
        assert_eq!(report.stage_reached, InjectStage::Submit);
        assert_eq!(report.attempts, 1);
    }

    #[test]
    fn empty_inject_is_single_direct_send_keys_never_buffer() {
        // 命令构造 golden(STEP-9):空文本禁走 buffer,只发一条 send-keys 提交键
        // (tmux_io.py:42 —— tmux 拒空 buffer 会卡 trust prompt)。
        // golden empty_inject_calls 仅 1 条:send-keys -t %7 Enter。RED via stub。
        assert_eq!(
            tmux_empty_inject_argv(&PaneId::new("%7"), Key::Enter),
            vec!["tmux", "send-keys", "-t", "%7", "Enter"]
        );
    }

    #[test]
    fn inject_text_payload_bracketed_reaches_submit_with_token_verification() {
        // contracts-rust-native test_inject_text_visible_parity:
        //   非空文本走 set/load-buffer + paste-buffer(-p)+ submit;
        //   有 token 时 verification=capture_contains_token,turn 仅 metadata(Gap42)。
        // RED via stub。
        let t = tmux();
        let report = t
            .inject(
                &Target::Pane(PaneId::new("%7")),
                &InjectPayload::Text("hello [team-agent-token:abc]".into()),
                Key::Enter,
                true,
            )
            .expect("inject text ok");
        assert_eq!(report.stage_reached, InjectStage::Submit);
        assert_eq!(
            report.inject_verification,
            InjectVerification::CaptureContainsToken
        );
        // Gap42:此 fixture 无 leader-boundary 标记(provider 非 fake,2s 内未观测到新 turn
        // → tmux_io.py:192-193 `turn_verification = "not_yet_observed"`)。metadata-only,
        // 绝非投递闸门 —— golden 钉死为 NotYetObserved(不是 2-of-4 析取)。
        assert_eq!(report.turn_verification, TurnVerification::NotYetObserved);

        // 命令构造 golden(STEP-9 头号目标):非空文本走 set-buffer → paste-buffer -p →
        // delete-buffer 序列(tmux_io.py:119/303/314)。-p == bracketed paste。
        let argv = tmux_inject_text_argv(
            &PaneId::new("%7"),
            "team-agent-buf",
            "hello [team-agent-token:abc]",
            true,
        );
        assert_eq!(
            argv,
            vec![
                vec![
                    "tmux",
                    "set-buffer",
                    "-b",
                    "team-agent-buf",
                    "hello [team-agent-token:abc]"
                ],
                vec!["tmux", "paste-buffer", "-t", "%7", "-b", "team-agent-buf", "-p"],
                vec!["tmux", "delete-buffer", "-b", "team-agent-buf"],
            ]
        );
    }

    // ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model finding) ═══════════════
    // P1 — inject must switch to `load-buffer -` (stdin) at TMUX_STDIN_BUFFER_THRESHOLD
    // (16*1024 bytes); below it stays `set-buffer <text>`. The current seam has NO size
    // param → always set-buffer → 16KiB+ prompts hit ARG_MAX/E2BIG.
    // Golden: messaging/tmux_io.py:292-303 (`size >= TMUX_STDIN_BUFFER_THRESHOLD`),
    // _tmux_load_buffer_stdin argv `["tmux","load-buffer","-b",<buf>,"-"]`; runtime.py:464
    // TMUX_STDIN_BUFFER_THRESHOLD = 16 * 1024.
    #[test]
    fn p2_inject_large_text_switches_to_load_buffer_stdin_at_16k() {
        let pane = PaneId::new("%7");
        let buf = "team-agent-buf";

        // small → set-buffer with the text inline (unchanged).
        let small = tmux_inject_text_argv(&pane, buf, "hello", true);
        assert_eq!(small[0], vec!["tmux", "set-buffer", "-b", buf, "hello"]);

        // just below threshold (16383 bytes) → still set-buffer.
        let below = "x".repeat(16383);
        let below_argv = tmux_inject_text_argv(&pane, buf, &below, true);
        assert_eq!(below_argv[0][1], "set-buffer", "16383 bytes < threshold → set-buffer");

        // at/above threshold (16384 bytes) → load-buffer - (text streamed via stdin).
        let big = "x".repeat(16384);
        let big_argv = tmux_inject_text_argv(&pane, buf, &big, true);
        assert_eq!(
            big_argv[0],
            vec!["tmux", "load-buffer", "-b", buf, "-"],
            ">=16384 bytes must stream via `load-buffer -` (stdin), not set-buffer argv"
        );
        // the payload must NOT ride the command line (ARG_MAX hazard).
        assert!(
            !big_argv[0].iter().any(|a| a.len() >= 16384),
            "the 16KiB text must not be passed as an argv argument"
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    // GROUP E — Key 枚举翻译(抽象 Key,各后端翻译,不透传 tmux 字面量)
    // contracts-rust-native test_key_enum_translation_parity / cancel_mode_noop。
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn key_enum_variants_are_distinct_and_copy() {
        // Enter/Up/Down/Left/Right/Char/CtrlC/CancelMode 互不相等;Char 携字符。
        assert_ne!(Key::Enter, Key::CtrlC);
        assert_ne!(Key::Up, Key::Down);
        assert_ne!(Key::Char('1'), Key::Char('2'));
        assert_eq!(Key::Char('3'), Key::Char('3'));
        // Copy:可按值传两次。
        let k = Key::Enter;
        let _ = (k, k);
    }

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
        assert_eq!(tmux_key_name(Key::CtrlC), "C-c");
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
    fn capture_range_tail_and_full_distinct() {
        assert_ne!(CaptureRange::Tail(40), CaptureRange::Full);
        assert_ne!(CaptureRange::Tail(40), CaptureRange::Tail(30));
        assert_eq!(CaptureRange::Tail(40), CaptureRange::Tail(40));
    }

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
