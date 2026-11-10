#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "../../team-agent/tests/support/proc_guard.rs"]
mod proc_guard;
#[path = "../../team-agent/tests/support/temp_workspace.rs"]
#[allow(dead_code)]
mod temp_workspace;

use proc_guard::FixtureGroup;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
mod physical_support;

fn tmux() -> std::path::PathBuf {
    std::env::var_os("CONTRACT_TMUX")
        .expect("prepared absolute tmux")
        .into()
}
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
    use std::os::unix::fs::DirBuilderExt;
    let path = std::path::Path::new("/tmp").join(format!("tac-guard-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let root = physical_support::OwnedTmuxRoot::new(&path, &tmux());
    let socket = root.path().join("oracle.sock");
    let status = Command::new(tmux())
        .args(["-f", "/dev/null", "-S"])
        .arg(&socket)
        .args(["new-session", "-d", "-s", "guard-oracle", "exec cat"])
        .env_remove("TMUX")
        .status()
        .unwrap();
    assert!(status.success());
    let server = Command::new(tmux())
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
    let output = Command::new(tmux())
        .args(["-S", row["socket"].as_str().unwrap(), "list-sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    println!("O1 parent-SIGKILL: detached pid, tmux server, and private root all gone");
}
