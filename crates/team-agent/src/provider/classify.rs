//! JSONL→neutral turn-state 翻译 + idle take-over predicate(`provider_state.common` +
//! `idle_predicate` 的 Rust 等价)。leader 经注入的 TurnStateClassifier 调到这里。

use super::helpers::parse_jsonl_records;
use super::types::{
    ClassifyResult, ClassifySource, FactKind, FaultFact, NoPingReason, ProcessLiveness, ProviderError,
    RemindResult, Signature, TurnId, TurnState,
};
use super::Provider;

/// `provider_state.read_turn_state(provider, session_log_text, process, file_silence_seconds)`
/// (`__init__.py:18` + `common.classify_with_reader`/`decide_state`)。
///
/// 流程:`parse_jsonl` → per-provider `extract_facts`(claude/codex)→ `decide_state`。
/// **归一**:`claude_code` → `claude` reader(`__init__.py:88` `_reader_provider`)。
/// **铁律**:不可读/空/无 lifecycle / 进程不可验 → `TurnState::Unknown`,**绝不 idle**(C5)。
/// `file_silence_seconds` 被显式丢弃——open turn 永不被静默 demote(C14,`__init__.py:32`)。
pub fn classify(
    provider: Provider,
    session_log_text: &str,
    process: ProcessLiveness,
    file_silence_seconds: f64,
) -> Result<ClassifyResult, ProviderError> {
    let _ = file_silence_seconds;
    let records = parse_jsonl_records(session_log_text);
    if records.is_empty() {
        return Ok(classify_result(
            TurnState::Unknown,
            None,
            "unreadable_or_empty",
            ClassifySource::SessionFile,
            Vec::new(),
        ));
    }
    let facts = extract_lifecycle_facts(provider, &records);
    // Blocker-2 Layer-2 (prerelease 0.4.0): Claude background-task lifecycle.
    // Claude's long-running work (e.g. `Bash run_in_background:true`,
    // followed by `task-notification status=completed`) can outlive the
    // assistant turn that started it. The existing lifecycle-fact scan
    // recognises `assistant.stop_reason=end_turn` as TurnComplete, so the
    // classifier would otherwise mark the worker idle while a background
    // shell continues. Synthesize a Working fact when at least one
    // background task started but has not yet been closed. Architect
    // verdict: bugs-prerelease-blockers.md §141-§143.
    if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        if let Some(open_turn_id) = claude_background_task_open(&records) {
            return Ok(decide_state(
                &lifecycle(
                    FactKind::TurnOpen,
                    open_turn_id,
                    "background_task",
                    vec!["background_task".to_string()],
                ),
                process,
            ));
        }
    }
    let Some(fact) = facts.last() else {
        return Ok(classify_result(
            TurnState::Unknown,
            None,
            "no_turn_lifecycle_fact",
            ClassifySource::SessionFile,
            Vec::new(),
        ));
    };
    Ok(decide_state(fact, process))
}

/// Blocker-2 Layer-2: scan Claude transcript records for background task
/// lifecycle markers. Returns `Some(turn_id)` when at least one background
/// task is OPEN (started but not closed) — caller treats as `TurnOpen` with
/// reason `background_task`. Returns `None` when no background task started,
/// or every started task has a matching close (completed / failed).
///
/// Open markers (set):
///   * Any record nested string containing
///     "Command running in background with ID: <id>" (Bash tool output).
///   * `tool_use_result.backgroundTaskId` (the structured form).
///
/// Close markers (clear):
///   * `task-notification` content with `<task-id>` matching open id and
///     `<status>completed</status>` or `failed`.
///   * `tool_use_result.bashOutput.status = "completed" | "failed"` for the
///     matching backgroundTaskId.
fn claude_background_task_open(records: &[serde_json::Value]) -> Option<Option<TurnId>> {
    use std::collections::BTreeSet;
    let mut open: BTreeSet<String> = BTreeSet::new();
    let mut last_open_request: Option<String> = None;
    for record in records {
        let mut found_open_ids: Vec<String> = Vec::new();
        collect_background_open_ids(record, &mut found_open_ids);
        for id in &found_open_ids {
            open.insert(id.clone());
        }
        if !found_open_ids.is_empty() {
            if let Some(req) = record.get("requestId").and_then(serde_json::Value::as_str) {
                last_open_request = Some(req.to_string());
            }
        }
        let mut closed_ids: Vec<String> = Vec::new();
        collect_background_close_ids(record, &mut closed_ids);
        for id in &closed_ids {
            open.remove(id);
        }
    }
    if open.is_empty() {
        None
    } else {
        Some(last_open_request.map(TurnId::new))
    }
}

fn collect_background_open_ids(value: &serde_json::Value, out: &mut Vec<String>) {
    const MARKER: &str = "Command running in background with ID: ";
    match value {
        serde_json::Value::String(s) => {
            for piece in s.split(MARKER).skip(1) {
                let id: String = piece.chars().take_while(|c| !c.is_whitespace() && *c != '\n').collect();
                if !id.is_empty() {
                    out.push(id);
                }
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(id) = map.get("backgroundTaskId").and_then(serde_json::Value::as_str) {
                if !id.is_empty() {
                    out.push(id.to_string());
                }
            }
            for v in map.values() {
                collect_background_open_ids(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_background_open_ids(v, out);
            }
        }
        _ => {}
    }
}

fn collect_background_close_ids(value: &serde_json::Value, out: &mut Vec<String>) {
    // Form A: tool_use_result.bashOutput { backgroundTaskId, status } where status is
    //   "completed" or "failed".
    // Form B: <task-notification> wrapper in a string with a status tag.
    if let serde_json::Value::Object(map) = value {
        if let Some(bash) = map.get("bashOutput").and_then(serde_json::Value::as_object) {
            let status = bash.get("status").and_then(serde_json::Value::as_str).unwrap_or("");
            if matches!(status, "completed" | "failed" | "killed") {
                if let Some(id) = bash.get("backgroundTaskId").and_then(serde_json::Value::as_str) {
                    if !id.is_empty() {
                        out.push(id.to_string());
                    }
                }
            }
        }
        for v in map.values() {
            collect_background_close_ids(v, out);
        }
    } else if let serde_json::Value::Array(arr) = value {
        for v in arr {
            collect_background_close_ids(v, out);
        }
    } else if let serde_json::Value::String(s) = value {
        // Form B: <task-notification ...><task-id>X</task-id>...<status>completed</status>...
        if s.contains("<task-notification") && s.contains("<status>completed</status>") {
            if let Some(id) = extract_xml_tag(s, "task-id") {
                if !id.is_empty() {
                    out.push(id);
                }
            }
        }
    }
}

fn extract_xml_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end_rel = text[start..].find(&close)?;
    Some(text[start..start + end_rel].trim().to_string())
}

pub fn latest_explicit_error_fact(provider: Provider, session_log_text: &str) -> Option<FaultFact> {
    let record = latest_jsonl_record(session_log_text)?;
    match provider {
        Provider::Codex => codex_latest_explicit_error_fact(&record),
        Provider::Claude | Provider::ClaudeCode => claude_latest_explicit_error_fact(&record),
        // C-3-5 cr verdict: copilot N35 通知不依赖 turn-state 分类;一期 Unknown,
        // 与 GeminiCli/Fake 同精神。二期接 sqlite turns 表后再回填。
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => None,
    }
}

/// `idle_predicate.evaluate_takeover_reminder(nodes, monitor_state, now, debounce)`
/// (`idle_predicate.py:20`)。中性 take-over reminder 判定。
///
/// 规则:仅 worker 的 delegated 态 arm(C1);任何非 `{idle, idle_interrupted}` node
/// 立刻 block 并报 `Node(state)`(C5/C14);全 idle + armed + debounce 到期 → ping(C2/C11);
/// `idle_interrupted` 算 idle 但进 `interrupted_nodes`(C12)。
///
/// 节点输入用 `serde_json::Value`(承载 `{node_id, role, state}` dict 形态),与 Python
/// 一致;`monitor_state` 同。porter 在 impl 时换成 typed `IdleNode`/`MonitorState`。
pub fn evaluate_takeover_reminder(
    nodes: &[serde_json::Value],
    monitor_state: Option<&serde_json::Value>,
    now_monotonic: f64,
    debounce_seconds: f64,
) -> Result<RemindResult, ProviderError> {
    if nodes.is_empty() {
        return Ok(remind(false, NoPingReason::NoNodes, Vec::new(), None));
    }

    let mut interrupted = Vec::new();
    for node in nodes {
        let state = node_state(node);
        match state {
            TurnState::Idle => {}
            TurnState::IdleInterrupted => {
                if let Some(id) = node.get("node_id").and_then(serde_json::Value::as_str) {
                    interrupted.push(id.to_string());
                }
            }
            TurnState::Working => {
                return Ok(remind(false, NoPingReason::Node(TurnState::Working), Vec::new(), None));
            }
            TurnState::BlockedOnHuman => {
                return Ok(remind(false, NoPingReason::Node(TurnState::BlockedOnHuman), Vec::new(), None));
            }
            TurnState::Abnormal => {
                return Ok(remind(false, NoPingReason::Node(TurnState::Abnormal), Vec::new(), None));
            }
            TurnState::Unknown => {
                return Ok(remind(false, NoPingReason::Node(TurnState::Unknown), Vec::new(), None));
            }
        }
    }

    let Some(ms) = monitor_state else {
        return Ok(remind(false, NoPingReason::NotArmedNoWorkerTurn, Vec::new(), None));
    };
    if ms.get("opened_worker_turn_since_ack").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(remind(false, NoPingReason::NotArmedNoWorkerTurn, Vec::new(), None));
    }
    if ms.get("suppressed").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(remind(false, NoPingReason::Acknowledged, Vec::new(), None));
    }
    let Some(all_idle_since) = ms.get("all_idle_since").and_then(serde_json::Value::as_f64) else {
        return Ok(remind(false, NoPingReason::NotArmedNoWorkerTurn, Vec::new(), None));
    };
    if ms.get("pinged_for_episode").and_then(serde_json::Value::as_f64) == Some(all_idle_since) {
        return Ok(remind(false, NoPingReason::AlreadyPingedThisEpisode, Vec::new(), None));
    }
    if now_monotonic - all_idle_since < debounce_seconds {
        return Ok(remind(false, NoPingReason::DebounceActive, Vec::new(), None));
    }
    Ok(remind(
        true,
        NoPingReason::AllIdleDebounceElapsed,
        interrupted,
        Some("All worker turns are idle.".to_string()),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LifecycleFact {
    kind: FactKind,
    turn_id: Option<TurnId>,
    reason: String,
    annotations: Vec<String>,
}

fn classify_result(
    state: TurnState,
    turn_id: Option<TurnId>,
    reason: &str,
    source: ClassifySource,
    annotations: Vec<String>,
) -> ClassifyResult {
    ClassifyResult {
        state,
        turn_id,
        reason: reason.to_string(),
        source,
        annotations,
        diagnostics: Vec::new(),
    }
}

fn extract_lifecycle_facts(provider: Provider, records: &[serde_json::Value]) -> Vec<LifecycleFact> {
    records
        .iter()
        .filter_map(|record| match provider {
            Provider::Claude | Provider::ClaudeCode => claude_lifecycle_fact(record),
            Provider::Codex => codex_lifecycle_fact(record),
            // C-3-1 cr verdict: copilot lifecycle facts 一期不导出(classify→None,
            // Unknown);二期读 sqlite turns 表(turn_index/assistant_response)。
            Provider::Copilot | Provider::GeminiCli | Provider::Fake => None,
        })
        .collect()
}

fn latest_jsonl_record(text: &str) -> Option<serde_json::Value> {
    let last = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    serde_json::from_str::<serde_json::Value>(last).ok()
}

fn codex_latest_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    let method = record.get("method").and_then(serde_json::Value::as_str);
    if method.is_some_and(|method| method.ends_with("requestApproval")) {
        return None;
    }
    if method != Some("turn/completed") {
        return None;
    }
    let turn = record.get("params").and_then(|p| p.get("turn"))?;
    if turn.get("status").and_then(serde_json::Value::as_str) != Some("failed") {
        return None;
    }
    Some(FaultFact::new(
        Signature::new("turn_failed"),
        turn.get("id").and_then(serde_json::Value::as_str).map(TurnId::new),
        FactKind::Failed,
    ))
}

fn claude_latest_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if super::faults::claude_record_has_error_tool_result(record) {
        return None;
    }
    super::faults::claude_explicit_error_fact(record)
}

fn decide_state(fact: &LifecycleFact, process: ProcessLiveness) -> ClassifyResult {
    match fact.kind {
        FactKind::TurnOpen => match process {
            ProcessLiveness::Alive => classify_result(
                TurnState::Working,
                fact.turn_id.clone(),
                if fact.reason == "assistant_in_flight" {
                    &fact.reason
                } else {
                    "open_turn"
                },
                ClassifySource::SessionFile,
                Vec::new(),
            ),
            ProcessLiveness::Dead => classify_result(
                TurnState::Abnormal,
                fact.turn_id.clone(),
                "crashed_mid_turn",
                ClassifySource::ProcessGuard,
                vec!["crashed_mid_turn".to_string()],
            ),
            ProcessLiveness::Unverifiable => classify_result(
                TurnState::Unknown,
                fact.turn_id.clone(),
                "process_identity_unverified",
                ClassifySource::ProcessGuard,
                Vec::new(),
            ),
        },
        FactKind::TurnComplete => classify_result(
            TurnState::Idle,
            fact.turn_id.clone(),
            &fact.reason,
            ClassifySource::SessionFile,
            fact.annotations.clone(),
        ),
        FactKind::Interrupted => classify_result(
            TurnState::IdleInterrupted,
            fact.turn_id.clone(),
            &fact.reason,
            ClassifySource::SessionFile,
            fact.annotations.clone(),
        ),
        FactKind::Failed | FactKind::Error => classify_result(
            TurnState::Abnormal,
            fact.turn_id.clone(),
            &fact.reason,
            ClassifySource::SessionFile,
            fact.annotations.clone(),
        ),
        FactKind::Approval => classify_result(
            TurnState::BlockedOnHuman,
            fact.turn_id.clone(),
            &fact.reason,
            ClassifySource::SessionFile,
            fact.annotations.clone(),
        ),
    }
}

fn claude_lifecycle_fact(record: &serde_json::Value) -> Option<LifecycleFact> {
    let record_type = record.get("type").and_then(serde_json::Value::as_str);
    if record_type == Some("system")
        && record.get("subtype").and_then(serde_json::Value::as_str) == Some("api_error")
        && record.get("level").and_then(serde_json::Value::as_str) == Some("error")
    {
        return Some(lifecycle(
            FactKind::Error,
            record
                .get("sessionId")
                .or_else(|| record.get("parentUuid"))
                .or_else(|| record.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            "api_error",
            vec!["api_error".to_string()],
        ));
    }
    if record_type == Some("user") && super::faults::claude_record_has_error_tool_result(record) {
        return Some(lifecycle(
            FactKind::Error,
            record
                .get("parentUuid")
                .or_else(|| record.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            "tool_result_is_error",
            vec!["tool_result_is_error".to_string()],
        ));
    }
    if record_type == Some("assistant") {
        let turn_id = record.get("requestId").and_then(serde_json::Value::as_str).map(TurnId::new);
        let Some(stop_reason) = record
            .get("message")
            .and_then(|m| m.get("stop_reason"))
            .and_then(serde_json::Value::as_str) else {
                if record
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(serde_json::Value::as_array)
                    .is_some()
                {
                    return Some(lifecycle(
                        FactKind::TurnOpen,
                        turn_id,
                        "assistant_in_flight",
                        vec!["assistant_in_flight".to_string()],
                    ));
                }
                return None;
            };
        return match stop_reason {
            "tool_use" => Some(lifecycle(FactKind::TurnOpen, turn_id, "open_turn", Vec::new())),
            "end_turn" => Some(lifecycle(FactKind::TurnComplete, turn_id, "end_turn", Vec::new())),
            "stop_sequence" => Some(lifecycle(
                FactKind::TurnComplete,
                turn_id,
                "stop_sequence",
                Vec::new(),
            )),
            _ => None,
        };
    }
    if record_type == Some("user") && claude_user_interrupted(record) {
        return Some(lifecycle(
            FactKind::Interrupted,
            record.get("uuid").and_then(serde_json::Value::as_str).map(TurnId::new),
            "user_interrupt",
            vec!["interrupted".to_string()],
        ));
    }
    None
}

fn codex_lifecycle_fact(record: &serde_json::Value) -> Option<LifecycleFact> {
    if record.get("type").and_then(serde_json::Value::as_str) == Some("event_msg") {
        let payload = record.get("payload")?;
        let payload_type = payload.get("type").and_then(serde_json::Value::as_str)?;
        let turn_id = payload
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .map(TurnId::new);
        return match payload_type {
            "task_started" => Some(lifecycle(
                FactKind::TurnOpen,
                turn_id,
                "task_started",
                Vec::new(),
            )),
            "task_complete" => Some(lifecycle(
                FactKind::TurnComplete,
                turn_id,
                "task_complete",
                Vec::new(),
            )),
            "turn_aborted" => {
                let reason = payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("interrupted");
                Some(lifecycle(
                    FactKind::Interrupted,
                    turn_id,
                    reason,
                    vec!["interrupted".to_string()],
                ))
            }
            _ => None,
        };
    }
    if record.get("jsonrpc").is_some() {
        let method = record.get("method").and_then(serde_json::Value::as_str);
        if method == Some("turn/completed") {
            let turn = record.get("params").and_then(|p| p.get("turn"))?;
            let turn_id = turn
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new);
            return match turn.get("status").and_then(serde_json::Value::as_str) {
                Some("completed") => Some(lifecycle(
                    FactKind::TurnComplete,
                    turn_id,
                    "completed",
                    Vec::new(),
                )),
                Some("interrupted") => Some(lifecycle(
                    FactKind::Interrupted,
                    turn_id,
                    "interrupted",
                    vec!["interrupted".to_string()],
                )),
                Some("inProgress") => Some(lifecycle(
                    FactKind::TurnOpen,
                    turn_id,
                    "open_turn",
                    Vec::new(),
                )),
                Some("failed") => Some(lifecycle(
                    FactKind::Failed,
                    turn_id,
                    "turn_failed",
                    vec!["turn_failed".to_string()],
                )),
                Some(_) | None => None,
            };
        }
        if method.is_some_and(|m| m.ends_with("requestApproval")) {
            return Some(lifecycle(
                FactKind::Approval,
                record
                    .get("params")
                    .and_then(|p| p.get("turnId"))
                    .or_else(|| record.get("params").and_then(|p| p.get("turn_id")))
                    .and_then(serde_json::Value::as_str)
                    .map(TurnId::new),
                "approval_required",
                vec!["awaiting_approval".to_string()],
            ));
        }
    }
    None
}

fn lifecycle(
    kind: FactKind,
    turn_id: Option<TurnId>,
    reason: &str,
    annotations: Vec<String>,
) -> LifecycleFact {
    LifecycleFact {
        kind,
        turn_id,
        reason: reason.to_string(),
        annotations,
    }
}

fn claude_user_interrupted(record: &serde_json::Value) -> bool {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && item
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        == Some("[Request interrupted by user]")
            })
        })
}

fn node_state(node: &serde_json::Value) -> TurnState {
    match node.get("state").and_then(serde_json::Value::as_str) {
        Some("idle") => TurnState::Idle,
        Some("working") => TurnState::Working,
        Some("idle_interrupted") => TurnState::IdleInterrupted,
        Some("blocked_on_human") => TurnState::BlockedOnHuman,
        Some("abnormal") => TurnState::Abnormal,
        Some("unknown") | None => TurnState::Unknown,
        Some(_) => TurnState::Unknown,
    }
}

fn remind(
    should_ping: bool,
    reason: NoPingReason,
    interrupted_nodes: Vec<String>,
    message: Option<String>,
) -> RemindResult {
    RemindResult {
        should_ping,
        reason,
        interrupted_nodes,
        message,
    }
}
