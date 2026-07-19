//! unit-11 (Stage 4) — coordinator tick `abnormal` step group.
//!
//! Abnormal-exit detection + classification step extracted from
//! `coordinator/tick.rs` without behavior changes.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::event_log::EventLog;
use crate::provider::wire::{parse_provider, provider_wire};
use crate::provider::ProcessLiveness;

use super::super::health::pid_is_running;
use super::super::tick::TickError;
use super::super::types::Pid;

/// #236 `worker.abnormal_exit` watcher.
///
/// Notify only when the bounded transcript/rollout tail contains a latest explicit
/// provider error that is fresh for the current worker cohort. A dead process
/// without an explicit error remains a suppressed `dead_only` audit event; process
/// liveness is otherwise diagnostic data for this path.
pub(crate) fn detect_abnormal_exits(
    workspace: &Path,
    transport: &dyn crate::transport::Transport,
    state: &mut Value,
    event_log: &EventLog,
    targets: &[crate::transport::PaneInfo],
) -> Result<(), TickError> {
    let snapshot = state.clone();
    let team = crate::state::projection::team_state_key(&snapshot);
    let session_name = snapshot.get("session_name").and_then(Value::as_str);
    for agent in abnormal_watch_agents(&snapshot) {
        // Pane/process liveness is independent of transcript content. A
        // frozen rollout is common after pane death, so probe before the
        // metadata dedupe gate; only the expensive tail scan stays deduped.
        let liveness = agent_process_liveness(&agent, session_name, targets, transport);
        let rollout_path = resolve_agent_rollout_path(workspace, &agent.rollout_path);
        let metadata = match std::fs::metadata(&rollout_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                upsert_abnormal_watch(
                    state,
                    &agent.agent_id,
                    abnormal_watch_payload(
                        &agent,
                        None,
                        None,
                        process_check(
                            ProcessLiveness::Unverifiable,
                            "rollout_metadata_unavailable".to_string(),
                        ),
                        None,
                        ErrorRecency::None,
                        None,
                        Some(error.to_string()),
                    ),
                );
                continue;
            }
        };
        let size = metadata.len();
        let mtime_ns = metadata_mtime_ns(&metadata);
        // P1 (C-P1-2/3): (size, mtime_ns) pair gate — an unchanged transcript is not
        // read at all (live sample: 332MB whole-file read per agent per 2s tick).
        // ANY field change (including a size shrink / truncate) falls through to the
        // re-read below.
        if let (Some(mtime), Some(stored)) = (
            mtime_ns,
            abnormal_watch_stored_metadata(&snapshot, &agent.agent_id),
        ) {
            if stored == (size, mtime) {
                refresh_abnormal_watch_liveness(state, &agent.agent_id, &liveness);
                continue;
            }
        }
        // P1 (C-P1-1): bounded tail read — the abnormal decision only consumes
        // this tail window. The provider scan walks backward inside it; window
        // matches Python `_TAIL_BYTES` (131072, idle_takeover_wiring.py:13),
        // never less.
        let text = match read_tail_text(&rollout_path, ABNORMAL_TAIL_BYTES) {
            Ok(text) => text,
            Err(error) => {
                upsert_abnormal_watch(
                    state,
                    &agent.agent_id,
                    abnormal_watch_payload(
                        &agent,
                        Some(size),
                        mtime_ns,
                        process_check(
                            ProcessLiveness::Unverifiable,
                            "rollout_read_failed".to_string(),
                        ),
                        None,
                        ErrorRecency::None,
                        None,
                        Some(error.to_string()),
                    ),
                );
                continue;
            }
        };
        let fact = crate::provider::latest_explicit_error_fact(agent.provider, &text);
        let error_observation_key = fact
            .as_ref()
            .map(|fact| abnormal_error_observation_key(&agent, fact));
        let error_observation_cohort = fact.as_ref().map(|_| abnormal_error_cohort_key(&agent));
        let error_recency = abnormal_error_recency(
            &snapshot,
            &agent,
            error_observation_key.as_deref(),
            error_observation_cohort.as_deref(),
        );
        let decision = abnormal_exit_decision(liveness.state, fact.as_ref(), error_recency);
        let check_key = abnormal_check_key(
            &agent,
            &liveness,
            fact.as_ref(),
            error_recency,
            error_observation_key.as_deref(),
        );
        upsert_abnormal_watch(
            state,
            &agent.agent_id,
            abnormal_watch_payload(
                &agent,
                Some(size),
                mtime_ns,
                liveness.clone(),
                fact.as_ref().map(|f| f.signature.as_str()),
                error_recency,
                error_observation_key.as_deref(),
                None,
            ),
        );
        if let (Some(observation_key), Some(cohort_key)) = (
            error_observation_key.as_deref(),
            error_observation_cohort.as_deref(),
        ) {
            mark_abnormal_error_observed(state, &agent.agent_id, observation_key, cohort_key);
        }
        if abnormal_last_check_key(state, &agent.agent_id).as_deref() != Some(check_key.as_str()) {
            write_abnormal_check(
                event_log,
                &team,
                &agent,
                &liveness,
                fact.as_ref(),
                decision,
                error_recency,
                size,
                mtime_ns,
            )?;
            mark_abnormal_checked(state, &agent.agent_id, &check_key);
        }
        let fact = match (decision, fact) {
            (AbnormalExitDecision::Notify, Some(fact)) => fact,
            (AbnormalExitDecision::Suppress(reason), _) => {
                let suppress_key = abnormal_suppression_key(&agent, &liveness, reason);
                if abnormal_last_suppressed_key(state, &agent.agent_id).as_deref()
                    != Some(suppress_key.as_str())
                {
                    write_abnormal_suppressed(event_log, &team, &agent, &liveness, reason)?;
                    mark_abnormal_suppressed(state, &agent.agent_id, &suppress_key);
                }
                continue;
            }
            (AbnormalExitDecision::NoSignal, _) => continue,
            (AbnormalExitDecision::Notify, None) => continue,
        };
        let dedupe_key = abnormal_dedupe_key(&agent, &fact);
        if abnormal_last_notified_key(state, &agent.agent_id).as_deref()
            == Some(dedupe_key.as_str())
        {
            continue;
        }
        // 0.5.36 (`.team/artifacts/supermarket-api-error-recovery-locate.md`
        // §7.1/§7.6/§7.5): classify, update backpressure, record recovery
        // intent (retryable only), then compose the policy-aware tail. This
        // is INTENT WRITE only — actual lifecycle work happens post-save
        // via `attempt_due_recoveries` (§7.3, R6 guard).
        let manual_command =
            recovery_manual_command(agent.agent_id.as_str(), workspace, team.as_str());
        let error_observation_key_string = error_observation_key.clone().unwrap_or_default();
        let (recovery_class, recovery_schedule, recovery_cohort_key, recovery_backpressure_until) =
            process_api_error_recovery_intent(
                state,
                event_log,
                &team,
                &agent,
                &fact,
                &manual_command,
                error_observation_key_string.as_str(),
            )?;
        let recovery_tail = recovery_notification_tail(
            recovery_class,
            recovery_schedule.as_ref(),
            recovery_backpressure_until,
            &recovery_cohort_key,
            &fact,
            &manual_command,
        );
        let content =
            format_abnormal_exit_message(&team, &agent, &fact, &liveness, size, &recovery_tail);
        let outcome = crate::messaging::send_to_leader_receiver(
            workspace,
            state,
            "leader",
            &content,
            None,
            &agent.agent_id,
            false,
            Some(&dedupe_key),
            event_log,
        )?;
        let notification_status = if outcome.ok {
            "queued"
        } else if matches!(outcome.status, crate::messaging::DeliveryStatus::Blocked) {
            "rebind_required"
        } else {
            "refused"
        };
        let provider_process_dead = provider_process_dead_fact(&liveness);
        event_log.write(
            "worker.abnormal_exit",
            serde_json::json!({
                "team_id": team.as_str(),
                "agent_id": agent.agent_id.as_str(),
                "provider": provider_wire(agent.provider),
                "path": agent.rollout_path_display.as_str(),
                "dead_process": liveness.state == ProcessLiveness::Dead,
                "process_dead": liveness.state == ProcessLiveness::Dead,
                "provider_process_dead": provider_process_dead,
                "latest_error": true,
                "latest_explicit_error": true,
                "error_recency": error_recency.as_str(),
                "fresh_error": error_recency.is_fresh(),
                "dead_process_and_latest_error": liveness.state == ProcessLiveness::Dead,
                "dead_process_and_latest_explicit_error": liveness.state == ProcessLiveness::Dead,
                "process_dead_and_latest_explicit_error": liveness.state == ProcessLiveness::Dead,
                "provider_process_dead_and_latest_explicit_error": provider_process_dead,
                "signature": fact.signature.as_str(),
                "turn_id": fact.turn_id.as_ref().map(|id| id.as_str()),
                "apiErrorStatus": fact.api_error_status,
                "error": fact.error.as_deref(),
                "requestId": fact.request_id.as_deref(),
                "assistant_uuid": fact.assistant_uuid.as_deref(),
                "size": size,
                "mtime_ns": mtime_ns,
                "process_liveness": process_liveness_wire(liveness.state),
                "pid_status": liveness.detail.as_str(),
                "notification_message_id": outcome.message_id,
                "notification_status": notification_status,
                "notification_channel": outcome.channel,
            }),
        )?;
        mark_abnormal_notified(state, &agent.agent_id, &dedupe_key);
    }
    Ok(())
}

fn provider_process_dead_fact(liveness: &ProcessCheck) -> bool {
    liveness.state == ProcessLiveness::Dead
        && !liveness.detail.starts_with("pane_dead:")
        && !liveness.detail.starts_with("window_missing:")
}

fn worker_provider_exited_fact(liveness: &ProcessCheck) -> bool {
    provider_process_dead_fact(liveness) && liveness.detail.starts_with("worker_provider_exited:")
}

fn refresh_abnormal_watch_liveness(state: &mut Value, agent_id: &str, liveness: &ProcessCheck) {
    let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch")
        .and_then(|watch| watch.get_mut(agent_id))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let dead_process = liveness.state == ProcessLiveness::Dead;
    let provider_process_dead = provider_process_dead_fact(liveness);
    let latest_explicit_error = watch
        .get("latest_explicit_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Persist fact transitions, not the tick's observation time; otherwise an
    // unchanged transcript makes every steady tick rewrite state.json.
    let patch = serde_json::json!({
        "last_liveness": process_liveness_wire(liveness.state),
        "last_liveness_detail": liveness.detail.as_str(),
        "dead_process": dead_process,
        "process_dead": dead_process,
        "provider_process_dead": provider_process_dead,
        "worker_provider_exited": worker_provider_exited_fact(liveness),
        "provider_process_dead_and_latest_explicit_error": provider_process_dead && latest_explicit_error,
    });
    if let Some(patch) = patch.as_object() {
        if patch
            .iter()
            .any(|(key, value)| watch.get(key) != Some(value))
        {
            watch.extend(patch.clone());
        }
    }
}

#[derive(Debug, Clone)]
struct AbnormalWatchAgent {
    agent_id: String,
    provider: crate::model::enums::Provider,
    rollout_path: PathBuf,
    rollout_path_display: String,
    spawn_epoch: Option<u64>,
    spawned_at: Option<String>,
    status: Option<String>,
    process_liveness: Option<ProcessLiveness>,
    window: Option<String>,
    pane_id: Option<String>,
    pid: Option<Pid>,
    current_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessCheck {
    state: ProcessLiveness,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbnormalExitDecision {
    Notify,
    Suppress(&'static str),
    NoSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorRecency {
    None,
    Stale,
    Fresh,
}

impl ErrorRecency {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Stale => "stale",
            Self::Fresh => "fresh",
        }
    }

    fn is_fresh(self) -> bool {
        matches!(self, Self::Fresh)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AbnormalExitGate {
    provider_process_dead: bool,
    latest_explicit_error: bool,
    error_recency: ErrorRecency,
}

impl AbnormalExitGate {
    fn new(
        process_liveness: ProcessLiveness,
        latest_explicit_error: bool,
        error_recency: ErrorRecency,
    ) -> Self {
        Self {
            provider_process_dead: process_liveness == ProcessLiveness::Dead,
            latest_explicit_error,
            error_recency,
        }
    }

    fn should_notify_worker_abnormal_exit(self) -> bool {
        should_notify_worker_abnormal_exit(self.latest_explicit_error, self.error_recency)
    }

    fn suppressed_reason(self) -> Option<&'static str> {
        match (self.provider_process_dead, self.latest_explicit_error) {
            (true, false) => Some("dead_only"),
            _ => None,
        }
    }
}

fn abnormal_exit_decision(
    process_liveness: ProcessLiveness,
    latest_explicit_error: Option<&crate::provider::FaultFact>,
    error_recency: ErrorRecency,
) -> AbnormalExitDecision {
    let gate = AbnormalExitGate::new(
        process_liveness,
        latest_explicit_error.is_some(),
        error_recency,
    );
    if gate.should_notify_worker_abnormal_exit() {
        return AbnormalExitDecision::Notify;
    }
    match gate.suppressed_reason() {
        Some(reason) => AbnormalExitDecision::Suppress(reason),
        None => AbnormalExitDecision::NoSignal,
    }
}

fn should_notify_worker_abnormal_exit(
    latest_explicit_error: bool,
    error_recency: ErrorRecency,
) -> bool {
    latest_explicit_error && error_recency.is_fresh()
}

fn resolve_agent_rollout_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn abnormal_watch_agents(state: &Value) -> Vec<AbnormalWatchAgent> {
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return Vec::new();
    };
    agents
        .iter()
        .filter_map(|(agent_id, agent)| {
            if matches!(agent.get("status").and_then(Value::as_str), Some("paused")) {
                return None;
            }
            let provider = agent
                .get("provider")
                .and_then(Value::as_str)
                .and_then(parse_provider)?;
            let rollout_path_display = ["rollout_path", "transcript_path", "session_log_path"]
                .into_iter()
                .find_map(|key| agent.get(key).and_then(Value::as_str))
                .filter(|path| !path.is_empty())?
                .to_string();
            Some(AbnormalWatchAgent {
                agent_id: agent_id.clone(),
                provider,
                rollout_path: PathBuf::from(&rollout_path_display),
                rollout_path_display,
                spawn_epoch: agent.get("spawn_epoch").and_then(Value::as_u64),
                spawned_at: agent
                    .get("spawned_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                status: agent
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                process_liveness: explicit_process_liveness(agent),
                window: agent
                    .get("window")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pane_id: agent
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pid: agent_pid(agent),
                current_command: agent
                    .get("pane_current_command")
                    .or_else(|| agent.get("current_command"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn agent_pid(agent: &Value) -> Option<Pid> {
    // 0.5.41 Slice 4 (fault-invisibility-locate.md §5 point 3 / §6.4,
    // 0.5.39 CR D-m): after the 0.5.39 worker shell wrapper landed,
    // `pane_pid` is the long-lived shell — NOT the provider. Treating
    // it as `provider pid` and probing it via `kill(0)` reports "alive"
    // for a pane whose provider CLI already exited, which is exactly
    // the false-BUSY that suppressed `worker_provider_exited`. Only
    // pids we know are the provider itself (`provider_pid` /
    // `process_id` / `pid` written by explicit spawn accounting, or
    // `child_pid` from the transport's SpawnResult) count here.
    ["provider_pid", "process_id", "pid", "child_pid"]
        .into_iter()
        .find_map(|key| json_u32(agent.get(key)).map(Pid::new))
}

pub(crate) fn explicit_process_liveness(agent: &Value) -> Option<ProcessLiveness> {
    if let Some(process) = agent
        .get("provider_process")
        .or_else(|| agent.get("process"))
    {
        if let Some(liveness) = explicit_process_liveness(process) {
            return Some(liveness);
        }
    }
    for key in [
        "provider_process_liveness",
        "process_liveness",
        "pane_liveness",
    ] {
        match agent.get(key).and_then(Value::as_str) {
            Some("dead") => return Some(ProcessLiveness::Dead),
            Some("alive" | "live") => return Some(ProcessLiveness::Alive),
            Some("unverifiable" | "unknown") => return Some(ProcessLiveness::Unverifiable),
            _ => {}
        }
    }
    for key in [
        "provider_process_alive",
        "process_alive",
        "provider_alive",
        "alive",
    ] {
        if let Some(alive) = agent.get(key).and_then(Value::as_bool) {
            return Some(if alive {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            });
        }
    }
    for key in [
        "provider_process_dead",
        "process_dead",
        "provider_dead",
        "dead",
    ] {
        if let Some(dead) = agent.get(key).and_then(Value::as_bool) {
            return Some(if dead {
                ProcessLiveness::Dead
            } else {
                ProcessLiveness::Alive
            });
        }
    }
    for key in ["status", "state", "liveness"] {
        match agent.get(key).and_then(Value::as_str) {
            Some("dead" | "exited" | "terminated" | "crashed" | "missing") => {
                return Some(ProcessLiveness::Dead);
            }
            Some("alive" | "live" | "running") => return Some(ProcessLiveness::Alive),
            Some("unverifiable" | "unknown") => return Some(ProcessLiveness::Unverifiable),
            _ => {}
        }
    }
    None
}

fn json_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        })
        .and_then(|n| u32::try_from(n).ok())
}

fn agent_process_liveness(
    agent: &AbnormalWatchAgent,
    session_name: Option<&str>,
    targets: &[crate::transport::PaneInfo],
    transport: &dyn crate::transport::Transport,
) -> ProcessCheck {
    // 0.5.41 Slice 4 (fault-invisibility-locate.md §5 point 3 / §6.4,
    // 0.5.39 CR D-m): after the worker wrapper landed, `pane_pid` may
    // be the long-lived shell that survived provider exit. Reordered
    // to prefer POSITIVE provider evidence in this order:
    //   1. Explicit provider pid (agent.pid) — set by the transport's
    //      SpawnResult.child_pid or explicit provider_pid state field.
    //   2. Explicit liveness field.
    //   3. Explicit dead-status field.
    //   4. Explicit provider current-command (state.pane_current_command).
    //   5. Live-pane current-command matching provider = alive.
    //   6. Live-pane current-command shell + capture-tail worker exit
    //      marker = DEAD (positive proof provider exited).
    //   7. Live pane, no positive evidence = Unverifiable (NOT alive,
    //      NOT working). Legacy code returned pid_process_check(pane_pid)
    //      here — that was the D-m false BUSY.
    //   8. Pane / window liveness falls back on Unverifiable.
    if let Some(pid) = agent.pid {
        return pid_process_check("pid", pid);
    }
    // 0.5.41 Slice 4 (fault-invisibility-locate.md §5 point 3 / §6.4):
    // BEFORE consulting the loose `explicit_process_liveness` path
    // (which happily maps state.status=`running` to Alive), probe the
    // pane for POSITIVE provider evidence via current-command + worker
    // exit marker. Under the 0.5.39 wrapper, `state.status=running` is
    // administrative spawn accounting, not runtime truth — a provider
    // may have exited into the shell fallback while state still reads
    // running. The marker probe positively proves that case; when it
    // fires we return Dead here instead of the false Alive further
    // down.
    if let Some(target) = matching_agent_target(agent, session_name, targets) {
        if let Some(command) = target.current_command.as_deref() {
            return pane_command_process_check_with_marker(agent, transport, target, command);
        }
    }
    if let Some(command) = agent.current_command.as_deref() {
        return command_process_check_with_marker(agent, transport, command);
    }
    if agent.pane_id.is_some() {
        // Even without pane current_command / matching target, try the
        // marker probe directly — the wrapper's printf leaves the marker
        // in the pane's capture tail whether or not the transport
        // reports pane_current_command.
        if let Some(check) = worker_provider_exit_marker_check(agent, transport) {
            return check;
        }
    }
    if let Some(liveness) = agent.process_liveness {
        return process_check(
            liveness,
            format!("explicit:{}", process_liveness_wire(liveness)),
        );
    }
    if agent.status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "stopped" | "missing" | "error" | "dead" | "exited" | "terminated" | "crashed"
        )
    }) {
        return process_check(
            ProcessLiveness::Dead,
            format!("status:{}", agent.status.as_deref().unwrap_or("unknown")),
        );
    }
    if let Some(target) = matching_agent_target(agent, session_name, targets) {
        // Reached only when the target has no current_command AND no
        // marker was found earlier. Legacy code returned
        // `pid_process_check("pane_pid", pane_pid)` here — that's the
        // wrapper shell under 0.5.39. Now Unverifiable (not-working).
        let _ = target;
        return process_check(
            ProcessLiveness::Unverifiable,
            "pane_present_provider_evidence_unknown".to_string(),
        );
    }
    if let Some(pane_id) = agent.pane_id.as_deref() {
        let pane = crate::transport::PaneId::new(pane_id);
        return match transport.liveness(&pane) {
            Ok(crate::transport::PaneLiveness::Dead) => {
                process_check(ProcessLiveness::Dead, format!("pane_dead:{pane_id}"))
            }
            Ok(crate::transport::PaneLiveness::Live) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_live_provider_evidence_unknown:{pane_id}"),
            ),
            Ok(crate::transport::PaneLiveness::Unknown) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_unknown:{pane_id}"),
            ),
            Err(error) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_unverifiable:{pane_id}:{error}"),
            ),
        };
    }
    let (Some(session), Some(window)) = (session_name, agent.window.as_deref()) else {
        return process_check(
            ProcessLiveness::Unverifiable,
            "missing_session_or_window".to_string(),
        );
    };
    let session = crate::transport::SessionName::new(session);
    match transport.list_windows(&session) {
        Ok(windows) if windows.iter().any(|known| known.as_str() == window) => process_check(
            ProcessLiveness::Unverifiable,
            "window_present_provider_evidence_unknown".to_string(),
        ),
        Ok(_) => process_check(ProcessLiveness::Dead, format!("window_missing:{window}")),
        Err(error) => process_check(
            ProcessLiveness::Unverifiable,
            format!("window_unverifiable:{window}:{error}"),
        ),
    }
}

fn matching_agent_target<'a>(
    agent: &AbnormalWatchAgent,
    session_name: Option<&str>,
    targets: &'a [crate::transport::PaneInfo],
) -> Option<&'a crate::transport::PaneInfo> {
    if let Some(pane_id) = agent.pane_id.as_deref() {
        if let Some(target) = targets
            .iter()
            .find(|target| target.pane_id.as_str() == pane_id)
        {
            return Some(target);
        }
    }
    let (Some(session), Some(window)) = (session_name, agent.window.as_deref()) else {
        return None;
    };
    targets.iter().find(|target| {
        target.session.as_str() == session
            && target
                .window_name
                .as_ref()
                .is_some_and(|known| known.as_str() == window)
    })
}

fn pid_process_check(label: &str, pid: Pid) -> ProcessCheck {
    match pid_is_running(pid) {
        Ok(true) => process_check(ProcessLiveness::Alive, format!("{label}_running:{pid}")),
        Ok(false) => process_check(ProcessLiveness::Dead, format!("{label}_not_running:{pid}")),
        Err(error) => process_check(
            ProcessLiveness::Unverifiable,
            format!("{label}_unverifiable:{pid}:{error}"),
        ),
    }
}

fn command_process_check(provider: crate::model::enums::Provider, command: &str) -> ProcessCheck {
    if crate::leader::command_matches_provider(provider, command) {
        process_check(ProcessLiveness::Alive, format!("current_command:{command}"))
    } else {
        process_check(
            ProcessLiveness::Dead,
            format!("provider_not_foreground:{command}"),
        )
    }
}

fn pane_command_process_check(
    provider: crate::model::enums::Provider,
    pane: &crate::transport::PaneInfo,
    command: &str,
) -> ProcessCheck {
    if crate::leader::attribute_pane_provider(pane)
        .is_some_and(|candidate| crate::leader::provider_matches(candidate, provider))
    {
        process_check(ProcessLiveness::Alive, format!("current_command:{command}"))
    } else {
        process_check(
            ProcessLiveness::Dead,
            format!("provider_not_foreground:{command}"),
        )
    }
}

/// 0.5.41 Slice 4 (fault-invisibility-locate.md §5 point 3 / §6.4):
/// after the 0.5.39 worker wrapper landed, a pane whose current command
/// is a shell + capture-tail contains the worker exit marker is POSITIVE
/// proof the provider exited. Falls back to plain current-command check
/// when a pane_id is not available for capture (state-only path).
fn command_process_check_with_marker(
    agent: &AbnormalWatchAgent,
    transport: &dyn crate::transport::Transport,
    command: &str,
) -> ProcessCheck {
    if crate::leader::command_matches_provider(agent.provider, command) {
        return process_check(ProcessLiveness::Alive, format!("current_command:{command}"));
    }
    if let Some(check) = worker_provider_exit_marker_check(agent, transport) {
        return check;
    }
    process_check(
        ProcessLiveness::Unverifiable,
        format!("provider_evidence_unverifiable:{command}"),
    )
}

fn pane_command_process_check_with_marker(
    agent: &AbnormalWatchAgent,
    transport: &dyn crate::transport::Transport,
    pane: &crate::transport::PaneInfo,
    command: &str,
) -> ProcessCheck {
    if crate::leader::attribute_pane_provider(pane)
        .is_some_and(|candidate| crate::leader::provider_matches(candidate, agent.provider))
    {
        return process_check(ProcessLiveness::Alive, format!("current_command:{command}"));
    }
    if let Some(check) = worker_provider_exit_marker_check(agent, transport) {
        return check;
    }
    process_check(
        ProcessLiveness::Unverifiable,
        format!("provider_evidence_unverifiable:{command}"),
    )
}

/// 0.5.41 Slice 4: probe the pane's capture-tail for the single-source
/// worker exit marker (`tmux_backend::worker_provider_exit_marker`).
/// Returns `Some(Dead)` when the marker is found (positive proof
/// provider exited), or `None` when we can't run the capture (no
/// pane_id in agent), so callers keep their default decision. Absence
/// of marker in a valid capture is intentionally NOT `alive` (locate
/// §8 risk: capture tail can scroll past the marker) — callers still
/// return `Dead` for "provider not foreground" because current command
/// already disproved provider liveness at that call site.
fn worker_provider_exit_marker_check(
    agent: &AbnormalWatchAgent,
    transport: &dyn crate::transport::Transport,
) -> Option<ProcessCheck> {
    let pane_id_str = agent.pane_id.as_deref()?;
    let pane = crate::transport::PaneId::new(pane_id_str);
    let target = crate::transport::Target::Pane(pane);
    let provider_label = crate::provider::wire::command_name(agent.provider);
    let marker = crate::tmux_backend::worker_provider_exit_marker(provider_label);
    let cap = transport
        .capture(&target, crate::transport::CaptureRange::Tail(200))
        .ok()?;
    if cap.text.contains(&marker) {
        Some(process_check(
            ProcessLiveness::Dead,
            format!("worker_provider_exited:{pane_id_str}"),
        ))
    } else {
        None
    }
}

fn process_check(state: ProcessLiveness, detail: String) -> ProcessCheck {
    ProcessCheck { state, detail }
}

fn process_liveness_wire(state: ProcessLiveness) -> &'static str {
    match state {
        ProcessLiveness::Alive => "alive",
        ProcessLiveness::Dead => "dead",
        ProcessLiveness::Unverifiable => "unverifiable",
    }
}

pub(crate) fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> Option<u64> {
    let duration = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(
        duration
            .as_secs()
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(duration.subsec_nanos())),
    )
}

fn abnormal_watch_payload(
    agent: &AbnormalWatchAgent,
    size: Option<u64>,
    mtime_ns: Option<u64>,
    liveness: ProcessCheck,
    signature: Option<&str>,
    error_recency: ErrorRecency,
    error_observation_key: Option<&str>,
    error: Option<String>,
) -> Value {
    let liveness_wire = process_liveness_wire(liveness.state);
    let dead_process = liveness.state == ProcessLiveness::Dead;
    let latest_explicit_error = signature.is_some();
    let gate = AbnormalExitGate::new(liveness.state, latest_explicit_error, error_recency);
    let notify = gate.should_notify_worker_abnormal_exit();
    let suppressed_reason = gate.suppressed_reason();
    // 0.5.41 Slice 4 (fault-invisibility-locate.md §6.4): typed
    // classification for status/render — `worker_provider_exited` is
    // true only when Dead was proven via the capture-tail worker exit
    // marker (detail prefix `worker_provider_exited:`). status_port's
    // RuntimeFreshness collector reads this field to downgrade the
    // corresponding agent row.
    let provider_process_dead = provider_process_dead_fact(&liveness);
    let worker_provider_exited = worker_provider_exited_fact(&liveness);
    serde_json::json!({
        "path": agent.rollout_path_display.as_str(),
        "provider": provider_wire(agent.provider),
        "mtime_ns": mtime_ns,
        "size": size,
        "last_offset": size,
        "last_signature": signature,
        "last_liveness": liveness_wire,
        "last_liveness_detail": liveness.detail,
        "dead_process": dead_process,
        "process_dead": dead_process,
        "provider_process_dead": provider_process_dead,
        "worker_provider_exited": worker_provider_exited,
        "latest_error": latest_explicit_error,
        "latest_explicit_error": latest_explicit_error,
        "error_recency": error_recency.as_str(),
        "fresh_error": error_recency.is_fresh(),
        "error_observation_key": error_observation_key,
        "dead_process_and_latest_error": dead_process && latest_explicit_error,
        "dead_process_and_latest_explicit_error": dead_process && latest_explicit_error,
        "process_dead_and_latest_explicit_error": dead_process && latest_explicit_error,
        "provider_process_dead_and_latest_explicit_error": provider_process_dead && latest_explicit_error,
        "suppressed_reason": suppressed_reason,
        "notification": notify,
        "last_error": error,
    })
}

fn upsert_abnormal_watch(state: &mut Value, agent_id: &str, mut payload: Value) {
    let preserved = [
        "last_notified_key",
        "last_notified_at",
        "last_suppressed_key",
        "last_suppressed_at",
        "last_check_key",
        "last_check_at",
        "last_error_observation_key",
        "last_error_observation_cohort",
        "last_error_observed_at",
    ]
    .into_iter()
    .filter_map(|key| abnormal_watch_field(state, agent_id, key).map(|value| (key, value)))
    .collect::<Vec<_>>();
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        if let Some(payload_obj) = payload.as_object_mut() {
            for (key, value) in preserved {
                payload_obj.insert(key.to_string(), value);
            }
        }
        watch.insert(agent_id.to_string(), payload);
    }
}

fn coordinator_child_object<'a>(
    state: &'a mut Value,
    key: &str,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let state_obj = state.as_object_mut()?;
    let coordinator = state_obj
        .entry("coordinator".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !coordinator.is_object() {
        *coordinator = serde_json::json!({});
    }
    let coord_obj = coordinator.as_object_mut()?;
    let child = coord_obj
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !child.is_object() {
        *child = serde_json::json!({});
    }
    child.as_object_mut()
}

fn abnormal_last_notified_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_notified_key")
}

fn abnormal_last_suppressed_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_suppressed_key")
}

fn abnormal_last_check_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_check_key")
}

fn abnormal_last_error_observation_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_error_observation_key")
}

fn abnormal_last_error_observation_cohort(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_error_observation_cohort")
}

/// P1: Python `_TAIL_BYTES` parity (idle_takeover_wiring.py:13) — RS must not read less.
const ABNORMAL_TAIL_BYTES: u64 = 131_072;

/// P1: bounded tail read; a partial first line is harmless (the consumer only parses
/// the latest complete JSONL record) and lossy UTF-8 keeps a mid-codepoint seek safe.
pub(crate) fn read_tail_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// P1: the previous tick's `(size, mtime_ns)` pair from the abnormal watch payload.
fn abnormal_watch_stored_metadata(state: &Value, agent_id: &str) -> Option<(u64, u64)> {
    let watch = state
        .get("coordinator")?
        .get("abnormal_exit_watch")?
        .get(agent_id)?;
    Some((
        watch.get("size")?.as_u64()?,
        watch.get("mtime_ns")?.as_u64()?,
    ))
}

fn abnormal_watch_str(state: &Value, agent_id: &str, field: &str) -> Option<String> {
    state
        .get("coordinator")
        .and_then(|v| v.get("abnormal_exit_watch"))
        .and_then(|v| v.get(agent_id))
        .and_then(|v| v.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn abnormal_watch_field(state: &Value, agent_id: &str, field: &str) -> Option<Value> {
    state
        .get("coordinator")
        .and_then(|v| v.get("abnormal_exit_watch"))
        .and_then(|v| v.get(agent_id))
        .and_then(|v| v.get(field))
        .cloned()
}

fn mark_abnormal_notified(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_notified_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_notified_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn mark_abnormal_suppressed(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_suppressed_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_suppressed_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn mark_abnormal_checked(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_check_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_check_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn mark_abnormal_error_observed(
    state: &mut Value,
    agent_id: &str,
    observation_key: &str,
    cohort_key: &str,
) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(
                "last_error_observation_key".to_string(),
                serde_json::json!(observation_key),
            );
            obj.insert(
                "last_error_observation_cohort".to_string(),
                serde_json::json!(cohort_key),
            );
            obj.insert(
                "last_error_observed_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn write_abnormal_check(
    event_log: &EventLog,
    team: &str,
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    fact: Option<&crate::provider::FaultFact>,
    decision: AbnormalExitDecision,
    error_recency: ErrorRecency,
    size: u64,
    mtime_ns: Option<u64>,
) -> Result<(), TickError> {
    let dead_process = liveness.state == ProcessLiveness::Dead;
    let provider_process_dead = provider_process_dead_fact(liveness);
    let latest_explicit_error = fact.is_some();
    event_log.write(
        "worker.abnormal_exit.check",
        serde_json::json!({
            "team_id": team,
            "agent_id": agent.agent_id.as_str(),
            "provider": provider_wire(agent.provider),
            "path": agent.rollout_path_display.as_str(),
            "size": size,
            "last_offset": size,
            "mtime_ns": mtime_ns,
            "dead_process": dead_process,
            "process_dead": dead_process,
            "provider_process_dead": provider_process_dead,
            "latest_error": latest_explicit_error,
            "latest_explicit_error": latest_explicit_error,
            "error_recency": error_recency.as_str(),
            "fresh_error": error_recency.is_fresh(),
            "dead_process_and_latest_error": dead_process && latest_explicit_error,
            "dead_process_and_latest_explicit_error": dead_process && latest_explicit_error,
            "process_dead_and_latest_explicit_error": dead_process && latest_explicit_error,
            "provider_process_dead_and_latest_explicit_error": provider_process_dead && latest_explicit_error,
            "notification": matches!(decision, AbnormalExitDecision::Notify),
            "suppressed_reason": match decision {
                AbnormalExitDecision::Suppress(reason) => Some(reason),
                AbnormalExitDecision::Notify | AbnormalExitDecision::NoSignal => None,
            },
            "signature": fact.map(|fact| fact.signature.as_str()),
            "turn_id": fact.and_then(|fact| fact.turn_id.as_ref().map(|id| id.as_str())),
            "apiErrorStatus": fact.and_then(|fact| fact.api_error_status),
            "error": fact.and_then(|fact| fact.error.as_deref()),
            "requestId": fact.and_then(|fact| fact.request_id.as_deref()),
            "assistant_uuid": fact.and_then(|fact| fact.assistant_uuid.as_deref()),
            "process_liveness": process_liveness_wire(liveness.state),
            "pid_status": liveness.detail.as_str(),
        }),
    )?;
    Ok(())
}

fn write_abnormal_suppressed(
    event_log: &EventLog,
    team: &str,
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    reason: &str,
) -> Result<(), TickError> {
    let provider_process_dead = provider_process_dead_fact(liveness);
    event_log.write(
        "abnormal_exit.single_signal_suppressed",
        serde_json::json!({
            "team_id": team,
            "agent_id": agent.agent_id.as_str(),
            "provider": provider_wire(agent.provider),
            "path": agent.rollout_path_display.as_str(),
            "reason": reason,
            "notification": false,
            "dead_process": liveness.state == ProcessLiveness::Dead,
            "process_dead": liveness.state == ProcessLiveness::Dead,
            "provider_process_dead": provider_process_dead,
            "latest_error": false,
            "latest_explicit_error": false,
            "error_recency": ErrorRecency::None.as_str(),
            "fresh_error": false,
            "dead_process_and_latest_error": false,
            "dead_process_and_latest_explicit_error": false,
            "process_dead_and_latest_explicit_error": false,
            "provider_process_dead_and_latest_explicit_error": false,
            "process_liveness": process_liveness_wire(liveness.state),
            "pid_status": liveness.detail.as_str(),
        }),
    )?;
    Ok(())
}

fn abnormal_dedupe_key(agent: &AbnormalWatchAgent, fact: &crate::provider::FaultFact) -> String {
    let bucket = fact
        .turn_id
        .as_ref()
        .map(|id| id.as_str().to_string())
        .or_else(|| abnormal_error_fact_identity(fact))
        .unwrap_or_else(|| "no_error_identity".to_string());
    format!(
        "worker.abnormal_exit:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        fact.signature.as_str(),
        bucket
    )
}

fn abnormal_error_cohort_key(agent: &AbnormalWatchAgent) -> String {
    let cohort = agent
        .spawn_epoch
        .map(|epoch| format!("spawn_epoch:{epoch}"))
        .or_else(|| {
            agent
                .spawned_at
                .as_deref()
                .map(|spawned_at| format!("spawned_at:{spawned_at}"))
        })
        .unwrap_or_else(|| "legacy".to_string());
    format!(
        "worker.abnormal_exit.cohort:{}:{}:{}",
        agent.agent_id, agent.rollout_path_display, cohort
    )
}

fn abnormal_error_observation_key(
    agent: &AbnormalWatchAgent,
    fact: &crate::provider::FaultFact,
) -> String {
    let bucket =
        abnormal_error_fact_identity(fact).unwrap_or_else(|| "no_error_identity".to_string());
    format!(
        "worker.abnormal_exit.error:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        fact.signature.as_str(),
        bucket
    )
}

fn abnormal_error_fact_identity(fact: &crate::provider::FaultFact) -> Option<String> {
    fact.assistant_uuid
        .as_deref()
        .or(fact.request_id.as_deref())
        .or_else(|| fact.turn_id.as_ref().map(|id| id.as_str()))
        .map(str::to_string)
}

fn abnormal_error_recency(
    state: &Value,
    agent: &AbnormalWatchAgent,
    observation_key: Option<&str>,
    cohort_key: Option<&str>,
) -> ErrorRecency {
    let (Some(observation_key), Some(cohort_key)) = (observation_key, cohort_key) else {
        return ErrorRecency::None;
    };
    let previous_key = abnormal_last_error_observation_key(state, &agent.agent_id);
    let previous_cohort = abnormal_last_error_observation_cohort(state, &agent.agent_id);
    match (previous_key.as_deref(), previous_cohort.as_deref()) {
        (Some(key), Some(cohort)) if cohort == cohort_key && key != observation_key => {
            ErrorRecency::Fresh
        }
        _ => ErrorRecency::Stale,
    }
}

fn abnormal_suppression_key(
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    reason: &str,
) -> String {
    format!(
        "abnormal_exit.single_signal_suppressed:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        reason,
        process_liveness_wire(liveness.state)
    )
}

fn abnormal_check_key(
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    fact: Option<&crate::provider::FaultFact>,
    error_recency: ErrorRecency,
    error_observation_key: Option<&str>,
) -> String {
    format!(
        "worker.abnormal_exit.check:{}:{}:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        process_liveness_wire(liveness.state),
        fact.map(|fact| fact.signature.as_str()).unwrap_or("-"),
        error_recency.as_str(),
        error_observation_key.unwrap_or("-")
    )
}

fn format_abnormal_exit_message(
    team: &str,
    agent: &AbnormalWatchAgent,
    fact: &crate::provider::FaultFact,
    liveness: &ProcessCheck,
    size: u64,
    recovery_tail: &str,
) -> String {
    let turn_id = fact.turn_id.as_ref().map(|id| id.as_str()).unwrap_or("-");
    format!(
        "Team Agent detected a provider abnormal exit.\n\n\
event: worker.abnormal_exit\n\
team: {team}\n\
node: {node}\n\
provider: {provider}\n\
signature: {signature}\n\
turn_id: {turn_id}\n\
transcript: {path}\n\
last_offset: {size}\n\
pid_status: {pid_status}\n\n\
{recovery_tail}",
        node = agent.agent_id.as_str(),
        provider = provider_wire(agent.provider),
        signature = fact.signature.as_str(),
        path = agent.rollout_path_display.as_str(),
        pid_status = liveness.detail.as_str(),
    )
}

// ─────────────────────────────────────────────────────────────────────────
// 0.5.36 (`.team/artifacts/supermarket-api-error-recovery-locate.md` §7-§8):
// api_error recovery classifier + policy + state I/O + notification tail.
// Recovery EXECUTION lives post-atomic_save in `attempt_due_recoveries`
// below; `detect_abnormal_exits` only RECORDS intent (§7.3, R6 guard).
// ─────────────────────────────────────────────────────────────────────────

/// 0.5.36 §7.1 classifier. Retryable = transient provider outage; recovery
/// may be scheduled. NonRetryable = configuration / auth issue; guide only.
/// Unknown = neither confirmed retryable nor known-bad; guide only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiErrorRecoveryClass {
    Retryable,
    NonRetryable,
    Unknown,
}

impl ApiErrorRecoveryClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::NonRetryable => "non_retryable",
            Self::Unknown => "unknown",
        }
    }
}

/// 0.5.36 §7.1: rules keyed on status/error text.
pub(crate) fn classify_api_error_recovery(
    signature: &str,
    api_error_status: Option<i64>,
    error: Option<&str>,
) -> ApiErrorRecoveryClass {
    if signature != "api_error" {
        return ApiErrorRecoveryClass::Unknown;
    }
    if let Some(status) = api_error_status {
        return match status {
            408 | 429 => ApiErrorRecoveryClass::Retryable,
            s if (500..600).contains(&s) => ApiErrorRecoveryClass::Retryable,
            400 | 401 | 403 | 404 => ApiErrorRecoveryClass::NonRetryable,
            _ => classify_error_text(error),
        };
    }
    classify_error_text(error)
}

fn classify_error_text(error: Option<&str>) -> ApiErrorRecoveryClass {
    match error {
        Some(text) => {
            let lower = text.to_ascii_lowercase();
            if ["rate_limit", "overloaded", "timeout"]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                ApiErrorRecoveryClass::Retryable
            } else if [
                "model_not_found",
                "auth",
                "invalid_request",
                "not_found_error",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
            {
                ApiErrorRecoveryClass::NonRetryable
            } else {
                ApiErrorRecoveryClass::Unknown
            }
        }
        None => ApiErrorRecoveryClass::Unknown,
    }
}

// 0.5.36 §7.2 defaults — code constants; a later config slice may expose
// them without a new CLI flag.
pub(crate) const RECOVERY_MAX_ATTEMPTS: u64 = 2;
pub(crate) const RECOVERY_BACKOFF_SECS: [i64; 2] = [30, 120];
pub(crate) const BACKPRESSURE_THRESHOLD: usize = 3;
pub(crate) const BACKPRESSURE_WINDOW_SECS: i64 = 120;
pub(crate) const BACKPRESSURE_COOLDOWN_SECS: i64 = 300;

fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn cohort_key_for(team: &str, provider: &str, fact: &crate::provider::FaultFact) -> String {
    let status = fact
        .api_error_status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string());
    let error = fact.error.as_deref().unwrap_or("-");
    format!(
        "{team}:{provider}:{signature}:{status}:{error}",
        signature = fact.signature.as_str()
    )
}

/// 0.5.36 §7.2 manual command line. shell-single-quoted workspace so paths
/// with spaces survive copy/paste (§7.5).
fn recovery_manual_command(agent_id: &str, workspace: &Path, team: &str) -> String {
    let workspace_str = workspace.to_string_lossy();
    let shell_workspace = workspace_str.replace('\'', "'\\''");
    format!(
        "team-agent start-agent {agent_id} --workspace '{shell_workspace}' --team {team} --force --json"
    )
}

fn recovery_intent_root<'a>(
    state: &'a mut Value,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    coordinator_child_object(state, "abnormal_api_error_recovery")
}

fn recovery_intent_agents<'a>(
    state: &'a mut Value,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    let root = recovery_intent_root(state)?;
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    agents.as_object_mut()
}

fn recovery_backpressure<'a>(
    state: &'a mut Value,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    let root = recovery_intent_root(state)?;
    let bp = root
        .entry("backpressure".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !bp.is_object() {
        *bp = serde_json::json!({});
    }
    bp.as_object_mut()
}

/// 0.5.36 §7.6: increment the cohort counter, prune stale events outside the
/// sliding window, and return the current active-cooldown state (if any) so
/// the caller can decide canary vs deferral. Only fresh retryable errors get
/// here — non-retryable/unknown never touch backpressure.
fn record_backpressure_event(
    state: &mut Value,
    team: &str,
    provider: &str,
    fact: &crate::provider::FaultFact,
    agent_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> BackpressureDecision {
    let cohort = cohort_key_for(team, provider, fact);
    let now_str = now.to_rfc3339();
    let window = chrono::Duration::seconds(BACKPRESSURE_WINDOW_SECS);
    let cooldown = chrono::Duration::seconds(BACKPRESSURE_COOLDOWN_SECS);
    let Some(bp) = recovery_backpressure(state) else {
        return BackpressureDecision {
            cooldown_until: None,
            just_activated: false,
            cohort_key: cohort,
        };
    };
    let entry = bp
        .entry(cohort.clone())
        .or_insert_with(|| serde_json::json!({}));
    let obj = match entry.as_object_mut() {
        Some(obj) => obj,
        None => {
            *entry = serde_json::json!({});
            entry.as_object_mut().expect("just replaced with object")
        }
    };
    let window_started = obj
        .get("window_started_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let reset_window = window_started
        .map(|start| now.signed_duration_since(start) > window)
        .unwrap_or(true);
    if reset_window {
        obj.insert("window_started_at".to_string(), serde_json::json!(now_str));
        obj.insert("count".to_string(), serde_json::json!(0u64));
        obj.insert(
            "agents".to_string(),
            serde_json::json!(Vec::<String>::new()),
        );
    }
    obj.insert("last_seen_at".to_string(), serde_json::json!(now_str));
    let count = obj
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    obj.insert("count".to_string(), serde_json::json!(count));
    let agents_arr = obj
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if let Some(list) = agents_arr.as_array_mut() {
        if !list.iter().any(|v| v.as_str() == Some(agent_id)) {
            list.push(serde_json::json!(agent_id));
        }
    }
    let previously_active = obj
        .get("cooldown_until")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc) > now)
        .unwrap_or(false);
    let mut just_activated = false;
    let mut cooldown_until: Option<chrono::DateTime<chrono::Utc>> = None;
    if previously_active {
        cooldown_until = obj
            .get("cooldown_until")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
    } else if (count as usize) >= BACKPRESSURE_THRESHOLD {
        let until = now + cooldown;
        obj.insert(
            "cooldown_until".to_string(),
            serde_json::json!(until.to_rfc3339()),
        );
        obj.insert("status".to_string(), serde_json::json!("active"));
        cooldown_until = Some(until);
        just_activated = true;
    }
    BackpressureDecision {
        cooldown_until,
        just_activated,
        cohort_key: cohort,
    }
}

struct BackpressureDecision {
    cooldown_until: Option<chrono::DateTime<chrono::Utc>>,
    just_activated: bool,
    cohort_key: String,
}

/// 0.5.36 §7.2: reserve or update the per-agent recovery intent. Returns the
/// scheduled `next_retry_at` for the notification/event.
fn schedule_recovery_intent(
    state: &mut Value,
    agent_id: &str,
    cohort_key: &str,
    error_key: &str,
    manual_command: &str,
    now: chrono::DateTime<chrono::Utc>,
    cooldown_until: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<RecoveryIntentSchedule> {
    let Some(agents) = recovery_intent_agents(state) else {
        return None;
    };
    let existing = agents.get(agent_id).cloned();
    let attempts = existing
        .as_ref()
        .and_then(|v| v.get("attempts").and_then(Value::as_u64))
        .unwrap_or(0);
    if attempts >= RECOVERY_MAX_ATTEMPTS {
        return None;
    }
    let backoff = RECOVERY_BACKOFF_SECS
        .get(attempts as usize)
        .copied()
        .unwrap_or(*RECOVERY_BACKOFF_SECS.last().unwrap_or(&120));
    let mut next_retry = now + chrono::Duration::seconds(backoff);
    let mut backpressured = false;
    if let Some(until) = cooldown_until {
        if until > next_retry {
            next_retry = until;
            backpressured = true;
        }
    }
    let status = if backpressured {
        "backpressured"
    } else {
        "scheduled"
    };
    let payload = serde_json::json!({
        "error_key": error_key,
        "cohort_key": cohort_key,
        "status": status,
        "attempts": attempts,
        "max_attempts": RECOVERY_MAX_ATTEMPTS,
        "next_retry_at": next_retry.to_rfc3339(),
        "last_attempt_at": Value::Null,
        "last_error": Value::Null,
        "backoff_seconds": backoff,
        "manual_command": manual_command,
    });
    agents.insert(agent_id.to_string(), payload);
    Some(RecoveryIntentSchedule {
        next_retry_at: next_retry.to_rfc3339(),
        attempt: attempts,
        backoff_seconds: backoff,
        backpressured,
    })
}

struct RecoveryIntentSchedule {
    next_retry_at: String,
    attempt: u64,
    backoff_seconds: i64,
    backpressured: bool,
}

/// 0.5.36 §7.6: returns true when any agent already holds a `scheduled`
/// recovery intent for the given cohort.
fn has_active_canary_in_cohort(state: &Value, cohort_key: &str) -> bool {
    let Some(agents) = state
        .pointer("/coordinator/abnormal_api_error_recovery/agents")
        .and_then(Value::as_object)
    else {
        return false;
    };
    agents.values().any(|entry| {
        entry.get("status").and_then(Value::as_str) == Some("scheduled")
            && entry.get("cohort_key").and_then(Value::as_str) == Some(cohort_key)
    })
}

fn recovery_intent_status(state: &Value, agent_id: &str) -> Option<String> {
    state
        .pointer(&format!(
            "/coordinator/abnormal_api_error_recovery/agents/{agent_id}/status"
        ))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn recovery_intent_field(state: &Value, agent_id: &str, field: &str) -> Option<Value> {
    state
        .pointer(&format!(
            "/coordinator/abnormal_api_error_recovery/agents/{agent_id}/{field}"
        ))
        .cloned()
}

/// 0.5.36 §7.5 policy-aware notification tail.
fn recovery_notification_tail(
    class: ApiErrorRecoveryClass,
    schedule: Option<&RecoveryIntentSchedule>,
    backpressure_until: Option<chrono::DateTime<chrono::Utc>>,
    cohort_key: &str,
    fact: &crate::provider::FaultFact,
    manual_command: &str,
) -> String {
    let manual_line = format!("manual_recovery: {manual_command}");
    let backpressure_line = match backpressure_until {
        Some(until) => {
            let status = fact
                .api_error_status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string());
            format!(
                "backpressure: active cohort={cohort_key} status={status} until={until}",
                until = until.to_rfc3339()
            )
        }
        None => "backpressure: inactive".to_string(),
    };
    let auto_line = match class {
        ApiErrorRecoveryClass::Retryable => match schedule {
            Some(sched) if sched.backpressured => format!(
                "auto_recovery: delayed by provider backpressure until {}",
                sched.next_retry_at
            ),
            Some(sched) => format!(
                "auto_recovery: scheduled attempt {attempt}/{max} in {backoff}s (due {due})",
                attempt = sched.attempt + 1,
                max = RECOVERY_MAX_ATTEMPTS,
                backoff = sched.backoff_seconds,
                due = sched.next_retry_at,
            ),
            None => "auto_recovery: not scheduled (recovery_exhausted); use manual command below"
                .to_string(),
        },
        ApiErrorRecoveryClass::NonRetryable => {
            let error = fact.error.as_deref().unwrap_or("non_retryable");
            format!("auto_recovery: not scheduled (non_retryable_api_error: {error})")
        }
        ApiErrorRecoveryClass::Unknown => {
            "auto_recovery: not scheduled (unknown_api_error_class)".to_string()
        }
    };
    format!("{auto_line}\n{manual_line}\n{backpressure_line}")
}

/// 0.5.36 §7 orchestration for a single fresh abnormal notification. Runs
/// inside `detect_abnormal_exits`; only classifies + records intent +
/// emits recovery-family events. Returns the classifier verdict, the
/// scheduled intent (if any), the cohort key, and the active backpressure
/// window (if any) so the caller can build the notification tail.
///
/// R6 guard: this function must not call any lifecycle helpers. Actual
/// lifecycle work runs post-atomic_save in `attempt_due_recoveries`.
fn process_api_error_recovery_intent(
    state: &mut Value,
    event_log: &EventLog,
    team: &str,
    agent: &AbnormalWatchAgent,
    fact: &crate::provider::FaultFact,
    manual_command: &str,
    error_key: &str,
) -> Result<
    (
        ApiErrorRecoveryClass,
        Option<RecoveryIntentSchedule>,
        String,
        Option<chrono::DateTime<chrono::Utc>>,
    ),
    TickError,
> {
    let class = classify_api_error_recovery(
        fact.signature.as_str(),
        fact.api_error_status,
        fact.error.as_deref(),
    );
    let provider_str = provider_wire(agent.provider);
    let cohort_key = cohort_key_for(team, provider_str, fact);
    if !matches!(class, ApiErrorRecoveryClass::Retryable) {
        return Ok((class, None, cohort_key, None));
    }
    let now = now_utc();
    let cohort_hint = cohort_key_for(team, provider_str, fact);
    let canary_active = has_active_canary_in_cohort(state, &cohort_hint);
    let mut bp_decision = record_backpressure_event(
        state,
        team,
        provider_str,
        fact,
        agent.agent_id.as_str(),
        now,
    );
    // 0.5.36 §7.6: "at most one canary". If another agent in the same
    // cohort already holds a scheduled intent, defer the new arrival to
    // the cohort cooldown (or a synthetic short cooldown if none yet).
    if canary_active {
        let synthetic = bp_decision
            .cooldown_until
            .unwrap_or_else(|| now + chrono::Duration::seconds(BACKPRESSURE_COOLDOWN_SECS));
        bp_decision.cooldown_until = Some(synthetic);
    }
    if bp_decision.just_activated {
        let bp_agents = state
            .pointer(&format!(
                "/coordinator/abnormal_api_error_recovery/backpressure/{}/agents",
                bp_decision.cohort_key
            ))
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let cooldown_until_str = bp_decision
            .cooldown_until
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        event_log.write(
            "worker.abnormal_exit.backpressure_started",
            serde_json::json!({
                "team_id": team,
                "provider": provider_str,
                "signature": fact.signature.as_str(),
                "apiErrorStatus": fact.api_error_status,
                "error": fact.error.as_deref(),
                "cohort_key": bp_decision.cohort_key,
                "threshold": BACKPRESSURE_THRESHOLD,
                "window_seconds": BACKPRESSURE_WINDOW_SECS,
                "cooldown_until": cooldown_until_str,
                "agents": bp_agents,
            }),
        )?;
    }
    let schedule = schedule_recovery_intent(
        state,
        agent.agent_id.as_str(),
        &bp_decision.cohort_key,
        error_key,
        manual_command,
        now,
        bp_decision.cooldown_until,
    );
    if let Some(sched) = schedule.as_ref() {
        if sched.backpressured {
            // Backpressured workers are not scheduled for the canary window;
            // emit a distinct `backpressure_active` event so the leader
            // notification tail can trace which cohort deferred which agent.
            event_log.write(
                "worker.abnormal_exit.backpressure_active",
                serde_json::json!({
                    "team_id": team,
                    "agent_id": agent.agent_id.as_str(),
                    "cohort_key": bp_decision.cohort_key,
                    "cooldown_until": bp_decision
                        .cooldown_until
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                    "action": "deferred_recovery",
                    "manual_command": manual_command,
                }),
            )?;
        } else {
            event_log.write(
                "worker.abnormal_exit.recovery_scheduled",
                serde_json::json!({
                    "team_id": team,
                    "agent_id": agent.agent_id.as_str(),
                    "provider": provider_str,
                    "signature": fact.signature.as_str(),
                    "apiErrorStatus": fact.api_error_status,
                    "error": fact.error.as_deref(),
                    "attempt": sched.attempt,
                    "max_attempts": RECOVERY_MAX_ATTEMPTS,
                    "due_at": sched.next_retry_at,
                    "backoff_seconds": sched.backoff_seconds,
                    "error_key": error_key,
                    "cohort_key": bp_decision.cohort_key,
                    "backpressured": false,
                    "manual_command": manual_command,
                }),
            )?;
        }
    } else {
        // schedule_recovery_intent returns None ONLY when max_attempts hit.
        event_log.write(
            "worker.abnormal_exit.recovery_exhausted",
            serde_json::json!({
                "team_id": team,
                "agent_id": agent.agent_id.as_str(),
                "attempts": RECOVERY_MAX_ATTEMPTS,
                "last_error": fact.error.as_deref(),
                "manual_command": manual_command,
            }),
        )?;
    }
    Ok((
        class,
        schedule,
        bp_decision.cohort_key,
        bp_decision.cooldown_until,
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// 0.5.36 §7.3 recovery execution — runs post-atomic_save, reloads fresh
// state, consumes due intents, invokes the lifecycle `start_agent_at_paths`
// with force=true (stop-before-start semantics for live panes, so noop is
// impossible), and writes the outcome back to state via the lifecycle
// path (which owns its own save).
// ─────────────────────────────────────────────────────────────────────────

/// 0.5.36 §7.3: process all due recovery intents. Called from tick.rs
/// AFTER atomic_save has flushed the detector-written intent. Reloads a
/// fresh state each call so the caller's stale in-memory state cannot
/// clobber lifecycle writes. Best-effort: recovery failure produces
/// events + updated intent, never a tick failure.
pub(crate) fn attempt_due_recoveries(
    workspace: &Path,
    event_log: &EventLog,
    transport: &dyn crate::transport::Transport,
) {
    // 0.5.43 debt-sweep (§5 D-j): single fresh post-save load. Do NOT
    // re-use the tick's pre-save Value — that would reopen the 0.5.36
    // R6 lifecycle-save-clobbered-by-tick window (highest risk item in
    // this slice per locate). We own our load here, mutate in memory
    // (`clear_stale_terminal_next_retry_at` is now a pure
    // `(&mut Value) -> bool` scrub), persist through the existing
    // allowlisted root save when the scrub actually changed a row, and
    // then collect due agents from the SAME post-scrub Value so
    // terminal intents newly stripped of `next_retry_at` cannot double-
    // fire below.
    let Ok(mut state) = crate::state::persist::load_runtime_state(workspace) else {
        return;
    };
    if clear_stale_terminal_next_retry_at(&mut state) {
        let _ = crate::state::repository::StateRepository::new(workspace).save(
            crate::state::repository::StateWriteIntent::CoordinatorApiErrorRecovery {
                team_key: state.get("active_team_key").and_then(Value::as_str),
                agent_id: None,
            },
            &state,
        );
    }
    let due_agents = collect_due_recovery_agents(&state);
    for agent_id in due_agents {
        run_single_recovery(workspace, event_log, transport, &agent_id);
    }
}

fn collect_due_recovery_agents(state: &Value) -> Vec<String> {
    let now = now_utc();
    let Some(agents) = state
        .pointer("/coordinator/abnormal_api_error_recovery/agents")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut due = Vec::new();
    for (agent_id, intent) in agents {
        let status = intent.get("status").and_then(Value::as_str).unwrap_or("");
        // 0.5.37 R8: dispatch only for non-terminal states. `succeeded`,
        // `blocked`, `exhausted`, `backpressured` are terminal / awaiting
        // manual action — a lifecycle dispatch here would be double-fire
        // or wasted work. `scheduled` / `running` remain dispatchable so
        // an interrupted tick can resume.
        if !matches!(status, "scheduled" | "running") {
            continue;
        }
        let Some(next_retry_at) = intent
            .get("next_retry_at")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        if next_retry_at <= now {
            due.push(agent_id.clone());
        }
    }
    due
}

/// 0.5.37 R8 + 0.5.43 debt-sweep §5 D-j: pure scrub helper. Terminal
/// recovery intents (`succeeded`/`blocked`/`exhausted`) must not carry
/// a stale `next_retry_at`. Returns true when at least one row was
/// stripped, so the caller can save through its own load/write cycle
/// (no double-load, no double-save). No I/O in this function — the
/// only responsibility is mutation on a caller-owned `Value`.
fn clear_stale_terminal_next_retry_at(state: &mut Value) -> bool {
    let mut mutated = false;
    if let Some(agents) = recovery_intent_agents(state) {
        for (_agent_id, entry) in agents.iter_mut() {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            let status = obj
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !matches!(status.as_str(), "succeeded" | "blocked" | "exhausted") {
                continue;
            }
            let had_stale = obj.get("next_retry_at").filter(|v| !v.is_null()).is_some();
            if had_stale {
                obj.remove("next_retry_at");
                mutated = true;
            }
        }
    }
    mutated
}

fn run_single_recovery(
    workspace: &Path,
    event_log: &EventLog,
    transport: &dyn crate::transport::Transport,
    agent_id: &str,
) {
    // Read the current intent so we can bump attempts / write result fields
    // through the shared writer (`write_recovery_intent_field`); the actual
    // lifecycle helper below owns its own state save.
    let Ok(state_before) = crate::state::persist::load_runtime_state(workspace) else {
        return;
    };
    let team_key = crate::state::projection::team_state_key(&state_before);
    let attempt = state_before
        .pointer(&format!(
            "/coordinator/abnormal_api_error_recovery/agents/{agent_id}/attempts"
        ))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let manual_command = state_before
        .pointer(&format!(
            "/coordinator/abnormal_api_error_recovery/agents/{agent_id}/manual_command"
        ))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| recovery_manual_command(agent_id, workspace, team_key.as_str()));
    let _ = event_log.write(
        "worker.abnormal_exit.recovery_started",
        serde_json::json!({
            "team_id": team_key.as_str(),
            "agent_id": agent_id,
            "attempt": attempt,
            "mode": "start_agent_force",
            "allow_fresh": false,
        }),
    );
    let agent_id_typed = crate::model::ids::AgentId::new(agent_id.to_string());
    let outcome = crate::lifecycle::restart::start_agent_at_paths_for_recovery(
        workspace,
        &agent_id_typed,
        Some(team_key.as_str()),
        transport,
    );
    let now = now_utc();
    match outcome {
        Ok(recovery_outcome) => {
            let _ = event_log.write(
                "worker.abnormal_exit.recovery_succeeded",
                serde_json::json!({
                    "team_id": team_key.as_str(),
                    "agent_id": agent_id,
                    "start_mode": recovery_outcome.start_mode,
                    "target": recovery_outcome.target,
                    "coordinator_started": recovery_outcome.coordinator_started,
                }),
            );
            write_recovery_intent_result(
                workspace,
                agent_id,
                RecoveryIntentUpdate {
                    status: "succeeded",
                    attempts: attempt + 1,
                    last_attempt_at: now.to_rfc3339(),
                    last_error: None,
                    blocked_reason: None,
                },
            );
        }
        Err(RecoveryError::NoopBlocked) => {
            let _ = event_log.write(
                "worker.abnormal_exit.recovery_blocked",
                serde_json::json!({
                    "team_id": team_key.as_str(),
                    "agent_id": agent_id,
                    "reason": "noop_not_recovery",
                    "manual_command": manual_command,
                }),
            );
            write_recovery_intent_result(
                workspace,
                agent_id,
                RecoveryIntentUpdate {
                    status: "blocked",
                    attempts: attempt + 1,
                    last_attempt_at: now.to_rfc3339(),
                    last_error: None,
                    blocked_reason: Some("noop_not_recovery"),
                },
            );
        }
        Err(RecoveryError::Lifecycle(reason)) => {
            let _ = event_log.write(
                "worker.abnormal_exit.recovery_blocked",
                serde_json::json!({
                    "team_id": team_key.as_str(),
                    "agent_id": agent_id,
                    "reason": "lifecycle_error",
                    "detail": reason,
                    "manual_command": manual_command,
                }),
            );
            write_recovery_intent_result(
                workspace,
                agent_id,
                RecoveryIntentUpdate {
                    status: "blocked",
                    attempts: attempt + 1,
                    last_attempt_at: now.to_rfc3339(),
                    last_error: Some(reason),
                    blocked_reason: Some("lifecycle_error"),
                },
            );
        }
    }
}

pub(crate) enum RecoveryError {
    NoopBlocked,
    Lifecycle(String),
}

/// 0.5.36 §7.3 typed outcome returned from
/// `lifecycle::restart::start_agent_at_paths_for_recovery` to the post-save
/// step. Small on purpose — just the fields the `recovery_succeeded` event
/// needs.
pub(crate) struct RecoveryLifecycleOutcome {
    pub start_mode: String,
    pub target: String,
    pub coordinator_started: bool,
}

struct RecoveryIntentUpdate {
    status: &'static str,
    attempts: u64,
    last_attempt_at: String,
    last_error: Option<String>,
    blocked_reason: Option<&'static str>,
}

fn write_recovery_intent_result(workspace: &Path, agent_id: &str, update: RecoveryIntentUpdate) {
    let Ok(mut state) = crate::state::persist::load_runtime_state(workspace) else {
        return;
    };
    if let Some(agents) = recovery_intent_agents(&mut state) {
        let entry = agents
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("status".to_string(), serde_json::json!(update.status));
            obj.insert("attempts".to_string(), serde_json::json!(update.attempts));
            obj.insert(
                "last_attempt_at".to_string(),
                serde_json::json!(update.last_attempt_at),
            );
            obj.insert(
                "last_error".to_string(),
                match update.last_error.as_ref() {
                    Some(text) => serde_json::json!(text),
                    None => Value::Null,
                },
            );
            match update.blocked_reason {
                Some(reason) => {
                    obj.insert("blocked_reason".to_string(), serde_json::json!(reason));
                }
                None => {
                    obj.remove("blocked_reason");
                }
            }
            // 0.5.37 R8: transitioning into a terminal state clears the
            // stale `next_retry_at`; a future retry earns a fresh due
            // time when it re-enters `scheduled` through the detector.
            if matches!(update.status, "succeeded" | "blocked" | "exhausted") {
                obj.remove("next_retry_at");
            }
        }
    }
    let _ = crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::CoordinatorApiErrorRecovery {
            team_key: state.get("active_team_key").and_then(Value::as_str),
            agent_id: Some(agent_id),
        },
        &state,
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::coordinator::tick::Coordinator;
    use crate::coordinator::types::{ErrorLists, ProviderRegistry, WorkspacePath};
    use std::io::Write as _;

    struct NormalRegistry;

    impl ProviderRegistry for NormalRegistry {
        fn adapter_for(
            &self,
            provider: crate::provider::Provider,
        ) -> Box<dyn crate::provider::ProviderAdapter> {
            crate::provider::get_adapter(provider)
        }

        fn error_lists(&self, _provider: crate::provider::Provider) -> ErrorLists {
            ErrorLists::default()
        }
    }
    #[test]
    fn abnormal_stale_error_baselines_then_fresh_alive_error_notifies() {
        let dir = temp_abnormal_dir("alive-fresh");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
        )
        .unwrap();
        seed_abnormal_state(&dir, &rollout, "alive", 1);
        let coordinator = abnormal_test_coordinator(&dir);

        coordinator.tick().unwrap();

        let first_events = read_test_events(&dir);
        assert!(
            find_event(&first_events, "worker.abnormal_exit").is_none(),
            "first observed explicit error is stale baseline and must not notify; events={first_events:?}"
        );
        let stale_check =
            find_event(&first_events, "worker.abnormal_exit.check").expect("stale check event");
        assert_eq!(stale_check["error_recency"], serde_json::json!("stale"));
        assert_eq!(stale_check["fresh_error"], serde_json::json!(false));
        assert_eq!(stale_check["notification"], serde_json::json!(false));
        assert!(
            find_event(&first_events, "abnormal_exit.single_signal_suppressed").is_none(),
            "stale/error-only observations must not emit single-signal suppression"
        );
        let watch = abnormal_test_watch(&dir);
        assert_eq!(watch["error_recency"], serde_json::json!("stale"));
        assert_eq!(watch["fresh_error"], serde_json::json!(false));
        assert!(
            watch["last_error_observation_key"].as_str().is_some(),
            "first explicit error must persist the stale baseline; watch={watch}"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                b"{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t2\",\"status\":\"failed\"}}}\n",
            )
            .unwrap();
        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        let abnormal =
            find_event(&events, "worker.abnormal_exit").expect("fresh alive error notification");
        assert_eq!(abnormal["provider_process_dead"], serde_json::json!(false));
        assert_eq!(abnormal["latest_explicit_error"], serde_json::json!(true));
        assert_eq!(abnormal["error_recency"], serde_json::json!("fresh"));
        assert_eq!(abnormal["fresh_error"], serde_json::json!(true));
        assert_eq!(
            abnormal["provider_process_dead_and_latest_explicit_error"],
            serde_json::json!(false)
        );
        assert_eq!(abnormal["notification_status"], serde_json::json!("queued"));
    }

    #[test]
    fn abnormal_claude_assistant_api_error_events_include_structured_details() {
        let dir = temp_abnormal_dir("claude-api-details");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            claude_assistant_api_error_line(
                "assistant-400",
                "parent-400",
                "session-400",
                400,
                "unknown",
                None,
            ),
        )
        .unwrap();
        seed_abnormal_state_with_provider(&dir, &rollout, "alive", 1, "claude_code");
        let coordinator = abnormal_test_coordinator(&dir);

        coordinator.tick().unwrap();

        let first_events = read_test_events(&dir);
        let stale_check =
            find_event(&first_events, "worker.abnormal_exit.check").expect("stale check event");
        assert_eq!(stale_check["error_recency"], serde_json::json!("stale"));
        assert_eq!(stale_check["notification"], serde_json::json!(false));
        assert_eq!(stale_check["apiErrorStatus"], serde_json::json!(400));
        assert_eq!(stale_check["error"], serde_json::json!("unknown"));
        assert_eq!(stale_check["requestId"], serde_json::json!(null));
        assert_eq!(
            stale_check["assistant_uuid"],
            serde_json::json!("assistant-400")
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                claude_assistant_api_error_line(
                    "assistant-404",
                    "parent-404",
                    "session-404",
                    404,
                    "model_not_found",
                    Some("req_011CceNfWj2aPY5gtCdakULt"),
                )
                .as_bytes(),
            )
            .unwrap();
        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        let fresh_check =
            find_event(&events, "worker.abnormal_exit.check").expect("fresh check event");
        assert_eq!(fresh_check["error_recency"], serde_json::json!("fresh"));
        assert_eq!(fresh_check["notification"], serde_json::json!(true));
        assert_eq!(fresh_check["apiErrorStatus"], serde_json::json!(404));
        assert_eq!(fresh_check["error"], serde_json::json!("model_not_found"));
        assert_eq!(
            fresh_check["requestId"],
            serde_json::json!("req_011CceNfWj2aPY5gtCdakULt")
        );
        assert_eq!(
            fresh_check["assistant_uuid"],
            serde_json::json!("assistant-404")
        );

        let abnormal =
            find_event(&events, "worker.abnormal_exit").expect("fresh alive error notification");
        assert_eq!(abnormal["signature"], serde_json::json!("api_error"));
        assert_eq!(abnormal["turn_id"], serde_json::json!("assistant-404"));
        assert_eq!(abnormal["apiErrorStatus"], serde_json::json!(404));
        assert_eq!(abnormal["error"], serde_json::json!("model_not_found"));
        assert_eq!(
            abnormal["requestId"],
            serde_json::json!("req_011CceNfWj2aPY5gtCdakULt")
        );
        assert_eq!(
            abnormal["assistant_uuid"],
            serde_json::json!("assistant-404")
        );
        assert_eq!(abnormal["notification_status"], serde_json::json!("queued"));
    }

    #[test]
    fn abnormal_claude_assistant_api_error_followed_by_bookkeeping_still_detected() {
        let dir = temp_abnormal_dir("claude-api-bookkeeping");
        let rollout = dir.join("rollout-w1.jsonl");
        let session_id = "97ec1070-f19b-49ed-b60f-3cb158e92053";
        std::fs::write(
            &rollout,
            format!(
                "{}{}",
                claude_assistant_api_error_line(
                    "94e88d55-aac7-46bc-85cd-b7fcfc8a9ef6",
                    "245fce6f-2427-4b05-af01-8619df64afab",
                    session_id,
                    404,
                    "model_not_found",
                    Some("req_011CceUVQ94dxahgwAHf8sdS"),
                ),
                claude_turn_duration_line(
                    "6c96328b-8c01-46c0-b68e-a096a89c904f",
                    "94e88d55-aac7-46bc-85cd-b7fcfc8a9ef6",
                    session_id,
                ),
            ),
        )
        .unwrap();
        seed_abnormal_state_with_provider(&dir, &rollout, "alive", 1, "claude_code");
        let coordinator = abnormal_test_coordinator(&dir);

        coordinator.tick().unwrap();

        let first_events = read_test_events(&dir);
        let first_check =
            find_event(&first_events, "worker.abnormal_exit.check").expect("stale check event");
        assert_eq!(
            first_check["latest_explicit_error"],
            serde_json::json!(true)
        );
        assert_eq!(first_check["error_recency"], serde_json::json!("stale"));
        assert_eq!(first_check["notification"], serde_json::json!(false));
        assert_eq!(first_check["apiErrorStatus"], serde_json::json!(404));
        assert_eq!(first_check["error"], serde_json::json!("model_not_found"));
        assert_eq!(
            first_check["requestId"],
            serde_json::json!("req_011CceUVQ94dxahgwAHf8sdS")
        );
        assert_eq!(
            first_check["assistant_uuid"],
            serde_json::json!("94e88d55-aac7-46bc-85cd-b7fcfc8a9ef6")
        );
        assert!(
            find_event(&first_events, "worker.abnormal_exit").is_none(),
            "first observed error baselines stale even when followed by turn_duration"
        );
        let first_observation_key = abnormal_test_watch(&dir)["last_error_observation_key"]
            .as_str()
            .expect("stale baseline persists observation key")
            .to_string();

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                format!(
                    "{}{}{}",
                    claude_file_history_snapshot_line(session_id),
                    claude_last_prompt_line(session_id),
                    claude_mode_line(session_id),
                )
                .as_bytes(),
            )
            .unwrap();
        coordinator.tick().unwrap();

        let after_bookkeeping_events = read_test_events(&dir);
        let bookkeeping_check = find_event(&after_bookkeeping_events, "worker.abnormal_exit.check")
            .expect("bookkeeping check event");
        assert_eq!(
            bookkeeping_check["error_recency"],
            serde_json::json!("stale")
        );
        assert_eq!(bookkeeping_check["notification"], serde_json::json!(false));
        assert_eq!(
            bookkeeping_check["assistant_uuid"],
            serde_json::json!("94e88d55-aac7-46bc-85cd-b7fcfc8a9ef6")
        );
        assert_eq!(
            abnormal_test_watch(&dir)["last_error_observation_key"],
            serde_json::json!(first_observation_key)
        );
        assert!(
            find_event(&after_bookkeeping_events, "worker.abnormal_exit").is_none(),
            "bookkeeping-only growth after the same error must not notify"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                format!(
                    "{}{}",
                    claude_assistant_api_error_line(
                        "11b8790e-718b-4910-bf6a-62d19135a93b",
                        "ccea3b0f-23e7-48ed-aa57-d59e321becfb",
                        session_id,
                        404,
                        "model_not_found",
                        Some("req_011CceUhRwYfdmLPbv9nMmqX"),
                    ),
                    claude_turn_duration_line(
                        "2d382eff-783b-48ca-83c1-c93d3dc8b068",
                        "11b8790e-718b-4910-bf6a-62d19135a93b",
                        session_id,
                    ),
                )
                .as_bytes(),
            )
            .unwrap();
        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        let fresh_check =
            find_event(&events, "worker.abnormal_exit.check").expect("fresh check event");
        assert_eq!(fresh_check["error_recency"], serde_json::json!("fresh"));
        assert_eq!(fresh_check["notification"], serde_json::json!(true));
        assert_eq!(
            fresh_check["assistant_uuid"],
            serde_json::json!("11b8790e-718b-4910-bf6a-62d19135a93b")
        );
        let abnormal =
            find_event(&events, "worker.abnormal_exit").expect("fresh API error notification");
        assert_eq!(
            abnormal["turn_id"],
            serde_json::json!("11b8790e-718b-4910-bf6a-62d19135a93b")
        );
        assert_eq!(
            abnormal["requestId"],
            serde_json::json!("req_011CceUhRwYfdmLPbv9nMmqX")
        );
        assert_eq!(abnormal["notification_status"], serde_json::json!("queued"));
    }

    #[test]
    fn codex_failed_turn_then_completed_turn_no_fresh_notification() {
        let dir = temp_abnormal_dir("codex-failed-then-completed");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
        )
        .unwrap();
        seed_abnormal_state(&dir, &rollout, "alive", 1);
        let coordinator = abnormal_test_coordinator(&dir);

        coordinator.tick().unwrap();

        let first_events = read_test_events(&dir);
        let first_check =
            find_event(&first_events, "worker.abnormal_exit.check").expect("stale check event");
        assert_eq!(first_check["turn_id"], serde_json::json!("t1"));
        assert_eq!(first_check["error_recency"], serde_json::json!("stale"));
        assert_eq!(first_check["notification"], serde_json::json!(false));
        assert!(
            find_event(&first_events, "worker.abnormal_exit").is_none(),
            "first failed turn is only a stale baseline"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                b"{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t-complete\",\"status\":\"completed\"}}}\n",
            )
            .unwrap();
        coordinator.tick().unwrap();

        let after_completed_events = read_test_events(&dir);
        let completed_check = find_event(&after_completed_events, "worker.abnormal_exit.check")
            .expect("completed-after-failure check event");
        assert_eq!(completed_check["turn_id"], serde_json::json!("t1"));
        assert_eq!(completed_check["error_recency"], serde_json::json!("stale"));
        assert_eq!(completed_check["fresh_error"], serde_json::json!(false));
        assert_eq!(completed_check["notification"], serde_json::json!(false));
        assert!(
            find_event(&after_completed_events, "worker.abnormal_exit").is_none(),
            "completed turn after the same old failed turn must not emit a fresh notification"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                b"{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t2\",\"status\":\"failed\"}}}\n",
            )
            .unwrap();
        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        let fresh_check =
            find_event(&events, "worker.abnormal_exit.check").expect("new failed turn check event");
        assert_eq!(fresh_check["turn_id"], serde_json::json!("t2"));
        assert_eq!(fresh_check["error_recency"], serde_json::json!("fresh"));
        assert_eq!(fresh_check["notification"], serde_json::json!(true));
        let abnormal =
            find_event(&events, "worker.abnormal_exit").expect("fresh Codex failure notification");
        assert_eq!(abnormal["turn_id"], serde_json::json!("t2"));
        assert_eq!(abnormal["notification_status"], serde_json::json!("queued"));
    }

    #[test]
    fn abnormal_dead_process_without_explicit_error_stays_dead_only() {
        let dir = temp_abnormal_dir("dead-only");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"completed\"}}}\n",
        )
        .unwrap();
        seed_abnormal_state(&dir, &rollout, "dead", 1);
        let coordinator = abnormal_test_coordinator(&dir);

        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        assert!(
            find_event(&events, "worker.abnormal_exit").is_none(),
            "dead process without explicit error must not notify; events={events:?}"
        );
        let suppressed =
            find_event(&events, "abnormal_exit.single_signal_suppressed").expect("dead_only");
        assert_eq!(suppressed["reason"], serde_json::json!("dead_only"));
        assert_eq!(
            suppressed["latest_explicit_error"],
            serde_json::json!(false)
        );
        assert_eq!(suppressed["error_recency"], serde_json::json!("none"));
        assert_eq!(suppressed["fresh_error"], serde_json::json!(false));
    }

    #[test]
    fn abnormal_unchanged_transcript_keeps_provider_death_out_of_pane_projection() {
        let dir = temp_abnormal_dir("unchanged-provider-dead");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"completed\"}}}\n",
        )
        .unwrap();
        seed_abnormal_state(&dir, &rollout, "alive", 1);
        let coordinator = abnormal_test_coordinator(&dir);
        coordinator.tick().unwrap();

        let mut state = crate::state::persist::load_runtime_state(&dir).unwrap();
        state["agents"]["w1"]["process_liveness"] = serde_json::json!("dead");
        crate::state::persist::save_runtime_state(&dir, &state).unwrap();
        coordinator.tick().unwrap();

        let state = crate::state::persist::load_runtime_state(&dir).unwrap();
        let agent = &state["agents"]["w1"];
        assert!(
            agent.get("provider_process_dead").is_none(),
            "provider death belongs to abnormal watch, not the pane-dead seat projection: {agent}"
        );
        assert!(
            agent.get("stale_reason").is_none(),
            "provider death must not be mislabeled pane_dead: {agent}"
        );
        let watch = &state["coordinator"]["abnormal_exit_watch"]["w1"];
        assert_eq!(watch["provider_process_dead"], serde_json::json!(true));
        assert_eq!(watch["worker_provider_exited"], serde_json::json!(false));
        assert_eq!(
            watch["last_liveness_detail"],
            serde_json::json!("explicit:dead")
        );
    }

    #[test]
    fn abnormal_pane_absence_is_not_provider_process_death() {
        let agent = test_abnormal_agent("/tmp/rollout.jsonl", Some(1), None);
        for detail in ["pane_dead:%1", "window_missing:w1"] {
            let payload = abnormal_watch_payload(
                &agent,
                Some(1),
                Some(2),
                process_check(ProcessLiveness::Dead, detail.to_string()),
                None,
                ErrorRecency::None,
                None,
                None,
            );
            assert_eq!(
                payload["provider_process_dead"],
                serde_json::json!(false),
                "pane absence and provider death are distinct facts: {payload}"
            );
            assert_eq!(payload["worker_provider_exited"], serde_json::json!(false));
        }
    }

    #[test]
    fn abnormal_recency_treats_cohort_change_as_stale() {
        let agent = test_abnormal_agent("/tmp/rollout.jsonl", Some(2), None);
        let fact = crate::provider::latest_explicit_error_fact(
            crate::provider::Provider::Codex,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
        )
        .unwrap();
        let observation_key = abnormal_error_observation_key(&agent, &fact);
        let state = serde_json::json!({
            "coordinator": {
                "abnormal_exit_watch": {
                    "w1": {
                        "last_error_observation_key": observation_key,
                        "last_error_observation_cohort": "worker.abnormal_exit.cohort:w1:/tmp/rollout.jsonl:spawn_epoch:1"
                    }
                }
            }
        });
        let cohort_key = abnormal_error_cohort_key(&agent);

        let recency = abnormal_error_recency(
            &state,
            &agent,
            Some(observation_key.as_str()),
            Some(cohort_key.as_str()),
        );

        assert_eq!(
            recency,
            ErrorRecency::Stale,
            "cohort change baselines the observed error instead of treating it as fresh"
        );
    }

    #[test]
    fn abnormal_dead_fresh_error_event_reports_actual_dead_booleans() {
        let dir = temp_abnormal_dir("dead-fresh");
        let rollout = dir.join("rollout-w1.jsonl");
        std::fs::write(
            &rollout,
            "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
        )
        .unwrap();
        seed_abnormal_state(&dir, &rollout, "dead", 1);
        let coordinator = abnormal_test_coordinator(&dir);
        coordinator.tick().unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(
                b"{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t2\",\"status\":\"failed\"}}}\n",
            )
            .unwrap();

        coordinator.tick().unwrap();

        let events = read_test_events(&dir);
        let abnormal =
            find_event(&events, "worker.abnormal_exit").expect("fresh dead error notification");
        assert_eq!(abnormal["provider_process_dead"], serde_json::json!(true));
        assert_eq!(abnormal["latest_explicit_error"], serde_json::json!(true));
        assert_eq!(abnormal["error_recency"], serde_json::json!("fresh"));
        assert_eq!(
            abnormal["provider_process_dead_and_latest_explicit_error"],
            serde_json::json!(true)
        );
    }

    fn abnormal_test_coordinator(dir: &std::path::Path) -> Coordinator {
        Coordinator::for_test(
            WorkspacePath::new(dir.to_path_buf()),
            Box::new(NormalRegistry),
            Box::new(
                crate::transport::test_support::OfflineTransport::new().with_session_present(true),
            ),
            None,
            None,
        )
    }

    fn seed_abnormal_state(
        dir: &std::path::Path,
        rollout: &std::path::Path,
        liveness: &str,
        spawn_epoch: u64,
    ) {
        seed_abnormal_state_with_provider(dir, rollout, liveness, spawn_epoch, "codex");
    }

    fn seed_abnormal_state_with_provider(
        dir: &std::path::Path,
        rollout: &std::path::Path,
        liveness: &str,
        spawn_epoch: u64,
        provider: &str,
    ) {
        crate::state::persist::save_runtime_state(
            dir,
            &serde_json::json!({
                "active_team_key": "team",
                "agents": {
                    "w1": {
                        "provider": provider,
                        "status": "running",
                        "agent_id": "w1",
                        "session_id": "session-w1",
                        "rollout_path": rollout.to_string_lossy(),
                        "spawn_cwd": dir.to_string_lossy(),
                        "spawn_epoch": spawn_epoch,
                        "process_liveness": liveness
                    }
                }
            }),
        )
        .unwrap();
    }

    fn claude_assistant_api_error_line(
        uuid: &str,
        parent_uuid: &str,
        session_id: &str,
        status: i64,
        error: &str,
        request_id: Option<&str>,
    ) -> String {
        let mut record = serde_json::json!({
            "type": "assistant",
            "parentUuid": parent_uuid,
            "uuid": uuid,
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "API Error"}]
            },
            "error": error,
            "isApiErrorMessage": true,
            "apiErrorStatus": status,
            "sessionId": session_id,
            "version": "2.1.181"
        });
        if let Some(request_id) = request_id {
            record
                .as_object_mut()
                .unwrap()
                .insert("requestId".to_string(), serde_json::json!(request_id));
        }
        format!("{record}\n")
    }

    fn claude_turn_duration_line(uuid: &str, parent_uuid: &str, session_id: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "system",
                "subtype": "turn_duration",
                "uuid": uuid,
                "parentUuid": parent_uuid,
                "sessionId": session_id
            })
        )
    }

    fn claude_file_history_snapshot_line(session_id: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "file-history-snapshot",
                "sessionId": session_id
            })
        )
    }

    fn claude_last_prompt_line(session_id: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "last-prompt",
                "sessionId": session_id,
                "prompt": "sanitized"
            })
        )
    }

    fn claude_mode_line(session_id: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "mode",
                "sessionId": session_id,
                "mode": "default"
            })
        )
    }

    fn abnormal_test_watch(dir: &std::path::Path) -> Value {
        crate::state::persist::load_runtime_state(dir)
            .unwrap()
            .pointer("/coordinator/abnormal_exit_watch/w1")
            .cloned()
            .unwrap()
    }

    fn read_test_events(dir: &std::path::Path) -> Vec<Value> {
        let events_path = crate::model::paths::logs_dir(dir).join("events.jsonl");
        std::fs::read_to_string(events_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn find_event(events: &[Value], name: &str) -> Option<Value> {
        events
            .iter()
            .rev()
            .find(|event| event.get("event").and_then(Value::as_str) == Some(name))
            .cloned()
    }

    fn temp_abnormal_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "team-agent-abnormal-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_abnormal_agent(
        rollout_path: &str,
        spawn_epoch: Option<u64>,
        spawned_at: Option<&str>,
    ) -> AbnormalWatchAgent {
        AbnormalWatchAgent {
            agent_id: "w1".to_string(),
            provider: crate::provider::Provider::Codex,
            rollout_path: PathBuf::from(rollout_path),
            rollout_path_display: rollout_path.to_string(),
            spawn_epoch,
            spawned_at: spawned_at.map(str::to_string),
            status: Some("running".to_string()),
            process_liveness: Some(ProcessLiveness::Alive),
            window: None,
            pane_id: None,
            pid: None,
            current_command: None,
        }
    }
}
