//! B7 / 4e8ff838: real Lifecycle authority for a controlled, owned native TTY.
//! Same admission/startup-handshake path as m4_native_delivery_dev, not provider acceptance.
#[path = "../../../team-agent-contract/tests/physical_support/mod.rs"]
mod physical_support;
#[path = "temp_workspace.rs"]
mod temp_workspace;

use std::{path::{Path, PathBuf}, process::Command, time::{Duration, Instant}};
use serde_json::json;
use team_agent::contract_runtime::backend::{self, Binding};
use team_agent_contract::{
    contract::{descriptor::{AuthMode, FreshSession}, hooks::{PlanHook, ProviderHooks}, plan::*, types::*},
    host::{clock::RealClock, command::{OutputLimits, RealCommandRunner}, materialize::ScopedMaterializer,
        process::{capture_process, fingerprint_file, resolve_cwd}, tmux::{HostLimits, TmuxHost}, transport::TargetReceipt},
    orchestration::{lifecycle::*, physical::*, store::*},
};

fn limits() -> HostLimits {
    HostLimits { command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 1024 * 1024 },
        max_script_bytes: 128 * 1024, max_capture_bytes: 256 * 1024, max_payload_bytes: 1024 * 1024,
        max_executable_bytes: 1024 * 1024 * 1024, poll_interval: Duration::from_millis(10), columns: 120, rows: 40 }
}
fn selected(command: &str) -> PathBuf {
    let output = Command::new("sh").args(["-c", &format!("command -v {command}")]).output().unwrap();
    assert!(output.status.success(), "public {command} is required");
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).canonicalize().unwrap()
}
struct ReceiverPlan(PathBuf);
impl PlanHook for ReceiverPlan {
    fn plan(&self, launch: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let mut plan = physical_support::HOOK.plan(launch)?;
        plan.arguments[1] = self.0.clone().into_os_string();
        Ok(plan)
    }
}

pub struct NativeAuthority {
    pub socket: String,
    pub pane: String,
    pub pid: u32,
    tmux: PathBuf,
    target: Option<TargetReceipt>,
    _storage: temp_workspace::TempWorkspace,
}
impl Drop for NativeAuthority {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            let clock = RealClock::new();
            if let Ok(mut host) = TmuxHost::restore_owned(target.clone(), self.tmux.clone(), limits(),
                RealCommandRunner::default(), &clock, Duration::from_secs(3)) {
                let receipt = host.close_owned_pane(&target, &clock, Duration::from_secs(3));
                if receipt.pane_close.outcome != team_agent_contract::contract::delivery::StepOutcome::Confirmed {
                    eprintln!("owned native pane cleanup unconfirmed: {receipt:?}");
                }
            }
        }
    }
}
impl NativeAuthority {
    pub fn start(root: &Path, provider: &'static str) -> Self {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::io::Write;
        let canonical = root.canonicalize().unwrap();
        let root = canonical.as_path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = selected("tmux");
        let python = selected("python3");
        let cwd = resolve_cwd(root).unwrap();
        let scope = backend::scope_for(&cwd, "alpha").unwrap();
        let endpoint = format!("framework:{}", scope.as_str());
        // The existing short-root fixture keeps macOS's 104-byte socket boundary intact.
        let storage = temp_workspace::TempWorkspace::new("ddn", "native");
        let transport_root = storage.join("store");
        let mut store = ProjectionStore::create(&transport_root, scope.clone(), &endpoint).unwrap();
        let identity = store.propose_identity(&SeatId::new("worker").unwrap()).unwrap();
        let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
        let candidate_sha256 = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(5)).unwrap();
        let binding = Binding { version: 1, workspace: cwd.clone(), team: "alpha".into(), scope: scope.clone(),
            endpoint, candidate: candidate.clone(), candidate_sha256, tmux: tmux.clone(), transport_root: transport_root.clone() };
        let binding_root = backend::root_for(root, &scope);
        std::fs::create_dir_all(&binding_root).unwrap();
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(binding_root.join("framework-binding.json")).unwrap()
            .write_all(&serde_json::to_vec(&binding).unwrap()).unwrap();
        let image = root.join("native-python");
        std::os::unix::fs::symlink(&python, &image).unwrap();
        let native_hash = fingerprint_file(&python, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
        let native = physical_support::native(native_hash);
        let mut descriptor = physical_support::descriptor("single", &native);
        descriptor.identity.id = provider;
        descriptor.session.fresh = FreshSession::NotApplicable;
        let script = root.join("native-receiver.py");
        let controlled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../team-agent-contract/tests/fixtures/fake_terminal.py").canonicalize().unwrap();
        let helper_file = transport_root.join("mcp-helper-pid");
        // Both processes are real, owned and bounded; no fabricated PID or readiness status.
        std::fs::write(&script, format!(
            "import pathlib, runpy, signal, subprocess\nsignal.alarm(110)\nhelper = subprocess.Popen(['/bin/sleep', '100'])\npathlib.Path({helper:?}).write_text(str(helper.pid))\ntry:\n    runpy.run_path({controlled:?}, run_name='__main__')\nfinally:\n    helper.terminate()\n    helper.wait()\n",
            helper=helper_file.to_string_lossy(), controlled=controlled.to_string_lossy())).unwrap();
        let planner = ReceiverPlan(script);
        let hooks = ProviderHooks { plan: HookBinding::Bound(&planner), ..physical_support::hooks() };
        let request = LaunchRequest { identity: identity.clone(), provider: provider.into(), native,
            operation: Operation::Fresh, mode: LaunchMode::FullWorker, auth: AuthMode::NativeSubscription,
            model: Some("single".into()), role_effort: None, team_effort: None, prompt: Some("controlled input budget".into()),
            bypass: false, paths: LaunchPaths { candidate, cwd, runtime_root: transport_root.clone(),
                executable: python, launcher: physical_support::public_launcher(&image, native_hash) },
            channel: Channel::Tmux, input_profile: Some("physical-fixture".into()), evidence_kind: EvidenceKind::Native,
            preassigned_session: None, resume: None, fork: None };
        let resolved = resolve_launch(&descriptor, &hooks, &request, None).unwrap();
        let adapter = Adapter { descriptor: &descriptor, hooks: &hooks, catalog: None };
        let plan = planner.plan(&resolved).unwrap();
        let operation = OperationId::new("direct-start").unwrap();
        let mut io = ScopedMaterializer::new(&descriptor, &resolved, &plan, operation.clone(), RealCommandRunner::default()).unwrap();
        let clock = RealClock::new();
        let mut physical = PhysicalRuntime::new(Registration { descriptor: &descriptor, hooks: &hooks, controls: &[],
            startup: None, session_panel: None }, PhysicalSettings { tmux: tmux.clone(), limits: limits(),
            action_budget: Duration::from_secs(10), freshness: Duration::from_secs(2), journal_bytes: 2 * 1024 * 1024 },
            operation.clone(), &clock).unwrap();
        physical.executing_candidate = Some(candidate_sha256);
        let started = Lifecycle { store: &mut store, host: &mut physical, io: &mut io }.startup(&adapter, &request,
            Routing { pane: "unpublished".into(), binding_key: "unpublished-direct".into(), server_key: "fixture-server".into() }, operation).unwrap();
        assert_eq!(started.outcome, Outcome::Committed, "{started:?}");
        let seat = store.assert_current(&identity).unwrap();
        let owned = Self { socket: seat.endpoint.clone(), pane: seat.pane.clone(),
            pid: seat.process.as_ref().unwrap().pid, tmux, target: seat.physical.clone(), _storage: storage };
        assert_eq!(seat.status, SeatStatus::Starting);
        assert!(!store.leased(&identity.seat).unwrap());
        assert!(store.unfinished().unwrap().is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !helper_file.exists() {
            assert!(Instant::now() < deadline, "native MCP descendant never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let helper_pid = std::fs::read_to_string(helper_file).unwrap().trim().parse().unwrap();
        let sleep = PathBuf::from("/bin/sleep").canonicalize().unwrap();
        let hash = fingerprint_file(&sleep, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
        let helper = capture_process(helper_pid, &sleep, hash, Duration::from_secs(5)).unwrap();
        let connection = InstanceId::new("direct-startup").unwrap();
        store.register_connection(&team_agent_contract::orchestration::protocol::ConnectionRecord {
            identity: identity.clone(), connection: connection.clone(), server_key: seat.server_key.clone(),
            binding_key: seat.binding_key.clone(), process: helper, native_process: seat.physical.as_ref().unwrap().process.clone(), closed: false,
        }).unwrap();
        let context = team_agent_contract::orchestration::mcp::CallContext { identity: identity.clone(),
            binding_key: seat.binding_key.clone(), connection_id: connection, task_id: "startup".into() };
        assert!(!store.startup_handshake_observed(&identity).unwrap());
        for (id, method) in [(1, "initialize"), (2, "tools/list")] {
            team_agent_contract::orchestration::mcp::response_written(&mut store, &context, &json!({"id":id,"method":method})).unwrap();
        }
        assert!(store.startup_handshake_observed(&identity).unwrap());
        owned
    }
}
