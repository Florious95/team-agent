//!
//! Phase-DX E7 (0.5.9 host-leader-registry-design): file-per-leader discovery index.
//!
//! Location: `~/.team-agent/leaders/<workspace_hash>__<team_key>.json`.
//!
//! Registry entries are a **derived** discovery index — never a source of
//! authority. Read paths must always canonical-validate a resolved entry
//! against the target workspace's runtime state (leader_receiver,
//! owner_epoch, transport_kind) before delivering. Ambiguous short names
//! are refused (`name_ambiguous`) rather than resolved by any priority
//! heuristic.
//!
//! Design constraints (host-leader-registry-design.md §§3, 9):
//! - No shared/global relational store (file-per-team on the host filesystem).
//! - No identity writes on read/send paths — write only happens from
//!   `claim-leader`/`attach-leader`/`takeover`/`attach-app-server-leader`
//!   canonical success hooks and shutdown/unregister on canonical success.
//! - Registry write failure never fails the underlying binding command;
//!   it is discoverability degradation, not binding failure.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::transport::Transport;

/// Current file schema_version for `~/.team-agent/leaders/*.json`.
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// Event name emitted after a successful atomic temp+rename write.
pub const EVENT_REGISTERED: &str = "leader_registry.registered";

/// Event name emitted when the write step (fs::write / rename / dir create)
/// failed — the binding command still returns ok, but discoverability
/// degrades until the next successful canonical hook.
pub const EVENT_WRITE_FAILED: &str = "leader_registry.write_failed";

/// Event name emitted when a canonical shutdown/unbind removed the entry.
pub const EVENT_UNREGISTERED: &str = "leader_registry.unregistered";

/// v1 file schema. Fields are copied verbatim from `LeaderRegistryEntry`
/// during atomic writes; readers deserialize the same shape then validate
/// against canonical state before using any field for routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaderRegistryEntry {
    pub schema_version: u32,
    /// Default delivery name — the runtime `team_key`. Never the display or
    /// spec name (host-leader-registry-design.md §5.1).
    pub delivery_name: String,
    /// Human-friendly qualified name: `<workspace_short>/<team_key>`.
    pub qualified_name: String,
    /// Stable qualified name: `<workspace_hash>/<team_key>`. Always accepted
    /// by resolvers even when the human form is ambiguous.
    pub stable_qualified_name: String,
    /// Explicit aliases attached at binding time. Alias collisions follow
    /// the same `name_ambiguous` rule as delivery_name collisions.
    pub aliases: Vec<String>,
    pub workspace: PathBuf,
    /// sha256-prefix of canonical realpath workspace; used only as a file
    /// name component and stable identifier — never an authority key.
    pub workspace_hash: String,
    pub workspace_short: String,
    pub team_key: String,
    pub transport_kind: String,
    pub channel: serde_json::Value,
    pub owner_epoch: u64,
    pub attached_at: String,
    pub updated_at: String,
    pub source: String,
    pub status: String,
}

impl LeaderRegistryEntry {
    /// Ambiguous-name refusal semantics (host-leader-registry-design.md §5.2):
    /// two entries with the same `delivery_name` remain visible in
    /// `leaders` under `ambiguous_names`, and `send --to-leader <short>`
    /// refuses `name_ambiguous`. Callers surface the ambiguity via
    /// candidates rather than choosing a winner. The design explicitly
    /// forbids priority heuristics based on write order, modification
    /// time, or ambient state — see host-leader-registry-design.md §5.2
    /// for the enumerated exclusions.
    #[must_use]
    pub fn short_name_collision_refuses() -> &'static str {
        "ambiguous"
    }
}

/// Payload key for the list of short-name collisions surfaced by
/// `team-agent leaders` — kept as a top-level constant so both `leaders`
/// and `send --to-leader` refer to the same wire name.
pub const AMBIGUOUS_NAMES_FIELD: &str = "ambiguous_names";

/// Wire reason used by both `leaders` and `send --to-leader` when a name
/// resolves to multiple canonical-live entries. Candidates carry
/// `workspace_hash` + `stable_qualified_name` so the caller can retry with
/// a fully-qualified form.
pub const REASON_AMBIGUOUS: &str = "name_ambiguous";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRollback {
    Restored,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRollbackError {
    InvalidReceipt,
    LockUnavailable,
    ReadFailed,
    RestoreFailed,
    DeleteFailed,
}

impl fmt::Display for RegistryRollbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::InvalidReceipt => "invalid receipt",
            Self::LockUnavailable => "registry lock unavailable",
            Self::ReadFailed => "registry receipt read failed",
            Self::RestoreFailed => "registry restore failed",
            Self::DeleteFailed => "registry delete failed",
        };
        f.write_str(reason)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryWriteReceipt {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryWriteOutcome {
    pub status: &'static str,
    pub path: Option<PathBuf>,
    pub receipt: Option<RegistryWriteReceipt>,
    pub team_key: String,
    pub workspace_hash: String,
    pub source: String,
    pub owner_epoch: u64,
}

impl RegistryWriteOutcome {
    #[must_use]
    pub fn response_json(&self) -> Value {
        match &self.path {
            Some(path) => json!({"status": self.status, "path": path.display().to_string()}),
            None => json!({"status": self.status}),
        }
    }
}

/// Absolute path to `~/.team-agent/leaders`. Returns `None` when the
/// caller has no HOME (e.g. some CI shells) — callers should treat this as
/// "no registry" rather than an error, since registry is derived.
#[must_use]
pub fn registry_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".team-agent").join("leaders"))
}

/// Compute the sha256 hex prefix (12 chars) of a canonical workspace path.
/// The canonical form is used so equivalent paths (symlinks, `.`, etc.)
/// map to the same discovery id. If canonicalize fails the input path is
/// used verbatim — the hash is a stable label, not an authority key.
#[must_use]
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compute the discovery-visible short label for a workspace path —
/// basename by default, "workspace" when the path has no name component.
#[must_use]
fn workspace_short(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "workspace".to_string())
}

/// Assemble a schema-v1 entry from canonical binding fields. Callers use
/// this from the binding-command success hooks; the entry then flows into
/// [`write_entry_best_effort`].
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_entry(
    workspace: &Path,
    team_key: &str,
    transport_kind: &str,
    channel: serde_json::Value,
    owner_epoch: u64,
    source: &str,
    now_rfc3339: String,
) -> LeaderRegistryEntry {
    let hash = workspace_hash(workspace);
    let short = workspace_short(workspace);
    LeaderRegistryEntry {
        schema_version: REGISTRY_SCHEMA_VERSION,
        delivery_name: team_key.to_string(),
        qualified_name: format!("{short}/{team_key}"),
        stable_qualified_name: format!("{hash}/{team_key}"),
        aliases: Vec::new(),
        workspace: std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf()),
        workspace_hash: hash,
        workspace_short: short,
        team_key: team_key.to_string(),
        transport_kind: transport_kind.to_string(),
        channel,
        owner_epoch,
        attached_at: now_rfc3339.clone(),
        updated_at: now_rfc3339,
        source: source.to_string(),
        status: "attached".to_string(),
    }
}

/// After a canonical leader binding succeeds, write the derived host
/// discovery entry from the just-persisted state. Registry failure is
/// non-fatal: callers get a write_failed outcome and the binding remains
/// successful.
pub fn register_binding_from_state_best_effort(
    workspace: &Path,
    team: Option<&str>,
    source: &str,
) -> Option<RegistryWriteOutcome> {
    let Ok(state) = crate::state::persist::load_runtime_state_without_migrations(workspace) else {
        return None;
    };
    let team_key = match team.filter(|team| !team.is_empty()) {
        Some(team) => team.to_string(),
        None => crate::state::projection::team_state_key(&state),
    };
    let receiver = state
        .get("teams")
        .and_then(|value| value.as_object())
        .and_then(|teams| teams.get(&team_key))
        .and_then(|team| team.get("leader_receiver"))
        .or_else(|| state.get("leader_receiver"))
        .cloned()?;
    let transport_kind = receiver
        .get("transport_kind")
        .and_then(Value::as_str)
        .unwrap_or("direct_tmux")
        .to_string();
    let owner_epoch = receiver
        .get("owner_epoch")
        .and_then(Value::as_u64)
        .or_else(|| {
            state
                .get("teams")
                .and_then(|value| value.as_object())
                .and_then(|teams| teams.get(&team_key))
                .and_then(|team| team.get("owner_epoch"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let entry = build_entry(
        workspace,
        &team_key,
        &transport_kind,
        receiver,
        owner_epoch,
        source,
        chrono::Utc::now().to_rfc3339(),
    );
    let event_log = crate::event_log::EventLog::new(workspace);
    let write_receipt = write_entry_best_effort_with_receipt(&entry);
    let write_result = write_receipt.as_ref().map(|receipt| receipt.path.clone());
    let status = if let Some(path) = &write_result {
        let _ = event_log.write(
            EVENT_REGISTERED,
            json!({
                "path": path.display().to_string(),
                "team_key": team_key,
                "workspace_hash": entry.workspace_hash,
                "source": source,
            }),
        );
        "registered"
    } else {
        let _ = event_log.write(
            EVENT_WRITE_FAILED,
            json!({
                "team_key": team_key,
                "workspace_hash": entry.workspace_hash,
                "source": source,
            }),
        );
        "write_failed"
    };
    Some(RegistryWriteOutcome {
        status,
        path: write_result,
        receipt: write_receipt,
        team_key,
        workspace_hash: entry.workspace_hash,
        source: source.to_string(),
        owner_epoch: entry.owner_epoch,
    })
}

fn entry_filename(entry: &LeaderRegistryEntry) -> String {
    format!("{}__{}.json", entry.workspace_hash, entry.team_key)
}

/// Registry write is best-effort. Writes to `.<file>.tmp-<pid>-<counter>`
/// first, then atomically renames to `<workspace_hash>__<team_key>.json`.
/// Every writer uses the registry lock so conditional rollback can compare
/// and restore without racing a concurrent publish.
pub fn write_entry_best_effort(entry: &LeaderRegistryEntry) -> Option<PathBuf> {
    write_entry_best_effort_with_receipt(entry).map(|receipt| receipt.path)
}

fn write_entry_best_effort_with_receipt(
    entry: &LeaderRegistryEntry,
) -> Option<RegistryWriteReceipt> {
    with_registry_lock(|dir| write_entry_locked(dir, entry)).flatten()
}

fn write_entry_locked(dir: &Path, entry: &LeaderRegistryEntry) -> Option<RegistryWriteReceipt> {
    let final_path = dir.join(entry_filename(entry));
    let tmp_name = format!(
        ".{}.tmp-{}-{}",
        entry_filename(entry),
        std::process::id(),
        rand_suffix()
    );
    let tmp_path = dir.join(tmp_name);
    let serialized = serde_json::to_vec_pretty(entry).ok()?;
    if std::fs::write(&tmp_path, &serialized).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    if std::fs::rename(&tmp_path, &final_path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    Some(RegistryWriteReceipt {
        path: final_path,
        bytes: serialized,
    })
}

/// Restore one registry path only when it still contains this writer's exact
/// receipt. The comparison and restore/delete are held under the same lock as
/// ordinary registry writers, so a winner cannot slip in between them.
pub fn restore_entry_if_current_matches(
    receipt: &RegistryWriteReceipt,
    previous: Option<&[u8]>,
) -> Result<RegistryRollback, RegistryRollbackError> {
    with_registry_lock_result(|dir| {
        if receipt.path.parent() != Some(dir) {
            return Err(RegistryRollbackError::InvalidReceipt);
        }
        let current =
            std::fs::read(&receipt.path).map_err(|_| RegistryRollbackError::ReadFailed)?;
        if current != receipt.bytes {
            return Ok(RegistryRollback::Superseded);
        }
        match previous {
            Some(bytes) if replace_entry_bytes_locked(&receipt.path, bytes) => {
                Ok(RegistryRollback::Restored)
            }
            Some(_) => Err(RegistryRollbackError::RestoreFailed),
            None if std::fs::remove_file(&receipt.path).is_ok() => Ok(RegistryRollback::Restored),
            None => Err(RegistryRollbackError::DeleteFailed),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPruneItem {
    pub workspace: PathBuf,
    pub team_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPruneReport {
    pub dry_run: bool,
    pub candidates: Vec<RegistryPruneItem>,
    pub removed: Vec<RegistryPruneItem>,
    pub kept: Vec<RegistryPruneItem>,
    pub skipped: Vec<RegistryPruneItem>,
    pub errors: Vec<RegistryPruneItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PruneAttempt {
    Removed,
    Skipped(&'static str),
    Error,
}

fn prune_item(entry: &LeaderRegistryEntry, reason: impl Into<String>) -> RegistryPruneItem {
    RegistryPruneItem {
        workspace: entry.workspace.clone(),
        team_key: entry.team_key.clone(),
        reason: reason.into(),
    }
}

fn prune_reason_from_state(entry: &LeaderRegistryEntry, state: &Value) -> Option<&'static str> {
    let (status, reason) = classify_loaded(entry, state);
    if status != "STALE" {
        return None;
    }
    match reason.as_deref() {
        Some("team_key_not_found") => Some("team_key_not_found"),
        Some("team_not_alive") => Some("team_not_alive"),
        _ => None,
    }
}

fn prune_reason(
    entry: &LeaderRegistryEntry,
    status: &str,
    reason: Option<&str>,
) -> Option<&'static str> {
    if status != "STALE" || !crate::state::persist::runtime_state_path(&entry.workspace).is_file() {
        // A missing state file is unavailable rather than proof that the
        // workspace was retired. Only an existing, readable canonical state
        // can authorize the team-key cleanup below.
        return None;
    }
    match reason {
        Some("team_key_not_found") => Some("team_key_not_found"),
        Some("team_not_alive") => Some("team_not_alive"),
        _ => None,
    }
}

fn prune_entry_if_current(
    path: &Path,
    expected: &LeaderRegistryEntry,
    snapshot: &[u8],
) -> PruneAttempt {
    let result = crate::state::persist::with_runtime_state_lock_without_migrations(
        &expected.workspace,
        |state| {
            let Some(state) = state.as_ref() else {
                return PruneAttempt::Skipped("canonical_state_unavailable");
            };
            // Lock ordering is canonical state -> host registry. The state
            // remains unchanged while the exact registry bytes are checked.
            with_registry_lock(|_| {
                let Ok(bytes) = std::fs::read(path) else {
                    return PruneAttempt::Skipped("registry_entry_missing");
                };
                if bytes != snapshot {
                    return PruneAttempt::Skipped("registry_entry_changed");
                }
                let Ok(current) = serde_json::from_slice::<LeaderRegistryEntry>(&bytes) else {
                    return PruneAttempt::Skipped("registry_entry_changed");
                };
                if current != *expected {
                    return PruneAttempt::Skipped("registry_entry_changed");
                }
                if prune_reason_from_state(&current, state).is_none() {
                    return PruneAttempt::Skipped("canonical_state_changed");
                }
                if std::fs::remove_file(path).is_ok() {
                    PruneAttempt::Removed
                } else {
                    PruneAttempt::Error
                }
            })
            .unwrap_or(PruneAttempt::Skipped("registry_lock_unavailable"))
        },
    );
    result.unwrap_or(PruneAttempt::Skipped("canonical_state_unavailable"))
}

fn replace_entry_bytes_locked(path: &Path, bytes: &[u8]) -> bool {
    let tmp = path.with_extension(format!("rollback-{}-{}", std::process::id(), rand_suffix()));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

struct RegistryLock {
    file: std::fs::File,
}

impl RegistryLock {
    fn acquire(dir: &Path) -> Option<Self> {
        let path = dir.join(".registry.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .ok()?;
        let started = Instant::now();
        loop {
            match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
                Ok(true) => return Some(Self { file }),
                Ok(false) if started.elapsed() < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(false) | Err(_) => return None,
            }
        }
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = crate::platform::file_lock::unlock(&self.file);
    }
}

fn with_registry_lock<T>(f: impl FnOnce(&Path) -> T) -> Option<T> {
    let dir = registry_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let _lock = RegistryLock::acquire(&dir)?;
    Some(f(&dir))
}

fn with_registry_lock_result<T>(
    f: impl FnOnce(&Path) -> Result<T, RegistryRollbackError>,
) -> Result<T, RegistryRollbackError> {
    let dir = registry_dir().ok_or(RegistryRollbackError::LockUnavailable)?;
    std::fs::create_dir_all(&dir).map_err(|_| RegistryRollbackError::LockUnavailable)?;
    let _lock = RegistryLock::acquire(&dir).ok_or(RegistryRollbackError::LockUnavailable)?;
    f(&dir)
}

fn rand_suffix() -> String {
    // Non-cryptographic uniqueness: a process-local monotonic counter is
    // enough because the tmp name only needs to be unique within one
    // process lifetime (we also embed the pid).
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:x}")
}

/// Remove the registry entry for `(workspace, team_key)` if it exists.
/// Called from shutdown/unbind success hooks after canonical state
/// reflects the leader as no longer bound. Missing files are treated as
/// success — the invariant is "no registry entry after unbind", not
/// "must have found something to delete".
pub fn unregister_entry(workspace: &Path, team_key: &str) -> Option<PathBuf> {
    with_registry_lock(|dir| {
        let hash = workspace_hash(workspace);
        let path = dir.join(format!("{hash}__{team_key}.json"));
        if path.exists() {
            std::fs::remove_file(&path).ok()?;
            return Some(path);
        }
        None
    })
    .flatten()
}

/// Read and deserialize every `*.json` file under the registry directory.
/// Skips unreadable / malformed files silently (returns fewer entries) —
/// registry is a derived discovery index and unreadable files are the
/// "STALE / UNREADABLE" dirty class rather than an error.
#[must_use]
fn read_all_entries_with_bytes() -> Vec<(PathBuf, LeaderRegistryEntry, Vec<u8>)> {
    let Some(dir) = registry_dir() else {
        return Vec::new();
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(entry) = serde_json::from_slice::<LeaderRegistryEntry>(&bytes) else {
            continue;
        };
        out.push((path, entry, bytes));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[must_use]
fn read_all_entries() -> Vec<(PathBuf, LeaderRegistryEntry)> {
    read_all_entries_with_bytes()
        .into_iter()
        .map(|(path, entry, _bytes)| (path, entry))
        .collect()
}

/// Validate a registry entry against its canonical workspace state.
/// Returns the wire status label — LIVE when canonical `leader_receiver`
/// matches, STALE otherwise. This is the pruning gate for GC and the
/// per-entry classification the `leaders` CLI reports.
#[must_use]
pub fn classify(entry: &LeaderRegistryEntry) -> (&'static str, Option<String>) {
    let Ok(state) = crate::state::persist::load_runtime_state(&entry.workspace) else {
        return ("STALE", Some("workspace_no_state".to_string()));
    };
    classify_loaded(entry, &state)
}

fn classify_loaded(entry: &LeaderRegistryEntry, state: &Value) -> (&'static str, Option<String>) {
    // A missing or malformed `teams` root is an unavailable/legacy state,
    // not proof that this registry entry's team was retired. Keep it out of
    // the prune candidate set unless a valid map was independently found.
    let Some(teams) = state.get("teams").and_then(Value::as_object) else {
        return ("STALE", Some("teams_invalid".to_string()));
    };
    let team = match teams.get(&entry.team_key) {
        Some(t) => t,
        None => return ("STALE", Some("team_key_not_found".to_string())),
    };
    // Team status must indicate liveness. Empty / missing counts as
    // alive for pre-status states; explicit down/stopped/archived is
    // terminal STALE.
    let team_status = team
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("alive");
    if team_status == "down" || team_status == "stopped" || team_status == "archived" {
        return ("STALE", Some("team_not_alive".to_string()));
    }
    let receiver = team.get("leader_receiver");
    let receiver = match receiver {
        Some(r) if !r.is_null() => r,
        _ => return ("STALE", Some("leader_not_attached".to_string())),
    };
    let canonical_epoch = receiver
        .get("owner_epoch")
        .and_then(|v| v.as_u64())
        .or_else(|| team.get("owner_epoch").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    if canonical_epoch != entry.owner_epoch {
        return (
            "STALE",
            Some(format!(
                "owner_epoch_mismatch:registry={},canonical={}",
                entry.owner_epoch, canonical_epoch
            )),
        );
    }
    // Verify the recorded leader pane is actually live on its recorded
    // tmux endpoint. A killed tmux session (test's kill_session) means
    // the receiver is stale even though the state hasn't been updated.
    let pane_id = receiver.get("pane_id").and_then(|v| v.as_str());
    let socket = receiver.get("tmux_socket").and_then(|v| v.as_str());
    if let (Some(pane), Some(sock)) = (pane_id, socket) {
        if !tmux_pane_live(sock, pane) {
            return ("STALE", Some("leader_pane_dead".to_string()));
        }
    }
    ("LIVE", None)
}

fn tmux_pane_live(socket: &str, pane_id: &str) -> bool {
    // Best-effort tmux liveness check via the same transport factory the
    // main runtime uses. On error we treat the pane as NOT live so a
    // send never routes through a socket we could not verify.
    let backend = crate::transport_factory::tmux_endpoint_transport(socket);
    match backend.list_targets() {
        Ok(targets) => targets.iter().any(|t| t.pane_id.as_str() == pane_id),
        Err(_) => false,
    }
}

/// Explicitly prune only entries whose readable canonical state proves that
/// the team is gone or terminal. Dead panes, old epochs, unattached leaders,
/// missing/unreadable state, and probe failures remain untouched.
#[must_use]
pub fn prune_registry(dry_run: bool) -> RegistryPruneReport {
    let mut report = RegistryPruneReport {
        dry_run,
        candidates: Vec::new(),
        removed: Vec::new(),
        kept: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };
    let mut registry_lock_unavailable = false;
    for (path, entry, snapshot) in read_all_entries_with_bytes() {
        let (status, classified_reason) = classify(&entry);
        let reason = prune_reason(&entry, status, classified_reason.as_deref());
        let kept_reason = classified_reason.as_deref().unwrap_or("LIVE");
        let Some(reason) = reason else {
            report.kept.push(prune_item(&entry, kept_reason));
            continue;
        };
        let candidate = prune_item(&entry, reason);
        report.candidates.push(candidate.clone());
        if dry_run {
            continue;
        }
        if registry_lock_unavailable {
            report
                .skipped
                .push(prune_item(&entry, "registry_lock_unavailable"));
            continue;
        }
        match prune_entry_if_current(&path, &entry, &snapshot) {
            PruneAttempt::Removed => report.removed.push(candidate),
            PruneAttempt::Skipped(reason) => {
                if reason == "registry_lock_unavailable" {
                    registry_lock_unavailable = true;
                }
                report.skipped.push(prune_item(&entry, reason));
            }
            PruneAttempt::Error => report.errors.push(prune_item(&entry, "remove_failed")),
        }
    }
    report
}

/// Read all entries and classify each without mutating the derived index.
/// `send --to-leader` needs to see stale entries so it can refuse with
/// `registry_stale` (and never silently fall through to
/// `leader_name_not_found` just because a listing ran first).
#[must_use]
pub fn list_validated_no_gc() -> Vec<(LeaderRegistryEntry, &'static str, Option<String>)> {
    read_all_entries()
        .into_iter()
        .map(|(_path, entry)| {
            let (status, reason) = classify(&entry);
            (entry, status, reason)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use serde_json::json;
    use serial_test::serial;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    struct HomeGuard(Option<std::ffi::OsString>);

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(home) = self.0.take() {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn test_home(label: &str) -> (HomeGuard, PathBuf, PathBuf) {
        let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("ta-registry-{label}-{}-{n}", std::process::id()));
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        (HomeGuard(previous), home, workspace)
    }

    fn write_state(workspace: &Path, state: Value) {
        let path = crate::state::persist::runtime_state_path(workspace);
        std::fs::create_dir_all(path.parent().expect("runtime parent")).expect("create runtime");
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&state).expect("serialize state"),
        )
        .expect("write state");
    }

    fn entry(workspace: &Path, team_key: &str) -> LeaderRegistryEntry {
        build_entry(
            workspace,
            team_key,
            "direct_tmux",
            json!({"owner_epoch": 1}),
            1,
            "test",
            "2026-09-16T00:00:00Z".to_string(),
        )
    }

    #[test]
    #[serial]
    fn prune_removes_team_proven_terminal() {
        let (_home_guard, _home, workspace) = test_home("terminal");
        write_state(
            &workspace,
            json!({"teams": {"retired": {"status": "stopped"}}}),
        );
        let registered = entry(&workspace, "retired");
        let path = write_entry_best_effort(&registered).expect("register terminal team");

        let report = prune_registry(false);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.removed.len(), 1);
        assert!(report.skipped.is_empty());
        assert!(!path.exists(), "terminal registry entry must be removed");
    }

    #[test]
    #[serial]
    fn prune_removes_missing_key_only_from_valid_teams_map() {
        let (_home_guard, _home, workspace) = test_home("missing-key");
        write_state(&workspace, json!({"teams": {"other": {"status": "alive"}}}));
        let registered = entry(&workspace, "gone");
        let path = write_entry_best_effort(&registered).expect("register missing team");

        let report = prune_registry(false);
        assert_eq!(report.removed.len(), 1);
        assert!(
            !path.exists(),
            "missing key in valid teams map is removable"
        );
    }

    #[test]
    #[serial]
    fn prune_keeps_null_nonobject_and_legacy_teams_shapes() {
        for (label, state) in [
            ("null", json!({"teams": null})),
            ("array", json!({"teams": []})),
            ("string", json!({"teams": "legacy"})),
            (
                "single",
                json!({"team_key": "retired", "status": "stopped"}),
            ),
        ] {
            let (_home_guard, _home, workspace) = test_home(label);
            write_state(&workspace, state);
            let registered = entry(&workspace, "retired");
            let path = write_entry_best_effort(&registered).expect("register malformed state");

            let report = prune_registry(false);
            assert!(
                report.candidates.is_empty(),
                "malformed shape became candidate: {label}"
            );
            assert_eq!(
                report.kept.len(),
                1,
                "malformed shape must be retained: {label}"
            );
            assert_eq!(report.kept[0].reason, "teams_invalid");
            assert!(path.exists(), "malformed state entry must remain: {label}");
        }
    }
}
