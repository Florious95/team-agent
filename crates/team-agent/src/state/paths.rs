//! Team-selection ambiguity gates for CLI commands.

use std::path::Path;

use crate::state::persist::{load_runtime_state, load_runtime_state_without_migrations};
use crate::state::projection::team_state_candidates;

///
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
/// For single-team workspaces (the 0.4.x baseline) `Resolved` is
/// returned and the gate is a no-op. For two+ alive teams the gate
/// refuses with a `MissingTeamScope` error listing the candidates.
#[derive(Debug, Clone)]
pub enum CommandScope {
    /// Caller passed `--team X` explicitly (or there's only one alive team
    /// and we resolved it).
    Resolved,
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
    ///
    /// Resolve the CLI's `--team` argument against the workspace state.
    /// On any I/O error the empty case is returned — destructive commands
    /// will run their own selector and surface the real error.
    pub fn resolve(workspace: &Path, requested_team: Option<&str>) -> Self {
        Self::resolve_from_state(requested_team, load_runtime_state(workspace))
    }

    /// Ambiguity gate for read-only commands. Must not migrate or persist state.
    pub fn resolve_readonly(workspace: &Path, requested_team: Option<&str>) -> Self {
        Self::resolve_from_state(
            requested_team,
            load_runtime_state_without_migrations(workspace),
        )
    }

    fn resolve_from_state(
        requested_team: Option<&str>,
        loaded: Result<serde_json::Value, crate::state::StateError>,
    ) -> Self {
        if requested_team.filter(|t| !t.is_empty()).is_some() {
            return Self::Resolved;
        }
        let Ok(state) = loaded else {
            return Self::EmptyWorkspace;
        };
        let alive = team_state_candidates(&state);
        match alive.len() {
            0 => Self::EmptyWorkspace,
            1 => Self::Resolved,
            _ => {
                let mut keys: Vec<String> = alive.keys().cloned().collect();
                keys.sort();
                Self::Ambiguous(keys)
            }
        }
    }

    ///
    /// True iff there are 2+ alive teams and no explicit `--team`.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }

    ///
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
    use std::path::PathBuf;

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
        assert!(matches!(scope, CommandScope::Resolved));
        assert!(!scope.is_ambiguous());
    }

    #[test]
    fn command_scope_empty_workspace_falls_through() {
        let ws = tmp_workspace("empty");
        // No state file written.
        let scope = CommandScope::resolve(&ws, None);
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
        assert!(matches!(scope, CommandScope::Resolved));
        assert_eq!(
            team_state_candidates(&load_runtime_state(&ws).unwrap())
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["alpha"]
        );
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
        assert!(matches!(scope, CommandScope::Resolved));
        assert_eq!(
            team_state_candidates(&load_runtime_state(&ws).unwrap())
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["alpha"]
        );
        assert!(!scope.is_ambiguous());
    }
}
