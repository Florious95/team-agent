use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::provider::{
    CapturedSession, CapturedSessionCandidate, CaptureSessionContext, Provider, ProviderAdapter,
    ProviderError, SessionId,
};

pub const SESSION_CAPTURE_CONVERGENCE_DEADLINE_MS: u64 = 12_000;
pub const SESSION_CAPTURE_CONVERGENCE_POLL_MS: u64 = 250;
pub const RESTART_SESSION_CONVERGENCE_DEADLINE_MS: u64 = SESSION_CAPTURE_CONVERGENCE_DEADLINE_MS;
pub const RESTART_SESSION_CONVERGENCE_POLL_MS: u64 = SESSION_CAPTURE_CONVERGENCE_POLL_MS;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturePassReport {
    pub changed: bool,
    pub pending: Vec<String>,
    pub assigned: Vec<String>,
    pub ambiguous: Vec<AmbiguousSessionCapture>,
    pub candidate_count_by_agent: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousSessionCapture {
    pub agent_id: String,
    pub spawn_cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConvergence {
    pub converged: bool,
    pub changed: bool,
    pub missing: Vec<String>,
    pub deadline: std::time::Duration,
    pub elapsed: std::time::Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConvergenceProgress {
    pub iteration: u64,
    pub elapsed_ms: u128,
    pub deadline_ms: u128,
    pub remaining_ms: u128,
    pub changed: bool,
    pub assigned: Vec<String>,
    pub missing: Vec<String>,
    pub required_missing_agent_ids: Vec<String>,
    pub pending_agent_ids: Vec<String>,
    pub candidate_count_by_agent: BTreeMap<String, usize>,
}

/// Bounded session convergence barrier for destructive lifecycle gates.
///
/// This is intentionally not one opportunistic capture pass and not an
/// unbounded wait: callers must pass an explicit `deadline` and `poll_interval`.
/// Each poll runs the shared allocator once, reports progress, and sleeps until
/// either all required agents have provider sessions or the deadline expires.
pub fn converge_missing_provider_sessions<F, M, P>(
    state: &mut Value,
    adapter_for: &mut F,
    deadline: std::time::Duration,
    poll_interval: std::time::Duration,
    mut missing_agent_ids: M,
    mut progress: P,
) -> Result<SessionConvergence, String>
where
    F: FnMut(Provider) -> Box<dyn ProviderAdapter>,
    M: FnMut(&Value) -> Vec<String>,
    P: FnMut(SessionConvergenceProgress) -> Result<(), String>,
{
    let start = std::time::Instant::now();
    let deadline_at = start + deadline;
    let mut changed = false;
    let mut iteration = 0_u64;
    loop {
        let timeout_s = poll_interval.as_secs().max(1);
        let required_missing = missing_agent_ids(state);
        let report = capture_missing_provider_sessions_once(state, adapter_for, false, timeout_s)
            .map_err(|e| e.to_string())?;
        changed |= report.changed;
        let missing = missing_agent_ids(state);
        progress(SessionConvergenceProgress {
            iteration,
            elapsed_ms: start.elapsed().as_millis(),
            deadline_ms: deadline.as_millis(),
            remaining_ms: deadline_at
                .saturating_duration_since(std::time::Instant::now())
                .as_millis(),
            changed: report.changed,
            assigned: report.assigned,
            missing: missing.clone(),
            required_missing_agent_ids: required_missing,
            pending_agent_ids: missing.clone(),
            candidate_count_by_agent: report.candidate_count_by_agent.clone(),
        })?;
        if missing.is_empty() {
            if !report.ambiguous.is_empty() {
                let final_report = capture_missing_provider_sessions_once(state, adapter_for, true, timeout_s)
                    .map_err(|e| e.to_string())?;
                changed |= final_report.changed;
            }
            return Ok(SessionConvergence {
                converged: true,
                changed,
                missing,
                deadline,
                elapsed: start.elapsed(),
            });
        }
        let now = std::time::Instant::now();
        if now >= deadline_at {
            return Ok(SessionConvergence {
                converged: false,
                changed,
                missing: missing_agent_ids(state),
                deadline,
                elapsed: start.elapsed(),
            });
        }
        std::thread::sleep(std::cmp::min(
            poll_interval,
            deadline_at.saturating_duration_since(now),
        ));
        iteration += 1;
    }
}

pub fn capture_missing_provider_sessions_once<F>(
    state: &mut Value,
    adapter_for: &mut F,
    finalize_ambiguous: bool,
    timeout_s: u64,
) -> Result<CapturePassReport, ProviderError>
where
    F: FnMut(Provider) -> Box<dyn ProviderAdapter>,
{
    let Some(agent_map) = state.get("agents").and_then(Value::as_object) else {
        return Ok(CapturePassReport::default());
    };
    let mut pending = Vec::new();
    let mut candidates_by_agent = BTreeMap::new();
    for (agent_id, agent) in agent_map {
        let Some(capture) = pending_session_capture(agent_id, agent, adapter_for) else {
            continue;
        };
        let adapter = adapter_for(capture.provider);
        let candidates = adapter.capture_session_candidates(&capture.context, timeout_s)?;
        candidates_by_agent.insert(capture.agent_id.clone(), candidates);
        pending.push(capture);
    }

    let pending_ids = pending
        .iter()
        .map(|item| item.agent_id.clone())
        .collect::<BTreeSet<_>>();
    let mut claimed = claimed_provider_session_keys(agent_map, &pending_ids);
    let (assignments, ambiguous_ids) =
        allocate_session_candidates(&pending, &candidates_by_agent, &mut claimed);

    let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
        return Ok(CapturePassReport::default());
    };
    let mut report = CapturePassReport {
        pending: pending.iter().map(|item| item.agent_id.clone()).collect(),
        candidate_count_by_agent: candidates_by_agent
            .iter()
            .map(|(agent_id, candidates)| (agent_id.clone(), candidates.len()))
            .collect(),
        ..CapturePassReport::default()
    };
    for item in pending {
        let Some(agent_obj) = agents.get_mut(&item.agent_id).and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(candidate) = assignments.get(&item.agent_id) {
            apply_captured_session(agent_obj, &candidate.captured);
            report.changed = true;
            report.assigned.push(item.agent_id);
            continue;
        }
        if ambiguous_ids.contains(&item.agent_id) {
            report.ambiguous.push(AmbiguousSessionCapture {
                agent_id: item.agent_id.clone(),
                spawn_cwd: item.context.spawn_cwd.to_string_lossy().to_string(),
            });
            if finalize_ambiguous {
                agent_obj.insert("attribution_ambiguous".to_string(), serde_json::json!(true));
                agent_obj.insert(
                    "captured_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                );
                report.changed = true;
            }
        }
    }
    Ok(report)
}

pub fn incomplete_resumable_agent_ids(state: &Value) -> Vec<String> {
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = agents
        .iter()
        .filter_map(|(agent_id, agent)| {
            if pending_session_capture(agent_id, agent, &mut crate::provider::get_adapter).is_some() {
                Some(agent_id.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    out.sort();
    out
}

pub fn session_capture_complete(state: &Value) -> bool {
    incomplete_resumable_agent_ids(state).is_empty()
}

pub fn recover_resume_session_from_events(
    workspace: &Path,
    agent_id: &str,
    previous: &Value,
    adapter: &dyn ProviderAdapter,
    auth_mode: crate::provider::AuthMode,
    exclude_session_ids: &BTreeSet<String>,
) -> Result<Option<Value>, ProviderError> {
    let events = crate::event_log::EventLog::new(workspace)
        .tail(0)
        .map_err(|e| ProviderError::Io(e.to_string()))?;
    let current_session = previous
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|session| !session.is_empty());
    for event in events.iter().rev() {
        if !event_matches_agent(event, agent_id) {
            continue;
        }
        match event.get("event").and_then(Value::as_str) {
            Some("discard.session_tombstone") => return Ok(None),
            Some("session.captured") => {}
            _ => continue,
        }
        let Some(session_id) = event
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|session| !session.is_empty())
        else {
            continue;
        };
        if current_session == Some(session_id) || exclude_session_ids.contains(session_id) {
            continue;
        }
        let Some(rollout_path) = event_rollout_path(event).filter(|path| path.exists()) else {
            continue;
        };
        let session = SessionId::new(session_id.to_string());
        if !adapter.session_is_resumable(Some(&session), auth_mode)? {
            continue;
        }
        let mut repaired = previous.clone();
        if !repaired.is_object() {
            repaired = serde_json::json!({});
        }
        let Some(obj) = repaired.as_object_mut() else {
            continue;
        };
        obj.insert("session_id".to_string(), serde_json::json!(session_id));
        obj.insert(
            "rollout_path".to_string(),
            serde_json::json!(rollout_path.to_string_lossy().to_string()),
        );
        if let Some(ts) = event.get("ts").and_then(Value::as_str).filter(|ts| !ts.is_empty()) {
            obj.insert("captured_at".to_string(), serde_json::json!(ts));
        }
        obj.insert(
            "captured_via".to_string(),
            serde_json::json!("event_log_repair"),
        );
        if let Some(confidence) = event.get("attribution_confidence").cloned() {
            obj.insert("attribution_confidence".to_string(), confidence);
        }
        obj.remove("attribution_ambiguous");
        return Ok(Some(repaired));
    }
    Ok(None)
}

fn event_matches_agent(event: &Value, agent_id: &str) -> bool {
    ["agent_id", "worker_id"]
        .iter()
        .any(|key| event.get(*key).and_then(Value::as_str) == Some(agent_id))
}

fn event_rollout_path(event: &Value) -> Option<PathBuf> {
    event
        .get("rollout_path")
        .or_else(|| event.get("transcript_path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

pub fn incomplete_interacted_resumable_agent_ids(state: &Value) -> Vec<String> {
    let mut out = incomplete_resumable_agent_ids(state)
        .into_iter()
        .filter(|agent_id| {
            state
                .get("agents")
                .and_then(|agents| agents.get(agent_id))
                .and_then(|agent| agent.get("first_send_at"))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        .collect::<Vec<_>>();
    out.sort();
    out
}

struct PendingSessionCapture {
    agent_id: String,
    provider: Provider,
    context: CaptureSessionContext,
}

fn pending_session_capture<F>(
    agent_id: &str,
    agent: &Value,
    adapter_for: &mut F,
) -> Option<PendingSessionCapture>
where
    F: FnMut(Provider) -> Box<dyn ProviderAdapter>,
{
    if agent
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status != "running")
    {
        return None;
    }
    if agent_session_complete(agent) {
        return None;
    }
    let provider = agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(parse_provider)?;
    let spawn_cwd = agent
        .get("spawn_cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())?;
    if !adapter_for(provider).caps().resume {
        return None;
    }
    Some(PendingSessionCapture {
        agent_id: agent_id.to_string(),
        provider,
        context: CaptureSessionContext {
            agent_id: agent_id.to_string(),
            spawn_cwd: PathBuf::from(spawn_cwd),
            pane_id: agent
                .get("pane_id")
                .and_then(Value::as_str)
                .filter(|pane| !pane.is_empty())
                .map(str::to_string),
            pane_pid: agent
                .get("pane_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            spawned_at: agent
                .get("spawned_at")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            expected_session_id: agent
                .get("_pending_session_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(SessionId::new),
            provider_projects_root: agent
                .get("claude_projects_root")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        },
    })
}

fn agent_session_complete(agent: &Value) -> bool {
    agent
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|session| !session.is_empty())
        && agent
            .get("rollout_path")
            .and_then(Value::as_str)
            .is_some_and(|path| !path.is_empty())
}

fn allocate_session_candidates(
    pending: &[PendingSessionCapture],
    candidates_by_agent: &BTreeMap<String, Vec<CapturedSessionCandidate>>,
    claimed: &mut BTreeSet<String>,
) -> (BTreeMap<String, CapturedSessionCandidate>, BTreeSet<String>) {
    let mut assignments = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for item in pending {
        if let Some(candidate) = unique_available_candidate(
            candidates_by_agent.get(&item.agent_id),
            claimed,
            CandidateMatchKind::PositiveAgentId,
        ) {
            claimed.extend(captured_provider_session_keys(&candidate.captured));
            assignments.insert(item.agent_id.clone(), candidate);
        }
    }
    for item in pending {
        if assignments.contains_key(&item.agent_id) {
            continue;
        }
        if let Some(candidate) = unique_available_candidate(
            candidates_by_agent.get(&item.agent_id),
            claimed,
            CandidateMatchKind::PathAgentId,
        ) {
            claimed.extend(captured_provider_session_keys(&candidate.captured));
            assignments.insert(item.agent_id.clone(), candidate);
        }
    }
    allocate_global_one_to_one(pending, candidates_by_agent, claimed, &mut assignments);
    for item in pending {
        if assignments.contains_key(&item.agent_id) {
            continue;
        }
        match unique_available_candidate(
            candidates_by_agent.get(&item.agent_id),
            claimed,
            CandidateMatchKind::Any,
        ) {
            Some(candidate) => {
                claimed.extend(captured_provider_session_keys(&candidate.captured));
                assignments.insert(item.agent_id.clone(), candidate);
            }
            None => {
                if candidates_by_agent
                    .get(&item.agent_id)
                    .is_some_and(|candidates| !candidates.is_empty())
                {
                    ambiguous.insert(item.agent_id.clone());
                }
            }
        }
    }
    (assignments, ambiguous)
}

fn allocate_global_one_to_one(
    pending: &[PendingSessionCapture],
    candidates_by_agent: &BTreeMap<String, Vec<CapturedSessionCandidate>>,
    claimed: &mut BTreeSet<String>,
    assignments: &mut BTreeMap<String, CapturedSessionCandidate>,
) {
    let remaining_agents = pending
        .iter()
        .filter(|item| !assignments.contains_key(&item.agent_id))
        .map(|item| item.agent_id.clone())
        .collect::<Vec<_>>();
    if remaining_agents.is_empty() {
        return;
    }
    let mut candidates = BTreeMap::new();
    for agent_id in &remaining_agents {
        let Some(agent_candidates) = candidates_by_agent.get(agent_id) else {
            return;
        };
        for candidate in agent_candidates {
            if candidate_keys_collide(candidate, claimed) {
                continue;
            }
            let key = candidate_key(candidate);
            if key.is_empty() {
                continue;
            }
            candidates.entry(key).or_insert_with(|| candidate.clone());
        }
    }
    if candidates.len() != remaining_agents.len() {
        return;
    }
    for (agent_id, candidate) in remaining_agents.into_iter().zip(candidates.into_values()) {
        claimed.extend(captured_provider_session_keys(&candidate.captured));
        assignments.insert(agent_id, candidate);
    }
}

fn unique_available_candidate(
    candidates: Option<&Vec<CapturedSessionCandidate>>,
    claimed: &BTreeSet<String>,
    match_kind: CandidateMatchKind,
) -> Option<CapturedSessionCandidate> {
    let matches = candidates?
        .iter()
        .filter(|candidate| match match_kind {
            CandidateMatchKind::PositiveAgentId => candidate.positive_agent_id_match,
            CandidateMatchKind::PathAgentId => candidate.agent_path_match,
            CandidateMatchKind::Any => true,
        })
        .filter(|candidate| !candidate_keys_collide(candidate, claimed))
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

#[derive(Clone, Copy)]
enum CandidateMatchKind {
    PositiveAgentId,
    PathAgentId,
    Any,
}

fn candidate_keys_collide(candidate: &CapturedSessionCandidate, claimed: &BTreeSet<String>) -> bool {
    captured_provider_session_keys(&candidate.captured)
        .iter()
        .any(|key| claimed.contains(key))
}

fn candidate_key(candidate: &CapturedSessionCandidate) -> String {
    captured_provider_session_keys(&candidate.captured)
        .into_iter()
        .collect::<Vec<_>>()
        .join("|")
}

fn apply_captured_session(agent_obj: &mut serde_json::Map<String, Value>, captured: &CapturedSession) {
    if let Some(session_id) = &captured.session_id {
        agent_obj.insert("session_id".to_string(), serde_json::json!(session_id.as_str()));
    }
    if let Some(rollout_path) = &captured.rollout_path {
        agent_obj.insert(
            "rollout_path".to_string(),
            serde_json::json!(rollout_path.as_path().to_string_lossy()),
        );
    }
    agent_obj.insert(
        "captured_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent_obj.insert(
        "captured_via".to_string(),
        serde_json::to_value(captured.captured_via).unwrap_or(Value::Null),
    );
    agent_obj.insert(
        "attribution_confidence".to_string(),
        serde_json::to_value(captured.attribution_confidence).unwrap_or(Value::Null),
    );
    agent_obj.remove("attribution_ambiguous");
}

fn claimed_provider_session_keys(
    agents: &serde_json::Map<String, Value>,
    pending_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (agent_id, agent) in agents {
        if pending_ids.contains(agent_id) {
            continue;
        }
        if let Some(session_id) = agent
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            keys.insert(format!("session:{session_id}"));
        }
        if let Some(rollout_path) = agent
            .get("rollout_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            keys.insert(format!("rollout:{rollout_path}"));
        }
    }
    keys
}

fn captured_provider_session_keys(captured: &CapturedSession) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Some(session_id) = &captured.session_id {
        keys.insert(format!("session:{}", session_id.as_str()));
    }
    if let Some(rollout_path) = &captured.rollout_path {
        keys.insert(format!(
            "rollout:{}",
            rollout_path.as_path().to_string_lossy()
        ));
    }
    keys
}

fn parse_provider(raw: &str) -> Option<Provider> {
    match raw {
        "claude" => Some(Provider::Claude),
        "claude_code" => Some(Provider::ClaudeCode),
        "codex" => Some(Provider::Codex),
        "copilot" => Some(Provider::Copilot),
        "gemini_cli" => Some(Provider::GeminiCli),
        "fake" => Some(Provider::Fake),
        _ => None,
    }
}
