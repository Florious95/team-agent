//! BUG-8 RED: shutdown must reap discovered runtime processes even when state.json is missing.
//!
//! User-facing contract: deleting `.team/runtime/state.json` must not turn shutdown into a
//! cosmetic no-op. A live team session and coordinator process discovered from the machine must be
//! gone after `team-agent shutdown --json`, without requiring provider subscriptions.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::coordinator::{coordinator_meta_path, coordinator_pid_path, WorkspacePath};
use team_agent::state::persist::{runtime_state_path, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn shutdown_reaps_discovered_processes_when_state_file_is_missing() {
    let case = MissingStateCase::new("shutdown-missing-state");
    let session = SessionName::new("team-bug8-missing-state");
    case.spawn_worker_session(&session);
    let mut coordinator = case.spawn_coordinator();
    let coordinator_pid = coordinator.id();
    case.wait_for_coordinator_pid_file();

    std::fs::remove_file(runtime_state_path(&case.workspace)).unwrap();
    let wp = WorkspacePath::new(case.workspace.clone());
    std::fs::remove_file(coordinator_pid_path(&wp)).unwrap();
    std::fs::remove_file(coordinator_meta_path(&wp)).unwrap();

    let out = run(
        &[
            "shutdown",
            "--workspace",
            case.workspace.to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
        &case.workspace,
    );
    let value = stdout_json(&out);
    let session_still_alive = case.backend.has_session(&session).unwrap_or(false);
    let coordinator_still_alive = !wait_for_child_exit(&mut coordinator, Duration::from_secs(2));

    cleanup_child(&mut coordinator);
    drop(case);

    assert!(
        out.status.success() && value["ok"] == json!(true),
        "BUG-8 precondition: shutdown should return a shaped success envelope; code={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !session_still_alive && !coordinator_still_alive,
        "BUG-8 contract: when state.json/coordinator pid markers are missing, shutdown must still \
         discover and reap the live team session and coordinator process. \
         session_still_alive={session_still_alive} coordinator_pid={coordinator_pid} \
         coordinator_still_alive={coordinator_still_alive} shutdown={value}"
    );
}

struct MissingStateCase {
    workspace: PathBuf,
    backend: TmuxBackend,
}

impl MissingStateCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&workspace)).unwrap();
        let backend = TmuxBackend::for_workspace(&workspace);
        backend.kill_server();
        save_runtime_state(
            &workspace,
            &json!({
                "session_name": "team-bug8-missing-state",
                "agents": {
                    "worker": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker"
                    }
                },
                "tasks": []
            }),
        )
        .unwrap();
        Self { workspace, backend }
    }

    fn spawn_worker_session(&self, session: &SessionName) {
        self.backend
            .spawn_first(
                session,
                &WindowName::new("worker"),
                &[
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "while true; do sleep 60; done".to_string(),
                ],
                &self.workspace,
                &BTreeMap::new(),
            )
            .expect("spawn disposable tmux worker session");
    }

    fn spawn_coordinator(&self) -> Child {
        Command::new(bin())
            .args([
                "coordinator",
                "--workspace",
                self.workspace.to_str().unwrap(),
                "--tick-interval",
                "60",
            ])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn disposable coordinator")
    }

    fn wait_for_coordinator_pid_file(&self) {
        let pid_path = coordinator_pid_path(&WorkspacePath::new(self.workspace.clone()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if pid_path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("coordinator did not write pid file at {}", pid_path.display());
    }
}

impl Drop for MissingStateCase {
    fn drop(&mut self) {
        self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
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

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-bug8-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn pid_is_alive(pid: u32) -> bool {
    // 0.5.x Windows portability Batch 5: route through
    // `platform::process::pid_is_alive` for cross-platform compile.
    team_agent::platform::process::pid_is_alive(pid)
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return !pid_is_alive(child.id()),
        }
    }
    false
}

fn cleanup_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}
