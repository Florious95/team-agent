//! ---
//! purpose: 加席过程中的 state 写入、spec 注入与失败回滚
//! contract:
//!   provides:
//!     - name: rollback_add_agent_atomic
//!       what: 尽力恢复 spec 字节与 runtime state，并写一条回滚事件
//!     - name: upsert_agent_state_from_role
//!       what: 由角色文档 front matter 写出该席位的 state 行
//!     - name: inject_agent_into_spec
//!       what: 把编译出的 agent 注入 spec 的 agents 与 routing 规则
//!     - name: runtime_agent_exists
//!       what: 判断 state 里是否已有同 id 席位
//!   depends:
//!     - crate::state::repository
//!     - crate::state::persist
//!     - crate::lifecycle::restart::remove
//!     - crate::event_log::EventLog
//! boundary:
//!   - 回滚是尽力而为，回滚自身的错误被吞掉，只在事件里如实记录成败
//!   - 注入 spec 不落盘，落盘由调用方原子写
//!   - 已存在同 id 的条目不重复注入
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

/// ---
/// purpose: 加席失败后恢复 spec 与 runtime state，并给新席立墓碑防止被合并回来
/// params:
///   pre_spec_text: 写之前的 spec 字节；None 表示原本没有该文件，回滚即删除
///   pre_runtime_state: 写之前的 runtime state；None 时改为从当前 state 里摘掉该席位
///   reason: 写进回滚事件的原因串
/// returns: 无返回值；回滚成败如实写进 add_agent.rollback 事件
/// ---
/// E42 (0.3.24 P0, double-spec deadlock): best-effort atomic rollback for a
/// failed add-agent. Restores the canonical spec to its pre-write bytes (or
/// removes the file if it didn't exist), and restores runtime state to its
/// pre-write JSON (so the half-written `status:starting` row is gone). The
/// caller propagates the ORIGINAL operation error after rollback; rollback
/// errors are swallowed (best-effort, no panic).
pub(super) fn rollback_add_agent_atomic(
    run_workspace: &Path,
    spec_path: &Path,
    pre_spec_text: Option<&str>,
    pre_runtime_state: Option<&serde_json::Value>,
    agent_id: &AgentId,
    reason: &str,
) {
    let _ = std::fs::remove_dir_all(
        run_workspace
            .join(".team/runtime/copilot-instructions")
            .join(agent_id.as_str()),
    );
    let spec_restored = if let Some(text) = pre_spec_text {
        std::fs::write(spec_path, text).is_ok()
    } else {
        std::fs::remove_file(spec_path)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .is_ok()
    };
    // 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.4):
    // rollback must tombstone the newly-added agent so the persist merge
    // does not re-attach a `roster_stub` from the latest on disk. Without
    // the tombstone the half-added `agents.standards` / `teams.<key>.agents.standards`
    // survives the restore-from-pre_state pass and the retry sees
    // "agent id already exists".
    let state_restored = if let Some(state) = pre_runtime_state {
        crate::state::repository::StateRepository::new(run_workspace)
            .save(
                crate::state::repository::StateWriteIntent::AgentRollback {
                    team_key: None,
                    agent_id: agent_id.as_str(),
                },
                state,
            )
            .is_ok()
    } else {
        // No prior runtime state — drop just the agent we added (load → strip → save).
        if let Ok(mut state) = crate::state::persist::load_runtime_state(run_workspace) {
            if let Some(agents) = state
                .get_mut("agents")
                .and_then(serde_json::Value::as_object_mut)
            {
                agents.remove(agent_id.as_str());
            }
            if let Some(teams) = state
                .get_mut("teams")
                .and_then(serde_json::Value::as_object_mut)
            {
                for team in teams.values_mut() {
                    if let Some(agents) = team
                        .get_mut("agents")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        agents.remove(agent_id.as_str());
                    }
                }
            }
            crate::state::repository::StateRepository::new(run_workspace)
                .save(
                    crate::state::repository::StateWriteIntent::AgentRollback {
                        team_key: None,
                        agent_id: agent_id.as_str(),
                    },
                    &state,
                )
                .is_ok()
        } else {
            false
        }
    };
    let rollback_ok = spec_restored && state_restored;
    let _ = crate::event_log::EventLog::new(run_workspace).write(
        "add_agent.rollback",
        serde_json::json!({
            "agent_id": agent_id.as_str(),
            "reason": reason,
            "rollback_ok": rollback_ok,
            "spec_restored": spec_restored,
            "state_restored": state_restored,
        }),
    );
}

/// ---
/// purpose: 由角色文档的 front matter 写出该席位的 state 行并落盘
/// params:
///   meta: 角色文档 front matter
///   dynamic_role_file: 角色文件路径，记进 state 供 restart 重建 spec 用
/// returns: 成功返回空值，席位状态记为 starting
/// errors: 读或写 runtime state 失败时返回 StatePersist
/// ---
pub(super) fn upsert_agent_state_from_role(
    workspace: &Path,
    canonical_team_key: &str,
    agent_id: &AgentId,
    meta: &Value,
    dynamic_role_file: &Path,
) -> Result<(), LifecycleError> {
    let mut state =
        crate::state::projection::select_runtime_state(workspace, Some(canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    if !state.is_object() {
        state = serde_json::json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state root is not an object".to_string(),
        ));
    };
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    let Some(agent_map) = agents.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state agents is not an object".to_string(),
        ));
    };
    let entry = starting_agent_entry(workspace, agent_id, meta, dynamic_role_file);
    agent_map.insert(agent_id.as_str().to_string(), entry);
    crate::lifecycle::restart::remove::clear_agent_retirement_in_state(&mut state, agent_id);
    save_launched_team_state_for_key(
        workspace,
        &state,
        Some(canonical_team_key),
        Some(agent_id.as_str()),
    )
}

/// ---
/// purpose: 首次登记 Pi fork 席位，原子带入目标角色与完整 fork capture seed
/// returns: 成功后 TARGET 已按 ForkAgent typed intent 登记为 starting
/// errors: 重复席位、无效 capture seed 或 state 写入失败时拒绝
/// ---
pub(crate) fn fork_upsert_agent_state_from_role(
    workspace: &Path,
    canonical_team_key: &str,
    agent_id: &AgentId,
    meta: &Value,
    compiled_agent: &Value,
    dynamic_role_file: &Path,
    seed: &ForkCaptureSeed,
) -> Result<(), LifecycleError> {
    if agent_id == &seed.source_agent_id {
        return Err(LifecycleError::RequirementUnmet(
            "fork target must differ from source".to_string(),
        ));
    }
    if seed.captured.captured_via != crate::provider::CaptureVia::ForkSnapshot
        || seed.captured.attribution_confidence != crate::provider::Confidence::High
        || seed.captured.session_id.is_none()
        || seed.captured.rollout_path.is_none()
        || seed.captured_at.is_empty()
    {
        return Err(LifecycleError::RequirementUnmet(
            "fork capture seed is incomplete or not a verified Pi snapshot".to_string(),
        ));
    }
    if compiled_agent.get("id").and_then(Value::as_str) != Some(agent_id.as_str()) {
        return Err(LifecycleError::Compile(
            "compiled fork role id does not match target".to_string(),
        ));
    }
    let sessions_root = std::fs::canonicalize(&seed.pi_sessions_root)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let backing = seed.captured.rollout_path.as_ref().ok_or_else(|| {
        LifecycleError::RequirementUnmet("fork backing path is missing".to_string())
    })?;
    let backing_path = std::fs::canonicalize(&backing.0)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if !backing_path.starts_with(&sessions_root) {
        return Err(LifecycleError::RequirementUnmet(
            "fork backing is outside the target Pi session root".to_string(),
        ));
    }

    let mut state =
        crate::state::projection::select_runtime_state(workspace, Some(canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    if !state.is_object() {
        state = serde_json::json!({});
    }
    let root = state.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("runtime state root is not an object".to_string())
    })?;
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    let agent_map = agents.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("runtime state agents is not an object".to_string())
    })?;
    if agent_map.contains_key(agent_id.as_str()) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists in selected team: {}",
            agent_id
        )));
    }

    let source_profile_dir = agent_map
        .get(seed.source_agent_id.as_str())
        .and_then(serde_json::Value::as_object)
        .and_then(|source| source.get("_profile_dir"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| serde_json::json!(path));
    let mut entry = starting_agent_entry(workspace, agent_id, meta, dynamic_role_file);
    let entry_object = entry.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("fork target entry is not an object".to_string())
    })?;
    for field in [
        "provider",
        "auth_mode",
        "role",
        "model",
        "effort",
        "profile",
        "tools",
        "system_prompt",
        "output_contract",
        "communication_mode",
        "working_directory",
        "cwd",
        "config",
        "name",
        "dangerously_skip_permissions",
    ] {
        if let Some(value) = compiled_agent.get(field) {
            entry_object.insert(field.to_string(), yaml_value_to_json(value));
        }
    }
    if compiled_agent.get("model").is_some() && !entry_object.contains_key("model_source") {
        entry_object.insert("model_source".to_string(), serde_json::json!("team"));
    }
    entry_object.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    entry_object.insert(
        "owner_team_id".to_string(),
        serde_json::json!(canonical_team_key),
    );
    entry_object.insert(
        "claude_projects_root".to_string(),
        serde_json::json!(sessions_root.to_string_lossy().to_string()),
    );
    entry_object.insert(
        "forked_from".to_string(),
        serde_json::json!(seed.source_agent_id.as_str()),
    );
    entry_object.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(seed.captured.spawn_cwd.to_string_lossy().to_string()),
    );
    entry_object.insert(
        "session_id".to_string(),
        serde_json::json!(seed.captured.session_id.as_ref().map(|id| id.as_str())),
    );
    let backing = seed.captured.rollout_path.as_ref().ok_or_else(|| {
        LifecycleError::RequirementUnmet("fork backing path is missing".to_string())
    })?;
    entry_object.insert(
        "rollout_path".to_string(),
        serde_json::json!(backing.0.to_string_lossy().to_string()),
    );
    entry_object.insert(
        "captured_at".to_string(),
        serde_json::json!(seed.captured_at.as_str()),
    );
    entry_object.insert(
        "captured_via".to_string(),
        serde_json::to_value(seed.captured.captured_via)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?,
    );
    entry_object.insert(
        "attribution_confidence".to_string(),
        serde_json::to_value(seed.captured.attribution_confidence)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?,
    );
    entry_object.insert("capture_state".to_string(), serde_json::json!("captured"));
    if let Some(profile_dir) = source_profile_dir {
        entry_object.insert("_profile_dir".to_string(), profile_dir);
    } else {
        entry_object.insert(
            "_profile_dir".to_string(),
            serde_json::json!(seed.profile_dir.to_string_lossy().to_string()),
        );
    }
    agent_map.insert(agent_id.as_str().to_string(), entry);
    crate::lifecycle::restart::remove::clear_agent_retirement_in_state(&mut state, agent_id);
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::ForkAgent {
                team_key: canonical_team_key,
                agent_id: agent_id.as_str(),
            },
            &state,
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn yaml_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(value) => serde_json::json!(value),
        Value::Int(value) => serde_json::json!(value),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(value) => serde_json::json!(value),
        Value::List(values) => {
            serde_json::Value::Array(values.iter().map(yaml_value_to_json).collect())
        }
        Value::Map(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), yaml_value_to_json(value)))
                .collect(),
        ),
    }
}

fn starting_agent_entry(
    workspace: &Path,
    agent_id: &AgentId,
    meta: &Value,
    dynamic_role_file: &Path,
) -> serde_json::Value {
    let provider = meta
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let auth_mode = meta
        .get("auth_mode")
        .and_then(Value::as_str)
        .unwrap_or("subscription");
    let role = meta
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_else(|| agent_id.as_str());
    let mut entry = serde_json::json!({
        "provider": provider,
        "auth_mode": auth_mode,
        "role": role,
        "status": "starting",
        "dynamic_role_file": dynamic_role_file.to_string_lossy().to_string(),
        "role_source_ownership": role_source_ownership(workspace, dynamic_role_file),
    });
    if let Some(model) = meta.get("model").and_then(Value::as_str) {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("model".to_string(), serde_json::json!(model));
            obj.insert("model_source".to_string(), serde_json::json!("role"));
        }
    }
    if let Some(profile) = meta.get("profile").and_then(Value::as_str) {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("profile".to_string(), serde_json::json!(profile));
            if let Some(team_dir) = dynamic_role_file.parent().and_then(Path::parent) {
                obj.insert(
                    "_profile_dir".to_string(),
                    serde_json::json!(team_dir.join("profiles").to_string_lossy().to_string()),
                );
            }
            if !obj.contains_key("model_source") {
                obj.insert("model_source".to_string(), serde_json::json!("default"));
            }
        }
    }
    if let Some(effort_str) = meta.get("effort").and_then(Value::as_str) {
        if !effort_str.is_empty() {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("effort".to_string(), serde_json::json!(effort_str));
            }
        }
    }
    if let Some(obj) = entry.as_object_mut() {
        let meta_provider = meta
            .get("provider")
            .and_then(Value::as_str)
            .and_then(crate::provider::wire::parse_provider)
            .unwrap_or(Provider::Codex);
        persist_effective_approval_policy_from_yaml_agent(obj, meta, meta_provider);
    }
    entry
}

/// ---
/// purpose: 判断角色文件是框架托管的还是外部的
/// returns: 路径落在托管目录下为 managed，否则为 external
/// ---
pub(super) fn role_source_ownership(workspace: &Path, role_file: &Path) -> &'static str {
    let managed_root = workspace.join(".team").join("dynamic-role-files");
    match (
        std::fs::canonicalize(&managed_root),
        std::fs::canonicalize(role_file),
    ) {
        (Ok(root), Ok(path)) if path.starts_with(&root) => "managed",
        _ => "external",
    }
}

/// ---
/// purpose: 把编译出的 agent 注入 spec 的 agents 列表并补一条同形路由规则
/// params:
///   spec: 就地改写；已有同 id 的 agent 或同目标的路由规则时不重复追加
/// returns: 成功返回空值，不落盘
/// errors: spec 不是 map 或 agents 缺失时返回 Compile
/// ---
/// E5 Bug1:把 add-agent 就地编译出的 agent 条目注入 base team spec(`agents` 列表 +
/// `routing.rules` 加 `route-<id>`),复刻 [`compile_team`] 的路由规则形态。不落任何文件。
///
/// 0.5.30 (`.team/artifacts/add-agent-restart-saveconflict-locate.md` §5.2):
/// `pub(crate)` 让 restart/rebuild.rs::rebuild_runtime_spec_from_roles 复用
/// 同一去重注入逻辑,把 add-agent 记录的 dynamic_role_file 合并回 restart
/// 重建 spec,防止 live helper 被 prune 后触发 SaveConflict。行为不变。
pub(crate) fn inject_agent_into_spec(
    spec: &mut Value,
    agent: Value,
    agent_id: &str,
) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = spec else {
        return Err(LifecycleError::Compile("spec is not a map".to_string()));
    };
    // agents 列表追加。
    match pairs.iter_mut().find(|(k, _)| k == "agents") {
        Some((_, Value::List(agents))) => {
            if !agents
                .iter()
                .any(|existing| yaml_agent_id(existing) == Some(agent_id))
            {
                agents.push(agent);
            }
        }
        _ => {
            return Err(LifecycleError::Compile(
                "spec.agents missing or not a list".to_string(),
            ))
        }
    }
    // routing.rules 追加 route-<id>(与 compile_team 同形)。
    if let Some((_, Value::Map(routing))) = pairs.iter_mut().find(|(k, _)| k == "routing") {
        if let Some((_, Value::List(rules))) = routing.iter_mut().find(|(k, _)| k == "rules") {
            if !rules
                .iter()
                .any(|rule| yaml_route_assigns_to(rule) == Some(agent_id))
            {
                rules.push(Value::Map(vec![
                    ("id".to_string(), Value::Str(format!("route-{agent_id}"))),
                    (
                        "match".to_string(),
                        Value::Map(vec![(
                            "assignee".to_string(),
                            Value::List(vec![Value::Str(agent_id.to_string())]),
                        )]),
                    ),
                    ("assign_to".to_string(), Value::Str(agent_id.to_string())),
                    ("priority".to_string(), Value::Int(10)),
                ]));
            }
        }
    }
    Ok(())
}

/// ---
/// purpose: 判断 state 的 agents 表里是否已有该 id
/// returns: 存在则 true
/// ---
pub(super) fn runtime_agent_exists(state: &serde_json::Value, agent_id: &AgentId) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| agents.contains_key(agent_id.as_str()))
}

/// ---
/// purpose: 取 YAML agent 节点的 id
/// returns: id 是字符串时返回它，否则 None
/// ---
pub(super) fn yaml_agent_id(agent: &Value) -> Option<&str> {
    let Value::Map(pairs) = agent else {
        return None;
    };
    pairs
        .iter()
        .find(|(key, _)| key == "id")
        .and_then(|(_, value)| match value {
            Value::Str(id) => Some(id.as_str()),
            _ => None,
        })
}

/// ---
/// purpose: 取 YAML 路由规则的 assign_to
/// returns: 该字段是字符串时返回它，否则 None
/// ---
pub(super) fn yaml_route_assigns_to(rule: &Value) -> Option<&str> {
    let Value::Map(pairs) = rule else {
        return None;
    };
    pairs
        .iter()
        .find(|(key, _)| key == "assign_to")
        .and_then(|(_, value)| match value {
            Value::Str(id) => Some(id.as_str()),
            _ => None,
        })
}
