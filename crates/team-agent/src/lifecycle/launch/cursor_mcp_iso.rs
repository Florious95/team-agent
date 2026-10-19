//! Cursor's native MCP project root must be an owned primary outside Git ancestry.
//! Keep the real project as --add-dir; never overwrite its shared MCP config or
//! silently relocate an existing native session. The persisted directory receipt
//! is transport ownership, not a canonical Provider/native-session identity.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::types::{
    Generation, InstanceId, InstanceIdentity, ScopeId, SeatId,
};
use team_agent_contract::host::files::{DirectoryReceipt, ScopedDirectory};
use team_agent_contract::host::{digest, digest_hex};

use crate::lifecycle::LifecycleError;

const ISOLATION_ENV: &str = "TEAM_AGENT_CURSOR_MCP_ISOLATION";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRecord {
    workspace: PathBuf,
    agent_id: String,
    owner_team_id: String,
    directory: DirectoryReceipt,
}

pub fn cursor_mcp_isolation_enabled() -> bool {
    match std::env::var(ISOLATION_ENV) {
        Ok(value) => !matches!(
            value.trim(),
            "0" | "false" | "FALSE" | "off" | "OFF" | "no" | "NO"
        ),
        Err(_) => true,
    }
}

fn failure(reason: impl std::fmt::Display) -> LifecycleError {
    LifecycleError::RequirementUnmet(format!(
        "cursor MCP private project refused: {reason}; do not overwrite shared .cursor/mcp.json or move an existing session"
    ))
}

fn record_path(workspace: &Path, agent_id: &str) -> PathBuf {
    workspace
        .join(".team/runtime/provider-config")
        .join(agent_id)
        .join("cursor-project.json")
}

fn project_key(workspace: &Path, agent_id: &str) -> String {
    digest_hex(digest(
        format!("{}\0{agent_id}", workspace.display()).as_bytes(),
    ))
}

fn planned_project(workspace: &Path, agent_id: &str) -> Result<PathBuf, LifecycleError> {
    // Do not assume TMPDIR is outside Git, or use an unchecked symlink alias.
    let parent = std::env::temp_dir().canonicalize().map_err(failure)?;
    validate_cursor_mcp_project_root(&parent)?;
    Ok(parent.join(format!(
        "ta-cursor-{}",
        &project_key(workspace, agent_id)[..32]
    )))
}

/// Computes a new path without creating it; existing allocations must reopen
/// their captured receipt, never adopt a replacement directory or a new TMPDIR.
pub fn cursor_mcp_project_dir(workspace: &Path, agent_id: &str) -> Result<PathBuf, LifecycleError> {
    validate_agent_id(agent_id)?;
    let workspace = workspace.canonicalize().map_err(failure)?;
    match read_record(&workspace, agent_id)? {
        Some(record) => Ok(reopen(&record)?.path().to_path_buf()),
        None => planned_project(&workspace, agent_id),
    }
}

pub fn cursor_mcp_json_path(workspace: &Path, agent_id: &str) -> Result<PathBuf, LifecycleError> {
    Ok(cursor_mcp_project_dir(workspace, agent_id)?.join(".cursor/mcp.json"))
}

/// Metadata-only mirror of the measured native project-root boundary. A .git
/// directory OR worktree file on any ancestor would redirect native MCP loading.
pub(super) fn validate_cursor_mcp_project_root(project: &Path) -> Result<(), LifecycleError> {
    if project.canonicalize().map_err(failure)? != project {
        return Err(failure("project path is not physical"));
    }
    for ancestor in project.ancestors() {
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {
                return Err(failure(
                    "Git ancestor would redirect native MCP projectRoot",
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failure(e)),
        }
    }
    Ok(())
}

pub fn materialize_cursor_mcp_project(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: &str,
) -> Result<PathBuf, LifecycleError> {
    validate_agent_id(agent_id)?;
    let workspace = workspace.canonicalize().map_err(failure)?;
    if owner_team_id.trim().is_empty() {
        return Err(failure("missing owner team"));
    }
    if let Some(record) = read_record(&workspace, agent_id)? {
        if record.owner_team_id != owner_team_id {
            return Err(failure("record belongs to another owner team"));
        }
        return Ok(reopen(&record)?.path().to_path_buf());
    }
    let legacy = workspace
        .join(".team/runtime/provider-config")
        .join(agent_id)
        .join("cursor");
    match std::fs::symlink_metadata(&legacy) {
        Ok(_) => return Err(failure("unrecorded legacy primary exists; preserve its cwd/SID and explicitly select a fresh seat")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failure(e)),
    }
    let path = planned_project(&workspace, agent_id)?;
    let key = project_key(&workspace, agent_id);
    let mut nonce = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut nonce))
        .map_err(failure)?;
    let owner = InstanceIdentity {
        scope: ScopeId::new(format!("legacy-cursor-{}", &key[..32])).map_err(failure)?,
        seat: SeatId::new(format!("seat-{}", &key[32..])).map_err(failure)?,
        instance: InstanceId::new(format!("cwd-{}", digest_hex(digest(&nonce))))
            .map_err(failure)?,
        generation: Generation(1),
    };
    let directory = ScopedDirectory::create(&path, owner).map_err(failure)?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path.join(".cursor"))
        .map_err(failure)?;
    link_workspace_rules(&workspace, &path.join(".cursor"))?;
    let record = ProjectRecord {
        workspace: workspace.clone(),
        agent_id: agent_id.into(),
        owner_team_id: owner_team_id.into(),
        directory: directory.receipt().clone(),
    };
    let record_path = record_path(&workspace, agent_id);
    let parent = record_path
        .parent()
        .ok_or_else(|| failure("missing record parent"))?;
    std::fs::create_dir_all(parent).map_err(failure)?;
    if parent.canonicalize().map_err(failure)? != parent {
        return Err(failure("record parent is a symlink"));
    }
    let bytes = serde_json::to_vec(&record).map_err(failure)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(record_path)
        .map_err(failure)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(failure)?;
    directory.check_live().map_err(failure)?;
    validate_cursor_mcp_project_root(&path)?;
    Ok(path)
}

fn read_record(workspace: &Path, agent_id: &str) -> Result<Option<ProjectRecord>, LifecycleError> {
    let path = record_path(workspace, agent_id);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failure(e)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || path.parent().and_then(|p| p.canonicalize().ok()).as_deref() != path.parent()
    {
        return Err(failure("unowned or aliased project record"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(failure)?;
    let opened = file.metadata().map_err(failure)?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(failure("project record replaced"));
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes).map_err(failure)?;
    if bytes.len() > 65536 {
        return Err(failure("oversized project record"));
    }
    let record: ProjectRecord = serde_json::from_slice(&bytes).map_err(failure)?;
    if record.workspace != workspace
        || record.agent_id != agent_id
        || record.owner_team_id.trim().is_empty()
    {
        return Err(failure("project record identity mismatch"));
    }
    Ok(Some(record))
}

fn reopen(record: &ProjectRecord) -> Result<ScopedDirectory, LifecycleError> {
    let key = project_key(&record.workspace, &record.agent_id);
    if record.directory.path.file_name().and_then(|n| n.to_str())
        != Some(format!("ta-cursor-{}", &key[..32]).as_str())
        || record.directory.owner.scope.as_str() != format!("legacy-cursor-{}", &key[..32])
        || record.directory.owner.seat.as_str() != format!("seat-{}", &key[32..])
        || record.directory.owner.generation != Generation(1)
    {
        return Err(failure("directory receipt identity mismatch"));
    }
    let directory = ScopedDirectory::reopen(record.directory.clone()).map_err(failure)?;
    validate_cursor_mcp_project_root(directory.path())?;
    let cursor = directory.path().join(".cursor");
    let metadata = std::fs::symlink_metadata(&cursor).map_err(failure)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(failure("unowned .cursor directory"));
    }
    Ok(directory)
}

fn validate_agent_id(agent_id: &str) -> Result<(), LifecycleError> {
    if agent_id.is_empty()
        || agent_id.contains("..")
        || !agent_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(failure(
            "agent id must be a single path segment [A-Za-z0-9._-]+",
        ));
    }
    Ok(())
}

fn link_workspace_rules(workspace: &Path, dest_cursor: &Path) -> Result<(), LifecycleError> {
    let source = workspace.join(".cursor/rules");
    if source.exists() {
        std::os::unix::fs::symlink(source, dest_cursor.join("rules")).map_err(failure)?;
    }
    Ok(())
}
