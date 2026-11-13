//! RED (Phase 1): native runtime operations after the host environment drifted.
//!
//! ISSUE-1: `framework-binding.json` pins `tmux` as a canonical install path
//! (e.g. `/opt/homebrew/Cellar/tmux/3.7c/bin/tmux`). After `brew upgrade tmux`
//! that path is gone, although `tmux` is still on PATH. Native stop and
//! purged-authority recovery must use the live `tmux` resolved from PATH,
//! not the stale pinned path.
//! ISSUE-9: during `team restart --allow-fresh`, a retained dead pane
//! (`remain-on-exit`, `pane_dead=1`) with the saved pane id is not a live
//! carrier. A live pane with the saved id still forbids recovery.
//!
//! Every `issue*_r*` test has a `*_g*` control that differs only in the defect
//! (a valid pinned tmux, or a live pane) and must stay green.
use super::{
    backend::{self, Binding},
    config::RoleConfig,
    discovery,
    prompt::Prompt,
    recovery,
};
use serde_json::{json, Value};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;
use team_agent_contract::contract::session::{CaptureOrigin, CwdIdentity, NativeSessionId, ResumeBinding, SessionStorage};
use team_agent_contract::contract::types::*;
use team_agent_contract::host::{
    digest, digest_hex,
    files::ScopedDirectory,
    process::{capture_process, fingerprint_file, resolve_cwd, sample_process, ProcessState},
    transport::TargetReceipt,
};
use team_agent_contract::orchestration::store::*;

const TEAM: &str = "drift";
const SEAT: &str = "worker";

struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl RestoreEnv {
    fn capture() -> Self {
        Self(["TMPDIR", "HOME", "PATH"].into_iter().map(|key| (key, std::env::var_os(key))).collect())
    }
}
impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The real `tmux` on this process's PATH (the test environment requires one).
fn live_tmux() -> PathBuf {
    discovery::executable("tmux").expect("tmux is required on PATH")
}

/// A tmux path recorded by an earlier install that no longer exists.
fn stale_tmux(root: &Path) -> PathBuf {
    let stale = root.join("Cellar/tmux/3.7c/bin/tmux");
    assert!(!stale.exists());
    stale
}

struct Workspace {
    root: PathBuf,
    cwd: CwdIdentity,
    binding: Binding,
    team_dir: PathBuf,
}

impl Workspace {
    /// A workspace with a durable binding. `lost` selects a purged legacy temp
    /// authority (`/tmp/ta-<24 hex>`); otherwise a live store in the workspace.
    fn new(label: &str, lost: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = Path::new("/tmp").canonicalize().unwrap();
        let root = tmp.join(format!("ta-p1-{label}-{}-{serial}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let cwd = resolve_cwd(&root).unwrap();
        let scope = backend::scope_for(&cwd, TEAM).unwrap();
        let durable = backend::root_for(&cwd.path, &scope);
        std::fs::create_dir_all(&durable).unwrap();
        let transport_root = if lost {
            let name = digest_hex(digest(format!("{}-{label}-{serial}", std::process::id()).as_bytes()));
            let purged = tmp.join(format!("ta-{}", &name[..24]));
            assert!(!purged.exists());
            purged
        } else {
            root.join("native-store")
        };
        let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
        let binding = Binding {
            version: 1,
            workspace: cwd.clone(),
            team: TEAM.into(),
            scope: scope.clone(),
            endpoint: format!("framework:{}", scope.as_str()),
            candidate_sha256: fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10)).unwrap(),
            candidate,
            tmux: live_tmux(),
            transport_root,
        };
        if !lost {
            drop(ProjectionStore::create(&binding.transport_root, scope, &binding.endpoint).unwrap());
        }
        let team_dir = root.join(".team").join(TEAM);
        std::fs::create_dir_all(team_dir.join("agents")).unwrap();
        std::fs::write(team_dir.join("TEAM.md"), format!("---\nname: {TEAM}\n---\nFixture team.\n")).unwrap();
        let workspace = Self { root, cwd, binding, team_dir };
        workspace.save_binding();
        workspace
    }

    fn save_binding(&self) {
        std::fs::write(
            backend::root_for(&self.cwd.path, &self.binding.scope).join("framework-binding.json"),
            serde_json::to_vec(&self.binding).unwrap(),
        )
        .unwrap();
    }

    fn identity(&self, instance: &str) -> InstanceIdentity {
        InstanceIdentity {
            scope: self.binding.scope.clone(),
            seat: SeatId::new(SEAT).unwrap(),
            instance: InstanceId::new(instance).unwrap(),
            generation: Generation(1),
        }
    }

    fn role(&self, provider: &str) -> RoleConfig {
        let body = "bounded fixture".to_string();
        RoleConfig {
            id: SeatId::new(SEAT).unwrap(),
            provider: ProviderId::new(provider).unwrap(),
            source: self.team_dir.join("agents").join(format!("{SEAT}.md")),
            role: SEAT.into(),
            model: "first-model".into(),
            effort: None,
            bypass: false,
            paused: false,
            body: body.clone(),
            prompt: Prompt { text: body.clone(), sha256: digest(body.as_bytes()), sources: vec![] },
        }
    }

    /// Persist the runtime state file (active team record + `teams` map).
    fn state(&self, agents: Value) {
        let team = json!({"team_key":TEAM,"active_team_key":TEAM,"team_dir":self.team_dir,
            "workspace":self.root,"session_name":format!("team-p1-{TEAM}"),"agents":agents});
        let mut file = team.clone();
        file["teams"] = json!({ TEAM: team.clone() });
        let runtime = self.root.join(".team").join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("state.json"), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    /// An explicitly stopped contract seat that recovery skips, whose saved
    /// carrier is `pane` on `endpoint`, plus its frozen role and role document.
    fn recovery_seat(&self, endpoint: &Path, pane: &str) {
        let identity = self.identity("instance-before-purge");
        let role = self.role("codex_tui");
        std::fs::write(&role.source, "---\nname: worker\nprovider: codex\nmodel: first-model\n---\nWorker.\n").unwrap();
        backend::write_role(&self.binding, &identity, &role).unwrap();
        self.state(json!({ SEAT: {
            "provider":"codex_tui","contract_provider":"codex_tui","runtime_family":"contract",
            "status":"stopped","paused":false,"explicit_stop":true,
            "contract_scope":self.binding.scope.as_str(),"contract_instance":"instance-before-purge","generation":1,
            "native_endpoint":endpoint,"native_pane":pane,"model":"first-model",
            "owner_team_id":TEAM,"team_key":TEAM}}));
    }

    fn recover_dry_run(&self) -> Result<Value, String> {
        match recovery::restart(&self.cwd.path, Some(TEAM), true, true, true) {
            Ok(Some(result)) => match result.output {
                crate::cli::types::CmdOutput::Json(value) => Ok(value),
                other => Err(format!("non-JSON recovery output: {other:?}")),
            },
            Ok(None) => Err("recovery did not own the request (authority not classified as lost)".into()),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------- ISSUE-1: native stop

/// Recording `tmux`: one live shared server whose saved pane is gone.
fn recording_tmux(path: &Path) {
    std::fs::write(
        path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/tmux-calls\"\ncase \"$*\" in\n *list-sessions*) exit 0;;\n *display-message*) printf '\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\n'; exit 0;;\n *) exit 92;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A committed native seat whose process exited while its shared tmux server
/// (same socket identity) still runs; then stop it through the backend.
fn stop_after_native_exit(label: &str, pin_stale: bool) {
    let _env = RestoreEnv::capture();
    std::env::set_var("TMPDIR", "/tmp");
    let mut workspace = Workspace::new(label, false);
    let wrappers = workspace.root.join("wrappers");
    let engines = workspace.root.join("engines");
    let home = workspace.root.join("home");
    for path in [&wrappers, &engines, &home] {
        std::fs::create_dir(path).unwrap();
    }
    let codex = wrappers.join("codex");
    std::fs::write(&codex, "#!/bin/sh\ncase \"$*\" in\n --version) echo 'codex-cli 0.167.2';;\n 'debug models') echo '{\"models\":[{\"slug\":\"first-model\",\"visibility\":\"list\",\"supported_reasoning_levels\":[]}]}' ;;\n *) exit 99;;\nesac\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tmux = wrappers.join("tmux");
    recording_tmux(&tmux);
    let executable = PathBuf::from("/bin/sleep").canonicalize().unwrap();
    std::os::unix::fs::symlink(&executable, engines.join("codex")).unwrap();
    std::env::set_var("PATH", format!("{}:{}:{}", wrappers.display(), engines.display(), std::env::var("PATH").unwrap()));
    std::env::set_var("HOME", &home);
    let provider = ProviderId::new("codex_tui").unwrap();
    let discovered = discovery::discover_runtime(&provider, &workspace.root).unwrap();
    workspace.binding.tmux = if pin_stale { stale_tmux(&workspace.root) } else { tmux.clone() };
    workspace.save_binding();

    let mut child = ChildGuard(std::process::Command::new(&executable).arg("110").current_dir(&workspace.root).spawn().unwrap());
    let process = capture_process(child.0.id(), &executable, discovered.native.executable_sha256, Duration::from_secs(5)).unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(sample_process(&process), ProcessState::Exited);

    let socket = workspace.root.join("s");
    let listener = UnixListener::bind(&socket).unwrap();
    let identity = std::fs::symlink_metadata(&socket).unwrap();
    let seat_identity = workspace.identity("first");
    let directory = ScopedDirectory::create(&workspace.root.join("native"), seat_identity.clone()).unwrap();
    let owner = directory.receipt();
    let signature = digest_hex(digest(format!("{}:{}:{}:{}:{}:{}", owner.owner.scope.as_str(), owner.owner.seat.as_str(),
        owner.owner.instance.as_str(), owner.owner.generation.0, owner.device, owner.inode).as_bytes()));
    let binding_key = digest_hex(digest(format!("shared-v1:{signature}:{}:{}:false:{}", identity.dev(), identity.ino(), socket.display()).as_bytes()));
    let session = ResumeBinding {
        provider: provider.clone(),
        native_session: NativeSessionId::new("01a13e52-77d5-7580-81cc-2f5d4749aba1").unwrap(),
        storage: SessionStorage::Local,
        cwd: workspace.cwd.clone(),
        native: discovered.native.clone(),
        source: seat_identity.clone(),
        origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Native,
        evidence_sha256: digest(b"p1-native-session"),
        backing: None,
    };
    let seat = SeatRecord {
        identity: seat_identity.clone(),
        provider: provider.clone(),
        native: discovered.native.clone(),
        cwd: workspace.cwd.clone(),
        endpoint: socket.to_string_lossy().into_owned(),
        pane: "%1".into(),
        binding_key: binding_key.clone(),
        profile: "fixture".into(),
        server_key: "fixture".into(),
        evidence_kind: EvidenceKind::Native,
        status: SeatStatus::Ready,
        process: Some(process.identity.clone()),
        physical: Some(TargetReceipt {
            directory: owner.clone(), endpoint: socket.clone(), socket_device: identity.dev(), socket_inode: identity.ino(),
            shared_endpoint: true, created_session: false, session: "$1".into(), window: "@1".into(), pane: "%1".into(),
            columns: 120, rows: 40, binding: binding_key, process, provider: provider.clone(), cwd: workspace.cwd.clone(),
            native: discovered.native.clone(), evidence_kind: EvidenceKind::Native,
            candidate_sha256: workspace.binding.candidate_sha256, native_session: Some(session.native_session.clone()),
        }),
        session: Some(session),
        resources: vec![],
        workspace_resources: vec![],
        bootstrap_used: false,
    };
    let operation = OperationRecord {
        id: OperationId::new("controlled-start").unwrap(),
        kind: TransactionKind::Startup,
        target: seat.clone(),
        parent: None,
        phase: Phase::F5Receipt,
        pending: None,
        spawn_observation: None,
        native_launch: None,
        resources: vec![],
        effect: team_agent_contract::contract::delivery::DeliveryEffect::NoEffect,
        outcome: Outcome::Committed,
        preserved: vec![],
        failure: None,
        recovery_failure: None,
        stop_receipt: None,
    };
    let mut store = ProjectionStore::open(&workspace.binding.transport_root, workspace.binding.scope.clone(), &workspace.binding.endpoint).unwrap();
    store.fixture_put_seat(&seat).unwrap();
    store.fixture_put_operation(&operation).unwrap();
    drop(store);
    backend::write_role(&workspace.binding, &seat_identity, &workspace.role("codex_tui")).unwrap();

    let stopped = backend::stop(&workspace.cwd.path, TEAM, &seat_identity.seat);
    let calls = std::fs::read_to_string(wrappers.join("tmux-calls")).unwrap_or_default();
    let pinned = if pin_stale { "a stale pinned tmux path while tmux is on PATH" } else { "a valid pinned tmux path" };
    let stopped = stopped.unwrap_or_else(|error| panic!("{label}: native stop with {pinned} failed: {error}\ntmux calls:\n{calls}"));
    assert_eq!(stopped.outcome, Outcome::Committed, "{label}: native stop with {pinned} must commit: {stopped:?}\ntmux calls:\n{calls}");
    assert_eq!(stopped.target.status, SeatStatus::Stopped, "{label}: {stopped:?}");
    assert_eq!(stopped.stop_receipt.as_ref().map(|receipt| receipt.method.as_str()), Some("receipt_close_absent_shared_pane"), "{label}: {stopped:?}");
    assert!(calls.contains("list-sessions") && calls.contains("display-message"), "{label}: the live tmux must observe the shared server\n{calls}");
    drop(listener);
}

#[test]
#[serial_test::serial(env)]
fn issue1_r1_native_stop_uses_live_tmux_when_the_pinned_path_is_gone() {
    stop_after_native_exit("i1r1", true);
}

#[test]
#[serial_test::serial(env)]
fn issue1_g1_control_native_stop_with_a_valid_pinned_tmux() {
    stop_after_native_exit("i1g1", false);
}

// ---------------------------------------------------------------- ISSUE-1: purged-authority recovery

/// A saved native carrier endpoint that is a stale socket (no server): proving
/// the carrier dead requires running tmux against it.
fn recover_with_stale_carrier_socket(label: &str, pin_stale: bool) {
    let mut workspace = Workspace::new(label, true);
    if pin_stale {
        workspace.binding.tmux = stale_tmux(&workspace.root);
        workspace.save_binding();
    }
    let socket = workspace.root.join("carrier.sock");
    drop(UnixListener::bind(&socket).unwrap());
    assert!(std::fs::symlink_metadata(&socket).unwrap().file_type().is_socket());
    workspace.recovery_seat(&socket, "%7");
    let pinned = if pin_stale { "a stale pinned tmux path while tmux is on PATH" } else { "a valid pinned tmux path" };
    let plan = workspace.recover_dry_run().unwrap_or_else(|error| panic!("{label}: --allow-fresh recovery with {pinned} was refused: {error}"));
    assert_eq!(plan["ok"], true, "{label}: {plan}");
    assert_eq!(plan["status"], "dry_run", "{label}: {plan}");
}

#[test]
#[serial_test::serial(env)]
fn issue1_r2_allow_fresh_recovery_uses_live_tmux_when_the_pinned_path_is_gone() {
    recover_with_stale_carrier_socket("i1r2", true);
}

#[test]
#[serial_test::serial(env)]
fn issue1_g2_control_allow_fresh_recovery_with_a_valid_pinned_tmux() {
    recover_with_stale_carrier_socket("i1g2", false);
}

// ---------------------------------------------------------------- ISSUE-9: retained dead panes

/// A test-owned tmux server at `socket` running one pane; killed on drop.
struct CarrierServer {
    tmux: PathBuf,
    socket: PathBuf,
}

impl CarrierServer {
    fn tmux(&self, args: &[&str]) -> String {
        let out = std::process::Command::new(&self.tmux).arg("-S").arg(&self.socket).args(args).output().unwrap();
        assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Starts `sleep` in a pane with `remain-on-exit`; returns (pane id, pane pid).
    fn start(socket: PathBuf) -> (Self, String, u32) {
        let server = Self { tmux: live_tmux(), socket };
        server.tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "carrier", "-x", "80", "-y", "24", "sleep 600"]);
        server.tmux(&["set-option", "-w", "-t", "carrier", "remain-on-exit", "on"]);
        let fields = server.tmux(&["display-message", "-p", "-t", "carrier", "#{pane_id} #{pane_pid}"]);
        let (pane, pid) = fields.split_once(' ').unwrap();
        (server, pane.to_string(), pid.parse().unwrap())
    }

    fn pane_dead(&self, pane: &str) -> String {
        self.tmux(&["display-message", "-p", "-t", pane, "#{pane_dead}"])
    }
}

impl Drop for CarrierServer {
    fn drop(&mut self) {
        let _ = std::process::Command::new(&self.tmux).arg("-S").arg(&self.socket).arg("kill-server").output();
    }
}

#[test]
#[serial_test::serial(env)]
fn issue9_r1_retained_dead_pane_does_not_block_allow_fresh_recovery() {
    let workspace = Workspace::new("i9r1", true);
    let (server, pane, pid) = CarrierServer::start(workspace.root.join("carrier.sock"));
    assert!(std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.pane_dead(&pane) != "1" {
        assert!(std::time::Instant::now() < deadline, "fixture: pane {pane} never became dead");
        std::thread::sleep(Duration::from_millis(50));
    }
    workspace.recovery_seat(&server.socket, &pane);
    let plan = workspace
        .recover_dry_run()
        .unwrap_or_else(|error| panic!("a retained dead pane ({pane}, pane_dead=1) is not a live carrier; recovery was refused: {error}"));
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(plan["status"], "dry_run", "{plan}");
    assert_eq!(server.pane_dead(&pane), "1", "recovery planning must not touch the retained pane");
}

#[test]
#[serial_test::serial(env)]
fn issue9_g1_live_pane_with_the_saved_id_still_blocks_recovery() {
    let workspace = Workspace::new("i9g1", true);
    let (server, pane, _pid) = CarrierServer::start(workspace.root.join("carrier.sock"));
    workspace.recovery_seat(&server.socket, &pane);
    let refused = workspace.recover_dry_run();
    assert!(refused.is_err(), "a live pane with the saved carrier id must forbid recovery: {refused:?}");
    assert_eq!(server.pane_dead(&pane), "0", "recovery planning must not touch the live pane");
}
