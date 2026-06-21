//! unit-3 (Stage 1) — restart preflight session-identity guard.
//!
//! Single check that runs BEFORE the worker-session teardown in
//! `rebuild::restart_inner` (and the parallel check in
//! `common::session_name_present` callers). The guard's purpose is to
//! REFUSE the kill when the value in `state.session_name` is actually a
//! leader launcher session name (the 0.3.39 leader-mis-kill bug shape).
//!
//! Typed identity comes from unit-1's `RuntimeSessions::from_state`. This
//! preflight is the first adoption site of that typed reader inside the
//! restart code path.

use crate::layout::{RuntimeSessionAnomaly, RuntimeSessions};

/// Outcome of the preflight session-identity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPreflight {
    /// State's session-name fields are well-formed. The caller may proceed
    /// with the worker-session teardown.
    Ok,
    /// `state.session_name` matches the leader launcher prefix. Refuse the
    /// kill — proceeding would tear down the leader pane (E49 / 0.3.39).
    WorkerSessionIsLeaderSession {
        session_name: String,
        reason: String,
    },
}

impl SessionPreflight {
    /// True when the preflight passed and the caller may proceed.
    pub fn is_ok(&self) -> bool {
        matches!(self, SessionPreflight::Ok)
    }
}

/// Inspect `state.json` and decide whether the upcoming worker-session
/// kill is safe. Pure: no I/O, no mutation.
pub fn check_session_preflight(state: &serde_json::Value) -> SessionPreflight {
    let sessions = RuntimeSessions::from_state(state);
    if let Some(anomaly) = sessions
        .anomalies
        .iter()
        .find(|a| matches!(a, RuntimeSessionAnomaly::WorkerSessionNameIsLeaderPrefixed { .. }))
    {
        if let RuntimeSessionAnomaly::WorkerSessionNameIsLeaderPrefixed { value } = anomaly {
            return SessionPreflight::WorkerSessionIsLeaderSession {
                session_name: value.clone(),
                reason: "worker_session_is_leader_session".to_string(),
            };
        }
    }
    SessionPreflight::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preflight_passes_clean_state() {
        let s = json!({ "session_name": "team-foo" });
        assert!(check_session_preflight(&s).is_ok());
    }

    #[test]
    fn preflight_passes_empty_state() {
        assert!(check_session_preflight(&json!({})).is_ok());
    }

    #[test]
    fn preflight_refuses_leader_prefixed_worker_session_name() {
        let s = json!({ "session_name": "team-agent-leader-claude-abc" });
        let r = check_session_preflight(&s);
        assert!(!r.is_ok());
        match r {
            SessionPreflight::WorkerSessionIsLeaderSession {
                session_name,
                reason,
            } => {
                assert_eq!(session_name, "team-agent-leader-claude-abc");
                assert_eq!(reason, "worker_session_is_leader_session");
            }
            _ => panic!("expected WorkerSessionIsLeaderSession refusal"),
        }
    }
}
