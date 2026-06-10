//! B5: `team-agent claude` leader terminal must survive workspace shutdown.
//!
//! Locate doc (sole basis): `.team/artifacts/b5-leader-terminal-kill-locate.md`
//! (fable-architect). Root cause: `team-agent claude` builds the leader terminal as a
//! `team-agent-leader-*` session on the framework's PER-WORKSPACE private tmux socket
//! (leader/start.rs:263 + tmux_backend.rs:192-199,308-319). Bare `shutdown` (no --team)
//! then ends with an unconditional `tmux kill-server` on that socket
//! (cli/mod.rs:178-180 -> tmux_backend.rs:299-305) — the whole server dies, leader
//! session included (V1, deterministic main culprit; any team's bare shutdown, even the
//! leader's own). The workspace residual process sweep (cli/mod.rs:614-638, 874-991)
//! additionally SIGTERMs any process whose command line contains the workspace path —
//! the leader's process qualifies and has no protection when another team runs the
//! shutdown (V2, accessory). Python 0.2.11 never had kill-server (default socket); the
//! kill-scope truth source is wrong: "everything on the socket" != "managed cleanable".
//!
//! Test shape per locate doc §4: REAL tmux on a throwaway workspace ⇒ socket name
//! `ta-<FNV(temp dir)>` is globally unique and physically isolated from every live
//! socket (default socket and any live team's `ta-*` socket untouched). Ordering is
//! fully deterministic — no races, no sleeps in the assertions (only a bounded
//! wait-for-process-visible poll after spawn). Teardown kills only the test socket's
//! server. The state.json fixtures mirror the real-machine shape
//! (session_name/active_team_key/agents/teams, pane_pid/spawn_cwd fields); the leader
//! session name comes from the REAL `leader_session_name()` generator, not a hand-typed
//! prefix.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::lifecycle_port;
use team_agent::model::enums::Provider;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

/// RED-V1 (main culprit, session level): bare shutdown must kill the team session but
/// MUST NOT kill the `team-agent-leader-*` session living on the same workspace socket.
/// Today `lifecycle_port::shutdown` unconditionally runs `kill_server()` when
/// `team.is_none()` (cli/mod.rs:178-180) and the leader session dies with the server.
#[test]
#[serial(b5_shutdown)]
fn red_v1_bare_shutdown_must_not_kill_leader_terminal_session() {
    let ws = tmp_ws("v1-killserver");
    let fixture = SocketFixture::spawn(&ws);
    write_team_state(&ws, fixture.worker_pid);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    let worker_alive = fixture
        .backend
        .has_session(&fixture.worker_session)
        .unwrap_or(false);
    let leader_alive = fixture
        .backend
        .has_session(&fixture.leader_session)
        .unwrap_or(false);
    assert!(
        !worker_alive,
        "sanity: bare shutdown must kill the team worker session {}",
        fixture.worker_session.as_str()
    );
    assert!(
        leader_alive,
        "V1: the leader terminal session `{}` lives on the workspace socket by design \
(leader/start.rs:263) and bare shutdown must spare it — `kill_server()` \
(cli/mod.rs:178-180) wipes the whole socket today, killing the leader terminal of \
ANY team in the workspace (and the invoker's own). The `team-agent-leader-` name \
prefix is the existing ownership truth source; the shutdown tail must spare it.",
        fixture.leader_session.as_str()
    );
}

/// RED-V2 (accessory, process level): the workspace residual sweep
/// (`reap_workspace_process_residuals` -> `matched_processes`, cli/mod.rs:874-991)
/// SIGTERMs every process whose command line contains the workspace path. The leader's
/// provider process runs with cwd=workspace and a workspace-path argv, and is only
/// protected when it happens to be the invoker's own ancestor — never when another team
/// runs the shutdown. The leader process here carries the workspace path in argv, so
/// the cmdline text match (:978) hits deterministically (no lsof-budget dice).
///
/// Note: today this case is red via BOTH culprits (the sweep kills the process, and
/// kill_server SIGHUPs the pane child anyway); it isolates F2 once F1 lands.
#[test]
#[serial(b5_shutdown)]
fn red_v2_workspace_residual_sweep_must_not_kill_leader_process() {
    let ws = tmp_ws("v2-sweep");
    let fixture = SocketFixture::spawn(&ws);
    write_team_state(&ws, fixture.worker_pid);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    assert!(
        pid_alive(fixture.leader_pid),
        "V2: the leader terminal's process (pid {}, argv contains the workspace path) \
must survive bare shutdown — the workspace residual sweep matches it by command line \
(cli/mod.rs:978) and the protection set (cli/mod.rs:742-766) only covers the invoker's \
own ancestry, so any other team's shutdown reaps the leader. The leader pane process \
tree must join the protected set (F2).",
        fixture.leader_pid
    );
}

/// GREEN regression lock: `--team` scoped shutdown already spares the leader — it kills
/// only the team session (no `kill_server`, cli/mod.rs:178 condition; reap scope
/// ScopedTeam skips workspace matching, cli/mod.rs:900). Lock the current behavior so
/// the F1/F2 fixes cannot regress the scoped tier.
#[test]
#[serial(b5_shutdown)]
fn green_scoped_shutdown_spares_leader_session_and_process() {
    let ws = tmp_ws("scoped-green");
    let fixture = SocketFixture::spawn(&ws);
    write_team_state(&ws, fixture.worker_pid);

    lifecycle_port::shutdown(&ws, false, Some("team-x"))
        .expect("scoped shutdown should succeed");

    let worker_alive = fixture
        .backend
        .has_session(&fixture.worker_session)
        .unwrap_or(false);
    let leader_alive = fixture
        .backend
        .has_session(&fixture.leader_session)
        .unwrap_or(false);
    assert!(
        !worker_alive,
        "sanity: scoped shutdown must kill the scoped team session {}",
        fixture.worker_session.as_str()
    );
    assert!(
        leader_alive && pid_alive(fixture.leader_pid),
        "GREEN lock: scoped shutdown must keep sparing the leader session `{}` and its \
process (pid {}) — no kill_server, no workspace-wide sweep in ScopedTeam scope.",
        fixture.leader_session.as_str(),
        fixture.leader_pid
    );
}

/// RED-V3 (third killer, server level — handed over by fable-developer): the workspace
/// residual sweep reaps the WHOLE process tree of any match, and the tmux SERVER
/// carrying the leader pane matches by command line (it was started with the first
/// spawn command, which contains the workspace path). Killing the server closes every
/// pane pty — SIGHUP kills the protected leader pane anyway, bypassing per-pid
/// protection (cli/mod.rs:577-590 reap_process_tree + :978 cmdline match). F1 (spare
/// kill_server) and F2 (protect leader pane tree) cannot stop this path; the server
/// PID itself must join the protection set when a `team-agent-leader-*` session lives
/// on the socket. RED verification: revert the server-protection loop in
/// `extend_protection_with_leader_panes` (commit c92b716) and this goes red.
#[test]
#[serial(b5_shutdown)]
fn red_v3_residual_sweep_must_not_reap_leaders_tmux_server() {
    let ws = tmp_ws("v3-server");
    let fixture = SocketFixture::spawn(&ws);
    write_team_state(&ws, fixture.worker_pid);
    let server_pid = parent_pid(fixture.leader_pid).expect("leader pane must have a tmux server parent");

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    let mut failures = Vec::new();
    if pid_alive(fixture.worker_pid) {
        failures.push(format!(
            "sanity: worker pane process {} must die in bare shutdown (server-only protection, \
NOT a tree protection)",
            fixture.worker_pid
        ));
    }
    if !pid_alive(server_pid) {
        failures.push(format!(
            "V3: the tmux server (pid {server_pid}) carrying the leader session must survive — \
reaping it SIGHUPs every pane and kills the leader terminal regardless of per-pid protection"
        ));
    }
    if !pid_alive(fixture.leader_pid) {
        failures.push(format!(
            "V3: the leader pane process (pid {}) must survive bare shutdown",
            fixture.leader_pid
        ));
    }
    assert!(
        failures.is_empty(),
        "B5 V3 server-protection contract failed:\n{}",
        failures.join("\n")
    );
}

/// GREEN boundary lock (MUST-17): a socket WITHOUT any `team-agent-leader-*` session
/// gets no server protection — bare shutdown still tears the whole worker server down
/// (the kill_server cleanup keeps preventing socket leaks).
#[test]
#[serial(b5_shutdown)]
fn green_pure_worker_socket_server_still_torn_down() {
    let ws = tmp_ws("v3-boundary");
    let backend = TmuxBackend::for_workspace(&ws);
    let worker_session = SessionName::new("team-x");
    let keepalive = ws.join("worker.keepalive");
    std::fs::write(&keepalive, "worker\n").unwrap();
    spawn_session(&backend, &worker_session, "w1", &keepalive, &ws);
    let worker_pid = wait_pid_for_cmdline(&keepalive);
    let server_pid = parent_pid(worker_pid).expect("worker pane must have a tmux server parent");
    write_team_state(&ws, worker_pid);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");
    backend.kill_server(); // teardown safety; should already be gone

    assert!(
        !pid_alive(worker_pid) && !pid_alive(server_pid),
        "MUST-17 boundary: with no leader session on the socket, bare shutdown must still \
tear down the worker server (worker_pid alive={}, server_pid alive={})",
        pid_alive(worker_pid),
        pid_alive(server_pid)
    );
}

fn parent_pid(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}

// ---------------------------------------------------------------------------
// fixture: isolated per-test workspace socket with a real leader + worker session
// ---------------------------------------------------------------------------

struct SocketFixture {
    backend: TmuxBackend,
    leader_session: SessionName,
    worker_session: SessionName,
    leader_pid: u32,
    worker_pid: u32,
}

impl SocketFixture {
    /// Start two REAL tmux sessions on the workspace's private socket:
    /// the leader session (real `leader_session_name()` naming) and the team worker
    /// session `team-x`. Both run `tail -f <ws>/<name>.keepalive` so their command
    /// lines deterministically contain the workspace path.
    fn spawn(ws: &Path) -> Self {
        let backend = TmuxBackend::for_workspace(ws);
        let leader_session = team_agent::leader::leader_session_name(Provider::Claude, ws);
        let worker_session = SessionName::new("team-x");
        let leader_keepalive = ws.join("leader.keepalive");
        let worker_keepalive = ws.join("worker.keepalive");
        std::fs::write(&leader_keepalive, "leader\n").unwrap();
        std::fs::write(&worker_keepalive, "worker\n").unwrap();

        spawn_session(&backend, &leader_session, "leader", &leader_keepalive, ws);
        spawn_session(&backend, &worker_session, "w1", &worker_keepalive, ws);
        let leader_pid = wait_pid_for_cmdline(&leader_keepalive);
        let worker_pid = wait_pid_for_cmdline(&worker_keepalive);
        assert!(
            backend.has_session(&leader_session).unwrap_or(false)
                && backend.has_session(&worker_session).unwrap_or(false),
            "fixture must start both sessions on the isolated socket"
        );
        Self {
            backend,
            leader_session,
            worker_session,
            leader_pid,
            worker_pid,
        }
    }
}

impl Drop for SocketFixture {
    fn drop(&mut self) {
        // Teardown of the TEST-OWNED isolated socket only (`ta-<FNV(temp ws)>`).
        // Never touches the default socket or any live team's socket.
        self.backend.kill_server();
    }
}

fn spawn_session(
    backend: &TmuxBackend,
    session: &SessionName,
    window: &str,
    keepalive: &Path,
    cwd: &Path,
) {
    let argv = vec![
        "tail".to_string(),
        "-f".to_string(),
        keepalive.to_string_lossy().to_string(),
    ];
    backend
        .spawn_first(session, &WindowName::new(window), &argv, cwd, &BTreeMap::new())
        .unwrap_or_else(|e| panic!("spawn {} failed: {e}", session.as_str()));
}

/// Bounded wait for the keepalive process to be visible in the process table
/// (tmux returns right after creating the pane; the pane shell exec is near-instant
/// but not synchronous). Not a race in the assertion path — purely fixture readiness.
fn wait_pid_for_cmdline(unique_path: &Path) -> u32 {
    let pattern = unique_path.to_string_lossy().to_string();
    for _ in 0..100 {
        let out = Command::new("pgrep")
            .args(["-n", "-f", &pattern])
            .output()
            .expect("run pgrep");
        if out.status.success() {
            if let Ok(pid) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
                return pid;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    panic!("keepalive process for {pattern} never appeared");
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// State fixture mirroring the real-machine `.team/runtime/state.json` shape
/// (top-level + teams projection; agent row carries window/pane_pid/spawn_cwd).
fn write_team_state(ws: &Path, worker_pid: u32) {
    let agent_row = json!({
        "status": "running",
        "provider": "codex",
        "agent_id": "w1",
        "window": "w1",
        "pane_pid": worker_pid,
        "spawn_cwd": ws.to_string_lossy(),
    });
    let state = json!({
        "session_name": "team-x",
        "active_team_key": "team-x",
        "agents": { "w1": agent_row },
        "teams": {
            "team-x": {
                "session_name": "team-x",
                "active_team_key": "team-x",
                "agents": { "w1": agent_row },
            },
        },
    });
    let path = team_agent::state::persist::runtime_state_path(ws);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-b5-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
