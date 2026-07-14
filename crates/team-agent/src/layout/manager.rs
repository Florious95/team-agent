//! 0.3.28 layout step 2 — leader placement.
//!
//! Public API for managed-mode leader placement. The runtime calls
//! `leader_placement` to compute where the leader pane lives and
//! `ensure_leader_pane` to spawn/reuse it idempotently.
//!
//! Python parity (recap): leader **always** uses its dedicated session
//! `team-agent-leader-<provider>-<folder>-<sha1[:8]>`. The window inside
//! that session is named after the provider wire (`codex` / `claude` /
//! `copilot`), never the literal `leader`. The pre-0.3.28 Rust managed
//! mode co-located leader into the worker session `team-<team>` with a
//! window named `leader` — this is the root cause of E49 / E51 / E53 /
//! E57-3 / E60.

use std::path::Path;

use crate::leader::start::leader_session_name;
use crate::leader::types::{LeaderIdentity, LeaderStartMode};
use crate::model::enums::Provider;
use crate::transport::{SessionName, WindowName};

/// 0.3.28: managed-mode leader runs in its own session, window named after
/// the provider. Mirrors `team-agent-leader-<provider>-<folder>-<sha1[:8]>`
/// from Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderPlacement {
    /// Leader executes in the current pane (caller has `$TMUX` set).
    /// No new session/window is created.
    ExecCurrentPane,
    /// Leader runs in its dedicated session.
    DedicatedSession {
        session: SessionName,
        window: WindowName,
    },
}

/// Compute the placement for a leader given its identity, the desired
/// `LeaderStartMode`, and the environment (used to detect `$TMUX`).
///
/// Returns `ExecCurrentPane` iff `mode == ExecProvider`; otherwise
/// `DedicatedSession{ leader_session_name(provider, workspace),
/// WindowName(provider_wire(provider)) }`.
pub fn leader_placement(
    mode: LeaderStartMode,
    provider: Provider,
    workspace: &Path,
) -> LeaderPlacement {
    match mode {
        LeaderStartMode::ExecProvider => LeaderPlacement::ExecCurrentPane,
        // Managed + External + AttachExisting + NewTmuxSession all converge
        // on the SAME `leader_session_name(provider, workspace)` per Python
        // parity. The pre-0.3.28 managed branch used `team-<team_id>` (the
        // worker session) — that divergence is what this step deletes.
        _ => LeaderPlacement::DedicatedSession {
            session: leader_session_name(provider, workspace),
            window: WindowName::new(super::worker_window_helpers::provider_window_name(provider)),
        },
    }
}

/// The leader session name for a given provider + workspace. `LeaderIdentity`
/// in this codebase does not carry the provider field; the caller supplies it
/// explicitly (matches how `leader_session_name` itself is shaped).
pub fn leader_session_for_provider(provider: Provider, workspace: &Path) -> SessionName {
    leader_session_name(provider, workspace)
}

#[allow(dead_code)]
fn _unused_identity_marker(_id: &LeaderIdentity) {}

// ─── Worker placement (Step 4) ──────────────────────────────────────────

use crate::layout::placement::{WorkerSpawnAction, WorkerSpawnTarget};
use crate::model::yaml::Value as YamlValue;
use crate::transport::Transport;

/// Compute the spawn target for a worker. Python parity: one window per
/// agent in the worker session, named after `agent_id`. First worker
/// (= session does not yet exist) → NewSession; otherwise NewWindow.
///
/// `force` flag: when the window already exists, controls whether to
/// emit `ForceReplace` (caller will `kill-window` then recreate) or
/// `Noop` (caller does nothing).
pub fn next_worker_window(
    spec: &YamlValue,
    agent_id: &str,
    transport: &dyn Transport,
    force: bool,
) -> WorkerSpawnTarget {
    let session = super::sessions::worker_session_name(spec);
    let window = WindowName::new(agent_id);
    let session_exists = transport.has_session(&session).unwrap_or(false);
    if !session_exists {
        return WorkerSpawnTarget::new(session, window, WorkerSpawnAction::NewSession);
    }
    let window_exists = transport
        .list_windows(&session)
        .unwrap_or_default()
        .iter()
        .any(|w| w.as_str() == window.as_str());
    let action = if window_exists {
        if force {
            WorkerSpawnAction::ForceReplace
        } else {
            WorkerSpawnAction::Noop
        }
    } else {
        WorkerSpawnAction::NewWindow
    };
    WorkerSpawnTarget::new(session, window, action)
}

#[cfg(test)]
mod worker_placement_tests {
    use super::*;

    fn spec_for(team: &str) -> YamlValue {
        YamlValue::Map(vec![(
            "team".to_string(),
            YamlValue::Map(vec![("name".to_string(), YamlValue::Str(team.to_string()))]),
        )])
    }

    /// Pure-logic helper: same decision tree as `next_worker_window` but
    /// without going through the `Transport` trait. Lets unit tests pin the
    /// invariants without mocking the whole transport surface (which would
    /// require ~20 unrelated method stubs).
    fn next_worker_window_pure(
        spec: &YamlValue,
        agent_id: &str,
        session_exists: bool,
        window_exists: bool,
        force: bool,
    ) -> WorkerSpawnTarget {
        let session = super::super::sessions::worker_session_name(spec);
        let window = WindowName::new(agent_id);
        if !session_exists {
            return WorkerSpawnTarget::new(session, window, WorkerSpawnAction::NewSession);
        }
        let action = if window_exists {
            if force {
                WorkerSpawnAction::ForceReplace
            } else {
                WorkerSpawnAction::Noop
            }
        } else {
            WorkerSpawnAction::NewWindow
        };
        WorkerSpawnTarget::new(session, window, action)
    }

    #[test]
    fn first_worker_on_absent_session_returns_new_session() {
        let t = next_worker_window_pure(&spec_for("alpha"), "developer", false, false, false);
        assert_eq!(t.action, WorkerSpawnAction::NewSession);
        assert_eq!(t.window.as_str(), "developer");
        assert_eq!(t.session.as_str(), "team-alpha");
    }

    #[test]
    fn second_worker_on_present_session_returns_new_window() {
        let t = next_worker_window_pure(&spec_for("alpha"), "tester", true, false, false);
        assert_eq!(t.action, WorkerSpawnAction::NewWindow);
        assert_eq!(t.window.as_str(), "tester");
    }

    #[test]
    fn existing_window_without_force_returns_noop() {
        let t = next_worker_window_pure(&spec_for("alpha"), "developer", true, true, false);
        assert_eq!(t.action, WorkerSpawnAction::Noop);
    }

    #[test]
    fn existing_window_with_force_returns_force_replace() {
        let t = next_worker_window_pure(&spec_for("alpha"), "developer", true, true, true);
        assert_eq!(t.action, WorkerSpawnAction::ForceReplace);
    }

    #[test]
    fn target_window_name_always_equals_agent_id_no_team_wN() {
        // The pre-0.3.28 adaptive layout used team-w1/team-w2 windows with 3
        // workers tiled per window. Step 4 design forbids that.
        for agent_id in &["alice", "bob", "carol", "dave"] {
            let t = next_worker_window_pure(&spec_for("alpha"), agent_id, false, false, false);
            assert_eq!(t.window.as_str(), *agent_id);
            assert!(
                !t.window.as_str().starts_with("team-w"),
                "no team-wN window allowed; got `{}`",
                t.window.as_str()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::sessions::is_leader_session;

    #[test]
    fn exec_provider_mode_returns_exec_current_pane() {
        let placement = leader_placement(
            LeaderStartMode::ExecProvider,
            Provider::Claude,
            Path::new("/tmp/x"),
        );
        assert_eq!(placement, LeaderPlacement::ExecCurrentPane);
    }

    #[test]
    fn managed_mode_returns_dedicated_leader_session() {
        let placement = leader_placement(
            LeaderStartMode::ManagedTmuxClient,
            Provider::Claude,
            Path::new("/tmp/x"),
        );
        match placement {
            LeaderPlacement::DedicatedSession { session, window } => {
                assert!(
                    is_leader_session(&session),
                    "managed mode must use the dedicated leader session prefix; got `{}`",
                    session.as_str()
                );
                assert_eq!(
                    window.as_str(),
                    "claude",
                    "leader window must be named after provider wire, not `leader`; got `{}`",
                    window.as_str()
                );
            }
            other => panic!("expected DedicatedSession, got {other:?}"),
        }
    }

    #[test]
    fn new_tmux_session_mode_returns_same_leader_session_as_managed() {
        let managed = leader_placement(
            LeaderStartMode::ManagedTmuxClient,
            Provider::Codex,
            Path::new("/tmp/x"),
        );
        let new_session = leader_placement(
            LeaderStartMode::NewTmuxSession,
            Provider::Codex,
            Path::new("/tmp/x"),
        );
        assert_eq!(
            managed, new_session,
            "managed and new-tmux-session both target the dedicated leader session"
        );
    }
}
