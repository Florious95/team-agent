//! Framework-owned process CWD policy. Pi, Claude, Codex and Grok use the
//! canonical workspace for fresh sessions and preserve the owned CWD on resume.
//! Cursor retains private isolation; Kiro's existing policy is unchanged.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::{session::CwdIdentity, types::SeatId};
use team_agent_contract::host::process::{cwd_matches, resolve_cwd, stable_cwd};

use super::backend::{root_for, BackendError, Binding};
use super::registry::NativeProvider;

/// Shared by fresh, resume and fork. Provider adapters cannot choose a different
/// process directory; the physical executor checks their plan against this value.
pub(super) fn bind_cwd(
    binding: &Binding,
    provider: NativeProvider,
    seat: &SeatId,
    previous: Option<&CwdIdentity>,
) -> Result<CwdIdentity, BackendError> {
    match provider {
        NativeProvider::Pi | NativeProvider::Claude | NativeProvider::Codex | NativeProvider::GrokBuild =>
            current_cwd(binding, previous.unwrap_or(&binding.workspace)),
        // Explicit release boundary: preserve these providers' existing config isolation.
        NativeProvider::Kiro => current_cwd(binding, previous.unwrap_or(&binding.workspace)),
        NativeProvider::Cursor => match previous {
            Some(cwd) => current_cwd(binding, cwd),
            None => prepare(binding, seat),
        },
    }
}

/// Only an adopted binding can vouch for a legacy CWD after device renumbering.
/// New incarnation evidence never falls back to a path-only comparison.
pub(super) fn current_cwd(binding: &Binding, cwd: &CwdIdentity) -> Result<CwdIdentity, BackendError> {
    let current = resolve_cwd(&cwd.path).map_err(|error| BackendError::Continuity(format!("cannot reopen preserved CWD {}: {error}", cwd.path.display())))?;
    if cwd_matches(cwd)? || (!stable_cwd(cwd) && !stable_cwd(&binding.workspace)
        && current.path == cwd.path
        && (cwd.path == binding.workspace.path
            || cwd.path.starts_with(root_for(&binding.workspace.path, &binding.scope).join("workspaces"))
            || cwd.path.starts_with(binding.transport_root.join("workspaces"))))
    { return Ok(current); }
    Err(BackendError::Continuity(format!("preserved CWD incarnation changed: {}", cwd.path.display())))
}

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
    let parent = binding.transport_root.join("workspaces");
    let path = parent.join(seat.as_str());
    let scoped_record = binding.transport_root.join(format!("cwd-{}.json", seat.as_str()));
    let record_path = match std::fs::symlink_metadata(&scoped_record) {
        Ok(_) => scoped_record,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.join(format!("cwd-{}.json", seat.as_str())),
        Err(error) => return Err(error.into()),
    };
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
            // Old sessions stay pinned to their original project-local cwd.
            // Only new allocations move outside Git; never relocate a resume.
            let expected = match record.version {
                1 => root.join("workspaces").join(seat.as_str()),
                2 => {
                    validate_directory(&binding.transport_root)?;
                    reject_git_ancestors(&binding.transport_root)?;
                    path.clone()
                }
                _ => return Err(BackendError::Metadata),
            };
            if record.seat != *seat || record.cwd.path != expected {
                return Err(BackendError::Metadata);
            }
            validate_directory(expected.parent().ok_or(BackendError::Metadata)?)?;
            validate_directory(&expected)?;
            return current_cwd(binding, &record.cwd);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    validate_directory(&binding.transport_root)?;
    reject_git_ancestors(&binding.transport_root)?;
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
    let record = CwdRecord { version: 2, seat: seat.clone(), cwd: cwd.clone() };
    let bytes = serde_json::to_vec(&record).map_err(|_| BackendError::Metadata)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(record_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(cwd)
}

// Metadata only: handle both ordinary .git directories and worktree .git files;
// never read Git config, credentials, execute hooks, or assume /tmp is non-Git.
fn reject_git_ancestors(path: &Path) -> Result<(), BackendError> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Err(BackendError::Metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
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
                tmux: "/controlled/tmux".into(), transport_root: path.with_extension("transport"),
            };
            std::fs::DirBuilder::new().mode(0o700).create(&binding.transport_root).unwrap();
            let root = root_for(&path, &binding.scope);
            std::fs::create_dir_all(root.parent().unwrap()).unwrap();
            std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            Self { path, binding }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = std::fs::remove_dir_all(&self.binding.transport_root);
        }
    }

    #[test]
    fn project_providers_use_canonical_fresh_cwd_and_preserve_owned_resume_cwd() {
        let f = Fixture::new();
        std::fs::create_dir(f.path.join(".git")).unwrap();
        let private = resolve_cwd(&f.binding.transport_root).unwrap();
        for provider in [NativeProvider::Pi, NativeProvider::Claude, NativeProvider::Codex, NativeProvider::GrokBuild] {
            for seat in ["worker", "fork-child"] {
                let seat = SeatId::new(seat).unwrap();
                assert_eq!(bind_cwd(&f.binding, provider, &seat, None).unwrap(), f.binding.workspace);
                assert_eq!(bind_cwd(&f.binding, provider, &seat, Some(&f.binding.workspace)).unwrap(), f.binding.workspace);
                assert_eq!(bind_cwd(&f.binding, provider, &seat, Some(&private)).unwrap(), private);
            }
        }
        assert!(!f.binding.transport_root.join("workspaces").exists());
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
        let parent = f.binding.transport_root.join("workspaces");
        std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(parent.join("worker")).unwrap();
        assert!(prepare(&f.binding, &SeatId::new("worker").unwrap()).is_err());
        assert!(!root.join("cwd-worker.json").exists());
    }

    #[test]
    fn new_primary_is_outside_project_git_ancestors() {
        let f = Fixture::new();
        std::fs::create_dir(f.path.join(".git")).unwrap();
        let cwd = prepare(&f.binding, &SeatId::new("worker").unwrap()).unwrap();
        assert!(cwd.path.starts_with(&f.binding.transport_root));
        assert!(!cwd.path.starts_with(&f.path));
        reject_git_ancestors(&cwd.path).unwrap();
    }

    #[test]
    fn existing_version_one_cwd_is_not_migrated() {
        let f = Fixture::new();
        let seat = SeatId::new("worker").unwrap();
        let root = root_for(&f.path, &f.binding.scope);
        let parent = root.join("workspaces");
        let path = parent.join(seat.as_str());
        std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let cwd = resolve_cwd(&path).unwrap();
        let record = CwdRecord { version: 1, seat: seat.clone(), cwd: cwd.clone() };
        std::fs::write(root.join("cwd-worker.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(prepare(&f.binding, &seat).unwrap(), cwd);
        assert!(!f.binding.transport_root.join("workspaces").exists());
    }

    #[test]
    fn legacy_resume_cwd_requires_an_adopted_binding_and_keeps_incarnation_checks() {
        let mut f = Fixture::new();
        let current = resolve_cwd(&f.path).unwrap();
        let mut legacy = team_agent_contract::host::process::legacy_cwd(&f.path).unwrap();
        // Simulate the opaque digest recorded on an earlier boot. Its original
        // device/inode cannot be recovered from v1 JSON.
        legacy.identity.0[0] ^= 2;
        assert!(!cwd_matches(&legacy).unwrap());
        assert!(current_cwd(&f.binding, &legacy).is_err());
        f.binding.workspace = legacy.clone();
        let seat = SeatId::new("worker").unwrap();
        assert_eq!(bind_cwd(&f.binding, NativeProvider::Pi, &seat, Some(&legacy)).unwrap(), current);
        assert_eq!(bind_cwd(&f.binding, NativeProvider::Claude, &seat, Some(&legacy)).unwrap(), current);
        // Adoption is not permission to bypass a new, recoverable incarnation.
        let mut changed = current.clone();
        changed.identity.0[8] ^= 1;
        assert!(current_cwd(&f.binding, &changed).is_err());
        let foreign = resolve_cwd(f.path.parent().unwrap()).unwrap();
        let mut foreign_legacy = team_agent_contract::host::process::legacy_cwd(&foreign.path).unwrap();
        foreign_legacy.identity.0[0] ^= 2;
        assert!(current_cwd(&f.binding, &foreign_legacy).is_err());
    }

    #[test]
    fn legacy_owned_cwd_record_survives_device_renumbering() {
        let mut f = Fixture::new();
        f.binding.workspace = team_agent_contract::host::process::legacy_cwd(&f.path).unwrap();
        let seat = SeatId::new("worker").unwrap();
        let current = prepare(&f.binding, &seat).unwrap();
        let mut cwd = team_agent_contract::host::process::legacy_cwd(&current.path).unwrap();
        cwd.identity.0[0] ^= 2;
        let record = CwdRecord { version: 2, seat: seat.clone(), cwd };
        std::fs::write(root_for(&f.path, &f.binding.scope).join("cwd-worker.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(prepare(&f.binding, &seat).unwrap(), current);
    }

    #[test]
    fn git_directory_or_worktree_file_on_transport_ancestor_is_refused() {
        let f = Fixture::new();
        let marker = f.binding.transport_root.join(".git");
        std::fs::write(&marker, b"gitdir: controlled-marker").unwrap();
        assert!(prepare(&f.binding, &SeatId::new("worker").unwrap()).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::fs::create_dir(&marker).unwrap();
        assert!(prepare(&f.binding, &SeatId::new("worker").unwrap()).is_err());
    }
}
