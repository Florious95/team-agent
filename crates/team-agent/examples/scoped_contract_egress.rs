//! Explicit operator-only forwarding of existing committed native MCP receipts.
//! No coordinator loop, native input, new source envelope, or candidate migration.
use std::{error::Error, io, path::PathBuf, time::Duration};

use serde_json::{json, Value};
use team_agent_contract::host::process::{
    capture_process, fingerprint_file, sample_process, ProcessState,
};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--help"] {
        println!("scoped_contract_egress --workspace ABS --team TEAM --scope SCOPE [--apply]");
        return Ok(());
    }
    let mut workspace = None;
    let mut team = None;
    let mut scope = None;
    let mut apply = false;
    let mut args = arguments.iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--workspace" if workspace.is_none() => {
                workspace = Some(PathBuf::from(
                    args.next().ok_or_else(|| invalid("workspace value"))?,
                ));
            }
            "--team" if team.is_none() => {
                team = Some(args.next().ok_or_else(|| invalid("team value"))?.clone());
            }
            "--scope" if scope.is_none() => {
                scope = Some(args.next().ok_or_else(|| invalid("scope value"))?.clone());
            }
            "--apply" if !apply => apply = true,
            _ => return Err(invalid("unknown, repeated, or positional argument").into()),
        }
    }
    let workspace = workspace.ok_or_else(|| invalid("absolute workspace required"))?;
    let team = team.ok_or_else(|| invalid("explicit team required"))?;
    let scope = scope.ok_or_else(|| invalid("explicit scope required"))?;
    if !workspace.is_absolute() {
        return Err(invalid("workspace must be absolute").into());
    }
    // Fence the running daemon without touching its PID/metadata/heartbeat or argv/env.
    let metadata = workspace.join(".team/runtime/coordinator.json");
    let original = std::fs::read(&metadata)?;
    let value: Value = serde_json::from_slice(&original)?;
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| invalid("coordinator PID missing"))?;
    let binary = PathBuf::from(
        value["binary_path"]
            .as_str()
            .ok_or_else(|| invalid("coordinator image missing"))?,
    )
    .canonicalize()?;
    let hash = fingerprint_file(&binary, 1024 * 1024 * 1024, Duration::from_secs(5))?;
    let daemon = capture_process(pid, &binary, hash, Duration::from_secs(5))?;
    let outcome = team_agent::contract_runtime::forward::drain_scoped_pending_egress(
        &workspace, &team, &scope, apply,
    )
    .map_err(io::Error::other)?;
    if std::fs::read(metadata)? != original || sample_process(&daemon) != ProcessState::Alive {
        return Err(io::Error::other(
            "coordinator identity changed; inspect, never replay Unknown forwards",
        )
        .into());
    }
    println!(
        "{}",
        json!({
            "ok":true, "coordinator_pid":pid, "coordinator_birth":daemon.identity.birth_identity,
            "coordinator_unchanged":true, "receipt":outcome,
            "boundary":"framework forwarding acceptance is not actual Leader consumption"
        })
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
