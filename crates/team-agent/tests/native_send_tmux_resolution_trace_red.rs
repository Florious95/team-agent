//! 0.6.2 observable contract (b)/(c) for a native-seat `send`, measured with a Linux
//! syscall trace of the real CLI process (no production observability):
//! (b) at most one tmux resolution per physical send. A unique missing PATH prefix
//!     sees one metadata probe per resolution.
//! (c) no PATH probe after the first valid match. A unique suffix after the real
//!     tmux directory must never be probed.
//! Child `execve` attempts (std::process PATH search) are counted separately.
//! On non-Linux hosts this test is reported as ignored (unexecuted), never as a pass.
#![cfg(unix)]
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[path = "support/direct_native_fixture.rs"]
mod direct_native_fixture;

fn real_tmux_dir() -> PathBuf {
    let output = Command::new("sh").args(["-c", "command -v tmux"]).output().unwrap();
    assert!(output.status.success(), "tmux is required");
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).parent().unwrap().to_path_buf()
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires Linux strace; unexecuted on this platform")]
fn native_send_resolves_tmux_at_most_once_and_never_probes_past_first_match() {
    let strace = Command::new("sh").args(["-c", "command -v strace"]).output().unwrap();
    assert!(strace.status.success(), "strace is required to execute this Linux oracle (install strace on the runner)");
    let fixture = direct_fixture::DirectFixture::new();
    assert!(Command::new("tmux").args(["-S", &fixture.socket, "rename-session", "-t", "oracle", "direct-test"])
        .status().unwrap().success());
    let mut state = json!({"active_team_key": "alpha", "tmux_socket": fixture.socket,
        "teams": {"alpha": {"team_id": "alpha", "team_name": "friendly", "status": "running",
            "tmux_socket": fixture.socket, "leader": {"pane_id": fixture.leader_pane, "provider": "retired-provider"},
            "agents": {"worker": {"pane_id": fixture.pane, "provider": "codex", "status": "running"}}}}});
    let native = direct_native_fixture::NativeAuthority::start(&fixture.root, "codex");
    {
        let worker = &mut state["teams"]["alpha"]["agents"]["worker"];
        worker["runtime_family"] = json!("contract");
        worker["native_endpoint"] = json!(native.socket);
        worker["pane_id"] = json!(native.pane);
        worker["pane_pid"] = json!(native.pid);
        worker["pane_birth_identity"] = json!(team_agent_contract::host::process::process_birth_identity(native.pid).unwrap());
    }
    fixture.set_state(&state);

    let tag = format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap());
    let prefix = fixture.root.join(format!("missing-prefix-{tag}"));
    let suffix = fixture.root.join(format!("missing-suffix-{tag}"));
    let real_dir = real_tmux_dir();
    let path = format!("{}:{}:{}:{}", prefix.display(), real_dir.display(), suffix.display(),
        std::env::var("PATH").unwrap());
    let log = fixture.root.join(format!("strace-{tag}.log"));
    let output = Command::new("strace")
        .args(["-f", "-qq", "-s", "4096", "-e", "trace=%file,execve", "-o"]).arg(&log)
        .arg(env!("CARGO_BIN_EXE_team-agent"))
        .args(["send", "worker", "resolution trace probe", "--team", "alpha", "--workspace"])
        .arg(&fixture.root).arg("--json")
        .env("PATH", &path)
        .env_clear_team_agent()
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["delivery"], "physical", "{receipt}");

    let trace = fs::read_to_string(&log).unwrap();
    let count = |sentinel: &PathBuf, exec: bool| {
        let needle = format!("\"{}/tmux\"", sentinel.display());
        trace.lines().filter(|line| line.contains(&needle) && line.contains("execve(") == exec).count()
    };
    let (prefix_probes, prefix_execs) = (count(&prefix, false), count(&prefix, true));
    let (suffix_probes, suffix_execs) = (count(&suffix, false), count(&suffix, true));
    eprintln!("tmux resolution trace: prefix probes={prefix_probes} execve={prefix_execs}; suffix probes={suffix_probes} execve={suffix_execs}");
    // Positive controls: an empty, filtered or mis-encoded (strace escapes non-ASCII)
    // trace must not pass. Direct tmux spawns search PATH, so the prefix is touched.
    assert!(prefix_probes + prefix_execs >= 1, "trace saw no PATH search through the prefix sentinel; trace unusable");
    assert!(trace.contains(&format!("\"{}/tmux\"", real_dir.display())), "trace never touched the real tmux; trace unusable");
    assert!(prefix_probes <= 1, "send resolved tmux {prefix_probes} times (contract: at most one per physical send)");
    assert_eq!(suffix_probes, 0, "PATH entries after the first valid tmux were probed");
    assert_eq!(suffix_execs, 0, "a child execve searched past the first valid tmux");
    drop(native);
}

/// The sender is an unbound operator: drop every inherited framework identity.
trait ClearTeamAgent { fn env_clear_team_agent(&mut self) -> &mut Self; }
impl ClearTeamAgent for Command {
    fn env_clear_team_agent(&mut self) -> &mut Self {
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("TEAM_AGENT_")) { self.env_remove(key); }
        self
    }
}
