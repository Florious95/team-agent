    //! TMUX-BACKEND RED — every `Transport` method is `unimplemented!()` today, so these PANIC (RED)
    //! until the porter wires the bodies + `RealCommandRunner`. The OS edge is mocked by
    //! `MockCommandRunner` (records each argv; returns canned `CommandOutput`/io::Error you stage).
    //! Each test asserts (1) the recorded argv == the golden-locked `transport::tmux_*_argv` builder
    //! (or the golden command form for builder-less ops) and (2) the parsed typed return. Golden:
    //! runtime.py (has-session/spawn/kill), leader/__init__.py:335 (set-environment), state.py:341
    //! (_tmux_pane_liveness three-state, §bug-085 unknown != dead), transport.rs argv-builders.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::{BTreeMap, VecDeque};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{CommandOutput, CommandRunner, RealCommandRunner, TmuxBackend};
    use crate::model::enums::PaneLiveness;
    use crate::transport::{
        normalize_capture, tmux_capture_argv, tmux_query_argv, tmux_send_keys_argv, tmux_spawn_argv,
        AttachOutcome, CaptureRange, InjectPayload, InjectStage, InjectVerification, Key, PaneField,
        PaneId, SessionName, SetEnvOutcome, SubmitVerification, Target, Transport, TransportError,
        TurnVerification, WindowName,
    };

    type RecordedArgv = Arc<Mutex<Vec<Vec<String>>>>;
    type RecordedStdin = Arc<Mutex<Vec<String>>>;

    /// A staged runner response: a canned `CommandOutput`, or an io::Error (kind) for the error path.
    #[derive(Clone)]
    enum MockResp {
        Out(CommandOutput),
        Io(std::io::ErrorKind),
    }

    /// Records every argv it is asked to run; replays staged responses (then a default).
    struct MockCommandRunner {
        recorded: RecordedArgv,
        stdin_recorded: RecordedStdin,
        queue: Mutex<VecDeque<MockResp>>,
        default: MockResp,
    }

    impl CommandRunner for MockCommandRunner {
        fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
            self.recorded.lock().unwrap().push(argv.to_vec());
            let resp = self.queue.lock().unwrap().pop_front().unwrap_or_else(|| self.default.clone());
            match resp {
                MockResp::Out(o) => Ok(o),
                MockResp::Io(kind) => Err(std::io::Error::new(kind, "mock runner io error")),
            }
        }

        fn run_with_stdin(
            &self,
            argv: &[String],
            stdin: &str,
        ) -> Result<CommandOutput, std::io::Error> {
            self.stdin_recorded.lock().unwrap().push(stdin.to_string());
            self.run(argv)
        }
    }

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput { success: true, code: Some(0), stdout: stdout.to_string(), stderr: String::new() }
    }
    fn fail(code: i32, stderr: &str) -> CommandOutput {
        CommandOutput { success: false, code: Some(code), stdout: String::new(), stderr: stderr.to_string() }
    }

    /// Build a backend over a mock runner: `default` answers every un-queued call; `queued` is drained
    /// first. Returns the backend + the shared recorded-argv handle (read AFTER the call).
    fn backend_with(default: MockResp, queued: Vec<MockResp>) -> (TmuxBackend, RecordedArgv) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdin_recorded = Arc::new(Mutex::new(Vec::new()));
        let runner = MockCommandRunner {
            recorded: Arc::clone(&recorded),
            stdin_recorded,
            queue: Mutex::new(queued.into_iter().collect()),
            default,
        };
        (TmuxBackend::with_runner(Box::new(runner)), recorded)
    }

    fn backend_with_stdin(
        default: MockResp,
        queued: Vec<MockResp>,
    ) -> (TmuxBackend, RecordedArgv, RecordedStdin) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdin_recorded = Arc::new(Mutex::new(Vec::new()));
        let runner = MockCommandRunner {
            recorded: Arc::clone(&recorded),
            stdin_recorded: Arc::clone(&stdin_recorded),
            queue: Mutex::new(queued.into_iter().collect()),
            default,
        };
        (TmuxBackend::with_runner(Box::new(runner)), recorded, stdin_recorded)
    }

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    // ── 1. has_session: exit 0 -> true, exit 1 -> false; argv = `tmux has-session -t <s>` ──────────
    #[test]
    fn has_session_argv_and_exit_code_maps_to_bool() {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        assert!(be.has_session(&SessionName::new("sess")).expect("has_session"), "exit 0 -> true");
        assert_eq!(rec.lock().unwrap()[0], svec(&["tmux", "has-session", "-t", "sess"]));

        let (be, rec) = backend_with(MockResp::Out(fail(1, "can't find session: sess")), vec![]);
        assert!(!be.has_session(&SessionName::new("sess")).expect("has_session"), "exit 1 -> false");
        assert_eq!(rec.lock().unwrap()[0], svec(&["tmux", "has-session", "-t", "sess"]));
    }

    // ── 2. spawn_first / spawn_into frame via tmux_spawn_argv; canned output parses pane id ────────
    #[test]
    fn spawn_first_frames_via_new_session_builder_and_parses_pane_id() {
        let (be, rec) = backend_with(MockResp::Out(ok("%3")), vec![]);
        let s = SessionName::new("teamsess");
        let w = WindowName::new("w1");
        let env = BTreeMap::from([("TEAM_AGENT_ID".to_string(), "w1".to_string())]);
        let result = be
            .spawn_first(&s, &w, &svec(&["provider-bin", "--flag"]), Path::new("/work/dir"), &env)
            .expect("spawn_first");
        let argv = rec.lock().unwrap()[0].clone();
        let cmd = argv.last().expect("the sh -lc command string").clone();
        assert_eq!(
            argv,
            tmux_spawn_argv(&s, &w, &cmd, true),
            "spawn_first must frame via tmux_spawn_argv (new-session -d -s <s> -n <w> sh -lc <cmd>)"
        );
        assert!(cmd.contains("provider-bin"), "the provider argv must be in the sh -lc command; got {cmd}");
        assert_eq!(result.pane_id.as_str(), "%3", "SpawnResult.pane_id must parse from the tmux output");
    }

    #[test]
    fn spawn_into_frames_via_new_window_builder() {
        let (be, rec) = backend_with(MockResp::Out(ok("%4")), vec![]);
        let s = SessionName::new("teamsess");
        let w = WindowName::new("w2");
        let result = be
            .spawn_into(&s, &w, &svec(&["provider-bin"]), Path::new("/work/dir"), &BTreeMap::new())
            .expect("spawn_into");
        let argv = rec.lock().unwrap()[0].clone();
        let cmd = argv.last().expect("the sh -lc command string").clone();
        assert_eq!(
            argv,
            tmux_spawn_argv(&s, &w, &cmd, false),
            "spawn_into must frame via tmux_spawn_argv first=false (new-window -t <s> -n <w> sh -lc <cmd>)"
        );
        assert_eq!(result.pane_id.as_str(), "%4");
    }

    // ── 3. set_session_env: argv = `tmux set-environment -t <s> <k> <v>`; success -> Applied ───────
    #[test]
    fn set_session_env_argv_and_applied_outcome() {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        let outcome = be.set_session_env(&SessionName::new("sess"), "KEY", "VAL").expect("set env");
        assert_eq!(rec.lock().unwrap()[0], svec(&["tmux", "set-environment", "-t", "sess", "KEY", "VAL"]));
        assert_eq!(outcome, SetEnvOutcome::Applied, "tmux set-environment success -> SetEnvOutcome::Applied");
    }

    // ── 4. capture: argv = tmux_capture_argv; canned scrollback -> normalize_capture -> CapturedText ─
    #[test]
    fn capture_argv_and_normalizes_scrollback() {
        let scroll = "line one  \nbusy\u{a0}marker   \n  \n";
        let (be, rec) = backend_with(MockResp::Out(ok(scroll)), vec![]);
        let pane = PaneId::new("%7");
        let captured = be
            .capture(&Target::Pane(pane.clone()), CaptureRange::Tail(40))
            .expect("capture");
        assert_eq!(rec.lock().unwrap()[0], tmux_capture_argv(&pane, CaptureRange::Tail(40)));
        assert_eq!(captured.text, normalize_capture(scroll), "capture output must be normalize_capture'd");
        assert_eq!(captured.range, CaptureRange::Tail(40));
    }

    // ── 5a. send_keys: argv = tmux_send_keys_argv ──────────────────────────────────────────────────
    #[test]
    fn send_keys_argv_matches_builder() {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        let pane = PaneId::new("%7");
        be.send_keys(&Target::Pane(pane.clone()), &[Key::Enter]).expect("send_keys");
        assert_eq!(rec.lock().unwrap()[0], tmux_send_keys_argv(&pane, &[Key::Enter]));
    }

    // ── 5b. inject (text): set/load-buffer(text) -> paste-buffer -p -> submit send-keys; report Submit ─
    #[test]
    fn inject_text_runs_buffer_paste_submit_sequence_and_reports_submit() {
        let (be, rec) = backend_with(MockResp::Out(ok("hello")), vec![]);
        let pane = PaneId::new("%7");
        let report = be
            .inject(&Target::Pane(pane.clone()), &InjectPayload::Text("hello".to_string()), Key::Enter, true)
            .expect("inject");
        let calls = rec.lock().unwrap().clone();
        let is = |a: &[String], sub: &str| a.get(1).map(String::as_str) == Some(sub);
        assert!(
            calls.iter().any(|a| (is(a, "set-buffer") || is(a, "load-buffer")) && a.iter().any(|x| x.contains("hello"))),
            "inject must stage the text into a tmux buffer (set-buffer/load-buffer); got {calls:?}"
        );
        assert!(
            calls.iter().any(|a| is(a, "paste-buffer") && a.contains(&"-p".to_string()) && a.contains(&"%7".to_string())),
            "inject must bracketed-paste (-p) the buffer to the pane; got {calls:?}"
        );
        assert!(
            calls.iter().any(|a| is(a, "send-keys") && a.contains(&"Enter".to_string())),
            "inject must send the submit key (Enter) last; got {calls:?}"
        );
        assert_eq!(report.stage_reached, InjectStage::Submit, "a fully-applied inject reaches the Submit stage");
        assert_eq!(report.inject_verification, InjectVerification::NoToken);
        assert_eq!(
            report.submit_verification,
            SubmitVerification::EnterSentWithoutPlaceholderCheck
        );
        assert_eq!(report.turn_verification, TurnVerification::NotYetObserved);
    }

    #[test]
    fn inject_large_text_load_buffer_writes_stdin_and_token_report() {
        let (be, rec, stdin_rec) = backend_with_stdin(MockResp::Out(ok("")), vec![]);
        let text = format!("{}{}", "x".repeat(16 * 1024), " [team-agent-token:abc]");
        let report = be
            .inject(&Target::Pane(PaneId::new("%7")), &InjectPayload::Text(text.clone()), Key::Down, true)
            .expect("inject large text");

        assert_eq!(report.inject_verification, InjectVerification::CaptureContainsToken);
        assert_eq!(
            report.submit_verification,
            SubmitVerification::KeySentAfterVisibleToken { key: Key::Down }
        );
        let calls = rec.lock().unwrap().clone();
        assert_eq!(calls[0], svec(&["tmux", "load-buffer", "-b", "team-agent-send-abc", "-"]));
        assert_eq!(stdin_rec.lock().unwrap()[0], text);
    }

    #[test]
    fn send_keys_cancel_mode_queries_mode_and_dispatches_cancel_argv() {
        let (be, rec) = backend_with(
            MockResp::Out(ok("")),
            vec![MockResp::Out(ok("tree-mode\n")), MockResp::Out(ok(""))],
        );
        be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::CancelMode])
            .expect("cancel mode");

        let calls = rec.lock().unwrap().clone();
        assert_eq!(
            calls[0],
            svec(&["tmux", "display-message", "-p", "-t", "%7", "#{pane_mode}"])
        );
        assert_eq!(calls[1], svec(&["tmux", "send-keys", "-t", "%7", "q"]));
    }

    #[test]
    fn cancel_mode_numeric_zero_is_input_ready_and_does_not_send_cancel() {
        // Golden /tmp/transport_golden_probe.py:
        // `_normalize_pane_mode("0") == ""`; `_prepare_tmux_pane_for_input` returns
        // pane_input_ready and does NOT call `_pane_mode_cancel`.
        // RED: pane_mode_from_raw("0") maps to Unknown, so Rust sends `-X cancel`.
        let (be, rec) = backend_with(
            MockResp::Out(ok("")),
            vec![MockResp::Out(ok("0\n"))],
        );
        be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::CancelMode])
            .expect("cancel mode input-ready no-op");

        let calls = rec.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![svec(&["tmux", "display-message", "-p", "-t", "%7", "#{pane_mode}"])],
            "pane_mode='0' is Python input-ready; CancelMode must stop after the mode query, got {calls:?}"
        );
    }

    #[test]
    fn inject_text_uses_message_id_scoped_buffer_from_token() {
        // Golden delivery.py:109-114 passes buffer_name = `team-agent-send-{message_id}` into
        // `_tmux_inject_text`; tmux_io.py then uses that exact name for set/load, paste, delete.
        // This prevents interleaved sends from sharing a stale global tmux buffer.
        // RED: Rust currently hard-codes `team-agent-buf`.
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        let text = "Team Agent message from leader:\n\nhello\n\n[team-agent-token:msg_abc123]".to_string();
        be.inject(&Target::Pane(PaneId::new("%7")), &InjectPayload::Text(text), Key::Enter, true)
            .expect("inject");

        let calls = rec.lock().unwrap().clone();
        let buffer_args: Vec<String> = calls
            .iter()
            .filter(|argv| matches!(argv.get(1).map(String::as_str), Some("set-buffer" | "load-buffer" | "paste-buffer" | "delete-buffer")))
            .filter_map(|argv| argv.iter().position(|arg| arg == "-b").and_then(|i| argv.get(i + 1)).cloned())
            .collect();
        assert_eq!(
            buffer_args,
            vec![
                "team-agent-send-msg_abc123".to_string(),
                "team-agent-send-msg_abc123".to_string(),
                "team-agent-send-msg_abc123".to_string(),
            ],
            "every tmux buffer operation must use the message-id-scoped golden buffer name; calls={calls:?}"
        );
    }

    // ── 6. liveness three-state (§bug-085): exit 0 -> Live; "can't find …" -> Dead; else -> Unknown ─
    #[test]
    fn liveness_is_three_state_unknown_is_not_dead() {
        let (be, rec) = backend_with(MockResp::Out(ok("%7")), vec![]);
        assert_eq!(be.liveness(&PaneId::new("%7")).expect("liveness"), PaneLiveness::Live);
        let argv0 = rec.lock().unwrap()[0].clone();
        assert!(
            argv0.contains(&"display-message".to_string())
                && argv0.iter().any(|x| x.contains("#{pane_id}"))
                && argv0.contains(&"%7".to_string()),
            "liveness must probe the pane via display-message #{{pane_id}}; got {argv0:?}"
        );

        let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane %7")), vec![]);
        assert_eq!(
            be.liveness(&PaneId::new("%7")).expect("liveness"),
            PaneLiveness::Dead,
            "a 'can't find pane' failure -> Dead"
        );

        let (be, _r) = backend_with(MockResp::Out(fail(1, "error connecting to server: No such file or directory")), vec![]);
        assert_eq!(
            be.liveness(&PaneId::new("%7")).expect("liveness"),
            PaneLiveness::Unknown,
            "a NON-'can't find' failure is UNKNOWN, not DEAD (§bug-085 three-state)"
        );
    }

    // ── CP-1: per-team socket — for_workspace injects `-L ta-<hash>` at the run chokepoint; new() does NOT ─
    #[test]
    fn for_workspace_backend_injects_per_team_socket_but_default_backend_does_not() {
        use super::socket_name_for_workspace;
        let ws = Path::new("/tmp/ta-cp1-socket-test-ws");
        let socket = socket_name_for_workspace(ws);
        assert!(
            socket.starts_with("ta-") && socket.len() == 15,
            "socket name must be short + deterministic `ta-<12 hex>`; got {socket:?}"
        );
        // deterministic: the SAME workspace path always derives the SAME socket (CLI == daemon == ops).
        assert_eq!(socket, socket_name_for_workspace(ws), "socket derivation must be deterministic");

        // workspace-bound backend: every executed `tmux` argv gets `-L <socket>` after the leading token.
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let runner = MockCommandRunner {
            recorded: Arc::clone(&recorded),
            stdin_recorded: Arc::new(Mutex::new(Vec::new())),
            queue: Mutex::new(VecDeque::new()),
            default: MockResp::Out(ok("")),
        };
        let be = TmuxBackend::with_runner_for_workspace(Box::new(runner), ws);
        be.has_session(&SessionName::new("sess")).expect("has_session");
        let argv = recorded.lock().unwrap()[0].clone();
        assert_eq!(
            argv,
            svec(&["tmux", "-L", &socket, "has-session", "-t", "sess"]),
            "for_workspace backend must inject `-L <socket>` right after `tmux`; got {argv:?}"
        );

        // default backend (new()/with_runner): NO `-L` — argv stays the golden-locked builder form.
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        be.has_session(&SessionName::new("sess")).expect("has_session");
        assert_eq!(
            rec.lock().unwrap()[0],
            svec(&["tmux", "has-session", "-t", "sess"]),
            "the default-socket backend must NOT inject `-L` (existing tests + non-team callers unaffected)"
        );
    }

    // ── 7. kill_session / kill_window: golden argv; success -> Ok(()) ───────────────────────────────
    #[test]
    fn kill_session_and_kill_window_argv() {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        be.kill_session(&SessionName::new("sess")).expect("kill_session");
        assert_eq!(rec.lock().unwrap()[0], svec(&["tmux", "kill-session", "-t", "sess"]));

        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        be.kill_window(&Target::Pane(PaneId::new("%7"))).expect("kill_window");
        assert_eq!(rec.lock().unwrap()[0], svec(&["tmux", "kill-window", "-t", "%7"]));
    }

    // ── 8. ERROR MAPPING: non-zero tmux exit -> TransportError::Subprocess; runner io::Error -> Err ──
    #[test]
    fn error_paths_map_to_transport_error_not_panic() {
        // tmux cli non-zero exit (the Subprocess variant's documented purpose).
        let (be, _r) = backend_with(MockResp::Out(fail(1, "no server running on /tmp/tmux-x/default")), vec![]);
        let err = be.kill_session(&SessionName::new("sess")).expect_err("kill_session must error on non-zero exit");
        assert!(
            matches!(err, TransportError::Subprocess { code: Some(1), .. }),
            "a non-zero tmux exit must map to TransportError::Subprocess{{code,stderr}}; got {err:?}"
        );

        // a runner io::Error (e.g. tmux not on PATH) must surface as a TransportError, never a panic.
        let (be, _r) = backend_with(MockResp::Io(std::io::ErrorKind::NotFound), vec![]);
        let err = be
            .capture(&Target::Pane(PaneId::new("%7")), CaptureRange::Full)
            .expect_err("capture must surface the runner io error");
        assert!(
            matches!(err, TransportError::Capture { .. } | TransportError::Io(_)),
            "a runner io error must map to a TransportError (not panic); got {err:?}"
        );
    }

    // ── 9. RealCommandRunner GOLDEN 5s TIMEOUT (rt-host-b transient-session race) ────────────────────
    // GOLDEN: terminal.py:12-13 `run_cmd(args, timeout=timeout, check=False)`; runtime.py:1010-1014
    // `_tmux_session_exists` runs `tmux has-session -t <s>` with timeout=5. A has-session that outlives
    // 5s raises `subprocess.TimeoutExpired`, which the coordinator daemon CATCHES
    // (coordinator/__main__.py:60-90 `except Exception`) and treats as a TOLERATED transient
    // (exponential backoff + retry next tick) — it is NEVER read as a definitive "session gone".
    // The 5s subprocess timeout is golden's ONLY tolerance for a slow/hung probe.
    //
    // RUST GAP (THE BUG): `RealCommandRunner::run` (tmux_backend.rs:52) calls
    // `std::process::Command::output()` with NO timeout, so a slow/hung tmux blocks indefinitely.
    // On the (slow) mac mini this is the ~17% single-round-trip flake: a transient slow has-session
    // tears down a healthy team. This is rt-host-b's deterministic 5/5 anchor — `run` on a HUNG
    // command must abandon at the golden 5s and surface `Err(TimedOut)`, NOT block on the full
    // subprocess.
    //
    // RED today: there is no timeout, so `run(["sleep","30"])` blocks ~30s and the `< 6s` bound fails.
    // #[ignore] real-machine: this is the only test here that spawns a real subprocess.
    // PORTER SEAM: add a 5s timeout inside `RealCommandRunner::run` (spawn child + wait-with-timeout
    // via a thread/channel + kill the child on expiry), returning `Err(io::Error, kind TimedOut)` —
    // NO new crate dependency. Keep the existing `CommandRunner::run(&[String]) -> Result<…, io::Error>`
    // signature (the timeout is internal; do not add a parameter).
    #[test]
    #[ignore = "real-machine: spawns a real sleeping subprocess; asserts RealCommandRunner enforces \
                the golden 5s timeout (terminal.py run_cmd timeout / runtime.py:1013 \
                _tmux_session_exists timeout=5)"]
    fn real_command_runner_enforces_golden_5s_timeout_on_hang() {
        use std::time::{Duration, Instant};
        let runner = RealCommandRunner;
        let started = Instant::now();
        let result = runner.run(&svec(&["sleep", "30"]));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(6),
            "RealCommandRunner::run must abandon a hung command at the golden 5s timeout, not block on \
             the full subprocess (terminal.py run_cmd timeout / runtime.py:1013 timeout=5); blocked {elapsed:?}"
        );
        let err = result.expect_err(
            "a command outliving the 5s timeout must surface as Err (subprocess.TimeoutExpired analog) so \
             the daemon backoff path tolerates it, instead of yielding a bogus has-session bool",
        );
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the timeout must be io::ErrorKind::TimedOut (golden: TimeoutExpired -> daemon except -> backoff/retry)"
        );
    }

    // ── 10. query (TRANSPORT TRIO) — single-field display-message; nonzero -> None ──────────────────
    // Golden _legacy_pane_discovery.py:35-39 _tmux_pane_info: `tmux display-message -p -t <target> -F
    // <fmt>` (returncode != 0 -> None), single-field reads at state.py:346 (#{pane_id}) / delivery.py:34
    // (#{pane_width}). The argv is exactly `transport::tmux_query_argv(pane, field)` (the golden-locked
    // builder). RED today: `query` is unimplemented!() -> PANIC. Porter: pane_from_target(target) ->
    // tmux_query_argv -> run; success => Some(stdout.trim()); nonzero => None (never Err).
    #[test]
    fn query_single_field_argv_and_nonzero_maps_to_none() {
        // PaneId field: argv == the golden builder; present value parsed (trimmed) into Some.
        let (be, rec) = backend_with(MockResp::Out(ok("%7\n")), vec![]);
        let got = be.query(&Target::Pane(PaneId::new("%7")), PaneField::PaneId).expect("query ok");
        assert_eq!(
            rec.lock().unwrap()[0],
            tmux_query_argv(&PaneId::new("%7"), PaneField::PaneId),
            "query must build the golden single-field `display-message -p -t <t> -F #{{pane_id}}` argv"
        );
        assert_eq!(got, Some("%7".to_string()), "a present field value is parsed (stripped) into Some");

        // PaneWidth uses -F too; lock argv + the parsed numeric-as-string field.
        let (be, rec) = backend_with(MockResp::Out(ok("180\n")), vec![]);
        let got = be.query(&Target::Pane(PaneId::new("%7")), PaneField::PaneWidth).expect("query ok");
        assert_eq!(rec.lock().unwrap()[0], tmux_query_argv(&PaneId::new("%7"), PaneField::PaneWidth));
        assert_eq!(got, Some("180".to_string()));

        // nonzero exit (pane gone) -> None, NOT an Err (golden _tmux_pane_info: returncode != 0 -> None).
        let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane %7")), vec![]);
        assert_eq!(
            be.query(&Target::Pane(PaneId::new("%7")), PaneField::PaneId).expect("query ok on nonzero"),
            None,
            "a nonzero / pane-gone query must map to None (not Err)"
        );
    }

    // ── 11. list_targets (TRANSPORT TRIO) — `list-panes -a -F TMUX_PANE_FORMAT` + per-line parse ────
    // Golden _legacy_pane_discovery.py:29-33 _tmux_list_panes: `tmux list-panes -a -F <TMUX_PANE_FORMAT>`
    // (returncode != 0 -> []), parse each tab line via _parse_tmux_pane_info. TMUX_PANE_FORMAT
    // (runtime.py:456-460) is the byte-exact 11-field tab string locked below. RED today: list_targets is
    // unimplemented!() -> PANIC. Porter: build the argv, split each stdout line on '\t', map the fields
    // into PaneInfo (pane_active=="1" -> active). leader_env / pane_pid are the reverse-env real-machine
    // bit (no field in TMUX_PANE_FORMAT) — out of this canned parse; the structured fields are locked here.
    #[test]
    fn list_targets_argv_and_parses_tmux_pane_format() {
        const FMT: &str = "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t#{pane_current_path}\t#{session_attached}\t#{pane_in_mode}";
        let stdout = "%7\tteam-x\t0\twin0\t0\t/dev/ttys003\tcodex\t1\t/Users/me/work\t1\t0\n\
                      %8\tteam-x\t1\twin1\t0\t/dev/ttys004\tnode\t0\t/Users/me/other\t0\t0\n";
        let (be, rec) = backend_with(MockResp::Out(ok(stdout)), vec![]);
        let panes = be.list_targets().expect("list_targets ok");
        assert_eq!(
            rec.lock().unwrap()[0],
            svec(&["tmux", "list-panes", "-a", "-F", FMT]),
            "list_targets must run `tmux list-panes -a -F <TMUX_PANE_FORMAT>` (golden _legacy_pane_discovery.py:29)"
        );
        assert_eq!(panes.len(), 2, "one PaneInfo per output line");
        let p = &panes[0];
        assert_eq!(p.pane_id.as_str(), "%7", "field[0] -> pane_id");
        assert_eq!(p.session.as_str(), "team-x", "field[1] -> session_name");
        assert_eq!(p.window_index, Some(0), "field[2] -> window_index (parsed u32)");
        assert_eq!(p.window_name.as_ref().map(|w| w.as_str().to_string()), Some("win0".to_string()), "field[3] -> window_name");
        assert_eq!(p.pane_index, Some(0), "field[4] -> pane_index (parsed u32)");
        assert_eq!(p.tty.as_deref(), Some("/dev/ttys003"), "field[5] -> pane_tty");
        assert_eq!(p.current_command.as_deref(), Some("codex"), "field[6] -> pane_current_command");
        assert!(p.active, "field[7] pane_active='1' -> active=true");
        assert_eq!(
            p.current_path.as_ref().map(|x| x.to_string_lossy().to_string()),
            Some("/Users/me/work".to_string()),
            "field[8] -> pane_current_path"
        );
        assert!(!panes[1].active, "field[7] pane_active='0' -> active=false");

        // nonzero exit -> empty vec (golden returncode != 0 -> []).
        let (be, _r) = backend_with(MockResp::Out(fail(1, "no server running on /tmp/tmux-x/default")), vec![]);
        assert!(
            be.list_targets().expect("list_targets ok on nonzero").is_empty(),
            "a nonzero list-panes must map to an EMPTY Vec (not Err)"
        );
    }

    // ── 12. attach_session (TRANSPORT TRIO) — `tmux attach-session -t <s>` -> Attached ──────────────
    // Golden tmux attach is `tmux attach-session -t <session>`; a successful attach -> AttachOutcome::
    // Attached. RED today: attach_session is unimplemented!() -> PANIC. The in-process lock asserts the
    // argv + outcome via the recording runner; the REAL attach is interactive (takes over the terminal)
    // — that is the real-machine boundary, not unit-testable.
    #[test]
    fn attach_session_argv_and_attached_outcome() {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
        let outcome = be.attach_session(&SessionName::new("sess")).expect("attach ok");
        assert_eq!(
            rec.lock().unwrap()[0],
            svec(&["tmux", "attach-session", "-t", "sess"]),
            "attach_session must run `tmux attach-session -t <session>`"
        );
        assert_eq!(outcome, AttachOutcome::Attached, "a successful tmux attach -> AttachOutcome::Attached");
    }

    // ── 13. TARGET-SCAN WIRING (a): list_targets is the LIVE pane-discovery primitive ───────────────
    // WAVE-2 Lane C. `list_targets` (the `tmux list-panes -a` scan, locked argv/parse in test #11) has
    // ZERO production callers today — it is dead code. Golden wires pane discovery on top of it: status
    // (_capture_missing_sessions / _tmux_session_exists, queries.py:46,52) and doctor (coordinator_health)
    // consume the live scan. The in-process wiring obligation is exercised at the status level by
    // cli::tests::status_tmux_session_present_uses_live_tmux_probe_not_is_some (RED). This #[ignore]
    // real-machine seam locks that a LIVE `list_targets` actually enumerates the running panes, proving
    // the primitive is usable by the status/doctor discovery the porter must wire.
    #[test]
    #[ignore = "real-machine: needs a live tmux server+session; asserts list_targets() (the dangling \
                pane-discovery primitive, zero production callers) enumerates live panes so status/doctor \
                discovery can consume it (golden _legacy_pane_discovery list-panes -a)"]
    fn list_targets_is_live_pane_discovery_primitive_for_status_doctor() {
        let be = TmuxBackend::with_runner(Box::new(RealCommandRunner));
        let panes = be.list_targets().expect("live list_targets must not error");
        assert!(
            !panes.is_empty(),
            "a live `tmux list-panes -a` must surface the running panes; status/doctor pane discovery \
             is wired on top of this scan (currently dead code — zero production callers)"
        );
    }

    // ── 14. TARGET-SCAN WIRING (b): R1 — caller_target.uuid is FIRST leader_session_uuid precedence ──
    // WAVE-2 Lane C / wave2-laneB-rereview PROBE-D. When the caller-target scan lands, golden
    // claim_lease_no_incident threads `_target_leader_session_uuid(caller_target)` as the FIRST
    // leader_session_uuid precedence (leader/__init__.py:679-684): caller_target.uuid BEFORE
    // owner.uuid / receiver.uuid / derived. A DIFFERENT live pane reclaiming a DEAD owner must persist
    // the CALLER's uuid, not the dead owner's (PROBE-D: PY "NEWUUID" / RUST persists "OLD"). The
    // caller-target uuid is read from the caller pane's INJECTED TEAM_AGENT_LEADER_SESSION_UUID via a
    // per-pane env query (NOT a TMUX_PANE_FORMAT field), so the live scan is the dependency this seam
    // marks. SCOPE NOTE: the decisive IN-PROCESS claim-path R1 RED belongs in leader/tests.rs, which is
    // outside this task's (cli + tmux_backend) editor scope — flagged to the leader for the
    // leader-contracts agent to graduate R1 to its own claim-path RED.
    #[test]
    #[ignore = "real-machine + SCOPE: R1 (PROBE-D) caller_target.uuid is FIRST leader_session_uuid \
                precedence (leader/__init__.py:679-684); the in-process claim-path assertion lives in \
                leader/tests.rs (out of cli+tmux_backend scope) — this seam marks the live caller-target \
                env-scan dependency"]
    fn r1_caller_target_uuid_is_first_leader_session_uuid_precedence_seam() {
        // The caller-target scan (reading the caller pane's injected TEAM_AGENT_LEADER_SESSION_UUID)
        // is the live precursor to R1's uuid precedence. The full uuid-persistence assertion is the
        // leader claim path's obligation (see report). Here we only confirm the scan is reachable.
        let be = TmuxBackend::with_runner(Box::new(RealCommandRunner));
        let _panes = be.list_targets().expect("live list_targets (caller-target scan precursor)");
    }
