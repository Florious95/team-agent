//! unit-1 (Stage 1) — typed runtime session identity.
//!
//! Provides two compile-time-distinct newtypes plus a `RuntimeSessions`
//! reader that pulls them out of `state.json` in one shot.
//!
//! Why: every place that reads `state.session_name` or
//! `leader_receiver.session_name` today does an ad-hoc `starts_with("team-
//! agent-leader-")` check (or, in the false-positive cases, doesn't). The
//! 0.3.39 leader-mis-kill class of bugs traces back to a string crossing
//! a boundary as the wrong identity.
//!
//! By going through `WorkerSession::new` / `LeaderLauncherSession::new`, a
//! leader-prefixed name can never be presented as a worker session by
//! construction. `LeaderLauncherSession::new` enforces the inverse — only
//! a leader-prefixed name passes.
//!
//! This unit is PURELY ADDITIVE: no existing call site is rewired yet.
//! unit-3 (restart preflight guard) and unit-4 (managed leader persistence
//! fix) are the adoption sites; they will route their state reads through
//! `RuntimeSessions::from_state`.

use crate::layout::sessions::{is_leader_session, LEADER_SESSION_PREFIX};
use crate::transport::SessionName;
use serde_json::Value;

/// A tmux session name that is guaranteed NOT to be a leader launcher
/// session (does not start with `team-agent-leader-`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerSession(SessionName);

/// A tmux session name that IS a leader launcher session (starts with
/// `team-agent-leader-`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaderLauncherSession(SessionName);

/// Why `WorkerSession::new` rejected a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSessionError {
    /// The name is empty.
    Empty,
    /// The name carries the leader launcher prefix — must not be a worker
    /// session by construction.
    LeaderPrefixed { name: String },
}

/// Why `LeaderLauncherSession::new` rejected a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderLauncherSessionError {
    /// The name is empty.
    Empty,
    /// The name does not carry the required leader launcher prefix.
    MissingLeaderPrefix { name: String },
}

impl WorkerSession {
    pub fn new(name: impl Into<String>) -> Result<Self, WorkerSessionError> {
        let raw: String = name.into();
        if raw.is_empty() {
            return Err(WorkerSessionError::Empty);
        }
        if raw.starts_with(LEADER_SESSION_PREFIX) {
            return Err(WorkerSessionError::LeaderPrefixed { name: raw });
        }
        Ok(WorkerSession(SessionName::new(raw)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_session_name(self) -> SessionName {
        self.0
    }

    pub fn session_name(&self) -> &SessionName {
        &self.0
    }
}

impl LeaderLauncherSession {
    pub fn new(name: impl Into<String>) -> Result<Self, LeaderLauncherSessionError> {
        let raw: String = name.into();
        if raw.is_empty() {
            return Err(LeaderLauncherSessionError::Empty);
        }
        if !raw.starts_with(LEADER_SESSION_PREFIX) {
            return Err(LeaderLauncherSessionError::MissingLeaderPrefix { name: raw });
        }
        Ok(LeaderLauncherSession(SessionName::new(raw)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_session_name(self) -> SessionName {
        self.0
    }

    pub fn session_name(&self) -> &SessionName {
        &self.0
    }
}

/// What `RuntimeSessions::from_state` learned about the runtime topology.
///
/// Both fields are `Option` because state.json may be partial: a workspace
/// that has never seen a successful quick-start has no `session_name`, and
/// `leader_receiver` is absent until a leader binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSessions {
    /// The worker session derived from `state.session_name`, if present
    /// AND well-formed (not leader-prefixed).
    pub worker: Option<WorkerSession>,
    /// The leader launcher session derived from
    /// `state.leader_receiver.session_name`, if present AND leader-prefixed.
    pub leader: Option<LeaderLauncherSession>,
    /// State fields that exist but failed validation (e.g.
    /// `state.session_name` is leader-prefixed = the 0.3.39 bug shape).
    /// Callers (unit-3 preflight) use this to refuse cleanly rather than
    /// proceed with a wrong-identity name.
    pub anomalies: Vec<RuntimeSessionAnomaly>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSessionAnomaly {
    /// `state.session_name` carried the leader launcher prefix — classic
    /// 0.3.39 bug shape. cli/lifecycle MUST refuse before taking a kill
    /// action against this string.
    WorkerSessionNameIsLeaderPrefixed { value: String },
    /// `leader_receiver.session_name` was present but not leader-prefixed.
    LeaderReceiverSessionNameNotLeaderPrefixed { value: String },
    /// `state.session_name == leader_receiver.session_name` — collision
    /// that unit-4 explicitly fixes by stopping the persistence overwrite.
    WorkerSessionEqualsLeaderSession { value: String },
}

impl RuntimeSessions {
    /// Read state.json's session-identity fields and wrap them in typed
    /// values. Never panics on bad state — anomalies are surfaced for
    /// callers to act on.
    pub fn from_state(state: &Value) -> Self {
        let raw_worker = state
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let raw_leader = state
            .get("leader_receiver")
            .and_then(|r| r.get("session_name"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut anomalies = Vec::new();

        let worker = match raw_worker.as_deref() {
            None | Some("") => None,
            Some(name) => match WorkerSession::new(name) {
                Ok(w) => Some(w),
                Err(WorkerSessionError::LeaderPrefixed { name }) => {
                    anomalies.push(RuntimeSessionAnomaly::WorkerSessionNameIsLeaderPrefixed {
                        value: name,
                    });
                    None
                }
                Err(WorkerSessionError::Empty) => None,
            },
        };

        let leader = match raw_leader.as_deref() {
            None | Some("") => None,
            Some(name) => match LeaderLauncherSession::new(name) {
                Ok(l) => Some(l),
                Err(LeaderLauncherSessionError::MissingLeaderPrefix { name }) => {
                    anomalies.push(
                        RuntimeSessionAnomaly::LeaderReceiverSessionNameNotLeaderPrefixed {
                            value: name,
                        },
                    );
                    None
                }
                Err(LeaderLauncherSessionError::Empty) => None,
            },
        };

        if let (Some(rw), Some(rl)) = (raw_worker.as_deref(), raw_leader.as_deref()) {
            if !rw.is_empty() && rw == rl {
                anomalies.push(RuntimeSessionAnomaly::WorkerSessionEqualsLeaderSession {
                    value: rw.to_string(),
                });
            }
        }

        Self {
            worker,
            leader,
            anomalies,
        }
    }

    /// True if any session-identity anomaly was detected in state.
    pub fn has_anomalies(&self) -> bool {
        !self.anomalies.is_empty()
    }

    /// True if a `state.session_name` looked like a leader launcher session
    /// — the exact 0.3.39 shape unit-3 must refuse before any kill.
    pub fn worker_session_name_is_leader_prefixed(&self) -> bool {
        self.anomalies.iter().any(|a| {
            matches!(
                a,
                RuntimeSessionAnomaly::WorkerSessionNameIsLeaderPrefixed { .. }
            )
        })
    }
}

/// True when `name` is a leader launcher session (mirrors
/// `layout::sessions::is_leader_session` on a raw &str).
pub fn name_is_leader_launcher(name: &str) -> bool {
    let session = SessionName::new(name);
    is_leader_session(&session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn worker_session_new_rejects_leader_prefixed() {
        let err = WorkerSession::new("team-agent-leader-claude-x").unwrap_err();
        assert!(matches!(err, WorkerSessionError::LeaderPrefixed { .. }));
    }

    #[test]
    fn worker_session_new_rejects_empty() {
        assert!(matches!(
            WorkerSession::new("").unwrap_err(),
            WorkerSessionError::Empty
        ));
    }

    #[test]
    fn worker_session_new_accepts_team_prefixed() {
        let w = WorkerSession::new("team-foo").unwrap();
        assert_eq!(w.as_str(), "team-foo");
    }

    #[test]
    fn leader_launcher_session_new_requires_leader_prefix() {
        assert!(matches!(
            LeaderLauncherSession::new("team-foo").unwrap_err(),
            LeaderLauncherSessionError::MissingLeaderPrefix { .. }
        ));
        let l = LeaderLauncherSession::new("team-agent-leader-codex-abc").unwrap();
        assert_eq!(l.as_str(), "team-agent-leader-codex-abc");
    }

    #[test]
    fn runtime_sessions_from_state_clean() {
        let state = json!({
            "session_name": "team-foo",
            "leader_receiver": { "session_name": "team-agent-leader-codex-abc" },
        });
        let r = RuntimeSessions::from_state(&state);
        assert_eq!(r.worker.unwrap().as_str(), "team-foo");
        assert_eq!(r.leader.unwrap().as_str(), "team-agent-leader-codex-abc");
        assert!(r.anomalies.is_empty());
    }

    #[test]
    fn runtime_sessions_from_state_flags_leader_prefixed_worker_name() {
        // The 0.3.39 bug shape.
        let state = json!({ "session_name": "team-agent-leader-bad" });
        let r = RuntimeSessions::from_state(&state);
        assert!(r.worker.is_none());
        assert!(r.worker_session_name_is_leader_prefixed());
        assert!(r.has_anomalies());
    }

    #[test]
    fn runtime_sessions_from_state_flags_worker_equals_leader() {
        let state = json!({
            "session_name": "team-agent-leader-x",
            "leader_receiver": { "session_name": "team-agent-leader-x" },
        });
        let r = RuntimeSessions::from_state(&state);
        // Both anomalies fire: leader-prefixed worker + worker==leader.
        assert!(r.anomalies.iter().any(|a| matches!(
            a,
            RuntimeSessionAnomaly::WorkerSessionNameIsLeaderPrefixed { .. }
        )));
        assert!(r.anomalies.iter().any(|a| matches!(
            a,
            RuntimeSessionAnomaly::WorkerSessionEqualsLeaderSession { .. }
        )));
    }

    #[test]
    fn runtime_sessions_from_state_empty_state_is_clean() {
        let r = RuntimeSessions::from_state(&json!({}));
        assert!(r.worker.is_none());
        assert!(r.leader.is_none());
        assert!(r.anomalies.is_empty());
    }

    #[test]
    fn name_is_leader_launcher_matches_constant() {
        assert!(name_is_leader_launcher("team-agent-leader-x"));
        assert!(!name_is_leader_launcher("team-x"));
    }
}
