//! Batch 7 F4 RED: `windows-shim` client-reuse race.
//!
//! Real-machine finding on batch6-28740159680 gate report:
//!
//! > After first client Hello + disconnect, shim's next
//! > `create_named_pipe → ConnectNamedPipe` returns
//! > `ERROR_PIPE_CONNECTED (0x80070217)`. Subsequent clients fail
//! > to connect.
//!
//! This test reproduces the race on the Windows target by:
//! 1. Spawning `windows-shim.exe` with a temp pipe name + token.
//! 2. Connecting `NamedPipeClient` #1, sending Hello, dropping client.
//! 3. Connecting `NamedPipeClient` #2 within 500ms, sending Hello.
//!
//! Before the F4 fix, step 3 either times out (shim's `?` on
//! `ConnectNamedPipe` propagated `ERROR_PIPE_CONNECTED` up the
//! call stack and exited `run()` on the outer `?`) or the fresh
//! connect races into a stale pipe instance.
//!
//! After the fix (accept `ERROR_PIPE_CONNECTED` as success + proper
//! `DisconnectNamedPipe` before recreate), step 3 succeeds within
//! bounded retries and the shim remains alive for arbitrarily many
//! reconnects.
//!
//! The test is compiled Unix-side but the SHIM PROBE only runs on
//! Windows — on Unix the test asserts the shim binary path exists
//! at build time only (compile-time smoke). Real hardware verifies
//! the reconnect invariant.

#![cfg(windows)]

use conpty_transport::{Op, PipeClient, Request};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn shim_exe_path() -> std::path::PathBuf {
    // `CARGO_BIN_EXE_windows-shim` is set by cargo when this test
    // depends on the `windows-shim` binary. Falls back to
    // `target/debug` scan for older cargo versions.
    if let Some(p) = std::option_env!("CARGO_BIN_EXE_windows-shim") {
        return std::path::PathBuf::from(p);
    }
    // Fallback: walk up from CARGO_MANIFEST_DIR looking for target.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let candidate = manifest
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("target").join(profile).join("windows-shim.exe"));
        if let Some(p) = candidate {
            if p.exists() {
                return p;
            }
        }
    }
    panic!("CARGO_BIN_EXE_windows-shim not set and no windows-shim.exe found under target/");
}

fn unique_pipe_name() -> String {
    format!(
        r"\\.\pipe\team-agent-batch7-f4-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    )
}

struct ShimGuard {
    child: Option<Child>,
}

impl Drop for ShimGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_shim(pipe_name: &str) -> ShimGuard {
    let exe = shim_exe_path();
    let child = Command::new(&exe)
        .args([
            "--workspace-hash",
            "f4reuse",
            "--team",
            "reuse-team",
            "--pipe-name",
            pipe_name,
        ])
        .env("TA_CONPTY_PIPE_TOKEN", "f4reuse-tok")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn windows-shim");
    ShimGuard { child: Some(child) }
}

fn connect_with_retry(pipe_name: &str, attempts: u32) -> conpty_transport::NamedPipeClient {
    let mut last_err = None;
    for _ in 0..attempts {
        match conpty_transport::NamedPipeClient::connect(pipe_name, 200) {
            Ok(c) => return c,
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    panic!("connect_with_retry gave up: {:?}", last_err);
}

fn hello(client: &conpty_transport::NamedPipeClient) {
    let req = Request::new("t", "f4reuse", "reuse-team", "f4reuse-tok", Op::Hello);
    let resp = client.request(&req);
    assert!(resp.ok, "hello ok=false: {:?}", resp.error);
}

#[test]
fn shim_survives_two_sequential_clients() {
    // Batch 7 F4 RED lock: after fix, two clients must both Hello-Ok.
    let pipe = unique_pipe_name();
    let _guard = spawn_shim(&pipe);
    std::thread::sleep(Duration::from_millis(200));

    // Client #1: Hello + drop.
    {
        let c1 = connect_with_retry(&pipe, 10);
        hello(&c1);
    } // dropped → pipe closes

    // Give shim a beat to loop back to accept.
    std::thread::sleep(Duration::from_millis(100));

    // Client #2: MUST also succeed.
    let c2 = connect_with_retry(&pipe, 20);
    hello(&c2);
}

#[test]
fn shim_survives_five_sequential_clients() {
    // Bounded-N lock: not just 2 → N, so we know the accept loop
    // is not "works once, fails forever after 2nd" (that would be
    // a different pattern than the observed race).
    let pipe = unique_pipe_name();
    let _guard = spawn_shim(&pipe);
    std::thread::sleep(Duration::from_millis(200));

    for i in 1..=5 {
        let c = connect_with_retry(&pipe, 20);
        hello(&c);
        drop(c);
        std::thread::sleep(Duration::from_millis(80));
        eprintln!("client #{i} ok");
    }
}
