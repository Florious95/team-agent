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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerPaneBindingMatch {
    LiveSameWorker { agent_id: String },
    Stale { agent_id: String, reason: &'static str },
    IncompleteLegacy { agent_id: String },
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
    let endpoint = non_empty_str(state, "tmux_endpoint");
    let socket = non_empty_str(state, "tmux_socket");
    let (Some(endpoint), Some(socket)) = (endpoint, socket) else {
        return;
    };
    if same_endpoint(endpoint, socket) {
        return;
    }

    let session = non_empty_str(state, "session_name").unwrap_or_default();
    issues.push(json!({
        "id": TMUX_ENDPOINT_SOCKET_CONFLICT,
        "tmux_endpoint": endpoint,
        "tmux_socket": socket,
    }));

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
    crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint)
        .has_session(&SessionName::new(session.to_string()))
        .unwrap_or(false)
}

fn same_endpoint(left: &str, right: &str) -> bool {
    normalize_endpoint(left) == normalize_endpoint(right)
}

fn normalize_endpoint(value: &str) -> String {
    if std::path::Path::new(value).is_absolute() {
        value.to_string()
    } else if let Some(path) = crate::tmux_backend::socket_path_for_name(value) {
        path.to_string_lossy().into_owned()
    } else {
        value.to_string()
    }
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}
