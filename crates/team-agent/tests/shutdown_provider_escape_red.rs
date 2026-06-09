//! #248 RED: true provider shutdown residuals cannot depend only on fake pane pids.
//!
//! Architect fixture: true Codex shutdown can leave node/Codex/MCP processes after
//! tmux teardown, while JSON reports `ok=true`, `session_killed=true`, and empty
//! residuals. These command-level contracts model the missing surfaces:
//! real tmux pane pid discovery, workspace MCP process scan, provider pgid/cwd
//! scan, and post-reap session verification.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::state::persist::save_runtime_state;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn shutdown_real_tmux_list_targets_populates_pane_pid() {
    let case = RealTmuxCase::new("pane-pid");
    let session = SessionName::new("team-t248-pane-pid");
    case.backend
        .spawn_first(
            &session,
            &WindowName::new("w1"),
            &["/bin/sh".into(), "-lc".into(), "while true; do sleep 60; done".into()],
            &case.workspace,
            &BTreeMap::new(),
        )
        .expect("spawn real tmux pane");

    let panes = case.backend.list_targets().expect("list real tmux panes");
    let pane = panes
        .iter()
        .find(|pane| pane.session.as_str() == session.as_str())
        .unwrap_or_else(|| panic!("spawned pane must appear in list_targets; panes={panes:?}"));

    assert!(
        pane.pane_pid.is_some_and(|pid| pid > 0),
        "real tmux list_targets must populate PaneInfo.pane_pid so shutdown can reap provider \
         process trees; pane={pane:?}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn shutdown_reaps_workspace_mcp_process_not_descended_from_pane_pid() {
    let workspace = tmp_dir("mcp-escape");
    let _cleanup = WorkspaceCleanup(workspace.clone());
    seed_state(&workspace, "team-t248-mcp-escape");
    let mut mcp = ManagedChild::spawn_mcp_server(&workspace);
    let pid = mcp.pid();

    let out = run_shutdown(&workspace);
    let report = stdout_json(&out);
    let alive = mcp.is_alive();

    assert!(
        !alive || (!json_ok(&report) && residuals_contain_pid(&report, pid)),
        "shutdown must kill a workspace MCP server outside the pane ppid tree, or honestly report \
         it in residuals.processes. pid={pid} alive={alive} stdout={report} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn shutdown_reaps_provider_process_by_pgid_or_workspace_when_ppid_escaped() {
    let workspace = tmp_dir("provider-pgid-escape");
    let _cleanup = WorkspaceCleanup(workspace.clone());
    seed_state(&workspace, "team-t248-provider-escape");
    let mut provider = ManagedChild::spawn_provider_with_escaped_pgid(&workspace);
    let pid = provider.pid();

    let out = run_shutdown(&workspace);
    let report = stdout_json(&out);
    let alive = provider.is_alive();

    assert!(
        !alive || (!json_ok(&report) && residuals_contain_pid(&report, pid)),
        "shutdown must not report ok=true while a provider-like process under the workspace remains \
         outside the pane ppid tree. pid={pid} alive={alive} stdout={report} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn shutdown_session_killed_requires_post_reap_has_session_false() {
    let case = DefaultSocketSessionCase::new("session-still-live");
    let workspace = tmp_dir("session-still-live");
    let _cleanup = WorkspaceCleanup(workspace.clone());
    let session = SessionName::new(format!("team-t248-session-still-live-{}", std::process::id()));
    case.spawn(&session, &workspace);
    seed_state(&workspace, session.as_str());

    let out = run_shutdown(&workspace);
    let report = stdout_json(&out);
    let live_after_shutdown = case.has_session(&session);

    assert!(
        !live_after_shutdown
            || (report.get("session_killed").and_then(Value::as_bool) == Some(false)
                && report.get("ok").and_then(Value::as_bool) == Some(false)
                && residual_sessions_contain(&report, session.as_str())),
        "session_killed=true is only valid after the real post-reap endpoint reports has_session=false; \
         live_after_shutdown={live_after_shutdown} report={report} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct RealTmuxCase {
    workspace: PathBuf,
    backend: TmuxBackend,
}

impl RealTmuxCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let backend = TmuxBackend::for_workspace(&workspace);
        backend.kill_server();
        Self { workspace, backend }
    }
}

impl Drop for RealTmuxCase {
    fn drop(&mut self) {
        self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct DefaultSocketSessionCase {
    backend: TmuxBackend,
}

impl DefaultSocketSessionCase {
    fn new(_tag: &str) -> Self {
        Self { backend: TmuxBackend::new() }
    }

    fn spawn(&self, session: &SessionName, workspace: &Path) {
        self.backend
            .spawn_first(
                session,
                &WindowName::new("w1"),
                &["/bin/sh".into(), "-lc".into(), "while true; do sleep 60; done".into()],
                workspace,
                &BTreeMap::new(),
            )
            .expect("spawn default-socket session");
    }

    fn has_session(&self, session: &SessionName) -> bool {
        self.backend.has_session(session).unwrap_or(false)
    }
}

impl Drop for DefaultSocketSessionCase {
    fn drop(&mut self) {
        // Do not kill the user's default tmux server; only our unique test session.
        let _ = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .ok()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|name| name.starts_with("team-t248-session-still-live-"))
                    .for_each(|name| {
                        let _ = Command::new("tmux").args(["kill-session", "-t", name]).status();
                    });
            });
    }
}

struct WorkspaceCleanup(PathBuf);

impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ManagedChild {
    child: Child,
}

impl ManagedChild {
    fn spawn_mcp_server(workspace: &Path) -> Self {
        let child = Command::new(bin())
            .args(["mcp-server", "--workspace", workspace.to_str().unwrap()])
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn dummy workspace MCP server process");
        wait_until_live(child.id());
        Self { child }
    }

    fn spawn_provider_with_escaped_pgid(workspace: &Path) -> Self {
        let script = workspace.join("codex");
        std::fs::write(
            &script,
            "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 60; done\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&script, perms).unwrap();

        let mut command = Command::new(&script);
        command.current_dir(workspace).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        let child = command.spawn().expect("spawn provider-like escaped pgid process");
        wait_until_live(child.id());
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => pid_is_alive(self.pid()),
            Err(_) => pid_is_alive(self.pid()),
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.is_alive() {
            kill_pid(self.pid(), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.is_alive() {
            kill_pid(self.pid(), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn seed_state(workspace: &Path, session_name: &str) {
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace)).unwrap();
    save_runtime_state(
        workspace,
        &json!({
            "session_name": session_name,
            "agents": {
                "w1": {
                    "agent_id": "w1",
                    "status": "running",
                    "provider": "codex",
                    "window": "w1",
                    "pane_id": "%1",
                    "spawn_cwd": workspace
                }
            },
            "tasks": []
        }),
    )
    .unwrap();
}

fn run_shutdown(workspace: &Path) -> Output {
    Command::new(bin())
        .args([
            "shutdown",
            "--workspace",
            workspace.to_str().unwrap(),
            "--keep-logs",
            "--json",
        ])
        .current_dir(workspace)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run shutdown")
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout must be JSON; code={:?} stdout={} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn json_ok(report: &Value) -> bool {
    report.get("ok").and_then(Value::as_bool) == Some(true)
}

fn residuals_contain_pid(report: &Value, pid: u32) -> bool {
    let pid_number = u64::from(pid);
    report
        .pointer("/residuals/processes")
        .and_then(Value::as_array)
        .is_some_and(|processes| {
            processes.iter().any(|entry| match entry {
                Value::Number(number) => number.as_u64() == Some(pid_number),
                Value::Object(object) => object
                    .get("pid")
                    .and_then(Value::as_u64)
                    .is_some_and(|value| value == pid_number),
                _ => false,
            })
        })
}

fn residual_sessions_contain(report: &Value, session: &str) -> bool {
    report
        .pointer("/residuals/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| sessions.iter().any(|entry| entry.as_str() == Some(session)))
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-shutdown-provider-escape-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn wait_until_live(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if pid_is_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("process {pid} did not become live");
}

fn pid_is_alive(pid: u32) -> bool {
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let rc = unsafe { libc::kill(pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

fn kill_pid(pid: u32, signal: libc::c_int) {
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return;
    };
    unsafe {
        libc::kill(pid_t, signal);
    }
}
