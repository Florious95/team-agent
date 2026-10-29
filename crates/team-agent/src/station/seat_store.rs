//! C4 storage: one manifest and one stable lock inode per addressed seat.
//! Reads never create directories, migrate state, or consult SQLite.
use super::codec;
use crate::platform::file_lock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatScope {
    pub workspace: String,
    pub team: String,
    pub seat: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeatManifest {
    pub v: u8,
    pub kind: String,
    #[serde(default)]
    pub requires: Vec<String>,
    pub scope: SeatScope,
    pub instance_id: String,
    pub generation: u64,
    pub desired: String,
    pub phase: String,
    /// Native launch specification; only the selected provider adapter interprets it.
    pub launch: Value,
    pub endpoint: Option<Value>,
    pub last_operation_id: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct SeatStore {
    root: PathBuf,
    pub scope: SeatScope,
}

pub struct SeatLock {
    _lock: std::rc::Rc<crate::platform::entity_lock::EntityLock>,
    store: SeatStore,
}

impl SeatStore {
    /// `workspace_id` comes from the immutable workspace binding, not a PID or version.
    pub fn new(workspace: &Path, scope: SeatScope) -> Result<Self, String> {
        component(&scope.team)?;
        component(&scope.seat)?;
        if scope.workspace.is_empty() {
            return Err("missing workspace identity".into());
        }
        Ok(Self {
            root: workspace.canonicalize().map_err(|e| e.to_string())?.join(".team/station-v1").join(&scope.team),
            scope,
        })
    }

    pub fn path(&self) -> PathBuf {
        self.root.join("seats").join(format!("{}.json", self.scope.seat))
    }

    pub fn read(&self) -> Result<Option<SeatManifest>, String> {
        let Some(value) = read_json(&self.path(), MAX_MANIFEST_BYTES)? else {
            return Ok(None);
        };
        let manifest: SeatManifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if manifest.scope != self.scope {
            return Err("seat_scope_mismatch".into());
        }
        if manifest.kind != "seat_manifest" { return Err("unsupported_seat_kind".into()); }
        Ok(Some(manifest))
    }

    pub fn lock(&self) -> Result<SeatLock, String> {
        let directory = self.root.join("locks");
        private_dir(&directory)?;
        let path = directory.join(format!("seat.{}.lock", self.scope.seat));
        if let Some(lock) = crate::platform::entity_lock::EntityLock::current(&path) {
            return Ok(SeatLock { _lock: lock, store: self.clone() });
        }
        let file = open_file(&path, false)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file_lock::try_lock_once_nonblocking(&file) {
                Ok(true) => {
                    let lock = crate::platform::entity_lock::EntityLock::acquired(&path, file)
                        .map_err(|error| error.to_string())?;
                    return Ok(SeatLock { _lock: lock, store: self.clone() });
                }
                Ok(false) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(false) => return Err(format!("seat_busy: {}", self.scope.seat)),
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

impl SeatLock {
    pub(crate) fn scope(&self) -> &SeatScope { &self.store.scope }

    pub fn read(&self) -> Result<Option<SeatManifest>, String> {
        self.store.read()
    }

    pub fn publish(&self, manifest: &SeatManifest) -> Result<(), String> {
        if manifest.scope != self.store.scope {
            return Err("seat_scope_mismatch".into());
        }
        let reserved = ["v", "kind", "requires", "scope", "instance_id", "generation", "desired",
            "phase", "launch", "endpoint", "last_operation_id", "updated_at"];
        if manifest.extra.keys().any(|key| reserved.contains(&key.as_str())) {
            return Err("reserved_manifest_field_in_extra".into());
        }
        super::lifecycle::validate_manifest(manifest)?;
        let value = serde_json::to_value(manifest).map_err(|e| e.to_string())?;
        codec::authorize_mutation(&value, &[])?;
        write_json(&self.store.path(), &value, MAX_MANIFEST_BYTES)
    }

    pub fn operation(&self, id: &str) -> Result<Option<Value>, String> {
        read_json(&self.operation_path(id)?, MAX_MANIFEST_BYTES * 3)
    }

    pub fn publish_operation(&self, id: &str, value: &Value) -> Result<(), String> {
        codec::authorize_mutation(value, super::lifecycle::OPERATION_CAPABILITIES)?;
        write_json(&self.operation_path(id)?, value, MAX_MANIFEST_BYTES * 3)
    }

    /// Retain the last 16 operations, pinning interrupted work. Unknown/corrupt
    /// records are never guessed at or deleted to satisfy a storage budget.
    pub(crate) fn prune_operations(&self) -> Result<(), String> {
        let directory = self.store.root.join("operations").join(&self.store.scope.seat);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let expected_scope = serde_json::to_value(&self.store.scope).map_err(|e| e.to_string())?;
        let pinned = self.read()?.map(|seat| seat.last_operation_id);
        let mut closed = Vec::new();
        let mut interrupted = 0usize;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let Ok(Some(value)) = read_json(&path, MAX_MANIFEST_BYTES * 3) else { continue; };
            if value.get("kind").and_then(Value::as_str) != Some("seat_operation")
                || value.pointer("/request/operation_id").and_then(Value::as_str) != path.file_stem().and_then(|s| s.to_str())
                || value.pointer("/after/scope") != Some(&expected_scope)
                || codec::authorize_mutation(&value, super::lifecycle::OPERATION_CAPABILITIES).is_err()
            { continue; }
            if !matches!(value.get("phase").and_then(Value::as_str), Some("completed" | "cancelled")) {
                interrupted += 1;
                continue;
            }
            let generation = value.pointer("/after/generation").and_then(Value::as_u64).unwrap_or(0);
            let modified = entry.metadata().and_then(|metadata| metadata.modified()).map_err(|e| e.to_string())?;
            let is_pinned = path.file_stem().and_then(|s| s.to_str()) == pinned.as_deref();
            closed.push((is_pinned, generation, modified, path));
        }
        closed.sort();
        let count = closed.len().saturating_sub(16usize.saturating_sub(interrupted));
        for (pinned, _, _, path) in closed.into_iter().take(count) {
            if !pinned { fs::remove_file(path).map_err(|e| e.to_string())?; }
        }
        if count > 0 { sync_directory(&directory)?; }
        Ok(())
    }

    fn operation_path(&self, id: &str) -> Result<PathBuf, String> {
        component(id)?;
        Ok(self.store.root.join("operations").join(&self.store.scope.seat).join(format!("{id}.json")))
    }
}

pub(crate) fn component(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 || matches!(value, "." | "..")
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("invalid seat path component".into());
    }
    Ok(())
}

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => return Ok(()),
        Ok(_) => return Err(format!("unsafe_directory: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    let parent = path.parent().ok_or("directory has no parent")?;
    private_dir(parent)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if let Err(e) = builder.create(path) {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e.to_string());
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("unsafe_directory".into());
    }
    sync_directory(parent)
}

fn open_file(path: &Path, exclusive: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive { options.create_new(true); } else { options.create(true); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path).map_err(|e| e.to_string())
}

pub(super) fn read_json(path: &Path, limit: u64) -> Result<Option<Value>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("invalid seat document".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit { return Err("seat document too large".into()); }
    codec::read(&bytes).map(Some)
}

pub(super) fn write_json(path: &Path, value: &Value, limit: u64) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit { return Err("seat document too large".into()); }
    let parent = path.parent().ok_or("document has no parent")?;
    private_dir(parent)?;
    let temp = parent.join(format!(".station-{}.tmp", random_instance()?));
    let result = (|| {
        let mut file = open_file(&temp, true)?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        sync_directory(parent)
    })();
    // Cleanup only this call's own temporary file, never a directory-wide glob.
    let _ = fs::remove_file(temp);
    result
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    { File::open(path).and_then(|file| file.sync_all()).map_err(|e| e.to_string()) }
    #[cfg(not(unix))]
    { let _ = path; Ok(()) }
}

pub(crate) fn random_instance() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    #[cfg(unix)]
    File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(algorithm: *mut std::ffi::c_void, buffer: *mut u8, length: u32, flags: u32) -> i32;
        }
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG; no provider executable or private path lookup.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), 16, 2) };
        if status != 0 { return Err(format!("system RNG failed: {status}")); }
    }
    #[cfg(not(any(unix, windows)))]
    return Err("unsupported system RNG".into());
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
