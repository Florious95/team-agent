//! step 6 · compiler — doc-driven team source → canonical `team.spec` dict.
//!
//! Truth source (READ-ONLY): `team-agent-public` @ v0.2.11, `team_agent/compiler.py`.
//! Two in-scope pure transforms (no I/O state, no provider clients, no network):
//!   1. [`read_front_matter`] — `--- … ---` YAML front matter + body split
//!      (`compiler._read_front_matter`, compiler.py:173-185).
//!   2. [`compile_team`] — `TEAM.md` + `agents/*.md` → full spec dict
//!      (`compiler.compile_team`, compiler.py:23-135). The returned spec MUST pass
//!      [`crate::model::spec::validate_spec`].
//!
//! The load-bearing contract is the **spec dict**: values + KEY INSERTION ORDER.
//! Tests below lock both by rendering the built [`Value`] to compact JSON
//! (`json.dumps(spec, sort_keys=False, separators=(",",":"))` equivalent) and
//! comparing byte-for-byte to Python golden. The absolute `workspace` path (env-
//! dependent) is templated to `__WS__` on both sides so every other byte is pinned.
//!
//! SCOPE (this wave): no-profile `subscription` role docs only. The `.env`
//! profile machinery (`profiles/`, `_profile_model`/`load_profile`) and the
//! `rust_core` inline-secret *detection* (`contains_inline_secret` / the secret-
//! lint rejection test) are DEFERRED to a follow-on — the compile path here only
//! needs `contains_inline_secret` to return `false` for clean (non-secret) input.
//!
//! §10: pure lib layer — no panic on malformed input; every parse/validate path
//! returns `Result<_, ModelError>` (mirrors Python `ValidationError`).

use std::fs;
use std::path::Path;

use crate::model::enums::{Provider, ProviderEffort};
use crate::model::yaml::Value;
use crate::model::{paths, spec, yaml, ModelError};
use crate::provider::wire::{
    builtin_provider_model as wire_builtin_provider_model, is_claude_family,
    parse_canonical_provider, provider_model_keys,
};

pub const IGNORED_OWNER_TEAM_ID_FIELD: &str = "owner_team_id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredTeamField {
    pub field: &'static str,
    pub value: String,
}

/// `compiler._read_front_matter` (compiler.py:173-185).
///
/// Reads `path` (UTF-8). If the text does not start with `"---\n"`, returns
/// `({}, full_text)`. Otherwise splits on the first `"\n---"` after byte 4:
/// unterminated (no closing marker) → `ValidationError "{path}: unterminated
/// front matter"`; the front-matter block is parsed via `simple_yaml.loads`
/// (empty block → `{}`); a non-dict block → `ValidationError "{path}: front
/// matter must be a YAML object"`. The body is everything after the closing
/// marker, `lstrip("\n")` (leading NEWLINES only — not other whitespace).
pub fn read_front_matter(path: &Path) -> Result<(Value, String), ModelError> {
    let text = fs::read_to_string(path)
        .map_err(|e| ModelError::Runtime(format!("{}: {e}", path.display())))?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok((Value::Map(Vec::new()), text));
    };
    let Some(close) = rest.find("\n---") else {
        return Err(ModelError::Validation(format!(
            "{}: unterminated front matter",
            path.display()
        )));
    };
    let raw_meta = rest.get(..close).ok_or_else(|| {
        ModelError::Validation(format!("{}: unterminated front matter", path.display()))
    })?;
    let after_meta = rest.get(close..).ok_or_else(|| {
        ModelError::Validation(format!("{}: unterminated front matter", path.display()))
    })?;
    let after_marker = after_meta
        .strip_prefix("\n---")
        .ok_or_else(|| {
            ModelError::Validation(format!("{}: unterminated front matter", path.display()))
        })?;
    let meta = if raw_meta.trim().is_empty() {
        Value::Map(Vec::new())
    } else {
        yaml::loads(raw_meta)?
    };
    if !meta.is_map() {
        return Err(ModelError::Validation(format!(
            "{}: front matter must be a YAML object",
            path.display()
        )));
    }
    Ok((meta, after_marker.trim_start_matches('\n').to_string()))
}

pub fn ignored_owner_team_id_from_team_md(team_dir: &Path) -> Result<Option<IgnoredTeamField>, ModelError> {
    let team_md = team_dir.join("TEAM.md");
    if !team_md.exists() {
        return Ok(None);
    }
    let (team_meta, _) = read_front_matter(&team_md)?;
    let Some(value) = team_meta.get(IGNORED_OWNER_TEAM_ID_FIELD) else {
        return Ok(None);
    };
    Ok(Some(IgnoredTeamField {
        field: IGNORED_OWNER_TEAM_ID_FIELD,
        value: front_matter_value_label(value),
    }))
}

/// `compiler.compile_team` (compiler.py:23-135) — returns the compiled spec dict.
///
/// `TEAM.md` + sorted `agents/*.md` → the canonical spec `Value::Map` with the
/// exact key insertion order Python emits (see RED golden). The returned spec is
/// validated via [`crate::model::spec::validate_spec`] before return. Missing
/// `TEAM.md` / missing `agents/` dir / no role docs / any role-doc validation
/// failure → `ModelError::Validation`.
///
/// NOTE: Python's `compile_team` returns `{ok, team_dir, out, spec}` and only
/// writes `dumps(spec)` when `out_path` is given. The CLI wrapper / out_path
/// write is NOT part of this contract — this function returns the spec dict
/// (the load-bearing artifact) directly.
pub fn compile_team(team_dir: &Path) -> Result<Value, ModelError> {
    let team_md = team_dir.join("TEAM.md");
    if !team_md.exists() {
        return Err(ModelError::Validation(format!(
            "{}: missing TEAM.md",
            team_md.display()
        )));
    }
    let agents_dir = team_dir.join("agents");
    if !agents_dir.exists() {
        return Err(ModelError::Validation(format!(
            "{}: missing agents directory",
            agents_dir.display()
        )));
    }

    let (team_meta, team_body) = read_front_matter(&team_md)?;
    let mut role_paths = Vec::new();
    if agents_dir.is_dir() {
        for entry in fs::read_dir(&agents_dir)
            .map_err(|e| ModelError::Runtime(format!("{}: {e}", agents_dir.display())))?
        {
            let entry = entry
                .map_err(|e| ModelError::Runtime(format!("{}: {e}", agents_dir.display())))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                role_paths.push(path);
            }
        }
    }
    role_paths.sort();
    if role_paths.is_empty() {
        return Err(ModelError::Validation(format!(
            "{}: no role docs found",
            agents_dir.display()
        )));
    }

    let workspace = paths::team_workspace(team_dir)?;
    let workspace_s = workspace.display().to_string();
    let team_name = string_field(&team_meta, "name").unwrap_or_else(|| team_dir_parent_name(team_dir));
    let objective = string_field(&team_meta, "objective")
        .or_else(|| non_empty_trimmed(&team_body))
        .unwrap_or_else(|| "Team Agent document-driven team.".to_string());
    let leader_provider =
        string_field(&team_meta, "provider").unwrap_or_else(|| "codex".to_string());
    let leader_model = optional_string_value(&team_meta, "model");
    let leader_role =
        string_field(&team_meta, "leader_role").unwrap_or_else(|| "leader".to_string());

    let mut agents = Vec::new();
    let mut agent_ids = Vec::new();
    for path in role_paths {
        let compiled = compile_role_agent(&path, &team_meta, &workspace_s)?;
        agent_ids.push(compiled.id);
        agents.push(compiled.agent);
    }

    let default_assignee = agent_ids.first().cloned().unwrap_or_default();
    let routing_rules = agent_ids
        .iter()
        .map(|id| {
            map(vec![
                ("id", Value::Str(format!("route-{id}"))),
                ("match", map(vec![("assignee", list_str(vec![id.as_str()]))])),
                ("assign_to", Value::Str(id.clone())),
                ("priority", Value::Int(10)),
            ])
        })
        .collect::<Vec<_>>();

    // 0.4.x provider effort MVP step 2: validate TEAM.md provider_effort early
    // (unknown literal rejects compile). Empty/absent → no team-level effort.
    let team_provider_effort = match string_field(&team_meta, "provider_effort") {
        Some(raw) if !raw.trim().is_empty() => {
            let value = raw.trim();
            let parsed = ProviderEffort::parse(value).ok_or_else(|| {
                ModelError::Validation(format!(
                    "{}: unknown provider_effort '{value}' (allowed: low|medium|high|xhigh|max)",
                    team_md.display()
                ))
            })?;
            Some(parsed)
        }
        _ => None,
    };

    let mut team_fields: Vec<(&str, Value)> = vec![
        ("name", Value::Str(team_name.clone())),
        ("mode", Value::Str("supervisor_worker".to_string())),
        ("objective", Value::Str(objective)),
        ("workspace", Value::Str(workspace_s)),
    ];
    if let Some(effort) = team_provider_effort {
        team_fields.push(("provider_effort", Value::Str(effort.as_str().to_string())));
    }

    let spec = map(vec![
        ("version", Value::Int(1)),
        ("team", map(team_fields)),
        (
            "leader",
            map(vec![
                ("id", Value::Str("leader".to_string())),
                ("role", Value::Str(leader_role)),
                ("provider", Value::Str(leader_provider)),
                ("model", leader_model),
                ("tools", list_str(vec!["fs_read", "fs_list", "mcp_team"])),
                (
                    "context_policy",
                    map(vec![
                        ("keep_user_thread", Value::Bool(true)),
                        (
                            "receive_worker_outputs",
                            Value::Str("business_messages_and_short_summaries".to_string()),
                        ),
                        ("max_worker_result_tokens", Value::Int(2000)),
                    ]),
                ),
            ]),
        ),
        ("agents", Value::List(agents)),
        (
            "routing",
            map(vec![
                ("default_assignee", Value::Str(default_assignee.clone())),
                ("rules", Value::List(routing_rules)),
            ]),
        ),
        (
            "communication",
            map(vec![
                ("protocol", Value::Str("mcp_inbox".to_string())),
                ("topology", Value::Str("leader_centered".to_string())),
                ("worker_to_worker", bool_field(&team_meta, "worker_to_worker", true)),
                ("ack_timeout_sec", Value::Int(60)),
                ("result_format", Value::Str("result_envelope_v1".to_string())),
                (
                    "message_store",
                    map(vec![
                        ("sqlite", Value::Str(".team/runtime/team.db".to_string())),
                        ("mirror_files", Value::Str(".team/messages".to_string())),
                    ]),
                ),
            ]),
        ),
        (
            "runtime",
            map(vec![
                ("backend", Value::Str("tmux".to_string())),
                (
                    "display_backend",
                    Value::Str(
                        string_field(&team_meta, "display_backend")
                            .unwrap_or_else(|| "adaptive".to_string()),
                    ),
                ),
                ("session_name", Value::Str(session_name(&team_meta, &team_name))),
                ("auto_launch", Value::Bool(true)),
                ("require_user_approval_before_launch", Value::Bool(true)),
                ("max_active_agents", Value::Int(max_active_agents(agent_ids.len()))),
                ("startup_order", list_str(agent_ids)),
                (
                    "dangerous_auto_approve",
                    bool_field(&team_meta, "dangerous_auto_approve", false),
                ),
                ("fast", bool_field(&team_meta, "fast", false)),
                ("tick_interval_sec", int_field(&team_meta, "tick_interval_sec", 2)),
                ("push_min_interval_sec", int_field(&team_meta, "push_min_interval_sec", 60)),
                ("stuck_timeout_sec", int_field(&team_meta, "stuck_timeout_sec", 300)),
            ]),
        ),
        (
            "context",
            map(vec![
                ("state_file", Value::Str("team_state.md".to_string())),
                ("artifact_dir", Value::Str(".team/artifacts".to_string())),
                ("log_dir", Value::Str(".team/logs".to_string())),
                (
                    "summarization",
                    map(vec![
                        (
                            "worker_full_logs",
                            Value::Str("retain_outside_leader_context".to_string()),
                        ),
                        ("state_update", Value::Str("after_each_result".to_string())),
                    ]),
                ),
            ]),
        ),
        (
            "tasks",
            Value::List(vec![map(vec![
                ("id", Value::Str("task_initial".to_string())),
                ("title", Value::Str("Initial document-driven team task".to_string())),
                ("type", Value::Str("implementation".to_string())),
                ("assignee", Value::Str(default_assignee)),
                ("deps", Value::List(Vec::new())),
                ("acceptance", list_str(vec!["Worker reports valid result_envelope_v1"])),
                ("status", Value::Str("pending".to_string())),
                ("requires_tools", list_str(vec!["mcp_team"])),
                ("files", Value::List(Vec::new())),
                ("risk", Value::Str("low".to_string())),
            ])]),
        ),
    ]);
    spec::validate_spec(&spec, &workspace)?;
    Ok(spec)
}

/// 单个角色文档 → 编译后的 agent spec 条目(从 [`compile_team`] 的 per-role 循环抽出)。
/// E5 Bug1:add-agent 复用它**就地读** role 文件编译,不再 copy 进平台目录。
pub struct CompiledRole {
    pub id: String,
    pub role: String,
    pub agent: Value,
}

/// 把一份 role 文档编译成 agent spec 条目。`team_meta` 供 model/auth_mode 继承;
/// `workspace_s` 是 working_directory。**纯读 `role_path`,无任何文件落地。**
pub fn compile_role_agent(
    role_path: &Path,
    team_meta: &Value,
    workspace_s: &str,
) -> Result<CompiledRole, ModelError> {
    let (meta, body) = read_front_matter(role_path)?;
    let id = required_string(&meta, role_path, "name")?;
    let role = required_string(&meta, role_path, "role")?;
    let provider = required_string(&meta, role_path, "provider")?;
    let model = resolve_model(&meta, team_meta, &provider);
    let auth_mode = string_field(&meta, "auth_mode")
        .or_else(|| string_field(team_meta, "default_auth_mode"))
        .unwrap_or_else(|| "subscription".to_string());
    if auth_mode != "subscription" && meta.get("profile").is_none() {
        return Err(ModelError::Validation(format!(
            "{}: profile is required when auth_mode is '{auth_mode}'",
            role_path.display(),
        )));
    }
    let tools = required_tools(&meta, role_path)?;
    let prompt_inline = non_empty_trimmed(&body).unwrap_or_else(|| role.clone());
    let mut agent_items = vec![
        ("id", Value::Str(id.clone())),
        ("role", Value::Str(role.clone())),
        ("provider", Value::Str(provider)),
        ("model", model),
        ("auth_mode", Value::Str(auth_mode)),
        ("working_directory", Value::Str(workspace_s.to_string())),
        (
            "system_prompt",
            map(vec![
                ("inline", Value::Str(prompt_inline)),
                ("file", Value::Null),
            ]),
        ),
        ("tools", list_str(tools)),
        ("permission_mode", Value::Str("restricted".to_string())),
        ("preferred_for", list_str(vec![id.clone(), role.clone()])),
        ("avoid_for", Value::List(Vec::new())),
        (
            "output_contract",
            map(vec![
                ("format", Value::Str("result_envelope_v1".to_string())),
                (
                    "required_fields",
                    list_str(vec!["task_id", "status", "summary", "artifacts"]),
                ),
            ]),
        ),
    ];
    if let Some(profile) = string_field(&meta, "profile") {
        agent_items.push(("profile", Value::Str(profile)));
    }
    // 0.4.x provider effort MVP step 3: resolve effort with role > team > none.
    // Validate (unknown literal) AND check provider/effort compatibility
    // (max is Claude-only; emit hard error for max + non-Claude here so
    // unsupported combinations fail at compile, not at runtime).
    let role_effort = match string_field(&meta, "effort") {
        Some(raw) if !raw.trim().is_empty() => {
            let value = raw.trim();
            let parsed = ProviderEffort::parse(value).ok_or_else(|| {
                ModelError::Validation(format!(
                    "{}: unknown effort '{value}' (allowed: low|medium|high|xhigh|max)",
                    role_path.display()
                ))
            })?;
            Some(parsed)
        }
        _ => None,
    };
    let team_effort = match string_field(team_meta, "provider_effort") {
        Some(raw) if !raw.trim().is_empty() => ProviderEffort::parse(raw.trim()),
        _ => None,
    };
    let resolved_effort = role_effort.or(team_effort);
    if let Some(effort) = resolved_effort {
        // Reject max + non-Claude at compile time.
        let provider_str = agent_items
            .iter()
            .find(|(k, _)| *k == "provider")
            .and_then(|(_, v)| match v {
                Value::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("");
        let provider_enum = parse_canonical_provider(provider_str).unwrap_or(Provider::Codex);
        if effort.is_claude_only() && !is_claude_family(provider_enum) {
            return Err(ModelError::Validation(format!(
                "{}: effort '{}' is only supported by claude/claude_code (provider: {provider_str})",
                role_path.display(),
                effort.as_str()
            )));
        }
        agent_items.push(("effort", Value::Str(effort.as_str().to_string())));
    }
    Ok(CompiledRole {
        id,
        role,
        agent: map(agent_items),
    })
}

fn map(items: Vec<(&str, Value)>) -> Value {
    Value::Map(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn list_str<I, S>(items: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Value::List(items.into_iter().map(|s| Value::Str(s.into())).collect())
}

fn string_field(meta: &Value, key: &str) -> Option<String> {
    meta.get(key).and_then(Value::as_str).map(ToString::to_string)
}

fn required_string(meta: &Value, path: &Path, key: &str) -> Result<String, ModelError> {
    string_field(meta, key).ok_or_else(|| {
        ModelError::Validation(format!(
            "{}: missing front matter field {key}",
            path.display()
        ))
    })
}

fn optional_string_value(meta: &Value, key: &str) -> Value {
    match string_field(meta, key) {
        Some(s) => Value::Str(s),
        None => Value::Null,
    }
}

fn bool_field(meta: &Value, key: &str, default: bool) -> Value {
    match meta.get(key) {
        Some(v) => Value::Bool(v.is_truthy()),
        _ => Value::Bool(default),
    }
}

fn int_field(meta: &Value, key: &str, default: i64) -> Value {
    match meta.get(key).and_then(py_int_value) {
        Some(i) => Value::Int(i),
        None => Value::Int(default),
    }
}

fn py_int_value(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(f.trunc() as i64),
        Value::Str(s) => s.parse::<i64>().ok(),
        Value::Null | Value::List(_) | Value::Map(_) => None,
    }
}

fn required_tools(meta: &Value, path: &Path) -> Result<Vec<String>, ModelError> {
    let Some(value) = meta.get("tools") else {
        return Err(ModelError::Validation(format!(
            "{}: missing front matter field tools",
            path.display()
        )));
    };
    let Some(items) = value.as_list() else {
        return Err(ModelError::Validation(format!(
            "{}: tools must be a list",
            path.display()
        )));
    };
    Ok(items
        .iter()
        .filter_map(Value::as_str)
        .map(|tool| {
            if tool == "shell" {
                "execute_bash".to_string()
            } else {
                tool.to_string()
            }
        })
        .collect())
}

fn resolve_model(role_meta: &Value, team_meta: &Value, provider: &str) -> Value {
    if let Some(model) = string_field(role_meta, "model") {
        return Value::Str(model);
    }
    if let Some(model) = provider_model(team_meta, provider)
        .or_else(|| string_field(team_meta, "default_model"))
    {
        return Value::Str(model);
    }
    if role_meta.get("profile").is_some() {
        return Value::Null;
    }
    builtin_provider_model(provider)
        .map(|m| Value::Str(m.to_string()))
        .unwrap_or(Value::Null)
}

fn provider_model(team_meta: &Value, provider: &str) -> Option<String> {
    let models = team_meta.get("provider_models")?;
    let provider = parse_canonical_provider(provider)?;
    provider_model_keys(provider)
        .iter()
        .find_map(|key| string_field(models, key))
}

fn builtin_provider_model(provider: &str) -> Option<&'static str> {
    parse_canonical_provider(provider).and_then(wire_builtin_provider_model)
}

fn non_empty_trimmed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn team_dir_parent_name(team_dir: &Path) -> String {
    team_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("team")
        .to_string()
}

fn session_name(team_meta: &Value, team_name: &str) -> String {
    string_field(team_meta, "session_name").unwrap_or_else(|| format!("team-{}", slug(team_name)))
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "team".to_string()
    } else {
        out
    }
}

fn max_active_agents(count: usize) -> i64 {
    if count < 2 {
        1
    } else {
        2
    }
}

fn front_matter_value_label(value: &Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| yaml::dumps(value).trim().to_string())
}

#[cfg(test)]
mod tests;
