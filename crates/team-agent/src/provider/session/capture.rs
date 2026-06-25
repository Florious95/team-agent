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
    pub capture_failures: Vec<SessionCaptureFailure>,
    pub candidate_count_by_agent: BTreeMap<String, usize>,
    /// 0.4.6 Stage 3: agents that transitioned into the
    /// `transcript_missing` capture_state during this pass. The caller
    /// (coordinator tick) emits a throttled
    /// `provider.session.transcript_missing` event for each entry.
    pub transcript_missing: Vec<TranscriptMissing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCaptureFailure {
    pub agent_id: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousSessionCapture {
    pub agent_id: String,
    pub spawn_cwd: String,
}

/// 0.4.6 Stage 3: information emitted when a pending agent transitions
/// into the `transcript_missing` capture_state. Carries the diagnostic
/// fields the caller uses to write the
/// `provider.session.transcript_missing` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMissing {
    pub agent_id: String,
    pub spawn_cwd: String,
    pub spawn_epoch: u64,
    pub expected_session_id: Option<String>,
    pub candidate_count: usize,
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
    let mut capture_failures = Vec::new();
    let mut candidates_by_agent = BTreeMap::new();
    for (agent_id, agent) in agent_map {
        let Some(capture) = pending_session_capture(agent_id, agent, adapter_for) else {
            continue;
        };
        let adapter = adapter_for(capture.provider);
        let candidates = match adapter.capture_session_candidates(&capture.context, timeout_s) {
            Ok(candidates) => candidates,
            Err(error) => {
                capture_failures.push(SessionCaptureFailure {
                    agent_id: capture.agent_id.clone(),
                    error: error.to_string(),
                });
                pending.push(capture);
                continue;
            }
        };
        candidates_by_agent.insert(capture.agent_id.clone(), candidates);
        pending.push(capture);
    }

    let pending_ids = pending
        .iter()
        .map(|item| item.agent_id.clone())
        .collect::<BTreeSet<_>>();
    let mut claimed = claimed_provider_session_keys(state, agent_map, &pending_ids);
    let (assignments, ambiguous_ids) =
        allocate_session_candidates(&pending, &candidates_by_agent, &mut claimed);

    let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
        return Ok(CapturePassReport::default());
    };
    let mut report = CapturePassReport {
        pending: pending.iter().map(|item| item.agent_id.clone()).collect(),
        capture_failures,
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
            // Stage 1 (identity-boundary unified plan, architect direction
            // 2026-06-23): defensive expected-id guard. The adapter scanner
            // is the primary defence (Claude no longer falls back to same-
            // cwd latest when expected_session_id is set), but the allocator
            // also goes through a one-to-one global pass that could match a
            // wrong candidate if the per-agent list still has stale entries.
            // Refuse to write `session_id`/`rollout_path` when the candidate's
            // session_id is set AND differs from the pending expected_session_id.
            // Capture stays pending; the agent is marked ambiguous so the
            // operator sees it.
            let mismatch = item
                .context
                .expected_session_id
                .as_ref()
                .zip(candidate.captured.session_id.as_ref())
                .is_some_and(|(expected, captured)| expected.as_str() != captured.as_str());
            if mismatch {
                report.ambiguous.push(AmbiguousSessionCapture {
                    agent_id: item.agent_id.clone(),
                    spawn_cwd: item.context.spawn_cwd.to_string_lossy().to_string(),
                });
                if finalize_ambiguous {
                    agent_obj.insert(
                        "attribution_ambiguous".to_string(),
                        serde_json::json!(true),
                    );
                    // 0.4.6 tuple-atomic contract (audit:93): do NOT write
                    // `captured_at` for ambiguity diagnostics. `captured_at`
                    // is part of the authoritative tuple and may only be
                    // set together with `session_id + rollout_path +
                    // captured_via`. Overloading it as both capture
                    // timestamp and ambiguity timestamp created false
                    // "looks captured" rows. Ambiguity diagnostics live in
                    // the `attribution_ambiguous` flag + events; persist
                    // event log for the timestamp.
                    report.changed = true;
                }
                continue;
            }
            // 0.4.6 tuple-atomic contract: apply_captured_session refuses
            // partial candidates (no session_id or no rollout_path). When
            // it returns false, the row stays unchanged; capture re-runs
            // on the next coordinator tick.
            if apply_captured_session(agent_obj, &candidate.captured) {
                report.changed = true;
                report.assigned.push(item.agent_id);
            }
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
                    "capture_state".to_string(),
                    serde_json::json!("attribution_ambiguous"),
                );
                // 0.4.6 tuple-atomic contract: see comment above. Removed
                // `captured_at` ambiguity write.
                report.changed = true;
            }
            continue;
        }
        // 0.4.6 Stage 3: transcript-ready handshake. For pending agents that
        // are NOT ambiguous and were NOT captured this pass, classify into:
        //   * `captured` — session_id + rollout_path already on row
        //   * `transcript_missing` — trigger event has fired (report_result /
        //     first_send_at / pane_output_advanced) AND elapsed > threshold
        //     AND zero candidates / no backing
        //   * `waiting_for_transcript` — still in grace period
        // The state field surfaces in status/diagnose without changing the
        // authoritative tuple contract (we never write session_id without
        // backing). Throttled event emission is done by the caller (tick).
        let has_session = agent_obj
            .get("session_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
            && agent_obj
                .get("rollout_path")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
        if has_session {
            // Captured already — fix state field if drifted.
            if agent_obj
                .get("capture_state")
                .and_then(Value::as_str)
                != Some("captured")
            {
                agent_obj.insert("capture_state".to_string(), serde_json::json!("captured"));
                report.changed = true;
            }
            continue;
        }
        let candidate_count = report
            .candidate_count_by_agent
            .get(&item.agent_id)
            .copied()
            .unwrap_or(0);
        let next_state = classify_pending_capture_state(agent_obj, candidate_count);
        let prev_state = agent_obj
            .get("capture_state")
            .and_then(Value::as_str)
            .map(str::to_string);
        if prev_state.as_deref() != Some(next_state) {
            agent_obj.insert(
                "capture_state".to_string(),
                serde_json::json!(next_state),
            );
            report.changed = true;
            if next_state == "transcript_missing" {
                report.transcript_missing.push(TranscriptMissing {
                    agent_id: item.agent_id.clone(),
                    spawn_cwd: item.context.spawn_cwd.to_string_lossy().to_string(),
                    spawn_epoch: agent_obj
                        .get("spawn_epoch")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    expected_session_id: item
                        .context
                        .expected_session_id
                        .as_ref()
                        .map(|s| s.as_str().to_string()),
                    candidate_count,
                });
            }
        }
    }
    Ok(report)
}

/// 0.4.6 Stage 3: classify a pending agent's capture state into one of:
///   * `transcript_missing` — has had a strong trigger event AND elapsed
///     beyond grace threshold AND still no backing
///   * `waiting_for_transcript` — still in grace period or no trigger yet
///
/// Trigger events (CR M2): any of `first_send_at`, `last_result_at`, or a
/// recent `last_pane_output_at` indicates the worker has been interactively
/// used / done something. Without ANY trigger, we stay in waiting state
/// even past the grace window (silent worker is not a failure to capture).
///
/// Threshold: 30 seconds from spawned_at OR 15 seconds from the first
/// trigger event, whichever is later. This avoids spurious
/// transcript_missing on slow first-paint while catching the
/// release-engineer scenario (report_result fired but no jsonl).
fn classify_pending_capture_state(
    agent_obj: &serde_json::Map<String, Value>,
    candidate_count: usize,
) -> &'static str {
    // Already-captured agents don't reach here (caller skips). We only
    // classify pending agents.
    let now = chrono::Utc::now();
    let spawned_at = agent_obj
        .get("spawned_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let elapsed_since_spawn = spawned_at
        .map(|t| now.signed_duration_since(t).num_seconds())
        .unwrap_or(0);
    // Trigger event detection — any strong signal that the worker has
    // actually been used. Pre-conditions (CR M2 in architect doc).
    let has_trigger = ["first_send_at", "last_result_at", "last_pane_output_at"]
        .iter()
        .any(|key| {
            agent_obj
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        });
    // Earliest trigger timestamp for the secondary threshold.
    let earliest_trigger = ["first_send_at", "last_result_at", "last_pane_output_at"]
        .iter()
        .filter_map(|key| {
            agent_obj
                .get(*key)
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
        .min();
    let elapsed_since_trigger = earliest_trigger
        .map(|t| now.signed_duration_since(t).num_seconds())
        .unwrap_or(0);
    const SPAWN_GRACE_SECS: i64 = 30;
    const TRIGGER_GRACE_SECS: i64 = 15;
    let past_spawn_grace = elapsed_since_spawn >= SPAWN_GRACE_SECS;
    let past_trigger_grace = has_trigger && elapsed_since_trigger >= TRIGGER_GRACE_SECS;
    // transcript_missing requires BOTH: a trigger occurred AND we've
    // waited past the spawn grace (so we're not blamed for slow startup).
    // candidate_count == 0 is the "no backing" signal; non-zero
    // candidates that didn't satisfy assignment go through the ambiguous
    // path above and don't reach here.
    if has_trigger && past_spawn_grace && past_trigger_grace && candidate_count == 0 {
        "transcript_missing"
    } else {
        "waiting_for_transcript"
    }
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
    // E57 postflight: read the agent's provider once; needed for the Claude
    // leader-marker filter below. Falls back to None on non-string / missing,
    // in which case the marker filter is skipped (only meaningful for Claude).
    let provider = previous
        .get("provider")
        .and_then(Value::as_str)
        .and_then(parse_provider);
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
        // E57 postflight (lane-046-capture-gap): the capture allocator already
        // refuses Claude leader transcripts (P0 d39b5104,
        // claude_records_have_leader_marker in provider/adapter.rs). Event-log
        // repair must apply the SAME filter — otherwise a stale `session.captured`
        // event from a pre-fix run (or from a window where the allocator was
        // bypassed) still pulls the 590MB leader transcript onto a worker on the
        // next restart (Mac mini repro: session_id=ea059b82 reassigned to
        // release-engineer via captured_via=event_log_repair). The check is
        // a no-op for non-Claude providers.
        if let Some(p) = provider {
            if crate::provider::adapter::rollout_path_has_claude_leader_marker(p, &rollout_path) {
                continue;
            }
        }
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
        // 0.4.6 tuple-atomic contract (audit §Capture 修改清单, line 113):
        // repair must yield a COMPLETE tuple. session_id + rollout_path are
        // already validated above; ensure captured_at + captured_via are
        // also always written (fall back to now() if the event has no ts).
        obj.insert("session_id".to_string(), serde_json::json!(session_id));
        obj.insert(
            "rollout_path".to_string(),
            serde_json::json!(rollout_path.to_string_lossy().to_string()),
        );
        let captured_at = event
            .get("ts")
            .and_then(Value::as_str)
            .filter(|ts| !ts.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        obj.insert("captured_at".to_string(), serde_json::json!(captured_at));
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
            // 0.3.31 Codex capture correction: Codex does NOT honor
            // `--session-id`, so any `_pending_session_id` stored for a Codex
            // agent (stale 0.3.30 state, or the framework's pre-spawn token)
            // is a local-only token and must NOT be used as expected_session_id
            // — that would trigger the Stage 1 mismatch guard against Codex's
            // real session_meta.payload.id and permanently reject the correct
            // transcript. Codex capture anchors purely on (cwd, spawned_at).
            // 0.4.7 (B1 verified, partial-resume revert of 9feafc31):
            // Claude --session-id was restored in adapter.rs build_command_plan
            // because Claude ≥ 2.1.185 DOES honour framework-supplied session
            // id and DOES create a transcript at that id. So `_pending_session_id`
            // is once again valid for Claude — re-enable the Stage 1 pre-pass
            // for Claude/ClaudeCode. Codex remains excluded (Codex CLI ignores
            // --session-id, capture must anchor purely on cwd+spawned_at).
            expected_session_id: if matches!(provider, Provider::Codex) {
                None
            } else {
                agent
                    .get("_pending_session_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(SessionId::new)
            },
            provider_projects_root: agent
                .get("claude_projects_root")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        },
    })
}

fn agent_session_complete(agent: &Value) -> bool {
    // RM-039-SESS-001 step 1 (architect verdict 2026-06-22): historically
    // this returned true the moment `rollout_path` was non-empty — even when
    // the file no longer existed on disk. That created a "stale-positive"
    // capture tuple: the worker had a session_id + a stored rollout_path,
    // but the provider had rotated/garbage-collected the actual transcript
    // file. `pending_session_capture` skipped agents whose
    // `agent_session_complete()` was true, so the runtime never recaptured
    // the now-broken backing, and downstream consumers saw the stale tuple
    // as authoritative.
    //
    // Fix: a non-empty `rollout_path` only counts as complete when the path
    // actually exists. session_id remains a required non-empty check
    // (architect directive: "Keep session_id non-empty as required; do not
    // infer context from event log alone unless the referenced transcript
    // path exists.").
    //
    // Blocker-2 Layer-1 (prerelease 0.4.0, architect bugs-prerelease-blockers.md §139):
    // existence alone is not enough — a small Claude session-metadata JSON
    // (~/.claude/sessions/<pid>.json, ~300-400 bytes, sessionId only, no
    // assistant/user records) used to satisfy this and prevent recapture of
    // the real .claude/projects/<cwd>/<sid>.jsonl transcript. For
    // Claude/ClaudeCode rollout paths, additionally require a recognizable
    // transcript lifecycle record (an assistant or user record). Other
    // providers keep existence-only semantics (codex 不可改项).
    let session_id_ok = agent
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|session| !session.is_empty());
    if !session_id_ok {
        return false;
    }
    let rollout_path = match agent
        .get("rollout_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        Some(path) => path,
        None => return false,
    };
    // 0.4.6 tuple-atomic contract (audit §Capture 修改清单, line 110):
    // a "complete" agent session requires the FULL authoritative tuple —
    // session_id + rollout_path + captured_at + captured_via. Without these
    // last two, a row is a partial tuple and capture must re-run.
    let captured_at_ok = agent
        .get("captured_at")
        .and_then(Value::as_str)
        .is_some_and(|ts| !ts.is_empty());
    let captured_via_ok = agent
        .get("captured_via")
        .and_then(Value::as_str)
        .is_some_and(|via| !via.is_empty());
    if !captured_at_ok || !captured_via_ok {
        return false;
    }
    let path = std::path::Path::new(rollout_path);
    if !path.exists() {
        return false;
    }
    let provider_wire = agent.get("provider").and_then(Value::as_str).unwrap_or("");
    if matches!(provider_wire, "claude" | "claude-code" | "claude_code") {
        return claude_rollout_has_lifecycle_records(path);
    }
    true
}

/// Blocker-2 Layer-1 (prerelease 0.4.0): a Claude rollout file qualifies as
/// activity backing only when it contains at least one recognizable transcript
/// lifecycle record (top-level `type:"assistant"` or `type:"user"`). The
/// ~/.claude/sessions/<pid>.json metadata file has neither, so this returns
/// false for it and the runtime recaptures via the .claude/projects/<cwd>/
/// scan in adapter.rs. Bounded read so a large transcript is not slurped.
fn claude_rollout_has_lifecycle_records(path: &std::path::Path) -> bool {
    use std::io::Read;
    const MAX_BYTES: u64 = 65_536;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(MAX_BYTES).read_to_end(&mut bytes).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "assistant" | "user") {
            return true;
        }
    }
    false
}

fn allocate_session_candidates(
    pending: &[PendingSessionCapture],
    candidates_by_agent: &BTreeMap<String, Vec<CapturedSessionCandidate>>,
    claimed: &mut BTreeSet<String>,
) -> (BTreeMap<String, CapturedSessionCandidate>, BTreeSet<String>) {
    let mut assignments = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();

    // Stage 1 amendment (architect direction 2026-06-23, S1-CAPTURE-002 fix):
    // ExpectedSessionId pre-pass. For each pending agent that carries an
    // `_pending_session_id`, the strongest possible binding is the candidate
    // whose `session_id` exactly matches that expected id. Run this BEFORE
    // the existing PositiveAgentId / PathAgentId / global one-to-one passes,
    // because the global one-to-one pass uses
    // `remaining_agents.zip(candidates.into_values())` (BTreeMap sorted by
    // candidate-key, not by agent ownership) and can produce CROSS
    // assignments (claude-a → claude-b's transcript and vice versa) when two
    // pending agents both have expected ids but no PositiveAgentId/PathAgentId
    // hint — those would be rejected by the mismatch guard at apply time,
    // leaving both agents `attribution_ambiguous` instead of correctly
    // bound. With this pre-pass the global one-to-one only ever sees agents
    // that have NO expected id (or whose expected candidate is unavailable
    // / collides), and the cross-assignment hazard is eliminated.
    for item in pending {
        let Some(expected) = item.context.expected_session_id.as_ref() else {
            continue;
        };
        let Some(agent_candidates) = candidates_by_agent.get(&item.agent_id) else {
            continue;
        };
        let exact_matches: Vec<&CapturedSessionCandidate> = agent_candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .captured
                    .session_id
                    .as_ref()
                    .is_some_and(|sid| sid.as_str() == expected.as_str())
            })
            .filter(|candidate| !candidate_keys_collide(candidate, claimed))
            .collect();
        // Uniqueness requirement: only assign when the expected id maps to
        // exactly one available candidate. Multiple matches or a colliding
        // single match leave the agent for the ambiguity path below.
        if exact_matches.len() == 1 {
            let candidate = exact_matches[0].clone();
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
        // P0 (lane-046-capture-gap): Claude/ClaudeCode with no
        // expected_session_id (natural fresh — Stage 1 expected-id miss
        // guard cannot fire) must NOT accept the weak `Any` fallback.
        // Without this guard, a same-cwd leader transcript (no positive
        // agent-id match, no path match) becomes the sole candidate via
        // the time window and gets attributed to a worker. Only
        // positive-agent-id / path-agent-id can authoritatively bind a
        // Claude no-expected worker session.
        let claude_no_expected = matches!(
            item.provider,
            Provider::Claude | Provider::ClaudeCode
        ) && item.context.expected_session_id.is_none();
        if claude_no_expected {
            if candidates_by_agent
                .get(&item.agent_id)
                .is_some_and(|candidates| !candidates.is_empty())
            {
                ambiguous.insert(item.agent_id.clone());
            }
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
    // P0 (lane-046-capture-gap): exclude Claude no-expected agents from the
    // global one-to-one weak allocator. They must use PositiveAgentId or
    // PathAgentId only — see allocate_session_candidates Any-block.
    let remaining_agents = pending
        .iter()
        .filter(|item| !assignments.contains_key(&item.agent_id))
        .filter(|item| {
            !(matches!(item.provider, Provider::Claude | Provider::ClaudeCode)
                && item.context.expected_session_id.is_none())
        })
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

/// 0.4.6 tuple-atomic contract (audit §Capture 修改清单, line 111): writes
/// the authoritative session tuple if and only if the candidate carries
/// BOTH `session_id` and `rollout_path`. A partial candidate (e.g.
/// scanner returned ambiguous attribution with no session_id) MUST NOT
/// stamp `captured_at` / `captured_via` onto the row, because that
/// produces a partial tuple that downstream readers treat as authority.
/// Returns true iff the full tuple was written.
fn apply_captured_session(
    agent_obj: &mut serde_json::Map<String, Value>,
    captured: &CapturedSession,
) -> bool {
    let Some(session_id) = captured.session_id.as_ref() else {
        return false;
    };
    let Some(rollout_path) = captured.rollout_path.as_ref() else {
        return false;
    };
    agent_obj.insert(
        "session_id".to_string(),
        serde_json::json!(session_id.as_str()),
    );
    agent_obj.insert(
        "rollout_path".to_string(),
        serde_json::json!(rollout_path.as_path().to_string_lossy()),
    );
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
    true
}

fn claimed_provider_session_keys(
    state: &Value,
    agents: &serde_json::Map<String, Value>,
    pending_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    // 1. Non-pending worker sessions (existing behaviour).
    for (agent_id, agent) in agents {
        if pending_ids.contains(agent_id) {
            continue;
        }
        push_provider_session_keys(&mut keys, agent);
    }
    // 2. P0 (lane-046-capture-gap): leader anchor sessions. The leader's
    //    own provider transcript must never be attributed to a worker. Scan
    //    state.leader_receiver, state.team_owner, and the same fields under
    //    state.teams.<key>. Cover all known provider session field names so
    //    a present transcript path/id excludes that session from worker
    //    allocation.
    push_leader_provider_session_keys(&mut keys, state);
    if let Some(teams) = state.get("teams").and_then(Value::as_object) {
        for team_state in teams.values() {
            push_leader_provider_session_keys(&mut keys, team_state);
        }
    }
    keys
}

fn push_provider_session_keys(keys: &mut BTreeSet<String>, value: &Value) {
    for field in ["session_id", "provider_session_id"] {
        if let Some(session_id) = value
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            keys.insert(format!("session:{session_id}"));
        }
    }
    for field in ["rollout_path", "transcript_path"] {
        if let Some(path) = value
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            keys.insert(format!("rollout:{path}"));
        }
    }
}

fn push_leader_provider_session_keys(keys: &mut BTreeSet<String>, scope: &Value) {
    for anchor in ["leader_receiver", "team_owner"] {
        if let Some(node) = scope.get(anchor) {
            push_provider_session_keys(keys, node);
        }
    }
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

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone)]
    pub(crate) struct CaptureCandidatesAdapter {
        provider: Provider,
        fail_agent_id: Option<String>,
        error: String,
        /// Stage 1 amendment test support (architect direction 2026-06-23):
        /// per-agent candidate map. When set, the adapter returns the mapped
        /// candidates for `context.agent_id` instead of an empty list. Lets
        /// the allocator-level test inject expected-id candidates for two
        /// pending agents and observe cross-assignment regressions.
        candidates_by_agent: Option<Arc<BTreeMap<String, Vec<CapturedSessionCandidate>>>>,
    }

    impl CaptureCandidatesAdapter {
        pub(crate) fn new(provider: Provider, fail_agent_id: Option<&str>, error: &str) -> Self {
            Self {
                provider,
                fail_agent_id: fail_agent_id.map(str::to_string),
                error: error.to_string(),
                candidates_by_agent: None,
            }
        }

        pub(crate) fn with_candidates(
            mut self,
            candidates_by_agent: BTreeMap<String, Vec<CapturedSessionCandidate>>,
        ) -> Self {
            self.candidates_by_agent = Some(Arc::new(candidates_by_agent));
            self
        }
    }

    impl ProviderAdapter for CaptureCandidatesAdapter {
        fn provider(&self) -> Provider {
            self.provider
        }

        fn caps(&self) -> crate::provider::ProviderCaps {
            crate::provider::ProviderCaps {
                resume: true,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: false,
            }
        }

        fn is_installed(&self) -> bool {
            true
        }

        fn version(&self) -> Result<String, ProviderError> {
            Ok("test".to_string())
        }

        fn auth_hint(
            &self,
            _auth_mode: crate::provider::AuthMode,
        ) -> crate::provider::AuthHintStatus {
            crate::provider::AuthHintStatus::Unknown
        }

        fn build_command(
            &self,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
            _system_prompt: Option<&str>,
            _model: Option<&str>,
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn build_command_with_tools(
            &self,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
            _system_prompt: Option<&str>,
            _model: Option<&str>,
            _tools: &[&str],
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn capture_session_id(
            &self,
            _agent_id: &str,
            _spawn_cwd: &std::path::Path,
            _timeout_s: u64,
        ) -> Result<Option<CapturedSession>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn capture_session_candidates(
            &self,
            context: &CaptureSessionContext,
            _timeout_s: u64,
        ) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
            if self.fail_agent_id.as_deref() == Some(context.agent_id.as_str()) {
                return Err(ProviderError::Io(self.error.clone()));
            }
            if let Some(map) = self.candidates_by_agent.as_ref() {
                if let Some(candidates) = map.get(&context.agent_id) {
                    return Ok(candidates.clone());
                }
            }
            Ok(Vec::new())
        }

        fn recover_session_id(
            &self,
            _agent_id: &str,
            _spawn_cwd: &std::path::Path,
        ) -> Result<Option<SessionId>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn session_is_resumable(
            &self,
            _session_id: Option<&SessionId>,
            _auth_mode: crate::provider::AuthMode,
        ) -> Result<bool, ProviderError> {
            Ok(true)
        }

        fn build_resume_command(
            &self,
            _session_id: Option<&SessionId>,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn build_resume_command_with_context(
            &self,
            _session_id: Option<&SessionId>,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
            _system_prompt: Option<&str>,
            _model: Option<&str>,
            _tools: &[&str],
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn fork(
            &self,
            _session_id: Option<&SessionId>,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn fork_with_context(
            &self,
            _session_id: Option<&SessionId>,
            _auth_mode: crate::provider::AuthMode,
            _mcp_config: Option<&crate::provider::McpConfig>,
            _system_prompt: Option<&str>,
            _model: Option<&str>,
            _tools: &[&str],
        ) -> Result<Vec<String>, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn mcp_config(
            &self,
            _auth_mode: crate::provider::AuthMode,
        ) -> Result<crate::provider::McpConfig, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn install_mcp(&self, _config: &crate::provider::McpConfig) -> Result<(), ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn status_patterns(&self) -> Result<crate::provider::StatusPatterns, ProviderError> {
            Err(ProviderError::CapabilityUnsupported(
                "test adapter".to_string(),
            ))
        }

        fn validate_model(&self, _model: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod u1_tests {
    use super::*;

    use crate::provider::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};
    use std::path::PathBuf;

    fn leader_like_candidate(session_id: &str, path: &str) -> CapturedSessionCandidate {
        CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(session_id)),
                rollout_path: Some(RolloutPath::new(PathBuf::from(path))),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: PathBuf::from("/tmp/u1-cwd"),
            },
            positive_agent_id_match: false,
            agent_path_match: false,
        }
    }

    fn worker_owned_candidate(session_id: &str, path: &str) -> CapturedSessionCandidate {
        CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(session_id)),
                rollout_path: Some(RolloutPath::new(PathBuf::from(path))),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: PathBuf::from("/tmp/u1-cwd"),
            },
            positive_agent_id_match: true,
            agent_path_match: false,
        }
    }

    /// P0 RED-1 (lane-046-capture-gap): a Claude worker with
    /// expected_session_id=None must NOT capture a candidate that has neither
    /// positive_agent_id_match nor agent_path_match — even when it is the
    /// only candidate (the leader-transcript-as-only-candidate failure mode
    /// from the macmini repro). Must end up ambiguous, not assigned.
    #[test]
    fn claude_no_expected_single_leader_candidate_is_not_assigned() {
        let mut state = serde_json::json!({
            "agents": {
                "release_engineer": {
                    "provider": "claude",
                    "status": "running",
                    "spawn_cwd": "/tmp/u1-cwd"
                }
            }
        });
        let mut canned = BTreeMap::new();
        canned.insert(
            "release_engineer".to_string(),
            vec![leader_like_candidate(
                "ea059b82-c53e-4654-9590-9f3e6d46f0ca",
                "/Users/alauda/.claude/projects/-Users-alauda-team/ea059b82.jsonl",
            )],
        );
        let canned_for_adapter = canned.clone();
        let mut adapter_for = move |provider| {
            Box::new(
                test_support::CaptureCandidatesAdapter::new(provider, None, "")
                    .with_candidates(canned_for_adapter.clone()),
            ) as Box<dyn ProviderAdapter>
        };
        let report = capture_missing_provider_sessions_once(&mut state, &mut adapter_for, true, 0)
            .expect("capture pass succeeds");
        assert!(
            report.assigned.is_empty(),
            "Claude no-expected + weak (no positive/path) candidate must NOT be assigned; got {:?}",
            report.assigned
        );
        let agent = state
            .get("agents")
            .and_then(|a| a.get("release_engineer"))
            .expect("agent present");
        assert!(
            agent.get("session_id").is_none(),
            "agent.session_id must remain empty when capture is refused; got {agent:?}"
        );
    }

    /// P0 RED-2: same shape but with a positive_agent_id_match candidate →
    /// must capture (positive agent id is authoritative for Claude
    /// no-expected).
    #[test]
    fn claude_no_expected_positive_worker_candidate_still_captures() {
        let mut state = serde_json::json!({
            "agents": {
                "release_engineer": {
                    "provider": "claude",
                    "status": "running",
                    "spawn_cwd": "/tmp/u1-cwd"
                }
            }
        });
        let mut canned = BTreeMap::new();
        canned.insert(
            "release_engineer".to_string(),
            vec![worker_owned_candidate(
                "abc12345-worker-owned",
                "/Users/alauda/.claude/projects/-Users-alauda-team/abc12345.jsonl",
            )],
        );
        let canned_for_adapter = canned.clone();
        let mut adapter_for = move |provider| {
            Box::new(
                test_support::CaptureCandidatesAdapter::new(provider, None, "")
                    .with_candidates(canned_for_adapter.clone()),
            ) as Box<dyn ProviderAdapter>
        };
        let report = capture_missing_provider_sessions_once(&mut state, &mut adapter_for, true, 0)
            .expect("capture pass succeeds");
        assert_eq!(
            report.assigned,
            vec!["release_engineer".to_string()],
            "Claude no-expected + positive_agent_id candidate MUST capture; got {:?}",
            report.assigned
        );
    }

    /// P0 RED-3: leader_receiver under teams.<key> with provider session
    /// fields must exclude that session from worker capture even if the
    /// candidate would otherwise match.
    #[test]
    fn claimed_session_keys_include_team_scoped_leader_receiver() {
        let mut state = serde_json::json!({
            "agents": {
                "release_engineer": {
                    "provider": "claude",
                    "status": "running",
                    "spawn_cwd": "/tmp/u1-cwd"
                }
            },
            "teams": {
                "teamA": {
                    "leader_receiver": {
                        "session_id": "leader-session-id-zzz",
                        "rollout_path": "/Users/alauda/.claude/projects/-Users-alauda-team/zzz.jsonl"
                    }
                }
            }
        });
        let mut canned = BTreeMap::new();
        canned.insert(
            "release_engineer".to_string(),
            vec![worker_owned_candidate(
                "leader-session-id-zzz",
                "/Users/alauda/.claude/projects/-Users-alauda-team/zzz.jsonl",
            )],
        );
        let canned_for_adapter = canned.clone();
        let mut adapter_for = move |provider| {
            Box::new(
                test_support::CaptureCandidatesAdapter::new(provider, None, "")
                    .with_candidates(canned_for_adapter.clone()),
            ) as Box<dyn ProviderAdapter>
        };
        let report = capture_missing_provider_sessions_once(&mut state, &mut adapter_for, true, 0)
            .expect("capture pass succeeds");
        assert!(
            report.assigned.is_empty(),
            "worker candidate that collides with team-scoped leader_receiver \
             session MUST be excluded by claimed_provider_session_keys; got {:?}",
            report.assigned
        );
    }

    #[test]
    fn capture_pass_keeps_pending_agent_when_one_adapter_capture_fails() {
        let mut state = serde_json::json!({
            "agents": {
                "bad": {
                    "provider": "codex",
                    "status": "running",
                    "spawn_cwd": "/tmp/u1-bad"
                },
                "good": {
                    "provider": "codex",
                    "status": "running",
                    "spawn_cwd": "/tmp/u1-good"
                }
            }
        });
        let mut adapter_for = |provider| {
            Box::new(test_support::CaptureCandidatesAdapter::new(
                provider,
                Some("bad"),
                "capture exploded",
            )) as Box<dyn ProviderAdapter>
        };

        let report = capture_missing_provider_sessions_once(&mut state, &mut adapter_for, true, 0)
            .expect("one agent capture failure must not abort the whole pass");

        assert_eq!(report.pending, vec!["bad".to_string(), "good".to_string()]);
        assert_eq!(report.assigned, Vec::<String>::new());
        assert_eq!(
            report.candidate_count_by_agent.get("good"),
            Some(&0),
            "the non-failing agent must still be probed"
        );
        assert_eq!(
            report.capture_failures,
            vec![SessionCaptureFailure {
                agent_id: "bad".to_string(),
                error: "provider io error: capture exploded".to_string(),
            }]
        );
    }

    /// RM-039-SESS-001 step 1 (architect verdict 2026-06-22): the
    /// `agent_session_complete` predicate must NOT treat a non-empty
    /// `rollout_path` as complete when the file does not exist. The
    /// historical bug was that a stale-positive capture tuple
    /// (`session_id` + `rollout_path` both present, but the path had
    /// been rotated away by the provider) was reported as complete and
    /// blocked `pending_session_capture` from recapturing. Now the
    /// predicate returns false if the path does not exist, so the
    /// session is re-evaluated on the next convergence tick.
    #[test]
    fn rm039_sess001_agent_session_complete_requires_existing_rollout_path() {
        // Case 1: session_id + rollout_path both non-empty, path absent.
        let missing = "/tmp/ta-rm039-sess001-nonexistent-rollout.jsonl";
        // Guard against a previous test run leaving the file behind.
        let _ = std::fs::remove_file(missing);
        let agent_stale = serde_json::json!({
            "session_id": "sess-stale-positive",
            "rollout_path": missing,
        });
        assert!(
            !agent_session_complete(&agent_stale),
            "RM-039-SESS-001: stale-positive tuple (session_id + missing rollout_path) \
             must NOT be reported complete"
        );

        // Case 2: session_id + rollout_path both non-empty, path exists.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let existing = std::env::temp_dir().join(format!(
            "ta-rm039-sess001-existing-{}-{}.jsonl",
            std::process::id(),
            n
        ));
        std::fs::write(&existing, b"{}\n").expect("write fixture rollout file");
        // 0.4.6 tuple-atomic contract: `agent_session_complete` now requires
        // the full authoritative tuple (session_id + rollout_path +
        // captured_at + captured_via), not just session_id + rollout_path.
        // Pre-0.4.6 partial tuples are no longer treated as complete.
        let agent_complete = serde_json::json!({
            "session_id": "sess-real",
            "rollout_path": existing.to_string_lossy(),
            "captured_at": "2026-06-25T10:00:00+00:00",
            "captured_via": "session.captured",
        });
        assert!(
            agent_session_complete(&agent_complete),
            "0.4.6: agent with full tuple (session_id + rollout_path + captured_at + captured_via) + existing rollout file is complete"
        );
        let _ = std::fs::remove_file(&existing);

        // Case 3: empty session_id must still fail completeness regardless.
        let agent_no_session = serde_json::json!({
            "session_id": "",
            "rollout_path": "/tmp/whatever",
        });
        assert!(!agent_session_complete(&agent_no_session));

        // Case 4: empty rollout_path remains incomplete (legacy contract).
        let agent_no_path = serde_json::json!({
            "session_id": "sess-x",
            "rollout_path": "",
        });
        assert!(!agent_session_complete(&agent_no_path));
    }

    /// Stage 1 amendment regression (architect direction 2026-06-23,
    /// S1-CAPTURE-002): two pending agents, each with a distinct
    /// `_pending_session_id`. Each agent's candidate list contains ONLY its
    /// own expected transcript (no positive worker identity hint, no path
    /// agent id hint — the file basename is just a UUID). Pre-fix, the
    /// allocator's `allocate_global_one_to_one` zipped agents (sorted by
    /// agent_id) with candidates (sorted by candidate-key), producing CROSS
    /// assignments that the mismatch guard then rejected — leaving both
    /// agents `attribution_ambiguous`. Post-fix, the ExpectedSessionId
    /// pre-pass binds each agent to its own expected candidate before any
    /// global one-to-one runs.
    #[test]
    fn capture_allocator_expected_session_id_binds_each_worker_to_its_own_transcript() {
        // 0.4.6 P0 amendment: Claude no longer carries _pending_session_id
        // (fresh spawn doesn't inject --session-id; capture anchors on
        // cwd+spawned_at+identity). The ExpectedSessionId pre-pass remains
        // valid for providers that DO use expected_session_id (Copilot is the
        // only one left that honors framework-supplied --session-id). Switch
        // this test to Copilot to preserve the pre-pass invariant coverage.
        use crate::provider::{CaptureVia, Confidence, RolloutPath};
        let dir = std::env::temp_dir().join(format!(
            "ta-stage1-allocator-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let rollout_a = dir.join("9a2d1668.jsonl");
        let rollout_b = dir.join("3e824e89.jsonl");
        std::fs::write(&rollout_a, b"{}\n").unwrap();
        std::fs::write(&rollout_b, b"{}\n").unwrap();
        let candidate_a = CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(
                    "9a2d1668-8987-4c36-8bde-a5135b10da02",
                )),
                rollout_path: Some(RolloutPath::new(rollout_a.clone())),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: dir.clone(),
            },
            positive_agent_id_match: false,
            agent_path_match: false,
        };
        let candidate_b = CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(
                    "3e824e89-25ac-4b3f-b272-b4f733f6403c",
                )),
                rollout_path: Some(RolloutPath::new(rollout_b.clone())),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: dir.clone(),
            },
            positive_agent_id_match: false,
            agent_path_match: false,
        };
        let mut candidates_by_agent: BTreeMap<String, Vec<CapturedSessionCandidate>> =
            BTreeMap::new();
        candidates_by_agent.insert("copilot-a".to_string(), vec![candidate_a.clone()]);
        candidates_by_agent.insert("copilot-b".to_string(), vec![candidate_b.clone()]);

        let cwd_str = dir.to_string_lossy().to_string();
        let mut state = serde_json::json!({
            "agents": {
                "copilot-a": {
                    "provider": "copilot",
                    "status": "running",
                    "spawn_cwd": cwd_str,
                    "_pending_session_id": "9a2d1668-8987-4c36-8bde-a5135b10da02"
                },
                "copilot-b": {
                    "provider": "copilot",
                    "status": "running",
                    "spawn_cwd": cwd_str,
                    "_pending_session_id": "3e824e89-25ac-4b3f-b272-b4f733f6403c"
                }
            }
        });
        let adapter = test_support::CaptureCandidatesAdapter::new(Provider::Copilot, None, "")
            .with_candidates(candidates_by_agent);
        let mut adapter_for = move |_provider| {
            Box::new(adapter.clone()) as Box<dyn ProviderAdapter>
        };

        let report = capture_missing_provider_sessions_once(&mut state, &mut adapter_for, true, 0)
            .expect("allocator pass should succeed");

        // ExpectedSessionId pre-pass must bind each agent to its own
        // expected transcript. No cross-assignment, no ambiguous mark.
        let agents = state["agents"].as_object().expect("agents object");
        assert_eq!(
            agents["copilot-a"]["session_id"].as_str(),
            Some("9a2d1668-8987-4c36-8bde-a5135b10da02"),
            "Stage 1 fix: copilot-a must bind to its own expected transcript; \
             state.copilot-a={}",
            agents["copilot-a"]
        );
        assert_eq!(
            agents["copilot-b"]["session_id"].as_str(),
            Some("3e824e89-25ac-4b3f-b272-b4f733f6403c"),
            "Stage 1 fix: copilot-b must bind to its own expected transcript; \
             state.copilot-b={}",
            agents["copilot-b"]
        );
        assert!(
            agents["copilot-a"]
                .get("attribution_ambiguous")
                .is_none_or(|v| v.as_bool() != Some(true)),
            "Stage 1 fix: copilot-a must NOT be flagged ambiguous after the \
             expected-id pre-pass bound it; state.copilot-a={}",
            agents["copilot-a"]
        );
        assert!(
            agents["copilot-b"]
                .get("attribution_ambiguous")
                .is_none_or(|v| v.as_bool() != Some(true)),
            "Stage 1 fix: copilot-b must NOT be flagged ambiguous; \
             state.copilot-b={}",
            agents["copilot-b"]
        );
        assert_eq!(
            report.ambiguous.len(),
            0,
            "Stage 1 fix: capture report must record zero ambiguous; report={report:?}"
        );

        let _ = std::fs::remove_file(&rollout_a);
        let _ = std::fs::remove_file(&rollout_b);
        let _ = std::fs::remove_dir(&dir);
    }

    /// E57 RED postflight (lane-046-capture-gap): `recover_resume_session_from_events`
    /// must refuse to repair a worker's session from a stale `session.captured`
    /// event whose rollout file is a Claude LEADER transcript
    /// (`customTitle == "claude leader"`). Without this filter, a fresh restart
    /// after a pre-fix run still pulls the leader transcript onto a worker via
    /// captured_via=event_log_repair (Mac mini evidence:
    /// resume.session_repaired session_id=ea059b82 → release-engineer).
    #[test]
    fn e57_recover_resume_refuses_claude_leader_marker_rollout() {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let workspace = std::env::temp_dir().join(format!("ta-e57-recover-{}-{}", std::process::id(), uniq));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");

        // Write a leader-marker rollout file (claude TUI leader transcript).
        let rollout = workspace.join("ea059b82-leader.jsonl");
        let mut f = std::fs::File::create(&rollout).expect("rollout");
        writeln!(
            f,
            "{{\"customTitle\":\"claude leader\",\"sessionId\":\"ea059b82-c53e-4654-9590-9f3e6d46f0ca\",\"cwd\":\"/tmp/e57\"}}"
        )
        .unwrap();
        writeln!(f, "{{\"role\":\"user\",\"content\":\"hi\"}}").unwrap();
        drop(f);

        // Write a `session.captured` event into events.jsonl pointing
        // release-engineer at the leader rollout — simulates the stale
        // pre-fix event that triggered the bug.
        let event_log = crate::event_log::EventLog::new(&workspace);
        event_log
            .write(
                "session.captured",
                serde_json::json!({
                    "agent_id": "release-engineer",
                    "provider": "claude",
                    "session_id": "ea059b82-c53e-4654-9590-9f3e6d46f0ca",
                    "rollout_path": rollout.to_string_lossy().to_string(),
                    "captured_via": "fs_watch",
                }),
            )
            .expect("event write");

        let previous = serde_json::json!({
            "provider": "claude",
            "session_id": null,
            "rollout_path": null,
        });
        let adapter = test_support::CaptureCandidatesAdapter::new(Provider::Claude, None, "");
        let exclude = BTreeSet::new();
        let result = recover_resume_session_from_events(
            &workspace,
            "release-engineer",
            &previous,
            &adapter,
            crate::provider::AuthMode::Subscription,
            &exclude,
        )
        .expect("recover call ok");

        assert!(
            result.is_none(),
            "E57 postflight: recover_resume_session_from_events must REFUSE \
             to repair from a Claude leader-marker rollout (event_log_repair \
             cannot resurrect a stale leader-to-worker assignment); got {result:?}"
        );

        let _ = std::fs::remove_file(&rollout);
        let _ = std::fs::remove_file(workspace.join(".team/logs/events.jsonl"));
        let _ = std::fs::remove_dir_all(workspace.join(".team"));
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
