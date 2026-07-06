use super::common::*;
use super::*;
use crate::provider::wire::{parse_canonical_provider, requires_resume_backing};

/// bug-085 四象限 `start_mode` 决策(`start.py:179-188` + `_resume_rollout_missing` `start.py:66-69`),
/// **从 start_agent 的整条 lock+spawn 路径里分离出的纯函数**(gate gap:porter 需要单元级 RED
/// for `FreshAfterMissingRollout`,而 start_agent 全路径不可单测)。语义:
/// - resume backing 缺失时不可 resume:codex/claude 用 transcript/rollout 文件,
///   copilot 用 session-store 行存在性(由调用方折叠进 `rollout_exists`)。
/// - 初始 `start_mode = if session_id { Resumed } else { Fresh }`(`start.py:179`)。
/// - `missing && allow_fresh` 升级为 `FreshAfterMissingRollout` 并清空 session_id。
/// - `missing && !allow_fresh` 返回 `Noop`,调用方据此诚实拒绝并提示 `--allow-fresh`。
pub fn decide_start_mode(
    provider: &str,
    session_id: Option<&SessionId>,
    _rollout_path: Option<&RolloutPath>,
    rollout_exists: bool,
    allow_fresh: bool,
) -> StartMode {
    match session_id {
        None => StartMode::Fresh,
        Some(_) => {
            let missing_resume_backing = !provider_wire_supports_resume(provider)
                || (resumable_provider_requires_backing(provider) && !rollout_exists);
            match (missing_resume_backing, allow_fresh) {
                (true, true) => StartMode::FreshAfterMissingRollout,
                (true, false) => StartMode::Noop,
                (false, _) => StartMode::Resumed,
            }
        }
    }
}

pub(crate) fn resumable_provider_requires_backing(provider: &str) -> bool {
    parse_canonical_provider(provider).is_some_and(requires_resume_backing)
}

/// `first_send_at` 严格分类(`_classify_first_send_at`,`orchestration.py:399`)。
/// **绝不靠 truthiness**:`""`/`0`/`False`/`"null"`/非 ISO → `Corrupt`。
pub fn classify_first_send_at(raw: &serde_json::Value) -> FirstSendAtState {
    match raw {
        serde_json::Value::Null => FirstSendAtState::Absent,
        serde_json::Value::String(s) => {
            if is_python_fromisoformat_like(s) {
                FirstSendAtState::Valid
            } else {
                FirstSendAtState::Corrupt
            }
        }
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => FirstSendAtState::Corrupt,
    }
}

/// RESTART-RESUME-001 (0.4.8): true if the agent has any signal of prior
/// interaction whose context would be lost by silent fresh-start. Checks
/// `first_send_at` (leader→worker delivery), `last_result_at` (worker
/// report_result), and `task_prompt_delivered` (MCP-only worker first task).
/// Used by both the selection-stage never-captured decision and the
/// pre-selection convergence missing-set predicate so the two layers share
/// a single "needs context preservation" semantic.
///
/// CR M2 callers (must stay in sync):
///   * lifecycle/restart/selection.rs (never_captured branch, classify_resume_decision)
///   * lifecycle/restart/common.rs::restart_required_missing_session_agent_ids
pub(crate) fn restart_agent_has_context_to_preserve(agent: &serde_json::Value) -> bool {
    let has_valid_first_send_at = matches!(
        classify_first_send_at(
            agent
                .get("first_send_at")
                .unwrap_or(&serde_json::Value::Null)
        ),
        FirstSendAtState::Valid
    );
    if has_valid_first_send_at {
        return true;
    }
    let has_last_result_at = agent
        .get("last_result_at")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if has_last_result_at {
        return true;
    }
    agent
        .get("task_prompt_delivered")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// RESTART-RESUME-001: an agent is "never-captured" when its session_id is
/// absent AND there is no context to preserve. Such an agent is safe to
/// auto-fresh without `--allow-fresh` and should NOT be required to capture
/// a transcript before restart proceeds.
pub(crate) fn restart_agent_never_captured(
    agent: &serde_json::Value,
    session_id: Option<&str>,
) -> bool {
    let session_present = session_id.is_some_and(|s| !s.is_empty())
        || agent
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty());
    if session_present {
        return false;
    }
    !restart_agent_has_context_to_preserve(agent)
}

fn is_python_fromisoformat_like(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    if chrono::DateTime::parse_from_rfc3339(raw).is_ok()
        || chrono::DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%z").is_ok()
        || chrono::DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f%z").is_ok()
    {
        return true;
    }

    let normalized = normalize_iso_separator(raw);
    for pattern in [
        "%Y-%m-%d",
        "%Y%m%d",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M%z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y%m%dT%H%M%S",
    ] {
        if chrono::NaiveDate::parse_from_str(&normalized, pattern).is_ok()
            || chrono::NaiveDateTime::parse_from_str(&normalized, pattern).is_ok()
            || chrono::DateTime::parse_from_str(&normalized, pattern).is_ok()
        {
            return true;
        }
    }
    false
}

fn normalize_iso_separator(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (idx, ch) in raw.chars().enumerate() {
        if idx == 10 && matches!(ch, ' ' | 't' | '_') {
            out.push('T');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Python `type(value).__name__` 映射(`orchestration.py:446`):corrupt first_send_at
/// 条目的 `raw_first_send_at_type` golden。锁死跨语言一致:`null→"NoneType"`、`""/"x"→"str"`、
/// `0/123→"int"`、`false→"bool"`、`[]→"list"`、`{}→"dict"`、float→`"float"`。
/// **绝不**用 Rust 的 `"null"/"string"/"number"/"boolean"/"array"/"object"`(serde 名)—— 必须是
/// Python 名,否则 audit payload 与真相源不一致。
pub fn python_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "NoneType",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
    }
}

/// Route B 全量验证(纯计算,**无破坏性副作用**;`_emit_resume_decisions` +
/// `_collect_corrupt_first_send_at`,`orchestration.py:430/467`)。读 fixture state 的
/// `agents.<id>`,对每非 paused worker:
/// (1) corrupt first_send_at → 收进 `corrupt_entries`(carry python type-name);
/// (2) 算 resume 决策(`session_id→Resume` / `null session && allow_fresh→FreshStart` /
///     否则 `Refuse`;E6 层2:null session 不再因 first_send_at=null 静默 fresh);
/// (3) `Refuse` 的 worker(reason=`no_persisted_session_id`(无 session)|`session_unresumable`)
///     进 `unresumable`。
/// restart() **先**调它再 teardown;corrupt 非空 → `RefusedInvalidFirstSendAt`,unresumable
/// 非空且 !allow_fresh → `RefusedResumeAtomicity`。**refuse 早于一切 teardown,nothing created**。
pub fn classify_restart_plan(
    state: &serde_json::Value,
    allow_fresh: bool,
) -> Result<RestartPlan, LifecycleError> {
    classify_restart_plan_with_resume_validation(None, state, allow_fresh)
}

pub(crate) fn classify_restart_plan_with_resume_validation(
    workspace: Option<&Path>,
    state: &serde_json::Value,
    allow_fresh: bool,
) -> Result<RestartPlan, LifecycleError> {
    let mut decisions = Vec::new();
    let mut corrupt_entries = Vec::new();
    let mut unresumable = Vec::new();

    let Some(agents) = state.get("agents").and_then(|v| v.as_object()) else {
        return Ok(RestartPlan {
            decisions,
            corrupt_entries,
            unresumable,
        });
    };

    for (worker_id, agent) in agents {
        if agent
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s == "paused")
            .unwrap_or(false)
        {
            continue;
        }

        let first_send_at_raw = agent
            .get("first_send_at")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let first_send_at_state = classify_first_send_at(&first_send_at_raw);
        if matches!(first_send_at_state, FirstSendAtState::Corrupt) {
            corrupt_entries.push(CorruptFirstSendAt {
                worker_id: AgentId::new(worker_id.clone()),
                raw_first_send_at_type: python_type_name(&first_send_at_raw).to_string(),
                raw_first_send_at: first_send_at_raw,
            });
            continue;
        }

        let session_id = agent
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(SessionId::new);
        let agent_id = AgentId::new(worker_id.clone());
        // E6 层2 (C2, 用户裁定"绝不静默 fresh"): null session 只有显式 --allow-fresh 才 fresh,
        // 否则 Refuse(→ resume_not_ready + 指引)。删 `!interacted` 短路 —— 自启动 worker
        // (leader 从未发消息 → first_send_at=null → interacted=false)会被它静默 fresh 丢上下文。
        let provider = agent_provider(agent);
        let provider_wire = provider_wire(provider);
        let provider_can_resume = provider_supports_resume(provider);
        let rollout_path = agent_rollout_path(agent);
        // Layer 2 self-healing (leader follow-up 2026-06-22): use the
        // structured probe so we can carry the list of paths the runtime
        // actually checked into the refusal — operators need to see WHICH
        // places we looked, not just "missing".
        let (resume_backing_exists, backing_checked_paths) =
            match (workspace, session_id.as_ref(), provider_can_resume) {
                (_, Some(_), false) => (false, Vec::new()),
                (Some(workspace), Some(session), true) => {
                    let probe = resume_backing_probe_for_agent(
                        workspace,
                        &agent_id,
                        agent,
                        provider,
                        session,
                        rollout_path.as_ref(),
                    );
                    (probe.exists, probe.checked_paths)
                }
                (None, Some(_), true) if resumable_provider_requires_backing(provider_wire) => {
                    let exists = rollout_path
                        .as_ref()
                        .is_some_and(|path| path.as_path().exists());
                    let checked = rollout_path
                        .as_ref()
                        .map(|p| vec![p.as_path().to_path_buf()])
                        .unwrap_or_default();
                    (exists, checked)
                }
                _ => (true, Vec::new()),
            };
        let identity_probe =
            session_identity_probe_for_agent(&agent_id, provider, rollout_path.as_ref());
        let session_identity_mismatch = session_id.is_some()
            && provider_can_resume
            && resume_backing_exists
            && identity_probe.identity_ok == Some(false);
        // 0.4.7 partial-resume + RESTART-RESUME-001 (0.4.8): when a worker
        // has NEVER been captured (no session_id AND no context-bearing
        // signal at all), it is structurally non-resumable — there is no
        // context to lose, so auto-fresh is safe even without --allow-fresh.
        //
        // The "never_captured" predicate is now the shared
        // restart_agent_never_captured(): session_id absent AND none of
        // first_send_at(Valid) / last_result_at / task_prompt_delivered.
        // This matches the pre-selection convergence semantic in
        // common.rs::restart_required_missing_session_agent_ids so both
        // layers refuse / auto-fresh in unison.
        //
        // If session_id is None but ANY context signal exists, that's the
        // "received message/result but session not captured" bug state —
        // keep the Refuse so we never silently drop context (architect
        // rule: "绝不静默 fresh").
        let _ = first_send_at_state; // retained for the Corrupt branch above
        let never_captured =
            restart_agent_never_captured(agent, session_id.as_ref().map(|s| s.as_str()));
        let decision = if session_id.is_some()
            && provider_can_resume
            && resume_backing_exists
            && !session_identity_mismatch
        {
            ResumeDecision::Resume
        } else if session_id.is_some() && allow_fresh {
            ResumeDecision::FreshStart
        } else if session_id.is_some() {
            ResumeDecision::Refuse
        } else if allow_fresh {
            ResumeDecision::FreshStart
        } else if never_captured {
            // No session_id + never captured → safe to auto-fresh without
            // --allow-fresh. No context to lose.
            ResumeDecision::FreshStart
        } else {
            ResumeDecision::Refuse
        };
        if matches!(decision, ResumeDecision::Refuse) {
            // unit-5: surface structured ResumeRefusalReason alongside the
            // legacy free-form string. The string wire is preserved exactly
            // (round-tripped through ResumeRefusalReason::wire) so the
            // CLI/JSON contract does not change.
            let (reason_str, structured) = if session_id.is_some() {
                if session_identity_mismatch {
                    let session = session_id
                        .as_ref()
                        .map(|session| session.as_str().to_string())
                        .unwrap_or_default();
                    (
                        "session_identity_mismatch".to_string(),
                        crate::provider::session::ResumeRefusalReason::SessionIdentityMismatch {
                            expected_agent_id: agent_id.as_str().to_string(),
                            embedded_agent_id: identity_probe
                                .embedded_agent_id
                                .clone()
                                .unwrap_or_default(),
                            session_id: session,
                            rollout_path: identity_probe.rollout_path.clone(),
                        },
                    )
                } else if !provider_can_resume {
                    (
                        "session_unresumable".to_string(),
                        crate::provider::session::ResumeRefusalReason::ProviderResumeUnsupported {
                            provider: provider_wire.to_string(),
                        },
                    )
                } else if !resume_backing_exists {
                    // Today the legacy wire collapses backing-missing under
                    // the catch-all `session_unresumable` — keep that wire,
                    // but record the structured reason so the new shape is
                    // available to callers that want it.
                    //
                    // Layer 2 self-healing (architect probe 2026-06-22): attach
                    // a recovery hint pointing at the agent_id (used as
                    // launch-time `--name`) and spawn_cwd. Operator-facing
                    // diagnostic only — no auto-resume off the hint.
                    let recovery_hint = Some(crate::provider::session::RecoveryHint {
                        provider_session_name_hint: Some(agent_id.as_str().to_string()),
                        spawn_cwd: agent
                            .get("spawn_cwd")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(std::path::PathBuf::from),
                        provider: provider_wire.to_string(),
                    });
                    (
                        "session_unresumable".to_string(),
                        crate::provider::session::ResumeRefusalReason::SessionBackingStoreMissing {
                            checked_paths: backing_checked_paths.clone(),
                            recovery_hint,
                        },
                    )
                } else {
                    (
                        "session_unresumable".to_string(),
                        crate::provider::session::ResumeRefusalReason::Other {
                            legacy_reason: "session_unresumable".to_string(),
                        },
                    )
                }
            } else {
                (
                    "no_persisted_session_id".to_string(),
                    crate::provider::session::ResumeRefusalReason::NoSessionId,
                )
            };
            unresumable.push(UnresumableWorker {
                agent_id: agent_id.clone(),
                reason: reason_str,
                refusal_reason: Some(structured),
                session_id: session_id.clone(),
                first_send_at: first_send_at_raw.as_str().map(|s| s.to_string()),
            });
        }
        decisions.push(RestartedAgent {
            agent_id,
            restart_mode: match decision {
                ResumeDecision::Resume => StartMode::Resumed,
                ResumeDecision::FreshStart => StartMode::Fresh,
                ResumeDecision::Refuse => StartMode::Noop,
            },
            decision,
            session_id,
        });
    }

    Ok(RestartPlan {
        decisions,
        corrupt_entries,
        unresumable,
    })
}
