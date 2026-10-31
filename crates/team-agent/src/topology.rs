use std::process::Command;

use serde_json::{json, Value};

use crate::transport::{PaneInfo, SessionName, Transport};

pub const WORKER_PANE_BINDING_STALE: &str = "worker_pane_binding_stale";
pub const TMUX_ENDPOINT_SOCKET_CONFLICT: &str = "tmux_endpoint_socket_conflict";
pub const LEADER_RECEIVER_SOCKET_MISMATCH: &str = "leader_receiver_socket_mismatch";
pub const ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET: &str = "orphan_team_session_on_ignored_socket";
pub const TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET: &str =
    "team_session_missing_on_canonical_socket";
pub const RECENT_COORDINATOR_SESSION_MISSING: &str = "recent_coordinator_session_missing";

pub fn diagnose_topology_issues(state: &Value, backend: &dyn Transport) -> Vec<Value> {
    let mut issues = Vec::new();
    append_socket_split_issues(state, &mut issues, true);
    append_worker_pane_binding_issues(state, backend, &mut issues);
    issues
}

pub fn restart_dirty_topology_issue_ids(state: &Value) -> Vec<String> {
    let mut issues = Vec::new();
    append_socket_split_issues(state, &mut issues, false);
    issues
        .into_iter()
        .filter_map(|issue| issue.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub fn issue_id(issue: &Value) -> Option<&str> {
    issue.get("id").and_then(Value::as_str)
}

pub fn endpoint_socket_conflict(state: &Value) -> Option<Value> {
    let endpoint = non_empty_str(state, "tmux_endpoint")?;
    let socket = non_empty_str(state, "tmux_socket")?;
    if same_endpoint(endpoint, socket) {
        return None;
    }
    Some(json!({
        "id": TMUX_ENDPOINT_SOCKET_CONFLICT,
        "tmux_endpoint": endpoint,
        "tmux_socket": socket,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointConvergenceDecision {
    NoConflict,
    Converge {
        old_endpoint: String,
        new_endpoint: String,
        reason: &'static str,
    },
    RefuseLiveOldEndpoint {
        old_endpoint: String,
        new_endpoint: String,
        reason: &'static str,
    },
    Unknown,
}

pub fn endpoint_convergence_decision(
    state: &Value,
    team_key: &str,
    candidate_endpoint: &str,
) -> EndpointConvergenceDecision {
    let candidate_endpoint = candidate_endpoint.trim();
    if candidate_endpoint.is_empty() {
        return EndpointConvergenceDecision::Unknown;
    }
    if endpoint_server_alive(candidate_endpoint) != Some(true) {
        return EndpointConvergenceDecision::Unknown;
    }

    let mut stale_endpoints = Vec::new();
    collect_stale_endpoint(
        state,
        "tmux_endpoint",
        candidate_endpoint,
        &mut stale_endpoints,
    );
    collect_stale_endpoint(
        state,
        "tmux_socket",
        candidate_endpoint,
        &mut stale_endpoints,
    );
    if let Some(team_state) = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_key))
    {
        collect_stale_endpoint(
            team_state,
            "tmux_endpoint",
            candidate_endpoint,
            &mut stale_endpoints,
        );
        collect_stale_endpoint(
            team_state,
            "tmux_socket",
            candidate_endpoint,
            &mut stale_endpoints,
        );
    }
    stale_endpoints.sort();
    stale_endpoints.dedup();
    let Some(first_old) = stale_endpoints.first().cloned() else {
        return EndpointConvergenceDecision::NoConflict;
    };

    let mut converge_reason = "old_endpoint_dead";
    for old_endpoint in stale_endpoints {
        match endpoint_server_alive(&old_endpoint) {
            Some(true) => match old_endpoint_team_liveness(state, team_key, &old_endpoint) {
                OldEndpointTeamLiveness::TeamLive { reason } => {
                    return EndpointConvergenceDecision::RefuseLiveOldEndpoint {
                        old_endpoint,
                        new_endpoint: candidate_endpoint.to_string(),
                        reason,
                    };
                }
                OldEndpointTeamLiveness::TeamAbsent { reason } => {
                    converge_reason = reason;
                }
                OldEndpointTeamLiveness::Unknown => return EndpointConvergenceDecision::Unknown,
            },
            Some(false) => {}
            None => return EndpointConvergenceDecision::Unknown,
        }
    }

    EndpointConvergenceDecision::Converge {
        old_endpoint: first_old,
        new_endpoint: candidate_endpoint.to_string(),
        reason: converge_reason,
    }
}

enum OldEndpointTeamLiveness {
    TeamLive { reason: &'static str },
    TeamAbsent { reason: &'static str },
    Unknown,
}

fn old_endpoint_team_liveness(
    state: &Value,
    team_key: &str,
    old_endpoint: &str,
) -> OldEndpointTeamLiveness {
    let team_state = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(state);
    let session = non_empty_str(team_state, "session_name")
        .or_else(|| non_empty_str(state, "session_name"))
        .unwrap_or_default();
    if !session.is_empty() {
        match session_exists_on_endpoint_checked(old_endpoint, session) {
            Some(true) => {
                return OldEndpointTeamLiveness::TeamLive {
                    reason: "old_team_session_live",
                };
            }
            Some(false) => {}
            None => return OldEndpointTeamLiveness::Unknown,
        }
    }
    match old_endpoint_has_live_team_tuple(state, team_key, old_endpoint) {
        Some(true) => OldEndpointTeamLiveness::TeamLive {
            reason: "old_team_tuple_live",
        },
        Some(false) => OldEndpointTeamLiveness::TeamAbsent {
            reason: "old_team_session_absent_on_live_endpoint",
        },
        None => OldEndpointTeamLiveness::Unknown,
    }
}

fn old_endpoint_has_live_team_tuple(
    state: &Value,
    team_key: &str,
    old_endpoint: &str,
) -> Option<bool> {
    let backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(old_endpoint);
    let targets = backend.list_targets().ok()?;
    let team_state = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_key));
    for observed in &targets {
        if matches!(
            classify_registered_worker_for_observed_pane(team_state.unwrap_or(state), observed),
            WorkerPaneBindingMatch::LiveSameWorker { .. }
        ) || leader_receiver_tuple_matches(team_state.unwrap_or(state), observed)
        {
            return Some(true);
        }
        if team_state.is_some() {
            continue;
        }
        if let Some(teams) = state.get("teams").and_then(Value::as_object) {
            if teams.values().any(|entry| {
                matches!(
                    classify_registered_worker_for_observed_pane(entry, observed),
                    WorkerPaneBindingMatch::LiveSameWorker { .. }
                ) || leader_receiver_tuple_matches(entry, observed)
            }) {
                return Some(true);
            }
        }
    }
    Some(false)
}

fn leader_receiver_tuple_matches(state: &Value, observed: &PaneInfo) -> bool {
    let Some(receiver) = state.get("leader_receiver") else {
        return false;
    };
    let Some(cached_pane_id) = non_empty_str(receiver, "pane_id") else {
        return false;
    };
    if cached_pane_id != observed.pane_id.as_str() {
        return false;
    }
    let expected_session = non_empty_str(receiver, "session_name")
        .or_else(|| non_empty_str(state, "session_name"))
        .unwrap_or_default();
    if !expected_session.is_empty() && observed.session.as_str() != expected_session {
        return false;
    }
    if let Some(expected_window) = non_empty_str(receiver, "window_name") {
        if observed.window_name.as_ref().map(|w| w.as_str()) != Some(expected_window) {
            return false;
        }
    }
    if let (Some(expected_pid), Some(observed_pid)) = (agent_pane_pid(receiver), observed.pane_pid)
    {
        if expected_pid != observed_pid {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerPaneBindingMatch {
    LiveSameWorker {
        agent_id: String,
    },
    Stale {
        agent_id: String,
        reason: &'static str,
    },
    IncompleteLegacy {
        agent_id: String,
    },
    NoMatch,
}

pub fn classify_worker_pane_binding(
    agent_id: &str,
    agent: &Value,
    expected_session: &str,
    observed: &PaneInfo,
) -> WorkerPaneBindingMatch {
    let Some(cached_pane_id) = non_empty_str(agent, "pane_id") else {
        return WorkerPaneBindingMatch::NoMatch;
    };
    if cached_pane_id != observed.pane_id.as_str() {
        return WorkerPaneBindingMatch::NoMatch;
    }

    let Some(observed_window) = observed.window_name.as_ref().map(|window| window.as_str()) else {
        return WorkerPaneBindingMatch::IncompleteLegacy {
            agent_id: agent_id.to_string(),
        };
    };
    let expected_window = non_empty_str(agent, "window").unwrap_or(agent_id);
    if expected_session.is_empty() || expected_window.is_empty() {
        return WorkerPaneBindingMatch::IncompleteLegacy {
            agent_id: agent_id.to_string(),
        };
    }

    if let (Some(expected_pid), Some(observed_pid)) = (agent_pane_pid(agent), observed.pane_pid) {
        if expected_pid != observed_pid {
            return WorkerPaneBindingMatch::Stale {
                agent_id: agent_id.to_string(),
                reason: "pane_pid_mismatch",
            };
        }
    }

    if observed.session.as_str() == expected_session && observed_window == expected_window {
        WorkerPaneBindingMatch::LiveSameWorker {
            agent_id: agent_id.to_string(),
        }
    } else {
        WorkerPaneBindingMatch::Stale {
            agent_id: agent_id.to_string(),
            reason: "session_window_mismatch",
        }
    }
}

/// Positive worker ownership requires the socket and recorded live PID, not
/// just a pane number that another tmux server can reuse.
pub(crate) fn registered_worker_on_endpoint(
    state: &Value,
    endpoint: &str,
    observed: &PaneInfo,
) -> Option<String> {
    let agents = state.get("agents")?.as_object()?;
    agents.iter().find_map(|(agent_id, agent)| {
        let recorded_endpoint = non_empty_str(agent, "native_endpoint").map(str::to_string)
            .or_else(|| crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(agent)).map(|v| v.0))
            .or_else(|| crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(state)).map(|v| v.0))?;
        let pid = agent_pane_pid(agent).filter(|pid| *pid > 0)?;
        if !same_endpoint(&recorded_endpoint, endpoint) || observed.pane_pid != Some(pid) {
            return None;
        }
        matches!(classify_worker_pane_binding(agent_id, agent,
            non_empty_str(state, "session_name").unwrap_or_default(), observed),
            WorkerPaneBindingMatch::LiveSameWorker { .. }).then(|| agent_id.clone())
    })
}

/// Check one team's anchor against the live directory. A worker can also lead
/// a child team, so cross-team contradictions require a binding older than the
/// worker's current spawn. Unknown or newer sibling bindings stay protected.
pub(crate) fn leader_anchor_occupied_by_worker(
    scope: &Value,
    state: &Value,
    endpoint: &str,
    observed: &PaneInfo,
) -> bool {
    let mut found = false;
    for (key, peer_key, timestamp) in [
        ("leader_receiver", "team_owner", "attached_at"),
        ("team_owner", "leader_receiver", "claimed_at"),
    ] {
        let Some(anchor) = scope.get(key)
            .filter(|anchor| non_empty_str(anchor, "pane_id") == Some(observed.pane_id.as_str())) else {
            continue;
        };
        let anchor_endpoint = crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(anchor))
            .or_else(|| scope.get(peer_key)
                .filter(|peer| peer.get("pane_id") == anchor.get("pane_id"))
                .and_then(|peer| crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(peer))));
        let Some((anchor_endpoint, _)) = anchor_endpoint else { return false; };
        if !same_endpoint(&anchor_endpoint, endpoint) {
            continue;
        }
        found = true;
        if registered_worker_on_endpoint(scope, endpoint, observed).is_some() {
            continue;
        }
        let Some(bound_at) = anchor.get(timestamp).and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()) else {
            return false;
        };
        let predates_worker = std::iter::once(state)
            .chain(state.get("teams").and_then(Value::as_object).into_iter().flat_map(|t| t.values()))
            .filter_map(|worker_scope| {
                let id = registered_worker_on_endpoint(worker_scope, endpoint, observed)?;
                let spawned_at = worker_scope.get("agents")?.get(id)?.get("spawned_at")?.as_str()?;
                chrono::DateTime::parse_from_rfc3339(spawned_at).ok()
            })
            .any(|spawned_at| bound_at < spawned_at);
        if !predates_worker {
            return false;
        }
    }
    found
}

pub fn classify_registered_worker_for_observed_pane(
    state: &Value,
    observed: &PaneInfo,
) -> WorkerPaneBindingMatch {
    let mut fallback = WorkerPaneBindingMatch::NoMatch;
    classify_agents_for_observed_pane(state, observed, &mut fallback);
    if matches!(fallback, WorkerPaneBindingMatch::LiveSameWorker { .. }) {
        return fallback;
    }
    if let Some(teams) = state.get("teams").and_then(Value::as_object) {
        for team_state in teams.values() {
            classify_agents_for_observed_pane(team_state, observed, &mut fallback);
            if matches!(fallback, WorkerPaneBindingMatch::LiveSameWorker { .. }) {
                return fallback;
            }
        }
    }
    fallback
}

fn classify_agents_for_observed_pane(
    state: &Value,
    observed: &PaneInfo,
    fallback: &mut WorkerPaneBindingMatch,
) {
    let expected_session = non_empty_str(state, "session_name").unwrap_or_default();
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return;
    };
    for (agent_id, agent) in agents {
        let candidate = classify_worker_pane_binding(agent_id, agent, expected_session, observed);
        if matches!(candidate, WorkerPaneBindingMatch::LiveSameWorker { .. }) {
            *fallback = candidate;
            return;
        }
        if matches!(fallback, WorkerPaneBindingMatch::NoMatch)
            && !matches!(candidate, WorkerPaneBindingMatch::NoMatch)
        {
            *fallback = candidate;
        }
    }
}

fn append_socket_split_issues(state: &Value, issues: &mut Vec<Value>, include_readiness: bool) {
    let Some(conflict) = endpoint_socket_conflict(state) else {
        return;
    };
    let endpoint = non_empty_str(state, "tmux_endpoint").unwrap_or_default();
    let socket = non_empty_str(state, "tmux_socket").unwrap_or_default();

    let session = non_empty_str(state, "session_name").unwrap_or_default();
    issues.push(conflict);

    if state
        .get("leader_receiver")
        .and_then(|receiver| non_empty_str(receiver, "tmux_socket"))
        .is_some_and(|leader_socket| !same_endpoint(leader_socket, endpoint))
    {
        issues.push(json!({
            "id": LEADER_RECEIVER_SOCKET_MISMATCH,
            "tmux_endpoint": endpoint,
            "leader_receiver_tmux_socket": state
                .get("leader_receiver")
                .and_then(|receiver| non_empty_str(receiver, "tmux_socket")),
        }));
    }

    if !session.is_empty() && session_exists_on_endpoint(endpoint, session) {
        issues.push(json!({
            "id": ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET,
            "ignored_tmux_endpoint": endpoint,
            "session_name": session,
        }));
    }

    if include_readiness && !session.is_empty() && !session_exists_on_endpoint(socket, session) {
        issues.push(json!({
            "id": TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET,
            "tmux_endpoint": socket,
            "session_name": session,
        }));
        issues.push(json!({
            "id": RECENT_COORDINATOR_SESSION_MISSING,
            "tmux_endpoint": socket,
            "session_name": session,
        }));
    }
}

fn append_worker_pane_binding_issues(
    state: &Value,
    backend: &dyn Transport,
    issues: &mut Vec<Value>,
) {
    let session = non_empty_str(state, "session_name").unwrap_or_default();
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return;
    };
    if agents.is_empty() {
        return;
    }
    let Ok(live_targets) = backend.list_targets() else {
        return;
    };
    for (agent_id, agent) in agents {
        let Some(cached_pane_id) = non_empty_str(agent, "pane_id") else {
            continue;
        };
        let window = non_empty_str(agent, "window").unwrap_or(agent_id.as_str());
        let Some(observed) = live_targets
            .iter()
            .find(|pane| pane.pane_id.as_str() == cached_pane_id)
        else {
            continue;
        };
        let observed_window = observed
            .window_name
            .as_ref()
            .map(|window| window.as_str())
            .unwrap_or_default();
        let classification = classify_worker_pane_binding(agent_id, agent, &session, observed);
        if matches!(
            classification,
            WorkerPaneBindingMatch::LiveSameWorker { .. } | WorkerPaneBindingMatch::NoMatch
        ) {
            continue;
        }
        let reason = match classification {
            WorkerPaneBindingMatch::Stale { reason, .. } => reason,
            WorkerPaneBindingMatch::IncompleteLegacy { .. } => "incomplete_legacy_tuple",
            WorkerPaneBindingMatch::LiveSameWorker { .. } | WorkerPaneBindingMatch::NoMatch => {
                "unknown"
            }
        };
        issues.push(json!({
            "id": WORKER_PANE_BINDING_STALE,
            "agent_id": agent_id,
            "cached_pane_id": cached_pane_id,
            "expected_session": session,
            "expected_window": window,
            "observed_session": observed.session.as_str(),
            "observed_window": observed_window,
            "observed_pane_pid": observed.pane_pid,
            "reason": reason,
        }));
    }
}

fn agent_pane_pid(agent: &Value) -> Option<u32> {
    agent
        .get("pane_pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn session_exists_on_endpoint(endpoint: &str, session: &str) -> bool {
    session_exists_on_endpoint_checked(endpoint, session).unwrap_or(false)
}

pub(crate) fn team_session_ready_on_endpoint(endpoint: &str, session: &str) -> Option<bool> {
    if endpoint.is_empty() || session.is_empty() {
        return None;
    }
    let backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint);
    let targets = backend.list_targets().ok()?;
    Some(
        targets
            .iter()
            .any(|target| target.session.as_str() == session),
    )
}

fn session_exists_on_endpoint_checked(endpoint: &str, session: &str) -> Option<bool> {
    crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint)
        .has_session(&SessionName::new(session.to_string()))
        .ok()
}

fn collect_stale_endpoint(
    state: &Value,
    key: &str,
    candidate_endpoint: &str,
    stale_endpoints: &mut Vec<String>,
) {
    let Some(endpoint) = non_empty_str(state, key) else {
        return;
    };
    if !same_endpoint(endpoint, candidate_endpoint) {
        stale_endpoints.push(endpoint.to_string());
    }
}

fn endpoint_server_alive(endpoint: &str) -> Option<bool> {
    let normalized = normalize_endpoint(endpoint);
    let mut command = Command::new("tmux");
    if normalized != "default" && !normalized.is_empty() {
        if std::path::Path::new(&normalized).is_absolute() {
            command.arg("-S").arg(&normalized);
        } else {
            command.arg("-L").arg(&normalized);
        }
    }
    match command.arg("list-sessions").output() {
        Ok(output) => Some(output.status.success()),
        Err(_) => None,
    }
}

pub(crate) fn same_endpoint(left: &str, right: &str) -> bool {
    normalize_endpoint(left) == normalize_endpoint(right)
}

fn normalize_endpoint(value: &str) -> String {
    let path = if std::path::Path::new(value).is_absolute() {
        std::path::PathBuf::from(value)
    } else if let Some(path) = crate::tmux_backend::socket_path_for_name(value) {
        path
    } else {
        return value.to_string();
    };
    path.canonicalize().unwrap_or(path).to_string_lossy().into_owned()
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod leader_identity_tests {
    use super::*;

    fn pane() -> PaneInfo {
        PaneInfo {
            pane_id: crate::transport::PaneId::new("%0"),
            session: SessionName::new("team-a"), window_index: Some(0),
            window_name: Some(crate::transport::WindowName::new("worker")),
            pane_index: Some(0), tty: None, current_command: None,
            current_path: None, active: true, pane_pid: Some(4321),
            leader_env: Default::default(),
        }
    }

    fn state() -> Value {
        json!({"session_name":"team-a", "tmux_socket":"/tmp/identity-a.sock",
            "leader_receiver":{"pane_id":"%0","tmux_socket":"/tmp/identity-a.sock"},
            "agents":{"worker":{"pane_id":"%0","pane_pid":4321,"window":"worker"}}})
    }

    #[test]
    fn worker_occupancy_is_socket_and_pid_qualified() {
        let state = state();
        let mut pane = pane();
        assert!(leader_anchor_occupied_by_worker(&state, &state, "/tmp/identity-a.sock", &pane));
        assert!(!leader_anchor_occupied_by_worker(&state, &state, "/tmp/identity-b.sock", &pane));
        pane.pane_pid = Some(4322);
        assert!(!leader_anchor_occupied_by_worker(&state, &state, "/tmp/identity-a.sock", &pane));
    }

    #[test]
    fn unknown_pid_does_not_remove_leader_protection() {
        let mut state = state();
        state["agents"]["worker"].as_object_mut().unwrap().remove("pane_pid");
        assert!(!leader_anchor_occupied_by_worker(&state, &state, "/tmp/identity-a.sock", &pane()));
    }

    #[test]
    fn parent_worker_can_be_child_team_leader() {
        let mut state = state();
        state["leader_receiver"]["pane_id"] = json!("%9");
        state["teams"] = json!({"child":{"tmux_socket":"/tmp/identity-a.sock",
            "leader_receiver":{"pane_id":"%0","tmux_socket":"/tmp/identity-a.sock"},
            "agents":{}}});
        assert!(!leader_anchor_occupied_by_worker(&state["teams"]["child"], &state, "/tmp/identity-a.sock", &pane()));
    }
}
