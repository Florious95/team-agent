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
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
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
    // E42 (0.3.24 P0, double-spec deadlock): persist the initial state row as
    // "starting" (not "running"). The caller (add_agent_with_transport_at_paths)
    // promotes to "running" only after start_agent_at_paths returns Running.
    // If the spawn fails, the rollback below removes the entry entirely.
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
    // 0.4.x provider effort MVP step 8 (dynamic add-agent): persist effort
    // from the role doc front matter (compiler.rs validates syntax/semantics
    // at compile; add-agent path validates here too in case of direct YAML).
    if let Some(effort_str) = meta.get("effort").and_then(Value::as_str) {
        if !effort_str.is_empty() {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("effort".to_string(), serde_json::json!(effort_str));
            }
        }
    }
    if let Some(obj) = entry.as_object_mut() {
        // 0.5.66 bypass 单源:policy 从 role 文档字段派生,不再用全队 `DangerousApproval`。
        let meta_provider = meta
            .get("provider")
            .and_then(Value::as_str)
            .and_then(crate::provider::wire::parse_provider)
            .unwrap_or(Provider::Codex);
        persist_effective_approval_policy_from_yaml_agent(obj, meta, meta_provider);
    }
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
