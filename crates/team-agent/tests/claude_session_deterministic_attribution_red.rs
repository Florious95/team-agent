//! RED contract for deterministic Claude session attribution.
//!
//! Requirement anchors:
//! - `.team/artifacts/PERF-DESIGN-FINAL-20260730.md` P1-P9.
//! - `ARB-20260730-12` replaces the old P7 compatible-api exclusion with
//!   P7a positive capture, P7b `.team` non-session exclusion, and P7c
//!   non-silent failure.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{Coordinator, ProviderRegistry, WorkspacePath};
use team_agent::provider::session::capture::capture_missing_provider_sessions_once;
use team_agent::provider::{
    get_adapter, CaptureSessionContext, Provider, ProviderAdapter, SessionId,
};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const AGENT: &str = "claude_compat";
const TARGET: &str = "22222222-2222-4222-8222-222222222222";
const OLD_TS: &str = "2020-01-01T00:00:00Z";

#[test]
#[serial(env)]
fn p1_expected_claude_session_is_addressed_without_candidate_enumeration_interference() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p1");
    let cwd = env.workspace("p1");
    let root = env.root().join("provider-projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);
    set_mtime(&target, SystemTime::UNIX_EPOCH + Duration::from_secs(1));

    for index in 0..301 {
        let path = cwd.join(format!("{AGENT}-enumeration-decoy-{index}.jsonl"));
        write_claude_user(
            &path,
            &format!("11111111-1111-4111-8111-{index:012}"),
            &cwd,
            None,
        );
    }

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    let paths = candidate_paths(&candidates);
    assert_eq!(
        paths,
        vec![target],
        "P1_RED_ENUMERATION_INTERFERENCE: Claude+expected must address exactly \
R/<slug>/<U>.jsonl; a >300 newer decoy pool must not evict or replace U; paths={paths:?}"
    );
}

#[test]
#[serial(env)]
fn p2_expected_codex_session_keeps_the_codex_scanner() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p2");
    let cwd = env.workspace("p2");
    let root = env.root().join("codex-sessions");
    let path = root.join(format!("rollout-2026-07-30T00-00-00-{TARGET}.jsonl"));
    write_codex(&path, TARGET, &cwd);

    let candidates = scan(Provider::Codex, context(&cwd, Some(TARGET), Some(root)));
    assert_eq!(
        candidate_paths(&candidates),
        vec![path],
        "P2_RED_PROVIDER_GATE: Codex+expected must keep the Codex rollout scanner; \
a Claude-only expected-path shortcut would return the wrong shape or no candidate"
    );
}

#[test]
#[serial(env)]
fn p3_claude_without_expected_session_keeps_existing_multi_candidate_behavior() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p3");
    let cwd = env.workspace("p3");
    let root = env.root().join("provider-projects");
    let first = root.join("first.jsonl");
    let second = root.join("second.jsonl");
    write_claude_user(&first, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &cwd, None);
    write_claude_user(&second, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &cwd, None);

    let candidates = scan(Provider::ClaudeCode, context(&cwd, None, Some(root)));
    let paths = candidate_paths(&candidates);
    assert!(
        paths.contains(&first) && paths.contains(&second),
        "P3_RED_NONE_BEHAVIOR_DRIFT: expected_session_id=None must retain the \
pre-change scanner and both ambiguous candidates; paths={paths:?}"
    );
}

#[test]
#[serial(env)]
fn p4_expected_path_is_not_double_prefixed_with_claude_projects() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p4");
    let cwd = env.workspace("p4");
    let root = env.root().join("provider-projects");
    let correct = expected_claude_path(&root, &cwd, TARGET);
    let doubled = root
        .join(".claude")
        .join("projects")
        .join(claude_slug(&cwd))
        .join(format!("{TARGET}.jsonl"));
    write_claude_user(&correct, TARGET, &cwd, None);
    write_claude_user(&doubled, TARGET, &cwd, None);

    let candidates = scan(Provider::Claude, context(&cwd, Some(TARGET), Some(root)));
    assert_eq!(
        candidate_paths(&candidates),
        vec![correct],
        "P4_RED_DOUBLE_PREFIX: R already denotes the provider projects root; \
the selected path must be R/<slug>/<U>.jsonl, never R/.claude/projects/<slug>/<U>.jsonl"
    );
}

#[test]
#[serial(env)]
fn p5_expected_claude_path_still_rejects_a_leader_transcript() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p5");
    let cwd = env.workspace("p5");
    let root = env.root().join("provider-projects");
    let path = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&path, TARGET, &cwd, Some("Claude Leader"));

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    assert!(
        candidates.is_empty(),
        "P5_RED_LEADER_MARKER_BYPASS: exact session-id addressing must not capture \
a transcript carrying the Claude leader marker; candidates={candidates:?}"
    );
}

#[test]
#[serial(env)]
fn p6_expected_claude_path_waits_for_user_or_assistant_lifecycle_record() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p6");
    let cwd = env.workspace("p6");
    let root = env.root().join("provider-projects");
    let path = expected_claude_path(&root, &cwd, TARGET);
    write_jsonl(
        &path,
        &[json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "sessionId": TARGET,
            "cwd": cwd.to_string_lossy(),
        })],
    );

    let candidates = scan(Provider::Claude, context(&cwd, Some(TARGET), Some(root)));
    assert!(
        candidates.is_empty(),
        "P6_RED_PREMATURE_TUPLE: queue-operation-only backing is not a usable \
provider session; capture must stay pending until type=user|assistant; candidates={candidates:?}"
    );
}

#[test]
#[serial(env)]
fn p7a_compatible_api_claude_provider_root_is_capturable_inside_team_runtime() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7a");
    let cwd = env.workspace("p7a");
    let root = cwd
        .join(".team/runtime/provider-config")
        .join(AGENT)
        .join("claude/projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    assert_eq!(
        candidate_paths(&candidates),
        vec![target],
        "P7A_RED_COMPATIBLE_ROOT_BLOCKED: a provider session root explicitly \
selected for a compatible_api Claude worker must be capturable even under .team"
    );
}

#[test]
#[serial(env)]
fn p7b_non_provider_session_team_files_stay_excluded_for_all_provider_families() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7b");
    let cwd = env.workspace("p7b");

    let claude_root = env.root().join("p7b-claude-projects");
    let claude_valid = expected_claude_path(&claude_root, &cwd, TARGET);
    write_claude_user(&claude_valid, TARGET, &cwd, None);
    let claude_junk = cwd.join(".team/runtime/provider-env/claude-junk.jsonl");
    write_claude_user(&claude_junk, TARGET, &cwd, None);
    let claude = scan(Provider::ClaudeCode, context(&cwd, None, Some(claude_root)));

    let codex_root = env.root().join("p7b-codex-sessions");
    let codex_valid = codex_root.join(format!("rollout-2026-07-30T00-00-00-{TARGET}.jsonl"));
    write_codex(&codex_valid, TARGET, &cwd);
    let codex_junk = cwd.join(".team/artifacts/codex-junk.jsonl");
    write_codex(&codex_junk, TARGET, &cwd);
    let codex = scan(
        Provider::Codex,
        context(&cwd, Some(TARGET), Some(codex_root)),
    );

    let copilot_junk = cwd.join(".team/runtime/provider-env/copilot-junk.jsonl");
    write_jsonl(
        &copilot_junk,
        &[json!({"id": TARGET, "cwd": cwd.to_string_lossy()})],
    );
    let copilot_home = env.root().join("p7b-copilot-home");
    let copilot_valid = write_copilot_store(&copilot_home, TARGET, &cwd);
    let _copilot_home = env.with_env(
        "COPILOT_HOME",
        copilot_home.to_str().expect("utf8 fixture path"),
    );
    let copilot = scan(Provider::Copilot, context(&cwd, Some(TARGET), None));

    for (provider, valid, junk, candidates) in [
        ("claude", claude_valid, claude_junk, claude),
        ("codex", codex_valid, codex_junk, codex),
        ("copilot", copilot_valid, copilot_junk, copilot),
    ] {
        let paths = candidate_paths(&candidates);
        assert_eq!(
            paths,
            vec![valid],
            "P7B_RED_TEAM_SHIELD_REMOVED: {provider} must retain its legal \
provider-session positive control while excluding non-session .team junk; \
narrowing the shield may exempt only an explicit provider session root, never \
runtime/artifact junk={junk:?}; paths={paths:?}"
        );
    }
}

#[test]
#[serial(env)]
fn p7c_compatible_api_failure_is_queryable_and_not_permanent_pending() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7c");

    let pending_cwd = env.workspace("p7c-pending");
    let pending_root = compatible_projects_root(&pending_cwd);
    std::fs::create_dir_all(&pending_root).unwrap();
    let mut pending = pending_state(&pending_cwd, &pending_root, "pending_first_turn");
    pending["agents"][AGENT]["spawned_at"] = json!(chrono::Utc::now().to_rfc3339());
    pending["agents"][AGENT]
        .as_object_mut()
        .unwrap()
        .remove("first_send_at");
    save_runtime_state(&pending_cwd, &pending).unwrap();
    coordinator(&pending_cwd)
        .tick()
        .expect("pending coordinator tick");
    let pending = load_runtime_state(&pending_cwd).unwrap();
    assert_eq!(
        pending.pointer(&format!("/agents/{AGENT}/capture_state")),
        Some(&json!("pending_first_turn")),
        "P7C pending control: a compatible_api worker before its first turn \
must remain pending, not be mislabeled as a permanent failure; state={pending}"
    );
    assert_eq!(
        event_count(&pending_cwd, "provider.session.transcript_missing"),
        0,
        "P7C pending control: no failure fact may be emitted before the first turn"
    );

    let failed_cwd = env.workspace("p7c-failed");
    let failed_root = compatible_projects_root(&failed_cwd);
    std::fs::create_dir_all(&failed_root).unwrap();
    save_runtime_state(
        &failed_cwd,
        &pending_state(&failed_cwd, &failed_root, "pending_first_turn"),
    )
    .unwrap();
    coordinator(&failed_cwd)
        .tick()
        .expect("failed coordinator tick");
    let failed = load_runtime_state(&failed_cwd).unwrap();
    assert_eq!(
        failed.pointer(&format!("/agents/{AGENT}/capture_state")),
        Some(&json!("transcript_missing")),
        "P7C_RED_PERMANENT_PENDING: after a strong trigger and expired grace, \
a compatible_api capture failure must become an independently queryable failure \
fact naming the agent; it must not remain indistinguishable from pending_first_turn; \
state={failed}"
    );
    assert_eq!(
        event_count(&failed_cwd, "provider.session.transcript_missing"),
        1,
        "P7C_RED_SILENT_CAPTURE_FAILURE: the durable observation surface must \
retain one queryable failure fact for {AGENT}; no receiver or push channel is prescribed"
    );
}

#[test]
#[serial(env)]
fn p8_terminal_capture_state_stops_repeat_scanning() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p8");
    let cwd = env.workspace("p8");
    let root = env.root().join("provider-projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);
    let mut state = pending_state(&cwd, &root, "attribution_ambiguous");

    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0).unwrap();
    assert!(
        report.assigned.is_empty()
            && !report.candidate_count_by_agent.contains_key(AGENT)
            && state
                .pointer(&format!("/agents/{AGENT}/session_id"))
                .is_none()
            && state.pointer(&format!("/agents/{AGENT}/capture_state"))
                == Some(&json!("attribution_ambiguous")),
        "P8_RED_TERMINAL_STATE_IGNORED: attribution_ambiguous is a terminal \
capture decision and must suppress the next scan; report={report:?} state={state}"
    );
}

#[test]
#[serial(env)]
fn p9_unchanged_attribution_ambiguity_emits_once() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p9");
    let cwd = env.workspace("p9");
    let root = env.root().join("provider-projects");
    write_claude_user(
        &root.join("a.jsonl"),
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        &cwd,
        None,
    );
    write_claude_user(
        &root.join("b.jsonl"),
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        &cwd,
        None,
    );
    let mut state = pending_state(&cwd, &root, "pending_first_turn");
    state["agents"][AGENT]["_pending_session_id"] = Value::Null;
    save_runtime_state(&cwd, &state).unwrap();
    let coordinator = coordinator(&cwd);

    coordinator.tick().expect("first coordinator tick");
    let first = event_count(&cwd, "provider.session.attribution_ambiguous");
    coordinator.tick().expect("second coordinator tick");
    let second = event_count(&cwd, "provider.session.attribution_ambiguous");

    assert_eq!(
        first, 1,
        "P9 fixture must produce exactly one initial ambiguity event; count={first}"
    );
    assert_eq!(
        second, first,
        "P9_RED_AMBIGUITY_EVENT_FLOOD: identical attribution_ambiguous state \
must not emit again on the next tick; first={first} second={second}"
    );
}

fn scan(
    provider: Provider,
    context: CaptureSessionContext,
) -> Vec<team_agent::provider::CapturedSessionCandidate> {
    get_adapter(provider)
        .capture_session_candidates(&context, 0)
        .expect("provider scan")
}

fn context(
    cwd: &Path,
    expected: Option<&str>,
    provider_projects_root: Option<PathBuf>,
) -> CaptureSessionContext {
    CaptureSessionContext {
        agent_id: AGENT.to_string(),
        spawn_cwd: cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: expected.map(SessionId::new),
        provider_projects_root,
    }
}

fn candidate_paths(candidates: &[team_agent::provider::CapturedSessionCandidate]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .captured
                .rollout_path
                .as_ref()
                .map(|path| path.as_path().to_path_buf())
        })
        .collect()
}

fn claude_slug(cwd: &Path) -> String {
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn expected_claude_path(root: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    root.join(claude_slug(cwd))
        .join(format!("{session_id}.jsonl"))
}

fn write_claude_user(path: &Path, session_id: &str, cwd: &Path, title: Option<&str>) {
    let mut records = Vec::new();
    if let Some(title) = title {
        records.push(json!({
            "type": "custom-title",
            "customTitle": title,
            "sessionId": session_id,
        }));
    }
    records.push(json!({
        "type": "user",
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
        "message": {"role": "user", "content": "fixture"},
    }));
    write_jsonl(path, &records);
}

fn write_codex(path: &Path, session_id: &str, cwd: &Path) {
    write_jsonl(
        path,
        &[json!({
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "cwd": cwd.to_string_lossy(),
            }
        })],
    );
}

fn write_copilot_store(home: &Path, session_id: &str, cwd: &Path) -> PathBuf {
    std::fs::create_dir_all(home).unwrap();
    let path = home.join("session-store.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "create table sessions (id text primary key, cwd text, updated_at integer)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into sessions (id, cwd, updated_at) values (?1, ?2, 1)",
        rusqlite::params![session_id, cwd.to_string_lossy()],
    )
    .unwrap();
    path
}

fn write_jsonl(path: &Path, records: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let text = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(path, text).unwrap();
}

fn set_mtime(path: &Path, when: SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

fn pending_state(cwd: &Path, root: &Path, capture_state: &str) -> Value {
    json!({
        "session_name": "team-attr-red",
        "active_team_key": "attr-red",
        "agents": {
            AGENT: {
                "agent_id": AGENT,
                "owner_team_id": "attr-red",
                "status": "running",
                "provider": "claude_code",
                "auth_mode": "compatible_api",
                "window": AGENT,
                "pane_id": "%71",
                "spawn_epoch": 1,
                "spawned_at": OLD_TS,
                "first_send_at": OLD_TS,
                "spawn_cwd": cwd.to_string_lossy(),
                "_pending_session_id": TARGET,
                "claude_projects_root": root.to_string_lossy(),
                "capture_state": capture_state,
            }
        }
    })
}

fn compatible_projects_root(cwd: &Path) -> PathBuf {
    cwd.join(".team/runtime/provider-config")
        .join(AGENT)
        .join("claude/projects")
}

fn event_count(workspace: &Path, event: &str) -> usize {
    team_agent::event_log::EventLog::new(workspace)
        .tail(0)
        .unwrap_or_default()
        .iter()
        .filter(|row| row.get("event").and_then(Value::as_str) == Some(event))
        .count()
}

struct AdapterRegistry;

impl ProviderRegistry for AdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }
}

fn coordinator(workspace: &Path) -> Coordinator {
    Coordinator::new(
        WorkspacePath::new(workspace.to_path_buf()),
        Box::new(AdapterRegistry),
        Box::new(NoopTransport::default()),
    )
}

#[derive(Clone, Default)]
struct NoopTransport;

impl Transport for NoopTransport {
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
        Ok(CapturedText {
            text: "OpenAI Codex\ncodex>".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
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
        Ok(vec![WindowName::new(AGENT)])
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
