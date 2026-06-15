//! Provider-aware paste submit verification contracts.
//!
//! #243: tmux accepting paste-buffer + one immediate Enter is not proof that Codex/Claude opened a
//! turn. Large provider pastes can become a `[Pasted Content ...]` / `[Pasted text ...]` input block;
//! delivery may be marked `delivered` only after the block is submitted and cleared.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::params;
use serde_json::Value;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::deliver_pending_messages;
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
fn tmux_text_inject_waits_for_pasted_content_block_then_retries_enter_until_cleared() {
    let runner = PastePromptRunner::new([
        "",                                  // baseline/no block
        "[Pasted Content 2048 chars]",       // block appears after paste
        "[Pasted Content 2048 chars]",       // first Enter was off-by-one, block still present
        "OpenAI Codex\n›",                   // retry Enter submitted the current block
    ]);
    let calls = runner.calls();
    let backend = TmuxBackend::with_runner(Box::new(runner));

    let report = backend
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text("structured payload line\n".repeat(12)),
            Key::Enter,
            true,
        )
        .expect("provider-aware inject should succeed once the pasted-content block clears");

    let calls = calls.lock().unwrap().clone();
    let paste_idx = first_call_index(&calls, "paste-buffer").expect("paste-buffer call");
    let first_enter_idx = first_send_enter_index(&calls).expect("first Enter send-keys");
    assert!(
        calls[paste_idx + 1..first_enter_idx]
            .iter()
            .any(|argv| is_tmux_subcommand(argv, "capture-pane")),
        "after paste-buffer, inject must capture/wait for the provider pasted-content block before pressing Enter; calls={calls:?}"
    );
    assert!(
        calls[first_enter_idx + 1..]
            .iter()
            .any(|argv| is_tmux_subcommand(argv, "capture-pane")),
        "after Enter, inject must poll capture until the pasted-content block disappears; calls={calls:?}"
    );
    assert!(
        send_enter_count(&calls) >= 2,
        "off-by-one guard: if the first Enter leaves the pasted-content block visible, inject must retry Enter; calls={calls:?}"
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::PastedContentPromptAbsentAfterSubmit,
        "successful placeholder submission must be reported as verified"
    );
    assert!(
        report.attempts >= 2,
        "report.attempts must expose Enter retries needed to clear the pasted-content block; report={report:?}"
    );
}

#[test]
fn tmux_text_inject_reports_unverified_when_pasted_content_block_still_present_after_retries() {
    let runner = PastePromptRunner::new([
        "[Pasted Content 4096 chars]",
        "[Pasted Content 4096 chars]",
        "[Pasted Content 4096 chars]",
        "[Pasted Content 4096 chars]",
        "[Pasted Content 4096 chars]",
    ]);
    let backend = TmuxBackend::with_runner(Box::new(runner));

    let report = backend
        .inject(
            &Target::Pane(PaneId::new("%8")),
            &InjectPayload::Text("structured payload line\n".repeat(16)),
            Key::Enter,
            true,
        )
        .expect("inject should return a diagnostic report even when submit verification fails");

    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureContainsNewPastedContentPrompt,
        "fixture sanity: this path must exercise the provider pasted-content block flow; report={report:?}"
    );
    assert!(
        !matches!(
            report.submit_verification,
            SubmitVerification::EnterSentWithoutPlaceholderCheck
                | SubmitVerification::PastedContentPromptAbsentAfterSubmit
                | SubmitVerification::KeySentAfterVisibleToken { .. }
        ),
        "when the pasted-content block remains visible after retries, transport must report a distinct unverified/failure state instead of the generic successful paste+Enter variant; report={report:?}"
    );
}

#[test]
fn delivery_marks_enter_sent_without_placeholder_check_as_delivered_after_successful_token_paste() {
    let ws = tmp_dir("delivery-enter-sent-delivered");
    let store = MessageStore::open(&ws).unwrap();
    let event_log = EventLog::new(&ws);
    let message_id = store
        .create_message(
            Some("task-1"),
            "worker_a",
            "worker_b",
            &"peer message payload with a rendered Team Agent token; no provider placeholder check is required "
                .repeat(3),
            None,
            false,
            Some("team"),
        )
        .unwrap();
    let state = delivery_state();
    let transport = ReportTransport::new(vec![enter_sent_without_placeholder_report()]);

    let delivered = deliver_pending_messages(&ws, &state, &transport, &event_log)
        .expect("delivery should accept successful paste+Enter without requiring a placeholder probe");

    assert_eq!(
        delivered,
        vec![message_id.clone()],
        "MUST-10: EnterSentWithoutPlaceholderCheck means paste+Enter completed and no placeholder probe was needed; it must be delivered, not submitted_unverified"
    );
    let (status, delivered_at) = message_status_and_delivered_at(&store, &message_id);
    assert_eq!(
        status, "delivered",
        "successful paste+Enter without placeholder verification must not be downgraded to submitted_unverified"
    );
    assert!(
        delivered_at.is_some(),
        "delivered messages must have delivered_at set; status={status}"
    );
    let events = read_events(&ws);
    assert!(
        events.contains("\"message.delivered\""),
        "successful paste+Enter must emit message.delivered; events={events}"
    );
    assert!(
        !events.contains("\"send.unverified\""),
        "successful paste+Enter must not emit send.unverified; events={events}"
    );
}

#[test]
fn delivery_verifies_each_peer_message_independently_so_second_paste_cannot_fake_deliver() {
    let ws = tmp_dir("delivery-off-by-one");
    let store = MessageStore::open(&ws).unwrap();
    let event_log = EventLog::new(&ws);
    let first = store
        .create_message(Some("task-1"), "talker", "coder", "first large block", None, false, Some("team"))
        .unwrap();
    let second = store
        .create_message(Some("task-2"), "talker", "coder", "second large block", None, false, Some("team"))
        .unwrap();
    let state = delivery_state();
    let transport = ReportTransport::new(vec![verified_report(2), failed_submit_report()]);

    let delivered = deliver_pending_messages(&ws, &state, &transport, &event_log)
        .expect("delivery should process both queued peer messages");

    assert_eq!(
        delivered,
        vec![first.clone()],
        "off-by-one guard: the second message must not be marked delivered just because Enter submitted a previous paste"
    );
    assert_eq!(message_status(&store, &first), "delivered");
    let second_status = message_status(&store, &second);
    assert!(
        matches!(second_status.as_str(), "submitted_unverified" | "failed"),
        "second message whose pasted-content block remains visible must not be delivered; status={second_status}"
    );
    let events = read_events(&ws);
    let delivered_events = events
        .lines()
        .filter(|line| line.contains("\"event\": \"message.delivered\""))
        .collect::<Vec<_>>();
    assert_eq!(
        delivered_events.len(),
        1,
        "only the independently verified first message may emit message.delivered; events={events}"
    );
    assert!(
        delivered_events[0].contains(&first) && !delivered_events[0].contains(&second),
        "message.delivered must refer to the verified message only; delivered_events={delivered_events:?}"
    );
}

fn verified_report(attempts: u32) -> InjectReport {
    InjectReport {
        stage_reached: InjectStage::Submit,
        inject_verification: InjectVerification::CaptureContainsToken,
        submit_verification: SubmitVerification::PastedContentPromptAbsentAfterSubmit,
        turn_verification: TurnVerification::NotYetObserved,
        attempts,
        submit_diagnostics: None,
    }
}

fn unverified_report() -> InjectReport {
    failed_submit_report()
}

fn enter_sent_without_placeholder_report() -> InjectReport {
    InjectReport {
        stage_reached: InjectStage::Submit,
        inject_verification: InjectVerification::CaptureContainsToken,
        submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
        turn_verification: TurnVerification::NotYetObserved,
        attempts: 1,
        submit_diagnostics: None,
    }
}

fn failed_submit_report() -> InjectReport {
    InjectReport {
        stage_reached: InjectStage::Submit,
        inject_verification: InjectVerification::CaptureContainsToken,
        submit_verification: SubmitVerification::SendKeysFailed,
        turn_verification: TurnVerification::NotYetObserved,
        attempts: 3,
        submit_diagnostics: None,
    }
}

fn delivery_state() -> Value {
    serde_json::json!({
        "active_team_key": "team",
        "session_name": "team-peer",
        "agents": {
            "worker_a": {"status": "running", "window": "worker_a", "provider": "codex"},
            "worker_b": {"status": "running", "window": "worker_b", "provider": "codex"},
            "talker": {"status": "running", "window": "talker", "provider": "codex"},
            "coder": {"status": "running", "window": "coder", "provider": "codex"}
        },
        "teams": {
            "team": {
                "session_name": "team-peer",
                "agents": {
                    "worker_a": {"status": "running", "window": "worker_a", "provider": "codex"},
                    "worker_b": {"status": "running", "window": "worker_b", "provider": "codex"},
                    "talker": {"status": "running", "window": "talker", "provider": "codex"},
                    "coder": {"status": "running", "window": "coder", "provider": "codex"}
                }
            }
        }
    })
}

fn message_status(store: &MessageStore, message_id: &str) -> String {
    message_status_and_delivered_at(store, message_id).0
}

fn message_status_and_delivered_at(store: &MessageStore, message_id: &str) -> (String, Option<String>) {
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status, delivered_at from messages where message_id = ?1",
        params![message_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .unwrap()
}

fn read_events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-provider-submit-verification-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn is_tmux_subcommand(argv: &[String], subcommand: &str) -> bool {
    argv.iter().any(|arg| arg == subcommand)
}

fn first_call_index(calls: &[Vec<String>], subcommand: &str) -> Option<usize> {
    calls.iter().position(|argv| is_tmux_subcommand(argv, subcommand))
}

fn first_send_enter_index(calls: &[Vec<String>]) -> Option<usize> {
    calls.iter().position(|argv| {
        is_tmux_subcommand(argv, "send-keys") && argv.iter().any(|arg| arg == "Enter")
    })
}

fn send_enter_count(calls: &[Vec<String>]) -> usize {
    calls
        .iter()
        .filter(|argv| is_tmux_subcommand(argv, "send-keys") && argv.iter().any(|arg| arg == "Enter"))
        .count()
}

#[derive(Clone)]
struct PastePromptRunner {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    captures: Arc<Mutex<VecDeque<String>>>,
}

impl PastePromptRunner {
    fn new<const N: usize>(captures: [&str; N]) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            captures: Arc::new(Mutex::new(
                captures.into_iter().map(ToString::to_string).collect(),
            )),
        }
    }

    fn calls(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        Arc::clone(&self.calls)
    }
}

impl CommandRunner for PastePromptRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let stdout = if is_tmux_subcommand(argv, "capture-pane") {
            self.captures.lock().unwrap().pop_front().unwrap_or_default()
        } else {
            String::new()
        };
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }

    fn run_with_stdin(&self, argv: &[String], _stdin: &str) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
    }
}

struct ReportTransport {
    reports: Mutex<VecDeque<InjectReport>>,
}

impl ReportTransport {
    fn new(reports: Vec<InjectReport>) -> Self {
        Self {
            reports: Mutex::new(reports.into()),
        }
    }
}

impl Transport for ReportTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("delivery tests do not spawn")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("delivery tests do not spawn")
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(self
            .reports
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(unverified_report))
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(
        &self,
        _target: &Target,
        field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
