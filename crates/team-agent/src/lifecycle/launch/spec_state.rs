//! ---
//! purpose: spec 的读取改写与 runtime state 初值物化，含 quick-start 的 owner 种入
//! contract:
//!   provides:
//!     - name: initial_runtime_state
//!       what: 由 spec 生成首份 runtime state 树，并种入 launched owner
//!     - name: write_spec_atomic
//!       what: 先写同目录临时文件再 rename，避免留下半截 spec
//!     - name: spec_session_name
//!       what: 取 runtime.session_name，缺省由 team 名派生
//!     - name: spec_agents
//!       what: 取 spec 里全部 agent id
//!     - name: spec_routes
//!       what: 对 spec 里每个 task 算出路由决策
//!     - name: effective_runtime_config_for_worker_spawn
//!       what: 由单个 agent 的 dangerously_skip_permissions 定出 bypass 审批结论
//!   depends:
//!     - crate::state::projection
//!     - crate::state::identity
//!     - crate::state::ownership
//!     - crate::model::yaml
//!     - crate::model::routing
//!     - crate::provider::bypass_flags
//!     - crate::lifecycle::launch::identity
//!     - crate::lifecycle::launch::leader_context
//!     - crate::lifecycle::launch::worker_env
//! boundary:
//!   - 只生成与改写数据，不 spawn 进程也不开显示
//!   - bypass 只认单个 agent 自己的声明，不从 runtime 配置或 leader argv 全队继承
//!   - spec 写盘一律走原子替换，不做就地截断写
//! maturity: wired
//! ---
//!
//! unit-8 (Stage 3) — `lifecycle::launch::spec_state` phase boundary.
//!
//! The dedicated home for spec/runtime-path resolution and state-tree
//! materialization phases of `quick_start`. Lives in the
//! `lifecycle/launch/` submodule so future commits can migrate the
//! existing inline phase fns (launch.rs:2781-2906 and 1680-1756 ranges)
//! here in small, reviewable pieces.
//!
//! Established phases (canonical names — keep stable for future
//! migration):
//!
//! * `resolve_spec_paths`   — `.team/runtime/<team>/team.spec.yaml`
//!                            resolution + workspace canonicalization
//! * `materialize_state`    — T1 layer state.json initialization including
//!                            agent capture fields and `spawn_cwd`
//!
//! This commit lands the boundary + a marker enum so unit-8's adoption
//! sites can reference the phases by name. The phase fns themselves
//! remain in launch.rs until the next batch of relocations.

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

use super::identity::spec_display_backend;
use super::leader_context::{
    attributed_provider_for_pane_across_tmux_sockets, caller_provider_for_seed_with_lookup,
    seed_unbound_launched_owner,
};
use super::worker_env::spawn_timestamp;

/// ---
/// purpose: 由 spec 生成首份 runtime state 树
/// params:
///   team_key: 本团队的 runtime 键
/// returns: 含 spec_path、workspace、team_dir、session_name、leader、agents、tasks 与 display_backend 的 state；随后按环境种入 launched owner，种不到则种一份 unbound owner
/// ---
pub(super) fn initial_runtime_state(
    spec: &Value,
    spec_path: &Path,
    workspace: &Path,
    team_dir: &Path,
    team_key: &str,
) -> serde_json::Value {
    let mut agents = serde_json::Map::new();
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("codex");
        let role = agent.get("role").and_then(Value::as_str).unwrap_or(id);
        let model = agent.get("model").and_then(Value::as_str);
        let auth_mode = agent.get("auth_mode").and_then(Value::as_str);
        let mut value = serde_json::json!({
            "provider": provider,
            "role": role,
        });
        if let Some(obj) = value.as_object_mut() {
            if let Some(model) = model {
                obj.insert("model".to_string(), serde_json::json!(model));
            }
            if let Some(auth_mode) = auth_mode {
                obj.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
            }
        }
        agents.insert(id.to_string(), value);
    }
    let display_backend = spec_display_backend(spec);
    let mut state = serde_json::Map::new();
    state.insert(
        "spec_path".to_string(),
        serde_json::json!(spec_path.to_string_lossy().to_string()),
    );
    state.insert(
        "workspace".to_string(),
        serde_json::json!(workspace.to_string_lossy().to_string()),
    );
    state.insert(
        "team_dir".to_string(),
        serde_json::json!(team_dir.to_string_lossy().to_string()),
    );
    state.insert("team_key".to_string(), serde_json::json!(team_key));
    state.insert(
        "session_name".to_string(),
        serde_json::json!(spec_session_name(spec).as_str()),
    );
    state.insert(
        "leader".to_string(),
        spec.get("leader")
            .map(yaml_value_to_json)
            .unwrap_or(serde_json::Value::Null),
    );
    state.insert("agents".to_string(), serde_json::Value::Object(agents));
    state.insert("tasks".to_string(), spec_tasks_json(spec));
    state.insert(
        "display_backend".to_string(),
        serde_json::json!(display_backend),
    );
    state.insert("is_external_leader".to_string(), serde_json::json!(false));
    let mut state = serde_json::Value::Object(state);
    if !seed_launched_owner_from_env(&mut state) {
        let team_id = crate::state::projection::team_state_key(&state);
        seed_unbound_launched_owner(&mut state, &team_id);
    }
    state
}

/// ---
/// purpose: 从环境里的 caller 身份种入 launched owner
/// params:
///   state: 待改写的 state，成功时写入 owner 与 leader receiver
/// returns: true 表示已种入，取不到 caller 身份或无 pane 时为 false
/// ---
pub(super) fn seed_launched_owner_from_env(state: &mut serde_json::Value) -> bool {
    let team_id = crate::state::projection::team_state_key(state);
    let Ok(caller) = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&team_id),
        None,
    ) else {
        return false;
    };
    seed_launched_owner_from_caller_with_provider_lookup(
        state,
        caller,
        attributed_provider_for_pane_across_tmux_sockets,
    )
}

/// ---
/// purpose: 用给定的 caller 身份与 pane provider 查询函数种入 owner 与 leader receiver
/// params:
///   caller: 调用方身份，pane_id 为空则不种
///   lookup_pane_provider: 由 pane 反查 provider 的函数，便于测试替换
/// returns: true 表示已写入 owner 记录
/// ---
pub(super) fn seed_launched_owner_from_caller_with_provider_lookup(
    state: &mut serde_json::Value,
    caller: crate::state::owner_gate::CallerIdentity,
    lookup_pane_provider: impl Fn(&PaneId) -> Option<Provider>,
) -> bool {
    if caller.pane_id.is_empty() {
        return false;
    }
    let provider = caller_provider_for_seed_with_lookup(&caller, lookup_pane_provider);
    let pane_id = caller.pane_id;
    let owner_epoch = 1u64;
    let mut owner = serde_json::json!({
        "pane_id": pane_id,
        "machine_fingerprint": caller.machine_fingerprint,
        "leader_session_uuid": caller.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
    });
    let mut receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "pane": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    if let Some(provider) = provider.as_ref() {
        if let Some(owner) = owner.as_object_mut() {
            owner.insert("provider".to_string(), serde_json::json!(provider));
        }
        if let Some(receiver) = receiver.as_object_mut() {
            receiver.insert("provider".to_string(), serde_json::json!(provider));
        }
    }
    if let (Some(receiver), Some(socket)) = (
        receiver.as_object_mut(),
        crate::tmux_backend::socket_name_from_tmux_env(),
    ) {
        receiver.insert("tmux_socket".to_string(), serde_json::json!(socket));
    }
    // Stage 3a (identity-boundary unified plan, architect direction 2026-06-23):
    // route quick-start attached-from-env seed through ownership repository.
    let team_key = crate::state::projection::team_state_key(state);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(owner_epoch);
    crate::state::ownership::write_owner(state, &team_key, record);
    true
}

/// ---
/// purpose: 判断环境里是否带有任一 leader 身份变量
/// returns: pane id、session uuid、其 override 或 provider 任一非空即 true
/// ---
pub(super) fn has_positive_caller_leader_env() -> bool {
    env_nonempty("TEAM_AGENT_LEADER_PANE_ID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE")
        || env_nonempty("TEAM_AGENT_LEADER_PROVIDER")
}

/// ---
/// purpose: 判断某环境变量存在且非空
/// returns: 存在且非空为 true
/// ---
pub(super) fn env_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| !value.is_empty())
}

/// ---
/// purpose: 把 spec 的 tasks 列表转成 JSON 数组
/// returns: 转换后的数组，没有 tasks 时为空数组
/// ---
pub(super) fn spec_tasks_json(spec: &Value) -> serde_json::Value {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| serde_json::Value::Array(tasks.iter().map(yaml_value_to_json).collect()))
        .unwrap_or_else(|| serde_json::json!([]))
}

/// ---
/// purpose: 把 YAML 值递归转成等价 JSON 值
/// returns: 结构与标量一一对应的 JSON
/// ---
pub(super) fn yaml_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(v) => serde_json::json!(v),
        Value::Int(v) => serde_json::json!(v),
        Value::Float(v) => serde_json::json!(v),
        Value::Str(v) => serde_json::json!(v),
        Value::List(values) => {
            serde_json::Value::Array(values.iter().map(yaml_value_to_json).collect())
        }
        Value::Map(entries) => {
            let mut out = serde_json::Map::new();
            for (key, item) in entries {
                out.insert(key.clone(), yaml_value_to_json(item));
            }
            serde_json::Value::Object(out)
        }
    }
}

/// ---
/// purpose: 原子写 spec，先写带进程号后缀的临时文件再 rename 覆盖
/// returns: 成功返回空值
/// errors: 建父目录、写临时文件或 rename 失败时返回 StatePersist；rename 失败会删掉临时文件且原 spec 不动
/// ---
///
/// Set `runtime.session_name` on the compiled spec to `session_name`, creating the
/// `runtime` map and/or the `session_name` entry if absent. Used by quick-start to
/// derive the tmux session from the REQUESTED team identity (CR-040/042) rather
/// than the template's compiled-in name.
/// E5 Bug2(atomic 真修):原子写 runtime spec —— 写 `<spec>.tmp-<pid>` 再 rename 覆盖,
/// 避免崩溃/并发留下半截 spec(plain fs::write 会 in-place truncate 后逐字节写)。
/// rename 失败时清理 tmp,原 spec(若有)不动。
pub(crate) fn write_spec_atomic(spec_path: &Path, spec: &Value) -> Result<(), LifecycleError> {
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let tmp = spec_path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, yaml::dumps(spec))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", tmp.display())))?;
    if let Err(e) = std::fs::rename(&tmp, spec_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(LifecycleError::StatePersist(format!(
            "{}: {e}",
            spec_path.display()
        )));
    }
    Ok(())
}

/// ---
/// purpose: 就地把 spec 的 runtime.session_name 改成给定值
/// ---
pub(crate) fn override_spec_session_name(spec: &mut Value, session_name: &str) {
    override_spec_runtime_str(spec, "session_name", session_name);
}

/// ---
/// purpose: 就地把 spec 里 team.workspace 与各 agent 的 working_directory 改成给定路径
/// params:
///   spec: 非 Map 时不动；只改已存在的字段，不新建
/// ---
pub(crate) fn override_spec_workspace(spec: &mut Value, workspace: &Path) {
    let workspace_s = workspace.to_string_lossy().to_string();
    let Value::Map(root) = spec else { return };
    if let Some((_, Value::Map(team))) = root.iter_mut().find(|(k, _)| k == "team") {
        if let Some((_, value)) = team.iter_mut().find(|(k, _)| k == "workspace") {
            *value = Value::Str(workspace_s.clone());
        }
    }
    if let Some((_, Value::List(agents))) = root.iter_mut().find(|(k, _)| k == "agents") {
        for agent in agents {
            if let Value::Map(fields) = agent {
                if let Some((_, value)) = fields.iter_mut().find(|(k, _)| k == "working_directory")
                {
                    *value = Value::Str(workspace_s.clone());
                }
            }
        }
    }
}

/// ---
/// purpose: 就地把 spec 的 runtime.display_backend 改成给定值
/// ---
pub(super) fn override_spec_display_backend(spec: &mut Value, display_backend: &str) {
    override_spec_runtime_str(spec, "display_backend", display_backend);
}

/// ---
/// purpose: 就地写 spec 的 runtime 段下某个字符串字段
/// params:
///   spec: 非 Map 时不动
/// returns: runtime 段缺失则新建，已存在同名键则覆盖，runtime 是非 Map 值则整段替换
/// ---
pub(super) fn override_spec_runtime_str(spec: &mut Value, key: &str, value: &str) {
    let Value::Map(root) = spec else { return };
    let runtime_slot = root
        .iter_mut()
        .find(|(k, _)| k == "runtime")
        .map(|(_, v)| v);
    match runtime_slot {
        Some(Value::Map(runtime)) => {
            if let Some((_, existing)) = runtime.iter_mut().find(|(k, _)| k == key) {
                *existing = Value::Str(value.to_string());
            } else {
                runtime.push((key.to_string(), Value::Str(value.to_string())));
            }
        }
        Some(other) => {
            *other = Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]);
        }
        None => {
            root.push((
                "runtime".to_string(),
                Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]),
            ));
        }
    }
}

/// ---
/// purpose: 定出本 spec 的 tmux session 名
/// returns: runtime.session_name 的非空值；缺失时由 team.name 派生，team 名也缺失时用 agent 兜底
/// ---
pub(super) fn spec_session_name(spec: &Value) -> SessionName {
    if let Some(name) = spec
        .get("runtime")
        .and_then(|v| v.get("session_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return SessionName::new(name);
    }
    // Python launch/core.py:56 — fallback derives from the team name, not a constant.
    let team_name = spec
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("agent");
    SessionName::new(format!("team-{team_name}"))
}

/// ---
/// purpose: 把 spec_session_name 以更宽可见性转出，供 layout 复用同一份实现
/// returns: 同 spec_session_name
/// ---
///
/// 0.3.28 layout step 1: pub re-export of `spec_session_name` for the new
/// `layout::sessions::worker_session_name` to delegate to. Single underlying
/// impl; this just widens visibility without duplicating logic.
pub fn worker_session_name_pub(spec: &Value) -> SessionName {
    spec_session_name(spec)
}

/// ---
/// purpose: 取出 spec 里全部 agent id
/// returns: 按 spec 顺序的 agent id，无 id 字段的条目被跳过
/// ---
pub(super) fn spec_agents(spec: &Value) -> Vec<AgentId> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(AgentId::new))
        .collect()
}

/// ---
/// purpose: 取出 spec 里全部 agent id 的集合，便于成员判定
/// returns: 有序集合
/// ---
///
/// Bug 1 (0.4.2): expose spec agent id set so the restart path can filter
/// state.agents to only the agents currently defined in the rebuilt spec.
/// Returns a `BTreeSet<String>` for O(log n) membership checks.
pub(crate) fn spec_agent_id_set(spec: &Value) -> std::collections::BTreeSet<String> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// ---
/// purpose: 取出 spec 的 agents 列表原始节点
/// returns: 节点引用列表，字段缺失或类型不对时为空
/// ---
pub(super) fn spec_agent_values(spec: &Value) -> Vec<&Value> {
    spec.get("agents")
        .and_then(Value::as_list)
        .map(|agents| agents.iter().collect())
        .unwrap_or_default()
}

/// ---
/// purpose: 对 spec 里每个 task 算出路由决策
/// returns: 每个 task 的选中 agent 与理由，manual_override 恒为 false
/// ---
pub(super) fn spec_routes(spec: &Value) -> Vec<RoutingDecision> {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| {
            tasks
                .iter()
                .map(|task| {
                    let routed = crate::model::routing::route_task(spec, task);
                    RoutingDecision {
                        task_id: task.get("id").and_then(Value::as_str).map(str::to_string),
                        selected_agent: routed.agent_id,
                        reason: routed.reason,
                        manual_override: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// ---
/// purpose: 定出默认承接人
/// returns: routing.default_assignee，缺失时取第一个 agent
/// ---
pub(super) fn spec_default_assignee(spec: &Value) -> Option<AgentId> {
    spec.get("routing")
        .and_then(|v| v.get("default_assignee"))
        .and_then(Value::as_str)
        .map(AgentId::new)
        .or_else(|| spec_agents(spec).into_iter().next())
}

/// ---
/// purpose: 由单个 YAML agent 的 dangerously_skip_permissions 定出 bypass 审批结论
/// returns: 字段为 true 时 enabled 且来源记 RuntimeConfig，并带上该 provider 的 bypass argv；否则 Disabled
/// errors: 当前实现不产生 Err
/// contract_id: lifecycle.spec_state.effective_runtime_config
/// ---
/// 0.5.66 bypass 单源:从**单个 agent**(yaml spec)的 `dangerously_skip_permissions` 构造
/// `DangerousApproval`(取代旧"runtime config + leader argv"全队一份)。
/// true → source=RuntimeConfig(角色声明即用户明确同意,coordinator 的
/// `explicit_yes_confirmed` 自动放行);false → Disabled。
pub(crate) fn effective_runtime_config_for_worker_spawn(
    agent: &Value,
    provider: Provider,
) -> Result<DangerousApproval, LifecycleError> {
    let enabled = matches!(
        agent.get("dangerously_skip_permissions"),
        Some(Value::Bool(true))
    );
    if !enabled {
        return Ok(DangerousApproval {
            enabled: false,
            source: DangerousApprovalSource::Disabled,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: false,
            ancestry_binary_name: None,
            unexpected_binary: false,
        });
    }
    let flag = crate::provider::bypass_flags::provider_bypass_flag(provider);
    Ok(DangerousApproval {
        enabled: true,
        source: DangerousApprovalSource::RuntimeConfig,
        inherited: false,
        provider: Some(provider_display_str(provider).to_string()),
        flag: flag.map(str::to_string),
        worker_capability_above_leader: false,
        ancestry_binary_name: None,
        unexpected_binary: false,
    })
}

/// ---
/// purpose: 同上，但读的是 runtime state 里的 JSON agent 节点
/// returns: 同 YAML 版
/// errors: 当前实现不产生 Err
/// contract_id: lifecycle.spec_state.effective_runtime_config
/// ---
/// 0.5.66 bypass 单源:restart/state 路径的 serde_json 版(agent 来自 state JSON)。
pub(crate) fn effective_runtime_config_for_worker_spawn_json(
    agent: &serde_json::Value,
    provider: Provider,
) -> Result<DangerousApproval, LifecycleError> {
    let enabled = agent
        .get("dangerously_skip_permissions")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return Ok(DangerousApproval {
            enabled: false,
            source: DangerousApprovalSource::Disabled,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: false,
            ancestry_binary_name: None,
            unexpected_binary: false,
        });
    }
    let flag = crate::provider::bypass_flags::provider_bypass_flag(provider);
    Ok(DangerousApproval {
        enabled: true,
        source: DangerousApprovalSource::RuntimeConfig,
        inherited: false,
        provider: Some(provider_display_str(provider).to_string()),
        flag: flag.map(str::to_string),
        worker_capability_above_leader: false,
        ancestry_binary_name: None,
        unexpected_binary: false,
    })
}

/// ---
/// purpose: 给出 provider 的稳定 wire 名
/// returns: 与 spec 写法一致的小写下划线名
/// ---
pub(crate) fn provider_display_str(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini_cli",
        Provider::Grok => "grok",
        Provider::CursorAgent => "cursor_agent",
        Provider::Pi => "pi",
        Provider::Fake => "fake",
    }
}

/// ---
/// purpose: 由 team 目录推出 workspace 根
/// returns: 解析成功用解析值，失败时退到 team 目录的父目录，没有父目录则用 team 目录本身
/// ---
pub(super) fn team_workspace(team_dir: &Path) -> PathBuf {
    crate::model::paths::team_workspace(team_dir).unwrap_or_else(|_| {
        team_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| team_dir.to_path_buf())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::enums::Provider;

    fn yaml_agent(bypass: bool) -> Value {
        crate::model::yaml::loads(&format!(
            "id: a\nrole: r\nprovider: codex\ndangerously_skip_permissions: {bypass}\n"
        ))
        .unwrap()
    }

    // 0.5.66 bypass 单源 §4.1:true → enabled + source=runtime_config(角色声明即明确同意)。
    #[test]
    fn test_effective_safety_from_true_field() {
        let safety =
            effective_runtime_config_for_worker_spawn(&yaml_agent(true), Provider::Codex).unwrap();
        assert!(safety.enabled);
        assert_eq!(safety.source, DangerousApprovalSource::RuntimeConfig);
        assert_eq!(
            safety.flag.as_deref(),
            Some("--dangerously-bypass-approvals-and-sandbox")
        );
    }

    // 0.5.66 bypass 单源 §4.1:false → Disabled。
    #[test]
    fn test_effective_safety_from_false_field() {
        let safety =
            effective_runtime_config_for_worker_spawn(&yaml_agent(false), Provider::Codex).unwrap();
        assert!(!safety.enabled);
        assert_eq!(safety.source, DangerousApprovalSource::Disabled);
    }
}
