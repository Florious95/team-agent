//! Read-only per-agent routing/preflight. Contract roles opt in independently;
//! other roles keep the legacy engine, including in the same logical team.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::{session::CwdIdentity, types::{ProviderId, ScopeId, SeatId}};
use team_agent_contract::host::{digest, digest_hex, process::resolve_cwd};
use thiserror::Error;

use crate::cli::QuickStartArgs;
use crate::communication_mode::CommunicationMode;
use crate::compiler::split_front_matter;
use crate::model::yaml::Value;
use super::prompt::{self, Prompt, Source};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid contract configuration field {field} in {path:?}")]
    Invalid { path: PathBuf, field: &'static str },
    #[error("contract runtime capability is not available: {0}")]
    Unsupported(&'static str),
    #[error("contract configuration I/O: {0}")]
    Io(#[from] std::io::Error),
}

// Prompts intentionally have no Debug implementation and are never diagnostics.
#[derive(Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    pub id: SeatId,
    pub provider: ProviderId,
    pub source: PathBuf,
    pub role: String,
    pub model: String,
    pub effort: Option<String>,
    pub bypass: bool,
    pub body: String,
    pub prompt: Prompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeFamily { Legacy, Contract }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRoute {
    pub id: String,
    pub provider: String,
    pub source: PathBuf,
    pub runtime: RuntimeFamily,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub selector: String,
    pub label: String,
    pub scope: ScopeId,
    pub team_dir: PathBuf,
    pub workspace: CwdIdentity,
    pub objective: String,
    pub members: Vec<MemberRoute>,
    /// Only contract roles; the legacy member definitions are not rewritten.
    pub roles: Vec<RoleConfig>,
}

fn invalid(path: &Path, field: &'static str) -> ConfigError {
    ConfigError::Invalid { path: path.into(), field }
}
fn text(meta: &Value, field: &'static str, path: &Path) -> Result<Option<String>, ConfigError> {
    match meta.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(value)) if !value.trim().is_empty() && !value.contains('\0') => Ok(Some(value.clone())),
        _ => Err(invalid(path, field)),
    }
}
fn required(meta: &Value, field: &'static str, path: &Path) -> Result<String, ConfigError> {
    text(meta, field, path)?.ok_or_else(|| invalid(path, field))
}
fn mode(meta: &Value, path: &Path) -> Result<Option<CommunicationMode>, ConfigError> {
    text(meta, "communication_mode", path)?
        .map(|value| CommunicationMode::parse(&value).ok_or_else(|| invalid(path, "communication_mode")))
        .transpose()
}

/// A malformed explicit Kiro header cannot fall back to the default Pi worker.
fn malformed_kiro_header(body: &str) -> bool {
    body.strip_prefix("---\n").is_some_and(|header| {
        header.lines().take_while(|line| *line != "---").any(|line| {
            line.split_once(':').is_some_and(|(key, value)| {
                key.trim() == "provider"
                    && super::registry::recognizes(value.split('#').next().unwrap_or("").trim().trim_matches(['\'', '"']))
            })
        })
    })
}

/// None delegates exactly to the existing no-Kiro/leader-only path. No native
/// command, model probe, config write or process is created by this selection.
pub fn read_team(args: &QuickStartArgs) -> Result<Option<TeamConfig>, ConfigError> {
    read_team_with_user_source(args, prompt::user_source)
}

/// A unified framework user-policy source can be injected without making the
/// provider know its storage location. Tests use this port instead of real HOME.
pub fn read_team_with_user_source(
    args: &QuickStartArgs,
    user_source: impl FnOnce() -> Result<Option<Source>, ConfigError>,
) -> Result<Option<TeamConfig>, ConfigError> {
    let agents = args.agents_dir.join("agents");
    let entries = match std::fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") { paths.push(path); }
    }
    paths.sort();
    let mut roles = Vec::new();
    let mut selected = false;
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if selected => return Err(error.into()),
            Err(_) => return Ok(None), // let the legacy compiler report its existing I/O error
        };
        let (meta, body) = split_front_matter(&raw);
        let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
        if meta.get("provider").is_none() && malformed_kiro_header(&normalized) {
            return Err(invalid(&path, "front matter"));
        }
        if meta.get("provider").and_then(Value::as_str).is_some_and(super::registry::recognizes) {
            selected = true;
        }
        roles.push((path, raw, meta, body));
    }
    if !selected { return Ok(None); }
    if args.backend.as_deref().is_some_and(|backend| backend != "tmux") {
        return Err(ConfigError::Unsupported("Kiro contract transport requires tmux"));
    }
    let team_dir = args.agents_dir.canonicalize()?;
    let team_path = team_dir.join("TEAM.md");
    let team_source = Source::read(&team_path)?;
    let (team_meta, team_body) = split_front_matter(&team_source.contents);
    let label = args.name.clone().or(text(&team_meta, "name", &team_path)?)
        .or_else(|| team_dir.file_name().and_then(|s| s.to_str()).map(str::to_owned))
        .ok_or_else(|| invalid(&team_path, "name"))?;
    let selector = args.team_id.clone().unwrap_or_else(|| label.clone());
    if selector.trim().is_empty() || selector.chars().any(char::is_control) {
        return Err(invalid(&team_path, "team selector"));
    }
    let workspace = resolve_cwd(&args.workspace).map_err(|_| invalid(&args.workspace, "workspace identity"))?;
    let key = serde_json::to_vec(&(&workspace, &selector)).map_err(|_| invalid(&team_path, "scope"))?;
    let scope = ScopeId::new(format!("contract-{}", &digest_hex(digest(&key))[..32]))
        .map_err(|_| invalid(&team_path, "scope"))?;
    let objective = text(&team_meta, "objective", &team_path)?.unwrap_or(team_body);
    let user = user_source()?;
    let project = prompt::project_source(&workspace.path)?;
    let team_instructions = prompt::team_sources(&team_source, &team_meta)?;
    let team_mode = mode(&team_meta, &team_path)?.unwrap_or_default();
    let mut ids = BTreeSet::new();
    let mut compiled = Vec::new();
    let mut members = Vec::new();
    for (source, raw, meta, body) in roles {
        let id = text(&meta, "agent_id", &source)?.or(text(&meta, "name", &source)?)
            .or_else(|| source.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
            .ok_or_else(|| invalid(&source, "agent id"))?;
        if !ids.insert(id.clone()) { return Err(invalid(&source, "duplicate agent id")); }
        let provider = text(&meta, "provider", &source)?.unwrap_or_else(|| "pi".into());
        let registration = super::registry::descriptor(&provider);
        if registration.is_none() && super::registry::recognizes(&provider) {
            return Err(invalid(&source, "provider"));
        }
        let runtime = if registration.is_some() { RuntimeFamily::Contract } else { RuntimeFamily::Legacy };
        members.push(MemberRoute { id: id.clone(), provider, source: source.canonicalize()?, runtime });
        let Some(registration) = registration else { continue; };
        let provider = ProviderId::new(registration.identity.id).map_err(|_| invalid(&source, "provider"))?;
        let id = SeatId::new(id).map_err(|_| invalid(&source, "agent id"))?;
        if id.as_str() == "leader" { return Err(invalid(&source, "reserved worker id")); }
        if text(&meta, "auth_mode", &source)?.is_some_and(|value| value != "subscription")
            || text(&meta, "profile", &source)?.is_some() {
            return Err(ConfigError::Unsupported("Kiro uses an existing native subscription; API/profile mappings are unavailable"));
        }
        let bypass = match meta.get("dangerously_skip_permissions") {
            Some(Value::Bool(value)) => *value,
            _ => return Err(invalid(&source, "dangerously_skip_permissions")),
        };
        if body.trim().is_empty() || body.contains('\0') { return Err(invalid(&source, "role body")); }
        if ["args", "cli_args"].iter().any(|field| meta.get(field).is_some()) {
            return Err(ConfigError::Unsupported("unregistered native arguments cannot override the managed launch plan"));
        }
        let role = text(&meta, "role", &source)?.unwrap_or_else(|| id.as_str().into());
        let document = Source { path: source.canonicalize()?, contents: raw };
        let prompt = prompt::assemble(id.as_str(), &role, mode(&meta, &source)?.unwrap_or(team_mode), user.as_ref(), project.as_ref(), &document, &team_instructions)?;
        compiled.push(RoleConfig {
            id, provider, source: document.path, role, model: required(&meta, "model", &source)?,
            effort: text(&meta, "effort", &source)?, bypass, body, prompt,
        });
    }
    Ok(Some(TeamConfig { selector, label, scope, team_dir, workspace, objective, members, roles: compiled }))
}
