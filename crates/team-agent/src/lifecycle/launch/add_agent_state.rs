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

pub(super) fn upsert_agent_state_from_role(
    workspace: &Path,
    canonical_team_key: &str,
    agent_id: &AgentId,
    meta: &Value,
    dynamic_role_file: &Path,
    safety: &DangerousApproval,
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
        persist_effective_approval_policy(obj, safety);
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

pub(super) fn runtime_agent_exists(state: &serde_json::Value, agent_id: &AgentId) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| agents.contains_key(agent_id.as_str()))
}

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
