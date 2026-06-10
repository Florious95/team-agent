//! Swallow-family batch 2: tmux/transport query layer observability.
//!
//! Basis: `.team/artifacts/swallow-family-slices.md` batch 2 (7 items; family-level
//! contracts per the batch-1 precedent: inject a failure -> a failure event with a
//! non-null error must land AND the return value must not fake-green).
//! Rule source: user CLAUDE.md §5 — every "looks executed but had no effect" needs a
//! log line that explains why.
//!
//! Contract ①: tmux_backend.rs:816-818 — one pane's pane_pid query erroring at the
//!   runner level (`?`) currently fails the WHOLE list_targets call (single-point
//!   failure amplification); it must instead record `tmux.pane_pid_query_failed` and
//!   keep collecting the remaining panes (pane_pid=None for the failed one).
//! Contract ②: A1 proper — startup prompt handling that cannot even capture the pane
//!   currently swallows the failure (`let _ =` family); the tick must surface a
//!   startup-prompt failure event with a non-null error and must NOT mark the agent's
//!   startup prompts as handled/complete.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::model::enums::Provider;
use team_agent::provider::{get_adapter, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

/// Batch 2 ①: one pane's display-message pane_pid probe erroring must not fail the
/// whole list_targets — the other panes must survive with their pids, the failed pane
/// degrades to pane_pid=None, and a `pane_pid_query_failed` event with a non-null
/// error lands in the workspace event log.
#[test]
fn b2_list_targets_survives_single_pane_pid_query_failure() {
    let ws = tmp_ws("b2-panes");
    let backend = TmuxBackend::with_runner_for_workspace(Box::new(PanePidFailRunner), &ws);

    let result = backend.list_targets();

    let mut failures = Vec::new();
    match &result {
        Err(error) => failures.push(format!(
            "①: a single pane's probe failure must not fail the whole list_targets \
(tmux_backend.rs:816-818 `?` amplification); got Err({error})"
        )),
        Ok(panes) => {
            if panes.len() != 2 {
                failures.push(format!(
                    "①: both panes must survive the single-pane probe failure; got {panes:?}"
                ));
            }
            if !panes
                .iter()
                .any(|pane| pane.pane_id.as_str() == "%2" && pane.pane_pid == Some(4242))
            {
                failures.push(format!(
                    "①: the healthy pane %2 must keep its probed pid; got {panes:?}"
                ));
            }
        }
    }
    let events = events_text(&ws);
    let probe_line = events.lines().find(|line| line.contains("pane_pid_query_failed"));
    match probe_line {
        None => failures.push(
            "①: the failed pane probe must write a `tmux.pane_pid_query_failed` event \
(CLAUDE.md §5: a query that silently yields nothing must explain why)"
                .to_string(),
        ),
        Some(line) => {
            if line.contains("\"error\":null") || line.contains("\"error\": null") {
                failures.push(format!("①: the failure event must carry a non-null error; line={line}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "batch2 ① list_targets resilience/observability contract failed:\n{}",
        failures.join("\n")
    );
}

/// Batch 2 ② (A1 proper): when the startup-prompt pass cannot even capture the pane,
/// the tick must surface a startup-prompt failure event (non-null error) and must not
/// fake-green the agent's startup prompt state.
#[test]
fn b2_startup_prompt_capture_failure_is_observable_not_silent() {
    let ws = tmp_ws("b2-startup");
    save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "ctxteam",
            "session_name": "team-ctxteam",
            "team_dir": ws.to_string_lossy(),
            "agents": {
                "clauder": {
                    "provider": "claude",
                    "status": "running",
                    "startup_prompts": "pending",
                    "startup_prompt_status": "pending",
                    "window": "clauder",
                    "pane_id": "%1",
                    "mcp_ready": true,
                    "owner_team_id": "ctxteam",
                    "session_id": "55555555-6666-4777-8888-999999999999",
                    "rollout_path": seed_rollout(&ws),
                }
            }
        }),
    )
    .unwrap();
    let coord = Coordinator::new(
        WorkspacePath::new(ws.clone()),
        Box::new(RealAdapterRegistry),
        Box::new(CaptureFailsTransport),
    );

    let _ = coord.tick();

    let events = events_text(&ws);
    let state = load_runtime_state(&ws).unwrap();
    let startup = state
        .pointer("/agents/clauder/startup_prompts")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut failures = Vec::new();
    let failure_line = events
        .lines()
        .find(|line| line.contains("startup_prompt") && line.contains("fail"));
    match failure_line {
        None => failures.push(
            "②: a capture failure during startup-prompt handling must write a \
startup-prompt failure event (A1 structured outcome: handled/failed/skipped + target); \
today the failure is swallowed (`let _ =` family)"
                .to_string(),
        ),
        Some(line) => {
            if line.contains("\"error\":null") || line.contains("\"error\": null") {
                failures.push(format!("②: the failure event must carry a non-null error; line={line}"));
            }
        }
    }
    if startup == "handled" || startup == "complete" {
        failures.push(format!(
            "②: an unobservable pane must not fake-green startup_prompts={startup:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "batch2 ② startup-prompt observability contract failed:\n{}\nevents={events}",
        failures.join("\n")
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

/// list-panes yields two panes (format string carries no pane_pid, forcing the
/// display-message fallback); the %1 probe errors at the runner level, %2 succeeds.
struct PanePidFailRunner;

impl CommandRunner for PanePidFailRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|arg| arg == "list-panes") {
            return Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: concat!(
                    "%1\tteam-x\t0\tw1\t0\t/dev/ttys001\tcodex\t1\t/tmp\t1\t0\n",
                    "%2\tteam-x\t1\tw2\t0\t/dev/ttys002\tcodex\t0\t/tmp\t1\t0\n",
                )
                .to_string(),
                stderr: String::new(),
            });
        }
        if argv.iter().any(|arg| arg == "display-message") {
            if argv.iter().any(|arg| arg == "%1") {
                return Err(std::io::Error::other("injected pane probe failure"));
            }
            return Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: "4242\n".to_string(),
                stderr: String::new(),
            });
        }
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Transport whose pane captures always fail (dead/unreadable pane).
struct CaptureFailsTransport;

impl Transport for CaptureFailsTransport {
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
            pane_id: PaneId::new("%1"),
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
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(&self, _target: &Target, _range: CaptureRange) -> Result<CapturedText, TransportError> {
        Err(TransportError::Subprocess {
            argv: vec!["tmux".to_string(), "capture-pane".to_string()],
            code: Some(1),
            stderr: "injected capture failure (dead pane)".to_string(),
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("clauder")])
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

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }
    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists { whitelist: Vec::new(), blacklist: Vec::new() }
    }
}

fn seed_rollout(ws: &Path) -> String {
    let rollout = ws.join("rollout-clauder.jsonl");
    std::fs::write(&rollout, "{\"type\":\"system\",\"subtype\":\"boot\"}\n").unwrap();
    rollout.to_string_lossy().to_string()
}

fn events_text(ws: &Path) -> String {
    std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-swallow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
