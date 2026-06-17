//! 0.3.28 layout step 7 — leader pane recovery.
//!
//! Python truth source (`leader/__init__.py:885-887` +
//! `restart/orchestration.py:369-383`): leader recovery requires
//! `TMUX_PANE` env. Without it, runtime emits `leader_receiver.rebind_required`
//! and continues — NO proactive co-located leader spawn.
//!
//! Replaces the pre-0.3.28 `ensure_managed_leader_pane` path that masked
//! missing leader windows by silently spawning a new one (often into the
//! worker session — E57-3 root). After Step 2 the leader lives in its
//! own session, so the proper recovery path is:
//!   1. If `TMUX_PANE` is set → autobind to that pane.
//!   2. Else → emit `leader_receiver.rebind_required` and return
//!      `NeedsUserAttach`; the user runs `team-agent attach-leader` or
//!      `team-agent claim-leader` from their leader pane.

use crate::transport::PaneId;

/// Outcome of a leader recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// `TMUX_PANE` env is set; caller should `autobind_leader` against this
    /// pane id.
    AutobindToPane(PaneId),
    /// No `TMUX_PANE` env; runtime emits `leader_receiver.rebind_required`
    /// and continues without a leader pane. The user must run
    /// `team-agent attach-leader` from their leader pane to recover.
    NeedsUserAttach,
}

/// Compute the recovery outcome from the current environment.
///
/// Reads `TMUX_PANE` from the supplied environment iterator (use
/// `std::env::vars()` in production; tests inject a synthetic map).
pub fn recover_leader_pane<I>(env: I) -> RecoveryOutcome
where
    I: IntoIterator<Item = (String, String)>,
{
    for (k, v) in env {
        if k == "TMUX_PANE" && !v.is_empty() {
            return RecoveryOutcome::AutobindToPane(PaneId::new(v));
        }
    }
    RecoveryOutcome::NeedsUserAttach
}

/// Emit the canonical `leader_receiver.rebind_required` event when the
/// runtime falls through to `NeedsUserAttach`. Logs to stderr so operators
/// see the cascade in event logs.
pub fn log_rebind_required(reason: &str) {
    eprintln!(
        "team_agent::layout leader_receiver.rebind_required reason={reason} \
         action=`run team-agent attach-leader from your leader pane`"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_pane_present_returns_autobind() {
        let env = vec![
            ("TMUX_PANE".to_string(), "%7".to_string()),
            ("PATH".to_string(), "/bin".to_string()),
        ];
        let outcome = recover_leader_pane(env);
        match outcome {
            RecoveryOutcome::AutobindToPane(pane) => assert_eq!(pane.as_str(), "%7"),
            other => panic!("expected AutobindToPane(%7), got {other:?}"),
        }
    }

    #[test]
    fn no_tmux_pane_returns_needs_user_attach() {
        let env = vec![("PATH".to_string(), "/bin".to_string())];
        assert_eq!(recover_leader_pane(env), RecoveryOutcome::NeedsUserAttach);
    }

    #[test]
    fn empty_tmux_pane_returns_needs_user_attach() {
        let env = vec![("TMUX_PANE".to_string(), "".to_string())];
        assert_eq!(recover_leader_pane(env), RecoveryOutcome::NeedsUserAttach);
    }

    #[test]
    fn recovery_never_spawns_co_located_leader_window() {
        // Python invariant #12: recovery requires TMUX_PANE; without it
        // runtime emits rebind_required and CONTINUES. No silent co-located
        // spawn into the worker session (the E57-3 root cause).
        let env = vec![("WORKER_SESSION_LIVE".to_string(), "1".to_string())];
        match recover_leader_pane(env) {
            RecoveryOutcome::NeedsUserAttach => {} // expected
            RecoveryOutcome::AutobindToPane(_) => {
                panic!("recovery must not autobind without TMUX_PANE — \
                        the worker session must NEVER carry a co-located leader \
                        window in managed mode (E57-3 root)")
            }
        }
    }
}
