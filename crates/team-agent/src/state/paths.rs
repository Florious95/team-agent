//! Stage 0 of the identity-boundary unified plan (architect direction
//! 2026-06-23): `TeamScope` + `TeamRuntimePaths` foundation.
//!
//! This module is **pure additive** — it does NOT change existing on-disk
//! paths or behaviour. It introduces a shared vocabulary that Stage 2 (owner
//! repository), Stage 5 (per-team runtime state), and Stage 6 (per-team
//! coordinator) will all consume, so that no later stage has to hand-build
//! `.team/runtime/<team_key>/...` paths or guess the canonical team_key from
//! a display name.
//!
//! Canonical rules:
//! - `team_key` is the IDENTITY of a team within a workspace. Display name
//!   (`team_dir.file_name()`) is NOT identity — two `agentX` dirs under
//!   different parents must end up with different team_keys.
//! - For the foundation slice, `TeamScope::new(team_key)` accepts whatever
//!   the caller already resolved (state/selector + state/persist already do
//!   this work). Slug/hash promotion to global uniqueness lands in Stage 5.
//! - `TeamRuntimePaths` is the SINGLE place where the
//!   `.team/runtime/<team_key>/` layout is constructed. Anywhere downstream
//!   code today hand-joins `runtime_dir(ws).join(team_key).join(...)` it
//!   should migrate to call a method on `TeamRuntimePaths` in a later stage.
//!
//! Single-team behaviour: unchanged. The foundation does not move data, does
//! not introduce new files, and is invoked by zero existing call sites yet.

use std::path::{Path, PathBuf};

use crate::model::paths::{runtime_dir, runtime_spec_path};
use crate::state::persist::load_runtime_state;
use crate::state::projection::team_state_candidates;

/// The identity of a team within a workspace. Carries the *workspace root*
/// and the *canonical team_key* (the directory name that state/selector
/// resolves; see `runtime_spec_path(workspace, team_key)`).
///
/// `TeamScope` is the input to `TeamRuntimePaths`. It deliberately does not
/// store a display name — display name is a UX label, not an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamScope {
    workspace: PathBuf,
    team_key: String,
}

impl TeamScope {
    /// Construct a TeamScope from an already-resolved workspace + team_key.
    /// Callers that have a display name must resolve it through the existing
    /// `state::selector` machinery first; this constructor refuses an empty
    /// team_key because that would silently fall through to the workspace
    /// root (the exact ambiguity Stage 5 is trying to eliminate).
    pub fn new(workspace: impl Into<PathBuf>, team_key: impl Into<String>) -> Option<Self> {
        let team_key = team_key.into();
        if team_key.is_empty() {
            return None;
        }
        Some(Self {
            workspace: workspace.into(),
            team_key,
        })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn team_key(&self) -> &str {
        &self.team_key
    }

    /// Convenience: derive the path helper from this scope.
    pub fn paths(&self) -> TeamRuntimePaths {
        TeamRuntimePaths::for_scope(self)
    }
}

/// Single source of `.team/runtime/<team_key>/...` path construction.
///
/// Stage 2 (owner repository), Stage 5 (per-team state), Stage 6 (per-team
/// coordinator), and Stage 7 (per-team tmux socket) will all migrate from
/// hand-built paths to `TeamRuntimePaths`. Today nothing reads from these
/// methods yet — the foundation just owns the layout decision so when later
/// stages start writing `.team/runtime/<team_key>/state.json` (Stage 5) or
/// `.team/runtime/<team_key>/coordinator.pid` (Stage 6), there is exactly
/// one place to change the layout.
#[derive(Debug, Clone)]
pub struct TeamRuntimePaths {
    workspace: PathBuf,
    team_key: String,
}

impl TeamRuntimePaths {
    pub fn new(workspace: impl Into<PathBuf>, team_key: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            team_key: team_key.into(),
        }
    }

    pub fn for_scope(scope: &TeamScope) -> Self {
        Self::new(
            scope.workspace().to_path_buf(),
            scope.team_key().to_string(),
        )
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn team_key(&self) -> &str {
        &self.team_key
    }

    /// `.team/runtime/<team_key>/` — the team's runtime directory. Today this
    /// already exists (runtime_spec lives under it). Stage 5 will start
    /// writing `state.json` here too.
    pub fn team_dir(&self) -> PathBuf {
        runtime_dir(&self.workspace).join(&self.team_key)
    }

    /// `.team/runtime/<team_key>/team.spec.yaml` — runtime spec. Mirrors the
    /// existing `runtime_spec_path` helper; provided here so downstream
    /// callers don't need to import two layout APIs.
    pub fn spec_path(&self) -> PathBuf {
        runtime_spec_path(&self.workspace, &self.team_key)
    }

    /// `.team/runtime/<team_key>/state.json` — the canonical per-team state
    /// path that Stage 5 will start writing. Not used by Stage 0 product
    /// code; callers must continue using `runtime_state_path` until Stage 5
    /// migrates the truth source.
    pub fn state_path(&self) -> PathBuf {
        self.team_dir().join("state.json")
    }

    /// `.team/runtime/<team_key>/coordinator.pid` — Stage 6 sidecar location.
    pub fn coordinator_pid_path(&self) -> PathBuf {
        self.team_dir().join("coordinator.pid")
    }

    /// `.team/runtime/<team_key>/coordinator.log` — Stage 6 sidecar location.
    pub fn coordinator_log_path(&self) -> PathBuf {
        self.team_dir().join("coordinator.log")
    }
}

/// Stage 4 of identity-boundary unified plan (architect direction
/// 2026-06-24, .team/artifacts/identity-boundary-unified-plan.md §2 Stage
/// 4): the CLI ambiguity gate for destructive commands. Pre-Stage-4 a
/// bare `shutdown` / `restart` / `reset-agent` / etc. on a workspace with
/// two alive teams silently picked the first one — exactly the
/// "active_team_key as destructive command authority" anti-pattern §4 不可改项
/// rejects. `CommandScope` makes this state explicit.
///
/// Usage from CLI dispatch:
///     let scope = CommandScope::resolve(workspace, args.team.as_deref());
///     scope.require_unambiguous_for_destructive(&workspace)?;
///
/// For single-team workspaces (the 0.4.x baseline) `Unambiguous` is
/// returned and the gate is a no-op. For two+ alive teams the gate
/// refuses with a `MissingTeamScope` error listing the candidates.
#[derive(Debug, Clone)]
pub enum CommandScope {
    /// Caller passed `--team X` explicitly (or there's only one alive team
    /// and we resolved it). Carries the canonical team_key.
    Resolved(String),
    /// No `--team` and multiple alive teams. Destructive commands AND
    /// selected-team commands (status, etc.) MUST refuse with this list
    /// of candidates so the operator chooses explicitly. Only true
    /// all-team aggregation commands may proceed without --team.
    /// (S4QR-001 0.4.8: status was previously documented as a read-only
    /// command that could proceed; in fact it projects a single team's
    /// view and silently defaulting to the active team caused gate
    /// failures when multiple alive teams existed.)
    Ambiguous(Vec<String>),
    /// No `--team` and no teams alive yet (fresh workspace) — bare
    /// commands fall through to legacy single-team behaviour.
    EmptyWorkspace,
}

impl CommandScope {
    /// Resolve the CLI's `--team` argument against the workspace state.
    /// On any I/O error the empty case is returned — destructive commands
    /// will run their own selector and surface the real error.
    pub fn resolve(workspace: &Path, requested_team: Option<&str>) -> Self {
        if let Some(team) = requested_team.filter(|t| !t.is_empty()) {
            return Self::Resolved(team.to_string());
        }
        let Ok(state) = load_runtime_state(workspace) else {
            return Self::EmptyWorkspace;
        };
        let alive = team_state_candidates(&state);
        match alive.len() {
            0 => Self::EmptyWorkspace,
            1 => Self::Resolved(alive.keys().next().cloned().unwrap_or_default()),
            _ => {
                let mut keys: Vec<String> = alive.keys().cloned().collect();
                keys.sort();
                Self::Ambiguous(keys)
            }
        }
    }

    /// Convert to an `Option<String>` for backward compatibility with
    /// existing `args.team.as_deref()` call sites.
    pub fn team_key(&self) -> Option<&str> {
        match self {
            Self::Resolved(key) => Some(key.as_str()),
            _ => None,
        }
    }

    /// True iff there are 2+ alive teams and no explicit `--team`.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }

    /// The candidate list when ambiguous; empty otherwise.
    pub fn candidates(&self) -> &[String] {
        match self {
            Self::Ambiguous(keys) => keys,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_scope_refuses_empty_team_key() {
        assert!(TeamScope::new(PathBuf::from("/ws"), "").is_none());
    }

    #[test]
    fn team_scope_accepts_workspace_and_team_key() {
        let scope = TeamScope::new(PathBuf::from("/ws"), "alpha").unwrap();
        assert_eq!(scope.workspace(), Path::new("/ws"));
        assert_eq!(scope.team_key(), "alpha");
    }

    #[test]
    fn team_runtime_paths_layout_matches_existing_runtime_dir_layout() {
        let paths = TeamRuntimePaths::new(PathBuf::from("/ws/proj"), "alpha");
        assert_eq!(
            paths.team_dir(),
            PathBuf::from("/ws/proj/.team/runtime/alpha")
        );
        assert_eq!(
            paths.spec_path(),
            PathBuf::from("/ws/proj/.team/runtime/alpha/team.spec.yaml")
        );
        assert_eq!(
            paths.state_path(),
            PathBuf::from("/ws/proj/.team/runtime/alpha/state.json")
        );
        assert_eq!(
            paths.coordinator_pid_path(),
            PathBuf::from("/ws/proj/.team/runtime/alpha/coordinator.pid")
        );
    }

    #[test]
    fn two_different_team_keys_in_same_workspace_get_different_paths() {
        let alpha = TeamRuntimePaths::new(PathBuf::from("/ws"), "alpha");
        let beta = TeamRuntimePaths::new(PathBuf::from("/ws"), "beta");
        assert_ne!(alpha.team_dir(), beta.team_dir());
        assert_ne!(alpha.state_path(), beta.state_path());
        assert_ne!(alpha.coordinator_pid_path(), beta.coordinator_pid_path());
    }

    #[test]
    fn team_scope_to_paths_round_trip() {
        let scope = TeamScope::new(PathBuf::from("/ws"), "alpha").unwrap();
        let paths = scope.paths();
        assert_eq!(paths.workspace(), scope.workspace());
        assert_eq!(paths.team_key(), scope.team_key());
    }

    // ─────────────── Stage 4: CommandScope tests ───────────────

    fn tmp_workspace(label: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let ws = std::env::temp_dir().join(format!(
            "ta_cmdscope_{}_{}_{}",
            label,
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    #[test]
    fn command_scope_explicit_team_always_wins() {
        let ws = tmp_workspace("explicit");
        let scope = CommandScope::resolve(&ws, Some("alpha"));
        assert_eq!(scope.team_key(), Some("alpha"));
        assert!(!scope.is_ambiguous());
    }

    #[test]
    fn command_scope_empty_workspace_falls_through() {
        let ws = tmp_workspace("empty");
        // No state file written.
        let scope = CommandScope::resolve(&ws, None);
        assert!(scope.team_key().is_none());
        assert!(!scope.is_ambiguous());
        assert!(matches!(scope, CommandScope::EmptyWorkspace));
    }

    #[test]
    fn command_scope_single_alive_team_resolves_automatically() {
        let ws = tmp_workspace("single");
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "teams": {"alpha": {"status": "alive"}},
            }),
        )
        .unwrap();
        let scope = CommandScope::resolve(&ws, None);
        assert_eq!(scope.team_key(), Some("alpha"));
        assert!(!scope.is_ambiguous());
    }

    #[test]
    fn command_scope_two_alive_teams_no_explicit_team_is_ambiguous() {
        let ws = tmp_workspace("multi");
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "teams": {
                    "alpha": {"status": "alive"},
                    "beta": {"status": "alive"},
                },
            }),
        )
        .unwrap();
        let scope = CommandScope::resolve(&ws, None);
        assert!(scope.is_ambiguous());
        assert_eq!(
            scope.candidates(),
            &["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn command_scope_archived_team_does_not_count_as_alive() {
        let ws = tmp_workspace("archived");
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "teams": {
                    "alpha": {"status": "alive"},
                    "beta": {"archived_at": "2026-06-01T00:00:00Z"},
                },
            }),
        )
        .unwrap();
        let scope = CommandScope::resolve(&ws, None);
        // Only alpha counts → not ambiguous, resolves to alpha.
        assert_eq!(scope.team_key(), Some("alpha"));
        assert!(!scope.is_ambiguous());
    }
}
