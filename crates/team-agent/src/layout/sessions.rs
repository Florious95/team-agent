//! 0.3.28 layout step 1 — session name constructors + topology invariant guard.
//!
//! Python 0.2.11 truth source (recap):
//!   * Leader runs in a SEPARATE tmux session named
//!     `team-agent-leader-<provider>-<folder>-<sha1[:8]>` (the existing
//!     `crate::leader::start::leader_session_name`).
//!   * Worker session is `runtime.session_name` or `team-<team.name>` (the
//!     existing `lifecycle::launch::spec_session_name`).
//!   * These two names MUST be disjoint by prefix
//!     (`team-agent-leader-` vs `team-...`).
//!
//! This module re-exports those name constructors under a single namespace
//! and adds the topology invariant guard. No behaviour change in Step 1;
//! the guard runs as `tracing::warn!` so any drift is logged but not fatal.

use std::path::Path;

use crate::model::enums::Provider;
use crate::model::yaml::Value as YamlValue;
use crate::transport::SessionName;
use serde_json::Value as JsonValue;

/// `team-agent-leader-` — disjoint prefix for the leader session.
/// Re-exported from `leader::start::LEADER_SESSION_PREFIX` so layout-layer
/// code does not have to depend on the leader module's internals.
pub const LEADER_SESSION_PREFIX: &str = crate::leader::start::LEADER_SESSION_PREFIX;

/// Leader's dedicated tmux session name.
///
/// Format: `team-agent-leader-<provider_wire>-<folder>-<sha1(workspace)[:8]>`.
///
/// Re-exports `crate::leader::start::leader_session_name` so the rest of the
/// runtime can derive this name without touching the leader module. The
/// single underlying impl lives in `leader/start.rs` for now to minimise the
/// Step-1 diff; Step 2 will move it.
pub fn leader_session_name(provider: Provider, workspace: &Path) -> SessionName {
    crate::leader::start::leader_session_name(provider, workspace)
}

/// Worker tmux session name derived from the team spec.
///
/// Format: `spec.runtime.session_name` if set, else `team-<team.name>`
/// (default `team-agent`). This is a thin re-export of the existing
/// `lifecycle::launch::worker_session_name_pub` — exposed here so layout-
/// layer code has one place to ask. The spec is a `model::yaml::Value`
/// (the team spec parser's native type).
pub fn worker_session_name(spec: &YamlValue) -> SessionName {
    crate::lifecycle::launch::worker_session_name_pub(spec)
}

/// True when `name` starts with the leader session prefix.
pub fn is_leader_session(name: &SessionName) -> bool {
    name.as_str().starts_with(LEADER_SESSION_PREFIX)
}

/// True when `name` equals the worker session name derived from `spec`.
pub fn is_worker_session(name: &SessionName, spec: &YamlValue) -> bool {
    worker_session_name(spec).as_str() == name.as_str()
}

/// A detected topology invariant violation, surfaced as a warn-level log line
/// during the incremental migration (Steps 1–9). Hard error in Step 10.
#[derive(Debug, Clone)]
pub struct TopologyViolation {
    pub kind: TopologyViolationKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyViolationKind {
    /// Worker session name shares the leader-session prefix.
    WorkerSessionNamedAsLeader,
    /// A worker window inside the worker session is named `leader`.
    WorkerWindowNamedLeader,
    /// Two agents share the same `pane_id` in state.
    AgentPaneIdCollision,
    /// `leader_receiver.pane_id` matches an active agent's `pane_id`.
    LeaderPaneIdCollidesWithAgent,
}

/// Assert the runtime-state topology invariants (Step 1 — `warn!` only).
///
/// Checks:
///   1. The worker session name does NOT start with the leader prefix.
///   2. No agent window in worker session is literally named `leader`.
///   3. No two agents share the same `pane_id`.
///   4. `state.leader_receiver.pane_id` is not equal to any agent's
///      `pane_id` (E51 family — co-located lease corruption).
///
/// Returns the list of violations without panicking. Callers may choose to
/// emit `tracing::warn!` for each (the default in Steps 1–9) or escalate.
pub fn assert_topology_invariants(state: &JsonValue, spec: &YamlValue) -> Vec<TopologyViolation> {
    let mut out = Vec::new();
    let worker_session = worker_session_name(spec);
    if worker_session.as_str().starts_with(LEADER_SESSION_PREFIX) {
        out.push(TopologyViolation {
            kind: TopologyViolationKind::WorkerSessionNamedAsLeader,
            detail: format!(
                "worker session name `{}` starts with leader prefix `{LEADER_SESSION_PREFIX}` — \
                 leader and worker sessions must be disjoint",
                worker_session.as_str()
            ),
        });
    }
    let agents = state.get("agents").and_then(JsonValue::as_object);
    if let Some(agents) = agents {
        for (agent_id, agent) in agents {
            let window = agent
                .get("window")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if window.eq_ignore_ascii_case("leader") {
                out.push(TopologyViolation {
                    kind: TopologyViolationKind::WorkerWindowNamedLeader,
                    detail: format!(
                        "agent `{agent_id}` lives in window `leader` of worker session \
                         `{}` — workers must use `agent_id` window names",
                        worker_session.as_str()
                    ),
                });
            }
        }
        // Pane-id collision among agents.
        let mut by_pane: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (agent_id, agent) in agents {
            if let Some(pane) = agent
                .get("pane_id")
                .and_then(JsonValue::as_str)
                .filter(|s| !s.is_empty())
            {
                by_pane
                    .entry(pane.to_string())
                    .or_default()
                    .push(agent_id.to_string());
            }
        }
        for (pane, ids) in by_pane.iter() {
            if ids.len() > 1 {
                out.push(TopologyViolation {
                    kind: TopologyViolationKind::AgentPaneIdCollision,
                    detail: format!(
                        "pane_id `{pane}` shared by agents {ids:?} — at most one agent may bind a pane_id"
                    ),
                });
            }
        }
        // leader_receiver.pane_id ∉ any agent.pane_id.
        let leader_pane = state
            .get("leader_receiver")
            .and_then(|lr| lr.get("pane_id"))
            .and_then(JsonValue::as_str)
            .filter(|s| !s.is_empty());
        if let Some(leader_pane) = leader_pane {
            for (agent_id, agent) in agents {
                if agent
                    .get("pane_id")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|p| p == leader_pane)
                {
                    out.push(TopologyViolation {
                        kind: TopologyViolationKind::LeaderPaneIdCollidesWithAgent,
                        detail: format!(
                            "leader_receiver.pane_id `{leader_pane}` is also bound to \
                             agent `{agent_id}` — legacy bare-pane advisory only; validate \
                             endpoint/session/window/pane_pid tuple before using this as an \
                             identity blocker"
                        ),
                    });
                }
            }
        }
    }
    out
}

/// Logs each violation in `violations` to stderr under the
/// `team_agent::layout` tag. No-op when the list is empty. This is the
/// Step-1 surface — `eprintln!` matches the codebase's existing logging
/// convention (see `lifecycle::launch::eprintln!` calls). Hard error path
/// is deferred to Step 10.
pub fn log_topology_violations(violations: &[TopologyViolation]) {
    for v in violations {
        eprintln!(
            "team_agent::layout topology_invariant_violation kind={:?} detail={}",
            v.kind, v.detail
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec_for(team: &str) -> YamlValue {
        YamlValue::Map(vec![(
            "team".to_string(),
            YamlValue::Map(vec![("name".to_string(), YamlValue::Str(team.to_string()))]),
        )])
    }

    #[test]
    fn leader_and_worker_session_names_are_disjoint_by_prefix() {
        let leader = leader_session_name(Provider::Claude, Path::new("/tmp/proj"));
        let worker = worker_session_name(&spec_for("alpha"));
        assert!(
            leader.as_str().starts_with(LEADER_SESSION_PREFIX),
            "leader name `{}` must start with `{LEADER_SESSION_PREFIX}`",
            leader.as_str()
        );
        assert!(
            !worker.as_str().starts_with(LEADER_SESSION_PREFIX),
            "worker name `{}` must NOT start with `{LEADER_SESSION_PREFIX}`",
            worker.as_str()
        );
        assert_ne!(leader.as_str(), worker.as_str());
        assert!(is_leader_session(&leader));
        assert!(!is_leader_session(&worker));
        assert!(is_worker_session(&worker, &spec_for("alpha")));
        assert!(!is_worker_session(&leader, &spec_for("alpha")));
    }

    #[test]
    fn assert_topology_clean_state_returns_empty() {
        let state = json!({
            "agents": {
                "developer": { "pane_id": "%1", "window": "developer" },
                "tester":    { "pane_id": "%2", "window": "tester" }
            },
            "leader_receiver": { "pane_id": "%0" }
        });
        let v = assert_topology_invariants(&state, &spec_for("alpha"));
        assert!(
            v.is_empty(),
            "clean state should produce no violations; got {v:?}"
        );
    }

    #[test]
    fn assert_topology_flags_pane_id_collision() {
        let state = json!({
            "agents": {
                "developer": { "pane_id": "%1", "window": "developer" },
                "tester":    { "pane_id": "%1", "window": "tester" }
            },
            "leader_receiver": { "pane_id": "%0" }
        });
        let v = assert_topology_invariants(&state, &spec_for("alpha"));
        assert!(
            v.iter()
                .any(|x| matches!(x.kind, TopologyViolationKind::AgentPaneIdCollision)),
            "must flag AgentPaneIdCollision; got {v:?}"
        );
    }

    #[test]
    fn assert_topology_flags_leader_pane_overlap_with_agent() {
        let state = json!({
            "agents": {
                "developer": { "pane_id": "%0", "window": "developer" }
            },
            "leader_receiver": { "pane_id": "%0" }
        });
        let v = assert_topology_invariants(&state, &spec_for("alpha"));
        assert!(
            v.iter()
                .any(|x| matches!(x.kind, TopologyViolationKind::LeaderPaneIdCollidesWithAgent)),
            "must flag LeaderPaneIdCollidesWithAgent; got {v:?}"
        );
    }

    #[test]
    fn assert_topology_flags_worker_window_named_leader() {
        let state = json!({
            "agents": {
                "stray": { "pane_id": "%5", "window": "leader" }
            }
        });
        let v = assert_topology_invariants(&state, &spec_for("alpha"));
        assert!(
            v.iter()
                .any(|x| matches!(x.kind, TopologyViolationKind::WorkerWindowNamedLeader)),
            "must flag WorkerWindowNamedLeader; got {v:?}"
        );
    }
}
