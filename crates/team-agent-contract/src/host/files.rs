use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{digest, HostError, HostErrorKind};
use crate::contract::plan::ResourceWriteEffect;
use crate::contract::types::{require_absolute, Digest, InstanceIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryReceipt {
    pub path: PathBuf,
    pub owner: InstanceIdentity,
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReceipt {
    pub directory: DirectoryReceipt,
    pub name: String,
    pub effect: ResourceWriteEffect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFailure {
    pub error: HostError,
    pub possible: Option<FileReceipt>,
}

/// A pinned private directory descriptor, not authority inferred from a caller's filename.
pub struct ScopedDirectory {
    receipt: DirectoryReceipt,
    directory: File,
}

fn name_valid(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<File, HostError> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|e| HostError::io("open owned directory", e.into()))
}

impl ScopedDirectory {
    pub fn create(path: &Path, owner: InstanceIdentity) -> Result<Self, HostError> {
        require_absolute(path, "owned directory")
            .map_err(|_| HostError::new("owned directory", HostErrorKind::Invalid))?;
        #[cfg(not(unix))]
        {
            let _ = owner;
            return Err(HostError::new(
                "owned directory",
                HostErrorKind::Unsupported,
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            let parent = path
                .parent()
                .ok_or_else(|| HostError::new("owned parent", HostErrorKind::Invalid))?;
            let parent = parent
                .canonicalize()
                .map_err(|e| HostError::io("owned parent", e))?;
            let leaf = path
                .file_name()
                .ok_or_else(|| HostError::new("owned leaf", HostErrorKind::Invalid))?;
            let canonical = parent.join(leaf);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&canonical)
                .map_err(|e| HostError::io("create private directory", e))?;
            let directory = open_directory(&canonical)?;
            let metadata = directory
                .metadata()
                .map_err(|e| HostError::io("owned directory metadata", e))?;
            let receipt = DirectoryReceipt {
                path: canonical,
                owner,
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            let scope = Self { receipt, directory };
            scope.check_live()?;
            Ok(scope)
        }
    }

    /// Receipt must come from the supervisor's captured state, not provider/user arguments.
    pub fn reopen(receipt: DirectoryReceipt) -> Result<Self, HostError> {
        #[cfg(not(unix))]
        {
            let _ = receipt;
            Err(HostError::new(
                "reopen owned directory",
                HostErrorKind::Unsupported,
            ))
        }
        #[cfg(unix)]
        {
            require_absolute(&receipt.path, "owned directory")
                .map_err(|_| HostError::new("owned directory", HostErrorKind::Invalid))?;
            let directory = open_directory(&receipt.path)?;
            let scope = Self { receipt, directory };
            scope.check_live()?;
            Ok(scope)
        }
    }

    pub fn receipt(&self) -> &DirectoryReceipt {
        &self.receipt
    }
    pub fn path(&self) -> &Path {
        &self.receipt.path
    }

    pub fn check_live(&self) -> Result<(), HostError> {
        #[cfg(not(unix))]
        {
            Err(HostError::new(
                "owned directory identity",
                HostErrorKind::Unsupported,
            ))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let fd = self
                .directory
                .metadata()
                .map_err(|e| HostError::io("owned directory metadata", e))?;
            let path = std::fs::symlink_metadata(&self.receipt.path)
                .map_err(|e| HostError::io("owned directory path", e))?;
            let uid = nix::unistd::geteuid().as_raw();
            if !fd.is_dir()
                || !path.is_dir()
                || path.file_type().is_symlink()
                || fd.dev() != self.receipt.device
                || fd.ino() != self.receipt.inode
                || path.dev() != fd.dev()
                || path.ino() != fd.ino()
                || fd.uid() != uid
                || path.uid() != uid
                || fd.mode() & 0o077 != 0
            {
                return Err(HostError::new(
                    "owned directory identity",
                    HostErrorKind::Ownership,
                ));
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    fn open_file(&self, name: &str, flags: nix::fcntl::OFlag) -> Result<File, HostError> {
        use nix::fcntl::{openat, OFlag};
        use nix::sys::stat::Mode;
        use std::os::unix::fs::MetadataExt;
        self.check_live()?;
        if !name_valid(name) {
            return Err(HostError::new("owned filename", HostErrorKind::Invalid));
        }
        let file: File = openat(
            &self.directory,
            name,
            flags | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map(File::from)
        .map_err(|e| HostError::io("open owned file", e.into()))?;
        let metadata = file
            .metadata()
            .map_err(|e| HostError::io("owned file metadata", e))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(HostError::new(
                "owned file identity",
                HostErrorKind::Ownership,
            ));
        }
        Ok(file)
    }

    pub fn create_file(&self, name: &str, bytes: &[u8]) -> Result<FileReceipt, FileFailure> {
        let possible = FileReceipt {
            directory: self.receipt.clone(),
            name: name.to_string(),
            effect: ResourceWriteEffect::MayHaveWritten,
        };
        if !name_valid(name) {
            return Err(FileFailure {
                error: HostError::new("owned filename", HostErrorKind::Invalid),
                possible: None,
            });
        }
        #[cfg(not(unix))]
        {
            let _ = bytes;
            Err(FileFailure {
                error: HostError::new("create owned file", HostErrorKind::Unsupported),
                possible: None,
            })
        }
        #[cfg(unix)]
        {
            use nix::fcntl::OFlag;
            let mut file = self
                .open_file(name, OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL)
                .map_err(|error| FileFailure {
                    possible: if matches!(
                        error.kind,
                        HostErrorKind::Io(std::io::ErrorKind::AlreadyExists)
                    ) {
                        None
                    } else {
                        Some(possible.clone())
                    },
                    error,
                })?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .and_then(|_| self.directory.sync_all())
                .map_err(|e| FileFailure {
                    error: HostError::io("write owned file", e),
                    possible: Some(possible.clone()),
                })?;
            Ok(FileReceipt {
                effect: ResourceWriteEffect::Written {
                    bytes_sha256: digest(bytes),
                },
                ..possible
            })
        }
    }

    pub fn read_file(&self, name: &str, max_bytes: usize) -> Result<Vec<u8>, HostError> {
        if max_bytes == 0 {
            return Err(HostError::new("owned read limit", HostErrorKind::Invalid));
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(HostError::new(
                "read owned file",
                HostErrorKind::Unsupported,
            ))
        }
        #[cfg(unix)]
        {
            let file = self.open_file(name, nix::fcntl::OFlag::O_RDONLY)?;
            let mut bytes = Vec::new();
            file.take(max_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| HostError::io("read owned file", e))?;
            if bytes.len() > max_bytes {
                return Err(HostError::new("owned read limit", HostErrorKind::Invalid));
            }
            Ok(bytes)
        }
    }

    pub fn open_append(&self, name: &str) -> Result<File, HostError> {
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(HostError::new(
                "append owned file",
                HostErrorKind::Unsupported,
            ))
        }
        #[cfg(unix)]
        {
            self.open_file(
                name,
                nix::fcntl::OFlag::O_WRONLY | nix::fcntl::OFlag::O_APPEND,
            )
        }
    }

    /// The same lock must be used by delivery, native control, session inspection and stop.
    /// Contention defers; there is no warning-and-continue or stale-file lock takeover.
    pub fn try_lane(&self) -> Result<NativeLane, HostError> {
        #[cfg(not(unix))]
        {
            Err(HostError::new("native lane", HostErrorKind::Unsupported))
        }
        #[cfg(unix)]
        {
            use nix::fcntl::OFlag;
            let file = self.open_file("native-input.lock", OFlag::O_RDWR | OFlag::O_CREAT)?;
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    HostError::new("native lane", HostErrorKind::Conflict)
                }
                std::fs::TryLockError::Error(error) => HostError::io("native lane", error),
            })?;
            Ok(NativeLane {
                _file: file,
                directory: self.receipt.clone(),
            })
        }
    }

    /// Compare-own-bytes cleanup for quiescent host artifacts only. The lifecycle must
    /// exclude writers; this is not an atomic CAS against arbitrary external writers.
    /// Never accepts sockets/directories or an uncertain write receipt.
    pub fn remove_file(&self, receipt: &FileReceipt, lane: &NativeLane) -> Result<(), HostError> {
        if lane.directory() != &self.receipt {
            return Err(HostError::new(
                "file cleanup lane",
                HostErrorKind::Ownership,
            ));
        }
        if receipt.directory != self.receipt {
            return Err(HostError::new(
                "file receipt owner",
                HostErrorKind::Ownership,
            ));
        }
        let ResourceWriteEffect::Written { bytes_sha256 } = receipt.effect else {
            return Err(HostError::new(
                "unverified file bytes",
                HostErrorKind::Unknown,
            ));
        };
        let bytes = self.read_file(&receipt.name, 16 * 1024 * 1024)?;
        if digest(&bytes) != bytes_sha256 {
            return Err(HostError::new("dirty owned file", HostErrorKind::Conflict));
        }
        #[cfg(not(unix))]
        {
            Err(HostError::new(
                "remove owned file",
                HostErrorKind::Unsupported,
            ))
        }
        #[cfg(unix)]
        {
            use nix::unistd::{unlinkat, UnlinkatFlags};
            unlinkat(
                &self.directory,
                receipt.name.as_str(),
                UnlinkatFlags::NoRemoveDir,
            )
            .map_err(|e| HostError::io("remove owned file", e.into()))?;
            self.directory
                .sync_all()
                .map_err(|e| HostError::io("sync owned directory", e))
        }
    }
}

/// Read a declared materialized file through pinned, no-follow component descriptors.
/// Its grant/provenance must already be checked against the launch and H3 receipt.
pub fn fingerprint_materialized(
    path: &crate::contract::types::OwnedPath,
    max_bytes: u64,
    budget: std::time::Duration,
) -> Result<Digest, HostError> {
    #[cfg(not(unix))]
    {
        let _ = (path, max_bytes, budget);
        Err(HostError::new(
            "materialization fingerprint",
            HostErrorKind::Unsupported,
        ))
    }
    #[cfg(unix)]
    {
        use nix::fcntl::{openat, OFlag};
        use nix::sys::stat::Mode;
        use std::path::Component;
        let started = std::time::Instant::now();
        if path
            .root()
            .canonicalize()
            .map_err(|e| HostError::io("materialization root", e))?
            != path.root()
        {
            return Err(HostError::new(
                "materialization root alias",
                HostErrorKind::Ownership,
            ));
        }
        let mut directory = open_directory(path.root())?;
        let parts: Vec<_> = path.relative().components().collect();
        for (index, component) in parts.iter().enumerate() {
            let Component::Normal(name) = component else {
                return Err(HostError::new(
                    "materialization path",
                    HostErrorKind::Invalid,
                ));
            };
            let last = index + 1 == parts.len();
            let flags = OFlag::O_RDONLY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | if last {
                    OFlag::empty()
                } else {
                    OFlag::O_DIRECTORY
                };
            let mut file = File::from(
                openat(&directory, *name, flags, Mode::empty())
                    .map_err(|e| HostError::io("materialization open", e.into()))?,
            );
            if last {
                return super::process::fingerprint_open_file(
                    &mut file,
                    max_bytes,
                    budget.saturating_sub(started.elapsed()),
                );
            }
            directory = file;
        }
        Err(HostError::new(
            "materialization path",
            HostErrorKind::Invalid,
        ))
    }
}

pub struct NativeLane {
    _file: File,
    directory: DirectoryReceipt,
}
impl NativeLane {
    pub fn owner(&self) -> &InstanceIdentity {
        &self.directory.owner
    }
    pub fn directory(&self) -> &DirectoryReceipt {
        &self.directory
    }
}

pub fn written_digest(receipt: &FileReceipt) -> Option<Digest> {
    match receipt.effect {
        ResourceWriteEffect::Written { bytes_sha256 } => Some(bytes_sha256),
        ResourceWriteEffect::MayHaveWritten => None,
    }
}
