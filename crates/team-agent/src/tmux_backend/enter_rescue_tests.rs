#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::model::enums::Provider;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Busy,
    Unknown,
    Capsule,
    History,
    Changed,
    UnknownAfterEnter,
    UnknownWithReplacement,
}
struct State {
    mode: Mode,
    nonce: String,
    endpoint: String,
    pid: u32,
    captures: usize,
    entered: usize,
    calls: Vec<Vec<String>>,
}
struct Runner(Arc<Mutex<State>>);
fn pi(composer: &str, history: &str, busy: bool) -> String {
    format!("{history}\n{}\n{composer}\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n",if busy{"── ⠋ Working ──────────────────"}else{"────────────────────"})
}
impl CommandRunner for Runner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        let mut s = self.0.lock().unwrap();
        s.calls.push(argv.to_vec());
        let stdout = if argv.iter().any(|a| a == "display-message") {
            let format = argv.last().unwrap();
            if format.contains("socket_path") {
                let nonce = if (matches!(s.mode, Mode::UnknownAfterEnter) && s.entered > 0)
                    || (matches!(s.mode, Mode::UnknownWithReplacement) && s.captures > 0)
                {
                    "replaced"
                } else {
                    &s.nonce
                };
                format!("{}|%1|{}|session|worker|{nonce}|0|120\n", s.endpoint, s.pid)
            } else if format == "#{pane_height}" {
                "120\n".into()
            } else if format == "#{pane_pid}" {
                format!("{}\n", s.pid)
            } else {
                "0\n".into()
            }
        } else if argv.iter().any(|a| a == "set-option") {
            assert!(s.nonce.is_empty());
            assert!(argv.iter().any(|a| a == "-o"));
            s.nonce = argv.last().unwrap().clone();
            String::new()
        } else if argv.iter().any(|a| a == "show-options") {
            format!("{}\n", s.nonce)
        } else if argv.iter().any(|a| a == "capture-pane") {
            s.captures += 1;
            let marker = "[team-agent-token:msg_one]";
            match s.mode {
                Mode::Unknown | Mode::UnknownWithReplacement => "unknown native screen".into(),
                Mode::Capsule => pi("[paste #1 +30 lines]", "", false),
                Mode::History => pi("", marker, false),
                Mode::Changed if s.captures >= 3 => pi("different draft", "", false),
                Mode::Busy => pi(marker, "", true),
                _ if s.entered > 0 => pi("", marker, false),
                _ => pi(marker, "", false),
            }
        } else if argv.iter().any(|a| a == "send-keys") {
            assert_eq!(argv.last().unwrap(), "Enter");
            s.entered += 1;
            String::new()
        } else {
            panic!("unexpected transport operation {argv:?}")
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
            "ta-independent-rescue-backend-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        let state = Arc::new(Mutex::new(State {
            mode,
            nonce: "original-nonce".into(),
            endpoint: "/tmp/owned.sock".into(),
            pid: std::process::id(),
            captures: 0,
            entered: 0,
            calls: vec![],
        }));
        let backend =
            TmuxBackend::with_runner_for_workspace(Box::new(Runner(state.clone())), &path);
        Self {
            path,
            backend,
            state,
        }
    }
    fn context(&self) -> EnterRescueContext {
        EnterRescueContext {
            message_id: "msg_one".into(),
            owner_team_id: "team".into(),
            recipient: "worker".into(),
            payload_sha256: digest("rendered payload"),
            target: Some(EnterRescueTarget::selected(
                Provider::Pi,
                "%1".into(),
                std::process::id(),
                "session".into(),
                "worker".into(),
            )),
            proof: None,
        }
    }
    fn snapshot(&self) -> EnterRescueContext {
        self.backend
            .snapshot_rescue_context(
                &Target::Pane(PaneId::new("%1")),
                &self.context(),
                "rendered payload",
            )
            .unwrap()
    }
    fn entered(&self) -> usize {
        self.state.lock().unwrap().entered
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn original_boundary_snapshot_records_only_exact_visible_current_marker() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    assert!(c.target.as_ref().unwrap().bound());
    let proof = c.proof.unwrap();
    assert!(proof.visible_marker && proof.transcript_marker_absent);
    assert!(proof.owned_capsule.is_none());
    assert_eq!(f.entered(), 0);
    for mode in [Mode::Unknown, Mode::Capsule, Mode::History] {
        let f = Fixture::new(mode);
        let c = f.snapshot();
        assert!(c.proof.is_none());
        assert_eq!(f.entered(), 0);
    }
}

#[test]
fn snapshot_rejects_wrong_payload_or_selected_carrier_and_creates_nonce_only_when_owned() {
    let f = Fixture::new(Mode::Normal);
    let c = f.context();
    let t = Target::Pane(PaneId::new("%1"));
    assert!(f
        .backend
        .snapshot_rescue_context(&t, &c, "edited")
        .unwrap()
        .proof
        .is_none());
    assert!(f.state.lock().unwrap().calls.is_empty());
    let mut c = f.context();
    c.target.as_mut().unwrap().pane_pid += 1;
    assert!(f
        .backend
        .snapshot_rescue_context(&t, &c, "rendered payload")
        .unwrap()
        .proof
        .is_none());
    let f = Fixture::new(Mode::Capsule);
    f.state.lock().unwrap().nonce.clear();
    assert!(f.snapshot().proof.is_none());
    assert!(f.state.lock().unwrap().nonce.is_empty());
    let f = Fixture::new(Mode::Normal);
    f.state.lock().unwrap().nonce.clear();
    assert!(f.snapshot().target.unwrap().bound());
    assert!(!f.state.lock().unwrap().nonce.is_empty());
    assert_eq!(f.entered(), 0);
}

#[test]
fn watchdog_rechecks_carrier_and_never_adopts_replacement_or_missing_receipt() {
    for changed in ["endpoint", "nonce", "birth", "pid"] {
        let f = Fixture::new(Mode::Normal);
        let mut c = f.snapshot();
        match changed {
            "endpoint" => f.state.lock().unwrap().endpoint = "/tmp/other.sock".into(),
            "nonce" => f.state.lock().unwrap().nonce = "other".into(),
            "birth" => c.target.as_mut().unwrap().carrier_birth = "reused".into(),
            _ => c.target.as_mut().unwrap().pane_pid += 1,
        };
        let _ = f.backend.rescue_guarded_enter(&c, &|| true, None);
        assert_eq!(f.entered(), 0, "{changed}");
    }
    let f = Fixture::new(Mode::Normal);
    assert!(matches!(
        f.backend
            .rescue_guarded_enter(&f.context(), &|| true, None)
            .unwrap(),
        EnterRescueOutcome::NoEffect { .. }
    ));
    assert_eq!(f.entered(), 0);
}

#[test]
fn busy_unstable_unknown_or_final_authorization_failure_sends_no_key() {
    for mode in [Mode::Busy, Mode::Changed] {
        let f = Fixture::new(mode);
        let c = f.snapshot();
        assert!(matches!(
            f.backend.rescue_guarded_enter(&c, &|| true, None).unwrap(),
            EnterRescueOutcome::NoEffect { .. }
        ));
        assert_eq!(f.entered(), 0);
    }
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    f.state.lock().unwrap().mode = Mode::Unknown;
    assert_eq!(
        f.backend.rescue_guarded_enter(&c, &|| true, None).unwrap(),
        no_effect("surface_unknown", true)
    );
    assert_eq!(f.entered(), 0);
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    assert!(matches!(
        f.backend.rescue_guarded_enter(&c, &|| false, None).unwrap(),
        EnterRescueOutcome::NoEffect { .. }
    ));
    assert_eq!(f.entered(), 0);
}

#[test]
fn unknown_observation_still_checks_original_carrier_before_zero_effect_refusal() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    {
        let mut state = f.state.lock().unwrap();
        state.mode = Mode::UnknownWithReplacement;
        state.captures = 0;
    }
    assert!(matches!(
        f.backend.rescue_guarded_enter(&c, &|| true, None),
        Err(TransportError::NativeInputNotReady { reason }) if reason == "rescue_binding_changed"
    ));
    assert_eq!(f.entered(), 0);
}

#[test]
fn required_input_lock_cannot_be_omitted_or_bypassed() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    let _lock = acquire_required(PaneInputLockRequest {
        workspace: Some(&f.path),
        target_key: "%1",
        operation: "fixture-held",
    })
    .unwrap();
    assert_eq!(
        f.backend.rescue_guarded_enter(&c, &|| true, None).unwrap(),
        no_effect("input_lock", true)
    );
    assert_eq!(f.entered(), 0);
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    let backend = TmuxBackend::with_runner(Box::new(Runner(f.state.clone())));
    assert_eq!(
        backend.rescue_guarded_enter(&c, &|| true, None).unwrap(),
        no_effect("input_lock", true)
    );
    assert_eq!(f.entered(), 0);
}

struct Observer(AtomicUsize);
impl SubmitObserver for Observer {
    fn after_physical_submit(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn one_logical_enter_notifies_before_verification_and_never_pastes() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    let observer = Observer(AtomicUsize::new(0));
    assert_eq!(
        f.backend
            .rescue_guarded_enter(&c, &|| true, Some(&observer))
            .unwrap(),
        EnterRescueOutcome::Entered { accepted: true }
    );
    assert_eq!(f.entered(), 1);
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    let s = f.state.lock().unwrap();
    assert!(!s
        .calls
        .iter()
        .flatten()
        .any(|a| matches!(a.as_str(), "paste-buffer" | "load-buffer" | "set-buffer")));
}

#[test]
fn already_consumed_transition_never_sends_another_enter_or_fakes_observer() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    f.state.lock().unwrap().mode = Mode::History;
    let observer = Observer(AtomicUsize::new(0));
    assert_eq!(
        f.backend
            .rescue_guarded_enter(&c, &|| true, Some(&observer))
            .unwrap(),
        EnterRescueOutcome::Consumed
    );
    assert_eq!(f.entered(), 0);
    assert_eq!(observer.0.load(Ordering::SeqCst), 0);
}

#[test]
fn foreign_token_or_bare_capsule_is_not_current_message_key_authority() {
    let f = Fixture::new(Mode::Normal);
    let mut c = f.snapshot();
    let mut frame = f
        .backend
        .rescue_frame(c.target.as_ref().unwrap())
        .unwrap()
        .unwrap();
    frame.composer.push_str("\n[team-agent-token:msg_foreign]");
    assert!(!pending(&c, &frame));
    frame.composer = "[paste #1 +30 lines]".into();
    frame.capsule = Some(frame.composer.clone());
    assert!(!pending(&c, &frame));
    let scope_sha256 = c.scope_sha256().unwrap();
    let proof = c.proof.as_mut().unwrap();
    proof.visible_marker = false;
    proof.owned_capsule = Some(crate::transport::enter_rescue::OwnedCapsule {
        scope_sha256,
        label: frame.composer.clone(),
        baseline_sha256: digest("original baseline"),
        baseline_capsules: vec![],
        observed_after_owned_paste: true,
    });
    assert!(pending(&c, &frame));
    for field in [
        "message",
        "payload",
        "owner",
        "recipient",
        "nonce",
        "birth",
        "endpoint",
    ] {
        let mut reassigned = c.clone();
        match field {
            "message" => reassigned.message_id = "msg_other".into(),
            "payload" => reassigned.payload_sha256 = digest("other payload"),
            "owner" => reassigned.owner_team_id = "other".into(),
            "recipient" => reassigned.recipient = "other".into(),
            "nonce" => reassigned.target.as_mut().unwrap().binding_nonce = "other".into(),
            "birth" => reassigned.target.as_mut().unwrap().carrier_birth = "other".into(),
            _ => reassigned.target.as_mut().unwrap().endpoint = "/tmp/other.sock".into(),
        }
        assert!(!pending(&reassigned, &frame), "{field}");
    }
    c.proof
        .as_mut()
        .unwrap()
        .owned_capsule
        .as_mut()
        .unwrap()
        .baseline_capsules
        .push(frame.composer.clone());
    assert!(!pending(&c, &frame));
}

#[test]
fn unknown_after_enter_is_terminal_uncertainty_not_no_effect() {
    let f = Fixture::new(Mode::UnknownAfterEnter);
    let c = f.snapshot();
    let observer = Observer(AtomicUsize::new(0));
    assert_eq!(
        f.backend
            .rescue_guarded_enter(&c, &|| true, Some(&observer))
            .unwrap(),
        EnterRescueOutcome::Entered { accepted: false }
    );
    assert_eq!(f.entered(), 1);
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
}

#[test]
fn transcript_transfer_requires_empty_not_whitespace_composer_and_unique_marker() {
    let f = Fixture::new(Mode::Normal);
    let c = f.snapshot();
    let proof = c.proof.as_ref().unwrap();
    let mut frame = InputFrame {
        composer: "".into(),
        transcript: c.marker(),
        identity: proof.ui_identity.clone(),
        footer: "changed while working".into(),
        busy: true,
        capsule: None,
    };
    assert!(accepted(&c, &frame));
    frame.composer = " ".into();
    assert!(!accepted(&c, &frame));
    frame.composer.clear();
    frame.transcript = format!("{}{}", c.marker(), c.marker());
    assert!(!accepted(&c, &frame));
    frame.transcript = c.marker();
    let mut c = c;
    c.proof.as_mut().unwrap().transcript_marker_absent = false;
    assert!(!accepted(&c, &frame));
}
