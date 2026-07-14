//! 0.5.41 RED contracts: stale runtime truth must be visible, not hidden behind
//! cached state / agent_health rows.
//!
//! Reference: `.team/artifacts/fault-invisibility-locate.md` §9.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::status::{agent_summary_counts, format_status_csv};
use team_agent::coordinator::{
    coordinator_meta_path, coordinator_pid_path, start_coordinator_with_team, stop_coordinator,
    Coordinator, ErrorLists, MetadataSource, Pid, ProviderRegistry, StartOutcome, WorkspacePath,
    PROTOCOL_VERSION,
};
use team_agent::db::schema::open_db;
use team_agent::lifecycle::{coordinator_start_summary_value, CoordinatorStartSummary};
use team_agent::message_store::MessageStore;
use team_agent::model::enums::Provider;
use team_agent::model::paths::runtime_dir;
use team_agent::provider::{get_adapter, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, runtime_state_path, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const TEAM: &str = "current";
const WORKER: &str = "helper";
const TEAM_SESSION: &str = "team-current";
const WORKER_PANE: &str = "%541";
const LEADER_PANE: &str = "%0";
const TMUX_ENDPOINT: &str = "/Volumes/nvme/tmp/ta-0541-fault-invisibility.sock";
const CALLER_IDENTITY_ENV: &str = "TEAM_AGENT_TEST_CALLER_BINARY_IDENTITY";

#[test]
#[serial(env)]
fn cross_boot_stale_bindings_are_visible_in_status_and_diagnose_without_mutation() {
    let case = CliCase::new("red1-boot-stale");
    case.write_fake_tmux("codex", "");
    case.seed_active_state(stale_busy_worker());
    case.seed_agent_health("WORKING");
    case.write_coordinator_metadata(
        std::process::id(),
        Some(cli_binary_path()),
        Some(current_version()),
        PROTOCOL_VERSION,
    );
    case.write_coordinator_tick("old-boot");
    let before = case.snapshot_runtime_files();

    let status = case.status_json_with_host_boot("new-boot");
    let diagnose = case.diagnose_json_with_host_boot("new-boot");
    let csv = format_status_csv(&status);
    let counts = agent_summary_counts(
        status.get("agents").unwrap_or(&Value::Null),
        status.get("agent_health").unwrap_or(&Value::Null),
    );

    assert!(
        value_contains(&status, "runtime_bindings_stale_after_boot"),
        "RED1: status --json --detail must expose runtime_bindings_stale_after_boot when coordinator_tick.host_boot_id != current host boot id; status={status}"
    );
    let worker = status
        .pointer("/agents/helper")
        .unwrap_or_else(|| panic!("RED1 setup: status missing helper; status={status}"));
    assert_eq!(
        worker.get("stale").and_then(Value::as_bool),
        Some(true),
        "RED1: every pane-bound worker must be stale across host boot mismatch; worker={worker}; status={status}"
    );
    assert_ne!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "RED1: cross-boot stale worker must not keep cached BUSY/working state; worker={worker}"
    );
    assert_eq!(
        counts.busy, 0,
        "RED1: summary counts must not count stale pre-boot WORKING health as busy; counts={counts:?}; status={status}"
    );
    assert!(
        !csv.contains("helper,工作"),
        "RED1: human CSV must not render stale pre-boot worker as 工作; csv={csv}; status={status}"
    );
    assert!(
        value_contains(&diagnose, "runtime_bindings_stale_after_boot")
            && value_contains(&diagnose, "team-agent restart"),
        "RED1: diagnose --json must include runtime_bindings_stale_after_boot and a restart hint; diagnose={diagnose}"
    );
    case.assert_runtime_files_unchanged(before, "RED1 status/diagnose read-only");
}

#[test]
#[serial(env)]
fn status_uses_same_coordinator_service_truth_as_diagnose_for_four_health_shapes() {
    let mut violations = Vec::new();

    for shape in [
        CoordinatorShape::StalePid,
        CoordinatorShape::HealthySameBinary,
        CoordinatorShape::LiveStaleIdentity,
        CoordinatorShape::DaemonNewerThanCaller,
    ] {
        let case = CliCase::new(shape.tag());
        case.write_fake_tmux("codex", "");
        case.seed_active_state(running_worker());
        case.seed_agent_health("WORKING");
        shape.seed(&case);
        let _caller_identity = shape.caller_identity_guard(&case);

        let status = case.status_json();
        let diagnose = case.diagnose_json();
        let service_available = status
            .pointer("/coordinator/service_available")
            .and_then(Value::as_bool);
        if service_available != Some(shape.expected_service_available()) {
            violations.push(format!(
                "{}: status.coordinator.service_available must be {:?}; got {:?}; status={status}",
                shape.tag(),
                shape.expected_service_available(),
                service_available
            ));
        }
        if status
            .pointer("/runtime/coordinator/ok")
            .and_then(Value::as_bool)
            != Some(shape.expected_service_available())
        {
            violations.push(format!(
                "{}: runtime.coordinator.ok must use service_available, not only pid-running; status={status}",
                shape.tag()
            ));
        }
        if shape.expect_diagnose_issue()
            && !value_contains(&diagnose, shape.expected_diagnose_issue())
        {
            violations.push(format!(
                "{}: diagnose must still expose {}; diagnose={diagnose}",
                shape.tag(),
                shape.expected_diagnose_issue()
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "RED2: coordinator health/status/diagnose must share one service-availability truth:\n{}",
        violations.join("\n")
    );
}

#[test]
#[serial(env)]
fn coordinator_rotation_reports_directionality_without_flattening_newer_daemon_preservation() {
    let old = CoordinatorStartCase::new("red3-old-daemon-rotates");
    let old_child = old.spawn_daemon_metadata("0.5.39");
    let old_report = start_coordinator_with_team(&old.workspace, Some(TEAM))
        .expect("start coordinator with old daemon");
    assert_eq!(
        old_report.status,
        StartOutcome::StartedAfterRotation,
        "RED3 setup: current caller must still rotate an older live daemon; report={old_report:?}"
    );
    assert_eq!(
        old_report.rotation_reason.as_deref(),
        Some("binary_version_mismatch"),
        "RED3 setup: old-daemon rotation must remain directional and loud; report={old_report:?}"
    );
    drop(old_child);

    let newer = CoordinatorStartCase::new("red3-newer-daemon-preserved");
    let _caller = newer.caller_identity("0.5.39");
    let newer_pid = Pid::new(std::process::id());
    newer.write_metadata(
        newer_pid,
        Some(cli_binary_path()),
        Some("0.5.40".to_string()),
    );
    fs::write(
        coordinator_pid_path(&newer.workspace),
        newer_pid.to_string(),
    )
    .expect("write newer pid");

    let newer_report = start_coordinator_with_team(&newer.workspace, Some(TEAM))
        .expect("start coordinator with newer daemon");
    let summary =
        coordinator_start_summary_value(&CoordinatorStartSummary::from_start_report(&newer_report));

    assert_eq!(
        newer_report.status,
        StartOutcome::AlreadyRunning,
        "RED3: older caller must preserve a service-compatible newer daemon; report={newer_report:?}"
    );
    assert_eq!(
        summary
            .get("binary_identity_relation")
            .and_then(Value::as_str),
        Some("daemon_newer_than_caller"),
        "RED3: restart/start coordinator summary must expose binary_identity_relation=daemon_newer_than_caller, not only flatten to already_running; report={newer_report:?} summary={summary}"
    );
}

#[test]
#[serial(env)]
fn wrapper_worker_provider_exit_marker_beats_pane_liveness_and_cached_working_health() {
    let case = WatchCase::new("red4-provider-exited");
    case.seed_state(adversarial_wrapper_worker(std::process::id()));
    case.seed_agent_health("WORKING");
    let marker = format!(
        "{} 1",
        team_agent::tmux_backend::worker_provider_exit_marker("codex")
    );

    case.coordinator(WatchTransport::provider_exited(&marker))
        .tick()
        .expect("coordinator tick");

    let state = case.read_state();
    let watch = state
        .pointer("/coordinator/abnormal_exit_watch/helper")
        .unwrap_or_else(|| panic!("RED4 setup: abnormal watch missing helper; state={state}"));
    assert!(
        value_contains(watch, "worker_provider_exited")
            || value_contains(watch, "provider_exit_marker"),
        "RED4: abnormal watch must record worker provider exit marker, not treat wrapper pane_pid/pane liveness as provider alive; watch={watch}; state={state}"
    );
    assert_eq!(
        watch.get("provider_process_dead").and_then(Value::as_bool),
        Some(true),
        "RED4: provider exit marker means provider_process_dead even though pane remains alive; watch={watch}"
    );

    let status = case.status_json();
    let worker = status
        .pointer("/agents/helper")
        .unwrap_or_else(|| panic!("RED4 setup: status missing helper; status={status}"));
    let csv = format_status_csv(&status);
    assert_ne!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "RED4: provider-exited wrapper pane must not remain BUSY from cached state; worker={worker}; status={status}"
    );
    assert!(
        !csv.contains("helper,工作") && !csv.contains("helper,空闲"),
        "RED4: human CSV must render provider-exited wrapper as 错误/未知, not 工作/空闲; csv={csv}; status={status}"
    );

    let live = WatchCase::new("red4-live-provider-guard");
    live.seed_state(live_provider_worker());
    live.seed_agent_health("WORKING");
    let live_status = live.status_json();
    let live_csv = format_status_csv(&live_status);
    assert!(
        live_csv.contains("helper,工作"),
        "RED4 guard: a live provider current-command still matching codex must remain renderable as 工作; csv={live_csv}; status={live_status}"
    );
}

#[test]
fn real_machine_fault_invisibility_gate_is_declared() {
    let candidates = candidate_gate_files();
    let required = [
        "FAULT_INVISIBILITY_0541_REAL_MACHINE",
        "TEAM_AGENT_TEST_HOST_BOOT_ID",
        "runtime_bindings_stale_after_boot",
        "worker_provider_exit_marker",
        "team-agent restart",
        "status",
        "diagnose",
    ];
    let matches = candidates
        .iter()
        .filter(|(_, text)| required.iter().all(|needle| text.contains(needle)))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();

    assert!(
        !matches.is_empty(),
        "RED5: a real-machine gate declaration must cover host-boot stale bindings plus wrapper provider-exit visibility. Missing marker FAULT_INVISIBILITY_0541_REAL_MACHINE; scanned {} files.",
        candidates.len()
    );
}

struct CliCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl CliCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        env.assert_no_real_tmux();
        let workspace = env.workspace(tag);
        let fake_bin = env.root().join(format!("fake-bin-{tag}"));
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        fs::create_dir_all(runtime_dir(&workspace)).expect("create runtime dir");
        let _ = MessageStore::open(&workspace).expect("create message store");
        Self {
            env,
            workspace,
            fake_bin,
        }
    }

    fn seed_active_state(&self, worker: Value) {
        save_runtime_state(&self.workspace, &base_state(&self.workspace, worker))
            .expect("save active state");
    }

    fn seed_agent_health(&self, status: &str) {
        seed_agent_health(&self.workspace, status);
    }

    fn write_fake_tmux(&self, worker_command: &str, capture_text: &str) {
        write_fake_tmux(
            &self.fake_bin,
            &self.workspace,
            worker_command,
            capture_text,
        );
    }

    fn write_coordinator_metadata(
        &self,
        pid: u32,
        binary_path: Option<String>,
        binary_version: Option<String>,
        protocol_version: u32,
    ) {
        write_raw_coordinator_metadata(
            &WorkspacePath::new(self.workspace.clone()),
            Pid::new(pid),
            binary_path,
            binary_version,
            protocol_version,
        );
    }

    fn write_coordinator_tick(&self, host_boot_id: &str) {
        fs::write(
            runtime_dir(&self.workspace).join("coordinator_tick.json"),
            serde_json::to_string_pretty(&json!({
                "coordinator_tick_iteration_count": 7,
                "pid": std::process::id(),
                "boot_id": "daemon-old",
                "host_boot_id": host_boot_id,
                "last_phase": "tick_finished",
                "last_tick_status": "ok",
                "updated_at": "2026-07-14T00:00:00Z"
            }))
            .expect("serialize coordinator tick"),
        )
        .expect("write coordinator tick");
    }

    fn status_json(&self) -> Value {
        self.status_json_with_extra_env(&[])
    }

    fn status_json_with_host_boot(&self, boot_id: &str) -> Value {
        self.status_json_with_extra_env(&[("TEAM_AGENT_TEST_HOST_BOOT_ID", boot_id)])
    }

    fn status_json_with_extra_env(&self, extra: &[(&str, &str)]) -> Value {
        parse_json_output(
            &self.run_ta(
                &[
                    "status",
                    "--workspace",
                    self.workspace_str(),
                    "--team",
                    TEAM,
                    "--json",
                    "--detail",
                ],
                extra,
            ),
            "status --json --detail",
        )
    }

    fn diagnose_json(&self) -> Value {
        self.diagnose_json_with_extra_env(&[])
    }

    fn diagnose_json_with_host_boot(&self, boot_id: &str) -> Value {
        self.diagnose_json_with_extra_env(&[("TEAM_AGENT_TEST_HOST_BOOT_ID", boot_id)])
    }

    fn diagnose_json_with_extra_env(&self, extra: &[(&str, &str)]) -> Value {
        parse_json_output(
            &self.run_ta(
                &["diagnose", "--workspace", self.workspace_str(), "--json"],
                extra,
            ),
            "diagnose --json",
        )
    }

    fn run_ta(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{TMUX_ENDPOINT},12345,0"))
            .env("TMUX_PANE", LEADER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0541-red");
        for (key, value) in extra {
            command.env(key, value);
        }
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn snapshot_runtime_files(&self) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        [
            runtime_state_path(&self.workspace),
            runtime_dir(&self.workspace).join("team.db"),
            runtime_dir(&self.workspace).join("coordinator.pid"),
            runtime_dir(&self.workspace).join("coordinator.json"),
            runtime_dir(&self.workspace).join("coordinator_tick.json"),
            self.workspace.join(".team/logs/events.jsonl"),
        ]
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
    }

    fn assert_runtime_files_unchanged(&self, before: Vec<(PathBuf, Option<Vec<u8>>)>, label: &str) {
        for (path, expected) in before {
            let actual = fs::read(&path).ok();
            assert_eq!(
                actual,
                expected,
                "{label}: status/diagnose must be read-only; changed {}",
                path.display()
            );
        }
    }
}

enum CoordinatorShape {
    StalePid,
    HealthySameBinary,
    LiveStaleIdentity,
    DaemonNewerThanCaller,
}

impl CoordinatorShape {
    fn tag(&self) -> &'static str {
        match self {
            Self::StalePid => "red2-stale-pid",
            Self::HealthySameBinary => "red2-healthy-same",
            Self::LiveStaleIdentity => "red2-stale-identity",
            Self::DaemonNewerThanCaller => "red2-newer-daemon",
        }
    }

    fn seed(&self, case: &CliCase) {
        match self {
            Self::StalePid => case.write_coordinator_metadata(
                4_000_000,
                Some(cli_binary_path()),
                Some(current_version()),
                PROTOCOL_VERSION,
            ),
            Self::HealthySameBinary => case.write_coordinator_metadata(
                std::process::id(),
                Some(cli_binary_path()),
                Some(current_version()),
                PROTOCOL_VERSION,
            ),
            Self::LiveStaleIdentity => case.write_coordinator_metadata(
                std::process::id(),
                Some(cli_binary_path()),
                Some("0.5.39".to_string()),
                PROTOCOL_VERSION,
            ),
            Self::DaemonNewerThanCaller => case.write_coordinator_metadata(
                std::process::id(),
                Some(cli_binary_path()),
                Some("0.5.40".to_string()),
                PROTOCOL_VERSION,
            ),
        }
    }

    fn caller_identity_guard<'a>(&self, case: &'a CliCase) -> Option<hermetic_guard::EnvOverride> {
        matches!(self, Self::DaemonNewerThanCaller).then(|| {
            case.env.with_env(
                CALLER_IDENTITY_ENV,
                &json!({
                    "binary_path": cli_binary_path(),
                    "binary_version": "0.5.39"
                })
                .to_string(),
            )
        })
    }

    fn expected_service_available(&self) -> bool {
        !matches!(self, Self::StalePid)
    }

    fn expect_diagnose_issue(&self) -> bool {
        matches!(self, Self::StalePid | Self::LiveStaleIdentity)
    }

    fn expected_diagnose_issue(&self) -> &'static str {
        match self {
            Self::StalePid => "coordinator_unavailable",
            Self::LiveStaleIdentity => "coordinator_stale_identity",
            Self::HealthySameBinary | Self::DaemonNewerThanCaller => "",
        }
    }
}

struct CoordinatorStartCase {
    env: hermetic_guard::HermeticTestEnv,
    root: PathBuf,
    workspace: WorkspacePath,
}

impl CoordinatorStartCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let root = env.workspace(tag);
        fs::create_dir_all(runtime_dir(&root)).expect("create runtime dir");
        let _ = MessageStore::open(&root).expect("create message store");
        Self {
            env,
            workspace: WorkspacePath::new(root.clone()),
            root,
        }
    }

    fn spawn_daemon_metadata(&self, version: &str) -> DaemonChild {
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep daemon fixture");
        let pid = Pid::new(child.id());
        self.write_metadata(pid, Some(cli_binary_path()), Some(version.to_string()));
        fs::write(coordinator_pid_path(&self.workspace), pid.to_string())
            .expect("write coordinator pid");
        DaemonChild {
            child: Some(child),
            workspace: self.workspace.clone(),
        }
    }

    fn write_metadata(
        &self,
        pid: Pid,
        binary_path: Option<String>,
        binary_version: Option<String>,
    ) {
        write_raw_coordinator_metadata(
            &self.workspace,
            pid,
            binary_path,
            binary_version,
            PROTOCOL_VERSION,
        );
    }

    fn caller_identity(&self, version: &str) -> hermetic_guard::EnvOverride {
        self.env.with_env(
            CALLER_IDENTITY_ENV,
            &json!({
                "binary_path": cli_binary_path(),
                "binary_version": version
            })
            .to_string(),
        )
    }
}

impl Drop for CoordinatorStartCase {
    fn drop(&mut self) {
        let _ = stop_coordinator(&self.workspace);
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct DaemonChild {
    child: Option<Child>,
    workspace: WorkspacePath,
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = stop_coordinator(&self.workspace);
    }
}

struct WatchCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
}

impl WatchCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        fs::create_dir_all(runtime_dir(&workspace)).expect("create runtime dir");
        let _ = MessageStore::open(&workspace).expect("create message store");
        Self { env, workspace }
    }

    fn seed_state(&self, worker: Value) {
        let _ = &self.env;
        save_runtime_state(&self.workspace, &base_state(&self.workspace, worker))
            .expect("save watch state");
    }

    fn seed_agent_health(&self, status: &str) {
        seed_agent_health(&self.workspace, status);
    }

    fn coordinator(&self, transport: WatchTransport) -> Coordinator {
        Coordinator::new(
            WorkspacePath::new(self.workspace.clone()),
            Box::new(RealAdapterRegistry),
            Box::new(transport),
        )
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("read runtime state")
    }

    fn status_json(&self) -> Value {
        team_agent::cli::status_port::status_scoped(
            &self.workspace,
            &self.read_state(),
            Some(TEAM),
            false,
            true,
        )
        .expect("status_scoped")
    }
}

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists {
            whitelist: Vec::new(),
            blacklist: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct WatchTransport {
    current_command: String,
    capture_text: String,
}

impl WatchTransport {
    fn provider_exited(marker: &str) -> Self {
        Self {
            current_command: "zsh".to_string(),
            capture_text: format!("{marker}\n$ "),
        }
    }
}

impl Transport for WatchTransport {
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
            pane_id: PaneId::new(WORKER_PANE),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(std::process::id()),
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
            text: self.capture_text.clone(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![PaneInfo {
            pane_id: PaneId::new(WORKER_PANE),
            session: SessionName::new(TEAM_SESSION),
            window_index: Some(0),
            window_name: Some(WindowName::new(WORKER)),
            pane_index: Some(0),
            tty: None,
            current_command: Some(self.current_command.clone()),
            current_path: None,
            active: true,
            pane_pid: Some(std::process::id()),
            leader_env: BTreeMap::new(),
        }])
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

fn base_state(workspace: &Path, worker: Value) -> Value {
    json!({
        "active_team_key": TEAM,
        "team_key": TEAM,
        "session_name": TEAM_SESSION,
        "team_dir": workspace.to_string_lossy(),
        "workspace": workspace.to_string_lossy(),
        "tmux_endpoint": TMUX_ENDPOINT,
        "tmux_socket": TMUX_ENDPOINT,
        "agents": {
            WORKER: worker.clone()
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": LEADER_PANE,
            "session_name": "leader-0541",
            "window_name": "leader",
            "owner_epoch": 1
        },
        "teams": {
            TEAM: {
                "active_team_key": TEAM,
                "team_key": TEAM,
                "session_name": TEAM_SESSION,
                "team_dir": workspace.to_string_lossy(),
                "workspace": workspace.to_string_lossy(),
                "tmux_endpoint": TMUX_ENDPOINT,
                "tmux_socket": TMUX_ENDPOINT,
                "agents": {
                    WORKER: worker
                }
            }
        }
    })
}

fn stale_busy_worker() -> Value {
    let mut worker = running_worker();
    worker["worker_state"] = json!("BUSY");
    worker["activity"] = json!({
        "status": "working",
        "rationale": "adversarial stale cache"
    });
    worker
}

fn running_worker() -> Value {
    json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "spawn_epoch": 0,
        "spawned_at": "2026-07-14T00:00:00+00:00",
        "spawn_cwd": "/tmp",
        "owner_team_id": TEAM
    })
}

fn adversarial_wrapper_worker(live_pane_pid: u32) -> Value {
    let rollout = std::env::temp_dir().join(format!(
        "ta-0541-provider-exit-{}-{}.jsonl",
        std::process::id(),
        live_pane_pid
    ));
    fs::write(&rollout, "{}\n").expect("write rollout");
    json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "pane_pid": live_pane_pid,
        "worker_state": "BUSY",
        "activity": {"status": "working"},
        "rollout_path": rollout.to_string_lossy(),
        "spawn_epoch": 1,
        "spawned_at": "2026-07-14T00:00:00+00:00",
        "owner_team_id": TEAM
    })
}

fn live_provider_worker() -> Value {
    let mut worker = running_worker();
    worker["worker_state"] = json!("BUSY");
    worker["activity"] = json!({"status": "working"});
    worker["pane_current_command"] = json!("codex");
    worker
}

fn seed_agent_health(workspace: &Path, status: &str) {
    let store = MessageStore::open(workspace).expect("open message store");
    let conn = open_db(store.db_path()).expect("open team db");
    conn.execute(
        "insert or replace into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            TEAM,
            WORKER,
            status,
            "2026-07-14T00:00:00Z",
            50_i64,
            "msg-stale",
            "2026-07-14T00:00:00Z"
        ],
    )
    .expect("seed agent_health");
}

fn write_raw_coordinator_metadata(
    workspace: &WorkspacePath,
    pid: Pid,
    binary_path: Option<String>,
    binary_version: Option<String>,
    protocol_version: u32,
) {
    fs::write(coordinator_pid_path(workspace), pid.to_string()).expect("write pid");
    fs::write(
        coordinator_meta_path(workspace),
        serde_json::to_string_pretty(&json!({
            "pid": pid.get(),
            "protocol_version": protocol_version,
            "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
            "binary_path": binary_path,
            "binary_version": binary_version,
            "source": MetadataSource::Boot,
            "updated_at": "2026-07-14T00:00:00Z"
        }))
        .expect("serialize metadata"),
    )
    .expect("write metadata");
}

fn write_fake_tmux(fake_bin: &Path, workspace: &Path, worker_command: &str, capture_text: &str) {
    let tmux = fake_bin.join("tmux");
    let worker_line = pane_line(
        WORKER_PANE,
        TEAM_SESSION,
        WORKER,
        worker_command,
        workspace,
        std::process::id(),
    );
    let leader_line = pane_line(
        LEADER_PANE,
        "leader-0541",
        "leader",
        "codex",
        workspace,
        std::process::id(),
    );
    let script = format!(
        r#"#!/bin/sh
target=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then
    target="$arg"
  fi
  previous="$arg"
done
case " $* " in
  *" has-session "*) exit 0 ;;
  *" list-windows "*) printf '%s\n' '0: {worker}'; exit 0 ;;
  *" list-sessions "*) printf '%s\n' '{team_session}: 1 windows'; exit 0 ;;
  *" list-panes "*) printf '%s' '{leader_line}'; printf '%s' '{worker_line}'; exit 0 ;;
  *" capture-pane "*) printf '%s\n' '{capture_text}'; exit 0 ;;
  *" display-message "*)
    case "$target" in
      %*) printf '%s\n' "$target"; exit 0 ;;
      *"{worker}"*) printf '%s\n' '{worker_pane}'; exit 0 ;;
      *) printf '%s\n' '{leader_pane}'; exit 0 ;;
    esac
    ;;
  *) exit 0 ;;
esac
"#,
        worker = WORKER,
        team_session = TEAM_SESSION,
        worker_line = shell_single_quoted_payload(&worker_line),
        leader_line = shell_single_quoted_payload(&leader_line),
        capture_text = shell_single_quoted_payload(capture_text),
        worker_pane = WORKER_PANE,
        leader_pane = LEADER_PANE,
    );
    fs::write(&tmux, script).expect("write fake tmux");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod fake tmux");
    }
}

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0541\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn parse_json_output(output: &Output, label: &str) -> Value {
    assert!(
        !output.stdout.is_empty(),
        "{label}: expected JSON stdout; status={} stderr={}",
        output.status,
        text(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label}: parse JSON failed: {error}; stdout={} stderr={}",
            text(&output.stdout),
            text(&output.stderr)
        )
    })
}

fn value_contains(value: &Value, needle: &str) -> bool {
    value.to_string().contains(needle)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}

fn cli_binary_path() -> String {
    fs::canonicalize(env!("CARGO_BIN_EXE_team-agent"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_team-agent")))
        .to_string_lossy()
        .to_string()
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn candidate_gate_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for relative_dir in [
        "crates/team-agent/tests",
        "crates/team-agent/tests/e2e/cases",
        "tools",
        ".team/artifacts/gate-harness",
    ] {
        let dir = root.join(relative_dir);
        if dir.exists() {
            collect_text_files(&root, &dir, &mut files);
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.ends_with("fault_invisibility_0541_contract.rs"))
        .collect()
}

fn collect_text_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("read candidate dir") {
        let path = entry.expect("read candidate entry").path();
        if path.is_dir() {
            collect_text_files(root, &path, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(ext, "rs" | "sh" | "md" | "json" | "toml" | "yaml" | "yml") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            out.push((relative_path(root, &path), text));
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
