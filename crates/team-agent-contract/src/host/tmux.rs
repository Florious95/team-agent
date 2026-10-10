use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use super::clock::{remaining, Clock};
use super::command::{CommandEnd, CommandReceipt, CommandRequest, CommandRunner, OutputLimits};
use super::files::{fingerprint_materialized, DirectoryReceipt, FileReceipt, ScopedDirectory};
use super::process::{
    capture_process, fingerprint_file, resolve_cwd, sample_process, ProcessState,
};
use super::shell::{launch_script, quote};
use super::transport::*;
use super::{digest, digest_hex, HostError, HostErrorKind};
use crate::contract::delivery::{PasteMode, PhysicalKey, StepOutcome};
use crate::contract::descriptor::ProviderDescriptor;
use crate::contract::plan::{
    validate_launch_plan, EnvironmentDelta, LaunchPlan, MaterializeReceipt, ResolvedLaunch,
    ResourceWriteEffect,
};
use crate::contract::probe::ProcessAliveEvidence;

const FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{session_name}\t#{window_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_in_mode}\t#{pane_mode}\t#{@tac_binding}\t#{pane_width}\t#{pane_height}";
const CONFIG: &[u8] = b"set-option -g default-shell /bin/sh\nset-window-option -g remain-on-exit on\nset-window-option -g automatic-rename off\nset-window-option -g allow-rename off\nset-option -s exit-empty on\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostLimits {
    pub command_output: OutputLimits,
    pub max_script_bytes: usize,
    pub max_capture_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_executable_bytes: u64,
    pub poll_interval: Duration,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneAddress {
    pub session: String,
    pub window: String,
    pub pane: String,
    pub pid: u32,
    pub session_name: String,
    pub window_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub address: PaneAddress,
    pub dead: bool,
    pub exit_code: Option<i32>,
    pub mode: PaneMode,
    pub binding: String,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneMetadataStage {
    NewSession,
    Query,
}

/// Only the fixed metadata FORMAT response, never a pane capture or command/env.
/// At most 256 original bytes are retained, escaped to at most 1024 ASCII bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneMetadataDiagnostic {
    pub stage: PaneMetadataStage,
    pub actual_fields: usize,
    pub output_bytes: usize,
    pub sample: String,
    pub truncated: bool,
}

impl std::fmt::Display for PaneMetadataDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stage = match self.stage {
            PaneMetadataStage::NewSession => "new-session",
            PaneMetadataStage::Query => "query",
        };
        write!(
            f,
            "stage={stage}; fields={}; bytes={}; sample=\"{}\"; truncated={}",
            self.actual_fields, self.output_bytes, self.sample, self.truncated
        )
    }
}

/// Identity-only diagnostics: no process argv, environment or pane contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundProcessDiagnostic {
    pub expected_pid: u32,
    pub pane_dead: bool,
    pub require_alive: bool,
    /// None when tmux already reports a dead pane (the original short circuit).
    pub sampled: Option<ProcessState>,
}

impl std::fmt::Display for BoundProcessDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stage=check-bound; expected_pid={}; pane_dead={}; require_alive={}; process_state={:?}",
            self.expected_pid, self.pane_dead, self.require_alive, self.sampled
        )
    }
}

fn check_bound_process(
    expected_pid: u32,
    pane_dead: bool,
    require_alive: bool,
    sample: impl FnOnce() -> ProcessState,
) -> Result<(), HostError> {
    let sampled = (!pane_dead).then(sample);
    if (require_alive && pane_dead)
        || sampled.as_ref().is_some_and(|state| *state != ProcessState::Alive)
    {
        let mut error = HostError::new(
            "native instance not alive or replaced",
            HostErrorKind::Unknown,
        );
        error.process = Some(Box::new(BoundProcessDiagnostic {
            expected_pid,
            pane_dead,
            require_alive,
            sampled,
        }));
        return Err(error);
    }
    Ok(())
}

fn directory_signature(owner: &DirectoryReceipt) -> String {
    let key = format!(
        "{}:{}:{}:{}:{}:{}",
        owner.owner.scope.as_str(),
        owner.owner.seat.as_str(),
        owner.owner.instance.as_str(),
        owner.owner.generation.0,
        owner.device,
        owner.inode
    );
    digest_hex(digest(key.as_bytes()))
}

fn numbered(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit()))
}

pub fn parse_pane(bytes: &[u8], stage: PaneMetadataStage) -> Result<PaneState, HostError> {
    let invalid = |operation| {
        let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let actual_fields = if line.is_empty() {
            0
        } else if line.contains(&b'\t') {
            line.split(|byte| *byte == b'\t').count()
        } else {
            line.windows(2).filter(|pair| *pair == b"\\t").count() + 1
        };
        HostError {
            operation,
            kind: HostErrorKind::Unknown,
            metadata: Some(PaneMetadataDiagnostic {
                stage,
                actual_fields,
                output_bytes: bytes.len(),
                sample: bytes[..bytes.len().min(256)]
                    .iter()
                    .flat_map(|byte| std::ascii::escape_default(*byte))
                    .map(char::from)
                    .collect(),
                truncated: bytes.len() > 256,
            }),
            process: None,
        }
    };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("tmux metadata encoding"))?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.is_empty() {
        return Err(invalid("tmux metadata empty output"));
    }
    // Preserve native field contents: prefer real TABs, otherwise accept the
    // escaped separator spelling. Never globally unescape or merge mixed frames.
    let fields: Vec<_> = if line.contains('\t') {
        line.split('\t').collect()
    } else {
        line.split(r"\t").collect()
    };
    if fields.len() != 13 {
        return Err(invalid("tmux metadata field count (expected 13)"));
    }
    for (index, prefix, operation) in [
        (0, '$', "tmux metadata invalid session ID"),
        (1, '@', "tmux metadata invalid window ID"),
        (2, '%', "tmux metadata invalid pane ID"),
    ] {
        if !numbered(fields[index], prefix) {
            return Err(invalid(operation));
        }
    }
    let pid = fields[3]
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| invalid("tmux pane pid"))?;
    let dead = match fields[6] {
        "0" => false,
        "1" => true,
        _ => return Err(invalid("tmux pane state")),
    };
    let exit_code = if fields[7].is_empty() {
        None
    } else {
        Some(
            fields[7]
                .parse()
                .map_err(|_| invalid("tmux exit receipt"))?,
        )
    };
    let mode = match (fields[8], fields[9]) {
        ("0", "") => PaneMode::Normal,
        ("1", "copy-mode") => PaneMode::Copy,
        ("1", "view-mode") => PaneMode::View,
        (_, name) => PaneMode::Unknown(name.to_string()),
    };
    let columns = fields[11]
        .parse::<u16>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("pane columns"))?;
    let rows = fields[12]
        .parse::<u16>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("pane rows"))?;
    Ok(PaneState {
        address: PaneAddress {
            session: fields[0].into(),
            window: fields[1].into(),
            pane: fields[2].into(),
            pid,
            session_name: fields[4].into(),
            window_name: fields[5].into(),
        },
        dead,
        exit_code,
        mode,
        binding: fields[10].into(),
        columns,
        rows,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnReceipt {
    pub target: Option<TargetReceipt>,
    pub pane: Option<PaneAddress>,
    pub files: Vec<FileReceipt>,
    pub may_have_spawned: bool,
    pub observed_exit_code: Option<i32>,
    pub problem: Option<HostError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseReceipt {
    pub pane_close: ActionResult,
    pub native: ProcessState,
    pub socket_preserved: bool,
}

/// After a close may have executed, observe only: reparenting/exit races do not
/// grant authority for another action, nor prove exit. Keep the original budget.
fn observe_native_exit(
    close_outcome: StepOutcome,
    clock: &dyn Clock,
    deadline: Duration,
    poll_interval: Duration,
    mut sample: impl FnMut() -> ProcessState,
) -> ProcessState {
    let mut native = sample();
    while close_outcome != StepOutcome::NoEffect && native != ProcessState::Exited {
        let Some(left) = remaining(clock, deadline) else {
            break;
        };
        clock.sleep(poll_interval.min(left));
        native = sample();
    }
    native
}

/// One host instance owns one private endpoint/generation. No default server or adoption.
pub struct TmuxHost<R: CommandRunner> {
    directory: ScopedDirectory,
    executable: PathBuf,
    endpoint: PathBuf,
    limits: HostLimits,
    runner: R,
    target: Option<TargetReceipt>,
    sequence: u64,
    buffers: BTreeMap<String, PasteMode>,
}

impl<R: CommandRunner> TmuxHost<R> {
    pub fn new(
        directory: ScopedDirectory,
        executable: PathBuf,
        limits: HostLimits,
        runner: R,
    ) -> Result<Self, HostError> {
        crate::contract::types::require_absolute(&executable, "tmux executable")
            .map_err(|_| HostError::new("tmux executable", HostErrorKind::Invalid))?;
        directory.check_live()?;
        let endpoint = directory.path().join("tmux.sock");
        // macOS sockaddr_un is the smaller boundary; no fallback to a shared/default endpoint.
        if endpoint.as_os_str().as_encoded_bytes().len() >= 104
            || endpoint.to_str().is_none()
            || limits.poll_interval.is_zero()
            || limits.max_script_bytes == 0
            || limits.max_payload_bytes == 0
            || limits.max_capture_bytes == 0
            || limits.max_executable_bytes == 0
            || !(10..=1000).contains(&limits.columns)
            || !(5..=500).contains(&limits.rows)
        {
            return Err(HostError::new(
                "tmux host limits/endpoint",
                HostErrorKind::Invalid,
            ));
        }
        Ok(Self {
            directory,
            executable,
            endpoint,
            limits,
            runner,
            target: None,
            sequence: 0,
            buffers: BTreeMap::new(),
        })
    }
    /// Reopen only a supervisor-persisted receipt, never a discovered pane or
    /// caller-supplied PID. The private directory, socket inode, pane binding,
    /// dimensions and native birth/image are checked before returning authority.
    /// Dead panes are retained so lifecycle teardown can observe their exit.
    pub fn restore_owned(
        target: TargetReceipt,
        executable: PathBuf,
        limits: HostLimits,
        runner: R,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<Self, HostError> {
        let directory = ScopedDirectory::reopen(target.directory.clone())?;
        let mut host = Self::new(directory, executable, limits, runner)?;
        let binding = digest_hex(digest(
            format!(
                "{}:{}:{}",
                directory_signature(&target.directory),
                target.socket_device,
                target.socket_inode
            )
            .as_bytes(),
        ));
        if target.endpoint != host.endpoint
            || target.binding != binding
            || target.columns != limits.columns
            || target.rows != limits.rows
            || resolve_cwd(&target.cwd.path)? != target.cwd
        {
            return Err(HostError::new(
                "persisted target identity",
                HostErrorKind::Ownership,
            ));
        }
        host.target = Some(target.clone());
        host.check_bound(&target, clock, deadline, false)?;
        Ok(host)
    }

    pub fn directory(&self) -> &ScopedDirectory {
        &self.directory
    }
    pub fn runner_mut(&mut self) -> &mut R {
        &mut self.runner
    }
    pub fn target(&self) -> Option<&TargetReceipt> {
        self.target.as_ref()
    }

    fn command(
        &mut self,
        arguments: Vec<OsString>,
        stdin: Option<Vec<u8>>,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<CommandReceipt, HostError> {
        self.directory.check_live()?;
        let budget = remaining(clock, deadline)
            .ok_or_else(|| HostError::new("tmux deadline", HostErrorKind::Deadline))?;
        // This is a UTF-8 machine protocol, independent of the caller's locale.
        // Without -u, tmux sanitizes TABs to '_' when TMUX/UTF-8 locale are absent.
        let mut argv = vec![
            OsString::from("-u"),
            OsString::from("-S"),
            self.endpoint.as_os_str().to_owned(),
        ];
        argv.extend(arguments);
        let result = self.runner.run(&CommandRequest {
            executable: self.executable.clone(),
            arguments: argv,
            cwd: None,
            environment: EnvironmentDelta {
                remove: BTreeSet::from(["TMUX".into(), "TMUX_PANE".into()]),
                set: BTreeMap::new(),
            },
            stdin,
            budget,
            limits: self.limits.command_output,
            reject_stdout: None,
        });
        Ok(result)
    }

    fn query_pane(
        &mut self,
        pane: &str,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<PaneState, HostError> {
        if !numbered(pane, '%') {
            return Err(HostError::new("tmux pane id", HostErrorKind::Invalid));
        }
        let result = self.command(
            vec![
                "display-message".into(),
                "-p".into(),
                "-t".into(),
                pane.into(),
                FORMAT.into(),
            ],
            None,
            clock,
            deadline,
        )?;
        if !result.success() {
            return Err(command_error(&result, "tmux pane query"));
        }
        parse_pane(&result.stdout, PaneMetadataStage::Query)
    }

    fn socket_identity(&self) -> Result<(u64, u64), HostError> {
        #[cfg(not(unix))]
        {
            Err(HostError::new("tmux socket", HostErrorKind::Unsupported))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            let metadata = std::fs::symlink_metadata(&self.endpoint)
                .map_err(|e| HostError::io("tmux socket", e))?;
            if !metadata.file_type().is_socket()
                || metadata.uid() != nix::unistd::geteuid().as_raw()
            {
                return Err(HostError::new(
                    "tmux socket owner",
                    HostErrorKind::Ownership,
                ));
            }
            Ok((metadata.dev(), metadata.ino()))
        }
    }

    fn check_bound(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
        require_alive: bool,
    ) -> Result<PaneState, HostError> {
        if self.target.as_ref() != Some(target)
            || target.directory != *self.directory.receipt()
            || target.endpoint != self.endpoint
            || self.socket_identity()? != (target.socket_device, target.socket_inode)
        {
            return Err(HostError::new(
                "tmux target binding",
                HostErrorKind::Ownership,
            ));
        }
        let pane = self.query_pane(&target.pane, clock, deadline)?;
        if pane.address.session != target.session
            || pane.address.window != target.window
            || pane.address.pane != target.pane
            || pane.address.pid != target.process.identity.pid
            || pane.binding != target.binding
            || pane.columns != target.columns
            || pane.rows != target.rows
        {
            return Err(HostError::new(
                "tmux target changed",
                HostErrorKind::Ownership,
            ));
        }
        check_bound_process(
            target.process.identity.pid,
            pane.dead,
            require_alive,
            || sample_process(&target.process),
        )?;
        Ok(pane)
    }

    pub fn spawn_owned(
        &mut self,
        descriptor: &ProviderDescriptor,
        resolved: &ResolvedLaunch,
        plan: &LaunchPlan,
        materialized: &MaterializeReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> SpawnReceipt {
        let mut receipt = SpawnReceipt {
            target: None,
            pane: None,
            files: Vec::new(),
            may_have_spawned: false,
            observed_exit_code: None,
            problem: None,
        };
        match self.spawn_inner(
            descriptor,
            resolved,
            plan,
            materialized,
            clock,
            deadline,
            &mut receipt,
        ) {
            Ok(target) => {
                self.target = Some(target.clone());
                receipt.target = Some(target);
            }
            Err(error) => receipt.problem = Some(error),
        }
        receipt
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_inner(
        &mut self,
        descriptor: &ProviderDescriptor,
        resolved: &ResolvedLaunch,
        plan: &LaunchPlan,
        materialized: &MaterializeReceipt,
        clock: &dyn Clock,
        deadline: Duration,
        receipt: &mut SpawnReceipt,
    ) -> Result<TargetReceipt, HostError> {
        validate_launch_plan(descriptor, resolved, plan)
            .map_err(|_| HostError::new("launch contract", HostErrorKind::Invalid))?;
        self.directory.check_live()?;
        if self.target.is_some()
            || self.directory.receipt().owner != resolved.request().identity
            || !self
                .directory
                .path()
                .starts_with(&resolved.request().paths.runtime_root)
            || resolve_cwd(&plan.cwd)? != resolved.request().paths.cwd
        {
            return Err(HostError::new("spawn scope/cwd", HostErrorKind::Ownership));
        }
        match std::fs::symlink_metadata(&self.endpoint) {
            Ok(_) => {
                return Err(HostError::new(
                    "existing endpoint is not adopted",
                    HostErrorKind::Conflict,
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(HostError::io("endpoint preflight", error)),
        }
        let candidate = resolved
            .request()
            .paths
            .candidate
            .canonicalize()
            .map_err(|e| HostError::io("candidate path", e))?;
        if candidate
            != std::env::current_exe()
                .and_then(|path| path.canonicalize())
                .map_err(|e| HostError::io("running candidate", e))?
        {
            return Err(HostError::new(
                "MCP candidate is not this runtime",
                HostErrorKind::Ownership,
            ));
        }
        let candidate_sha256 = fingerprint_file(
            &candidate,
            self.limits.max_executable_bytes,
            remaining(clock, deadline)
                .ok_or_else(|| HostError::new("spawn deadline", HostErrorKind::Deadline))?,
        )?;
        let budget = remaining(clock, deadline)
            .ok_or_else(|| HostError::new("spawn deadline", HostErrorKind::Deadline))?;
        if fingerprint_file(&plan.executable, self.limits.max_executable_bytes, budget)?
            != resolved.request().native.executable_sha256
        {
            return Err(HostError::new(
                "native executable fingerprint",
                HostErrorKind::Ownership,
            ));
        }
        for request in &plan.materialization {
            let expected = digest(&request.contents);
            let found = materialized.resources.iter().find(|resource| {
                resource.path == request.path
                    && resource.kind == request.kind
                    && resource.owner == request.owner
                    && resource.exclusive
                    && resource.write_effect
                        == (ResourceWriteEffect::Written {
                            bytes_sha256: expected,
                        })
            });
            if found.is_none() {
                return Err(HostError::new(
                    "materialization receipt",
                    HostErrorKind::Ownership,
                ));
            }
            let budget = remaining(clock, deadline)
                .ok_or_else(|| HostError::new("spawn deadline", HostErrorKind::Deadline))?;
            if fingerprint_materialized(&request.path, self.limits.max_script_bytes as u64, budget)?
                != expected
            {
                return Err(HostError::new(
                    "materialized bytes changed",
                    HostErrorKind::Conflict,
                ));
            }
        }
        // Inspect names only; values are neither used nor logged. HOME/XDG/auth settings
        // are preserved. New scope identity comes solely from plan.environment.set.
        let mut inherited_identity_keys = BTreeSet::new();
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"TEAM_AGENT_") {
                inherited_identity_keys.insert(key.into_string().map_err(|_| {
                    HostError::new("inherited identity key encoding", HostErrorKind::Invalid)
                })?);
            }
        }
        let script = launch_script(plan, &inherited_identity_keys)?;
        if script.len() > self.limits.max_script_bytes {
            return Err(HostError::new(
                "launch script limit",
                HostErrorKind::Invalid,
            ));
        }
        for (name, bytes) in [("tmux.conf", CONFIG), ("launch.sh", script.as_slice())] {
            match self.directory.create_file(name, bytes) {
                Ok(file) => receipt.files.push(file),
                Err(failure) => {
                    if let Some(file) = failure.possible {
                        receipt.files.push(file);
                    }
                    return Err(failure.error);
                }
            }
        }
        let signature = directory_signature(self.directory.receipt());
        let session_name = format!("tac-{}", &signature[..24]);
        let mut shell = b"exec /bin/sh ".to_vec();
        shell.extend(quote(self.directory.path().join("launch.sh").as_os_str())?);
        #[cfg(unix)]
        let shell = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(shell)
        };
        #[cfg(not(unix))]
        let shell = OsString::from(
            String::from_utf8(shell)
                .map_err(|_| HostError::new("host platform", HostErrorKind::Unsupported))?,
        );
        let result = self.command(
            vec![
                "-f".into(),
                self.directory.path().join("tmux.conf").into_os_string(),
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                session_name.clone().into(),
                "-n".into(),
                "worker".into(),
                "-x".into(),
                self.limits.columns.to_string().into(),
                "-y".into(),
                self.limits.rows.to_string().into(),
                "-P".into(),
                "-F".into(),
                FORMAT.into(),
                shell,
            ],
            None,
            clock,
            deadline,
        )?;
        receipt.may_have_spawned = result.may_have_executed();
        if !result.success() {
            return Err(command_error(&result, "tmux spawn"));
        }
        let initial = parse_pane(&result.stdout, PaneMetadataStage::NewSession)?;
        receipt.pane = Some(initial.address.clone());
        if initial.address.session_name != session_name
            || initial.address.window_name != "worker"
            || initial.columns != self.limits.columns
            || initial.rows != self.limits.rows
        {
            return Err(HostError::new(
                "returned spawn target",
                HostErrorKind::Ownership,
            ));
        }
        let (device, inode) = self.socket_identity()?;
        let binding = digest_hex(digest(format!("{signature}:{device}:{inode}").as_bytes()));
        let set = self.command(
            vec![
                "set-option".into(),
                "-p".into(),
                "-t".into(),
                initial.address.pane.clone().into(),
                "@tac_binding".into(),
                binding.clone().into(),
            ],
            None,
            clock,
            deadline,
        )?;
        if !set.success() {
            return Err(command_error(&set, "tmux owner binding"));
        }
        loop {
            let pane = self.query_pane(&initial.address.pane, clock, deadline)?;
            if pane.address != initial.address || pane.binding != binding {
                return Err(HostError::new(
                    "spawn pane changed",
                    HostErrorKind::Ownership,
                ));
            }
            if pane.dead {
                receipt.observed_exit_code = pane.exit_code;
                return Err(HostError::new(
                    "native exited during spawn",
                    HostErrorKind::Unknown,
                ));
            }
            let budget = remaining(clock, deadline).ok_or_else(|| {
                HostError::new("native identity deadline", HostErrorKind::Deadline)
            })?;
            if let Ok(process) = capture_process(
                pane.address.pid,
                &plan.executable,
                resolved.request().native.executable_sha256,
                budget,
            ) {
                if remaining(clock, deadline).is_none() {
                    return Err(HostError::new(
                        "native identity deadline",
                        HostErrorKind::Deadline,
                    ));
                }
                return Ok(TargetReceipt {
                    directory: self.directory.receipt().clone(),
                    endpoint: self.endpoint.clone(),
                    socket_device: device,
                    socket_inode: inode,
                    session: pane.address.session,
                    window: pane.address.window,
                    pane: pane.address.pane,
                    columns: pane.columns,
                    rows: pane.rows,
                    binding,
                    process,
                    provider: resolved.provider().clone(),
                    cwd: resolved.request().paths.cwd.clone(),
                    native: resolved.request().native.clone(),
                    evidence_kind: resolved.request().evidence_kind,
                    candidate_sha256,
                    native_session: None,
                });
            }
            clock.sleep(
                self.limits
                    .poll_interval
                    .min(remaining(clock, deadline).unwrap_or_default()),
            );
        }
    }

    /// H4's captured local binding can update the same held native interaction lane.
    /// Changing SID invalidates prior scoped protocol evidence; it does not create readiness.
    pub fn bind_session(
        &mut self,
        binding: &crate::contract::session::ResumeBinding,
        lane: &dyn InputLease,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<TargetReceipt, HostError> {
        let mut target = self
            .target
            .clone()
            .ok_or_else(|| HostError::new("missing native target", HostErrorKind::Unknown))?;
        if lane.directory() != &target.directory {
            return Err(HostError::new("session lane", HostErrorKind::Ownership));
        }
        self.check_bound(&target, clock, deadline, true)?;
        crate::contract::session::validate_resume(
            binding,
            &crate::contract::session::ResumeExpectation {
                provider: &target.provider,
                source: target.identity(),
                cwd: &target.cwd,
                native: &target.native,
                evidence_kind: target.evidence_kind,
                mode: crate::contract::descriptor::ResumeMode::ExactId,
            },
        )
        .map_err(|_| HostError::new("native session binding", HostErrorKind::Ownership))?;
        target.native_session = Some(binding.native_session.clone());
        self.target = Some(target.clone());
        Ok(target)
    }

    /// Closes only the still-bound owned pane. It does not claim whole-tree cleanup,
    /// kill a server, unlink a socket, delete native sessions or remove other resources.
    pub fn close_owned_pane(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> CloseReceipt {
        let action = match self.directory.try_lane() {
            Err(error) => ActionResult::refused(error),
            Ok(_lane) => match self.check_bound(target, clock, deadline, false) {
                Err(error) => ActionResult::refused(error),
                Ok(_) => match self.command(
                    vec!["kill-pane".into(), "-t".into(), target.pane.clone().into()],
                    None,
                    clock,
                    deadline,
                ) {
                    Ok(result) => action_result(result, "owned pane close"),
                    Err(error) => ActionResult::refused(error),
                },
            },
        };
        let native = observe_native_exit(
            action.outcome,
            clock,
            deadline,
            self.limits.poll_interval,
            || sample_process(&target.process),
        );
        CloseReceipt {
            pane_close: action,
            native,
            socket_preserved: std::fs::symlink_metadata(&self.endpoint).is_ok(),
        }
    }
}

fn command_error(receipt: &CommandReceipt, operation: &'static str) -> HostError {
    HostError::new(
        operation,
        if receipt.end == CommandEnd::TimedOut {
            HostErrorKind::Deadline
        } else {
            HostErrorKind::Command
        },
    )
}
fn action_result(receipt: CommandReceipt, operation: &'static str) -> ActionResult {
    if receipt.success() {
        ActionResult::confirmed()
    } else if receipt.may_have_executed() {
        ActionResult::uncertain(command_error(&receipt, operation))
    } else {
        ActionResult::refused(command_error(&receipt, operation))
    }
}

impl<R: CommandRunner> PhysicalTransport for TmuxHost<R> {
    fn acquire_lane(&mut self, target: &TargetReceipt) -> Result<Box<dyn InputLease>, HostError> {
        if self.target.as_ref() != Some(target) {
            return Err(HostError::new(
                "input lane binding",
                HostErrorKind::Ownership,
            ));
        }
        Ok(Box::new(self.directory.try_lane()?))
    }
    fn validate_target(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<ProcessAliveEvidence, HostError> {
        self.check_bound(target, clock, deadline, true)?;
        Ok(ProcessAliveEvidence {
            process: target.process.identity.clone(),
        })
    }
    fn capture(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
        freshness: Duration,
    ) -> Result<HostCapture, HostError> {
        let pane = self.check_bound(target, clock, deadline, true)?;
        let result = self.command(
            vec![
                "capture-pane".into(),
                "-p".into(),
                "-t".into(),
                target.pane.clone().into(),
            ],
            None,
            clock,
            deadline,
        )?;
        if !result.success() {
            return Err(command_error(&result, "tmux capture"));
        }
        if result.stdout.len() > self.limits.max_capture_bytes {
            return Err(HostError::new("capture bounds", HostErrorKind::Invalid));
        }
        let text = String::from_utf8(result.stdout)
            .map_err(|_| HostError::new("capture encoding", HostErrorKind::Unknown))?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| HostError::new("capture sequence", HostErrorKind::Invalid))?;
        Ok(HostCapture {
            scope: target.scope(self.sequence, clock.now(), freshness, "tmux-current-pane"),
            text,
            mode: pane.mode,
        })
    }
    fn prepare_host_mode(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> HostPreparation {
        let before = match self.check_bound(target, clock, deadline, true) {
            Ok(pane) => pane.mode,
            Err(error) => {
                return HostPreparation {
                    mode_before: PaneMode::Unknown("unobserved".into()),
                    control_commands_confirmed: 0,
                    control_commands_uncertain: 0,
                    error: Some(error),
                }
            }
        };
        let mut report = HostPreparation {
            mode_before: before.clone(),
            control_commands_confirmed: 0,
            control_commands_uncertain: 0,
            error: None,
        };
        match before {
            PaneMode::Normal => {}
            PaneMode::Copy | PaneMode::View => {
                let action = match self.command(
                    vec![
                        "send-keys".into(),
                        "-X".into(),
                        "-t".into(),
                        target.pane.clone().into(),
                        "cancel".into(),
                    ],
                    None,
                    clock,
                    deadline,
                ) {
                    Ok(result) => action_result(result, "host copy-mode cancel"),
                    Err(error) => ActionResult::refused(error),
                };
                if action.outcome == StepOutcome::Confirmed {
                    report.control_commands_confirmed = 1;
                }
                if action.outcome == StepOutcome::MayHaveOccurred {
                    report.control_commands_uncertain = 1;
                }
                report.error = action.error;
                if report.error.is_none() {
                    match self.check_bound(target, clock, deadline, true) {
                        Ok(pane) if pane.mode == PaneMode::Normal => {}
                        _ => {
                            report.error = Some(HostError::new(
                                "host mode did not clear",
                                HostErrorKind::Unknown,
                            ))
                        }
                    }
                }
            }
            PaneMode::Unknown(_) => {
                report.error = Some(HostError::new("unknown host mode", HostErrorKind::Unknown))
            }
        }
        report
    }
    fn stage_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        bytes: &[u8],
        mode: PasteMode,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult {
        if bytes.len() > self.limits.max_payload_bytes
            || !name.starts_with("tac-")
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return ActionResult::refused(HostError::new(
                "owned buffer request",
                HostErrorKind::Invalid,
            ));
        }
        if let Err(error) = self.check_bound(target, clock, deadline, true) {
            return ActionResult::refused(error);
        }
        let listed = match self.command(
            vec!["list-buffers".into(), "-F".into(), "#{buffer_name}".into()],
            None,
            clock,
            deadline,
        ) {
            Ok(result) if result.success() => result,
            Ok(result) => {
                return ActionResult::refused(command_error(&result, "buffer name query"))
            }
            Err(error) => return ActionResult::refused(error),
        };
        let Ok(names) = std::str::from_utf8(&listed.stdout) else {
            return ActionResult::refused(HostError::new(
                "buffer name query",
                HostErrorKind::Unknown,
            ));
        };
        if names.lines().any(|existing| existing == name) || self.buffers.contains_key(name) {
            return ActionResult::refused(HostError::new(
                "existing buffer is not overwritten",
                HostErrorKind::Conflict,
            ));
        }
        // Reserve before the command: failures may still have created this exact buffer.
        self.buffers.insert(name.to_string(), mode);
        // tmux -p only frames when MODE_BRACKETPASTE is set; it is not an unconditional
        // framing guarantee. Put the complete paired frame in this single owned paste.
        // No isolated CSI201~ is ever sent as a recovery key or separate operation.
        let wire = if mode == PasteMode::Bracketed {
            [b"\x1b[200~".as_slice(), bytes, b"\x1b[201~".as_slice()].concat()
        } else {
            bytes.to_vec()
        };
        match self.command(
            vec!["load-buffer".into(), "-b".into(), name.into(), "-".into()],
            Some(wire),
            clock,
            deadline,
        ) {
            Ok(result) => action_result(result, "stage owned buffer"),
            Err(error) => ActionResult::refused(error),
        }
    }
    fn paste_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        mode: PasteMode,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult {
        if self.buffers.get(name) != Some(&mode) {
            return ActionResult::refused(HostError::new(
                "unknown buffer owner/mode",
                HostErrorKind::Ownership,
            ));
        }
        if let Err(error) = self.check_bound(target, clock, deadline, true) {
            return ActionResult::refused(error);
        }
        let args = if mode == PasteMode::DirectTyping {
            // Read only this attempt's staged buffer. Literal typing contains no
            // terminal control bytes and does NOT include the submit key.
            let result = match self.command(
                vec!["save-buffer".into(), "-b".into(), name.into(), "-".into()],
                None,
                clock,
                deadline,
            ) {
                Ok(result) if result.success() => result,
                Ok(result) => {
                    return ActionResult::refused(command_error(
                        &result,
                        "read owned control buffer",
                    ))
                }
                Err(error) => return ActionResult::refused(error),
            };
            let text = match std::str::from_utf8(&result.stdout) {
                Ok(text)
                    if !text.is_empty()
                        && text.len() <= self.limits.max_payload_bytes
                        && text.bytes().all(|byte| (b' '..=b'~').contains(&byte)) =>
                {
                    text
                }
                _ => {
                    return ActionResult::refused(HostError::new(
                        "literal control bytes",
                        HostErrorKind::Invalid,
                    ))
                }
            };
            if let Err(error) = self.check_bound(target, clock, deadline, true) {
                return ActionResult::refused(error);
            }
            vec![
                "send-keys".into(),
                "-l".into(),
                "-t".into(),
                target.pane.clone().into(),
                "--".into(),
                text.into(),
            ]
        } else {
            // -r preserves LF; tmux otherwise silently substitutes CR for each LF.
            vec![
                "paste-buffer".into(),
                "-r".into(),
                "-b".into(),
                name.into(),
                "-t".into(),
                target.pane.clone().into(),
            ]
        };
        match self.command(args, None, clock, deadline) {
            Ok(result) => action_result(result, "insert owned input"),
            Err(error) => ActionResult::refused(error),
        }
    }
    fn release_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult {
        if !self.buffers.contains_key(name) {
            return ActionResult::refused(HostError::new(
                "unknown buffer owner",
                HostErrorKind::Ownership,
            ));
        }
        if let Err(error) = self.check_bound(target, clock, deadline, false) {
            return ActionResult::refused(error);
        }
        let action = match self.command(
            vec!["delete-buffer".into(), "-b".into(), name.into()],
            None,
            clock,
            deadline,
        ) {
            Ok(result) => action_result(result, "release owned buffer"),
            Err(error) => ActionResult::refused(error),
        };
        if action.outcome == StepOutcome::Confirmed {
            self.buffers.remove(name);
        }
        action
    }
    fn key(
        &mut self,
        target: &TargetReceipt,
        key: PhysicalKey,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult {
        if let Err(error) = self.check_bound(target, clock, deadline, true) {
            return ActionResult::refused(error);
        }
        let literal = match key {
            PhysicalKey::Enter => "Enter",
            PhysicalKey::LineFeed | PhysicalKey::CtrlJ => "C-j",
            PhysicalKey::Up => "Up",
            PhysicalKey::Down => "Down",
            PhysicalKey::Tab => "Tab",
            PhysicalKey::Escape => "Escape",
        };
        match self.command(
            vec![
                "send-keys".into(),
                "-t".into(),
                target.pane.clone().into(),
                literal.into(),
            ],
            None,
            clock,
            deadline,
        ) {
            Ok(result) => action_result(result, "native key"),
            Err(error) => ActionResult::refused(error),
        }
    }
}

#[cfg(test)]
mod bound_process_tests {
    use super::*;

    #[derive(Default)]
    struct TestClock(std::cell::Cell<Duration>);
    impl Clock for TestClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    #[test]
    fn close_observation_waits_through_transient_states_for_actual_exit() {
        let clock = TestClock::default();
        let mut sequence = [
            ProcessState::Alive,
            ProcessState::Replaced,
            ProcessState::Unknown(HostError::new("process birth", HostErrorKind::Unknown)),
            ProcessState::Exited,
        ]
        .into_iter();
        let result = observe_native_exit(
            StepOutcome::Confirmed,
            &clock,
            Duration::from_secs(30),
            Duration::from_millis(100),
            || sequence.next().expect("no sample after Exited"),
        );
        assert_eq!(result, ProcessState::Exited);
        assert_eq!(clock.now(), Duration::from_millis(300));
        assert!(sequence.next().is_none());
    }

    #[test]
    fn close_observation_is_bounded_and_never_turns_nonexit_into_success() {
        for state in [
            ProcessState::Alive,
            ProcessState::Replaced,
            ProcessState::Unknown(HostError::new("process birth", HostErrorKind::Unknown)),
        ] {
            let clock = TestClock::default();
            let mut samples = 0;
            let result = observe_native_exit(
                StepOutcome::Confirmed,
                &clock,
                Duration::from_millis(250),
                Duration::from_millis(100),
                || {
                    samples += 1;
                    state.clone()
                },
            );
            assert_eq!(result, state);
            assert_eq!(clock.now(), Duration::from_millis(250));
            assert_eq!(samples, 4);
        }
    }

    #[test]
    fn refused_close_and_already_exited_do_not_start_a_wait_loop() {
        for (outcome, state) in [
            (StepOutcome::NoEffect, ProcessState::Alive),
            (StepOutcome::Confirmed, ProcessState::Exited),
        ] {
            let clock = TestClock::default();
            let mut samples = 0;
            let result = observe_native_exit(
                outcome,
                &clock,
                Duration::from_secs(30),
                Duration::from_millis(100),
                || {
                    samples += 1;
                    state.clone()
                },
            );
            assert_eq!(result, state);
            assert_eq!(clock.now(), Duration::ZERO);
            assert_eq!(samples, 1);
        }
    }

    #[test]
    fn process_gate_preserves_each_state_and_probe_error_without_weakening_checks() {
        for require_alive in [false, true] {
            for state in [
                ProcessState::Alive,
                ProcessState::Exited,
                ProcessState::Replaced,
                ProcessState::Unknown(HostError::new(
                    "process birth",
                    HostErrorKind::Io(std::io::ErrorKind::PermissionDenied),
                )),
            ] {
                let mut samples = 0;
                let result = check_bound_process(44123, false, require_alive, || {
                    samples += 1;
                    state.clone()
                });
                assert_eq!(samples, 1);
                if state == ProcessState::Alive {
                    assert!(result.is_ok());
                } else {
                    let error = result.unwrap_err();
                    let diagnostic = error.process.as_ref().unwrap();
                    assert_eq!(diagnostic.expected_pid, 44123);
                    assert_eq!(diagnostic.sampled.as_ref(), Some(&state));
                    assert_eq!(diagnostic.require_alive, require_alive);
                    let shown = error.to_string();
                    assert!(shown.contains("stage=check-bound; expected_pid=44123"));
                    assert!(shown.contains(&format!("process_state=Some({state:?})")));
                }
            }
        }
    }

    #[test]
    fn dead_pane_keeps_the_existing_shutdown_and_live_action_short_circuit() {
        for require_alive in [false, true] {
            let result = check_bound_process(44123, true, require_alive, || {
                panic!("a dead pane must not sample a possibly recycled PID")
            });
            if require_alive {
                let error = result.unwrap_err();
                assert_eq!(error.process.as_ref().unwrap().sampled, None);
                assert!(error.to_string().contains("pane_dead=true; require_alive=true"));
            } else {
                assert!(result.is_ok());
            }
        }
    }
}
