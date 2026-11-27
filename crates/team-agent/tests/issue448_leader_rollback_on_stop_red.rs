//! Issue #448 RED: a managed leader launcher must not fail its exit-time
//! registry rollback after a clean `team-agent stop` already removed the
//! registry entry.
//!
//! Field evidence: `team-agent claude` -> `team-agent stop --json` (ok) ->
//! provider `/exit` (0) -> launcher exits 1 with
//! `leader start error: leader registry rollback failed: registry receipt read failed`
//! and writes a `cli-error-*.log`.
//!
//! Contract (see `.team/artifacts/real-incident-cards/issue-448-rollback-on-stop/CONTRACT.md`):
//! - C1 A receipt whose registry file is absent (`NotFound`) is a benign rollback:
//!   `restore_entry_if_current_matches` returns `Ok`, the entry stays absent, and a
//!   recorded preimage is never resurrected after a canonical unbind.
//! - C2 The end-to-end managed launch -> in-pane `team-agent stop` -> provider exit 0
//!   journey exits 0, prints no rollback error, writes no `cli-error-*.log`, and
//!   leaves no registry entry behind.
//! - C3 Every other rollback failure stays fail-closed: an unreadable entry
//!   (directory, permission denied) is still `ReadFailed` and untouched; a receipt
//!   outside the registry directory is still `InvalidReceipt` even when its file is
//!   absent; foreign bytes are still `Superseded` and untouched; a live own receipt
//!   is still `Restored`.
//!
//! Hermetic: HOME, the registry directory, TMUX_TMPDIR and the tmux server are all
//! test-owned. The provider is a PATH shim; no real provider is started.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hermetic_guard::HermeticTestEnv;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::transport::Transport;
use team_agent::leader::registry::{
    self, RegistryRollback, RegistryRollbackError, RegistryWriteReceipt,
};

const TEAM: &str = "current";

// ── C1 / C3: registry rollback contract ─────────────────────────────────

fn publish(workspace: &Path, team: &str, epoch: u64) -> RegistryWriteReceipt {
    let entry = registry::build_entry(
        workspace,
        team,
        "direct_tmux",
        json!({"status": "attached", "pane_id": "%448", "owner_epoch": epoch}),
        epoch,
        "issue448-fixture",
        "2026-11-27T00:00:00Z".to_string(),
    );
    let path = registry::write_entry_best_effort(&entry).expect("SETUP: registry publish");
    let bytes = fs::read(&path).expect("SETUP: read published receipt bytes");
    RegistryWriteReceipt { path, bytes }
}

fn entry_path(workspace: &Path, team: &str) -> PathBuf {
    registry::registry_dir()
        .expect("SETUP: hermetic HOME")
        .join(format!("{}__{team}.json", registry::workspace_hash(workspace)))
}

#[test]
#[serial(env)]
fn c1_rollback_after_stop_unregister_without_preimage_is_ok_and_stays_absent() {
    let env = HermeticTestEnv::enter("i448-c1-none");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 3);
    assert_eq!(receipt.path, entry_path(&workspace, TEAM), "SETUP: receipt path");

    // Exactly the `team-agent stop` success hook.
    let removed = registry::unregister_entry(&workspace, TEAM);
    assert_eq!(removed.as_deref(), Some(receipt.path.as_path()), "SETUP: stop unregistered");
    assert!(!receipt.path.exists(), "SETUP: entry absent after unbind");

    let first = registry::restore_entry_if_current_matches(&receipt, None);
    assert!(
        first.is_ok(),
        "C1: an absent registry entry after canonical unbind already satisfies the \
         rollback target; restore must be Ok, got {first:?}"
    );
    assert!(!receipt.path.exists(), "C1: rollback must not create an entry");

    let second = registry::restore_entry_if_current_matches(&receipt, None);
    assert!(second.is_ok(), "C1: absent-entry rollback must be idempotent, got {second:?}");
    assert!(!receipt.path.exists(), "C1: repeated rollback must not create an entry");
}

#[test]
#[serial(env)]
fn c1_rollback_after_stop_unregister_never_resurrects_the_preimage() {
    let env = HermeticTestEnv::enter("i448-c1-preimage");
    let workspace = env.workspace("ws");
    let path = entry_path(&workspace, TEAM);
    let preimage = b"{\n  \"prior_discovery_record\": true\n}\n".to_vec();
    fs::write(&path, &preimage).expect("SETUP: seed preimage");
    let receipt = publish(&workspace, TEAM, 4);
    assert_ne!(fs::read(&path).unwrap(), preimage, "SETUP: publish replaced the preimage");

    assert!(registry::unregister_entry(&workspace, TEAM).is_some(), "SETUP: stop unregistered");

    let result = registry::restore_entry_if_current_matches(&receipt, Some(&preimage));
    assert!(
        matches!(result, Ok(outcome) if outcome != RegistryRollback::Restored),
        "C1: after canonical unbind the receipt is no longer current; rollback must be a \
         benign non-restoring Ok, got {result:?}"
    );
    assert!(
        !path.exists(),
        "C1: rollback must not resurrect the stale preimage after `team-agent stop` \
         (invariant: no registry entry after unbind); found {:?}",
        fs::read(&path).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    );
}

#[test]
#[serial(env)]
fn c1_rollback_after_registry_directory_removed_is_ok_and_stays_absent() {
    let env = HermeticTestEnv::enter("i448-c1-dir");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 5);
    fs::remove_dir_all(registry::registry_dir().unwrap()).expect("SETUP: remove registry dir");

    let result = registry::restore_entry_if_current_matches(&receipt, None);
    assert!(
        result.is_ok(),
        "C1: a missing registry directory means the entry is absent; restore must be Ok, \
         got {result:?}"
    );
    assert!(!receipt.path.exists(), "C1: rollback must not create an entry");
}

#[test]
#[serial(env)]
fn c3_rollback_fails_closed_when_entry_path_is_unreadable_directory() {
    let env = HermeticTestEnv::enter("i448-c3-eisdir");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 6);
    fs::remove_file(&receipt.path).unwrap();
    fs::create_dir(&receipt.path).expect("SETUP: occupy entry path with a directory");

    let result = registry::restore_entry_if_current_matches(&receipt, None);
    assert_eq!(
        result,
        Err(RegistryRollbackError::ReadFailed),
        "C3: only NotFound is benign; an unreadable entry must stay fail-closed"
    );
    assert!(receipt.path.is_dir(), "C3: an unreadable entry must be left untouched");
}

#[test]
#[serial(env)]
fn c3_rollback_fails_closed_when_entry_is_permission_denied() {
    let env = HermeticTestEnv::enter("i448-c3-eperm");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 7);
    fs::set_permissions(&receipt.path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&receipt.path).is_ok() {
        fs::set_permissions(&receipt.path, fs::Permissions::from_mode(0o600)).unwrap();
        eprintln!("C3 EPERM case not observable: this user bypasses file modes");
        return;
    }

    let result = registry::restore_entry_if_current_matches(&receipt, None);
    fs::set_permissions(&receipt.path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        result,
        Err(RegistryRollbackError::ReadFailed),
        "C3: permission denied is not absence; rollback must stay fail-closed"
    );
    assert_eq!(
        fs::read(&receipt.path).unwrap(),
        receipt.bytes,
        "C3: an unreadable entry must be left untouched"
    );
}

#[test]
#[serial(env)]
fn c3_rollback_refuses_receipt_outside_registry_even_when_absent() {
    let env = HermeticTestEnv::enter("i448-c3-parent");
    let workspace = env.workspace("ws");
    let mut receipt = publish(&workspace, TEAM, 8);
    let foreign = env.root().join("not-the-registry");
    fs::create_dir_all(&foreign).unwrap();
    receipt.path = foreign.join(receipt.path.file_name().unwrap());
    assert!(!receipt.path.exists(), "SETUP: foreign receipt path is absent");

    let result = registry::restore_entry_if_current_matches(&receipt, None);
    assert_eq!(
        result,
        Err(RegistryRollbackError::InvalidReceipt),
        "C3: NotFound tolerance must not bypass the registry-directory check"
    );
    assert!(!receipt.path.exists(), "C3: an invalid receipt must not create a file");
}

#[test]
#[serial(env)]
fn c3_rollback_preserves_foreign_bytes_as_superseded() {
    let env = HermeticTestEnv::enter("i448-c3-foreign");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 9);
    let foreign = b"not json: concurrent or corrupt writer".to_vec();
    fs::write(&receipt.path, &foreign).unwrap();

    let result = registry::restore_entry_if_current_matches(&receipt, None);
    assert_eq!(result, Ok(RegistryRollback::Superseded), "C3: foreign bytes are not ours");
    assert_eq!(fs::read(&receipt.path).unwrap(), foreign, "C3: foreign bytes must survive");
}

#[test]
#[serial(env)]
fn c3_rollback_still_restores_a_live_own_receipt() {
    let env = HermeticTestEnv::enter("i448-c3-own");
    let workspace = env.workspace("ws");
    let receipt = publish(&workspace, TEAM, 10);
    assert_eq!(
        registry::restore_entry_if_current_matches(&receipt, None),
        Ok(RegistryRollback::Restored),
        "C3: a still-current own receipt without preimage is deleted"
    );
    assert!(!receipt.path.exists(), "C3: own receipt must be removed");

    let preimage = b"{\"prior\":true}\n".to_vec();
    fs::write(&receipt.path, &preimage).unwrap();
    let receipt = publish(&workspace, TEAM, 11);
    assert_eq!(
        registry::restore_entry_if_current_matches(&receipt, Some(&preimage)),
        Ok(RegistryRollback::Restored),
        "C3: a still-current own receipt with preimage is restored"
    );
    assert_eq!(fs::read(&receipt.path).unwrap(), preimage, "C3: exact preimage restored");
}

// ── C2: end-to-end managed launch -> in-pane stop -> provider exit ──────

const STEP: Duration = Duration::from_secs(30);

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fixture");
}

fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// The provider shim acts as the Leader. On `go` it optionally starts one
/// built-in fake worker and runs the whole-team `team-agent stop --json` from
/// inside its own managed pane, then marks `phase1.done`; it exits 0 on
/// `exit`. Every loop is bounded.
fn provider_shim(state: &Path, team_agent: &Path, with_stop: bool) -> String {
    let s = sh_quote(state);
    let bin = sh_quote(team_agent);
    let leader_commands = if with_stop {
        format!(
            "{bin} start alpha --provider fake --model fake --bypass false --json > {s}/start.stdout 2> {s}/start.stderr\n\
             printf '%s\\n' \"$?\" > {s}/start.code\n\
             {bin} stop --json > {s}/stop.stdout 2> {s}/stop.stderr\n\
             printf '%s\\n' \"$?\" > {s}/stop.code\n"
        )
    } else {
        String::new()
    };
    format!(
        r#"#!/bin/sh
if [ "${{1-}}" = "--version" ]; then printf '2.1.0 (Claude Code)\n'; exit 0; fi
: > {s}/provider.started
i=0
until [ -f {s}/go ] || [ "$i" -ge 600 ]; do sleep 0.1; i=$((i + 1)); done
{leader_commands}: > {s}/phase1.done
i=0
until [ -f {s}/exit ] || [ "$i" -ge 600 ]; do sleep 0.1; i=$((i + 1)); done
exit 0
"#
    )
}

fn open_pty() -> io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    if unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

fn drain(mut reader: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        let _ = tx.send(out);
    });
    rx
}

fn cli_error_logs(workspace: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(workspace.join(".team").join("logs")) else {
        return Vec::new();
    };
    let mut logs: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("cli-error-"))
        })
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap_or_default();
            (path, text)
        })
        .collect();
    logs.sort();
    logs
}

fn workspace_registry_entries(workspace: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}__", registry::workspace_hash(workspace));
    let Ok(entries) = fs::read_dir(registry::registry_dir().unwrap()) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
                name.starts_with(&prefix) && name.ends_with(".json")
            })
        })
        .collect();
    paths.sort();
    paths
}

struct ManagedLeader {
    child: Child,
    master: Option<mpsc::Receiver<Vec<u8>>>,
    stderr: Option<mpsc::Receiver<Vec<u8>>>,
    tmux: PathBuf,
    tmux_tmpdir: PathBuf,
    endpoint: String,
}

impl ManagedLeader {
    fn tmux(&self, args: &[&str]) -> String {
        Command::new(&self.tmux)
            .args(["-L", &self.endpoint])
            .args(args)
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    fn wait_exit(&mut self, deadline: Duration) -> Option<ExitStatus> {
        let started = Instant::now();
        while started.elapsed() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn outputs(&mut self) -> (String, String) {
        let take = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
            rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(5)).ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        (take(self.stderr.take()), take(self.master.take()))
    }
}

impl Drop for ManagedLeader {
    fn drop(&mut self) {
        let pid = self.child.id() as libc::pid_t;
        if self.alive() {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let _ = self.wait_exit(Duration::from_secs(5));
        }
        // setsid below: the launcher leads its own process group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        let _ = self.child.wait();
        let _ = self.tmux(&["kill-server"]);
    }
}

fn wait_for(what: &str, deadline: Duration, mut ready: impl FnMut() -> bool, diag: impl Fn() -> String) {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("SETUP timeout waiting for {what} within {deadline:?}; {}", diag());
}

struct Journey {
    status: ExitStatus,
    stderr: String,
    tty: String,
    cli_errors: Vec<(PathBuf, String)>,
    registry_after: Vec<PathBuf>,
}

impl Journey {
    fn transcript(&self) -> String {
        let chars: Vec<char> = self.tty.chars().collect();
        let tty_tail: String = chars[chars.len().saturating_sub(400)..].iter().collect();
        format!(
            "status={:?}\nstderr={}\ntty_tail={tty_tail:?}\ncli_error_logs={:#?}\nregistry_after={:?}",
            self.status, self.stderr, self.cli_errors, self.registry_after
        )
    }
}

/// Real public journey: `team-agent claude` in a PTY outside tmux creates the
/// managed leader on the workspace's isolated tmux server and attaches to it.
fn managed_leader_journey(tag: &str, with_stop: bool) -> Journey {
    let real_registry = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter(tag);
    let workspace = env.workspace("ws");
    let state = env.root().join("provider-state");
    let fake_bin = env.root().join("fake-bin");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    let team_agent = PathBuf::from(env!("CARGO_BIN_EXE_team-agent"));
    write_executable(&fake_bin.join("claude"), &provider_shim(&state, &team_agent, with_stop));
    let tmux = hermetic_guard::fixture_tmux_binary();
    let tmux_tmpdir = PathBuf::from(std::env::var_os("TMUX_TMPDIR").expect("hermetic TMUX_TMPDIR"));
    let endpoint = team_agent::tmux_backend::TmuxBackend::for_workspace(&workspace)
        .tmux_endpoint()
        .expect("workspace tmux endpoint");
    assert!(cli_error_logs(&workspace).is_empty(), "SETUP: fresh workspace has no cli-error log");
    assert!(workspace_registry_entries(&workspace).is_empty(), "SETUP: no prior registry entry");

    let (master, slave) = open_pty().expect("allocate controlling tty");
    let slave_out = slave.try_clone().unwrap();
    let master_reader = master.try_clone().unwrap();
    let mut command = Command::new(&team_agent);
    command.arg("claude").current_dir(&workspace);
    env.configure_command(&mut command);
    hermetic_guard::remove_actions_job_env(&mut command);
    command
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn managed leader launcher in a pty");
    let stderr = drain(child.stderr.take().unwrap());
    let mut leader = ManagedLeader {
        child,
        master: Some(drain(master_reader)),
        stderr: Some(stderr),
        tmux,
        tmux_tmpdir,
        endpoint,
    };
    let _master_keepalive = master;

    let snapshot = |leader: &ManagedLeader| {
        format!(
            "files={:?} registry={:?} clients={:?} panes={:?}",
            fs::read_dir(&state)
                .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
                .unwrap_or_default(),
            workspace_registry_entries(&workspace),
            leader.tmux(&["list-clients", "-F", "#{client_tty} #{session_name}"]),
            leader.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_current_command} #{pane_dead}"]),
        )
    };

    // 1. Managed launch reaches a published registry entry and an attached client.
    let started = Instant::now();
    while started.elapsed() < STEP {
        let attached = !leader.tmux(&["list-clients", "-F", "#{client_tty}"]).trim().is_empty();
        if state.join("provider.started").exists()
            && !workspace_registry_entries(&workspace).is_empty()
            && attached
        {
            break;
        }
        if !leader.alive() {
            let status = leader.child.try_wait().unwrap();
            let (stderr, _tty) = leader.outputs();
            panic!("SETUP: launcher exited before attach: {status:?}; stderr={stderr}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        started.elapsed() < STEP,
        "SETUP: managed leader did not attach within {STEP:?}; {}",
        snapshot(&leader)
    );
    let published = workspace_registry_entries(&workspace);

    // 2. Optionally, the Leader starts a worker and stops the whole team from
    //    inside its own pane.
    fs::write(state.join("go"), b"").unwrap();
    wait_for("leader phase 1", Duration::from_secs(60), || state.join("phase1.done").exists(), || snapshot(&leader));
    if with_stop {
        let read = |name: &str| fs::read_to_string(state.join(name)).unwrap_or_default();
        assert_eq!(
            read("start.code").trim(),
            "0",
            "SETUP: in-pane `team-agent start alpha --provider fake` must succeed; stdout={} stderr={}",
            read("start.stdout"),
            read("start.stderr")
        );
        let stop_json: Value = serde_json::from_str(read("stop.stdout").trim()).unwrap_or(Value::Null);
        assert!(
            read("stop.code").trim() == "0" && stop_json["ok"] == json!(true),
            "SETUP: in-pane whole-team `team-agent stop --json` must succeed; code={:?} stdout={} stderr={}",
            read("stop.code"),
            read("stop.stdout"),
            read("stop.stderr")
        );
        assert!(
            workspace_registry_entries(&workspace).is_empty(),
            "C2-precondition: a clean whole-team stop must unregister the published entry \
             {published:?} (invariant: no registry entry after unbind); {}",
            snapshot(&leader)
        );
    } else {
        assert_eq!(
            workspace_registry_entries(&workspace),
            published,
            "SETUP: without stop the published entry remains until launcher exit"
        );
    }

    // 3. The provider exits 0; the launcher tears down and returns.
    fs::write(state.join("exit"), b"").unwrap();
    let status = leader.wait_exit(STEP);
    let (stderr, tty) = leader.outputs();
    let status = status.unwrap_or_else(|| {
        panic!("C2: launcher did not exit within {STEP:?} after provider exit; stderr={stderr}")
    });
    let journey = Journey {
        status,
        stderr,
        tty,
        cli_errors: cli_error_logs(&workspace),
        registry_after: workspace_registry_entries(&workspace),
    };
    drop(leader);
    env.assert_real_registry_unchanged(real_registry);
    journey
}

fn assert_clean_exit(journey: &Journey, case: &str) {
    let transcript = journey.transcript();
    assert!(
        !journey.stderr.contains("registry rollback failed")
            && !journey.tty.contains("registry rollback failed"),
        "{case}: a clean provider exit must not report a registry rollback failure\n{transcript}"
    );
    assert_eq!(
        journey.status.code(),
        Some(0),
        "{case}: the managed launcher must exit 0 after a clean provider exit\n{transcript}"
    );
    assert!(
        journey.cli_errors.is_empty(),
        "{case}: no spurious cli-error log may be written for a clean journey\n{transcript}"
    );
    assert!(
        journey.registry_after.is_empty(),
        "{case}: no registry entry may remain or be resurrected after launcher exit\n{transcript}"
    );
}

#[test]
#[serial(env)]
fn c2_managed_leader_exits_zero_after_in_pane_stop_and_clean_provider_exit() {
    let journey = managed_leader_journey("i448-e2e-stop", true);
    assert_clean_exit(&journey, "C2");
}

/// Positive control for the harness: the same journey without `stop` rolls its
/// own entry back and exits 0 on the base, so a C2 failure is attributable to
/// the stop-then-exit ordering alone.
#[test]
#[serial(env)]
fn c2_control_managed_leader_exits_zero_on_clean_provider_exit_without_stop() {
    let journey = managed_leader_journey("i448-e2e-ctl", false);
    assert_clean_exit(&journey, "C2-control");
}
