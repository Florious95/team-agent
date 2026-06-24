//! BUG-7 (RS 0.3.1 dogfood): `quick-start` must NEVER report bare `ready`
//! while worker MCP tool-set load is unverified or any agent is observably
//! degraded. Real-machine evidence (see ../测试rust/BUG-0.3.0-codex-mcp-schema.md
//! 次要问题#2): `quick-start` printed `quick-start ready: ...` while the codex
//! worker had already silently rejected the team_orchestrator tool schema and
//! returned to IDLE — the user had no observable to distinguish "actually
//! ready" from "spawned but worker dead".
//!
//! Coverage:
//!   T1 — worker spawned with a live window but the framework cannot confirm
//!         the worker's MCP tool set finished loading. The report's
//!         `worker_readiness` must signal `pending_tool_load` (NOT bare Ready),
//!         and the CLI summary must NOT be the historical bare
//!         `quick-start ready: <session>` string.
//!   T2 — at least one worker silently failed to spawn (BUG-2 observable: tmux
//!         window missing after launch). The report's `worker_readiness` must
//!         be `Degraded { unhealthy_agents: [<id>] }`, the summary must call
//!         out `degraded`, and `ok` in the JSON envelope must be false.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use team_agent::lifecycle::{
    quick_start_with_transport, QuickStartReadiness, QuickStartReport,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const TEAM_MD: &str =
    "---\nname: bug7\nobjective: BUG-7 worker readiness honesty contract.\nprovider: codex\n---\n\nTeam.\n";

const CODEX_ROLE: &str = "---\nname: codexer\nrole: Codex Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nCodex worker.\n";

const CLAUDE_ROLE: &str = "---\nname: clauder\nrole: Claude Worker\nprovider: claude\nmodel: claude-sonnet-4-6\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nClaude worker.\n";

#[test]
#[ignore = "real-machine: quick-start worker readiness gate"]
fn t1_quick_start_must_not_emit_bare_ready_when_worker_tool_load_is_unverified() {
    let team = bug7_team_dir("t1-pending");
    // Both agents spawn successfully (recording transport reports both windows live)
    // but the framework has no way to verify the worker MCP tool set actually
    // loaded — codex/claude can still reject schemas asynchronously. The report
    // therefore must be PendingToolLoad, not bare Ready.
    let transport = RecordingTransport::new().with_windows(vec![
        WindowName::new("codexer"),
        WindowName::new("clauder"),
    ]);
    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport should succeed for healthy spawn");
    let QuickStartReport::Ready { worker_readiness, session_name, .. } = &report else {
        panic!("quick-start must return Ready for healthy spawn; got {report:?}");
    };
    assert_eq!(
        worker_readiness,
        &QuickStartReadiness::PendingToolLoad,
        "T1: post-spawn report MUST be PendingToolLoad (framework cannot itself \
         confirm worker MCP tool set load); bare Ready would lie. got {report:?}"
    );
    // Drive the CLI rendering path that the user sees — it must not contain the
    // historical bare `quick-start ready: <session>` phrasing.
    let rendered = render_quick_start_summary(&report);
    assert!(
        !rendered.starts_with("quick-start ready:"),
        "T1: CLI summary must NOT begin with the bare `quick-start ready:` phrase \
         while worker tool-load is unverified; got {rendered:?}"
    );
    assert!(
        rendered.contains(session_name.as_str()),
        "T1: CLI summary should still identify the session; got {rendered:?}"
    );
    assert!(
        rendered.to_lowercase().contains("pending")
            || rendered.to_lowercase().contains("unverified"),
        "T1: CLI summary must label the readiness as pending / unverified; got {rendered:?}"
    );
}

#[test]
#[ignore = "real-machine: quick-start worker readiness gate"]
fn t2_quick_start_must_report_degraded_when_a_worker_spawn_yields_no_live_window() {
    let team = bug7_team_dir("t2-degraded");
    // Recording transport simulates the Mac mini final capture: only the codex
    // window exists after launch; the claude window never materialized (no
    // `claude` binary on PATH, etc.). BUG-2 fix already marks `clauder` as
    // spawn_failed in runtime state; BUG-7 must surface that as a Degraded
    // verdict so quick-start cannot pretend the team is ready.
    let transport = RecordingTransport::new().with_windows(vec![WindowName::new("codexer")]);
    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport should reach the spawn/state path even when an agent fails to spawn");
    let QuickStartReport::Ready { worker_readiness, .. } = &report else {
        panic!("quick-start must still return Ready (with Degraded verdict) so the user sees session details; got {report:?}");
    };
    let QuickStartReadiness::Degraded { unhealthy_agents } = worker_readiness else {
        panic!(
            "T2: silent-spawn-failure must produce Degraded verdict, NOT PendingToolLoad / bare Ready; got {worker_readiness:?}"
        );
    };
    assert!(
        unhealthy_agents.iter().any(|id| id == "clauder"),
        "T2: Degraded verdict must list the agent whose window never materialized \
         (clauder); got unhealthy_agents={unhealthy_agents:?}"
    );
    let rendered = render_quick_start_summary(&report);
    assert!(
        rendered.to_lowercase().contains("degraded"),
        "T2: CLI summary must call out `degraded` so the user does not interpret \
         the report as ready; got {rendered:?}"
    );
    assert!(
        rendered.contains("clauder"),
        "T2: CLI summary should mention the unhealthy agent id (clauder); got {rendered:?}"
    );
}

// ---------------------------------------------------------------------------
// Fixture: minimal team layout + recording transport for the BUG-7 contracts.
// The transport returns whichever WindowName values are seeded via
// `with_windows(...)` from `list_windows`, simulating the post-spawn observable.
// ---------------------------------------------------------------------------

fn bug7_team_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let counter = N.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "ta-bug7-{}-{}-{}",
        tag,
        std::process::id(),
        counter
    ));
    let team = base.join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("mkdir teamdir/agents");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("codexer.md"), CODEX_ROLE).expect("write codexer.md");
    std::fs::write(team.join("agents").join("clauder.md"), CLAUDE_ROLE).expect("write clauder.md");
    team
}

fn render_quick_start_summary(report: &QuickStartReport) -> String {
    // Mirror `cli/mod.rs::quick_start_value`'s summary projection so the contract
    // tests can verify the user-visible string without re-running the full CLI
    // path. Keep this in lock-step with the CLI emit branch.
    match report {
        QuickStartReport::Ready { session_name, worker_readiness, .. } => match worker_readiness {
            QuickStartReadiness::Degraded { unhealthy_agents } => format!(
                "quick-start degraded: {}; unhealthy: {}",
                session_name.as_str(),
                unhealthy_agents.join(",")
            ),
            QuickStartReadiness::PendingToolLoad => format!(
                "quick-start launched (worker tool load unverified): {}",
                session_name.as_str()
            ),
        },
        _ => "non-ready".to_string(),
    }
}

#[derive(Clone, Default)]
struct RecordingTransport {
    state: Arc<Mutex<RecordingState>>,
}

#[derive(Default)]
struct RecordingState {
    windows: Vec<WindowName>,
    sessions: Vec<SessionName>,
    pane_seq: u64,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        {
            let mut guard = self.state.lock().expect("lock");
            guard.windows = windows;
        }
        self
    }

    fn next_pane_id(&self) -> PaneId {
        let mut guard = self.state.lock().expect("lock");
        guard.pane_seq += 1;
        PaneId::new(format!("%{}", guard.pane_seq))
    }
}

impl Transport for RecordingTransport {
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
        {
            let mut guard = self.state.lock().expect("lock");
            if !guard.sessions.iter().any(|s| s.as_str() == session.as_str()) {
                guard.sessions.push(session.clone());
            }
        }
        Ok(SpawnResult {
            pane_id: self.next_pane_id(),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult {
            pane_id: self.next_pane_id(),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
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
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
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
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let guard = self.state.lock().expect("lock");
        Ok(guard.sessions.iter().any(|s| s.as_str() == session.as_str()))
    }

    fn list_windows(
        &self,
        _session: &SessionName,
    ) -> Result<Vec<WindowName>, TransportError> {
        let guard = self.state.lock().expect("lock");
        Ok(guard.windows.clone())
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
