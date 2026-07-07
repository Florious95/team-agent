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

/// Absolute path to `~/.team-agent/leaders`. Returns `None` when the
/// caller has no HOME (e.g. some CI shells) — callers should treat this as
/// "no registry" rather than an error, since registry is derived.
#[must_use]
pub fn registry_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".team-agent").join("leaders"))
}

/// Registry write is best-effort. On any I/O error we emit
/// `leader_registry.write_failed` (documented above) and return `None`
/// so the binding command can attach the failure notice to its own JSON
/// output without cascading.
///
/// Actual filesystem I/O is intentionally deferred: E7 first slice ships
/// the module surface, event names, and schema types so the CLI/binding
/// hooks can be wired in a follow-up commit without touching the RED
/// grep guards. `_entry` is unused today.
pub fn write_entry_best_effort(_entry: &LeaderRegistryEntry) -> Option<PathBuf> {
    None
}

/// Same rationale as `write_entry_best_effort` — the actual read/canonical
/// validate loop is a follow-up. Returning an empty list keeps callers
/// safe (empty leaders list, no ambiguity).
#[must_use]
pub fn list_validated(_registry_root: &Path) -> Vec<LeaderRegistryEntry> {
    Vec::new()
}
