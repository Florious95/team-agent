//! Stable per-seat primary cwd. The project stays an additional directory;
//! private MCP files never overwrite a shared project or another seat's config.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::{session::CwdIdentity, types::SeatId};
use team_agent_contract::host::process::resolve_cwd;

use super::backend::{root_for, BackendError, Binding};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CwdRecord {
    version: u32,
    seat: SeatId,
    cwd: CwdIdentity,
}

/// Requires an already owned framework root. Existing unrecorded directories
/// are refused, not adopted. A replaced inode or any symlink is a hard error.
pub(super) fn prepare(binding: &Binding, seat: &SeatId) -> Result<CwdIdentity, BackendError> {
    let root = root_for(&binding.workspace.path, &binding.scope);
    validate_directory(&root)?;
    let parent = root.join("workspaces");
    let path = parent.join(seat.as_str());
    let record_path = root.join(format!("cwd-{}.json", seat.as_str()));
    match std::fs::symlink_metadata(&record_path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(BackendError::Metadata);
            }
            let mut file = std::fs::OpenOptions::new().read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&record_path)?;
            let opened = file.metadata()?;
            if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
                return Err(BackendError::Metadata);
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file).take(65537).read_to_end(&mut bytes)?;
            if bytes.len() > 65536 {
                return Err(BackendError::Metadata);
            }
            let record: CwdRecord = serde_json::from_slice(&bytes).map_err(|_| BackendError::Metadata)?;
            if record.version != 1 || record.seat != *seat || record.cwd.path != path {
                return Err(BackendError::Metadata);
            }
            validate_directory(&parent)?;
            validate_directory(&path)?;
            if resolve_cwd(&path)? != record.cwd {
                return Err(BackendError::Metadata);
            }
            return Ok(record.cwd);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match std::fs::symlink_metadata(&parent) {
        Ok(_) => validate_directory(&parent)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(&parent)?;
        }
        Err(error) => return Err(error.into()),
    }
    // Exclusive create is deliberate: missing record + existing path is not
    // enough evidence that this framework owns the previous incarnation.
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    validate_directory(&path)?;
    let cwd = resolve_cwd(&path)?;
    let record = CwdRecord { version: 1, seat: seat.clone(), cwd: cwd.clone() };
    let bytes = serde_json::to_vec(&record).map_err(|_| BackendError::Metadata)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(record_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(cwd)
}

fn validate_directory(path: &Path) -> Result<(), BackendError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || path.canonicalize()? != path
        || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0
    {
        return Err(BackendError::Metadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use team_agent_contract::contract::types::{Digest, ScopeId};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Fixture { path: PathBuf, binding: Binding }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "contract-cwd-{}-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            let binding = Binding {
                version: 1, workspace: resolve_cwd(&path).unwrap(), team: "test".into(),
                scope: ScopeId::new("cwd-test").unwrap(), endpoint: "framework:cwd-test".into(),
                candidate: "/controlled/candidate".into(), candidate_sha256: Digest([1; 32]),
                tmux: "/controlled/tmux".into(), transport_root: path.join("transport"),
            };
            let root = root_for(&path, &binding.scope);
            std::fs::create_dir_all(root.parent().unwrap()).unwrap();
            std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            Self { path, binding }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.path); }
    }

    #[test]
    fn stable_cwd_is_reused_and_distinct_seats_do_not_share_a_primary() {
        let f = Fixture::new();
        let a = SeatId::new("a").unwrap();
        let b = SeatId::new("b").unwrap();
        let first = prepare(&f.binding, &a).unwrap();
        assert_eq!(prepare(&f.binding, &a).unwrap(), first);
        let second = prepare(&f.binding, &b).unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.identity, second.identity);
        assert!(!f.path.join(".cursor").exists());
    }

    #[test]
    fn replaced_inode_or_symlink_is_not_adopted_on_resume() {
        let f = Fixture::new();
        let seat = SeatId::new("worker").unwrap();
        let original = prepare(&f.binding, &seat).unwrap();
        let retained = original.path.with_extension("old");
        std::fs::rename(&original.path, &retained).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(&original.path).unwrap();
        assert!(prepare(&f.binding, &seat).is_err());
        std::fs::remove_dir(&original.path).unwrap();
        std::os::unix::fs::symlink(&retained, &original.path).unwrap();
        assert!(prepare(&f.binding, &seat).is_err());
    }

    #[test]
    fn unrecorded_existing_directory_is_not_claimed() {
        let f = Fixture::new();
        let root = root_for(&f.path, &f.binding.scope);
        let parent = root.join("workspaces");
        std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(parent.join("worker")).unwrap();
        assert!(prepare(&f.binding, &SeatId::new("worker").unwrap()).is_err());
        assert!(!root.join("cwd-worker.json").exists());
    }
}
