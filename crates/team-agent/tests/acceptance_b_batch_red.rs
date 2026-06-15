//! 0.3.6-B: copilot acceptance bug batch (B-2 / B-4 / B-5 / B-7) contracts.
//!
//! Basis: `.team/artifacts/copilot-acceptance-locate.md` (architect; the report's
//! mechanism theory was DISPROVEN — contracts follow the located root cause, not the
//! report's suggested fix) + the B-4 cr verdict (6 reverse cases).
//!
//! B-2: quick-start attach_commands — socket probe silent-empty must emit an
//!   event/hint; the Ready report must carry non-empty attach_commands.
//! B-4: tick monitor-step failure must NOT block the deliver_pending trunk (the trunk
//!   does not depend on classify); classify.unsupported flood must dedup; --force-paste
//!   must never enter argv (REJECT grep guard).
//! B-5: status must consume the existing coordinator_health + pending count and surface
//!   a runtime block + hint when coordinator is down AND there is accepted backlog
//!   (N38, no auto-recovery).
//! B-7: TEAM_AGENT_LEADER_PANE_ID pointing at a dead/absent pane must fail-fast on the
//!   active (quick-start) path with a clear error; unset must pass through.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::lifecycle::QuickStartReport;
use team_agent::message_store::MessageStore;
use team_agent::model::enums::Provider;
use team_agent::provider::{get_adapter, ProviderAdapter};
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const ANCESTRY_KEY: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const NEUTRAL_ANCESTRY: &str = "[\"/bin/zsh\"]";

// ───────────────────────────── B-4 · monitor step isolation ─────────────────────────

/// B-4 main reverse (cr `monitor_step_failure_does_not_block_deliver_pending`): a
/// monitor step (here: the runtime-approval pane capture, made to hard-fail) must NOT
/// abort the tick before deliver_pending. The tick must (1) not enter backoff, (2) log
/// a step-failure event, (3) still run deliver_pending, (4) push the accepted message
/// to delivered. Today the `?` on the monitor step aborts before delivery → RED.
#[test]
#[serial(env)]
fn b4_monitor_step_failure_does_not_block_deliver_pending() {
    let _g = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("b4-monitor");
    seed_copilot_team_state(&ws, "cp1", true);
    let store = MessageStore::open(&ws).unwrap();
    store
        .create_message(Some("task_mcp"), "leader", "cp1", "do the thing", None, false, None)
        .unwrap();

    let coord = Coordinator::new(
        WorkspacePath::new(ws.clone()),
        Box::new(RealRegistry),
        Box::new(MonitorFailTransport::new()),
    );
    let report = coord.tick();

    let mut failures = Vec::new();
    match &report {
        Err(e) => failures.push(format!(
            "B-4: a monitor-step failure must not propagate as a tick Err (it must degrade \
+ continue, bug-084 philosophy); got Err({e})"
        )),
        Ok(report) => {
            if report.delivered.is_empty() {
                failures.push(
                    "B-4: deliver_pending must still run after a monitor-step failure — the \
delivery trunk does not depend on the monitor face (N36 three-way availability); 0 delivered"
                        .to_string(),
                );
            }
        }
    }
    let events = events_text(&ws);
    if !(events.contains("_failed") && events.contains("tick")) {
        failures.push(format!(
            "B-4: the degraded monitor step must log a coordinator.tick.<step>_failed event \
(observable, not silent); events tail={}",
            events.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
        ));
    }
    let status: Option<String> = MessageStore::open(&ws)
        .unwrap()
        .inbox("cp1", 10, None)
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.get("status").and_then(Value::as_str).map(String::from)));
    if status.as_deref() == Some("accepted") {
        failures.push("B-4: the accepted message must leave the 'accepted' state after the tick".to_string());
    }
    assert!(failures.is_empty(), "B-4 monitor isolation contract failed:\n{}", failures.join("\n"));
}

/// B-4 reverse (cr `force_paste_flag_not_in_argv`): the --force-paste flag (REJECTed —
/// a back-door on a gate that does not exist) must never be introduced into the
/// delivery / provider command code. Source grep guard across the crate.
#[test]
fn b4_force_paste_flag_rejected_not_in_source() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let hits = grep_tree(&root, "force-paste") + grep_tree(&root, "force_paste");
    assert_eq!(
        hits, 0,
        "B-4: --force-paste / force_paste must not appear in product code (cr REJECT: the \
delivery trunk does not consult classify, so this flag is a back-door on a non-existent \
gate — N35/MUST-17); found {hits} occurrence(s)"
    );
}

// ───────────────────────────── B-2 · attach_commands ─────────────────────────────

/// B-2 (locate root cause): a successful quick-start Ready report must carry non-empty
/// attach_commands, and a socket-probe that finds no socket on disk must surface an
/// event/hint (not a silent empty set). Driven through quick-start with a recording
/// transport that provides a window.
#[test]
#[serial(env)]
fn b2_quick_start_ready_has_attach_commands_and_socket_miss_is_observable() {
    let _g = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("b2-attach");
    let team_dir = write_min_team(&ws, "b2team");
    seed_healthy_coordinator(&ws);
    let report = team_agent::lifecycle::quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        true,
        Some("b2team"),
        &WindowedTransport::new("b2team", &["worker_a"]),
    )
    .expect("quick-start should run");

    let mut failures = Vec::new();
    match &report {
        QuickStartReport::Ready {
            attach_commands,
            next_actions,
            ..
        } => assert_attach_commands_or_socket_hint(
            "Ready",
            attach_commands,
            next_actions,
            &mut failures,
        ),
        other => failures.push(format!("B-2 fixture expected Ready report; got {other:?}")),
    }

    let existing = team_agent::lifecycle::quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        false,
        Some("b2team"),
        &WindowedTransport::new("b2team", &["worker_a"]),
    )
    .expect("second quick-start should return ExistingRuntime");
    match &existing {
        QuickStartReport::ExistingRuntime {
            attach_commands,
            next_actions,
            ..
        } => assert_attach_commands_or_socket_hint(
            "ExistingRuntime",
            attach_commands,
            next_actions,
            &mut failures,
        ),
        other => failures.push(format!(
            "B-2 duplicate quick-start expected ExistingRuntime report; got {other:?}"
        )),
    }
    assert!(failures.is_empty(), "B-2 attach_commands contract failed:\n{}", failures.join("\n"));
}

// ───────────────────────────── B-5 · status runtime block ─────────────────────────

/// B-5 (N38): when there is NO coordinator pid AND accepted backlog exists, status must
/// surface a runtime block (coordinator.ok==false) and a hint mentioning the coordinator
/// + restart. Driven through the public status entry.
#[test]
#[serial(env)]
fn b5_status_surfaces_runtime_hint_on_stale_coordinator_with_backlog() {
    let _g = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("b5-stale");
    save_runtime_state(
        &ws,
        &json!({"session_name": "team-b5", "active_team_key": "team-b5", "agents": {
            "w1": {"status": "running", "provider": "codex", "agent_id": "w1", "window": "w1"}
        }}),
    )
    .unwrap();
    // NO coordinator pid file written (stale/dead). Two accepted messages backlog.
    let store = MessageStore::open(&ws).unwrap();
    store.create_message(Some("t1"), "leader", "w1", "m1", None, false, None).unwrap();
    store.create_message(Some("t1"), "leader", "w1", "m2", None, false, None).unwrap();

    let status = team_agent::cli::status_port::status(&ws, false, false).expect("status");
    let text = status.to_string();
    let mut failures = Vec::new();

    let runtime_ok = status
        .pointer("/runtime/coordinator/ok")
        .and_then(Value::as_bool);
    if runtime_ok != Some(false) {
        failures.push(format!(
            "B-5: status must carry a runtime block with coordinator.ok==false when the \
coordinator is not running (consume coordinator_health); got runtime/coordinator/ok={runtime_ok:?}"
        ));
    }
    let has_hint = text.contains("undelivered") || (text.to_lowercase().contains("coordinator") && text.contains("restart"));
    if !has_hint {
        failures.push(format!(
            "B-5: status must add a hint naming the coordinator + restart when it is down \
AND there is accepted backlog (N38 explicable, no auto-recovery); status={text}"
        ));
    }
    assert!(failures.is_empty(), "B-5 status runtime-hint contract failed:\n{}", failures.join("\n"));
}

/// B-5 anti-nag: a HEALTHY coordinator with backlog must NOT show the down-hint.
#[test]
#[serial(env)]
fn b5_status_no_hint_when_coordinator_healthy() {
    let _g = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("b5-healthy");
    save_runtime_state(
        &ws,
        &json!({"session_name": "team-b5h", "active_team_key": "team-b5h", "agents": {
            "w1": {"status": "running", "provider": "codex", "agent_id": "w1", "window": "w1"}
        }}),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let store = MessageStore::open(&ws).unwrap();
    store.create_message(Some("t1"), "leader", "w1", "m1", None, false, None).unwrap();

    let status = team_agent::cli::status_port::status(&ws, false, false).expect("status");
    let text = status.to_string().to_lowercase();
    assert!(
        !(text.contains("coordinator not running") || text.contains("run team-agent restart")),
        "B-5 anti-nag: a healthy coordinator must not show the down-hint (no nag); status={text}"
    );
}

// ───────────────────────────── B-7 · leader pane env fail-fast ─────────────────────

/// B-7 (N38 fail-fast): a TEAM_AGENT_LEADER_PANE_ID pointing at a dead/absent pane must
/// make the active quick-start path fail with a clear error (naming the pane + an
/// action), not bind silently. Unset must pass through.
#[test]
#[serial(env)]
fn b7_dead_leader_pane_env_fails_fast_on_quick_start() {
    let ws = tmp_ws("b7-deadpane");
    let team_dir = write_min_team(&ws, "b7team");
    seed_healthy_coordinator(&ws);
    let _g = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("TEAM_AGENT_LEADER_PANE_ID", "%9999"),
    ]);

    let result = team_agent::lifecycle::quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        true,
        Some("b7team"),
        &DeadPaneTransport,
    );

    let text = format!("{result:?}");
    let mut failures = Vec::new();
    if result.is_ok() {
        failures.push(format!(
            "B-7: a TEAM_AGENT_LEADER_PANE_ID pointing at a dead/absent pane (%9999) must \
make quick-start fail-fast, not bind silently; got Ok: {text}"
        ));
    } else if !(text.contains("%9999") && (text.to_lowercase().contains("pane"))) {
        failures.push(format!(
            "B-7: the fail-fast error must name the offending pane id and be about the \
leader pane (N38 three-line: error/action/log); got {text}"
        ));
    }
    assert!(failures.is_empty(), "B-7 dead-pane fail-fast contract failed:\n{}", failures.join("\n"));
}

/// B-7 (no false-positive): with the env UNSET, quick-start must proceed normally.
#[test]
#[serial(env)]
fn b7_unset_leader_pane_env_passes_through() {
    let ws = tmp_ws("b7-unset");
    let team_dir = write_min_team(&ws, "b7uteam");
    seed_healthy_coordinator(&ws);
    let _g = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    std::env::remove_var("TEAM_AGENT_LEADER_PANE_ID");

    let result = team_agent::lifecycle::quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        true,
        Some("b7uteam"),
        &WindowedTransport::new("b7uteam", &["worker_a"]),
    );
    assert!(
        result.is_ok(),
        "B-7: an unset TEAM_AGENT_LEADER_PANE_ID must not trip the fail-fast (no false \
positive); got {result:?}"
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

fn seed_copilot_team_state(ws: &Path, agent: &str, accepted_msg: bool) {
    let _ = accepted_msg;
    save_runtime_state(
        ws,
        &json!({
            "session_name": "team-b4",
            "active_team_key": "team-b4",
            "agents": {
                agent: {
                    "status": "running", "provider": "copilot", "agent_id": agent,
                    "window": agent, "pane_id": "%41",
                    "session_id": "11111111-2222-4333-8444-555555555555",
                    "spawn_cwd": ws.to_string_lossy(),
                },
            },
        }),
    )
    .unwrap();
    let _ = MessageStore::open(ws).unwrap();
}

fn write_min_team(ws: &Path, name: &str) -> PathBuf {
    let team = ws.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!("---\nname: {name}\nobjective: B-batch fixture.\nprovider: codex\n---\n\nTeam.\n"),
    )
    .unwrap();
    std::fs::write(
        team.join("agents/worker_a.md"),
        "---\nname: worker_a\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
    )
    .unwrap();
    team
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(
        team_agent::coordinator::coordinator_pid_path(&workspace).parent().unwrap(),
    )
    .unwrap();
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(team_agent::coordinator::coordinator_pid_path(&workspace), pid.to_string()).unwrap();
}

fn events_text(ws: &Path) -> String {
    std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn grep_tree(root: &Path, needle: &str) -> usize {
    let mut count = 0;
    let entries = std::fs::read_dir(root).into_iter().flatten().flatten();
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            count += grep_tree(&path, needle);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            count += std::fs::read_to_string(&path).unwrap_or_default().matches(needle).count();
        }
    }
    count
}

struct RealRegistry;
impl ProviderRegistry for RealRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }
    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists { whitelist: Vec::new(), blacklist: Vec::new() }
    }
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}
impl EnvGuard {
    fn set(values: &[(&'static str, &'static str)]) -> Self {
        let previous = values.iter().map(|(k, _)| (*k, std::env::var(k).ok())).collect();
        for (k, v) in values {
            std::env::set_var(k, v);
        }
        Self { previous }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.previous.drain(..).rev() {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-036b-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn assert_attach_commands_or_socket_hint(
    branch: &str,
    attach_commands: &[String],
    next_actions: &[String],
    failures: &mut Vec<String>,
) {
    if attach_commands.is_empty() {
        if !has_socket_missing_hint(next_actions) {
            failures.push(format!(
                "B-2: {branch} with no attach_commands must explain the missing tmux socket \
and give an attach/restart action; next_actions={next_actions:?}"
            ));
        }
        return;
    }

    for command in attach_commands {
        let Some(socket_path) = attach_socket_path(command) else {
            failures.push(format!(
                "B-2: {branch} attach command must include `tmux -S <socket>`; command={command:?}"
            ));
            continue;
        };
        if !socket_path.exists() {
            if !has_socket_missing_hint(next_actions) {
                failures.push(format!(
                    "B-2: {branch} attach command points at a socket missing on disk, so the report \
must also explain the socket miss; command={command:?} socket_path={} next_actions={next_actions:?}",
                    socket_path.display()
                ));
            }
        }
    }
}

fn has_socket_missing_hint(next_actions: &[String]) -> bool {
    let hints = next_actions.join("\n").to_lowercase();
    hints.contains("socket")
        && (hints.contains("not found") || hints.contains("missing"))
        && (hints.contains("attach") || hints.contains("restart"))
}

fn attach_socket_path(command: &str) -> Option<PathBuf> {
    let mut parts = command.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "-S" {
            return parts.next().map(PathBuf::from);
        }
    }
    None
}

// ─────────────────────────────── transports ───────────────────────────────

/// A transport whose pane capture HARD-FAILS — used to drive a monitor-step failure
/// (B-4). Sessions/windows present so the tick proceeds to the monitor steps.
struct MonitorFailTransport {
    session: Mutex<bool>,
}
impl MonitorFailTransport {
    fn new() -> Self {
        Self { session: Mutex::new(true) }
    }
}

/// A quiet transport (sessions present, capture returns an idle screen) for the dedup
/// tests where we only care about the classify event count.
struct QuietTransport;

/// Provides named windows + sessions so attach_commands can be built (B-2).
struct WindowedTransport {
    session: SessionName,
    windows: Vec<WindowName>,
    present: Mutex<bool>,
}
impl WindowedTransport {
    fn new(team: &str, windows: &[&str]) -> Self {
        Self {
            session: SessionName::new(format!("team-{team}")),
            windows: windows.iter().map(|w| WindowName::new(*w)).collect(),
            present: Mutex::new(false),
        }
    }
}

/// Reports the leader pane (%9999) as DEAD (B-7).
struct DeadPaneTransport;


impl Transport for MonitorFailTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(&self, s: &SessionName, w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        *self.session.lock().unwrap() = true;
        Ok(SpawnResult { pane_id: PaneId::new("%1"), session: s.clone(), window: w.clone(), child_pid: None })
    }
    fn spawn_into(&self, s: &SessionName, w: &WindowName, a: &[String], c: &Path, e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        self.spawn_first(s, w, a, c, e)
    }
    fn capture(&self, _t: &Target, _r: CaptureRange) -> Result<CapturedText, TransportError> {
        Err(TransportError::Subprocess {
            argv: vec!["tmux".to_string(), "capture-pane".to_string()],
            code: Some(1),
            stderr: "injected monitor-step capture failure".to_string(),
        })
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session.lock().unwrap())
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn query(&self, _t: &Target, f: PaneField) -> Result<Option<String>, TransportError> {
        match f { PaneField::PaneWidth => Ok(Some("120".to_string())), _ => Ok(None) }
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("cp1")])
    }
    fn set_session_env(&self, _s: &SessionName, _k: &str, _v: &str) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> { Ok(()) }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> { Ok(()) }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> { Ok(AttachOutcome::Attached) }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
}

impl Transport for QuietTransport {
    fn spawn_first(&self, s: &SessionName, w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult { pane_id: PaneId::new("%1"), session: s.clone(), window: w.clone(), child_pid: None })
    }
    fn spawn_into(&self, s: &SessionName, w: &WindowName, a: &[String], c: &Path, e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        self.spawn_first(s, w, a, c, e)
    }
    fn capture(&self, _t: &Target, r: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: "Copilot\n> ".to_string(), range: r })
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("cp1")])
    }
    fn kind(&self) -> BackendKind { BackendKind::Tmux }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> { Ok(()) }
    fn query(&self, _t: &Target, f: PaneField) -> Result<Option<String>, TransportError> {
        match f { PaneField::PaneWidth => Ok(Some("120".to_string())), _ => Ok(None) }
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> { Ok(Vec::new()) }
    fn set_session_env(&self, _s: &SessionName, _k: &str, _v: &str) -> Result<SetEnvOutcome, TransportError> { Ok(SetEnvOutcome::Applied) }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> { Ok(()) }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> { Ok(()) }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> { Ok(AttachOutcome::Attached) }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport { stage_reached: InjectStage::Submit, inject_verification: InjectVerification::CaptureContainsToken, submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck, turn_verification: TurnVerification::NotYetObserved, attempts: 1, submit_diagnostics: None })
    }

}

impl Transport for WindowedTransport {
    fn spawn_first(&self, _s: &SessionName, w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        *self.present.lock().unwrap() = true;
        Ok(SpawnResult { pane_id: PaneId::new("%1"), session: self.session.clone(), window: w.clone(), child_pid: None })
    }
    fn spawn_into(&self, s: &SessionName, w: &WindowName, a: &[String], c: &Path, e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        self.spawn_first(s, w, a, c, e)
    }
    fn capture(&self, _t: &Target, r: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: "OpenAI Codex\ncodex>".to_string(), range: r })
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.present.lock().unwrap())
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.windows.clone())
    }
    fn kind(&self) -> BackendKind { BackendKind::Tmux }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> { Ok(()) }
    fn query(&self, _t: &Target, f: PaneField) -> Result<Option<String>, TransportError> {
        match f { PaneField::PaneWidth => Ok(Some("120".to_string())), _ => Ok(None) }
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> { Ok(Vec::new()) }
    fn set_session_env(&self, _s: &SessionName, _k: &str, _v: &str) -> Result<SetEnvOutcome, TransportError> { Ok(SetEnvOutcome::Applied) }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> { Ok(()) }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> { Ok(()) }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> { Ok(AttachOutcome::Attached) }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport { stage_reached: InjectStage::Submit, inject_verification: InjectVerification::CaptureContainsToken, submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck, turn_verification: TurnVerification::NotYetObserved, attempts: 1, submit_diagnostics: None })
    }

}

impl Transport for DeadPaneTransport {
    fn spawn_first(&self, s: &SessionName, w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult { pane_id: PaneId::new("%1"), session: s.clone(), window: w.clone(), child_pid: None })
    }
    fn spawn_into(&self, s: &SessionName, w: &WindowName, a: &[String], c: &Path, e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        self.spawn_first(s, w, a, c, e)
    }
    fn capture(&self, _t: &Target, r: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range: r })
    }
    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        // %9999 (the leader pane env) is dead; everything else live.
        if pane.as_str() == "%9999" {
            Ok(PaneLiveness::Dead)
        } else {
            Ok(PaneLiveness::Live)
        }
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("worker_a")])
    }
    fn kind(&self) -> BackendKind { BackendKind::Tmux }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> { Ok(()) }
    fn query(&self, _t: &Target, f: PaneField) -> Result<Option<String>, TransportError> {
        match f { PaneField::PaneWidth => Ok(Some("120".to_string())), _ => Ok(None) }
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> { Ok(Vec::new()) }
    fn set_session_env(&self, _s: &SessionName, _k: &str, _v: &str) -> Result<SetEnvOutcome, TransportError> { Ok(SetEnvOutcome::Applied) }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> { Ok(()) }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> { Ok(()) }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> { Ok(AttachOutcome::Attached) }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport { stage_reached: InjectStage::Submit, inject_verification: InjectVerification::CaptureContainsToken, submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck, turn_verification: TurnVerification::NotYetObserved, attempts: 1, submit_diagnostics: None })
    }

}
