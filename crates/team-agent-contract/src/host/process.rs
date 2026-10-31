use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{HostError, HostErrorKind};
use crate::contract::probe::ProcessIdentity;
use crate::contract::types::Digest;
use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImageStamp {
    pub device: u64,
    pub inode: u64,
    pub length: u64,
    pub modified_ns: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutableInputStamp {
    pub path: PathBuf,
    pub sha256: Digest,
    pub image: ImageStamp,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessStamp {
    pub identity: ProcessIdentity,
    pub parent: u32,
    pub image: ImageStamp,
    /// Captured interpreter operands / approved launch entry, never process argv/environment reads.
    #[serde(default)]
    pub inputs: Vec<ExecutableInputStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Alive,
    Exited,
    Replaced,
    Unknown(HostError),
}

#[cfg(unix)]
fn image_stamp(metadata: &Metadata) -> ImageStamp {
    use std::os::unix::fs::MetadataExt;
    ImageStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
            + i128::from(metadata.mtime_nsec()),
    }
}

#[cfg(target_os = "linux")]
fn read_limited(path: &Path, limit: usize, operation: &'static str) -> Result<Vec<u8>, HostError> {
    let file = File::open(path).map_err(|e| HostError::io(operation, e))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| HostError::io(operation, e))?;
    if bytes.len() > limit {
        return Err(HostError::new(
            "process identity size",
            HostErrorKind::Invalid,
        ));
    }
    Ok(bytes)
}

/// /proc stat's comm field may contain whitespace and parentheses; field 22 is starttime.
pub fn parse_linux_stat(bytes: &[u8]) -> Result<(u32, String, bool), HostError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| HostError::new("process stat encoding", HostErrorKind::Invalid))?;
    let close = text
        .rfind(')')
        .ok_or_else(|| HostError::new("process stat", HostErrorKind::Invalid))?;
    let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
    let parent = fields
        .get(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| HostError::new("process parent", HostErrorKind::Invalid))?;
    let birth = fields
        .get(19)
        .filter(|value| value.bytes().all(|b| b.is_ascii_digit()) && !value.is_empty())
        .ok_or_else(|| HostError::new("process birth", HostErrorKind::Invalid))?
        .to_string();
    let exited = matches!(fields.first(), Some(&"Z" | &"X"));
    Ok((parent, birth, exited))
}

#[cfg(target_os = "linux")]
fn identity_parts(pid: u32) -> Result<(u32, String, PathBuf, PathBuf, bool), HostError> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let (parent, ticks, exited) =
        parse_linux_stat(&read_limited(&root.join("stat"), 8192, "process stat")?)?;
    let boot = String::from_utf8(read_limited(
        Path::new("/proc/sys/kernel/random/boot_id"),
        128,
        "boot identity",
    )?)
    .map_err(|_| HostError::new("boot identity", HostErrorKind::Unknown))?;
    let image = root.join("exe");
    if exited {
        return Ok((
            parent,
            format!("linux:{}:{ticks}", boot.trim()),
            PathBuf::new(),
            image,
            true,
        ));
    }
    let path = std::fs::read_link(&image).map_err(|e| HostError::io("process image", e))?;
    Ok((
        parent,
        format!("linux:{}:{ticks}", boot.trim()),
        path,
        image,
        exited,
    ))
}

#[cfg(target_os = "macos")]
fn identity_parts(pid: u32) -> Result<(u32, String, PathBuf, PathBuf, bool), HostError> {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::proc_pid::{pidinfo, pidpath};
    let raw =
        i32::try_from(pid).map_err(|_| HostError::new("process pid", HostErrorKind::Invalid))?;
    let info = pidinfo::<BSDInfo>(raw, 0)
        .map_err(|_| HostError::new("process birth", HostErrorKind::Unknown))?;
    let exited = info.pbi_status == 5;
    let path = if exited {
        PathBuf::new()
    } else {
        PathBuf::from(
            pidpath(raw).map_err(|_| HostError::new("process image", HostErrorKind::Unknown))?,
        )
    };
    Ok((
        info.pbi_ppid,
        format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        path.clone(),
        path,
        exited,
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn identity_parts(_: u32) -> Result<(u32, String, PathBuf, PathBuf, bool), HostError> {
    Err(HostError::new(
        "process identity",
        HostErrorKind::Unsupported,
    ))
}

/// Observe the kernel birth of one live PID without authorizing its executable.
/// Callers must separately bind ownership/image; this is only a PID-reuse fence.
pub fn process_birth_identity(pid: u32) -> Result<String, HostError> {
    if pid <= 1 {
        return Err(HostError::new("process pid", HostErrorKind::Invalid));
    }
    let (_, birth, _, _, exited) = identity_parts(pid)?;
    if exited {
        return Err(HostError::new("process birth", HostErrorKind::Unknown));
    }
    Ok(birth)
}

/// Hash an already selected regular file with a finite byte/application-time budget.
/// Callers canonicalize executable aliases deliberately; config/journal paths use no-follow ports.
pub fn fingerprint_file(
    path: &Path,
    max_bytes: u64,
    budget: Duration,
) -> Result<Digest, HostError> {
    let started = Instant::now();
    let mut file = File::open(path).map_err(|e| HostError::io("file fingerprint", e))?;
    fingerprint_open_file(
        &mut file,
        max_bytes,
        budget.saturating_sub(started.elapsed()),
    )
}

pub(crate) fn fingerprint_open_file(
    file: &mut File,
    max_bytes: u64,
    budget: Duration,
) -> Result<Digest, HostError> {
    let started = Instant::now();
    let metadata = file
        .metadata()
        .map_err(|e| HostError::io("file fingerprint metadata", e))?;
    if !metadata.is_file() || metadata.len() > max_bytes || budget.is_zero() {
        return Err(HostError::new(
            "file fingerprint bounds",
            HostErrorKind::Invalid,
        ));
    }
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 65536];
    let mut total = 0_u64;
    loop {
        if started.elapsed() >= budget {
            return Err(HostError::new("file fingerprint", HostErrorKind::Deadline));
        }
        let count = file
            .read(&mut chunk)
            .map_err(|e| HostError::io("file fingerprint", e))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| HostError::new("file fingerprint size", HostErrorKind::Invalid))?;
        if total > max_bytes {
            return Err(HostError::new(
                "file fingerprint size",
                HostErrorKind::Invalid,
            ));
        }
        hash.update(&chunk[..count]);
    }
    let after = file
        .metadata()
        .map_err(|e| HostError::io("file fingerprint metadata", e))?;
    #[cfg(unix)]
    if image_stamp(&metadata) != image_stamp(&after) {
        return Err(HostError::new(
            "file changed during fingerprint",
            HostErrorKind::Conflict,
        ));
    }
    Ok(Digest(hash.finalize().into()))
}

/// Snapshot declared interpreter operands before spawn. The same generic file
/// fingerprint/stamp checks used for images apply; no provider-specific paths.
pub fn capture_executable_inputs(
    inputs: &[crate::contract::plan::ExecutableInput],
    max_bytes: u64,
    budget: Duration,
) -> Result<Vec<ExecutableInputStamp>, HostError> {
    #[cfg(not(unix))]
    {
        let _ = (inputs, max_bytes, budget);
        Err(HostError::new("interpreter inputs", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        let started = Instant::now();
        let mut stamps = Vec::new();
        for input in inputs {
            stamps.push(capture_launch_file(&input.path, input.sha256, max_bytes,
                budget.saturating_sub(started.elapsed()))?);
        }
        Ok(stamps)
    }
}

/// Byte/stamp fence for an explicitly declared executable operand or exec-through
/// launcher. No interpreter argv or wrapper authentication configuration is read.
pub fn capture_launch_file(
    path: &Path, expected: Digest, max_bytes: u64, budget: Duration,
) -> Result<ExecutableInputStamp, HostError> {
    #[cfg(not(unix))]
    { let _ = (path, expected, max_bytes, budget); Err(HostError::new("launch file", HostErrorKind::Unsupported)) }
    #[cfg(unix)]
    {
        let before = std::fs::symlink_metadata(path)
            .map_err(|error| HostError::io("launch file metadata", error))?;
        if !before.is_file() || path.canonicalize().ok().as_deref() != Some(path) {
            return Err(HostError::new("launch file identity", HostErrorKind::Ownership));
        }
        let sha256 = fingerprint_file(path, max_bytes, budget)?;
        let after = std::fs::symlink_metadata(path)
            .map_err(|error| HostError::io("launch file metadata", error))?;
        if sha256 != expected || image_stamp(&before) != image_stamp(&after) {
            return Err(HostError::new("launch file changed", HostErrorKind::Conflict));
        }
        Ok(ExecutableInputStamp { path: path.into(), sha256, image: image_stamp(&after) })
    }
}

/// Pre-spawn fence for the bare-name exec. PATH must still select the planned
/// entry and the bytes behind it must be unchanged, so the shell's own lookup
/// runs what was probed. The stamp covers the file the entry resolves to.
pub fn capture_public_command(
    entry: &crate::contract::plan::LaunchEntrypoint, max_bytes: u64, budget: Duration,
) -> Result<ExecutableInputStamp, HostError> {
    entry.validate().map_err(|_| HostError::new("public launch command", HostErrorKind::Invalid))?;
    let resolved = super::command::PublicCommand::resolve(&entry.command, &entry.search_path)?;
    if resolved.entry != entry.executable || resolved.search_path != entry.search_path {
        return Err(HostError::new("public command PATH route changed", HostErrorKind::Conflict));
    }
    let target = entry.executable.canonicalize()
        .map_err(|error| HostError::io("public command resolution", error))?;
    capture_launch_file(&target, entry.sha256, max_bytes, budget)
}

/// Recheck the captured inode/size/mtime, not a hash of large code on every poll.
pub fn verify_executable_inputs(inputs: &[ExecutableInputStamp]) -> Result<(), HostError> {
    #[cfg(not(unix))]
    {
        let _ = inputs;
        Err(HostError::new("interpreter inputs", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        for input in inputs {
            let metadata = std::fs::symlink_metadata(&input.path)
                .map_err(|error| HostError::io("interpreter input metadata", error))?;
            if !metadata.is_file() || image_stamp(&metadata) != input.image {
                return Err(HostError::new("interpreter input changed", HostErrorKind::Conflict));
            }
        }
        Ok(())
    }
}

pub fn resolve_cwd(path: &Path) -> Result<crate::contract::session::CwdIdentity, HostError> {
    let path = path
        .canonicalize()
        .map_err(|e| HostError::io("cwd identity", e))?;
    let metadata = std::fs::metadata(&path).map_err(|e| HostError::io("cwd identity", e))?;
    if !metadata.is_dir() {
        return Err(HostError::new("cwd identity", HostErrorKind::Invalid));
    }
    #[cfg(not(unix))]
    {
        Err(HostError::new("cwd identity", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut hash = Sha256::new();
        hash.update(b"team-agent-contract-cwd-v1\0");
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update(metadata.dev().to_le_bytes());
        hash.update(metadata.ino().to_le_bytes());
        Ok(crate::contract::session::CwdIdentity {
            path,
            identity: Digest(hash.finalize().into()),
        })
    }
}

/// Capture once before readiness; later samples compare birth/image metadata without hashing
/// a large executable on every terminal poll. No name/argv scan and no protected data store.
pub fn capture_process(
    pid: u32,
    expected_executable: &Path,
    expected_sha256: Digest,
    budget: Duration,
) -> Result<ProcessStamp, HostError> {
    if pid == 0 || budget.is_zero() {
        return Err(HostError::new("process capture", HostErrorKind::Invalid));
    }
    let started = Instant::now();
    let (parent, birth, path, image_path, exited) = identity_parts(pid)?;
    if exited {
        return Err(HostError::new("process exited", HostErrorKind::Unknown));
    }
    let expected = expected_executable
        .canonicalize()
        .map_err(|e| HostError::io("executable identity", e))?;
    if path != expected {
        return Err(HostError::new(
            "process executable",
            HostErrorKind::Ownership,
        ));
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, birth, image_path, expected_sha256, started);
        Err(HostError::new(
            "process capture",
            HostErrorKind::Unsupported,
        ))
    }
    #[cfg(unix)]
    {
        let mut image =
            File::open(&image_path).map_err(|e| HostError::io("process image open", e))?;
        let before = image_stamp(
            &image
                .metadata()
                .map_err(|e| HostError::io("process image metadata", e))?,
        );
        let actual = fingerprint_open_file(
            &mut image,
            before.length,
            budget.saturating_sub(started.elapsed()),
        )?;
        let after = image_stamp(
            &image
                .metadata()
                .map_err(|e| HostError::io("process image metadata", e))?,
        );
        let fresh = identity_parts(pid)?;
        if actual != expected_sha256
            || before != after
            || parent != fresh.0
            || birth != fresh.1
            || path != fresh.2
            || fresh.4
        {
            return Err(HostError::new(
                "process capture changed",
                HostErrorKind::Ownership,
            ));
        }
        Ok(ProcessStamp {
            identity: ProcessIdentity {
                pid,
                birth_identity: birth,
                executable: path,
                executable_sha256: actual,
            },
            parent,
            image: after,
            inputs: Vec::new(),
        })
    }
}

/// At most three parent edges: the native process may own up to two live
/// intermediary helpers. No process-name matching, ambient process scan or cache.
pub const MAX_NATIVE_PARENT_HOPS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
struct AncestorLink {
    parent: u32,
    birth: String,
    executable: PathBuf,
}

fn ancestor_link(pid: u32) -> Result<AncestorLink, HostError> {
    let (parent, birth, executable, _, exited) = identity_parts(pid)?;
    if exited {
        return Err(HostError::new(
            "native ancestor exited",
            HostErrorKind::Ownership,
        ));
    }
    Ok(AncestorLink {
        parent,
        birth,
        executable,
    })
}

/// Re-establish the relationship on every use. Both captured endpoint processes
/// must still have their original birth/image/parent, and each intermediary's
/// birth, parent and executable must agree before and after the bounded walk.
/// A numeric ancestor PID alone never establishes authority.
pub fn verify_native_ancestry(
    process: &ProcessStamp,
    native: &ProcessStamp,
) -> Result<Vec<u32>, HostError> {
    verify_ancestry_with(process, native, sample_process, ancestor_link)
}

fn ancestry_alive(state: ProcessState) -> Result<(), HostError> {
    match state {
        ProcessState::Alive => Ok(()),
        ProcessState::Unknown(error) => Err(error),
        _ => Err(HostError::new(
            "native ancestor identity",
            HostErrorKind::Ownership,
        )),
    }
}

fn verify_ancestry_with(
    process: &ProcessStamp,
    native: &ProcessStamp,
    mut sample: impl FnMut(&ProcessStamp) -> ProcessState,
    mut read: impl FnMut(u32) -> Result<AncestorLink, HostError>,
) -> Result<Vec<u32>, HostError> {
    let invalid = || HostError::new("native parent chain", HostErrorKind::Ownership);
    if process.identity.pid == 0
        || native.identity.pid == 0
        || process.identity.pid == native.identity.pid
    {
        return Err(invalid());
    }
    ancestry_alive(sample(process))?;
    ancestry_alive(sample(native))?;
    let mut parent = process.parent;
    let mut chain = Vec::new();
    let mut snapshots: Vec<(u32, AncestorLink)> = Vec::new();
    for _ in 0..MAX_NATIVE_PARENT_HOPS {
        if parent == 0 || parent == process.identity.pid || chain.contains(&parent) {
            return Err(invalid());
        }
        chain.push(parent);
        if parent == native.identity.pid {
            for (pid, before) in snapshots.iter().rev() {
                if read(*pid)? != *before {
                    return Err(HostError::new(
                        "native ancestor changed",
                        HostErrorKind::Ownership,
                    ));
                }
            }
            ancestry_alive(sample(process))?;
            ancestry_alive(sample(native))?;
            return Ok(chain);
        }
        if chain.len() == MAX_NATIVE_PARENT_HOPS {
            break;
        }
        let link = read(parent)?;
        let next = link.parent;
        snapshots.push((parent, link));
        parent = next;
    }
    Err(HostError::new(
        "native parent depth",
        HostErrorKind::Ownership,
    ))
}

/// Partial-spawn recovery may observe absence without a native image receipt.
/// A live PID requires its previously captured birth; it never grants image authority.
pub fn sample_spawn_birth(pid: u32, expected_birth: Option<&str>) -> ProcessState {
    #[cfg(unix)]
    {
        let Some(raw) = i32::try_from(pid).ok().filter(|pid| *pid > 1) else {
            return ProcessState::Unknown(HostError::new("spawn pid", HostErrorKind::Invalid));
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None) {
            Err(nix::errno::Errno::ESRCH) => return ProcessState::Exited,
            Err(error) => return ProcessState::Unknown(HostError::io("spawn existence", error.into())),
            Ok(()) => {}
        }
    }
    match identity_parts(pid) {
        Ok((_, _, _, _, true)) => ProcessState::Exited,
        Ok((_, birth, _, _, false)) => match expected_birth {
            Some(expected) if expected == birth => ProcessState::Alive,
            Some(_) => ProcessState::Replaced,
            None => ProcessState::Unknown(HostError::new("spawn birth not captured", HostErrorKind::Ownership)),
        },
        Err(error) => ProcessState::Unknown(error),
    }
}

pub fn sample_process(expected: &ProcessStamp) -> ProcessState {
    #[cfg(unix)]
    {
        let Ok(raw) = i32::try_from(expected.identity.pid) else {
            return ProcessState::Unknown(HostError::new("process pid", HostErrorKind::Invalid));
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None) {
            Err(nix::errno::Errno::ESRCH) => return ProcessState::Exited,
            Err(error) => {
                return ProcessState::Unknown(HostError::io("process existence", error.into()))
            }
            Ok(()) => {}
        }
    }
    let parts = match identity_parts(expected.identity.pid) {
        Ok(parts) => parts,
        Err(error)
            if error.kind == HostErrorKind::Io(std::io::ErrorKind::NotFound)
                && matches!(error.operation, "process stat" | "process image") =>
        {
            return ProcessState::Exited
        }
        Err(error) => return ProcessState::Unknown(error),
    };
    let (parent, birth, _path, image_path, exited) = parts;
    if exited {
        return ProcessState::Exited;
    }
    if parent != expected.parent || birth != expected.identity.birth_identity {
        return ProcessState::Replaced;
    }
    // proc_pidpath may report another hard link to the same live executable.
    // Path spelling is not image identity: the sampled image must still match
    // every recorded stamp field below (device/inode/length/modified_ns).
    #[cfg(not(unix))]
    {
        let _ = image_path;
        ProcessState::Unknown(HostError::new("process sample", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        match std::fs::metadata(image_path) {
            Ok(metadata) if image_stamp(&metadata) == expected.image => {
                match verify_executable_inputs(&expected.inputs) {
                    Ok(()) => ProcessState::Alive,
                    Err(error) if matches!(error.kind, HostErrorKind::Conflict | HostErrorKind::Io(std::io::ErrorKind::NotFound)) => ProcessState::Replaced,
                    Err(error) => ProcessState::Unknown(error),
                }
            }
            Ok(_) => ProcessState::Replaced,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProcessState::Exited,
            Err(error) => ProcessState::Unknown(HostError::io("process image metadata", error)),
        }
    }
}

#[cfg(test)]
mod ancestry_tests {
    use super::*;

    #[test]
    fn partial_spawn_birth_never_adopts_live_missing_or_reused_identity() {
        let pid = std::process::id();
        let birth = process_birth_identity(pid).unwrap();
        assert_eq!(sample_spawn_birth(pid, Some(&birth)), ProcessState::Alive);
        assert_eq!(sample_spawn_birth(pid, Some("foreign-birth")), ProcessState::Replaced);
        assert!(matches!(sample_spawn_birth(pid, None), ProcessState::Unknown(_)));
        assert!(matches!(sample_spawn_birth(0, None), ProcessState::Unknown(_)));
    }

    #[test]
    fn process_birth_identity_is_readonly_and_rejects_invalid_or_dead_pid() {
        assert!(process_birth_identity(0).is_err());
        assert!(process_birth_identity(1).is_err());
        assert!(process_birth_identity(u32::MAX).is_err());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let current = process_birth_identity(std::process::id()).unwrap();
            assert!(!current.is_empty());
            assert_eq!(current, process_birth_identity(std::process::id()).unwrap());
            let mut child = std::process::Command::new("/bin/sleep").arg("2").spawn().unwrap();
            let pid = child.id();
            let born = process_birth_identity(pid).unwrap();
            assert_ne!(born, current);
            assert!(child.wait().unwrap().success());
            assert!(process_birth_identity(pid).is_err());
        }
    }
    use std::collections::BTreeMap;

    fn stamp(pid: u32, parent: u32) -> ProcessStamp {
        ProcessStamp {
            inputs: vec![],
            identity: ProcessIdentity {
                pid,
                birth_identity: format!("birth-{pid}"),
                executable: "/fixture/image".into(),
                executable_sha256: super::super::digest(b"fixture"),
            },
            parent,
            image: ImageStamp {
                device: 1,
                inode: 1,
                length: 1,
                modified_ns: 1,
            },
        }
    }
    fn link(parent: u32) -> AncestorLink {
        AncestorLink {
            parent,
            birth: "helper-birth".into(),
            executable: "/fixture/helper".into(),
        }
    }

    #[test]
    fn direct_and_two_helper_chains_recheck_each_birth_and_both_endpoints() {
        for chain in [vec![10], vec![20, 10], vec![30, 20, 10]] {
            let process = stamp(100, chain[0]);
            let native = stamp(10, 1);
            let graph: BTreeMap<_, _> = chain.windows(2).map(|p| (p[0], link(p[1]))).collect();
            let mut samples = 0;
            let mut reads = Vec::new();
            let result = verify_ancestry_with(
                &process,
                &native,
                |_| {
                    samples += 1;
                    ProcessState::Alive
                },
                |pid| {
                    reads.push(pid);
                    Ok(graph[&pid].clone())
                },
            )
            .unwrap();
            assert_eq!(result, chain);
            assert_eq!(samples, 4);
            let mut expected = chain[..chain.len() - 1].to_vec();
            expected.extend(chain[..chain.len() - 1].iter().rev());
            assert_eq!(reads, expected);
        }
    }

    #[test]
    fn deeper_foreign_cyclic_and_missing_chains_never_gain_authority() {
        for pairs in [
            vec![(40, 30), (30, 20), (20, 10)], // fourth edge cannot be followed
            vec![(40, 30), (30, 20), (20, 1)],
            vec![(40, 30), (30, 40)],
            vec![(40, 100)],
            vec![(40, 0)],
            vec![],
        ] {
            let mut reads = 0;
            assert!(verify_ancestry_with(
                &stamp(100, 40),
                &stamp(10, 1),
                |_| ProcessState::Alive,
                |pid| {
                    reads += 1;
                    pairs
                        .iter()
                        .find(|(id, _)| *id == pid)
                        .map(|(_, parent)| link(*parent))
                        .ok_or_else(|| HostError::new("missing helper", HostErrorKind::Unknown))
                },
            )
            .is_err());
            assert!(reads <= 2);
        }
    }

    #[test]
    fn helper_reparent_pid_reuse_and_exec_during_walk_are_rejected() {
        let original = link(10);
        let mut changed = [original.clone(), original.clone(), original.clone()];
        changed[0].parent = 999;
        changed[1].birth = "reused-pid".into();
        changed[2].executable = "/replacement".into();
        for changed in changed {
            let mut reads = 0;
            let error = verify_ancestry_with(
                &stamp(100, 20),
                &stamp(10, 1),
                |_| ProcessState::Alive,
                |_| {
                    reads += 1;
                    Ok(if reads == 1 {
                        original.clone()
                    } else {
                        changed.clone()
                    })
                },
            )
            .unwrap_err();
            assert_eq!(error.operation, "native ancestor changed");
        }
    }

    #[test]
    fn both_endpoint_samples_fail_closed_for_exit_replacement_and_unknown() {
        for bad_at in 1..=4 {
            for state in [
                ProcessState::Exited,
                ProcessState::Replaced,
                ProcessState::Unknown(HostError::new("denied probe", HostErrorKind::Unknown)),
            ] {
                let mut calls = 0;
                assert!(verify_ancestry_with(
                    &stamp(100, 10),
                    &stamp(10, 1),
                    |_| {
                        calls += 1;
                        if calls == bad_at {
                            state.clone()
                        } else {
                            ProcessState::Alive
                        }
                    },
                    |_| panic!("direct parent must not read intermediaries"),
                )
                .is_err());
            }
        }
    }

    #[test]
    fn null_self_and_failed_recheck_cannot_become_a_native_parent() {
        for (child, native) in [
            (stamp(0, 10), stamp(10, 1)),
            (stamp(100, 0), stamp(10, 1)),
            (stamp(100, 10), stamp(0, 1)),
            (stamp(10, 10), stamp(10, 1)),
        ] {
            assert!(verify_ancestry_with(
                &child,
                &native,
                |_| ProcessState::Alive,
                |_| Err(HostError::new("unreadable", HostErrorKind::Unknown)),
            )
            .is_err());
        }
        let mut reads = 0;
        assert!(verify_ancestry_with(
            &stamp(100, 20),
            &stamp(10, 1),
            |_| ProcessState::Alive,
            |_| {
                reads += 1;
                if reads == 1 {
                    Ok(link(10))
                } else {
                    Err(HostError::new("vanished helper", HostErrorKind::Unknown))
                }
            },
        )
        .is_err());
    }
}
