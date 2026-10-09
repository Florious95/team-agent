//! Descriptor-scoped owned I/O for runtime-root and working-directory resources.
//! Existing cwd/.kiro directories are never chmod'ed, adopted as private runtime
//! directories, or recursively removed. Every path component is opened no-follow.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, PathBuf};

use super::command::*;
use super::process::{fingerprint_file, resolve_cwd};
use super::{digest, HostError, HostErrorKind};
use crate::contract::{descriptor::*, hooks::*, plan::*, session::*, types::*};

pub struct ScopedMaterializer<R> {
    requests: Vec<OwnedResourceRequest>,
    roots: BTreeMap<PathBuf, CwdIdentity>,
    policies: Vec<ResourcePolicy>,
    operation: OperationId,
    executable: PathBuf,
    executable_hash: Digest,
    runner: R,
}

impl<R: CommandRunner> ScopedMaterializer<R> {
    pub fn new(
        descriptor: &ProviderDescriptor,
        resolved: &ResolvedLaunch,
        plan: &LaunchPlan,
        operation: OperationId,
        runner: R,
    ) -> Result<Self, HostError> {
        validate_launch_plan(descriptor, resolved, plan)
            .map_err(|_| HostError::new("materialization plan", HostErrorKind::Ownership))?;
        let mut roots = BTreeMap::new();
        for request in &plan.materialization {
            let captured = resolve_cwd(request.path.root())?;
            if captured.path != request.path.root()
                || (materialization_scope(descriptor, request.kind)
                    == ResourceScope::WorkingDirectory
                    && captured != resolved.request().paths.cwd)
            {
                return Err(HostError::new(
                    "materialization root identity",
                    HostErrorKind::Ownership,
                ));
            }
            roots.insert(captured.path.clone(), captured);
        }
        Ok(Self {
            requests: plan.materialization.clone(),
            roots,
            policies: descriptor.teardown.resources.to_vec(),
            operation,
            executable: plan.executable.clone(),
            executable_hash: resolved.request().native.executable_sha256,
            runner,
        })
    }
    pub fn runner(&self) -> &R {
        &self.runner
    }
}

fn contract_failure(error: HostError, receipt: MaterializeReceipt) -> PartialFailure {
    let _ = error; // Stable error only, never native output or config contents.
    PartialFailure {
        error: ContractError::Invalid("owned filesystem operation failed"),
        receipt,
    }
}

impl<R: CommandRunner> OwnedIo for ScopedMaterializer<R> {
    fn create_exclusive(
        &mut self,
        request: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        let empty = || MaterializeReceipt { resources: vec![] };
        if !self.requests.contains(request) {
            return Err(PartialFailure {
                error: ContractError::Mismatch("captured write grant"),
                receipt: empty(),
            });
        }
        let root = self
            .roots
            .get(request.path.root())
            .ok_or_else(|| PartialFailure {
                error: ContractError::Mismatch("captured root"),
                receipt: empty(),
            })?;
        let disposition = self
            .policies
            .iter()
            .find(|p| p.kind == request.kind)
            .map(|p| p.disposition)
            .ok_or_else(|| PartialFailure {
                error: ContractError::Invalid("resource policy"),
                receipt: empty(),
            })?;
        let mut receipt = OwnedResourceReceipt {
            path: request.path.clone(),
            owner: request.owner.clone(),
            operation: self.operation.clone(),
            kind: request.kind,
            disposition,
            write_effect: ResourceWriteEffect::MayHaveWritten,
            exclusive: false,
            creation_identity: None,
        };
        let (parent, name) =
            open_parent(root, &request.path, true).map_err(|e| contract_failure(e, empty()))?;
        #[cfg(not(unix))]
        {
            let _ = (parent, name);
            Err(contract_failure(
                HostError::new("owned materialization", HostErrorKind::Unsupported),
                empty(),
            ))
        }
        #[cfg(unix)]
        {
            use nix::fcntl::{openat, OFlag};
            use nix::sys::stat::Mode;
            let fd = openat(
                &parent,
                name.as_str(),
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|e| {
                contract_failure(
                    HostError::io("exclusive materialization", e.into()),
                    empty(),
                )
            })?;
            receipt.exclusive = true;
            let mut file = File::from(fd);
            file.write_all(&request.contents)
                .and_then(|_| file.sync_all())
                .and_then(|_| parent.sync_all())
                .map_err(|e| {
                    contract_failure(
                        HostError::io("materialization write", e),
                        MaterializeReceipt {
                            resources: vec![receipt.clone()],
                        },
                    )
                })?;
            receipt.write_effect = ResourceWriteEffect::Written {
                bytes_sha256: digest(&request.contents),
            };
            receipt.creation_identity =
                Some(file_identity(root, &receipt, &file).map_err(|e| {
                    contract_failure(
                        e,
                        MaterializeReceipt {
                            resources: vec![receipt.clone()],
                        },
                    )
                })?);
            // Catch a renamed/replaced ancestor before claiming a path-bound success.
            let (current_parent, _) = open_parent(root, &request.path, false).map_err(|e| {
                contract_failure(
                    e,
                    MaterializeReceipt {
                        resources: vec![receipt.clone()],
                    },
                )
            })?;
            if !same_directory(&parent, &current_parent).map_err(|e| {
                contract_failure(
                    e,
                    MaterializeReceipt {
                        resources: vec![receipt.clone()],
                    },
                )
            })? {
                return Err(contract_failure(
                    HostError::new(
                        "materialization ancestor replaced",
                        HostErrorKind::Ownership,
                    ),
                    MaterializeReceipt {
                        resources: vec![receipt],
                    },
                ));
            }
            Ok(receipt)
        }
    }
    fn read_bound_session(
        &mut self,
        _: &ResumeBinding,
        _: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Unknown(Reason {
            code: "native-backing-not-granted",
            message: "No session database or unverified backing read is authorized",
        }))
    }
    fn validate_configuration(
        &mut self,
        request: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure> {
        let started = std::time::Instant::now();
        request.bounds.validate().map_err(ReadFailure::Error)?;
        if request.validator_id != "kiro-agent-validate"
            || request.resource.operation != self.operation
            || !self.requests.iter().any(|r| {
                r.path == request.resource.path
                    && r.owner == request.resource.owner
                    && r.kind == request.resource.kind
            })
        {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "configuration validation grant",
            )));
        }
        let root = self
            .roots
            .get(request.resource.path.root())
            .ok_or(ReadFailure::Error(ContractError::Mismatch(
                "configuration root",
            )))?;
        if !matches_current_file(root, &request.resource)
            .map_err(|_| ReadFailure::Error(ContractError::Mismatch("configuration identity")))?
        {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "configuration bytes",
            )));
        }
        if fingerprint_file(
            &self.executable,
            1024 * 1024 * 1024,
            request.bounds.deadline.saturating_sub(started.elapsed()),
        )
        .map_err(|_| ReadFailure::Error(ContractError::Mismatch("validator image")))?
            != self.executable_hash
        {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "validator image",
            )));
        }
        let budget = request.bounds.deadline.saturating_sub(started.elapsed());
        if budget.is_zero() {
            return Err(ReadFailure::TimedOut {
                elapsed: started.elapsed(),
            });
        }
        let result = self.runner.run(&CommandRequest {
            executable: self.executable.clone(),
            arguments: vec![
                "agent".into(),
                "validate".into(),
                "--path".into(),
                request.resource.path.path().into_os_string(),
            ],
            cwd: Some(root.path.clone()),
            environment: EnvironmentDelta {
                remove: Default::default(),
                set: Default::default(),
            },
            stdin: None,
            reject_stdout: None,
            budget,
            limits: OutputLimits {
                stdout: request.bounds.max_output_bytes,
                stderr: request.bounds.max_output_bytes,
                stdin: 0,
            },
        });
        match result.end {
            CommandEnd::Exited => Ok(ReadOutput {
                stdout: result.stdout,
                elapsed: started.elapsed(),
                exit_code: result.exit_code.unwrap_or(-1),
            }),
            CommandEnd::TimedOut => Err(ReadFailure::TimedOut {
                elapsed: started.elapsed(),
            }),
            CommandEnd::OutputLimit => Err(ReadFailure::OutputLimit {
                limit: request.bounds.max_output_bytes,
            }),
            _ => Err(ReadFailure::Unknown(Reason {
                code: "validator-not-observed",
                message: "Bounded native validator did not produce an exit receipt",
            })),
        }
    }
}

#[cfg(unix)]
fn check_directory(file: &File) -> Result<(), HostError> {
    use std::os::unix::fs::MetadataExt;
    let m = file
        .metadata()
        .map_err(|e| HostError::io("workspace directory", e))?;
    if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o022 != 0 {
        return Err(HostError::new(
            "workspace directory owner/mode",
            HostErrorKind::Ownership,
        ));
    }
    Ok(())
}

fn open_parent(
    root: &CwdIdentity,
    path: &OwnedPath,
    create: bool,
) -> Result<(File, String), HostError> {
    if path.root() != root.path || resolve_cwd(&root.path)? != *root {
        return Err(HostError::new(
            "captured workspace changed",
            HostErrorKind::Ownership,
        ));
    }
    #[cfg(not(unix))]
    {
        let _ = create;
        Err(HostError::new(
            "workspace materialization",
            HostErrorKind::Unsupported,
        ))
    }
    #[cfg(unix)]
    {
        use nix::fcntl::{open, openat, OFlag};
        use nix::sys::stat::{mkdirat, Mode};
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let mut directory = File::from(
            open(&root.path, flags, Mode::empty())
                .map_err(|e| HostError::io("workspace root open", e.into()))?,
        );
        check_directory(&directory)?;
        let mut components = path.relative().components().peekable();
        while let Some(Component::Normal(name)) = components.next() {
            let name = name.to_str().ok_or_else(|| {
                HostError::new("materialization filename utf8", HostErrorKind::Invalid)
            })?;
            if components.peek().is_none() {
                return Ok((directory, name.to_owned()));
            }
            if create {
                match mkdirat(&directory, name, Mode::from_bits_truncate(0o700)) {
                    Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
                    Err(e) => return Err(HostError::io("create config directory", e.into())),
                }
            }
            directory = File::from(
                openat(&directory, name, flags, Mode::empty())
                    .map_err(|e| HostError::io("open config directory", e.into()))?,
            );
            check_directory(&directory)?;
        }
        Err(HostError::new(
            "materialization path",
            HostErrorKind::Invalid,
        ))
    }
}

#[cfg(unix)]
fn same_directory(a: &File, b: &File) -> Result<bool, HostError> {
    use std::os::unix::fs::MetadataExt;
    let a = a
        .metadata()
        .map_err(|e| HostError::io("directory identity", e))?;
    let b = b
        .metadata()
        .map_err(|e| HostError::io("directory identity", e))?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
fn file_identity(
    root: &CwdIdentity,
    receipt: &OwnedResourceReceipt,
    file: &File,
) -> Result<Digest, HostError> {
    #[cfg(not(unix))]
    {
        let _ = (root, receipt, file);
        Err(HostError::new("file identity", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file
            .metadata()
            .map_err(|e| HostError::io("config file metadata", e))?;
        if !m.is_file()
            || m.nlink() != 1
            || m.uid() != nix::unistd::geteuid().as_raw()
            || m.mode() & 0o077 != 0
        {
            return Err(HostError::new(
                "config file ownership",
                HostErrorKind::Ownership,
            ));
        }
        let bytes = serde_json::to_vec(&(
            root,
            &receipt.owner,
            &receipt.operation,
            &receipt.path,
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        ))
        .map_err(|_| HostError::new("file identity encoding", HostErrorKind::Invalid))?;
        Ok(digest(&bytes))
    }
}
fn matches_current_file(
    root: &CwdIdentity,
    receipt: &OwnedResourceReceipt,
) -> Result<bool, HostError> {
    let ResourceWriteEffect::Written { bytes_sha256 } = receipt.write_effect else {
        return Ok(false);
    };
    if !receipt.exclusive || receipt.creation_identity.is_none() {
        return Ok(false);
    }
    let (parent, name) = open_parent(root, &receipt.path, false)?;
    #[cfg(not(unix))]
    {
        let _ = (parent, name, bytes_sha256);
        Ok(false)
    }
    #[cfg(unix)]
    {
        use nix::fcntl::{openat, OFlag};
        use nix::sys::stat::Mode;
        let file = File::from(
            openat(
                &parent,
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| HostError::io("config file open", e.into()))?,
        );
        if Some(file_identity(root, receipt, &file)?) != receipt.creation_identity {
            return Ok(false);
        }
        let mut bytes = vec![];
        (&file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| HostError::io("config file read", e))?;
        Ok(bytes.len() <= 16 * 1024 * 1024
            && digest(&bytes) == bytes_sha256
            && Some(file_identity(root, receipt, &file)?) == receipt.creation_identity)
    }
}

/// Caller must hold the lifecycle lease and have proved the exact owner has no
/// live writer. The captured root comes from that owner's persisted cwd, never
/// from the resource path. External arbitrary concurrent writers are not CAS'ed.
pub fn remove_quiescent(
    root: &CwdIdentity,
    owner: &InstanceIdentity,
    receipt: &OwnedResourceReceipt,
) -> Result<bool, HostError> {
    if &receipt.owner != owner
        || receipt.disposition != ResourceDisposition::OwnedRemovable
        || matches!(
            receipt.kind,
            ResourceKind::SessionBacking
                | ResourceKind::GlobalSettings
                | ResourceKind::NativeDatabase
        )
    {
        return Ok(false);
    }
    if !matches_current_file(root, receipt)? {
        return Ok(false);
    }
    let (parent, name) = open_parent(root, &receipt.path, false)?;
    #[cfg(not(unix))]
    {
        let _ = (parent, name);
        Ok(false)
    }
    #[cfg(unix)]
    {
        nix::unistd::unlinkat(
            &parent,
            name.as_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )
        .map_err(|e| HostError::io("remove owned config", e.into()))?;
        parent
            .sync_all()
            .map_err(|e| HostError::io("config directory sync", e))?;
        Ok(true)
    }
}
