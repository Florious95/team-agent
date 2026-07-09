// 0.5.x Windows portability Batch 5: this test file installs shell-
// script shims via `chmod +x` and probes Unix socket paths — Unix-
// only. Cfg-gate the whole file so `cargo check --tests --target
// x86_64-pc-windows-msvc` compiles cleanly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use serial_test::{file_serial, serial};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::save_runtime_state;

#[test]
#[ignore = "real-machine: spawns a persistent coordinator-shaped process for orphan scanning"]
#[file_serial(tmux)]
#[serial(doctor_comms)]
fn doctor_orphans_gate_reports_real_orphan_coordinator_instead_of_empty_pass() {
    let clean = DoctorFixture::new("orphans-clean");
    clean.seed_runtime();
    let orphan = DoctorFixture::new("orphans-missing");
    let mut child = ChildGuard::spawn_coordinator_placeholder(orphan.workspace());
    child.assert_still_running();
    std::fs::remove_dir_all(orphan.workspace()).expect("remove orphaned workspace");
    child.assert_still_running();

    let output = run_team_agent([
        "doctor",
        "--workspace",
        clean.workspace_str(),
        "--gate",
        "orphans",
        "--json",
    ]);
    let body = parse_stdout_json(&output);

    assert!(
        !output.status.success(),
        "doctor --gate orphans must exit nonzero when real orphan residue is present; stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        body["ok"],
        json!(false),
        "orphan gate must not return ok:true while a real orphan coordinator process exists; body={body}"
    );
    assert!(
        matches!(body["status"].as_str(), Some("failed" | "fail" | "degraded")),
        "orphan gate must report a non-pass status for real orphan residue; body={body}"
    );
    assert!(
        body["scanned"].as_i64().unwrap_or_default() > 0,
        "orphan gate must provide scan evidence, not scanned=0 placeholder; body={body}"
    );
    let expected_workspace = child.workspace_str().to_string();
    let orphans = body["orphans"]
        .as_array()
        .unwrap_or_else(|| panic!("orphan gate must expose an orphans array; body={body}"));
    assert!(
        orphans.iter().any(|item| {
            item["kind"].as_str() == Some("coordinator_process")
                && item["pid"].as_i64() == Some(i64::from(child.id()))
                && item["workspace"].as_str() == Some(expected_workspace.as_str())
                && item.get("reason").is_some()
        }),
        "orphan gate must report the real orphan coordinator pid/workspace/reason; pid={} body={body}",
        child.id()
    );
}

#[test]
#[serial(doctor_comms)]
fn doctor_comms_gate_fails_stale_receiver_and_runs_contract_suite_not_deferred() {
    let fixture = DoctorFixture::new("comms-stale");
    fixture.seed_runtime_with_stale_receiver();

    let output = run_team_agent([
        "doctor",
        "--workspace",
        fixture.workspace_str(),
        "--comms",
        "--json",
    ]);
    let body = parse_stdout_json(&output);

    assert!(
        !output.status.success(),
        "doctor --comms must exit nonzero when receiver binding is stale/broken; stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        body["ok"],
        json!(false),
        "comms gate top-level ok must be false when a mandatory subcheck fails; body={body}"
    );
    assert!(
        matches!(body["status"].as_str(), Some("fail" | "failed" | "degraded")),
        "comms gate top-level status must be non-pass when receiver_binding fails; body={body}"
    );
    assert_eq!(
        body.pointer("/checks/receiver_binding/status").and_then(Value::as_str),
        Some("fail"),
        "stale stored receiver pane must fail receiver_binding; body={body}"
    );
    assert!(
        body.pointer("/checks/receiver_binding/mismatches")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        "receiver_binding failure must include concrete mismatch/stale-pane evidence; body={body}"
    );
    assert_eq!(
        body.pointer("/checks/contract_suite/status").and_then(Value::as_str),
        Some("pass"),
        "contract_suite must run as a zero-token executable suite; deferred is no longer an acceptable passing gate; body={body}"
    );
    assert_eq!(
        body.pointer("/checks/provider_sdk_calls/calls/anthropic"),
        Some(&json!(0)),
        "comms selftest must prove zero Anthropic SDK calls; body={body}"
    );
    assert_eq!(
        body.pointer("/checks/provider_sdk_calls/calls/openai"),
        Some(&json!(0)),
        "comms selftest must prove zero OpenAI SDK calls; body={body}"
    );
    assert_eq!(
        body.pointer("/checks/provider_sdk_calls/calls/httpx"),
        Some(&json!(0)),
        "comms selftest must prove zero HTTP client calls; body={body}"
    );
}

struct DoctorFixture {
    workspace: PathBuf,
}

impl DoctorFixture {
    fn new(label: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let workspace = std::env::temp_dir().join(format!(
            "ta_doctor_comms_{label}_{}_{}",
            std::process::id(),
            n
        ));
        if workspace.exists() {
            let _ = std::fs::remove_dir_all(&workspace);
        }
        std::fs::create_dir_all(&workspace).unwrap();
        Self { workspace }
    }

    fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("utf8 temp workspace")
    }

    fn seed_runtime(&self) {
        let _ = MessageStore::open(&self.workspace).unwrap();
        save_runtime_state(
            &self.workspace,
            &json!({
                "session_name": "team-doctorcomms",
                "active_team_key": "doctorcomms",
                "agents": {
                    "worker": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker",
                        "owner_team_id": "doctorcomms"
                    }
                },
                "leader_receiver": {
                    "status": "attached",
                    "pane_id": "%leader",
                    "session_name": "team-doctorcomms"
                },
                "owner": {
                    "pane_id": "%leader"
                }
            }),
        )
        .unwrap();
    }

    fn seed_runtime_with_stale_receiver(&self) {
        self.seed_runtime();
        save_runtime_state(
            &self.workspace,
            &json!({
                "session_name": "team-doctorcomms",
                "active_team_key": "doctorcomms",
                "agents": {
                    "worker": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker",
                        "owner_team_id": "doctorcomms"
                    }
                },
                "leader_receiver": {
                    "status": "attached",
                    "pane_id": "%stale_receiver",
                    "session_name": "team-doctorcomms"
                },
                "owner": {
                    "pane_id": "%owner"
                }
            }),
        )
        .unwrap();
    }
}

impl Drop for DoctorFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct ChildGuard {
    child: Child,
    workspace: PathBuf,
    script_dir: PathBuf,
}

impl ChildGuard {
    fn spawn_coordinator_placeholder(workspace: &Path) -> Self {
        let script_dir = workspace
            .parent()
            .unwrap_or_else(|| Path::new("/tmp"))
            .join("orphan-bin");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("team-agent");
        std::fs::write(&script, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let child = Command::new(&script)
            .args(["coordinator", "--workspace"])
            .arg(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn persistent coordinator-shaped orphan process");
        Self {
            child,
            workspace: workspace.to_path_buf(),
            script_dir,
        }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("utf8 orphan workspace")
    }

    fn assert_still_running(&mut self) {
        thread::sleep(Duration::from_millis(50));
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "orphan fixture must keep a coordinator-shaped process alive until doctor scans it"
        );
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        terminate_process_tree(self.child.id());
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.workspace);
        let _ = std::fs::remove_dir_all(&self.script_dir);
    }
}

fn terminate_process_tree(root_pid: u32) {
    let mut pids = descendant_pids(root_pid);
    pids.push(root_pid);
    for signal in ["TERM", "KILL"] {
        for pid in &pids {
            let _ = Command::new("kill")
                .args([format!("-{signal}"), pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn descendant_pids(root_pid: u32) -> Vec<u32> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .expect("scan process table for orphan fixture cleanup");
    let text = String::from_utf8_lossy(&output.stdout);
    let pairs = text
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let pid = cols.next()?.parse::<u32>().ok()?;
            let ppid = cols.next()?.parse::<u32>().ok()?;
            Some((pid, ppid))
        })
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut stack = vec![root_pid];
    while let Some(parent) = stack.pop() {
        for (pid, ppid) in &pairs {
            if *ppid == parent && !out.contains(pid) {
                out.push(*pid);
                stack.push(*pid);
            }
        }
    }
    out
}

fn run_team_agent<const N: usize>(args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(args)
        .env("TMUX_PANE", "%caller")
        .output()
        .expect("run team-agent")
}

fn parse_stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be parseable JSON; parse_error={error}; stdout={} stderr={}",
            stdout(output),
            stderr(output)
        )
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}
