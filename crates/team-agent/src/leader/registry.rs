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

use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryWriteOutcome {
    pub status: &'static str,
    pub path: Option<PathBuf>,
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
pub fn workspace_short(workspace: &Path) -> String {
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
    let write_result = write_entry_best_effort(&entry);
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
/// Returns the final path when the write succeeded, `None` otherwise —
/// the binding command must never fail on registry errors.
pub fn write_entry_best_effort(entry: &LeaderRegistryEntry) -> Option<PathBuf> {
    let dir = registry_dir()?;
    if let Err(_error) = std::fs::create_dir_all(&dir) {
        return None;
    }
    let final_path = dir.join(entry_filename(entry));
    let tmp_name = format!(
        ".{}.tmp-{}-{}",
        entry_filename(entry),
        std::process::id(),
        rand_suffix()
    );
    let tmp_path = dir.join(tmp_name);
    let serialized = serde_json::to_string_pretty(entry).ok()?;
    if std::fs::write(&tmp_path, serialized).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    if std::fs::rename(&tmp_path, &final_path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }
    Some(final_path)
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
    let dir = registry_dir()?;
    let hash = workspace_hash(workspace);
    let path = dir.join(format!("{hash}__{team_key}.json"));
    if path.exists() {
        std::fs::remove_file(&path).ok()?;
        return Some(path);
    }
    None
}

/// Read and deserialize every `*.json` file under the registry directory.
/// Skips unreadable / malformed files silently (returns fewer entries) —
/// registry is a derived discovery index and unreadable files are the
/// "STALE / UNREADABLE" dirty class rather than an error.
#[must_use]
pub fn read_all_entries() -> Vec<(PathBuf, LeaderRegistryEntry)> {
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
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(entry) = serde_json::from_str::<LeaderRegistryEntry>(&text) else {
            continue;
        };
        out.push((path, entry));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
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
    let team = state
        .get("teams")
        .and_then(|v| v.as_object())
        .and_then(|teams| teams.get(&entry.team_key));
    let team = match team {
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

/// Return the canonical state's `leader_receiver` value for the entry's
/// team, if one exists. Send/leaders paths use this so they never route
/// through a stale registry channel — canonical state is the truth.
#[must_use]
pub fn canonical_receiver(entry: &LeaderRegistryEntry) -> Option<serde_json::Value> {
    let state = crate::state::persist::load_runtime_state(&entry.workspace).ok()?;
    let team = state
        .get("teams")
        .and_then(|v| v.as_object())
        .and_then(|teams| teams.get(&entry.team_key))?;
    team.get("leader_receiver").cloned()
}

/// Read all entries, classify each, and prune terminal-stale entries
/// (`workspace_no_state`, `team_key_not_found`). Live entries are never
/// touched. Returns the surviving (LIVE + STALE-not-yet-pruned) entries
/// classified for the `leaders` command output.
///
/// Design §14 step 6: "Successful scoped shutdown removes matching registry
/// entry. Failed/degraded shutdown leaves entry STALE, not deleted."
/// The pruning here is the GC arm — it only removes entries whose
/// canonical workspace/team has no state at all, i.e. the target has been
/// permanently removed. Leader-detached-but-team-alive stays STALE and
/// visible so the operator can decide.
#[must_use]
pub fn list_validated_with_gc() -> Vec<(LeaderRegistryEntry, &'static str, Option<String>)> {
    let mut out = Vec::new();
    for (path, entry) in read_all_entries() {
        let (status, reason) = classify(&entry);
        // Terminal stale reasons that indicate the entry can never be
        // useful again: canonical workspace/team gone, or the leader
        // binding has been fully released (no receiver at all). These
        // are safe to prune; leader-attached-with-dead-pane and
        // epoch-mismatch stay visible so the operator can see them.
        if status == "STALE"
            && reason.as_deref().is_some_and(|r| {
                r == "workspace_no_state"
                    || r == "team_key_not_found"
                    || r == "leader_not_attached"
                    || r == "team_not_alive"
            })
        {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        out.push((entry, status, reason));
    }
    out
}

/// Same as `list_validated_with_gc` but preserves all entries. `send
/// --to-leader` needs to see stale entries so it can refuse with
/// `registry_stale` (and never silently fall through to
/// `leader_name_not_found` just because GC ran first).
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
