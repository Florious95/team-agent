//! task#8 RED contract: tests that exercise Team Agent product side effects
//! must run inside one HOME/registry/socket/env hermetic boundary.
//!
//! References:
//! - `.team/artifacts/test-isolation-escape-locate.md` §8 R1-R6.
//! - Slice A: shared `HermeticTestEnv` is implementation-owned; this file keeps
//!   only the minimal hostile harness needed to express the RED contracts.
//! - Slice B: restart `auto_attach_leader` is product behavior and must write
//!   the same derived host leader registry entry as explicit binding commands.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::{restart_with_transport_with_readiness_deadline, RestartReport};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const CALLER_PANE: &str = "%7";
const TMUX_SOCKET: &str = "/private/tmp/tmux-501/ta-0515-auto-attach";
const TEAM_KEY: &str = "ctxteam";
const WORKER: &str = "alpha";

#[test]
fn r1_claim_takeover_contracts_must_enter_hermetic_home_boundary() {
    let source =
        read_repo_file("crates/team-agent/tests/explicit_claim_takeover_any_live_pane_red.rs");
    assert_contains_all(
        "R1",
        &source,
        &[
            "HermeticTestEnv",
            "assert_real_registry_unchanged",
            "registry_entries",
            "HOME",
        ],
        "ta-rs-bug3 claim/takeover contracts run real CARGO_BIN_EXE_team-agent and binding hooks; they must isolate HOME and prove real ~/.team-agent/leaders is unchanged while hermetic HOME gets the expected entries",
    );
    assert!(
        !source.contains("std::env::temp_dir()"),
        "R1: explicit_claim_takeover_any_live_pane_red.rs must allocate workspaces through HermeticTestEnv, not std::env::temp_dir()"
    );
}

#[test]
fn r2_bug4_canary_contract_must_not_observe_hostile_workspace_or_real_tmux() {
    let source = read_repo_file("crates/team-agent/tests/verify_rs031_window_consistency_red.rs");
    assert_contains_all(
        "R2",
        &source,
        &[
            "HermeticTestEnv",
            "TEAM_AGENT_WORKSPACE",
            "assert_store_under_root",
            "assert_path_under_root",
            "RecordingTransport",
            "BUG4 worker to leader canary",
        ],
        "BUG4 delivery contracts use MessageStore/send/deliver primitives; hostile TEAM_AGENT_* and TMUX inputs must be scrubbed, fixture DB rows must stay under the hermetic root, and delivery must use RecordingTransport only",
    );
    assert!(
        !source.contains("fn temp_ws("),
        "R2: verify_rs031_window_consistency_red.rs must not keep a standalone temp_ws helper; workspace allocation must come from HermeticTestEnv"
    );
}

#[test]
fn r3_socket_escape_contract_requires_declared_offline_or_recording_transport() {
    let verify = read_repo_file("crates/team-agent/tests/verify_rs031_window_consistency_red.rs");
    let quick = read_repo_file("crates/team-agent/tests/quick_start_worker_readiness_red.rs");
    let combined = format!("{verify}\n{quick}");
    assert_contains_all(
        "R3",
        &combined,
        &[
            "HermeticTestEnv",
            "scrub_tmux",
            "RecordingTransport",
            "assert_no_real_tmux",
        ],
        "non-real-machine contracts may set hostile TMUX/TMUX_PANE only through HermeticTestEnv and must prove they did not construct a real TmuxBackend/default transport",
    );
}

#[test]
fn r4_phase_golden_full_order_must_enter_hermetic_before_workspace_creation() {
    let source = read_repo_file("crates/team-agent/src/lifecycle/tests/phase_golden.rs");
    let hermetic = source.find("HermeticTestEnv").unwrap_or_else(|| {
        panic!("R4: phase_golden must import/enter shared HermeticTestEnv before creating test workspaces")
    });
    let workspace = source.find("two_worker_team_dir").unwrap_or_else(|| {
        panic!("R4 setup: phase_golden still expected to create the two-worker fixture")
    });
    assert!(
        hermetic < workspace,
        "R4: phase_golden must enter HermeticTestEnv before two_worker_team_dir/workspace creation so full --lib order cannot inherit prior env/HOME/socket state"
    );
    assert!(
        !source.contains("const CALLER_IDENTITY_ENVS"),
        "R4: phase_golden must not keep a private caller-identity scrub list that can drift from HermeticTestEnv"
    );
}

#[test]
#[serial(env)]
fn r5_restart_auto_attach_registers_isolated_live_leader_entry() {
    let case = RestartAutoAttachCase::new("r5-auto-attach");
    let _process_env = case.enter_process_env();
    case.seed_restartable_workspace();
    seed_healthy_coordinator(&case.workspace);

    let report = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM_KEY),
        &RestartAutoAttachTransport::new(),
        Some(1_000),
    )
    .expect("R5 setup: restart should complete against fake transport");
    assert!(
        matches!(report, RestartReport::Restarted { .. }),
        "R5 setup: restart must succeed before checking registry side effects; report={report:?}"
    );
    let state = load_runtime_state(&case.workspace).expect("read state after restart");
    assert_eq!(
        canonical_leader_receiver(&state)
            .and_then(|receiver| receiver.get("status"))
            .and_then(Value::as_str),
        Some("attached"),
        "R5 setup: restart auto_attach must bind canonical leader_receiver before registry assertion; state={state}"
    );

    let entries = case.registry_entries();
    assert_eq!(
        entries.len(),
        1,
        "R5: successful restart auto_attach_leader must write exactly one isolated HOME registry entry; canonical binding exists but entries={entries:?}"
    );
    let entry = &entries[0].1;
    assert_eq!(
        entry.get("source").and_then(Value::as_str),
        Some("restart-auto-attach"),
        "R5: restart auto_attach registry entry must identify source=restart-auto-attach; entry={entry}"
    );
    case.assert_no_tmp_registry_files();

    let leaders = case.run_cli(["leaders", "--json"]);
    let leaders_json = json_output(&leaders, "R5 leaders --json");
    assert!(
        leader_statuses(&leaders_json).iter().any(|(name, status)| {
            (name == TEAM_KEY || name.ends_with(&format!("/{TEAM_KEY}"))) && status == "LIVE"
        }),
        "R5: leaders --json under isolated HOME must list the restart auto-attached leader as LIVE; output={leaders_json} entry={entry}"
    );
}

#[test]
fn r6_static_guard_rejects_dangerous_tests_without_hermetic_boundary() {
    let synthetic = r#"
        use team_agent::message_store::MessageStore;
        fn fixture() {
            let _ = env!("CARGO_BIN_EXE_team-agent");
        }
    "#;
    assert!(
        static_guard_offenders([(
            "synthetic_missing_hermetic.rs".to_string(),
            synthetic.to_string()
        )])
        .iter()
        .any(|offender| offender.path == "synthetic_missing_hermetic.rs"),
        "R6 setup: synthetic MessageStore/CARGO_BIN_EXE test without HermeticTestEnv must be rejected"
    );

    let files = dangerous_test_files();
    let offenders = static_guard_offenders(files);
    assert!(
        offenders.is_empty(),
        "R6: any test importing MessageStore/CARGO_BIN_EXE/send/delivery/quick-start/launch/registry/binding CLI surfaces must enter HermeticTestEnv or declare an equivalent real-machine isolation marker. offenders(first 30)={:#?}",
        offenders.into_iter().take(30).collect::<Vec<_>>()
    );
}

fn assert_contains_all(label: &str, source: &str, needles: &[&str], message: &str) {
    let missing = needles
        .iter()
        .copied()
        .filter(|needle| !source.contains(needle))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "{label}: {message}; missing markers={missing:?}"
    );
}

#[allow(dead_code)]
#[derive(Debug)]
struct StaticOffender {
    path: String,
    signals: Vec<&'static str>,
}

fn static_guard_offenders(
    files: impl IntoIterator<Item = (String, String)>,
) -> Vec<StaticOffender> {
    files
        .into_iter()
        .filter_map(|(path, source)| {
            let signals = dangerous_signals(&source);
            if signals.is_empty() {
                return None;
            }
            let has_hermetic = source.contains("HermeticTestEnv");
            let has_real_machine_isolation = source.contains("real-machine")
                && source.contains("HOME")
                && source.contains("TMUX")
                && source.contains("TEAM_AGENT_WORKSPACE");
            if has_hermetic || has_real_machine_isolation {
                None
            } else {
                Some(StaticOffender { path, signals })
            }
        })
        .collect()
}

fn dangerous_signals(source: &str) -> Vec<&'static str> {
    [
        "MessageStore",
        "send_message",
        "deliver_pending_message",
        "deliver_pending_messages",
        "quick_start_with_transport",
        "launch_with_transport",
        "CARGO_BIN_EXE_team-agent",
        "leader::registry",
        "claim-leader",
        "takeover",
        "attach-leader",
    ]
    .into_iter()
    .filter(|needle| source.contains(needle))
    .collect()
}

fn dangerous_test_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    collect_rs_files(
        &root,
        "crates/team-agent/tests",
        |name| name.ends_with("_red.rs") || name.ends_with("_contract.rs"),
        &mut files,
    );
    collect_rs_files(
        &root,
        "crates/team-agent/src/lifecycle/tests",
        |name| name.ends_with(".rs"),
        &mut files,
    );
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn read_repo_file(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("read repo file {path}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect_rs_files(
    root: &Path,
    relative_dir: &'static str,
    keep: impl Fn(&str) -> bool,
    out: &mut Vec<(String, String)>,
) {
    let dir = root.join(relative_dir);
    let read_dir = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read static guard dir {}: {error}", dir.display()));
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !keep(name) {
            continue;
        }
        let relative = format!("{relative_dir}/{name}");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read static guard file {relative}: {error}"));
        out.push((relative, source));
    }
}

struct RestartAutoAttachCase {
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    fake_bin: PathBuf,
}

impl RestartAutoAttachCase {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let workspace = root.join("teamdir");
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("create isolated HOME");
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: restart auto attach registry contract\nprovider: fake\n---\n\nTeam.\n",
        )
        .expect("write TEAM.md");
        std::fs::write(
            workspace.join("agents").join(format!("{WORKER}.md")),
            format!(
                "---\nname: {WORKER}\nrole: Worker {WORKER}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
            ),
        )
        .expect("write worker role");
        let spec = team_agent::compiler::compile_team(&workspace).expect("compile team");
        std::fs::write(
            workspace.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .expect("write team.spec.yaml");
        let fake_bin = fake_tmux_bin(&root, &workspace);
        Self {
            root,
            workspace,
            home,
            fake_bin,
        }
    }

    fn enter_process_env(&self) -> EnvGuard {
        EnvGuard::set([
            ("HOME", Some(self.home.to_string_lossy().to_string())),
            (
                "PATH",
                Some(format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                )),
            ),
            ("TMUX", Some(format!("{TMUX_SOCKET},12345,0"))),
            ("TMUX_PANE", Some(CALLER_PANE.to_string())),
            ("TEAM_AGENT_LEADER_PROVIDER", Some("codex".to_string())),
            (
                "TEAM_AGENT_MACHINE_FINGERPRINT",
                Some("machine-0515-red".to_string()),
            ),
            ("TEAM_AGENT_LEADER_PANE_ID", None),
            ("TEAM_AGENT_LEADER_SESSION_UUID", None),
            ("TEAM_AGENT_WORKSPACE", None),
            ("TEAM_AGENT_TEAM_ID", None),
            ("TEAM_AGENT_OWNER_TEAM_ID", None),
            ("TEAM_AGENT_ACTIVE_TEAM", None),
            ("TEAM_AGENT_ID", None),
        ])
    }

    fn seed_restartable_workspace(&self) {
        let worker = json!({
            "status": "stopped",
            "provider": "fake",
            "role": format!("Worker {WORKER}"),
            "tools": ["mcp_team"],
            "window": WORKER,
            "spawn_cwd": self.workspace.to_string_lossy().to_string()
        });
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": TEAM_KEY,
                "team_key": TEAM_KEY,
                "team_dir": self.workspace.to_string_lossy().to_string(),
                "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy().to_string(),
                "session_name": "team-ctxteam",
                "tmux_endpoint": TMUX_SOCKET,
                "tmux_socket": TMUX_SOCKET,
                "agents": { WORKER: worker.clone() },
                "teams": {
                    TEAM_KEY: {
                        "active_team_key": TEAM_KEY,
                        "team_key": TEAM_KEY,
                        "team_dir": self.workspace.to_string_lossy().to_string(),
                        "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy().to_string(),
                        "session_name": "team-ctxteam",
                        "tmux_endpoint": TMUX_SOCKET,
                        "tmux_socket": TMUX_SOCKET,
                        "agents": { WORKER: worker }
                    }
                }
            }),
        )
        .expect("seed runtime state");
    }

    fn registry_entries(&self) -> Vec<(PathBuf, Value)> {
        registry_entries(&self.home)
    }

    fn assert_no_tmp_registry_files(&self) {
        let dir = self.home.join(".team-agent/leaders");
        let tmp = std::fs::read_dir(&dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".tmp") || name.ends_with(".tmp"))
            })
            .collect::<Vec<_>>();
        assert!(
            tmp.is_empty(),
            "R5: registry temp files must not remain: {tmp:?}"
        );
    }

    fn run_cli<const N: usize>(&self, args: [&str; N]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{TMUX_SOCKET},12345,0"))
            .env("TMUX_PANE", CALLER_PANE)
            .env_remove("TEAM_AGENT_LEADER_PANE_ID")
            .env_remove("TEAM_AGENT_LEADER_SESSION_UUID")
            .env_remove("TEAM_AGENT_WORKSPACE")
            .env_remove("TEAM_AGENT_TEAM_ID")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_ACTIVE_TEAM")
            .env_remove("TEAM_AGENT_ID")
            .output()
            .expect("run team-agent CLI")
    }
}

impl Drop for RestartAutoAttachCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[derive(Clone, Default)]
struct RestartAutoAttachTransport {
    pane_seq: Arc<Mutex<u64>>,
}

impl RestartAutoAttachTransport {
    fn new() -> Self {
        Self::default()
    }

    fn spawn_result(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        let mut pane_seq = self.pane_seq.lock().unwrap();
        let pane = format!("%{}", *pane_seq);
        *pane_seq = pane_seq.saturating_add(1);
        SpawnResult {
            pane_id: PaneId::new(pane),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(4242),
        }
    }
}

impl Transport for RestartAutoAttachTransport {
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
        Ok(self.spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(inject_report())
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

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(Some("fake".to_string()))
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![])
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new(WORKER)])
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

fn inject_report() -> InjectReport {
    InjectReport {
        stage_reached: InjectStage::Submit,
        inject_verification: InjectVerification::NoToken,
        submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
        turn_verification: TurnVerification::NotRequired,
        attempts: 1,
        submit_diagnostics: None,
    }
}

fn fake_tmux_bin(root: &Path, cwd: &Path) -> PathBuf {
    let bin = root.join("fake-bin");
    std::fs::create_dir_all(&bin).expect("create fake bin dir");
    let tmux = bin.join("tmux");
    let line = format!(
        "{CALLER_PANE}\tteam-agent-leader-0515\t0\tleader\t0\t/dev/ttys0515\tcodex\t1\t{}\t1\t0\t{}\n",
        cwd.display(),
        std::process::id()
    );
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *" list-panes "*) printf '%s' '{line}'; exit 0 ;;
  *" list-sessions "*) printf '%s\n' 'team-agent-leader-0515: 1 windows'; exit 0 ;;
  *" display-message "*) printf '%s\n' '{pid}'; exit 0 ;;
  *" has-session "*) exit 0 ;;
  *) exit 0 ;;
esac
"#,
        line = shell_single_quoted_payload(&line),
        pid = std::process::id()
    );
    std::fs::write(&tmux, script).expect("write fake tmux");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tmux");
    }
    bin
}

fn registry_entries(home: &Path) -> Vec<(PathBuf, Value)> {
    let dir = home.join(".team-agent/leaders");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .map(|path| {
            let value = serde_json::from_str::<Value>(
                &std::fs::read_to_string(&path).expect("read registry entry"),
            )
            .expect("parse registry json");
            (path, value)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn leader_statuses(value: &Value) -> Vec<(String, String)> {
    value
        .get("leaders")
        .or_else(|| value.get("entries"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            let name = entry
                .get("name")
                .or_else(|| entry.get("qualified_name"))
                .or_else(|| entry.get("team_key"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (name, status)
        })
        .collect()
}

fn canonical_leader_receiver<'a>(state: &'a Value) -> Option<&'a Value> {
    state
        .pointer(&format!("/teams/{TEAM_KEY}/leader_receiver"))
        .or_else(|| state.get("leader_receiver"))
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{label}: expected success; code={:?} stdout={stdout} stderr={stderr}",
        output.status.code()
    );
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("{label}: parse JSON failed: {error}; stdout={stdout:?}; stderr={stderr:?}")
    })
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace_path = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace)).unwrap();
    let _ = team_agent::message_store::MessageStore::open(workspace).unwrap();
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace_path,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace_path),
        pid.to_string(),
    )
    .unwrap();
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set<const N: usize>(values: [(&'static str, Option<String>); N]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("create TEAM_AGENT_TEST_TMP root");
    let dir = root.join(format!(
        "ta-0515-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::canonicalize(dir).expect("canonicalize temp dir")
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}
