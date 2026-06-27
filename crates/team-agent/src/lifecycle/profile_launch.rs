use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::lifecycle::LifecycleError;
use crate::model::enums::{AuthMode, Provider};
use crate::model::yaml::Value as YamlValue;
use crate::provider::{McpConfig, ProviderCommandOverrides, ProviderProfileLaunch};

const COMPATIBLE_NETWORK_ENV_KEYS: &[&str] = &[
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
];

#[derive(Debug, Clone)]
pub(crate) struct ProfileValues {
    pub(crate) path: PathBuf,
    pub(crate) values: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct AgentProfileInput {
    id: String,
    provider: Provider,
    auth_mode: AuthMode,
    profile: Option<String>,
    profile_dir: Option<PathBuf>,
    model: Option<String>,
    model_source: Option<String>,
}

pub(crate) fn prepare_provider_profile_launch(
    workspace: &Path,
    agent_id: &str,
    agent: &YamlValue,
    mcp_config: Option<&McpConfig>,
) -> Result<ProviderProfileLaunch, LifecycleError> {
    prepare_provider_profile_launch_with_profile_dir(workspace, agent_id, agent, None, mcp_config)
}

pub(crate) fn prepare_provider_profile_launch_with_profile_dir(
    workspace: &Path,
    agent_id: &str,
    agent: &YamlValue,
    profile_dir: Option<&Path>,
    mcp_config: Option<&McpConfig>,
) -> Result<ProviderProfileLaunch, LifecycleError> {
    let input = AgentProfileInput {
        id: agent
            .get("id")
            .and_then(YamlValue::as_str)
            .unwrap_or(agent_id)
            .to_string(),
        provider: agent
            .get("provider")
            .and_then(YamlValue::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex),
        auth_mode: agent
            .get("auth_mode")
            .and_then(YamlValue::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription),
        profile: agent
            .get("profile")
            .and_then(YamlValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        profile_dir: agent
            .get("_profile_dir")
            .and_then(YamlValue::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| profile_dir.map(Path::to_path_buf)),
        model: agent
            .get("model")
            .and_then(YamlValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        model_source: agent
            .get("model_source")
            .and_then(YamlValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    };
    prepare_profile_launch(workspace, input, mcp_config)
}

pub(crate) fn prepare_provider_profile_launch_from_json(
    workspace: &Path,
    agent_id: &str,
    agent: &serde_json::Value,
    mcp_config: Option<&McpConfig>,
) -> Result<ProviderProfileLaunch, LifecycleError> {
    let input = AgentProfileInput {
        id: agent
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_id)
            .to_string(),
        provider: agent
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex),
        auth_mode: agent
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription),
        profile: agent
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        profile_dir: agent
            .get("_profile_dir")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        model: agent
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        model_source: agent
            .get("model_source")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    };
    prepare_profile_launch(workspace, input, mcp_config)
}

fn prepare_profile_launch(
    workspace: &Path,
    agent: AgentProfileInput,
    mcp_config: Option<&McpConfig>,
) -> Result<ProviderProfileLaunch, LifecycleError> {
    let Some(profile) = agent.profile.as_deref() else {
        return Ok(ProviderProfileLaunch::default());
    };
    let loaded = load_profile(workspace, profile, agent.profile_dir.as_deref())?;
    validate_profile(&agent, profile, &loaded)?;

    let mut env_overlay = provider_env_exports(agent.provider, agent.auth_mode, &loaded.values);
    let mut env_unset = provider_env_unsets(agent.provider, agent.auth_mode);
    let mut claude_config_dir = None;
    let mut claude_projects_root = None;
    let mut managed_mcp_config = false;

    if matches!(agent.provider, Provider::Claude | Provider::ClaudeCode)
        && agent.auth_mode == AuthMode::CompatibleApi
    {
        let dir = compatible_claude_config_dir(workspace, &agent.id)?;
        if let Some(config) = mcp_config {
            ensure_compatible_claude_mcp_config(&dir, workspace, config)?;
            managed_mcp_config = true;
        }
        let projects_root = dir.join("projects");
        env_overlay.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().to_string(),
        );
        claude_projects_root = Some(projects_root);
        claude_config_dir = Some(dir);
    }

    env_overlay.extend(compatible_api_network_exports(agent.auth_mode, &loaded.values));
    if agent.auth_mode == AuthMode::CompatibleApi && profile_proxy_mode(&loaded.values) == "direct"
    {
        for key in COMPATIBLE_NETWORK_ENV_KEYS {
            env_unset.insert((*key).to_string());
            env_overlay.remove(*key);
        }
    }

    let command_overrides = provider_command_overrides(&agent, &loaded.values);
    write_runtime_env_file(workspace, &agent.id, &env_overlay, &env_unset)?;

    Ok(ProviderProfileLaunch {
        env_overlay,
        env_unset,
        command_overrides,
        claude_config_dir,
        claude_projects_root,
        managed_mcp_config,
    })
}

pub(crate) fn load_profile(
    workspace: &Path,
    name: &str,
    profile_dir: Option<&Path>,
) -> Result<ProfileValues, LifecycleError> {
    let dirs = profile_lookup_dirs(workspace, profile_dir);
    let path = dirs
        .iter()
        .map(|directory| directory.join(format!("{name}.env")))
        .find(|path| path.exists())
        .or_else(|| {
            dirs.iter()
                .map(|directory| directory.join(format!("{name}.example.env")))
                .find(|path| path.exists())
        })
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!(
                "profile {name} not found; run team-agent profile init {name} --auth-mode subscription"
            ))
        })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(ProfileValues {
        path,
        values: parse_env_text(&text),
    })
}

fn profile_lookup_dirs(workspace: &Path, profile_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = profile_dir {
        push_unique_path(&mut dirs, dir.to_path_buf());
    }
    push_unique_path(&mut dirs, workspace.join(".team/current/profiles"));
    push_unique_path(&mut dirs, workspace.join("profiles"));
    push_unique_path(&mut dirs, workspace.join(".team/profiles"));
    if workspace.file_name().and_then(|name| name.to_str()) == Some(".team") {
        push_unique_path(&mut dirs, workspace.join("current/profiles"));
    }
    dirs
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn parse_env_text(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim();
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_profile_key(key) {
            continue;
        }
        values.insert(key.to_string(), strip_env_value(value.trim()));
    }
    values
}

fn strip_env_value(raw: &str) -> String {
    let value = raw.trim();
    if value.len() >= 2 {
        if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            return inner.to_string();
        }
        if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            return inner.to_string();
        }
    }
    value.to_string()
}

fn is_profile_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_profile(
    agent: &AgentProfileInput,
    profile: &str,
    loaded: &ProfileValues,
) -> Result<(), LifecycleError> {
    if loaded
        .values
        .get("AUTH_MODE")
        .is_some_and(|mode| parse_auth_mode(mode) != Some(agent.auth_mode))
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "profile {profile} AUTH_MODE does not match agent auth_mode"
        )));
    }
    if agent.auth_mode == AuthMode::CompatibleApi
        && matches!(agent.model_source.as_deref(), Some("role" | "team"))
    {
        let profile_model = profile_model(&loaded.values);
        if let Some(profile_model) = profile_model {
            if agent.model.as_deref().is_some_and(|model| model != profile_model) {
                return Err(LifecycleError::RequirementUnmet(format!(
                    "role/team model does not match profile MODEL in {}",
                    loaded.path.display()
                )));
            }
        }
    }
    let mut missing: Vec<&str> = Vec::new();
    for key in required_profile_keys(agent.provider, agent.auth_mode) {
        if key == &"API_KEY" {
            continue;
        }
        if !profile_value_or_alternate(&loaded.values, key).is_some_and(|value| !value.is_empty())
        {
            missing.push(*key);
        }
    }
    if agent.auth_mode == AuthMode::CompatibleApi
        && effective_profile_or_agent_model(agent, &loaded.values).is_none()
    {
        missing.push("MODEL");
    }
    if !missing.is_empty() {
        return Err(LifecycleError::RequirementUnmet(format!(
            "profile {profile} missing required values: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn provider_env_exports(
    provider: Provider,
    auth_mode: AuthMode,
    values: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut exports = BTreeMap::new();
    if auth_mode == AuthMode::Subscription {
        return exports;
    }
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            if let Some(value) = value_or_alternate(values, "ANTHROPIC_BASE_URL", "BASE_URL") {
                exports.insert("ANTHROPIC_BASE_URL".to_string(), value.to_string());
            }
            if auth_mode == AuthMode::OfficialApi {
                if let Some(value) = value_or_alternate(values, "ANTHROPIC_API_KEY", "API_KEY") {
                    exports.insert("ANTHROPIC_API_KEY".to_string(), value.to_string());
                }
            }
            if let Some(value) = value_or_alternate(values, "ANTHROPIC_AUTH_TOKEN", "AUTH_TOKEN")
                .or_else(|| {
                    (auth_mode == AuthMode::CompatibleApi)
                        .then(|| value_or_alternate(values, "ANTHROPIC_API_KEY", "API_KEY"))
                        .flatten()
                })
            {
                exports.insert("ANTHROPIC_AUTH_TOKEN".to_string(), value.to_string());
            }
            if let Some(value) = profile_model(values) {
                exports.insert("ANTHROPIC_MODEL".to_string(), value.to_string());
            }
        }
        Provider::Codex => {
            if let Some(value) = value_or_alternate(values, "OPENAI_API_KEY", "API_KEY") {
                exports.insert("TEAM_AGENT_PROVIDER_API_KEY".to_string(), value.to_string());
                exports.insert("OPENAI_API_KEY".to_string(), value.to_string());
            }
            if let Some(value) = values.get("BASE_URL").filter(|value| !value.is_empty()) {
                exports.insert("OPENAI_BASE_URL".to_string(), value.to_string());
            }
        }
        Provider::GeminiCli => {
            if let Some(value) = value_or_alternate(values, "GEMINI_API_KEY", "API_KEY") {
                exports.insert("GEMINI_API_KEY".to_string(), value.to_string());
            }
        }
        // C-A-4 cr verdict v2 — copilot BYOK(== compatible_api 档,help-providers 原文
        // "GitHub authentication is not required" when COPILOT_PROVIDER_BASE_URL set):
        // profile 值经 env_overlay 落 COPILOT_PROVIDER_BASE_URL/TYPE/API_KEY/WIRE_API
        // + COPILOT_MODEL。BYOK 必须含 MODEL("A model is required for BYOK")—
        // 这一硬性校验在 launch 路径处理(profile compile 时无法判别 model 来源)。
        // Subscription/OfficialApi 不导出 COPILOT_PROVIDER_*(避免误改 auth 通道)。
        Provider::Copilot => {
            if auth_mode == AuthMode::CompatibleApi {
                if let Some(value) = value_or_alternate(values, "COPILOT_PROVIDER_BASE_URL", "BASE_URL") {
                    exports.insert("COPILOT_PROVIDER_BASE_URL".to_string(), value.to_string());
                }
                if let Some(value) = value_or_alternate(values, "COPILOT_PROVIDER_TYPE", "PROVIDER_TYPE") {
                    exports.insert("COPILOT_PROVIDER_TYPE".to_string(), value.to_string());
                }
                if let Some(value) = value_or_alternate(values, "COPILOT_PROVIDER_API_KEY", "API_KEY") {
                    exports.insert("COPILOT_PROVIDER_API_KEY".to_string(), value.to_string());
                }
                if let Some(value) = value_or_alternate(values, "COPILOT_PROVIDER_WIRE_API", "WIRE_API") {
                    exports.insert("COPILOT_PROVIDER_WIRE_API".to_string(), value.to_string());
                }
                if let Some(value) = value_or_alternate(values, "COPILOT_MODEL", "MODEL") {
                    exports.insert("COPILOT_MODEL".to_string(), value.to_string());
                }
            }
        }
        Provider::Fake => {}
    }
    exports
}

fn provider_env_unsets(provider: Provider, auth_mode: AuthMode) -> BTreeSet<String> {
    let mut unsets = BTreeSet::new();
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            // E57 (0.3.27 P0): unset CLAUDE_CODE_SESSION_ID inherited from the
            // leader's environment. When a Claude leader spawns a Claude worker,
            // the worker inherits the leader's CLAUDE_CODE_SESSION_ID; Claude
            // Code then routes the worker's transcript to the leader's session
            // file (~/.claude/projects/<hash>/<leader-session-uuid>.jsonl),
            // corrupting attribution and transcripts. Always unset so Claude
            // Code generates a fresh session id per worker.
            //
            // S1-CAPTURE (0.4.x P0): also unset the rest of the Claude Code
            // child/team env block. These variables make the spawned process
            // believe it is a CHILD of the parent Claude (CLAUDE_CODE_CHILD_SESSION),
            // a TEAM member (CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS), or otherwise
            // inherit the parent's invocation identity (ENTRYPOINT / EXECPATH /
            // CLAUDECODE marker). Without unsetting these, a Claude worker
            // spawned from a Claude leader is not an INDEPENDENT process — it
            // joins the leader's session tree and its transcript / state /
            // billing are attributed to the leader. CLAUDE_EFFORT is preserved
            // (effort preference does not affect session attribution).
            unsets.insert("CLAUDE_CODE_SESSION_ID".to_string());
            unsets.insert("CLAUDE_CODE_CHILD_SESSION".to_string());
            unsets.insert("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".to_string());
            unsets.insert("CLAUDE_CODE_ENTRYPOINT".to_string());
            unsets.insert("CLAUDE_CODE_EXECPATH".to_string());
            unsets.insert("CLAUDECODE".to_string());
            if auth_mode == AuthMode::CompatibleApi {
                unsets.insert("ANTHROPIC_API_KEY".to_string());
            }
            if auth_mode == AuthMode::OfficialApi {
                unsets.insert("ANTHROPIC_AUTH_TOKEN".to_string());
            }
        }
        Provider::Codex => {
            if auth_mode == AuthMode::CompatibleApi {
                unsets.insert("OPENAI_API_KEY".to_string());
                unsets.insert("OPENAI_BASE_URL".to_string());
            }
        }
        Provider::GeminiCli => {
            if auth_mode == AuthMode::CompatibleApi {
                unsets.insert("GEMINI_API_KEY".to_string());
            }
        }
        // C-7-1 cr verdict: 一期 subscription-only(已登录态),exports/unsets 空;
        // BYOK(COPILOT_PROVIDER_*)二期立项时单独 cr verdict 再开。
        Provider::Copilot => {}
        Provider::Fake => {}
    }
    unsets
}

fn provider_command_overrides(
    agent: &AgentProfileInput,
    values: &BTreeMap<String, String>,
) -> ProviderCommandOverrides {
    let model = match agent.model_source.as_deref() {
        Some("role" | "team") => agent.model.clone(),
        Some("default") => profile_model(values)
            .map(str::to_string)
            .or_else(|| agent.model.clone()),
        _ if agent.model.is_some() => agent.model.clone(),
        _ => profile_model(values).map(str::to_string),
    };
    let mut codex_profile = None;
    let mut codex_config = Vec::new();
    // Python provider_env.py:62-79 (_provider_command_overrides codex branch).
    match agent.provider {
        Provider::Codex => {
            codex_profile = value_or_alternate(values, "CODEX_PROFILE", "NATIVE_PROFILE")
                .map(str::to_string);
            let model_provider = values.get("MODEL_PROVIDER").filter(|v| !v.is_empty());
            let base_url = values.get("BASE_URL").filter(|v| !v.is_empty());
            if agent.auth_mode == AuthMode::CompatibleApi {
                if let (Some(model_provider), Some(base_url)) = (model_provider, base_url) {
                    if safe_codex_provider_id(model_provider) {
                        codex_config.push(format!("model_provider=\"{model_provider}\""));
                        let prefix = format!("model_providers.{model_provider}");
                        codex_config.push(format!("{prefix}.base_url=\"{base_url}\""));
                        codex_config
                            .push(format!("{prefix}.env_key=\"TEAM_AGENT_PROVIDER_API_KEY\""));
                        if let Some(wire_api) = values.get("WIRE_API").filter(|v| !v.is_empty()) {
                            codex_config.push(format!("{prefix}.wire_api=\"{wire_api}\""));
                        }
                        if let Some(name) = values.get("PROVIDER_NAME").filter(|v| !v.is_empty()) {
                            codex_config.push(format!("{prefix}.name=\"{name}\""));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    ProviderCommandOverrides {
        model,
        codex_profile,
        codex_config,
    }
}

/// Python helpers.py:33-34 `_safe_codex_provider_id`: `[A-Za-z0-9_-]+`.
fn safe_codex_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn write_runtime_env_file(
    workspace: &Path,
    agent_id: &str,
    exports: &BTreeMap<String, String>,
    unsets: &BTreeSet<String>,
) -> Result<(), LifecycleError> {
    if exports.is_empty() && unsets.is_empty() {
        return Ok(());
    }
    let dir = workspace.join(".team/runtime/provider-env");
    std::fs::create_dir_all(&dir)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{agent_id}.env"));
    let mut lines = Vec::new();
    for key in unsets {
        lines.push(format!("unset {key}"));
    }
    for (key, value) in exports {
        lines.push(format!("export {key}={}", shell_quote(value)));
    }
    std::fs::write(&path, format!("{}\n", lines.join("\n")))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<(), LifecycleError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(path: &Path) -> Result<(), LifecycleError> {
    let _ = path;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn compatible_claude_config_dir(
    workspace: &Path,
    agent_id: &str,
) -> Result<PathBuf, LifecycleError> {
    let dir = workspace
        .join(".team/runtime/provider-config")
        .join(agent_id)
        .join("claude");
    std::fs::create_dir_all(&dir)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", dir.display())))?;
    ensure_compatible_claude_config(&dir, workspace)?;
    Ok(dir)
}

fn ensure_compatible_claude_config(dir: &Path, workspace: &Path) -> Result<(), LifecycleError> {
    let settings_path = dir.join("settings.json");
    let mut settings = read_json_object(&settings_path)?;
    insert_if_missing(&mut settings, "theme", serde_json::json!("auto"));
    insert_if_missing(
        &mut settings,
        "skipDangerousModePermissionPrompt",
        serde_json::json!(true),
    );
    settings.insert("hasCompletedOnboarding".to_string(), serde_json::json!(true));
    settings.insert("hasTrustDialogAccepted".to_string(), serde_json::json!(true));
    insert_if_missing(
        &mut settings,
        "lastOnboardingVersion",
        serde_json::json!("2.1.0"),
    );
    insert_if_missing(
        &mut settings,
        "firstStartTime",
        serde_json::json!("1970-01-01T00:00:00.000Z"),
    );
    insert_if_missing(&mut settings, "numStartups", serde_json::json!(0));
    insert_if_missing(
        &mut settings,
        "projectOnboardingSeenCount",
        serde_json::json!(1),
    );
    write_json_object(&settings_path, &settings)?;

    let state_path = dir.join(".claude.json");
    let mut state = read_json_object(&state_path)?;
    state.insert("hasCompletedOnboarding".to_string(), serde_json::json!(true));
    insert_if_missing(
        &mut state,
        "lastOnboardingVersion",
        serde_json::json!("2.1.0"),
    );
    insert_if_missing(
        &mut state,
        "firstStartTime",
        serde_json::json!("1970-01-01T00:00:00.000Z"),
    );
    insert_if_missing(&mut state, "numStartups", serde_json::json!(0));
    ensure_claude_projects(&mut state, workspace, None);
    write_json_object(&state_path, &state)
}

fn ensure_compatible_claude_mcp_config(
    dir: &Path,
    workspace: &Path,
    mcp_config: &McpConfig,
) -> Result<(), LifecycleError> {
    let state_path = dir.join(".claude.json");
    let mut state = read_json_object(&state_path)?;
    state.insert("hasCompletedOnboarding".to_string(), serde_json::json!(true));
    state.insert("hasTrustDialogAccepted".to_string(), serde_json::json!(true));
    insert_if_missing(
        &mut state,
        "projectOnboardingSeenCount",
        serde_json::json!(1),
    );
    state.insert("mcpServers".to_string(), mcp_config.raw.clone());
    state.insert(
        "enabledMcpjsonServers".to_string(),
        serde_json::json!(mcp_server_names(&mcp_config.raw)),
    );
    ensure_claude_projects(&mut state, workspace, Some(&mcp_config.raw));
    write_json_object(&state_path, &state)
}

fn ensure_claude_projects(
    state: &mut serde_json::Map<String, serde_json::Value>,
    workspace: &Path,
    mcp_servers: Option<&serde_json::Value>,
) {
    if !state.get("projects").is_some_and(serde_json::Value::is_object) {
        state.insert("projects".to_string(), serde_json::json!({}));
    }
    let Some(projects) = state
        .get_mut("projects")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for key in claude_project_keys(workspace) {
        let entry = projects.entry(key).or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        let Some(project) = entry.as_object_mut() else {
            continue;
        };
        project.insert("hasTrustDialogAccepted".to_string(), serde_json::json!(true));
        insert_if_missing(project, "projectOnboardingSeenCount", serde_json::json!(1));
        if let Some(mcp_servers) = mcp_servers {
            insert_if_missing(project, "allowedTools", serde_json::json!([]));
            insert_if_missing(project, "mcpContextUris", serde_json::json!([]));
            project.insert(
                "enabledMcpjsonServers".to_string(),
                serde_json::json!(mcp_server_names(mcp_servers)),
            );
            insert_if_missing(project, "disabledMcpjsonServers", serde_json::json!([]));
            insert_if_missing(
                project,
                "hasClaudeMdExternalIncludesApproved",
                serde_json::json!(false),
            );
            insert_if_missing(
                project,
                "hasClaudeMdExternalIncludesWarningShown",
                serde_json::json!(false),
            );
            if !project
                .get("mcpServers")
                .is_some_and(serde_json::Value::is_object)
            {
                project.insert("mcpServers".to_string(), serde_json::json!({}));
            }
            if let Some(existing) = project
                .get_mut("mcpServers")
                .and_then(serde_json::Value::as_object_mut)
            {
                if let Some(servers) = mcp_servers.as_object() {
                    existing.extend(servers.clone());
                }
            }
        }
    }
}

fn mcp_server_names(mcp_servers: &serde_json::Value) -> Vec<String> {
    mcp_servers
        .as_object()
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

/// swallow batch 4 C1-C4 (MUST-NOT-9/13): a CORRUPT provider-config JSON fails
/// EXPLICITLY — the old `unwrap_or_default` turned a parse failure into an empty map
/// that the subsequent write flattened back over the user's file (silent destructive
/// rewrite). A missing file stays Ok(empty) — only an existing-but-unparseable file
/// errors, naming the path + parse detail + next action; the file is never touched.
fn read_json_object(
    path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, LifecycleError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::Map::new());
        }
        Err(error) => {
            return Err(LifecycleError::RequirementUnmet(format!(
                "profile_invalid_json: {} could not be read: {error}; fix or remove the file (or restore a backup) and relaunch",
                path.display()
            )));
        }
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(other) => Err(LifecycleError::RequirementUnmet(format!(
            "profile_invalid_json: {} is not a JSON object (found {}); fix or remove the file (or restore a backup) and relaunch",
            path.display(),
            json_kind(&other)
        ))),
        Err(error) => Err(LifecycleError::RequirementUnmet(format!(
            "profile_invalid_json: {} could not be parsed: {error}; fix or remove the file (or restore a backup) and relaunch",
            path.display()
        ))),
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

fn write_json_object(
    path: &Path,
    value: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), LifecycleError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| LifecycleError::StatePersist(format!("serialize {}: {e}", path.display())))?;
    std::fs::write(path, format!("{body}\n"))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    set_owner_only_permissions(path)
}

fn insert_if_missing(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: serde_json::Value,
) {
    map.entry(key.to_string()).or_insert(value);
}

fn claude_project_keys(workspace: &Path) -> Vec<String> {
    let mut keys = vec![workspace.to_string_lossy().to_string()];
    if let Ok(resolved) = workspace.canonicalize() {
        let resolved = resolved.to_string_lossy().to_string();
        if !keys.iter().any(|key| key == &resolved) {
            keys.push(resolved);
        }
    }
    for key in keys.clone() {
        if let Some(stripped) = key.strip_prefix("/private/var/") {
            push_unique_string(&mut keys, format!("/var/{stripped}"));
        } else if let Some(stripped) = key.strip_prefix("/var/") {
            push_unique_string(&mut keys, format!("/private/var/{stripped}"));
        }
    }
    keys
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn compatible_api_network_exports(
    auth_mode: AuthMode,
    values: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    if auth_mode != AuthMode::CompatibleApi || profile_proxy_mode(values) == "direct" {
        return BTreeMap::new();
    }
    COMPATIBLE_NETWORK_ENV_KEYS
        .iter()
        .filter_map(|key| {
            values
                .get(*key)
                .filter(|value| !value.is_empty())
                .map(|value| ((*key).to_string(), value.clone()))
        })
        .collect()
}

pub(crate) fn profile_proxy_mode(values: &BTreeMap<String, String>) -> String {
    values
        .get("PROXY_MODE")
        .or_else(|| values.get("NETWORK_MODE"))
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "inherit".to_string())
}

fn required_profile_keys(provider: Provider, auth_mode: AuthMode) -> &'static [&'static str] {
    if auth_mode == AuthMode::Subscription {
        return &[];
    }
    match provider {
        Provider::Claude | Provider::ClaudeCode | Provider::Codex => match auth_mode {
            AuthMode::OfficialApi => &["API_KEY"],
            AuthMode::CompatibleApi => &["BASE_URL", "API_KEY"],
            AuthMode::Subscription => &[],
        },
        Provider::GeminiCli => &["API_KEY"],
        // C-7-1 cr verdict: copilot 一期 subscription-only,无 BYOK required key 集合;
        // 二期 BYOK 立项时填(向后兼容字段保留)。
        Provider::Copilot => &[],
        Provider::Fake => &[],
    }
}

pub(crate) fn profile_value_or_alternate<'a>(
    values: &'a BTreeMap<String, String>,
    key: &str,
) -> Option<&'a str> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .or_else(|| match key {
            "API_KEY" => values
                .get("ANTHROPIC_API_KEY")
                .or_else(|| values.get("ANTHROPIC_AUTH_TOKEN"))
                .or_else(|| values.get("AUTH_TOKEN"))
                .or_else(|| values.get("CLAUDE_API_KEY"))
                .or_else(|| values.get("CLAUDE_AUTH_TOKEN"))
                .or_else(|| values.get("BEARER_TOKEN"))
                .or_else(|| values.get("TEAM_AGENT_PROVIDER_API_KEY"))
                .or_else(|| values.get("OPENAI_API_KEY")),
            "BASE_URL" => values.get("ANTHROPIC_BASE_URL").or_else(|| values.get("OPENAI_BASE_URL")),
            "MODEL" => values.get("ANTHROPIC_MODEL"),
            _ => None,
        }
        .filter(|value| !value.is_empty())
        .map(String::as_str))
}

pub(crate) fn value_or_alternate<'a>(
    values: &'a BTreeMap<String, String>,
    primary: &str,
    alternate: &str,
) -> Option<&'a str> {
    values
        .get(primary)
        .filter(|value| !value.is_empty())
        .or_else(|| values.get(alternate).filter(|value| !value.is_empty()))
        .map(String::as_str)
}

pub(crate) fn profile_model(values: &BTreeMap<String, String>) -> Option<&str> {
    values
        .get("MODEL")
        .filter(|value| !value.is_empty())
        .or_else(|| values.get("ANTHROPIC_MODEL").filter(|value| !value.is_empty()))
        .map(String::as_str)
}

fn effective_profile_or_agent_model<'a>(
    agent: &'a AgentProfileInput,
    values: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    agent.model.as_deref().or_else(|| profile_model(values))
}

pub(crate) use crate::provider::wire::parse_provider;

pub(crate) fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}
