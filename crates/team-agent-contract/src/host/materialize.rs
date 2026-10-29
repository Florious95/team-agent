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
        if matches!(plan.expected_session, ExpectedSession::PendingAllocation) {
            return Err(HostError::new("native session allocation required", HostErrorKind::Ownership));
        }
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
            // Validation runs the same public command as the pane, wrappers included.
            executable: plan.launcher.executable.clone(),
            executable_hash: plan.launcher.sha256,
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
    fn merge_shared_entry(
        &mut self,
        request: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        let refuse = |error| PartialFailure { error, receipt: MaterializeReceipt { resources: vec![] } };
        if !self.requests.contains(request) {
            return Err(refuse(ContractError::Mismatch("captured write grant")));
        }
        let root = self.roots.get(request.path.root()).ok_or_else(|| refuse(ContractError::Mismatch("captured root")))?;
        if request.kind != ResourceKind::McpConfig
            || !self.policies.contains(&ResourcePolicy { kind: request.kind, disposition: ResourceDisposition::SharedEntry })
        {
            return Err(refuse(ContractError::Invalid("resource policy")));
        }
        let mut receipt = OwnedResourceReceipt {
            path: request.path.clone(),
            owner: request.owner.clone(),
            operation: self.operation.clone(),
            kind: request.kind,
            disposition: ResourceDisposition::SharedEntry,
            write_effect: ResourceWriteEffect::MayHaveWritten,
            exclusive: false,
            creation_identity: None,
        };
        let key = shared_entry_key(&request.owner);
        // Only static operation names reach the error; never file contents.
        update_shared(root, &request.path, true, |current| edit_shared(&request.path, current, &key, Some(&request.contents)))
            .map_err(|error| PartialFailure {
                error: ContractError::Invalid(error.operation),
                receipt: MaterializeReceipt { resources: vec![receipt.clone()] },
            })?;
        receipt.write_effect = ResourceWriteEffect::Written { bytes_sha256: digest(&request.contents) };
        Ok(receipt)
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
    if receipt.disposition == ResourceDisposition::SharedEntry {
        if &receipt.owner != owner || receipt.kind != ResourceKind::McpConfig {
            return Ok(false);
        }
        let key = shared_entry_key(owner);
        // A missing file or directory holds no entry of this owner.
        return match update_shared(root, &receipt.path, false, |current| edit_shared(&receipt.path, current, &key, None)) {
            Err(HostError { kind: HostErrorKind::Io(std::io::ErrorKind::NotFound), .. }) => Ok(true),
            result => result.map(|_| true),
        };
    }
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

/// Name of a seat's own entry inside a shared configuration file: one per
/// scope/seat/instance/generation, recomputed from a receipt's owner at removal.
pub fn shared_entry_key(owner: &InstanceIdentity) -> String {
    let text = format!(
        "team-entry-v1\0{}\0{}\0{}\0{}",
        owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0
    );
    format!("team_{}", &super::digest_hex(digest(text.as_bytes()))[..32])
}

/// Verify only this generation's entry, not mutable user/sibling configuration.
/// Uses the same no-follow, owner-checked directory lock as merge and removal.
pub fn verify_shared_entry(request: &OwnedResourceRequest) -> Result<(), HostError> {
    let root = resolve_cwd(request.path.root())?;
    let key = shared_entry_key(&request.owner);
    update_shared(&root, &request.path, false, |current| {
        let valid = current.is_some_and(|bytes| match request.path.relative().extension().and_then(|extension| extension.to_str()) {
            Some("toml") => {
                let mut expected = format!("# >>> team-agent seat entry {key} (removed when the seat stops)\n").into_bytes();
                expected.extend_from_slice(&request.contents);
                expected.extend_from_slice(format!("# <<< team-agent seat entry {key}\n").as_bytes());
                bytes.windows(expected.len()).filter(|window| *window == expected).count() == 1
            }
            Some("json") => {
                let actual = serde_json::from_slice::<serde_json::Value>(bytes).ok();
                let expected = serde_json::from_slice::<serde_json::Value>(&request.contents).ok();
                let entry = |document: &serde_json::Value| document.get("mcpServers").and_then(|servers| servers.get(&key)).cloned();
                expected.as_ref().and_then(entry).is_some_and(|expected| actual.as_ref().and_then(entry) == Some(expected))
            }
            _ => false,
        });
        if !valid { return Err(HostError::new("shared materialized entry changed", HostErrorKind::Conflict)); }
        Ok(SharedEdit::Unchanged)
    }).map(|_| ())
}

enum SharedEdit {
    Unchanged,
    Write(Vec<u8>),
    Remove,
}

/// Merges (`entry` Some) or removes (None) exactly one owner entry. JSON keeps
/// every other key; TOML keeps every other byte and holds the entry between
/// marker lines. A file left with nothing else in it is removed.
fn edit_shared(path: &OwnedPath, current: Option<&[u8]>, key: &str, entry: Option<&[u8]>) -> Result<SharedEdit, HostError> {
    let invalid = |operation| HostError::new(operation, HostErrorKind::Invalid);
    match path.relative().extension().and_then(|extension| extension.to_str()) {
        Some("json") => {
            let mut document = match current {
                Some(bytes) => serde_json::from_slice(bytes)
                    .map_err(|_| invalid("project MCP configuration is not valid JSON; fix or remove it"))?,
                None => serde_json::json!({}),
            };
            let servers = document.as_object_mut()
                .ok_or_else(|| invalid("project MCP configuration is not a JSON object"))?
                .entry("mcpServers").or_insert_with(|| serde_json::json!({}))
                .as_object_mut().ok_or_else(|| invalid("project MCP configuration mcpServers is not an object"))?;
            let removal = match entry {
                Some(fragment) => {
                    let fragment: serde_json::Value = serde_json::from_slice(fragment).map_err(|_| invalid("seat MCP entry"))?;
                    let value = fragment.as_object().filter(|root| root.len() == 1)
                        .and_then(|root| root.get("mcpServers")).and_then(serde_json::Value::as_object)
                        .filter(|entries| entries.len() == 1).and_then(|entries| entries.get(key))
                        .filter(|value| value.is_object()).ok_or_else(|| invalid("seat MCP entry"))?;
                    servers.insert(key.into(), value.clone());
                    false
                }
                None if servers.remove(key).is_none() => return Ok(SharedEdit::Unchanged),
                None => true,
            };
            if removal && document == serde_json::json!({"mcpServers": {}}) {
                return Ok(SharedEdit::Remove);
            }
            let mut bytes = serde_json::to_vec_pretty(&document).map_err(|_| invalid("project MCP configuration encoding"))?;
            bytes.push(b'\n');
            Ok(SharedEdit::Write(bytes))
        }
        Some("toml") => {
            let mut text = match current {
                Some(bytes) => std::str::from_utf8(bytes)
                    .map_err(|_| invalid("project MCP configuration is not UTF-8 TOML; fix or remove it"))?.to_owned(),
                None => String::new(),
            };
            let begin = format!("# >>> team-agent seat entry {key} (removed when the seat stops)\n");
            let end = format!("# <<< team-agent seat entry {key}\n");
            let found = match text.find(&begin) {
                Some(start) => {
                    let stop = text[start..].find(&end).map(|offset| start + offset + end.len())
                        .ok_or_else(|| invalid("project MCP configuration has an unterminated team-agent entry"))?;
                    text.replace_range(start..stop, "");
                    true
                }
                None => false,
            };
            match entry {
                Some(fragment) => {
                    let fragment = std::str::from_utf8(fragment).ok().filter(|fragment| fragment.ends_with('\n'))
                        .ok_or_else(|| invalid("seat MCP entry"))?;
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&begin);
                    text.push_str(fragment);
                    text.push_str(&end);
                }
                None if !found => return Ok(SharedEdit::Unchanged),
                None if text.trim().is_empty() => return Ok(SharedEdit::Remove),
                None => {}
            }
            Ok(SharedEdit::Write(text.into_bytes()))
        }
        _ => Err(HostError::new("shared configuration format", HostErrorKind::Unsupported)),
    }
}

/// Read-edit-replace under an exclusive lock on the containing directory, so
/// concurrent seats never lose each other's entries. The file is replaced by an
/// atomic rename and keeps its mode; a symlink or foreign-owned file is refused.
fn update_shared(
    root: &CwdIdentity,
    path: &OwnedPath,
    create: bool,
    edit: impl FnOnce(Option<&[u8]>) -> Result<SharedEdit, HostError>,
) -> Result<bool, HostError> {
    let (parent, name) = open_parent(root, path, create)?;
    #[cfg(not(unix))]
    {
        let _ = (parent, name, edit);
        Err(HostError::new("shared configuration", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        use nix::fcntl::{openat, renameat, Flock, FlockArg, OFlag};
        use nix::sys::stat::{fchmod, Mode};
        use nix::unistd::{unlinkat, UnlinkatFlags};
        use std::os::unix::fs::MetadataExt;
        let parent = Flock::lock(parent, FlockArg::LockExclusive)
            .map_err(|(_, error)| HostError::io("shared configuration lock", error.into()))?;
        let current = match openat(
            &*parent,
            name.as_str(),
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let file = File::from(fd);
                let metadata = file.metadata().map_err(|e| HostError::io("shared configuration metadata", e))?;
                if !metadata.is_file() || metadata.uid() != nix::unistd::geteuid().as_raw() {
                    return Err(HostError::new("project MCP configuration is not a regular file owned by this user", HostErrorKind::Ownership));
                }
                let mut bytes = vec![];
                (&file).take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)
                    .map_err(|e| HostError::io("shared configuration read", e))?;
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err(HostError::new("shared configuration size", HostErrorKind::Invalid));
                }
                Some((bytes, metadata.mode() & 0o7777))
            }
            Err(nix::errno::Errno::ENOENT) => None,
            Err(error) => return Err(HostError::io("shared configuration open", error.into())),
        };
        let bytes = match edit(current.as_ref().map(|(bytes, _)| bytes.as_slice()))? {
            SharedEdit::Unchanged => return Ok(false),
            SharedEdit::Remove => {
                unlinkat(&*parent, name.as_str(), UnlinkatFlags::NoRemoveDir)
                    .map_err(|e| HostError::io("shared configuration remove", e.into()))?;
                parent.sync_all().map_err(|e| HostError::io("shared configuration directory sync", e))?;
                return Ok(true);
            }
            SharedEdit::Write(bytes) => bytes,
        };
        let mode = current.map_or(0o644, |(_, mode)| mode);
        let temporary = format!(".{name}.team-agent-{}", std::process::id());
        // Only a crashed earlier holder of this pid can have left it; we hold the lock.
        let _ = unlinkat(&*parent, temporary.as_str(), UnlinkatFlags::NoRemoveDir);
        let fd = openat(
            &*parent,
            temporary.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|e| HostError::io("shared configuration temporary", e.into()))?;
        let mut file = File::from(fd);
        let written = file.write_all(&bytes)
            .and_then(|_| fchmod(&file, Mode::from_bits_truncate(mode as _)).map_err(std::io::Error::from))
            .and_then(|_| file.sync_all())
            .and_then(|_| renameat(&*parent, temporary.as_str(), &*parent, name.as_str()).map_err(std::io::Error::from))
            .and_then(|_| parent.sync_all());
        if let Err(error) = written {
            let _ = unlinkat(&*parent, temporary.as_str(), UnlinkatFlags::NoRemoveDir);
            return Err(HostError::io("shared configuration write", error));
        }
        Ok(true)
    }
}
