use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{HostError, HostErrorKind};
use crate::contract::probe::ProcessIdentity;
use crate::contract::types::Digest;
use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageStamp {
    pub device: u64,
    pub inode: u64,
    pub length: u64,
    pub modified_ns: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessStamp {
    pub identity: ProcessIdentity,
    pub parent: u32,
    pub image: ImageStamp,
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
        })
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
    let (parent, birth, path, image_path, exited) = parts;
    if exited {
        return ProcessState::Exited;
    }
    if parent != expected.parent
        || birth != expected.identity.birth_identity
        || path != expected.identity.executable
    {
        return ProcessState::Replaced;
    }
    #[cfg(not(unix))]
    {
        let _ = image_path;
        ProcessState::Unknown(HostError::new("process sample", HostErrorKind::Unsupported))
    }
    #[cfg(unix)]
    {
        match std::fs::metadata(image_path) {
            Ok(metadata) if image_stamp(&metadata) == expected.image => ProcessState::Alive,
            Ok(_) => ProcessState::Replaced,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProcessState::Exited,
            Err(error) => ProcessState::Unknown(HostError::io("process image metadata", error)),
        }
    }
}
