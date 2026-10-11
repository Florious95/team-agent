//!
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
//! Profile references are carried as role metadata; profile files and secrets are
//! handled by lifecycle/profile_launch, not loaded or inspected by this compiler.
//!
//! §10: pure lib layer — no panic on malformed input; every parse/validate path
//! returns `Result<_, ModelError>` (mirrors Python `ValidationError`).

use std::fs;
use std::path::Path;

use crate::communication_mode::CommunicationMode;
use crate::model::enums::{Provider, ProviderEffort};
use crate::model::yaml::Value;
use crate::model::{paths, spec, yaml, ModelError};
use crate::provider::wire::parse_canonical_provider;

pub const IGNORED_OWNER_TEAM_ID_FIELD: &str = "owner_team_id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredTeamField {
    pub field: &'static str,
    pub value: String,
}

/// Read optional YAML front matter; malformed or non-object headers are plain text.
pub fn read_front_matter(path: &Path) -> Result<(Value, String), ModelError> {
    let text = fs::read_to_string(path)
        .map_err(|e| ModelError::Runtime(format!("{}: {e}", path.display())))?;
    Ok(split_front_matter(&text))
}

/// Parse the same captured bytes that a prompt receipt hashes; the existing
/// path-based compiler keeps its original normalization/plain-text fallback.
pub(crate) fn split_front_matter(text: &str) -> (Value, String) {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let empty_meta = || Value::Map(Vec::new());
    let Some(rest) = text.strip_prefix("---\n") else {
        return (empty_meta(), text);
    };

    let mut offset = 0;
    let mut close = None;
    for line in rest.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == "---" {
            close = Some(offset);
            break;
        }
        offset += line.len();
    }
    let Some(close) = close else {
        return (empty_meta(), text);
    };
    let raw_meta = &rest[..close];
    let meta = if raw_meta.trim().is_empty() {
        empty_meta()
    } else {
        match yaml::loads(raw_meta) {
            Ok(meta) if meta.is_map() => meta,
            _ => return (empty_meta(), text),
        }
    };
    let after_marker = &rest[close + 3..];
    (meta, after_marker.trim_start_matches('\n').to_string())
}

pub fn ignored_owner_team_id_from_team_md(
    team_dir: &Path,
) -> Result<Option<IgnoredTeamField>, ModelError> {
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
    let team_communication_mode =
        communication_mode_field(&team_meta, &team_md)?.unwrap_or_default();
    let mut role_paths = Vec::new();
    if agents_dir.is_dir() {
        for entry in fs::read_dir(&agents_dir)
            .map_err(|e| ModelError::Runtime(format!("{}: {e}", agents_dir.display())))?
        {
            let entry =
                entry.map_err(|e| ModelError::Runtime(format!("{}: {e}", agents_dir.display())))?;
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
    let team_name =
        string_field(&team_meta, "name").unwrap_or_else(|| workspace_dir_name(&workspace));
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
        let compiled =
            compile_role_agent_with_mode(&path, &team_meta, &workspace_s, team_communication_mode)?;
        agent_ids.push(compiled.id);
        agents.push(compiled.agent);
    }

    let default_assignee = agent_ids.first().cloned().unwrap_or_default();
    let routing_rules = agent_ids
        .iter()
        .map(|id| {
            map(vec![
                ("id", Value::Str(format!("route-{id}"))),
                (
                    "match",
                    map(vec![("assignee", list_str(vec![id.as_str()]))]),
                ),
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
                    "{}: unknown provider_effort '{value}' (allowed: low|medium|high|xhigh|max|ultra)",
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
                (
                    "worker_to_worker",
                    bool_field(&team_meta, "worker_to_worker", true),
                ),
                ("ack_timeout_sec", Value::Int(60)),
                (
                    "result_format",
                    Value::Str("result_envelope_v1".to_string()),
                ),
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
                    "session_name",
                    Value::Str(session_name(&team_meta, &team_name)),
                ),
                ("auto_launch", Value::Bool(true)),
                ("require_user_approval_before_launch", Value::Bool(true)),
                (
                    "max_active_agents",
                    Value::Int(max_active_agents(agent_ids.len())),
                ),
                ("startup_order", list_str(agent_ids)),
                ("fast", bool_field(&team_meta, "fast", false)),
                (
                    "tick_interval_sec",
                    int_field(&team_meta, "tick_interval_sec", 2),
                ),
                (
                    "push_min_interval_sec",
                    int_field(&team_meta, "push_min_interval_sec", 60),
                ),
                (
                    "stuck_timeout_sec",
                    int_field(&team_meta, "stuck_timeout_sec", 300),
                ),
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
                (
                    "title",
                    Value::Str("Initial document-driven team task".to_string()),
                ),
                ("type", Value::Str("implementation".to_string())),
                ("assignee", Value::Str(default_assignee)),
                ("deps", Value::List(Vec::new())),
                (
                    "acceptance",
                    list_str(vec!["Worker reports valid result_envelope_v1"]),
                ),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiModelPreflightError {
    pub requested: String,
    pub candidates: Vec<String>,
    pub action: String,
    pub not_ready: bool,
}

impl std::fmt::Display for PiModelPreflightError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Pi role model {:?} is not a qualified exact model",
            self.requested
        )
    }
}

/// Shared admission gate: omitted and qualified models pass; only an
/// unqualified present model causes one bounded catalog observation.
pub fn preflight_pi_role_model(meta: &Value) -> Result<(), PiModelPreflightError> {
    preflight_pi_role_model_with(meta, |requested| {
        crate::lifecycle::launch::pi_mcp::pi_model_candidates(requested).map_err(|_| ())
    })
}

pub fn preflight_pi_role_model_with<F>(
    meta: &Value,
    mut discover: F,
) -> Result<(), PiModelPreflightError>
where
    F: FnMut(&str) -> Result<Vec<String>, ()>,
{
    if parse_canonical_provider(meta.get("provider").and_then(Value::as_str).unwrap_or(""))
        != Some(Provider::Pi)
    {
        return Ok(());
    }
    let Some(model) = string_field(meta, "model").filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    let requested = model.trim().to_string();
    if requested.split_once('/').is_some_and(|(provider, name)| {
        !provider.is_empty() && !name.is_empty() && !requested.contains('*')
    }) {
        return Ok(());
    }
    let (candidates, not_ready) = match discover(&requested) {
        Ok(candidates) => (candidates, false),
        Err(()) => (Vec::new(), true),
    };
    let action = if candidates.is_empty() {
        format!("run `team-agent models --provider pi --search {requested}`")
    } else {
        format!("copy a candidate into the role, or run `team-agent models --provider pi --search {requested}`")
    };
    Err(PiModelPreflightError {
        requested,
        candidates,
        action,
        not_ready,
    })
}

pub fn preflight_pi_models_in_team(team_dir: &Path) -> Result<(), PiModelPreflightError> {
    preflight_pi_models_in_team_with(team_dir, |requested| {
        crate::lifecycle::launch::pi_mcp::pi_model_candidates(requested).map_err(|_| ())
    })
}

pub fn preflight_pi_models_in_team_with<F>(
    team_dir: &Path,
    mut discover: F,
) -> Result<(), PiModelPreflightError>
where
    F: FnMut(&str) -> Result<Vec<String>, ()>,
{
    let agents_dir = team_dir.join("agents");
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(PiModelPreflightError {
                requested: "<role directory>".into(),
                candidates: Vec::new(),
                action: "repair the role directory and retry".into(),
                not_ready: true,
            })
        }
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let (meta, _) = read_front_matter(&path).map_err(|_| PiModelPreflightError {
            requested: path.display().to_string(),
            candidates: Vec::new(),
            action: "repair the role document and retry".into(),
            not_ready: true,
        })?;
        preflight_pi_role_model_with(&meta, &mut discover)?;
    }
    Ok(())
}

/// 把一份 role 文档编译成 agent spec 条目。`team_meta` 供 model/auth_mode 继承;
/// `workspace_s` 是 working_directory。**纯读 `role_path`,无任何文件落地。**
pub fn compile_role_agent(
    role_path: &Path,
    team_meta: &Value,
    workspace_s: &str,
) -> Result<CompiledRole, ModelError> {
    let team_communication_mode =
        communication_mode_field(team_meta, role_path)?.unwrap_or_default();
    compile_role_agent_with_mode(role_path, team_meta, workspace_s, team_communication_mode)
}

fn compile_role_agent_with_mode(
    role_path: &Path,
    team_meta: &Value,
    workspace_s: &str,
    team_communication_mode: CommunicationMode,
) -> Result<CompiledRole, ModelError> {
    let (meta, body) = read_front_matter(role_path)?;
    let communication_mode =
        communication_mode_field(&meta, role_path)?.unwrap_or(team_communication_mode);
    let id = string_field(&meta, "agent_id")
        .or_else(|| string_field(&meta, "name"))
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            role_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "worker".to_string());
    let role = string_field(&meta, "role")
        .filter(|role| !role.trim().is_empty())
        .unwrap_or_else(|| id.clone());
    let provider = string_field(&meta, "provider")
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or_else(|| "pi".to_string());
    let is_pi = parse_canonical_provider(&provider) == Some(Provider::Pi);
    #[cfg(unix)]
    let contract = crate::contract_runtime::registry::descriptor(&provider);
    #[cfg(unix)]
    let contract_role = contract.is_some();
    #[cfg(not(unix))]
    let contract_role = false;
    #[cfg(unix)]
    let skip_team_effort =
        contract.is_some_and(|descriptor| !descriptor.effort.inherit_team_default);
    #[cfg(not(unix))]
    let skip_team_effort = false;
    validate_pi_role_fields(&meta, role_path, &provider)?;
    let model = string_field(&meta, "model")
        .filter(|value| !value.trim().is_empty())
        .map(Value::Str)
        .unwrap_or(Value::Null);
    let auth_mode = string_field(&meta, "auth_mode")
        .or_else(|| string_field(team_meta, "default_auth_mode"))
        .unwrap_or_else(|| "subscription".to_string());
    if is_pi && auth_mode != "subscription" {
        return Err(ModelError::Validation(format!(
            "{}: Pi roles support subscription auth_mode only",
            role_path.display()
        )));
    }
    if auth_mode != "subscription" && meta.get("profile").is_none() {
        return Err(ModelError::Validation(format!(
            "{}: profile is required when auth_mode is '{auth_mode}'",
            role_path.display(),
        )));
    }
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
        (
            "dangerously_skip_permissions",
            Value::Bool(optional_dangerously_skip_permissions(&meta)),
        ),
        (
            "communication_mode",
            Value::Str(communication_mode.as_str().to_string()),
        ),
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
    // Role effort wins; Pi uses only an explicit role value. Other providers may
    // inherit TEAM provider_effort for Issue #238 compatibility.
    let role_effort = match string_field(&meta, "effort") {
        Some(raw) if !raw.trim().is_empty() => {
            let value = raw.trim();
            let parsed = ProviderEffort::parse(value).ok_or_else(|| {
                ModelError::Validation(format!(
                    "{}: unknown effort '{value}' (allowed: low|medium|high|xhigh|max|ultra)",
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
    let resolved_effort = if is_pi || skip_team_effort {
        role_effort
    } else {
        role_effort.or(team_effort)
    };
    if let Some(effort) = resolved_effort {
        // Apply the shared provider admission policy at compile time.
        let provider_str = agent_items
            .iter()
            .find(|(k, _)| *k == "provider")
            .and_then(|(_, v)| match v {
                Value::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("");
        #[cfg(unix)]
        if let Some(descriptor) = contract {
            use team_agent_contract::contract::descriptor::{Effort, EffortAdmission};
            let native_effort = Effort::parse(effort.as_str())
                .map_err(|error| ModelError::Validation(error.to_string()))?;
            if let EffortAdmission::Reject(reason) =
                descriptor.effort.admission[native_effort.index()]
            {
                return Err(ModelError::Validation(format!(
                    "{}: {}",
                    role_path.display(),
                    reason.message
                )));
            }
        }
        if !contract_role {
            let provider_enum = parse_canonical_provider(provider_str).unwrap_or(Provider::Codex);
            if let Err(reason) = effort.resolve_for_provider(provider_enum) {
                return Err(ModelError::Validation(format!(
                    "{}: {reason} (effort: {}; provider: {provider_str})",
                    role_path.display(),
                    effort.as_str()
                )));
            }
        }
        agent_items.push(("effort", Value::Str(effort.as_str().to_string())));
    }
    Ok(CompiledRole {
        id,
        role,
        agent: map(agent_items),
    })
}

fn communication_mode_field(
    meta: &Value,
    path: &Path,
) -> Result<Option<CommunicationMode>, ModelError> {
    let Some(raw) = meta.get("communication_mode") else {
        return Ok(None);
    };
    let Some(value) = raw.as_str() else {
        return Err(ModelError::Validation(format!(
            "{}: unknown communication_mode (allowed: {})",
            path.display(),
            CommunicationMode::ALL
                .iter()
                .map(|mode| mode.as_str())
                .collect::<Vec<_>>()
                .join("|")
        )));
    };
    CommunicationMode::parse(value).map(Some).ok_or_else(|| {
        ModelError::Validation(format!(
            "{}: unknown communication_mode '{value}' (allowed: {})",
            path.display(),
            CommunicationMode::ALL
                .iter()
                .map(|mode| mode.as_str())
                .collect::<Vec<_>>()
                .join("|")
        ))
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
    meta.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn validate_pi_role_fields(meta: &Value, path: &Path, provider: &str) -> Result<(), ModelError> {
    if parse_canonical_provider(provider) != Some(Provider::Pi) {
        return Ok(());
    }
    if let Some(model) = string_field(meta, "model").filter(|value| !value.trim().is_empty()) {
        let (catalog, name) = model.trim().split_once('/').ok_or_else(|| {
            ModelError::Validation(format!(
                "{}: Pi roles require a qualified exact model such as provider/model",
                path.display()
            ))
        })?;
        if catalog.is_empty() || name.is_empty() || model.contains('*') {
            return Err(ModelError::Validation(format!(
                "{}: Pi roles require a qualified exact model",
                path.display()
            )));
        }
    }
    Ok(())
}

fn optional_dangerously_skip_permissions(meta: &Value) -> bool {
    match meta.get("dangerously_skip_permissions") {
        Some(Value::Bool(value)) => *value,
        _ => false,
    }
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

#[cfg(test)]
mod pi_preflight_tests {
    use super::*;
    use std::cell::Cell;

    fn role(provider: &str, model: Option<&str>) -> Value {
        let mut fields = vec![("provider".to_string(), Value::Str(provider.into()))];
        if let Some(model) = model {
            fields.push(("model".to_string(), Value::Str(model.into())));
        }
        Value::Map(fields)
    }

    #[test]
    fn omitted_qualified_and_non_pi_do_not_discover() {
        for meta in [
            role("pi", None),
            role("pi", Some("openai/gpt-5")),
            role("codex", Some("gpt-5")),
        ] {
            let calls = Cell::new(0);
            assert!(preflight_pi_role_model_with(&meta, |_| {
                calls.set(calls.get() + 1);
                Ok(vec![])
            })
            .is_ok());
            assert_eq!(calls.get(), 0);
        }
    }

    #[test]
    fn unqualified_discovers_once_and_projects_candidates() {
        let calls = Cell::new(0);
        let error = preflight_pi_role_model_with(&role("pi", Some("gpt-5.6-sol")), |requested| {
            calls.set(calls.get() + 1);
            assert_eq!(requested, "gpt-5.6-sol");
            Ok(vec![
                "openai-codex/gpt-5.6-sol".into(),
                "azure/gpt-5.6-sol".into(),
            ])
        })
        .expect_err("unqualified model must fail closed");
        assert_eq!(calls.get(), 1);
        assert_eq!(
            error.candidates,
            vec!["openai-codex/gpt-5.6-sol", "azure/gpt-5.6-sol"]
        );
        assert!(!error.not_ready);
        assert!(error
            .action
            .contains("team-agent models --provider pi --search gpt-5.6-sol"));
    }

    #[test]
    fn catalog_failure_is_not_ready_without_partial_candidates() {
        let error = preflight_pi_role_model_with(&role("pi", Some("gpt-5.6-sol")), |_| Err(()))
            .expect_err("catalog failure must reject");
        assert!(error.not_ready);
        assert!(error.candidates.is_empty());
        assert!(!error.to_string().contains("stderr"));
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

fn non_empty_trimmed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn workspace_dir_name(workspace: &Path) -> String {
    workspace
        .file_name()
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
