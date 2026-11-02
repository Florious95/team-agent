//! RED: a contract seat spawned into the SHARED workspace tmux server must carry
//! its seat id as the tmux window name. Clients (AgentMirror) drop a window name
//! equal to the foreground executable, and a pane title equal to basename(cwd).
//! They then fall back to the workspace directory name.
//! Real tmux on a private fixture socket, real TmuxHost::new_shared + spawn_owned,
//! controlled native image. The pane title is set to basename(cwd), the same way
//! Codex sets it through OSC.
#![cfg(any(target_os = "linux", target_os = "macos"))]
mod physical_support;
use physical_support::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::host::clock::{Clock, RealClock};
use team_agent_contract::host::command::*;
use team_agent_contract::host::files::ScopedDirectory;
use team_agent_contract::host::process::*;
use team_agent_contract::host::tmux::*;

#[test]
#[ignore = "internal owned TTY fixture; invoked only by codex_naming_red cases"]
fn native_terminal_fixture() {
    native_terminal::run();
}

fn tmux_binary() -> PathBuf {
    let value = std::env::var_os("CONTRACT_TMUX").expect("set CONTRACT_TMUX to an absolute tmux");
    PathBuf::from(value).canonicalize().unwrap()
}

/// Only ever addresses this test's own fixture socket; TMUX is removed so the
/// user's servers cannot be reached.
fn raw(tmux: &Path, endpoint: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(tmux)
        .arg("-u").arg("-S").arg(endpoint).args(args)
        .env_remove("TMUX").env_remove("TMUX_PANE")
        .output().unwrap()
}

struct OwnedFixtureServer { tmux: PathBuf, endpoint: PathBuf }
impl Drop for OwnedFixtureServer {
    fn drop(&mut self) {
        let own = self.endpoint.to_string_lossy();
        if own.starts_with("/tmp/tac-naming-") || own.starts_with("/private/tmp/tac-naming-") {
            let _ = raw(&self.tmux, &self.endpoint, &["kill-server"]);
        }
    }
}

fn case(label: &str, seat: &str, workspace: &str, new_session: bool) {
    let tmux = tmux_binary();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = PathBuf::from("/tmp").join(format!("tac-naming-{}-{}", std::process::id(), nonce % 1_000_000_000));
    let mut identity = owner();
    identity.seat = SeatId::new(seat).unwrap();
    identity.instance = InstanceId::new(format!("naming-{nonce}")).unwrap();
    let directory = ScopedDirectory::create(&root, identity.clone()).unwrap();
    let cwd_path = directory.path().join(workspace);
    std::fs::create_dir(&cwd_path).unwrap();
    let cwd = resolve_cwd(&cwd_path).unwrap();
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let (executable, native_hash) = native_terminal::copy_executable(directory.path());
    let launcher = public_launcher(&executable, native_hash);
    let n = native(native_hash);
    let d = descriptor("single", &n);
    let h = native_terminal::hooks();
    let request = LaunchRequest {
        provider: "physical-fixture".into(), operation: Operation::Fresh, mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription, model: Some("single".into()), role_effort: None, team_effort: None,
        bypass: false, prompt: Some("naming fixture".into()), identity, native: n,
        paths: LaunchPaths { launcher, executable, candidate, cwd, runtime_root: directory.path().to_path_buf() },
        channel: Channel::Tmux, input_profile: Some("physical-fixture".into()), evidence_kind: EvidenceKind::Fixture,
        preassigned_session: None, resume: None, fork: None,
    };
    let launch = resolve_launch(&d, &h, &request, None).unwrap();
    let plan = team_agent_contract::contract::hooks::PlanHook::plan(&native_terminal::PLAN, &launch).unwrap();
    let clock = RealClock::new();
    let limits = HostLimits {
        command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 2 * 1024 * 1024 },
        max_script_bytes: 128 * 1024, max_capture_bytes: 256 * 1024,
        max_payload_bytes: 1024 * 1024, max_executable_bytes: 512 * 1024 * 1024,
        poll_interval: Duration::from_millis(10), columns: 120, rows: 40,
    };

    // An independent peer owns the "workspace" server (stand-in for the leader's
    // team session). Its server keeps tmux defaults (automatic-rename on).
    let mut peer_request = launch.request().clone();
    peer_request.identity.seat = SeatId::new("workspace-peer").unwrap();
    peer_request.identity.instance = InstanceId::new(format!("anchor-{nonce}")).unwrap();
    let anchor_dir = ScopedDirectory::create(&directory.path().join("anchor"), peer_request.identity.clone()).unwrap();
    peer_request.paths.runtime_root = anchor_dir.path().to_owned();
    let peer_launch = resolve_launch(&d, &h, &peer_request, None).unwrap();
    let peer_plan = team_agent_contract::contract::hooks::PlanHook::plan(&native_terminal::PLAN, &peer_launch).unwrap();
    let mut peer = TmuxHost::new(anchor_dir, tmux.clone(), limits, RealCommandRunner::default()).unwrap();
    let anchor = peer.spawn_owned(&d, &peer_launch, &peer_plan, &MaterializeReceipt { resources: vec![] },
        &clock, clock.now() + Duration::from_secs(20));
    assert!(anchor.problem.is_none(), "anchor spawn: {anchor:?}");
    let anchor_session_name = anchor.pane.as_ref().unwrap().session_name.clone();
    let anchor = anchor.target.unwrap();
    let _server = OwnedFixtureServer { tmux: tmux.clone(), endpoint: anchor.endpoint.clone() };
    let global = raw(&tmux, &anchor.endpoint, &["show-options", "-gwv", "automatic-rename"]);
    assert_eq!(String::from_utf8_lossy(&global.stdout).trim(), "on", "fixture server must keep the tmux default");

    let shared = SharedTmux {
        endpoint: anchor.endpoint.clone(),
        socket: Some((anchor.socket_device, anchor.socket_inode)),
        session_name: if new_session { format!("team-naming-{}", nonce % 1_000_000) } else { anchor_session_name },
        session: if new_session { None } else { Some(anchor.session.clone()) },
    };
    let mut host = TmuxHost::new_shared(directory, tmux.clone(), limits, RealCommandRunner::default(), shared).unwrap();
    let spawn = host.spawn_owned(&d, &launch, &plan, &MaterializeReceipt { resources: vec![] },
        &clock, clock.now() + Duration::from_secs(20));
    assert!(spawn.problem.is_none(), "shared spawn: {spawn:?}");
    let target = spawn.target.unwrap();
    assert!(target.shared_endpoint);

    // Codex writes OSC 0/2 = basename(cwd); tmux stores that as pane_title.
    assert!(raw(&tmux, &target.endpoint, &["select-pane", "-t", &target.pane, "-T", workspace]).status.success());
    // Let tmux's automatic-rename timer run more than once.
    std::thread::sleep(Duration::from_millis(2500));
    let out = raw(&tmux, &target.endpoint, &["display-message", "-p", "-t", &target.pane,
        "#{window_name}\u{1f}#{pane_title}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}\u{1f}#{automatic-rename}"]);
    assert!(out.status.success());
    let line = String::from_utf8(out.stdout).unwrap();
    let f: Vec<&str> = line.trim_end_matches('\n').split('\u{1f}').collect();
    assert_eq!(f.len(), 5, "{line:?}");
    let observation = serde_json::json!({
        "case": label, "seat": seat, "workspace": workspace, "new_session": new_session,
        "spawn_window_name": spawn.pane.as_ref().map(|p| p.window_name.clone()),
        "window_name": f[0], "pane_title": f[1], "pane_current_path": f[2],
        "pane_current_command": f[3], "automatic_rename": f[4],
    });
    println!("NAMING_OBSERVATION={observation}");
    assert_eq!(f[0], seat,
        "contract seat window must be named after the seat id, got window_name={:?} (pane_title={:?}, cmd={:?}, automatic-rename={})",
        f[0], f[1], f[3], f[4]);
}

#[test]
fn shared_new_window_seat_at_workspace_root_is_named_after_seat() {
    case("existing-session/new-window/workspace-root-cwd", "topic_scout", "my-workspace", false);
}

#[test]
fn shared_new_session_first_window_is_named_after_seat() {
    case("new-team-session/first-window/workspace-root-cwd", "topic_scout", "my-workspace", true);
}

#[test]
fn shared_new_window_cjk_workspace_root_is_named_after_seat() {
    case("existing-session/new-window/cjk-workspace", "douyin_publisher", "opus做视频", false);
}
