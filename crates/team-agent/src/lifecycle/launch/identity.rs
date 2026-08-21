//! ---
//! purpose: team 与席位的身份取值，团队键、display 后端、auth_mode、effort 与 quick-start 层级判定
//! contract:
//!   provides:
//!     - name: spec_team_id
//!       what: 从 spec 里取 team 标识
//!     - name: runtime_team_key_for_spec
//!       what: 由 spec 路径与 session 名算出 runtime 团队键
//!     - name: transport_has_session
//!       what: 探测 tmux session 是否存在，探测本身出错一律当不存在
//!     - name: provider_effort_from_raw
//!       what: 解析 effort 并要求该 provider 支持，否则 None
//!     - name: quick_start_depth_guard
//!       what: 定出本次 quick-start 的父团队与层级，推不出父团队时拒绝
//!     - name: annotate_persisted_team_depth
//!       what: 把父团队与层级写回已落盘的 runtime state
//!   depends:
//!     - crate::state::projection
//!     - crate::state::persist
//!     - crate::state::repository
//!     - crate::lifecycle::display
//!     - crate::provider::wire
//! boundary:
//!   - 只做身份判定与取值，不 spawn、不开显示
//!   - effort 不被支持时只丢弃并给出事件载荷，不改写成别的档位
//!   - 层级判定拿不准时报错，不默认当顶层团队
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

/// ---
/// purpose: 从 spec 里取 team 标识
/// params:
///   spec: 已解析的 spec
/// returns: team.id 优先，其次 team.name，再次顶层 name；都没有则 None
/// ---
pub(super) fn spec_team_id(spec: &Value) -> Option<String> {
    spec.get("team")
        .and_then(|v| v.get("id").or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.get("name").and_then(Value::as_str).map(str::to_string))
}

/// ---
/// purpose: 取 runtime state 里显式记录的当前活跃团队键
/// returns: 非空的 active_team_key，缺失或为空则 None
/// ---
pub(super) fn explicit_active_team_key(state: &serde_json::Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|team| !team.is_empty())
        .map(str::to_string)
}

/// ---
/// purpose: 由 spec 路径、spec 内容与 session 名拼出 runtime 团队键
/// returns: 交给 state::projection 计算出的团队键
/// ---
pub(super) fn runtime_team_key_for_spec(
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
) -> String {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let state = serde_json::json!({
        "team_dir": team_dir.to_string_lossy(),
        "spec_path": spec_path.to_string_lossy(),
        "session_name": session_name.as_str(),
        "team": spec.get("team").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null),
    });
    crate::state::projection::team_state_key(&state)
}

/// ---
/// purpose: 探测某 tmux session 是否存在
/// returns: 只有确定存在才返回 true；transport 报错或 panic 一律当作不存在
/// ---
pub(super) fn transport_has_session(transport: &dyn Transport, session_name: &SessionName) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) | Err(_) => false,
    }
}

/// ---
/// purpose: 取 spec 请求的显示后端并交给 display 解析
/// returns: 解析后的后端，spec 未写或写了未知值时为默认 adaptive
/// ---
pub(super) fn spec_display_backend(spec: &Value) -> DisplayBackend {
    let requested = spec
        .get("runtime")
        .and_then(|runtime| runtime.get("display_backend"))
        .and_then(Value::as_str)
        .and_then(|backend| {
            serde_json::from_value::<DisplayBackend>(serde_json::json!(backend)).ok()
        });
    crate::lifecycle::display::resolve_display_backend(requested, None).backend
}

use crate::provider::wire::parse_provider;

/// ---
/// purpose: 把 auth_mode 字符串解析成枚举
/// params:
///   raw: 只接受 subscription、official_api、compatible_api
/// returns: 对应枚举，未知取值返回 None
/// ---
pub(super) fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

/// ---
/// purpose: 从原始字符串解析 effort，并要求当前 provider 支持它
/// params:
///   raw: effort 原文，None 或空白返回 None
/// returns: 解析成功且该 provider 支持时返回该档位，否则 None
/// ---
/// 0.4.x provider effort MVP step 4: low-level from a raw string. Returns
/// `Some(effort)` when the level parses AND the provider supports it.
pub(crate) fn provider_effort_from_raw(
    raw: Option<&str>,
    provider: Provider,
) -> Option<ProviderEffort> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let effort = ProviderEffort::parse(raw)?;
    if effort.is_supported_by(provider) {
        Some(effort)
    } else {
        None
    }
}

/// ---
/// purpose: 当 spec 请求了该 provider 不支持的 effort 时，给出告警事件载荷
/// returns: 含 agent_id、provider、effort 与 ignored 动作的 JSON；未请求或本就支持时为 None
/// ---
/// 0.4.x provider effort MVP step 7: warning event payload when the spec
/// requested an effort level the provider does not support.
pub(crate) fn provider_effort_event_payload(
    raw: Option<&str>,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let effort = ProviderEffort::parse(raw)?;
    if effort.is_supported_by(provider) {
        return None;
    }
    Some(serde_json::json!({
        "agent_id": agent_id,
        "provider": format!("{provider:?}").to_lowercase(),
        "effort": effort.as_str(),
        "action": "ignored",
        "reason": "provider does not support effort",
    }))
}

/// ---
/// purpose: 对 claude 系 provider 兜底确保 CLAUDE_EFFORT 进入待清除环境变量表
/// params:
///   base: 已有的待清除变量名列表
/// returns: 非 claude 系原样返回；claude 系在缺失时补上 CLAUDE_EFFORT
/// ---
/// 0.4.x provider effort MVP step 9: defensive guarantee that `CLAUDE_EFFORT`
/// is unset in the Claude/ClaudeCode worker spawn env. As of the
/// `profile_launch::provider_env_unsets` update, the base list already
/// includes `CLAUDE_EFFORT` for Claude — so this function is idempotent
/// (returns input unchanged). Kept as a belt-and-braces guard so a future
/// refactor that bypasses provider_env_unsets cannot silently drop the
/// scrub. The structural win is in `tmux_backend::shell_command` which now
/// filters env exports by env_unset (preventing inherited values from
/// re-introducing keys we just unset).
pub(crate) fn extend_worker_env_unset_for_effort(
    base: Vec<String>,
    provider: Provider,
) -> Vec<String> {
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return base;
    }
    let mut out = base;
    if !out.iter().any(|k| k == "CLAUDE_EFFORT") {
        out.push("CLAUDE_EFFORT".to_string());
    }
    out
}

/// ---
/// purpose: 从 YAML agent 节点解析出可用的 effort
/// returns: 该 provider 支持的 effort，否则 None
/// contract_id: lifecycle.identity.effort_for_spawn
/// ---
/// Convenience: resolve effort for a yaml::Value agent (spec / compiled).
pub(crate) fn provider_effort_for_spawn(
    agent: &crate::model::yaml::Value,
    provider: Provider,
) -> Option<ProviderEffort> {
    provider_effort_from_raw(agent.get("effort").and_then(|v| v.as_str()), provider)
}

/// ---
/// purpose: 从 YAML agent 节点取 effort，若被丢弃则给出告警事件载荷
/// returns: 告警载荷或 None
/// contract_id: lifecycle.identity.effort_event_if_dropped
/// ---
pub(crate) fn provider_effort_event_if_dropped(
    agent: &crate::model::yaml::Value,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    provider_effort_event_payload(
        agent.get("effort").and_then(|v| v.as_str()),
        provider,
        agent_id,
    )
}

/// ---
/// purpose: 从 runtime state 的 JSON agent 节点解析出可用的 effort
/// returns: 该 provider 支持的 effort，否则 None
/// contract_id: lifecycle.identity.effort_for_spawn
/// ---
/// Same as [`provider_effort_for_spawn`] but for serde_json state values
/// (used by restart paths reading from `state.agents[id]`).
pub(crate) fn provider_effort_for_spawn_json(
    agent: &serde_json::Value,
    provider: Provider,
) -> Option<ProviderEffort> {
    provider_effort_from_raw(
        agent.get("effort").and_then(serde_json::Value::as_str),
        provider,
    )
}

/// ---
/// purpose: 从 JSON agent 节点取 effort，若被丢弃则给出告警事件载荷
/// returns: 告警载荷或 None
/// contract_id: lifecycle.identity.effort_event_if_dropped
/// ---
pub(crate) fn provider_effort_event_if_dropped_json(
    agent: &serde_json::Value,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    provider_effort_event_payload(
        agent.get("effort").and_then(serde_json::Value::as_str),
        provider,
        agent_id,
    )
}

/// ---
/// purpose: 定出 quick-start 本次请求的团队键
/// returns: team_id 优先于 name 的第一个非空值
/// ---
pub(super) fn quick_start_requested_team_key<'a>(
    team_id: Option<&'a str>,
    name: Option<&'a str>,
) -> Option<&'a str> {
    team_id.or(name).filter(|team| !team.is_empty())
}

pub(super) struct QuickStartDepth {
    pub(super) parent_team_key: Option<String>,
    pub(super) team_depth: u64,
}

/// ---
/// purpose: 定出本次 quick-start 的父团队键与团队层级
/// params:
///   requested_team: 请求的团队键，用于识别 child 或 grandchild 这类嵌套意图
///   _agents_dir: 未参与判定
///   _strict_real_runtime: 未参与判定
/// returns: 父团队键与层级；无父上下文时父键为 None、层级为 1
/// errors: 已有活跃团队且请求键像嵌套意图但推不出父团队时返回 RequirementUnmet
/// ---
pub(super) fn quick_start_depth_guard(
    workspace: &Path,
    _agents_dir: &Path,
    requested_team: Option<&str>,
    _strict_real_runtime: bool,
) -> Result<QuickStartDepth, LifecycleError> {
    let env_parent = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let parent = env_parent;
    let Some(parent) = parent else {
        let state = crate::state::persist::load_runtime_state(workspace)
            .unwrap_or_else(|_| serde_json::json!({}));
        let ambiguous_nested_intent = requested_team.is_some_and(|team| {
            looks_ambiguous_child_team_key(team) || looks_grandchild_team_key(team)
        });
        if has_live_runtime_teams(&state) && ambiguous_nested_intent {
            if requested_team.is_some_and(looks_grandchild_team_key) {
                if let Some(parent_key) = infer_parent_team_from_active_state(&state) {
                    let parent_state =
                        crate::state::projection::project_top_level_view(&state, &parent_key);
                    let parent_depth = parent_state
                        .get("team_depth")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1);
                    return Ok(QuickStartDepth {
                        parent_team_key: Some(parent_key),
                        team_depth: parent_depth.saturating_add(1),
                    });
                }
            }
            return Err(LifecycleError::RequirementUnmet(
                "cannot infer parent team for nested quick-start; pass an explicit worker/subleader owner context"
                    .to_string(),
            ));
        }
        return Ok(QuickStartDepth {
            parent_team_key: None,
            team_depth: 1,
        });
    };
    let state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let parent_key = crate::state::projection::resolve_owner_team_id(&state, &parent)
        .canonical_key()
        .map(str::to_string)
        .unwrap_or(parent);
    let parent_state = crate::state::projection::project_top_level_view(&state, &parent_key);
    let parent_depth = parent_state
        .get("team_depth")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let team_depth = parent_depth.saturating_add(1);
    Ok(QuickStartDepth {
        parent_team_key: Some(parent_key),
        team_depth,
    })
}

/// ---
/// purpose: 从 runtime state 推断父团队键
/// returns: 活跃团队键，且该团队确有 running 席位时返回它，否则 None
/// ---
pub(super) fn infer_parent_team_from_active_state(state: &serde_json::Value) -> Option<String> {
    let active = explicit_active_team_key(state)?;
    let team = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(&active))?;
    team_has_running_agent(team).then_some(active)
}

/// ---
/// purpose: 判断 state 里是否存在带 running 席位的团队
/// returns: 任一团队有 running 席位即 true
/// ---
pub(super) fn has_live_runtime_teams(state: &serde_json::Value) -> bool {
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.values().any(team_has_running_agent))
}

/// ---
/// purpose: 判断某团队节点里是否有状态为 running 的席位
/// returns: 有则 true
/// ---
pub(super) fn team_has_running_agent(team: &serde_json::Value) -> bool {
    team.get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                agent.get("status").and_then(serde_json::Value::as_str) == Some("running")
            })
        })
}

/// ---
/// purpose: 判断团队键是否像有歧义的子团队名
/// params:
///   team: 团队键，比较前去空白并转小写
/// returns: 以 child 开头但不等于 child 时为 true
/// ---
pub(super) fn looks_ambiguous_child_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team != "child"
        && (team.starts_with("child-")
            || team.starts_with("child_")
            || team.starts_with("child.")
            || team.starts_with("child"))
}

/// ---
/// purpose: 判断团队键是否像孙团队名
/// returns: 等于 grandchild 或以 grandchild 开头时为 true
/// ---
pub(super) fn looks_grandchild_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team == "grandchild"
        || team.starts_with("grandchild-")
        || team.starts_with("grandchild_")
        || team.starts_with("grandchild.")
        || team.starts_with("grandchild")
}

/// ---
/// purpose: 就地给团队节点写上 team_depth 与可选的 parent_team_key
/// params:
///   state: 待改写的团队节点，非对象时不动
///   parent_team_key: 非空时才写入
/// ---
pub(super) fn annotate_team_depth(
    state: &mut serde_json::Value,
    parent_team_key: Option<&str>,
    team_depth: u64,
) {
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    obj.insert("team_depth".to_string(), serde_json::json!(team_depth));
    if let Some(parent) = parent_team_key.filter(|value| !value.is_empty()) {
        obj.insert("parent_team_key".to_string(), serde_json::json!(parent));
    }
}

/// ---
/// purpose: 读出 runtime state，给指定团队写上层级与父键后存回
/// returns: 成功返回空值；state 里没有该团队时直接返回成功且不写盘
/// errors: 读或写 runtime state 失败时返回 StatePersist
/// ---
pub(super) fn annotate_persisted_team_depth(
    workspace: &Path,
    team_key: &str,
    parent_team_key: Option<&str>,
    team_depth: u64,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let Some(team) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_key))
    else {
        return Ok(());
    };
    annotate_team_depth(team, parent_team_key, team_depth);
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::AnnotateTeamDepth { team_key },
            &state,
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

/// ---
/// purpose: 判断 runtime state 里是否已存在该 quick-start 团队
/// returns: 活跃键、teams 表、投影出的团队键、身份字段或 session 名任一命中即 true
/// ---
pub(super) fn runtime_state_has_quick_start_team(state: &serde_json::Value, team: &str) -> bool {
    explicit_active_team_key(state).as_deref() == Some(team)
        || state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                teams.contains_key(team)
                    || teams
                        .values()
                        .any(|entry| json_team_identity_matches(entry, team))
            })
        || crate::state::projection::team_state_key(state) == team
        || json_team_identity_matches(state, team)
        || state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session == team || session.strip_prefix("team-") == Some(team))
}

/// ---
/// purpose: 判断某 JSON 节点的团队身份字段是否等于给定团队键
/// returns: team.id、team.name 或顶层 name 命中即 true
/// ---
pub(super) fn json_team_identity_matches(state: &serde_json::Value, team: &str) -> bool {
    state
        .get("team")
        .and_then(|value| value.get("id").or_else(|| value.get("name")))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == team)
        || state
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == team)
}
