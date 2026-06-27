//! 0.4.x leader launcher fixes (CR C-5):
//!
//! 1. `managed_leader_shell_line_includes_claude_unset_block` — unit/golden
//!    that `leader_shell_wrapper_command` for a Claude leader includes the
//!    full Claude session-inheritance unset list (CR C-1 single source).
//! 2. `managed_leader_shell_line_emits_exit_marker_and_exec_shell` — the
//!    wrapper line has all 4 sections (cd, unset, env+cmd no exec, exit
//!    marker + exec shell).
//! 3. `leader_provider_health_detects_provider_exited_when_shell_with_marker` —
//!    when a pane is addressable but current-command is a shell and the
//!    exit marker is in the capture, health reports `ProviderExited`.
//! 4. `leader_env_unset_single_source_grep_guard` — repo grep guard
//!    asserting that the Claude unset literals live in only ONE source
//!    file (CR C-1).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use team_agent::leader::{leader_provider_health, LeaderProviderHealth};
use team_agent::model::enums::PaneLiveness;
use team_agent::tmux_backend::{
    leader_provider_exit_marker, leader_shell_wrapper_command,
    LEADER_PROVIDER_EXIT_MARKER_PREFIX, LEADER_PROVIDER_EXIT_MARKER_SUFFIX,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[allow(unused_imports)]
use team_agent::transport::PaneLiveness as TransportPaneLiveness;

// CR C-1 + C-2 unit test: the leader shell wrapper for a Claude launcher
// must include EVERY entry in profile_launch::provider_env_unsets(Claude, *)
// in the form `unset <KEY> &&` BEFORE the env exports.
#[test]
fn managed_leader_shell_line_includes_claude_unset_block() {
    let claude_unsets = vec![
        "CLAUDE_CODE_SESSION_ID".to_string(),
        "CLAUDE_CODE_CHILD_SESSION".to_string(),
        "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".to_string(),
        "CLAUDE_CODE_ENTRYPOINT".to_string(),
        "CLAUDE_CODE_EXECPATH".to_string(),
        "CLAUDECODE".to_string(),
    ];
    let env: BTreeMap<String, String> = [
        ("TEAM_AGENT_LEADER_PROVIDER".to_string(), "claude".to_string()),
    ]
    .into_iter()
    .collect();
    let line = leader_shell_wrapper_command(
        &["claude".to_string()],
        Path::new("/tmp/workspace"),
        &env,
        &claude_unsets,
        "claude",
    );
    for key in &claude_unsets {
        let expected = format!("unset {key} &&");
        assert!(
            line.contains(&expected),
            "leader shell wrapper must include `{expected}` (CR C-1: single \
             unset source). Line: {line}"
        );
    }
    // Unset must come BEFORE env exports.
    let first_unset_pos = line.find("unset CLAUDE_CODE_SESSION_ID").unwrap();
    let first_env_pos = line.find("TEAM_AGENT_LEADER_PROVIDER=").unwrap();
    assert!(
        first_unset_pos < first_env_pos,
        "unset must precede env exports; got line: {line}"
    );
}

// CR C-2: all 4 envelope sections present.
#[test]
fn managed_leader_shell_line_emits_exit_marker_and_exec_shell() {
    let env: BTreeMap<String, String> = BTreeMap::new();
    let line = leader_shell_wrapper_command(
        &["claude".to_string(), "--help".to_string()],
        Path::new("/var/work"),
        &env,
        &["CLAUDECODE".to_string()],
        "claude",
    );
    assert!(line.starts_with("cd "), "section 1: cd cwd; got {line}");
    assert!(
        line.contains("unset CLAUDECODE &&"),
        "section 2: unset; got {line}"
    );
    assert!(
        line.contains(" claude --help ;") || line.contains(" claude --help;"),
        "section 3: provider invocation (NOT exec); got {line}"
    );
    assert!(
        line.contains("rc=$?;"),
        "section 4: capture exit code; got {line}"
    );
    assert!(
        line.contains("[team-agent] claude exited with"),
        "section 4: exit marker; got {line}"
    );
    assert!(
        line.contains("exec \"${SHELL:-/bin/zsh}\" -l"),
        "section 4: fall back to interactive shell; got {line}"
    );
    // Critical anti-regression: no `exec claude` (the bug we're fixing).
    assert!(
        !line.contains("exec claude"),
        "leader wrapper MUST NOT use `exec claude` — provider must run as \
         child of shell so pane returns to shell on exit. Got: {line}"
    );
}

// CR C-3 P0: leader_provider_health distinguishes ProviderExited from Alive.
#[test]
fn leader_provider_health_detects_provider_exited_when_shell_with_marker() {
    let transport = HealthMockTransport::new(
        PaneLiveness::Live,
        Some("zsh".to_string()),
        Some(
            "user@host project % claude\n\
             [tasks ...]\n\
             \n\
             [team-agent] claude exited with 0\n\
             user@host project %"
                .to_string(),
        ),
    );
    let pane_id = PaneId::new("%42");
    let health = leader_provider_health(&transport, &pane_id, "claude");
    assert_eq!(
        health,
        LeaderProviderHealth::ProviderExited,
        "shell + exit marker must report ProviderExited; got {health:?}"
    );
}

#[test]
fn leader_provider_health_reports_alive_when_pane_current_command_is_provider() {
    let transport = HealthMockTransport::new(
        PaneLiveness::Live,
        Some("claude".to_string()),
        None,
    );
    let pane_id = PaneId::new("%43");
    let health = leader_provider_health(&transport, &pane_id, "claude");
    assert_eq!(
        health,
        LeaderProviderHealth::Alive,
        "pane_current_command == provider must report Alive; got {health:?}"
    );
}

#[test]
fn leader_provider_health_reports_unreachable_when_pane_dead() {
    let transport = HealthMockTransport::new(PaneLiveness::Dead, None, None);
    let pane_id = PaneId::new("%44");
    let health = leader_provider_health(&transport, &pane_id, "claude");
    assert_eq!(
        health,
        LeaderProviderHealth::Unreachable,
        "dead pane must report Unreachable; got {health:?}"
    );
}

// CR R6: single-source marker — wrapper printf text and health check
// substring share the same `leader_provider_exit_marker` helper.
#[test]
fn leader_exit_marker_single_source_wrapper_and_health_agree() {
    let env: BTreeMap<String, String> = BTreeMap::new();
    let line = leader_shell_wrapper_command(
        &["claude".to_string()],
        Path::new("/tmp"),
        &env,
        &[],
        "claude",
    );
    let marker = leader_provider_exit_marker("claude");
    assert!(
        line.contains(&marker),
        "wrapper line must contain the single-source marker `{marker}`; \
         got line: {line}"
    );
    // Round-trip: the captured pane text containing the marker should be
    // detected as ProviderExited by the health helper.
    let transport = HealthMockTransport::new(
        PaneLiveness::Live,
        Some("zsh".to_string()),
        Some(format!("user@host % claude\n{marker} 0\nuser@host %")),
    );
    let pane_id = PaneId::new("%99");
    assert_eq!(
        leader_provider_health(&transport, &pane_id, "claude"),
        LeaderProviderHealth::ProviderExited,
        "round-trip: marker emitted by wrapper must be detected by health"
    );
}

#[test]
fn leader_exit_marker_constants_are_stable() {
    assert_eq!(LEADER_PROVIDER_EXIT_MARKER_PREFIX, "[team-agent]");
    assert_eq!(LEADER_PROVIDER_EXIT_MARKER_SUFFIX, "exited with");
    let marker = leader_provider_exit_marker("foo");
    assert!(
        marker.starts_with(LEADER_PROVIDER_EXIT_MARKER_PREFIX),
        "marker must start with the public prefix constant; got {marker}"
    );
    assert!(
        marker.contains(LEADER_PROVIDER_EXIT_MARKER_SUFFIX),
        "marker must contain the public suffix constant; got {marker}"
    );
}

// CR R6: shell detection robust across POSIX + common interactive shells.
#[test]
fn leader_provider_health_recognizes_diverse_shells_as_fallback() {
    use team_agent::leader::is_interactive_shell_basename;
    let shells = [
        "zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh",
        "ash", "mksh", "yash", "elvish", "nu", "nushell", "xonsh",
    ];
    for sh in shells {
        assert!(
            is_interactive_shell_basename(sh),
            "{sh} must be recognised as an interactive shell (CR R6)"
        );
        // Case insensitivity.
        assert!(
            is_interactive_shell_basename(&sh.to_uppercase()),
            "{sh} (uppercase) must be recognised case-insensitively"
        );
    }
    // Negative — provider binaries must NOT match.
    for provider in ["claude", "codex", "copilot", "gemini"] {
        assert!(
            !is_interactive_shell_basename(provider),
            "{provider} must NOT be classified as an interactive shell"
        );
    }
}

// 0.4.x env-leak regression (scenario 1, in-tmux ExecProvider path):
// leader_env_unset_for_provider(Claude) must contain the full Claude
// CLAUDE_CODE_* child/team env block. The in-tmux ExecProvider path
// (`run_leader_argv`) calls Command::env_remove for each of these so a
// Claude leader launched from a Claude-owned shell does not inherit the
// parent's session-inheritance variables.
#[test]
fn leader_env_unset_for_claude_contains_full_claude_block() {
    use team_agent::leader::leader_env_unset_for_provider;
    use team_agent::provider::Provider;
    let unsets = leader_env_unset_for_provider(Provider::Claude);
    let required = [
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDECODE",
    ];
    for key in required {
        assert!(
            unsets.iter().any(|u| u == key),
            "leader_env_unset_for_provider(Claude) must contain `{key}`; \
             got {unsets:?}"
        );
    }
    // ClaudeCode (Claude Code distinct provider variant) must have the same
    // env-unset list — same Anthropic CLI under the hood.
    let cc_unsets = leader_env_unset_for_provider(Provider::ClaudeCode);
    for key in required {
        assert!(
            cc_unsets.iter().any(|u| u == key),
            "leader_env_unset_for_provider(ClaudeCode) must contain `{key}`; \
             got {cc_unsets:?}"
        );
    }
}

// 0.4.x env-leak regression grep guard: run_leader_argv in
// leader/start.rs MUST call `command.env_remove(key)` for each entry in
// `leader_env_unset_for_provider`. This pins the in-tmux ExecProvider
// path so a future refactor cannot silently re-introduce the env leak.
#[test]
fn in_tmux_execprovider_path_uses_env_remove_grep_guard() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let start_rs = manifest.join("src").join("leader").join("start.rs");
    let contents = std::fs::read_to_string(&start_rs)
        .expect("must read leader/start.rs");
    // The run_leader_argv function must call env_remove inside a loop
    // over leader_env_unset_for_provider.
    let run_leader_argv_pos = contents
        .find("fn run_leader_argv(")
        .expect("run_leader_argv must exist in leader/start.rs");
    let next_fn_pos = contents[run_leader_argv_pos + 1..]
        .find("\nfn ")
        .map(|p| run_leader_argv_pos + 1 + p)
        .unwrap_or(contents.len());
    let body = &contents[run_leader_argv_pos..next_fn_pos];
    assert!(
        body.contains("leader_env_unset_for_provider(plan.provider)"),
        "run_leader_argv must call leader_env_unset_for_provider \
         (CR R6 single source). Body excerpt: {body}"
    );
    assert!(
        body.contains("command.env_remove(key)") || body.contains(".env_remove(key)"),
        "run_leader_argv must call env_remove on each unset key \
         (in-tmux ExecProvider path env-leak regression guard). \
         Body excerpt: {body}"
    );
}

// CR C-1 grep guard.
#[test]
fn leader_env_unset_single_source_grep_guard() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut hits = Vec::new();
    walk_rs_files(&src, &mut |path, contents| {
        if contents.contains("\"CLAUDE_CODE_CHILD_SESSION\"") {
            hits.push(path.strip_prefix(manifest).unwrap_or(path).to_path_buf());
        }
    });
    assert_eq!(
        hits.len(),
        1,
        "CR C-1: CLAUDE_CODE_CHILD_SESSION literal must appear in EXACTLY \
         one source file (single source of truth). Found in: {hits:?}"
    );
    assert!(
        hits[0].ends_with("lifecycle/profile_launch.rs"),
        "the single source must be lifecycle/profile_launch.rs; got {:?}",
        hits[0]
    );
}

fn walk_rs_files(dir: &Path, callback: &mut dyn FnMut(&Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files(&path, callback);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                callback(&path, &contents);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal mock Transport for leader_provider_health tests.
// ---------------------------------------------------------------------------

struct HealthMockTransport {
    liveness: PaneLiveness,
    current_command: Option<String>,
    capture_text: Mutex<Option<String>>,
}

impl HealthMockTransport {
    fn new(
        liveness: PaneLiveness,
        current_command: Option<String>,
        capture_text: Option<String>,
    ) -> Self {
        Self {
            liveness,
            current_command,
            capture_text: Mutex::new(capture_text),
        }
    }
}

impl Transport for HealthMockTransport {
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
        Ok(SpawnResult {
            pane_id: PaneId::new("%0"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }
    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
    }
    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::NoToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotRequired,
            attempts: 0,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let text = self
            .capture_text
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        Ok(CapturedText { text, range })
    }
    fn query(
        &self,
        _target: &Target,
        field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        if matches!(field, PaneField::PaneCurrentCommand) {
            Ok(self.current_command.clone())
        } else {
            Ok(None)
        }
    }
    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(self.liveness)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(
        &self,
        _session: &SessionName,
    ) -> Result<Vec<WindowName>, TransportError> {
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
    fn attach_session(
        &self,
        _session: &SessionName,
    ) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
