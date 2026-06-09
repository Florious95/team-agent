use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::event_log::EventLog;
use crate::model::enums::Provider;
use crate::model::ids::AgentId;

use super::runtime_observation::{
    CapturedRuntimeFact, LeaderCaptureFact, RuntimeObservationResults,
};
use super::types::{CompactionResult, LeaderApiError, SessionDriftResult};

const COMPACTION_RESET_THRESHOLD_DEFAULT: i64 = 3;

pub fn observe_runtime(
    workspace: &Path,
    state: &mut Value,
    captures_by_agent: BTreeMap<AgentId, CapturedRuntimeFact>,
    leader_capture: Option<LeaderCaptureFact>,
) -> RuntimeObservationResults {
    let event_log = EventLog::new(workspace);
    let mut compaction = Vec::new();
    let mut session_drift = Vec::new();
    for fact in captures_by_agent.values() {
        if let Some(result) = detect_compaction(state, &event_log, fact) {
            compaction.push(result);
        }
        if let Some(result) = detect_session_drift(state, &event_log, fact) {
            session_drift.push(result);
        }
    }
    let api_errors = detect_leader_api_error(state, &event_log, leader_capture.as_ref());
    RuntimeObservationResults {
        captures_by_agent,
        compaction,
        session_drift,
        api_errors,
    }
}

fn detect_compaction(
    state: &mut Value,
    event_log: &EventLog,
    fact: &CapturedRuntimeFact,
) -> Option<CompactionResult> {
    let count = count_compaction_markers(&fact.scrollback_tail);
    if count <= 0 {
        return None;
    }
    let team = fact
        .team_key
        .as_ref()
        .map(|team| team.as_str().to_string())
        .unwrap_or_else(|| crate::state::projection::team_state_key(state));
    let current = update_compaction_count(state, &team, &fact.agent_id, count);
    let provider = fact.provider;
    let _ = event_log.write(
        "coordinator.compaction_observed",
        json!({
            "agent_id": fact.agent_id.as_str(),
            "provider": provider.map(provider_name),
            "team": team,
            "compaction_count": current,
            "stuck_loop": false,
        }),
    );
    let threshold = compaction_reset_threshold(state);
    let recommendation = if provider == Some(Provider::Codex) && current >= threshold {
        let message = format!(
            "agent {} crossed Codex compaction threshold; run team-agent reset-agent {} --discard-session",
            fact.agent_id.as_str(),
            fact.agent_id.as_str()
        );
        let _ = event_log.write(
            "compaction_threshold_crossed.recommend_reset",
            json!({
                "agent_id": fact.agent_id.as_str(),
                "provider": provider.map(provider_name),
                "team": team,
                "compaction_count": current,
                "threshold": threshold,
                "leader_visible_message": message,
            }),
        );
        Some(message)
    } else {
        None
    };
    Some(CompactionResult {
        agent_id: fact.agent_id.clone(),
        provider,
        observed: true,
        reason: Some("compaction_observed".to_string()),
        recommendation,
    })
}

fn detect_session_drift(
    state: &mut Value,
    event_log: &EventLog,
    fact: &CapturedRuntimeFact,
) -> Option<SessionDriftResult> {
    if fact.provider != Some(Provider::Codex) {
        return None;
    }
    let stored = fact
        .stored_session_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())?;
    let actual = extract_thread_id_from_scrollback(&fact.scrollback_tail)?;
    if actual.eq_ignore_ascii_case(stored) {
        return None;
    }
    if fact
        .agent_state_snapshot
        .get("status")
        .and_then(Value::as_str)
        == Some("session_drift")
    {
        return None;
    }
    let detected_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    let remediation = "team-agent reset-agent --discard-session <agent>";
    let _ = event_log.write(
        "coordinator.session_drift_detected",
        json!({
            "agent_id": fact.agent_id.as_str(),
            "stored_session_id": stored,
            "actual_thread_id": actual,
            "status": "session_drift",
            "provider": "codex",
            "ts": detected_at,
            "remediation": remediation,
        }),
    );
    mark_agent_session_drift(
        state,
        &fact.agent_id,
        stored,
        &actual,
        &detected_at,
        remediation,
    );
    Some(SessionDriftResult {
        agent_id: fact.agent_id.clone(),
        stored_session_id: Some(stored.to_string()),
        observed_session_id: Some(actual),
        status: "session_drift".to_string(),
    })
}

fn detect_leader_api_error(
    state: &mut Value,
    event_log: &EventLog,
    leader_capture: Option<&LeaderCaptureFact>,
) -> Vec<LeaderApiError> {
    let Some(capture) = leader_capture else {
        return Vec::new();
    };
    let Some((error_class, snippet)) = match_api_error(&capture.scrollback_tail) else {
        clear_last_api_error_fingerprint(state);
        return Vec::new();
    };
    let fingerprint = format!("{error_class}::{}", tail_chars(&snippet, 120));
    if get_coordinator(state)
        .and_then(|c| c.get("last_api_error_fingerprint"))
        .and_then(Value::as_str)
        == Some(fingerprint.as_str())
    {
        return Vec::new();
    }
    let Some(coordinator) = coordinator_object_mut(state) else {
        return Vec::new();
    };
    coordinator.insert(
        "last_api_error_fingerprint".to_string(),
        Value::String(fingerprint.clone()),
    );
    let provider = leader_receiver_provider(capture.leader_receiver.as_ref())
        .or_else(|| leader_receiver_provider(state.get("leader_receiver")));
    let pane_id = capture
        .pane_id
        .as_ref()
        .map(|pane| pane.as_str().to_string())
        .or_else(|| {
            capture
                .leader_receiver
                .as_ref()
                .and_then(|r| r.get("pane_id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
    let leader_session_uuid = state
        .get("team_owner")
        .and_then(|owner| owner.get("leader_session_uuid"))
        .or_else(|| {
            capture
                .leader_receiver
                .as_ref()
                .and_then(|receiver| receiver.get("leader_session_uuid"))
        })
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let partial_response_streamed =
        scrollback_has_partial_response(&capture.scrollback_tail, &snippet);
    let _ = event_log.write(
        "leader.api_error",
        json!({
            "leader_session_uuid": leader_session_uuid,
            "error_class": error_class,
            "provider": provider.map(provider_name),
            "partial_response_streamed": partial_response_streamed,
            "worker_dispatch_just_before": [],
            "retry_count": 0,
            "matched_pattern_snippet": snippet.chars().take(160).collect::<String>(),
        }),
    );
    vec![LeaderApiError {
        provider,
        pane_id,
        fingerprint,
        message: snippet,
    }]
}

fn count_compaction_markers(scrollback: &str) -> i64 {
    let lower = scrollback.to_ascii_lowercase();
    lower.matches("context compacted").count() as i64
        + lower.matches("compaction occurred").count() as i64
}

fn update_compaction_count(state: &mut Value, team: &str, agent_id: &AgentId, count: i64) -> i64 {
    let Some(coordinator) = coordinator_object_mut(state) else {
        return count;
    };
    let Some(counts) = object_field_mut(coordinator, "compaction_counts") else {
        return count;
    };
    let Some(team_counts) = object_field_mut(counts, team) else {
        return count;
    };
    let previous = team_counts
        .get(agent_id.as_str())
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let current = previous.max(count);
    team_counts.insert(agent_id.as_str().to_string(), json!(current));
    current
}

fn compaction_reset_threshold(state: &Value) -> i64 {
    state
        .get("runtime")
        .and_then(|runtime| runtime.get("compaction_reset_threshold"))
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(COMPACTION_RESET_THRESHOLD_DEFAULT)
}

fn extract_thread_id_from_scrollback(scrollback: &str) -> Option<String> {
    let mut found = None;
    let lower = scrollback.to_ascii_lowercase();
    for needle in ["switched to thread", "resume", "thread"] {
        let mut offset = 0;
        while let Some(pos) = lower.get(offset..).and_then(|tail| tail.find(needle)) {
            let start = offset + pos + needle.len();
            if let Some(token) = first_token(scrollback.get(start..).unwrap_or_default()) {
                found = Some(token.to_ascii_lowercase());
            }
            offset = start;
        }
    }
    found
}

fn first_token(text: &str) -> Option<String> {
    let trimmed =
        text.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '=' | '#'));
    let trimmed = trimmed
        .strip_prefix("id")
        .map(|rest| {
            rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '=' | '#'))
        })
        .unwrap_or(trimmed);
    let token: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    (!token.is_empty()).then_some(token)
}

fn mark_agent_session_drift(
    state: &mut Value,
    agent_id: &AgentId,
    stored: &str,
    actual: &str,
    detected_at: &str,
    remediation: &str,
) {
    let drift = json!({
        "stored_session_id": stored,
        "actual_thread_id": actual,
        "detected_at": detected_at,
        "remediation": remediation,
    });
    if let Some(agent) = agent_object_mut(state, agent_id) {
        agent.insert(
            "status".to_string(),
            Value::String("session_drift".to_string()),
        );
        agent.insert("session_drift".to_string(), drift.clone());
    }
    if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
        for team in teams.values_mut() {
            if let Some(agent) = team
                .get_mut("agents")
                .and_then(Value::as_object_mut)
                .and_then(|agents| agents.get_mut(agent_id.as_str()))
                .and_then(Value::as_object_mut)
            {
                agent.insert(
                    "status".to_string(),
                    Value::String("session_drift".to_string()),
                );
                agent.insert("session_drift".to_string(), drift.clone());
            }
        }
    }
}

fn match_api_error(scrollback: &str) -> Option<(String, String)> {
    let lines: Vec<String> = scrollback
        .lines()
        .rev()
        .take(100)
        .map(str::trim)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut best = None;
    for start in 0..lines.len() {
        for size in 1..=3 {
            if start + size > lines.len() {
                break;
            }
            let mut window = lines[start..start + size]
                .iter()
                .filter(|line| !line.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if window.len() > 400 {
                window = tail_chars(&window, 400);
            }
            let lower = window.to_ascii_lowercase();
            let class = if lower.contains("api error: overloaded") {
                Some("Overloaded")
            } else if lower.contains("429 too many requests")
                || (has_api_context(&lower) && lower.contains("429"))
            {
                Some("RateLimit")
            } else if lower.contains("etimedout")
                || (has_api_context(&lower)
                    && (lower.contains("request timed out")
                        || lower.contains("request timeout")
                        || lower.contains("connection timed out")
                        || lower.contains("connection timeout")))
            {
                Some("Timeout")
            } else if has_api_context(&lower)
                && (lower.contains("500")
                    || lower.contains("502")
                    || lower.contains("503")
                    || lower.contains("504")
                    || lower.contains("fetch failed"))
            {
                Some("NetworkError")
            } else {
                None
            };
            if let Some(class) = class {
                best = Some((
                    start,
                    class.to_string(),
                    window.chars().take(240).collect::<String>(),
                ));
            }
        }
    }
    best.map(|(_, class, snippet)| (class, snippet))
}

fn has_api_context(lower: &str) -> bool {
    lower.contains("api error")
        || lower.contains("http error")
        || lower.contains("httperror")
        || lower.contains("request failed")
        || lower.contains("codex")
        || lower.contains("claude")
        || lower.contains("anthropic")
        || lower.contains("openai")
        || lower.contains("typeerror")
}

fn scrollback_has_partial_response(scrollback: &str, snippet: &str) -> bool {
    let Some(idx) = scrollback.rfind(snippet) else {
        return false;
    };
    let start = idx.saturating_sub(4000);
    let head = scrollback
        .get(start..idx)
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "assistant",
        "i'll ",
        "i will ",
        "i'm ",
        "i am ",
        "let me ",
        "> ",
    ]
    .iter()
    .any(|hint| head.contains(hint))
}

fn clear_last_api_error_fingerprint(state: &mut Value) {
    if let Some(coordinator) = get_coordinator_mut(state) {
        if coordinator.get("last_api_error_fingerprint").is_some() {
            coordinator.insert("last_api_error_fingerprint".to_string(), Value::Null);
        }
    }
}

fn leader_receiver_provider(receiver: Option<&Value>) -> Option<Provider> {
    let raw = receiver
        .and_then(|receiver| receiver.get("provider"))
        .and_then(Value::as_str)?;
    serde_json::from_value(Value::String(raw.to_string())).ok()
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini_cli",
        Provider::Fake => "fake",
    }
}

fn coordinator_object_mut(state: &mut Value) -> Option<&mut Map<String, Value>> {
    if !state.is_object() {
        *state = json!({});
    }
    let obj = state.as_object_mut()?;
    if !obj.get("coordinator").is_some_and(Value::is_object) {
        obj.insert("coordinator".to_string(), json!({}));
    }
    obj.get_mut("coordinator").and_then(Value::as_object_mut)
}

fn get_coordinator(state: &Value) -> Option<&Map<String, Value>> {
    state.get("coordinator").and_then(Value::as_object)
}

fn get_coordinator_mut(state: &mut Value) -> Option<&mut Map<String, Value>> {
    state.get_mut("coordinator").and_then(Value::as_object_mut)
}

fn object_field_mut<'a>(
    obj: &'a mut Map<String, Value>,
    key: &str,
) -> Option<&'a mut Map<String, Value>> {
    if !obj.get(key).is_some_and(Value::is_object) {
        obj.insert(key.to_string(), json!({}));
    }
    obj.get_mut(key).and_then(Value::as_object_mut)
}

fn agent_object_mut<'a>(
    state: &'a mut Value,
    agent_id: &AgentId,
) -> Option<&'a mut Map<String, Value>> {
    state
        .get_mut("agents")
        .and_then(Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(Value::as_object_mut)
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}
