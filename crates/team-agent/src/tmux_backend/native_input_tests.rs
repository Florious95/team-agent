#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::model::enums::Provider;
use crate::transport::native_input::with_native_input;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    UnknownBefore,
    UnknownAfterPaste,
    UnknownAfterEnter,
    DelayedReady,
    Swallow,
    NonceRace,
    NonceSetFailed,
    NonceBindingChanged,
    TransientUnknown,
    R6IdleNotice,
    PiFolded,
    PiFoldedUnstable,
    CursorFollowup,
    CursorHeaderEvicted,
    CursorHistoryOnly,
    CursorLongHistory,
    CursorExpanded,
    CursorExpandedDirty,
}
struct State {
    mode: Mode,
    captures: usize,
    stage_captures: [usize; 3],
    pasted: usize,
    entered: usize,
    calls: Vec<Vec<String>>,
    nonce: String,
    nonce_sets: usize,
    busy: bool,
    provider: Provider,
    folded_label: String,
}
struct Runner(Arc<Mutex<State>>);
fn pi(composer: &str, history: &str, busy: bool) -> String {
    format!("{history}\n{}\n{composer}\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n",
        if busy {"── ⠋ Working ──────────────────"} else {"────────────────────"})
}
impl CommandRunner for Runner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(argv.to_vec());
        let marker = "[team-agent-token:msg_one]";
        let stdout = if argv.iter().any(|arg| arg == "display-message") {
            let window = if matches!(state.mode, Mode::NonceBindingChanged) && state.nonce_sets > 0
            {
                "replaced"
            } else {
                "worker"
            };
            let height = if matches!(
                state.mode,
                Mode::R6IdleNotice
                    | Mode::PiFolded
                    | Mode::PiFoldedUnstable
                    | Mode::CursorHistoryOnly
                    | Mode::CursorLongHistory
                    | Mode::CursorExpanded
                    | Mode::CursorExpandedDirty
            ) {
                24
            } else {
                120
            };
            format!(
                "/tmp/owned-test.sock|%1|{}|session|{window}|{}|0|{height}\n",
                std::process::id(),
                state.nonce
            )
        } else if argv.iter().any(|arg| arg == "set-option") {
            assert!(argv.iter().any(|arg| arg == "-p"));
            assert!(argv.iter().any(|arg| arg == "-o"));
            state.nonce_sets += 1;
            if matches!(state.mode, Mode::NonceSetFailed | Mode::NonceRace) {
                let stderr = if matches!(state.mode, Mode::NonceRace) {
                    state.nonce = "atomic-winner".into();
                    "already set"
                } else {
                    "fixture nonce write failed"
                };
                return Ok(CommandOutput {
                    success: false,
                    code: Some(1),
                    stdout: String::new(),
                    stderr: stderr.into(),
                });
            }
            assert!(
                state.nonce.is_empty(),
                "existing worker nonce must never be overwritten"
            );
            state.nonce = argv.last().unwrap().clone();
            String::new()
        } else if argv.iter().any(|arg| arg == "show-options") {
            format!("{}\n", state.nonce)
        } else if argv.iter().any(|arg| arg == "capture-pane") {
            state.captures += 1;
            let stage = if state.pasted == 0 {
                0
            } else if state.entered == 0 {
                1
            } else {
                2
            };
            state.stage_captures[stage] += 1;
            if (matches!(state.mode, Mode::TransientUnknown) && state.stage_captures[stage] <= 2)
                || matches!(state.mode, Mode::UnknownBefore)
                || (state.pasted > 0
                    && state.entered == 0
                    && matches!(state.mode, Mode::UnknownAfterPaste))
                || (state.entered > 0 && matches!(state.mode, Mode::UnknownAfterEnter))
                || (matches!(state.mode, Mode::PiFoldedUnstable)
                    && stage == 1
                    && state.stage_captures[stage] > 1)
            {
                "startup warning/raw echo".into()
            } else if matches!(state.mode, Mode::PiFolded | Mode::PiFoldedUnstable) {
                let folded = include_str!("../../tests/fixtures/pi-r7-folded-24.txt");
                let composer = if stage == 1 {
                    state.folded_label.as_str()
                } else {
                    ""
                };
                let frame = folded.replace("[paste #1 +30 lines]", composer);
                let frame = if state.busy {
                    frame.replace(
                        "──────────────────────────────────────────────────────────────────── pi-target ─",
                        "── ⠋ Working ──────────────────────────────────────────────────────────────────",
                    )
                } else {
                    frame
                };
                if stage == 2 {
                    format!("{marker}\n{frame}")
                } else {
                    frame
                }
            } else if matches!(state.mode, Mode::R6IdleNotice) {
                let idle = include_str!("../../tests/fixtures/pi-r6-idle-24.txt");
                if state.entered > 0 {
                    format!("{marker}\n{idle}")
                } else if state.pasted > 0 {
                    idle.replace(
                        " pi-target ─\n\n",
                        &format!(" pi-target ─\nline\n{marker}\n"),
                    )
                } else {
                    idle.into()
                }
            } else if state.entered > 0
                && (!matches!(state.mode, Mode::Swallow) || state.entered >= 2)
            {
                pi("", marker, false)
            } else if state.pasted > 0 {
                pi(&format!("line\n{marker}"), "old", state.busy)
            } else if matches!(state.mode, Mode::DelayedReady) && state.captures <= 2 {
                pi("existing draft", "old", false)
            } else {
                pi("", "old", false)
            }
        } else {
            if argv.iter().any(|arg| arg == "paste-buffer") {
                state.pasted += 1;
            }
            if argv.iter().any(|arg| arg == "send-keys") {
                assert_eq!(argv.last().unwrap(), "Enter");
                state.entered += 1;
            }
            String::new()
        };
        let stdout = if state.provider == Provider::CursorAgent
            && argv.iter().any(|arg| arg == "capture-pane")
        {
            let followup = matches!(
                state.mode,
                Mode::CursorFollowup
                    | Mode::CursorHeaderEvicted
                    | Mode::CursorHistoryOnly
                    | Mode::CursorLongHistory
                    | Mode::CursorExpanded
                    | Mode::CursorExpandedDirty
            );
            let placeholder = if followup {
                "Add a follow-up"
            } else {
                "Plan, search, build anything"
            };
            let header = if matches!(state.mode, Mode::CursorFollowup)
                || (matches!(state.mode, Mode::CursorHeaderEvicted) && state.pasted > 0)
            {
                ""
            } else {
                "Cursor Agent\nv2026.10.01-e373342\n"
            };
            let (composer, history) = if state.entered > 0 {
                (placeholder, "[Pasted text #2 +25 lines]")
            } else if state.pasted > 0 {
                ("[Pasted text #2 +25 lines]", "[Pasted text #1 +25 lines]")
            } else {
                (placeholder, "[Pasted text #1 +25 lines]")
            };
            let padding = if matches!(state.mode, Mode::CursorLongHistory) {
                "historical transcript row\n".repeat(70)
            } else {
                String::new()
            };
            let frame = format!(
                "{header}{history}\n{padding}→ {composer}\nGrok 4.7 High Run Everything\n/owned/project\n"
            );
            if matches!(state.mode, Mode::CursorExpanded | Mode::CursorExpandedDirty) {
                let expanded = include_str!("../../tests/fixtures/cursor-followup-expanded.txt");
                if matches!(state.mode, Mode::CursorExpandedDirty)
                    || state.pasted > 0 && state.entered == 0
                {
                    format!("[Pasted text #1 +30 lines]\n{expanded}")
                } else if state.entered > 0 {
                    format!("{marker}\n→ Add a follow-up\nGrok 4.7 256K High · 8.6% Run Everything\n/owned/project\n")
                } else {
                    "[Pasted text #1 +30 lines]\n→ Add a follow-up\nGrok 4.7 256K High · 8.6% Run Everything\n/owned/project\n".into()
                }
            } else if matches!(state.mode, Mode::CursorHistoryOnly) {
                format!("{frame}{}", "unknown current row\n".repeat(24))
            } else {
                frame
            }
        } else {
            stdout
        };
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }
}
struct Fixture {
    path: std::path::PathBuf,
    backend: TmuxBackend,
    state: Arc<Mutex<State>>,
}
impl Fixture {
    fn new(mode: Mode) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ta-native-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        let state = Arc::new(Mutex::new(State {
            mode,
            captures: 0,
            stage_captures: [0; 3],
            pasted: 0,
            entered: 0,
            calls: vec![],
            nonce: "owned-nonce".into(),
            nonce_sets: 0,
            busy: false,
            provider: Provider::Pi,
            folded_label: "[paste #1 +30 lines]".into(),
        }));
        let backend =
            TmuxBackend::with_runner_for_workspace(Box::new(Runner(state.clone())), &path);
        Self {
            path,
            backend,
            state,
        }
    }
    fn request(&self) -> NativeInputRequest {
        NativeInputRequest {
            message_id: "msg_one".into(),
            owner_team_id: "team".into(),
            recipient: "worker".into(),
            provider: self.state.lock().unwrap().provider,
            pane_id: "%1".into(),
            pane_pid: std::process::id(),
            session: "session".into(),
            window: "worker".into(),
        }
    }
    fn inject(
        &self,
    ) -> (
        Result<InjectReport, TransportError>,
        Option<NativeInputEvidence>,
    ) {
        with_native_input(Some(self.request()), || {
            self.backend.inject(
                &Target::Pane(PaneId::new("%1")),
                &InjectPayload::Text("line\n[team-agent-token:msg_one]".into()),
                Key::Enter,
                true,
            )
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn native_input_runtime_endpoint_factory_preserves_workspace_and_native_capability() {
    assert!(!crate::transport::test_support::OfflineTransport::new().supports_owned_native_input());
    for field in ["tmux_endpoint", "tmux_socket"] {
        let mut f = Fixture::new(Mode::Normal);
        let mut state = serde_json::json!({});
        state[field] = "/tmp/owned-test.sock".into();
        let selection =
            crate::tmux_backend::tmux_backend_with_runner_for_runtime_state_or_workspace(
                Box::new(Runner(f.state.clone())),
                &f.path,
                Some(&state),
            );
        assert_eq!(
            selection.backend.event_workspace.as_deref(),
            Some(f.path.as_path())
        );
        assert_eq!(
            selection.tmux_endpoint_used.as_deref(),
            Some("/tmp/owned-test.sock")
        );
        assert!(selection.backend.supports_owned_native_input());
        f.backend = selection.backend;
        let (report, evidence) = f.inject();
        assert_eq!(
            report.unwrap().submit_verification,
            SubmitVerification::EnterSentWithoutPlaceholderCheck
        );
        assert_eq!(evidence.unwrap().phase, NativeInputPhase::Accepted);
        let observed = f.state.lock().unwrap();
        assert_eq!(observed.pasted, 1);
        assert_eq!(observed.entered, 1);
        assert!(observed.calls.iter().all(|argv| argv
            .windows(2)
            .any(|pair| pair == ["-S", "/tmp/owned-test.sock"])));
    }
}

#[test]
fn native_input_installs_missing_worker_nonce_once_and_preserves_atomic_winner() {
    let existing = Fixture::new(Mode::Normal);
    assert!(existing.inject().0.is_ok());
    assert_eq!(existing.state.lock().unwrap().nonce_sets, 0);
    assert_eq!(existing.state.lock().unwrap().nonce, "owned-nonce");
    for mode in [Mode::Normal, Mode::NonceRace] {
        let f = Fixture::new(mode);
        f.state.lock().unwrap().nonce.clear();
        let (report, evidence) = f.inject();
        assert!(report.is_ok());
        let evidence = evidence.unwrap();
        assert_eq!(evidence.phase, NativeInputPhase::Accepted);
        let observed = f.state.lock().unwrap();
        assert_eq!(observed.nonce_sets, 1);
        assert_eq!(evidence.binding.binding_nonce, observed.nonce);
        assert!(!observed.nonce.is_empty());
        if matches!(mode, Mode::NonceRace) {
            assert_eq!(observed.nonce, "atomic-winner");
        }
        assert_eq!((observed.pasted, observed.entered), (1, 1));
    }
}

#[test]
fn native_input_nonce_setup_requires_exact_selected_worker_before_any_write() {
    for field in ["pane", "pid", "session", "window"] {
        let f = Fixture::new(Mode::Normal);
        f.state.lock().unwrap().nonce.clear();
        let mut request = f.request();
        match field {
            "pane" => request.pane_id = "%2".into(),
            "pid" => request.pane_pid += 1,
            "session" => request.session = "other".into(),
            "window" => request.window = "other".into(),
            _ => unreachable!(),
        }
        let (report, evidence) = with_native_input(Some(request), || {
            f.backend.inject(
                &Target::Pane(PaneId::new("%1")),
                &InjectPayload::Text("line\n[team-agent-token:msg_one]".into()),
                Key::Enter,
                true,
            )
        });
        assert!(report.is_err());
        assert!(evidence.is_none());
        let observed = f.state.lock().unwrap();
        assert_eq!(
            (observed.nonce_sets, observed.pasted, observed.entered),
            (0, 0, 0)
        );
    }
}

#[test]
fn native_input_nonce_write_failure_or_replacement_never_pastes_or_enters() {
    for mode in [Mode::NonceSetFailed, Mode::NonceBindingChanged] {
        let f = Fixture::new(mode);
        f.state.lock().unwrap().nonce.clear();
        let (report, evidence) = f.inject();
        assert!(report.is_err());
        assert!(evidence.is_none());
        let observed = f.state.lock().unwrap();
        assert_eq!(
            (observed.nonce_sets, observed.pasted, observed.entered),
            (1, 0, 0)
        );
    }
}

#[test]
fn native_input_delayed_ready_then_owned_paste_then_exactly_one_enter() {
    let f = Fixture::new(Mode::DelayedReady);
    let (report, evidence) = f.inject();
    assert_eq!(
        report.unwrap().submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(evidence.unwrap().phase, NativeInputPhase::Accepted);
    let state = f.state.lock().unwrap();
    assert_eq!(state.pasted, 1);
    assert_eq!(state.entered, 1);
    let paste = state
        .calls
        .iter()
        .position(|argv| argv.iter().any(|arg| arg == "paste-buffer"))
        .unwrap();
    assert!(
        state.calls[..paste]
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "capture-pane"))
            .count()
            >= 4
    );
    let submit = state
        .calls
        .iter()
        .position(|argv| argv.iter().any(|arg| arg == "send-keys"))
        .unwrap();
    assert!(
        state.calls[paste + 1..submit]
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "capture-pane"))
            .count()
            >= 2
    );
}

#[test]
fn native_input_r6_idle_notice_reaches_owned_paste_and_one_submit() {
    let f = Fixture::new(Mode::R6IdleNotice);
    let (report, evidence) = f.inject();
    assert_eq!(
        report.unwrap().submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    let evidence = evidence.unwrap();
    assert_eq!(evidence.binding.pane_height, 24);
    assert_eq!(evidence.phase, NativeInputPhase::Accepted);
    let state = f.state.lock().unwrap();
    assert_eq!((state.pasted, state.entered), (1, 1));
    assert!(state.stage_captures[0] >= 2);
    assert!(state.stage_captures[1] >= 2);
}

#[test]
fn native_input_capsule_baseline_exclusion_is_provider_aware() {
    let history =
        "old [paste #1 +30 lines] tail\n[paste #2 1234 chars]\n[Pasted text #3 +25 lines]\n";
    assert_eq!(
        capsule_labels(Provider::Pi, history),
        vec!["[paste #1 +30 lines]", "[paste #2 1234 chars]"]
    );
    assert_eq!(
        capsule_labels(Provider::CursorAgent, history),
        vec!["[Pasted text #3 +25 lines]"]
    );
}

#[test]
fn native_input_pi_folded_paste_persists_owned_latch_before_single_submit() {
    let f = Fixture::new(Mode::PiFolded);
    let (result, evidence) = f.inject();
    assert!(result.is_ok());
    let evidence = evidence.unwrap();
    assert_eq!(evidence.phase, NativeInputPhase::Accepted);
    assert_eq!(evidence.capsule.as_deref(), Some("[paste #1 +30 lines]"));
    assert_eq!(evidence.binding.pane_height, 24);
    let events: Vec<serde_json::Value> =
        std::fs::read_to_string(f.path.join(".team/logs/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let latch = events.iter().position(|event| {
        event["event"] == "native_input.evidence"
            && event["phase"] == "pasted"
            && event["capsule"] == "[paste #1 +30 lines]"
    });
    let seen = events.iter().position(|event| event["phase"] == "seen");
    assert!(latch.unwrap() < seen.unwrap());
    let state = f.state.lock().unwrap();
    assert_eq!((state.pasted, state.entered), (1, 1));
}

#[test]
fn native_input_pi_rescue_retains_latch_but_never_adopts_unowned_residue() {
    let f = Fixture::new(Mode::PiFoldedUnstable);
    let (result, evidence) = f.inject();
    assert!(matches!(
        result,
        Err(TransportError::NativeInputNotReady { reason }) if reason == "owned_paste_not_stable"
    ));
    let evidence = evidence.unwrap();
    assert_eq!(evidence.phase, NativeInputPhase::Pasted);
    assert_eq!(evidence.capsule.as_deref(), Some("[paste #1 +30 lines]"));
    assert_eq!(f.state.lock().unwrap().entered, 0);
    f.state.lock().unwrap().mode = Mode::PiFolded;
    for mutation in ["missing_latch", "wrong_latch", "baseline_label"] {
        let mut invalid = evidence.clone();
        match mutation {
            "missing_latch" => invalid.capsule = None,
            "wrong_latch" => invalid.capsule = Some("[paste #2 +30 lines]".into()),
            "baseline_label" => invalid
                .baseline_capsules
                .push("[paste #1 +30 lines]".into()),
            _ => unreachable!(),
        }
        assert_eq!(
            f.backend
                .rescue_native_input(&invalid, &|| true, None)
                .unwrap(),
            EnterRescueOutcome::NoEffect {
                reason: "residue_mismatch",
                reobserve: false,
            }
        );
    }
    for label in [
        "[paste #2 +30 lines]",
        "[paste #1 +30 lines]\nforeign draft",
    ] {
        f.state.lock().unwrap().folded_label = label.into();
        assert_eq!(
            f.backend
                .rescue_native_input(&evidence, &|| true, None)
                .unwrap(),
            EnterRescueOutcome::NoEffect {
                reason: "residue_mismatch",
                reobserve: false,
            }
        );
    }
    f.state.lock().unwrap().folded_label = "[paste #1 +30 lines]".into();
    f.state.lock().unwrap().busy = true;
    assert_eq!(
        f.backend
            .rescue_native_input(&evidence, &|| true, None)
            .unwrap(),
        EnterRescueOutcome::NoEffect {
            reason: "busy",
            reobserve: true,
        }
    );
    f.state.lock().unwrap().busy = false;
    assert_eq!(f.state.lock().unwrap().entered, 0);
    assert_eq!(
        f.backend
            .rescue_native_input(&evidence, &|| true, None)
            .unwrap(),
        EnterRescueOutcome::Entered { accepted: true }
    );
    assert!(matches!(
        f.backend
            .rescue_native_input(&evidence, &|| true, None)
            .unwrap(),
        EnterRescueOutcome::NoEffect { .. }
    ));
    let state = f.state.lock().unwrap();
    assert_eq!((state.pasted, state.entered), (1, 1));
}

#[test]
fn native_input_partial_redraws_only_reobserve_until_known_stable_single_submit() {
    let f = Fixture::new(Mode::TransientUnknown);
    let (report, evidence) = f.inject();
    assert_eq!(
        report.unwrap().submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(evidence.unwrap().phase, NativeInputPhase::Accepted);
    let observed = f.state.lock().unwrap();
    assert!(observed.stage_captures.iter().all(|count| *count >= 3));
    assert_eq!((observed.pasted, observed.entered), (1, 1));
}

#[test]
fn native_input_unknown_before_paste_is_zero_effect_after_paste_never_enters() {
    for mode in [Mode::UnknownBefore, Mode::UnknownAfterPaste] {
        let f = Fixture::new(mode);
        let (result, evidence) = f.inject();
        assert!(result.is_err());
        let state = f.state.lock().unwrap();
        assert_eq!(state.entered, 0);
        if matches!(mode, Mode::UnknownBefore) {
            assert_eq!(state.pasted, 0);
            assert!(evidence.is_none());
        } else {
            assert_eq!(state.pasted, 1);
            assert_eq!(evidence.unwrap().phase, NativeInputPhase::Pasted);
        }
    }
}

#[test]
fn native_input_post_enter_unknown_retains_effect_floor_without_another_key() {
    let f = Fixture::new(Mode::UnknownAfterEnter);
    let (result, evidence) = f.inject();
    assert!(result.is_err());
    assert_eq!(evidence.unwrap().phase, NativeInputPhase::Submitted);
    let state = f.state.lock().unwrap();
    assert_eq!(state.pasted, 1);
    assert_eq!(state.entered, 1);
}

#[test]
fn native_input_swallowed_enter_only_delayed_rescue_can_press_once_no_repaste() {
    let f = Fixture::new(Mode::Swallow);
    let (report, evidence) = f.inject();
    assert_eq!(
        report.unwrap().submit_verification,
        SubmitVerification::SubmitConsumptionUnverified
    );
    let evidence = evidence.unwrap();
    assert_eq!(evidence.phase, NativeInputPhase::Submitted);
    struct ObservedSubmit(AtomicUsize);
    impl SubmitObserver for ObservedSubmit {
        fn after_physical_submit(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let observer = ObservedSubmit(AtomicUsize::new(0));
    assert_eq!(
        f.backend
            .rescue_enter(&evidence, &|| true, Some(&observer))
            .unwrap(),
        EnterRescueOutcome::Entered { accepted: true }
    );
    let state = f.state.lock().unwrap();
    assert_eq!(state.pasted, 1);
    assert_eq!(state.entered, 2);
    drop(state);
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.backend
            .rescue_enter(&evidence, &|| true, Some(&observer))
            .unwrap(),
        EnterRescueOutcome::Consumed
    );
    assert_eq!(f.state.lock().unwrap().entered, 2);
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
}

#[test]
fn native_input_rescue_binding_busy_and_canonical_changes_never_send_a_key() {
    let f = Fixture::new(Mode::Normal);
    let (_, evidence) = f.inject();
    let mut evidence = evidence.unwrap();
    // Controlled fixture represents the same pending draft, not a new injection.
    {
        let mut state = f.state.lock().unwrap();
        state.entered = 0;
        state.busy = true;
    }
    let before = f.state.lock().unwrap().calls.len();
    assert_eq!(
        f.backend.rescue_enter(&evidence, &|| true, None).unwrap(),
        EnterRescueOutcome::NoEffect {
            reason: "busy",
            reobserve: true
        }
    );
    assert!(!f.state.lock().unwrap().calls[before..]
        .iter()
        .any(|argv| argv.iter().any(|arg| arg == "send-keys")));
    f.state.lock().unwrap().busy = false;
    assert_eq!(
        f.backend.rescue_enter(&evidence, &|| false, None).unwrap(),
        EnterRescueOutcome::NoEffect {
            reason: "canonical_changed",
            reobserve: false
        }
    );
    for field in [
        "birth", "nonce", "endpoint", "pid", "pane", "session", "window",
    ] {
        let mut wrong = evidence.clone();
        match field {
            "birth" => wrong.binding.carrier_birth.push_str("-wrong"),
            "nonce" => wrong.binding.binding_nonce.push_str("-wrong"),
            "endpoint" => wrong.binding.endpoint.push_str("-wrong"),
            "pid" => wrong.binding.pane_pid += 1,
            "pane" => wrong.binding.pane_id = "%2".into(),
            "session" => wrong.binding.session.push_str("-wrong"),
            "window" => wrong.binding.window.push_str("-wrong"),
            _ => unreachable!(),
        }
        assert!(
            f.backend.rescue_enter(&wrong, &|| true, None).is_err(),
            "{field}"
        );
    }
    evidence.phase = NativeInputPhase::MayHavePasted;
    assert_eq!(
        f.backend.rescue_enter(&evidence, &|| true, None).unwrap(),
        EnterRescueOutcome::NoEffect {
            reason: "proof_missing",
            reobserve: false
        }
    );
    assert_eq!(f.state.lock().unwrap().entered, 0);
}

#[test]
fn native_input_cursor_owned_new_capsule_transfers_to_transcript_without_token_claim() {
    let f = Fixture::new(Mode::Normal);
    f.state.lock().unwrap().provider = Provider::CursorAgent;
    let (report, evidence) = f.inject();
    let report = report.unwrap();
    let evidence = evidence.unwrap();
    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureContainsNewPastedContentPrompt
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(
        evidence.capsule.as_deref(),
        Some("[Pasted text #2 +25 lines]")
    );
    assert_eq!(evidence.baseline_capsules, ["[Pasted text #1 +25 lines]"]);
    let state = f.state.lock().unwrap();
    assert_eq!(state.pasted, 1);
    assert_eq!(state.entered, 1);
}

#[test]
fn native_input_cursor_clean_followup_uses_one_paste_one_enter_across_banner_eviction() {
    for mode in [
        Mode::CursorFollowup,
        Mode::CursorHeaderEvicted,
        Mode::CursorLongHistory,
    ] {
        let f = Fixture::new(mode);
        f.state.lock().unwrap().provider = Provider::CursorAgent;
        let (report, evidence) = f.inject();
        let report = report.unwrap();
        assert_eq!(
            report.submit_verification,
            SubmitVerification::EnterSentWithoutPlaceholderCheck
        );
        let evidence = evidence.unwrap();
        assert_eq!(evidence.phase, NativeInputPhase::Accepted);
        assert_eq!(
            evidence.capsule.as_deref(),
            Some("[Pasted text #2 +25 lines]")
        );
        let state = f.state.lock().unwrap();
        assert_eq!((state.pasted, state.entered), (1, 1));
    }
}

#[test]
fn native_input_cursor_expanded_followup_reaches_seen_then_one_enter() {
    let f = Fixture::new(Mode::CursorExpanded);
    f.state.lock().unwrap().provider = Provider::CursorAgent;
    let text = "For a harmless terminal-scroll fixture, reply with exactly 240 numbered lines, one per line, from 001 through 240, each formatted `NNN: amber`; do not use tools or change any files. Then wait.\n\n[team-agent-token:msg_one]";
    let (report, evidence) = with_native_input(Some(f.request()), || {
        f.backend.inject(
            &Target::Pane(PaneId::new("%1")),
            &InjectPayload::Text(text.into()),
            Key::Enter,
            true,
        )
    });
    let report = report.unwrap();
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureContainsToken
    );
    let evidence = evidence.unwrap();
    assert_eq!(evidence.phase, NativeInputPhase::Accepted);
    assert!(evidence.capsule.is_none());
    assert_eq!(evidence.baseline_capsules, ["[Pasted text #1 +30 lines]"]);
    let state = f.state.lock().unwrap();
    assert_eq!((state.pasted, state.entered), (1, 1));
    assert!(state.stage_captures[1] >= 2);
}

#[test]
fn native_input_cursor_foreign_expanded_followup_is_zero_effect() {
    let f = Fixture::new(Mode::CursorExpandedDirty);
    f.state.lock().unwrap().provider = Provider::CursorAgent;
    let (report, _) = f.inject();
    assert!(report.is_err());
    let state = f.state.lock().unwrap();
    assert_eq!((state.pasted, state.entered), (0, 0));
}

#[test]
fn native_input_cursor_history_only_composer_cannot_authorize_paste_or_enter() {
    let f = Fixture::new(Mode::CursorHistoryOnly);
    f.state.lock().unwrap().provider = Provider::CursorAgent;
    let (result, _) = f.inject();
    let state = f.state.lock().unwrap();
    assert_eq!(
        (state.pasted, state.entered),
        (0, 0),
        "old history is not current composer authority"
    );
    assert!(result.is_err());
}

#[test]
fn native_input_buffer_uses_authoritative_message_not_first_quoted_marker() {
    let f = Fixture::new(Mode::Normal);
    let quoted = "[team-agent-token:msg_old]\nline\n[team-agent-token:msg_one]";
    let (_, evidence) = with_native_input(Some(f.request()), || {
        f.backend.inject(
            &Target::Pane(PaneId::new("%1")),
            &InjectPayload::Text(quoted.into()),
            Key::Enter,
            true,
        )
    });
    let evidence = evidence.unwrap();
    let buffer = owned_buffer(&evidence);
    assert!(buffer.contains("msg_one"));
    assert!(!buffer.contains("msg_old"));
    let state = f.state.lock().unwrap();
    for argv in state.calls.iter().filter(|argv| {
        argv.iter()
            .any(|arg| arg == "set-buffer" || arg == "paste-buffer")
    }) {
        let index = argv.iter().position(|arg| arg == "-b").unwrap();
        assert_eq!(argv[index + 1], buffer);
    }
    let mut other = evidence.clone();
    other.request.message_id = "msg_two".into();
    assert_ne!(buffer, owned_buffer(&other));
}

#[test]
fn native_input_required_lock_contention_or_missing_workspace_is_zero_effect() {
    let f = Fixture::new(Mode::Normal);
    let lock = acquire_required(PaneInputLockRequest {
        workspace: Some(&f.path),
        target_key: "%1",
        operation: "fixture",
    })
    .unwrap();
    let (result, evidence) = f.inject();
    assert!(result.is_err());
    assert!(evidence.is_none());
    assert!(f.state.lock().unwrap().calls.is_empty());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(f
                .backend
                .send_keys(&Target::Pane(PaneId::new("%1")), &[Key::Enter])
                .is_err())
        });
        scope.spawn(|| {
            assert!(f
                .backend
                .inject(
                    &Target::Pane(PaneId::new("%1")),
                    &InjectPayload::TextSkipConsumptionPoll("foreign command".into()),
                    Key::Enter,
                    true
                )
                .is_err())
        });
    });
    assert!(f.state.lock().unwrap().calls.is_empty());
    drop(lock);
    let other = TmuxBackend::with_runner(Box::new(Runner(f.state.clone())));
    let (result, evidence) = with_native_input(Some(f.request()), || {
        other.inject(
            &Target::Pane(PaneId::new("%1")),
            &InjectPayload::Text("body".into()),
            Key::Enter,
            true,
        )
    });
    assert!(result.is_err());
    assert!(evidence.is_none());
    assert!(f.state.lock().unwrap().calls.is_empty());
}
