use super::*;
use super::common::*;

/// bug-085 四象限 `start_mode` 决策(`start.py:179-188` + `_resume_rollout_missing` `start.py:66-69`),
/// **从 start_agent 的整条 lock+spawn 路径里分离出的纯函数**(gate gap:porter 需要单元级 RED
/// for `FreshAfterMissingRollout`,而 start_agent 全路径不可单测)。语义:
/// - `_resume_rollout_missing` 仅 codex 且有 session_id 时可能 true:`!rollout_path || !exists`。
/// - 初始 `start_mode = if session_id { Resumed } else { Fresh }`(`start.py:179`)。
/// - **仅当** `missing && allow_fresh` 才升级为 `FreshAfterMissingRollout` 并清空 session_id
///   (`start.py:180-190`)。`missing && !allow_fresh` 仍 `Resumed`(随后真实 resume 会 fail)。
/// - 非 codex:rollout 永不"缺失",直接看 session_id。
pub fn decide_start_mode(
    provider: &str,
    session_id: Option<&SessionId>,
    rollout_path: Option<&RolloutPath>,
    rollout_exists: bool,
    allow_fresh: bool,
) -> StartMode {
    match session_id {
        None => StartMode::Fresh,
        Some(_) => {
            let missing_codex_rollout =
                provider == "codex" && (rollout_path.is_none() || !rollout_exists);
            if missing_codex_rollout && allow_fresh {
                StartMode::FreshAfterMissingRollout
            } else {
                StartMode::Resumed
            }
        }
    }
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
/// (2) 算 resume 决策(`resumable→Resume` / `!resumable&&!interacted→FreshStart` /
///     `!resumable&&interacted&&allow_fresh→FreshStart` / 否则 `Refuse`);
/// (3) `Refuse` 的 worker(reason=`no_persisted_session_id`(无 session)|`session_unresumable`)
///     进 `unresumable`。
/// restart() **先**调它再 teardown;corrupt 非空 → `RefusedInvalidFirstSendAt`,unresumable
/// 非空且 !allow_fresh → `RefusedResumeAtomicity`。**refuse 早于一切 teardown,nothing created**。
pub fn classify_restart_plan(
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
        let interacted = matches!(first_send_at_state, FirstSendAtState::Valid);
        let decision = if session_id.is_some() {
            ResumeDecision::Resume
        } else if !interacted || allow_fresh {
            ResumeDecision::FreshStart
        } else {
            ResumeDecision::Refuse
        };
        let agent_id = AgentId::new(worker_id.clone());
        if matches!(decision, ResumeDecision::Refuse) {
            unresumable.push(UnresumableWorker {
                agent_id: agent_id.clone(),
                reason: "no_persisted_session_id".to_string(),
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
