//! spec / result-envelope 校验(`spec.py`)。
//!
//! 本 module **操作解析后的 Value**(非 typed struct)——与 Python 一样校验 dict,
//! 产出**逐字节一致的有序错误消息列表**(测试对 Python 真相源取 golden 锁死)。
//! serde_json `preserve_order` 保证 child 迭代序 == Python 插入序。
//!
//! 本文件先落地 `validate_result_envelope`(自包含,step 7/11 的门)。`validate_spec`
//! 依赖 `expand_tools`(permissions)+ `find_dependency_cycle`(task_graph)+ yaml Value,
//! 待那几个叶子模块集成后再加。

use std::path::Path;

use serde_json::Value;

use crate::model::enums::TaskStatus;
use crate::model::errors::ModelError;
use crate::model::ids::TaskId;
use crate::model::task_graph::{find_dependency_cycle, TaskNode};
use crate::model::yaml::Value as Yaml;
use crate::model::{permissions, yaml};

/// result_envelope_v1 顶层 required(= allowed)。
const RESULT_REQUIRED: &[&str] = &[
    "schema_version",
    "task_id",
    "agent_id",
    "status",
    "summary",
    "changes",
    "tests",
    "risks",
    "artifacts",
    "next_actions",
];

/// `RESULT_COLLECTION_SCHEMAS`(`spec.py:93-99`),有序:(field, required, allowed)。
const RESULT_COLLECTIONS: &[(&str, &[&str], &[&str])] = &[
    ("changes", &["path", "kind", "description"], &["path", "kind", "description"]),
    ("tests", &["command", "status"], &["command", "status", "detail"]),
    ("risks", &["severity", "description"], &["severity", "description"]),
    ("artifacts", &["path", "description"], &["path", "description"]),
    ("next_actions", &["description"], &["description"]),
];

/// `spec.validate_result_envelope`:校验 result_envelope_v1。失败 → `ModelError::Validation`,
/// 消息体与 Python 字节一致(`result_envelope_v1 validation failed:\n- ...`)。
pub fn validate_result_envelope(envelope: &Value) -> Result<(), ModelError> {
    let errors = result_schema_errors(envelope);
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors
        .iter()
        .map(|e| format!("- {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    Err(ModelError::Validation(format!(
        "result_envelope_v1 validation failed:\n{joined}"
    )))
}

/// `spec._check_keys`:非 object → "must be an object";否则 missing(排序)+ unknown(排序)。
fn check_keys(obj: &Value, path: &str, required: &[&str], allowed: &[&str], errors: &mut Vec<String>) {
    let Some(map) = obj.as_object() else {
        errors.push(format!("{path}: must be an object"));
        return;
    };
    let base = path.trim_end_matches('/');
    let mut missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !map.contains_key(*k))
        .collect();
    missing.sort_unstable();
    for k in missing {
        errors.push(format!("{base}/{k}: missing required field"));
    }
    let mut unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    unknown.sort_unstable();
    for k in unknown {
        errors.push(format!("{base}/{k}: unknown field"));
    }
}

/// `spec._result_schema_errors`,逐行对齐(含错误产出顺序)。
fn result_schema_errors(envelope: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check_keys(envelope, "/", RESULT_REQUIRED, RESULT_REQUIRED, &mut errors);
    let Some(map) = envelope.as_object() else {
        return errors;
    };

    if map.get("schema_version").and_then(Value::as_str) != Some("result_envelope_v1") {
        errors.push("/schema_version: must be result_envelope_v1".to_string());
    }

    for field in ["task_id", "agent_id", "summary"] {
        if let Some(v) = map.get(field) {
            if !v.is_string() {
                errors.push(format!("/{field}: must be a string"));
            } else if v.as_str() == Some("") {
                errors.push(format!("/{field}: must not be empty"));
            }
        }
    }

    if !matches!(
        map.get("status").and_then(Value::as_str),
        Some("success" | "blocked" | "failed" | "partial")
    ) {
        errors.push("/status: invalid result status".to_string());
    }

    if map.contains_key("schema") {
        errors.push("/schema: use schema_version, not schema".to_string());
    }

    for (field, item_required, item_allowed) in RESULT_COLLECTIONS {
        let Some(value) = map.get(*field) else {
            continue;
        };
        let Some(arr) = value.as_array() else {
            errors.push(format!("/{field}: must be a list"));
            continue;
        };
        for (idx, item) in arr.iter().enumerate() {
            let item_path = format!("/{field}/{idx}");
            check_keys(item, &item_path, item_required, item_allowed, &mut errors);
            let Some(item_map) = item.as_object() else {
                continue;
            };
            // 枚举字段校验(changes.kind / tests.status / risks.severity):值不在合法集 → 报错。
            let enum_field: Option<(&str, &[&str], &str)> = match *field {
                "changes" => Some((
                    "kind",
                    &["created", "modified", "deleted", "observed"],
                    "invalid change kind",
                )),
                "tests" => Some((
                    "status",
                    &["passed", "failed", "not_run", "skipped"],
                    "invalid test status",
                )),
                "risks" => Some(("severity", &["low", "medium", "high"], "invalid risk severity")),
                _ => None,
            };
            if let Some((key, valid, msg)) = enum_field {
                let value_ok = item_map
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|s| valid.contains(&s));
                if !value_ok {
                    errors.push(format!("{item_path}/{key}: {msg}"));
                }
            }
            // child string 校验:插入序(preserve_order)== Python item.items()。
            for (key, child) in item_map {
                if item_allowed.contains(&key.as_str()) && !child.is_string() {
                    errors.push(format!("{item_path}/{key}: must be a string"));
                }
            }
        }
    }
    errors
}

// ===================== validate_spec(team.spec.yaml) =====================
// 操作 yaml::Value(spec 走 simple_yaml)。basic_schema 全部先于 semantic;错误消息逐字节
// 对齐 Python `spec.validate_spec`(golden 由真相源双跑锁死)。

const ROOT_KEYS: &[&str] = &[
    "version", "team", "leader", "agents", "routing", "communication", "runtime", "context", "tasks",
];
// Copilot 一期加入白名单(design §B compiler.py:249-251 同位 + cr verdict 总裁,
// MUST-NOT-7 跨厂商等价 — 设计 / cr 已落地 26 约束)。
const SUPPORTED_PROVIDERS: &[&str] = &["claude", "claude_code", "codex", "copilot", "gemini_cli", "fake"];
const AUTH_MODES: &[&str] = &["subscription", "official_api", "compatible_api"];
const VALID_DISPLAY_BACKENDS: &[&str] = &[
    "none", "tmux_attach", "iterm", "ghostty", "ghostty_window", "ghostty_workspace", "adaptive",
];
const TASK_STATUS_STRS: &[&str] = &[
    "pending", "ready", "running", "blocked", "needs_retry", "done", "failed", "cancelled",
];

/// `spec.validate_spec`:basic schema + semantic 校验。失败 → `ModelError::Validation`,
/// 消息与 Python 字节一致(`team.spec.yaml validation failed:\n- ...`)。
pub fn validate_spec(spec: &Yaml, base_dir: &Path) -> Result<(), ModelError> {
    let mut errors = basic_schema_errors(spec);
    errors.extend(semantic_errors(spec, base_dir));
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors.iter().map(|m| format!("- {m}")).collect::<Vec<_>>().join("\n");
    Err(ModelError::Validation(format!(
        "team.spec.yaml validation failed:\n{joined}"
    )))
}

/// 便捷:YAML 文本 → load → 校验(对应 `load_spec` 的校验部分,不含 deprecation 发射)。
pub fn load_and_validate_spec(text: &str, base_dir: &Path) -> Result<Yaml, ModelError> {
    let spec = yaml::loads(text)?;
    validate_spec(&spec, base_dir)?;
    Ok(spec)
}

fn is_map_some(v: Option<&Yaml>) -> bool {
    v.is_some_and(Yaml::is_map)
}

/// `spec._check_keys` 的 yaml::Value 版。
fn check_keys_y(obj: Option<&Yaml>, path: &str, required: &[&str], allowed: &[&str], errors: &mut Vec<String>) {
    let Some(map) = obj.and_then(Yaml::as_map) else {
        errors.push(format!("{path}: must be an object"));
        return;
    };
    let base = path.trim_end_matches('/');
    let mut missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !map.iter().any(|(mk, _)| mk == k))
        .collect();
    missing.sort_unstable();
    for k in missing {
        errors.push(format!("{base}/{k}: missing required field"));
    }
    let mut unknown: Vec<&str> = map
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !allowed.contains(k))
        .collect();
    unknown.sort_unstable();
    for k in unknown {
        errors.push(format!("{base}/{k}: unknown field"));
    }
}

fn check_list_y(value: Option<&Yaml>, path: &str, errors: &mut Vec<String>) {
    if !matches!(value, Some(Yaml::List(_))) {
        errors.push(format!("{path}: must be a list"));
    }
}

fn basic_schema_errors(spec: &Yaml) -> Vec<String> {
    let mut e = Vec::new();
    check_keys_y(Some(spec), "/", ROOT_KEYS, ROOT_KEYS, &mut e);
    if !matches!(spec.get("version"), Some(Yaml::Int(1))) {
        e.push("/version: must equal 1".to_string());
    }
    let team_keys = &["name", "mode", "objective", "workspace"];
    check_keys_y(spec.get("team"), "/team", team_keys, team_keys, &mut e);
    let mode = spec.get("team").and_then(|t| t.get("mode")).and_then(Yaml::as_str);
    if !matches!(mode, Some("supervisor_worker" | "swarm_limited")) {
        e.push("/team/mode: invalid mode".to_string());
    }
    let leader_keys = &["id", "role", "provider", "model", "tools", "context_policy"];
    check_keys_y(spec.get("leader"), "/leader", leader_keys, leader_keys, &mut e);
    let cp_keys = &["keep_user_thread", "receive_worker_outputs", "max_worker_result_tokens"];
    check_keys_y(
        spec.get("leader").and_then(|l| l.get("context_policy")),
        "/leader/context_policy",
        cp_keys,
        cp_keys,
        &mut e,
    );
    match spec.get("agents") {
        Some(Yaml::List(agents)) if !agents.is_empty() => {
            for (idx, agent) in agents.iter().enumerate() {
                check_agent(agent, &format!("/agents/{idx}"), &mut e);
            }
        }
        _ => e.push("/agents: must be a non-empty list".to_string()),
    }
    check_routing(spec.get("routing"), &mut e);
    check_communication(spec.get("communication"), &mut e);
    check_runtime(spec.get("runtime"), &mut e);
    check_context(spec.get("context"), &mut e);
    match spec.get("tasks") {
        Some(Yaml::List(tasks)) => {
            for (idx, task) in tasks.iter().enumerate() {
                check_task(task, &format!("/tasks/{idx}"), &mut e);
            }
        }
        _ => e.push("/tasks: must be a list".to_string()),
    }
    e
}

fn check_agent(agent: &Yaml, path: &str, errors: &mut Vec<String>) {
    let req = &[
        "id", "role", "provider", "model", "working_directory", "system_prompt", "tools",
        "permission_mode", "preferred_for", "avoid_for", "output_contract",
    ];
    let allowed = &[
        "id", "role", "provider", "model", "working_directory", "system_prompt", "tools",
        "permission_mode", "preferred_for", "avoid_for", "output_contract", "paused", "auth_mode",
        "profile", "credential_ref", "forked_from",
    ];
    check_keys_y(Some(agent), path, req, allowed, errors);
    if !agent.is_map() {
        return;
    }
    check_keys_y(agent.get("system_prompt"), &format!("{path}/system_prompt"), &["inline", "file"], &["inline", "file"], errors);
    check_list_y(agent.get("tools"), &format!("{path}/tools"), errors);
    check_list_y(agent.get("preferred_for"), &format!("{path}/preferred_for"), errors);
    check_list_y(agent.get("avoid_for"), &format!("{path}/avoid_for"), errors);
    check_keys_y(agent.get("output_contract"), &format!("{path}/output_contract"), &["format", "required_fields"], &["format", "required_fields"], errors);
    if agent.get("output_contract").and_then(|o| o.get("format")).and_then(Yaml::as_str) != Some("result_envelope_v1") {
        errors.push(format!("{path}/output_contract/format: must be result_envelope_v1"));
    }
}

fn check_routing(routing: Option<&Yaml>, errors: &mut Vec<String>) {
    check_keys_y(routing, "/routing", &["default_assignee", "rules"], &["default_assignee", "rules"], errors);
    if !is_map_some(routing) {
        return;
    }
    let Some(Yaml::List(rules)) = routing.and_then(|r| r.get("rules")) else {
        errors.push("/routing/rules: must be a list".to_string());
        return;
    };
    for (idx, rule) in rules.iter().enumerate() {
        check_keys_y(Some(rule), &format!("/routing/rules/{idx}"), &["id", "assign_to", "priority"], &["id", "when", "match", "assign_to", "priority"], errors);
        let has_clause = rule.get("when").is_some_and(Yaml::is_truthy) || rule.get("match").is_some_and(Yaml::is_truthy);
        if rule.is_map() && !has_clause {
            errors.push(format!("/routing/rules/{idx}: must include when or match"));
        }
    }
}

fn check_communication(comm: Option<&Yaml>, errors: &mut Vec<String>) {
    let req = &["protocol", "topology", "worker_to_worker", "ack_timeout_sec", "result_format", "message_store"];
    check_keys_y(comm, "/communication", req, req, errors);
    if !is_map_some(comm) {
        return;
    }
    if !matches!(comm.and_then(|c| c.get("protocol")).and_then(Yaml::as_str), Some("mcp_inbox" | "file_bus")) {
        errors.push("/communication/protocol: invalid protocol".to_string());
    }
    if comm.and_then(|c| c.get("result_format")).and_then(Yaml::as_str) != Some("result_envelope_v1") {
        errors.push("/communication/result_format: must be result_envelope_v1".to_string());
    }
    check_keys_y(comm.and_then(|c| c.get("message_store")), "/communication/message_store", &["sqlite", "mirror_files"], &["sqlite", "mirror_files"], errors);
}

fn check_runtime(runtime: Option<&Yaml>, errors: &mut Vec<String>) {
    let req = &["backend", "session_name", "auto_launch", "require_user_approval_before_launch", "max_active_agents", "startup_order"];
    let allowed = &[
        "backend", "session_name", "auto_launch", "require_user_approval_before_launch",
        "max_active_agents", "startup_order", "display_backend", "dangerous_auto_approve",
        "auto_attach_leader", "fast", "tick_interval_sec", "push_min_interval_sec",
        "stuck_timeout_sec", "auto_trust_own_workspace",
    ];
    check_keys_y(runtime, "/runtime", req, allowed, errors);
    if !is_map_some(runtime) {
        return;
    }
    let get = |k: &str| runtime.and_then(|r| r.get(k));
    if !matches!(get("backend").and_then(Yaml::as_str), Some("tmux" | "pty")) {
        errors.push("/runtime/backend: invalid backend".to_string());
    }
    if let Some(db) = get("display_backend") {
        if !db.as_str().is_some_and(|s| VALID_DISPLAY_BACKENDS.contains(&s)) {
            errors.push("/runtime/display_backend: invalid display backend".to_string());
        }
    }
    if get("dangerous_auto_approve").is_some_and(|v| !matches!(v, Yaml::Bool(_))) {
        errors.push("/runtime/dangerous_auto_approve: must be a boolean".to_string());
    }
    if get("auto_trust_own_workspace").is_some_and(|v| !matches!(v, Yaml::Bool(_))) {
        errors.push("/runtime/auto_trust_own_workspace: must be a boolean".to_string());
    }
    check_list_y(get("startup_order"), "/runtime/startup_order", errors);
}

fn check_context(context: Option<&Yaml>, errors: &mut Vec<String>) {
    let req = &["state_file", "artifact_dir", "log_dir", "summarization"];
    check_keys_y(context, "/context", req, req, errors);
    if is_map_some(context) {
        check_keys_y(context.and_then(|c| c.get("summarization")), "/context/summarization", &["worker_full_logs", "state_update"], &["worker_full_logs", "state_update"], errors);
    }
}

fn check_task(task: &Yaml, path: &str, errors: &mut Vec<String>) {
    let req = &["id", "title", "type", "assignee", "deps", "acceptance", "status"];
    let allowed = &[
        "id", "title", "type", "assignee", "deps", "acceptance", "status", "description",
        "requires_tools", "files", "risk", "retry_limit", "human_confirmation",
    ];
    check_keys_y(Some(task), path, req, allowed, errors);
    if !task.is_map() {
        return;
    }
    check_list_y(task.get("deps"), &format!("{path}/deps"), errors);
    check_list_y(task.get("acceptance"), &format!("{path}/acceptance"), errors);
    if !task.get("status").and_then(Yaml::as_str).is_some_and(|s| TASK_STATUS_STRS.contains(&s)) {
        errors.push(format!("{path}/status: invalid task status"));
    }
}

fn semantic_errors(spec: &Yaml, base_dir: &Path) -> Vec<String> {
    use std::collections::HashSet;
    let mut e = Vec::new();
    let leader = spec.get("leader");
    let agents: &[Yaml] = spec.get("agents").and_then(Yaml::as_list).unwrap_or(&[]);
    let map_agents: Vec<&Yaml> = agents.iter().filter(|a| a.is_map()).collect();

    // duplicate agent id(集合含 None 复刻 Python `{a.get("id") ...}`)。
    // SMOKE-1 N38 失败可解释性:不仅报"有重复",还点出**哪个 id 重复**(可能多个),
    // 给 operator 直接定位线索;locate.md §"Smallest likely code touch" item 3。
    let id_set: HashSet<Option<&str>> = map_agents.iter().map(|a| a.get("id").and_then(Yaml::as_str)).collect();
    if id_set.len() != map_agents.len() {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut duplicates: Vec<&str> = Vec::new();
        for agent in &map_agents {
            if let Some(id) = agent.get("id").and_then(Yaml::as_str) {
                if !seen.insert(id) && !duplicates.contains(&id) {
                    duplicates.push(id);
                }
            }
        }
        if duplicates.is_empty() {
            e.push("/agents: duplicate agent id".to_string());
        } else {
            e.push(format!(
                "/agents: duplicate agent id: {}",
                duplicates
                    .iter()
                    .map(|id| format!("`{id}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // all_ids:present 的 agent id + leader id(若 truthy)。
    let mut all_ids: HashSet<&str> = map_agents.iter().filter_map(|a| a.get("id").and_then(Yaml::as_str)).collect();
    if let Some(lid) = leader.and_then(|l| l.get("id")).and_then(Yaml::as_str) {
        if !lid.is_empty() {
            all_ids.insert(lid);
        }
    }

    let leader_provider = leader.and_then(|l| l.get("provider"));
    if !leader_provider.and_then(Yaml::as_str).is_some_and(|p| SUPPORTED_PROVIDERS.contains(&p)) {
        e.push(format!("/leader/provider: unknown provider {}", py_repr(leader_provider)));
    }

    for (idx, agent) in agents.iter().enumerate() {
        let provider = agent.get("provider");
        if !provider.and_then(Yaml::as_str).is_some_and(|p| SUPPORTED_PROVIDERS.contains(&p)) {
            e.push(format!("/agents/{idx}/provider: unknown provider {}", py_repr(provider)));
        }
        if let Some(auth) = agent.get("auth_mode") {
            if !matches!(auth, Yaml::Null) && !auth.as_str().is_some_and(|a| AUTH_MODES.contains(&a)) {
                e.push(format!("/agents/{idx}/auth_mode: unknown auth_mode {}", py_repr(Some(auth))));
            }
        }
        if let Some(f) = agent.get("system_prompt").and_then(|sp| sp.get("file")).filter(|p| p.is_truthy()).and_then(Yaml::as_str) {
            let candidate = Path::new(f);
            let full = if candidate.is_absolute() { candidate.to_path_buf() } else { base_dir.join(candidate) };
            if !full.exists() {
                e.push(format!("/agents/{idx}/system_prompt/file: file not found: {}", full.display()));
            }
        }
        let tools: Vec<&str> = agent.get("tools").and_then(Yaml::as_list).unwrap_or(&[]).iter().filter_map(Yaml::as_str).collect();
        for tool in permissions::expand_tool_strings(tools) {
            if !permissions::is_canonical_tool(&tool) {
                e.push(format!("/agents/{idx}/tools: unknown tool {}", py_repr_str(&tool)));
            }
        }
    }

    let leader_tools: Vec<&str> = leader.and_then(|l| l.get("tools")).and_then(Yaml::as_list).unwrap_or(&[]).iter().filter_map(Yaml::as_str).collect();
    for tool in permissions::expand_tool_strings(leader_tools) {
        if !permissions::is_canonical_tool(&tool) {
            e.push(format!("/leader/tools: unknown tool {}", py_repr_str(&tool)));
        }
    }

    let routing = spec.get("routing");
    if let Some(da) = routing.and_then(|r| r.get("default_assignee")).and_then(Yaml::as_str) {
        if !da.is_empty() && !all_ids.contains(da) {
            e.push(format!("/routing/default_assignee: unknown agent {}", py_repr_str(da)));
        }
    }
    let rules = routing.and_then(|r| r.get("rules")).and_then(Yaml::as_list).unwrap_or(&[]);
    for (idx, rule) in rules.iter().enumerate() {
        let target = rule.get("assign_to");
        if !target.and_then(Yaml::as_str).is_some_and(|t| all_ids.contains(t)) {
            e.push(format!("/routing/rules/{idx}/assign_to: unknown agent {}", py_repr(target)));
        }
    }

    let tasks: &[Yaml] = spec.get("tasks").and_then(Yaml::as_list).unwrap_or(&[]);
    let task_ids: HashSet<&str> = tasks.iter().filter(|t| t.is_map()).filter_map(|t| t.get("id").and_then(Yaml::as_str)).collect();
    for (idx, task) in tasks.iter().enumerate() {
        if let Some(a) = task.get("assignee").and_then(Yaml::as_str) {
            if !a.is_empty() && !all_ids.contains(a) {
                e.push(format!("/tasks/{idx}/assignee: unknown agent {}", py_repr_str(a)));
            }
        }
        for dep in task.get("deps").and_then(Yaml::as_list).unwrap_or(&[]) {
            if !dep.as_str().is_some_and(|d| task_ids.contains(d)) {
                e.push(format!("/tasks/{idx}/deps: unknown dependency {}", py_repr(Some(dep))));
            }
        }
    }

    // dependency cycle(只含有非空 id 的 task,复刻 Python `if t.get("id")`)。
    let nodes: Vec<TaskNode> = tasks
        .iter()
        .filter_map(|t| {
            let id = t.get("id").and_then(Yaml::as_str).filter(|s| !s.is_empty())?;
            let deps: Vec<TaskId> = t.get("deps").and_then(Yaml::as_list).unwrap_or(&[]).iter().filter_map(|d| d.as_str().map(TaskId::from)).collect();
            Some(TaskNode::new(TaskId::from(id), deps, TaskStatus::Pending))
        })
        .collect();
    let cycle = find_dependency_cycle(&nodes);
    if !cycle.is_empty() {
        let chain = cycle.iter().map(TaskId::as_str).collect::<Vec<_>>().join(" -> ");
        e.push(format!("/tasks: dependency cycle detected: {chain}"));
    }
    e
}

/// Python `repr()` of a string(选引号 + 转义),用于 `unknown X 'name'`。
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr()` of an optional yaml value(None/absent→"None",Str→repr,标量→字面)。
fn py_repr(v: Option<&Yaml>) -> String {
    match v {
        None | Some(Yaml::Null) => "None".to_string(),
        Some(Yaml::Str(s)) => py_repr_str(s),
        Some(Yaml::Bool(true)) => "True".to_string(),
        Some(Yaml::Bool(false)) => "False".to_string(),
        Some(Yaml::Int(i)) => i.to_string(),
        Some(Yaml::Float(f)) => f.to_string(),
        // list/map 作 provider/dep 等极少见的退化输入;非 Python-exact,仅保证不 panic。
        Some(other) => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    // golden 错误列表由 Python 真相源 `spec._result_schema_errors` 算出(team-agent-public@439bef8)。
    // §4.2 行为 diff 双跑:Rust 必须产出逐字节一致的有序消息。
    #[test]
    fn valid_envelope_has_no_errors() {
        let v = json!({
            "schema_version":"result_envelope_v1","task_id":"t1","agent_id":"a1",
            "status":"success","summary":"done",
            "changes":[],"tests":[],"risks":[],"artifacts":[],"next_actions":[]
        });
        assert!(result_schema_errors(&v).is_empty());
        assert!(validate_result_envelope(&v).is_ok());
    }

    #[test]
    fn missing_unknown_status_schema_order_matches_python() {
        let v = json!({
            "schema_version":"v0","task_id":"","status":"weird","summary":"s","schema":"x",
            "changes":[],"tests":[],"risks":[],"artifacts":[],"next_actions":[]
        });
        assert_eq!(
            result_schema_errors(&v),
            vec![
                "/agent_id: missing required field",
                "/schema: unknown field",
                "/schema_version: must be result_envelope_v1",
                "/task_id: must not be empty",
                "/status: invalid result status",
                "/schema: use schema_version, not schema",
            ]
        );
    }

    #[test]
    fn non_object_envelope() {
        assert_eq!(
            result_schema_errors(&json!(["x"])),
            vec!["/: must be an object"]
        );
    }

    #[test]
    fn bad_collection_items_match_python() {
        let v = json!({
            "schema_version":"result_envelope_v1","task_id":"t","agent_id":"a",
            "status":"success","summary":"s",
            "changes":[{"path":"p","kind":"bogus","description":"d"}],
            "tests":[{"command":"c","status":"weird"}],
            "risks":[{"severity":"huge","description":"d"}],
            "artifacts":[],
            "next_actions":[{"description":123}]
        });
        assert_eq!(
            result_schema_errors(&v),
            vec![
                "/changes/0/kind: invalid change kind",
                "/tests/0/status: invalid test status",
                "/risks/0/severity: invalid risk severity",
                "/next_actions/0/description: must be a string",
            ]
        );
    }

    #[test]
    fn validate_returns_joined_message() {
        let v = json!(["x"]);
        let err = validate_result_envelope(&v).unwrap_err();
        assert_eq!(
            err.to_string(),
            "validation error: result_envelope_v1 validation failed:\n- /: must be an object"
        );
    }

    // --- validate_spec(对 Python `spec.validate_spec` 取 golden) ---

    const TD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/model/testdata");

    fn all_spec_errors(spec: &Yaml) -> Vec<String> {
        let mut v = basic_schema_errors(spec);
        v.extend(semantic_errors(spec, Path::new(TD)));
        v
    }

    #[test]
    fn valid_team_spec_passes() {
        let spec = yaml::loads(include_str!("testdata/team.spec.yaml")).unwrap();
        let r = validate_spec(&spec, Path::new(TD));
        assert!(r.is_ok(), "expected valid, got: {r:?}");
    }

    #[test]
    fn invalid_spec_a_matches_python_golden() {
        let spec = yaml::loads(include_str!("testdata/spec_invalid_a.yaml")).unwrap();
        assert_eq!(
            all_spec_errors(&spec),
            vec![
                "/version: must equal 1",
                "/team/mode: invalid mode",
                "/leader/provider: unknown provider 'badprov'",
                "/agents/0/tools: unknown tool 'banana'",
            ]
        );
    }

    #[test]
    fn empty_spec_matches_python_golden() {
        let spec = yaml::loads("{}").unwrap();
        assert_eq!(
            all_spec_errors(&spec),
            vec![
                "/agents: missing required field",
                "/communication: missing required field",
                "/context: missing required field",
                "/leader: missing required field",
                "/routing: missing required field",
                "/runtime: missing required field",
                "/tasks: missing required field",
                "/team: missing required field",
                "/version: missing required field",
                "/version: must equal 1",
                "/team: must be an object",
                "/team/mode: invalid mode",
                "/leader: must be an object",
                "/leader/context_policy: must be an object",
                "/agents: must be a non-empty list",
                "/routing: must be an object",
                "/communication: must be an object",
                "/runtime: must be an object",
                "/context: must be an object",
                "/tasks: must be a list",
                "/leader/provider: unknown provider None",
            ]
        );
    }
}
