#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "support/hermetic.rs"]
mod hermetic;
#[path = "support/proc_guard.rs"]
mod proc_guard;
#[path = "support/temp_workspace.rs"]
#[allow(dead_code)]
mod temp_workspace;

use proc_guard::{FixtureGroup, TmuxRoot};
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use temp_workspace::TempWorkspace;

fn alive(pid: u32) -> bool {
    Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .ok()
        .is_some_and(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .any(|state| !state.starts_with('Z'))
        })
}
fn until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    condition()
}

#[test]
#[ignore = "owned subprocess for the interrupt-safety oracle"]
fn proc_guard_child() {
    let marker = std::path::PathBuf::from(std::env::var_os("FIXTURE_GUARD_WORKSPACE").unwrap());
    let (_group, pid) = FixtureGroup::detached(
        &marker,
        "trap '' TERM; sleep 60 & wait $!",
        &[],
        Duration::from_secs(60),
    );
    let root = TmuxRoot::new();
    let socket = root.path().join("oracle.sock");
    let status = Command::new("tmux")
        .args(["-f", "/dev/null", "-S"])
        .arg(&socket)
        .args(["new-session", "-d", "-s", "guard-oracle", "exec cat"])
        .env_remove("TMUX")
        .status()
        .unwrap();
    assert!(status.success());
    let server = Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .args(["display-message", "-p", "#{pid}"])
        .output()
        .unwrap();
    assert!(server.status.success());
    let server_pid: u32 = String::from_utf8_lossy(&server.stdout)
        .trim()
        .parse()
        .unwrap();
    println!(
        "\nGUARD {}",
        serde_json::json!({"pid":pid,"server_pid":server_pid,"socket":socket,"root":root.path()})
    );
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[test]
fn sigkill_parent_closes_lifelines_and_reaps_detached_process_and_tmux() {
    let workspace = TempWorkspace::new("ta-rs", "guard-interrupt");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "proc_guard_child",
            "--ignored",
            "--nocapture",
            "--skip",
        ])
        .arg(workspace.path())
        .env("FIXTURE_GUARD_WORKSPACE", workspace.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut helper = FixtureGroup::spawn(&mut command, workspace.path(), Duration::from_secs(60));
    let reader = std::io::BufReader::new(helper.stdout.take().unwrap());
    let row = reader
        .lines()
        .map(Result::unwrap)
        .find_map(|line| line.strip_prefix("GUARD ").map(str::to_owned))
        .expect("helper published owned identities");
    let row: serde_json::Value = serde_json::from_str(&row).unwrap();
    let pid = row["pid"].as_u64().unwrap() as u32;
    let server = row["server_pid"].as_u64().unwrap() as u32;
    let root = std::path::Path::new(row["root"].as_str().unwrap());
    assert!(alive(pid) && alive(server));
    helper.kill().unwrap();
    helper.wait().unwrap();
    // The outer guard is deliberately still alive: only the dead helper's
    // closed pipes may clean its separately grouped descendants and server.
    assert!(
        until(|| !alive(pid) && !alive(server) && !root.exists()),
        "owned residue after parent SIGKILL: {row}"
    );
    let output = Command::new("tmux")
        .args(["-S", row["socket"].as_str().unwrap(), "list-sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    println!("O1 parent-SIGKILL: detached pid, tmux server, and private root all gone");
}

#[test]
fn unregistered_named_tmux_server_is_owned_by_hermetic_root() {
    let environment = hermetic::HermeticTestEnv::for_subprocess_with_leader("unregistered-root");
    let mut command = Command::new(hermetic::fixture_tmux_binary());
    environment.configure_command(&mut command);
    let root = std::path::PathBuf::from(
        command
            .get_envs()
            .find(|(key, _)| *key == "TMUX_TMPDIR")
            .unwrap()
            .1
            .unwrap(),
    );
    let output = command
        .args([
            "-f",
            "/dev/null",
            "-L",
            "unregistered",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pid}",
            "-s",
            "owned",
            "exec cat",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid));
    // Deliberately never register the SUT-style named socket.
    drop(environment);
    assert!(until(|| !alive(pid) && !root.exists()));
}

#[test]
fn coordinator_presence_exits_when_its_stdin_lifeline_closes() {
    let workspace = TempWorkspace::new("ta-rs", "coordinator-pipe");
    let mut child = hermetic::spawn_owned_coordinator(workspace.path());
    child.stdin.take();
    assert!(until(|| !alive(child.id())));
    child.wait().unwrap();
}

#[test]
fn drop_kills_a_term_ignoring_grandchild() {
    let workspace = TempWorkspace::new("ta-rs", "guard-drop");
    let (guard, pid) = FixtureGroup::detached(
        workspace.path(),
        "trap '' TERM; sleep 60 & wait $!",
        &[],
        Duration::from_secs(60),
    );
    assert!(alive(pid));
    drop(guard);
    assert!(until(|| !alive(pid)));
    println!("O2 normal Drop: TERM-ignoring grandchild gone");
}

#[test]
fn mismatched_marker_never_signals_an_unrelated_group() {
    let workspace = TempWorkspace::new("ta-rs", "guard-mismatch");
    let mut bystander = FixtureGroup::presence(workspace.path(), Duration::from_secs(30));
    assert!(!proc_guard::kill_marked_group(
        bystander.id(),
        &workspace.path().join("not-this-fixture")
    ));
    assert!(bystander.try_wait().unwrap().is_none());
    println!("O2 mismatched marker: unrelated group remains alive");
}
