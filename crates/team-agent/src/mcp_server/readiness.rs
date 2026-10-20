//! A flushed MCP handshake is admission to first business, not client consumption.
//! Receipts are scope- and live-process-bound; no pane input or tools/call probe.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use team_agent_contract::host::process::{
    capture_process, fingerprint_file, process_birth_identity, sample_process, ProcessStamp,
    ProcessState, MAX_NATIVE_PARENT_HOPS,
};
use team_agent_contract::host::{digest, digest_hex};

use crate::transport::{PaneInfo, Target};

type Result<T> = std::result::Result<T, &'static str>;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
// A 73MB debug image exceeded 2s on the official host. This is a finite
// computation budget, never a sleep or a minimum startup delay.
const IMAGE_EVIDENCE_BUDGET: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ancestor {
    pid: u32,
    birth: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handshake {
    schema: u8,
    workspace: PathBuf,
    owner: String,
    agent: String,
    started_at: String,
    flushed_at: String,
    server: ProcessStamp,
    ancestors: Vec<Ancestor>,
}

fn private_metadata(path: &Path, directory: bool) -> Result<std::fs::Metadata> {
    let m = std::fs::symlink_metadata(path).map_err(|_| "mcp_handshake_missing")?;
    // SAFETY: geteuid has no arguments, pointers, or side effects.
    let uid = unsafe { libc::geteuid() };
    if m.file_type().is_symlink()
        || m.uid() != uid
        || (directory && !m.is_dir())
        || (!directory && (!m.is_file() || m.mode() & 0o022 != 0))
    {
        return Err("mcp_handshake_ownership");
    }
    Ok(m)
}

fn directory(workspace: &Path, create: bool) -> Result<PathBuf> {
    let mut path = workspace.to_path_buf();
    for component in [".team", "runtime", "mcp-handshakes"] {
        path.push(component);
        if create && !path.exists() {
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("mcp_handshake_directory"),
            }
        }
        let m = private_metadata(&path, true)?;
        if component == "mcp-handshakes" && m.mode() & 0o777 != 0o700 {
            return Err("mcp_handshake_ownership");
        }
    }
    Ok(path)
}

fn key(owner: &str, agent: &str) -> String {
    digest_hex(digest(format!("{owner}\0{agent}").as_bytes()))
}

fn read_json(path: &Path, require_private: bool) -> Result<Value> {
    let before = private_metadata(path, false)?;
    if (require_private && before.mode() & 0o777 != 0o600) || before.len() > MAX_RECEIPT_BYTES {
        return Err("mcp_handshake_invalid");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "mcp_handshake_missing")?;
    let opened = file.metadata().map_err(|_| "mcp_handshake_invalid")?;
    if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
        return Err("mcp_handshake_replaced");
    }
    let mut data = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| "mcp_handshake_invalid")?;
    if data.len() as u64 > MAX_RECEIPT_BYTES {
        return Err("mcp_handshake_invalid");
    }
    serde_json::from_slice(&data).map_err(|_| "mcp_handshake_invalid")
}

struct ReceiptLock(File);
impl Drop for ReceiptLock {
    fn drop(&mut self) {
        let _ = crate::platform::file_lock::unlock(&self.0);
    }
}

fn lock(path: &Path) -> Result<ReceiptLock> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "mcp_handshake_lock")?;
    private_metadata(path, false)?;
    let start = Instant::now();
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => return Ok(ReceiptLock(file)),
            Ok(false) if start.elapsed() < Duration::from_millis(500) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return Err("mcp_handshake_lock"),
        }
    }
}

/// Only the actual stdio writer calls this after initialize and tools/list flush.
/// File publication failure stays unready; it never becomes an input bypass.
pub(crate) fn publish(
    workspace: &Path,
    owner: &str,
    agent: &str,
    started_at: DateTime<Utc>,
) -> Result<()> {
    if owner.is_empty() || agent.is_empty() {
        return Err("mcp_handshake_scope_missing");
    }
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "mcp_handshake_workspace")?;
    let executable = std::env::current_exe().map_err(|_| "mcp_handshake_image")?;
    let image = fingerprint_file(&executable, 512 * 1024 * 1024, IMAGE_EVIDENCE_BUDGET).map_err(
        |error| {
            let _ = crate::event_log::EventLog::new(&workspace).write(
                "mcp.handshake_evidence_failed",
                serde_json::json!({
                    "stage": "image_fingerprint",
                    "operation": error.operation,
                    "kind": format!("{:?}", error.kind),
                    "image_bytes": std::fs::metadata(&executable).ok().map(|m| m.len()),
                    "budget_ms": IMAGE_EVIDENCE_BUDGET.as_millis(),
                }),
            );
            "mcp_handshake_image"
        },
    )?;
    let server = capture_process(
        std::process::id(),
        &executable,
        image,
        IMAGE_EVIDENCE_BUDGET,
    )
    .map_err(|_| "mcp_handshake_process")?;
    let mut ancestors = Vec::new();
    let mut parent = server.parent;
    for _ in 0..MAX_NATIVE_PARENT_HOPS {
        if parent <= 1 {
            break;
        }
        let birth = process_birth_identity(parent).map_err(|_| "mcp_handshake_ancestry")?;
        ancestors.push(Ancestor { pid: parent, birth });
        parent = crate::platform::argv::parent_pid(parent).unwrap_or(0);
    }
    if ancestors.is_empty() || sample_process(&server) != ProcessState::Alive {
        return Err("mcp_handshake_process");
    }
    let receipt = Handshake {
        schema: 1,
        workspace: workspace.clone(),
        owner: owner.to_string(),
        agent: agent.to_string(),
        started_at: started_at.to_rfc3339(),
        flushed_at: Utc::now().to_rfc3339(),
        server,
        ancestors,
    };
    let dir = directory(&workspace, true)?;
    let before = private_metadata(&dir, true)?;
    let name = key(owner, agent);
    let path = dir.join(format!("{name}.json"));
    let _lock = lock(&dir.join(format!("{name}.lock")))?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let old: Handshake = serde_json::from_value(read_json(&path, true)?)
                .map_err(|_| "mcp_handshake_invalid")?;
            if old.workspace != workspace
                || old.owner != owner
                || old.agent != agent
                || old.schema != 1
            {
                return Err("mcp_handshake_scope_mismatch");
            }
            let old_started = DateTime::parse_from_rfc3339(&old.started_at)
                .map_err(|_| "mcp_handshake_invalid")?;
            if old_started > started_at {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("mcp_handshake_invalid"),
    }
    let temp = dir.join(format!(
        "{name}.{}-{}.tmp",
        std::process::id(),
        crate::provider::adapter::next_session_token()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)
            .map_err(|_| "mcp_handshake_write")?;
        serde_json::to_writer(&mut file, &receipt).map_err(|_| "mcp_handshake_write")?;
        file.write_all(b"\n").map_err(|_| "mcp_handshake_write")?;
        file.sync_all().map_err(|_| "mcp_handshake_write")?;
        let after = private_metadata(&dir, true)?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err("mcp_handshake_replaced");
        }
        std::fs::rename(&temp, &path).map_err(|_| "mcp_handshake_write")?;
        File::open(&dir)
            .and_then(|file| file.sync_all())
            .map_err(|_| "mcp_handshake_write")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[derive(Debug)]
pub(crate) struct Admission {
    server: ProcessStamp,
    carrier: u32,
    carrier_birth: String,
    pane_id: String,
}

/// Observation-only preflight, before claiming a delivery attempt. Missing facts
/// perform no input; the delivery owner releases only its exact claim for retry.
pub(crate) fn preflight(
    workspace: &Path,
    owner: Option<&str>,
    agent_id: &str,
    state: &Value,
) -> Result<Option<Admission>> {
    if agent_id == "leader" {
        return Ok(None);
    }
    let Some(agent) = state.get("agents").and_then(|agents| agents.get(agent_id)) else {
        return Ok(None);
    };
    let provider = agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(crate::provider::wire::parse_provider)
        .ok_or("mcp_handshake_provider_missing")?;
    if !crate::provider::get_adapter(provider).requires_worker_mcp_handshake() {
        return Ok(None);
    }
    let config = agent
        .get("mcp_config")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or("mcp_handshake_config_missing")?;
    let owner = owner
        .filter(|s| !s.is_empty())
        .ok_or("mcp_handshake_scope_missing")?;
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "mcp_handshake_workspace")?;
    let dir = directory(&workspace, false)?;
    let receipt: Handshake = serde_json::from_value(read_json(
        &dir.join(format!("{}.json", key(owner, agent_id))),
        true,
    )?)
    .map_err(|_| "mcp_handshake_invalid")?;
    if receipt.schema != 1
        || receipt.workspace != workspace
        || receipt.owner != owner
        || receipt.agent != agent_id
    {
        return Err("mcp_handshake_scope_mismatch");
    }
    let spawned_at = agent
        .get("spawned_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or("mcp_handshake_spawn_missing")?;
    let started_at =
        DateTime::parse_from_rfc3339(&receipt.started_at).map_err(|_| "mcp_handshake_invalid")?;
    let flushed_at =
        DateTime::parse_from_rfc3339(&receipt.flushed_at).map_err(|_| "mcp_handshake_invalid")?;
    if started_at < spawned_at || flushed_at < started_at {
        return Err("mcp_handshake_stale_spawn");
    }
    let expected_config = workspace
        .join(".team/runtime/mcp")
        .join(format!("{agent_id}.json"));
    if Path::new(config)
        .canonicalize()
        .map_err(|_| "mcp_handshake_config_missing")?
        != expected_config
    {
        return Err("mcp_handshake_config_scope");
    }
    private_metadata(&workspace.join(".team/runtime/mcp"), true)?;
    let config = read_json(Path::new(config), false)?;
    let server_config = config
        .get("team_orchestrator")
        .or_else(|| {
            config
                .get("mcpServers")
                .and_then(|servers| servers.get("team_orchestrator"))
        })
        .ok_or("mcp_handshake_config_missing")?;
    let expected = server_config
        .get("command")
        .and_then(Value::as_str)
        .ok_or("mcp_handshake_config_missing")?;
    if !Path::new(expected).is_absolute()
        || Path::new(expected)
            .canonicalize()
            .map_err(|_| "mcp_handshake_image")?
            != receipt.server.identity.executable
    {
        return Err("mcp_handshake_image");
    }
    let env = server_config
        .get("env")
        .ok_or("mcp_handshake_config_missing")?;
    if env.get("TEAM_AGENT_ID").and_then(Value::as_str) != Some(agent_id)
        || env.get("TEAM_AGENT_OWNER_TEAM_ID").and_then(Value::as_str) != Some(owner)
        || env
            .get("TEAM_AGENT_WORKSPACE")
            .and_then(Value::as_str)
            .and_then(|s| Path::new(s).canonicalize().ok())
            .as_ref()
            != Some(&workspace)
    {
        return Err("mcp_handshake_config_scope");
    }
    let carrier = agent
        .get("pane_pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or("mcp_handshake_carrier_missing")?;
    let pane_id = agent
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("mcp_handshake_carrier_missing")?;
    let carrier_birth =
        process_birth_identity(carrier).map_err(|_| "mcp_handshake_carrier_missing")?;
    if sample_process(&receipt.server) != ProcessState::Alive {
        return Err("mcp_handshake_server_not_live");
    }
    let mut parent = receipt.server.parent;
    let mut matched = false;
    for ancestor in receipt.ancestors.iter().take(MAX_NATIVE_PARENT_HOPS) {
        if ancestor.pid != parent
            || process_birth_identity(parent).ok().as_deref() != Some(ancestor.birth.as_str())
        {
            return Err("mcp_handshake_ancestry");
        }
        if parent == carrier {
            matched = true;
            break;
        }
        parent = crate::platform::argv::parent_pid(parent).unwrap_or(0);
    }
    if !matched || sample_process(&receipt.server) != ProcessState::Alive {
        return Err("mcp_handshake_ancestry");
    }
    Ok(Some(Admission {
        server: receipt.server,
        carrier,
        carrier_birth,
        pane_id: pane_id.to_string(),
    }))
}

/// Recheck against the actual resolved target immediately before the input path.
pub(crate) fn revalidate(admission: &Admission, target: &Target, panes: &[PaneInfo]) -> Result<()> {
    let Target::Pane(id) = target else {
        return Err("mcp_handshake_carrier_missing");
    };
    let pane = panes
        .iter()
        .find(|pane| &pane.pane_id == id)
        .ok_or("mcp_handshake_carrier_missing")?;
    if id.as_str() != admission.pane_id
        || pane.pane_pid != Some(admission.carrier)
        || process_birth_identity(admission.carrier).ok().as_deref()
            != Some(admission.carrier_birth.as_str())
        || sample_process(&admission.server) != ProcessState::Alive
    {
        return Err("mcp_handshake_carrier_mismatch");
    }
    Ok(())
}

// Test construction stays in the MCP process-evidence owner, not messaging.
#[cfg(test)]
pub(crate) fn test_carrier_pid() -> Option<u32> {
    crate::platform::process::current_parent_pid()
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
