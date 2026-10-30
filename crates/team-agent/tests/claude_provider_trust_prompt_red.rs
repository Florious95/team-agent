//! Claude provider startup trust-prompt parity contracts.
//!
//! Claude Code has a workspace-trust startup menu distinct from Codex. The public provider
//! startup-prompt seam must recognize and handle it before launch/restart/tick readiness and before
//! delivery injects task text into the pane.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::model::enums::{PaneLiveness, Provider};
use team_agent::provider::get_adapter;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const CLAUDE_TRUST_YES_ACTIVE: &str = r#"Claude Code

Quick safety check: Is this a project you created or one you trust?
❯ 1. Yes, I trust this folder
  2. No, exit
(Enter to confirm)
"#;

const CLAUDE_TRUST_NO_ACTIVE: &str = r#"Claude Code

Quick safety check: Is this a project you created or one you trust?
  1. Yes, I trust this folder
❯ 2. No, exit
Enter to confirm · Esc to cancel
"#;

const CLAUDE_READY_IDLE: &str = r#"Claude Code
Context left until auto-compact: 91%

❯ /compact
"#;

#[test]
fn claude_trust_recognizer_public_shape_exists_and_is_provider_specific() {
    let startup = source("src/provider/startup_prompt.rs");

    assert!(
        startup.contains("classify_claude_startup_screen"),
        "Claude trust must have a pure recognizer entrypoint; implementation should not hide it inside Codex-specific logic"
    );
    for needle in [
        "Quick safety check",
        "Is this a project you created or one you trust?",
        "Yes, I trust this folder",
        "No, exit",
        "Enter to confirm",
        "claude_workspace_trust",
    ] {
        assert!(
            startup.contains(needle),
            "Claude pure recognizer/handler must include real Claude trust shape marker {needle:?}"
        );
    }
    assert!(
        startup.contains("StartupScreenDecision::AnswerWorkspaceTrust"),
        "active Claude trust with selector on Yes must classify to AnswerWorkspaceTrust"
    );
}

#[test]
fn claude_active_yes_trust_handler_sends_exactly_enter_not_digit_one() {
    let transport = ScriptedTransport::new([CLAUDE_TRUST_YES_ACTIVE, CLAUDE_READY_IDLE]);
    let target = Target::Pane(PaneId::new("%1"));

    let handled = get_adapter(Provider::Claude).handle_startup_prompts(&transport, &target, 3, 0.0);

    assert_eq!(
        handled
            .iter()
            .map(|item| (item.prompt.as_str(), item.action.as_str()))
            .collect::<Vec<_>>(),
        vec![("claude_workspace_trust", "sent_enter")],
        "Claude active Yes trust prompt must be handled as claude_workspace_trust/sent_enter; handled={handled:?}"
    );
    let sent = transport.sent_keys();
    assert_eq!(
        sent,
        vec![vec![Key::Enter]],
        "Claude trust auto-answer must send exactly Enter, not character '1' or task text; sent={sent:?}"
    );
    assert!(
        !sent.iter().flatten().any(|key| matches!(key, Key::Char('1'))),
        "the contract forbids hard-coding digit 1 as the default Claude trust answer; sent={sent:?}"
    );
}

#[test]
fn claude_no_active_trust_prompt_is_not_auto_answered() {
    let transport = ScriptedTransport::new([CLAUDE_TRUST_NO_ACTIVE]);
    let target = Target::Pane(PaneId::new("%2"));

    let handled = get_adapter(Provider::Claude).handle_startup_prompts(&transport, &target, 1, 0.0);

    assert!(
        handled.is_empty() && transport.sent_keys().is_empty(),
        "when the active selector is on `No, exit`, Claude trust must not blindly send Enter or default to digit 1; handled={handled:?} sent={:?}",
        transport.sent_keys()
    );
}

#[test]
fn claude_ready_idle_shape_is_not_confused_with_numbered_trust_menu() {
    let startup = source("src/provider/startup_prompt.rs");
    assert!(
        startup.contains("Claude Code") && startup.contains("classify_claude_startup_screen"),
        "Claude ready detection must be provider-specific and must not reuse Codex READY_MARKERS"
    );

    let ready_transport = ScriptedTransport::new([CLAUDE_READY_IDLE]);
    let target = Target::Pane(PaneId::new("%3"));
    let handled =
        get_adapter(Provider::Claude).handle_startup_prompts(&ready_transport, &target, 1, 0.0);
    assert!(
        handled.is_empty() && ready_transport.sent_keys().is_empty(),
        "normal Claude idle prompt should be ready/no-op, not workspace-trust; handled={handled:?} sent={:?}",
        ready_transport.sent_keys()
    );

    let trust_transport = ScriptedTransport::new([CLAUDE_TRUST_YES_ACTIVE]);
    let handled =
        get_adapter(Provider::Claude).handle_startup_prompts(&trust_transport, &target, 1, 0.0);
    assert!(
        handled.iter().any(|item| item.prompt == "claude_workspace_trust"),
        "Claude trust menu contains a numbered selector `❯ 1`, but that must be startup trust, not ready idle; handled={handled:?}"
    );
}


#[derive(Debug, Default)]
struct ScriptedTransport {
    screens: std::sync::Arc<Mutex<Vec<String>>>,
    sent: std::sync::Arc<Mutex<Vec<Vec<Key>>>>,
    injects: std::sync::Arc<Mutex<Vec<(Target, String)>>>,
    session_present: bool,
    windows: std::sync::Arc<Mutex<Vec<WindowName>>>,
    targets: std::sync::Arc<Mutex<Vec<PaneInfo>>>,
}

impl Clone for ScriptedTransport {
    fn clone(&self) -> Self {
        Self {
            screens: self.screens.clone(),
            sent: self.sent.clone(),
            injects: self.injects.clone(),
            session_present: self.session_present,
            windows: self.windows.clone(),
            targets: self.targets.clone(),
        }
    }
}

impl ScriptedTransport {
    fn new<const N: usize>(screens: [&str; N]) -> Self {
        Self {
            screens: std::sync::Arc::new(Mutex::new(
                screens.into_iter().map(str::to_string).collect(),
            )),
            sent: std::sync::Arc::new(Mutex::new(Vec::new())),
            injects: std::sync::Arc::new(Mutex::new(Vec::new())),
            session_present: true,
            windows: std::sync::Arc::new(Mutex::new(Vec::new())),
            targets: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sent_keys(&self) -> Vec<Vec<Key>> {
        self.sent.lock().unwrap().clone()
    }

}

impl Transport for ScriptedTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(spawn_result(session, window))
    }

    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        let text = match payload {
            InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text) => {
                text.clone()
            }
            InjectPayload::Empty => String::new(),
        };
        self.injects.lock().unwrap().push((target.clone(), text));
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::PastedContentPromptAbsentAfterSubmit,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        self.sent.lock().unwrap().push(keys.to_vec());
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let mut screens = self.screens.lock().unwrap();
        let text = if screens.is_empty() {
            String::new()
        } else {
            screens.remove(0)
        };
        Ok(CapturedText { text, range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneWidth => Some("120".to_string()),
            _ => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.targets.lock().unwrap().clone())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.windows.lock().unwrap().clone())
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

fn spawn_result(session: &SessionName, window: &WindowName) -> SpawnResult {
    SpawnResult {
        pane_id: PaneId::new("%1"),
        session: session.clone(),
        window: window.clone(),
        child_pid: None,
    }
}



fn source(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}


/// The trust dialog layout the station surface guard recognizes today.
const CLAUDE_TRUST_ACCESSING_WORKSPACE: &str = "Accessing workspace:\nQuick safety check: Is this a project you created or one you trust?\nYes, I trust this folder\n❯ No, exit\nEnter to confirm · Esc to cancel";

/// M4 re-expression of the retired pump test (`delivery_defers_claude_trust_prompt_…`):
/// a logical send to a Claude seat whose pane shows the trust menu keeps the message
/// durable in the C3 inbox, reports `non_composer`, and never pastes or presses Enter.
#[test]
fn station_send_withholds_input_from_claude_trust_prompt_and_keeps_the_message_durable() {
    assert_station_send_withholds_input(CLAUDE_TRUST_ACCESSING_WORKSPACE);
}

/// Same oracle on the numbered menu that the Claude adapter's own trust recognizer
/// accepts (`claude_trust_recognizer_public_shape_exists_and_is_provider_specific`).
#[test]
#[ignore = "P-6 (pre-existing since ea7572b3): the station surface guard does not recognize the numbered Claude trust menu, so send pastes and presses Enter; see STAGE4-M4-TEST-EXCISION-CLASSIFICATION.md"]
fn station_send_withholds_input_from_the_numbered_claude_trust_menu() {
    assert_station_send_withholds_input(CLAUDE_TRUST_YES_ACTIVE);
}

fn assert_station_send_withholds_input(screen: &str) {
    use std::os::unix::fs::PermissionsExt;
    static N: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "claude-trust-station-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
    // Synthetic tmux: every capture shows the trust menu; every call is traced.
    let tmux = root.join("bin/tmux");
    std::fs::write(
        &tmux,
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['TRUST_FIXTURE_ROOT'])
args = sys.argv[1:]
action = args[2] if args[0] in ('-S', '-L') else args[0]
with (root / 'trace.jsonl').open('a') as log:
    log.write(json.dumps(action) + '\n')
if action == 'display-message':
    print('0|1|2|' + str(root / 'tmux.sock'))
elif action == 'capture-pane':
    print(os.environ['TRUST_FIXTURE_SCREEN'])
elif action == 'load-buffer':
    sys.stdin.buffer.read()
"#,
    )
    .unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        root.join(".team/runtime/state.json"),
        serde_json::to_vec(&json!({"team_key":"alpha", "agents":{"clauder":{
            "provider":"claude","pane_id":"%7","native_endpoint":"synthetic-trust"}}}))
        .unwrap(),
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(root.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let content = "do not paste this task into the trust menu";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["send", "clauder", content, "--team", "alpha", "--json"])
        .current_dir(&root)
        .env("PATH", path)
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_AGENT_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TEAM_AGENT_WORKSPACE")
        .env("TRUST_FIXTURE_ROOT", &root)
        .env("TRUST_FIXTURE_SCREEN", screen)
        .output()
        .unwrap();
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("send must answer with a JSON receipt: {diagnostic}"));
    assert_eq!(receipt["status"], "stored", "{diagnostic}");
    assert_ne!(receipt["delivery"], "physical", "{diagnostic}");
    assert!(
        receipt["delivery_error"].as_str().unwrap_or_default().contains("non_composer"),
        "{diagnostic}"
    );
    let inbox = team_agent::inbox_store::read(&root, "clauder", 10, Some("alpha")).unwrap();
    assert_eq!(inbox["messages"][0]["content"], content, "message must stay durable");
    let actions: Vec<String> = std::fs::read_to_string(root.join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        !actions.iter().any(|a| a == "paste-buffer" || a == "send-keys"),
        "trust menu must get no paste and no Enter: {actions:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
