//! Provider-independent instruction source policy. The Kiro adapter receives
//! one resolved prompt, never a provider-private home/path search algorithm.
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::types::Digest;
use team_agent_contract::host::digest;

use super::config::ConfigError;
use crate::communication_mode::CommunicationMode;
use crate::lifecycle::worker_command_context::{runtime_contract_section, worker_identity_section};
use crate::model::yaml::Value;

pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;

/// Source contents are private; public diagnostics project only their receipts.
#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    pub contents: String,
}
impl Source {
    pub fn read(path: &Path) -> Result<Self, ConfigError> {
        let resolved = path.canonicalize()?;
        let mut bytes = Vec::new();
        std::fs::File::open(&resolved)?.take(MAX_PROMPT_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROMPT_BYTES {
            return Err(ConfigError::Unsupported("instruction source exceeds the bounded prompt limit; no truncation is performed"));
        }
        let contents = String::from_utf8(bytes).map_err(|_| ConfigError::Invalid { path: path.into(), field: "UTF-8 instructions" })?;
        if contents.contains('\0') { return Err(ConfigError::Invalid { path: path.into(), field: "NUL in instructions" }); }
        Ok(Self { path: resolved, contents })
    }
    fn optional(path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(_) => Self::read(path).map(Some),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layer { UserGlobal, Project, Role, TeamExplicit }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionReceipt {
    pub layer: Layer,
    pub path: PathBuf,
    pub sha256: Digest,
    pub bytes: usize,
    /// Exact UTF-8 byte range in the assembled native prompt, excluding headings.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    pub sha256: Digest,
    pub sources: Vec<InstructionReceipt>,
}

/// Central compatibility default, not a Kiro adapter constant. An explicit
/// unified TEAM_AGENT_USER_INSTRUCTIONS file takes precedence and is required
/// to exist. Without an override the authorized user-level AGENTS is optional.
pub fn user_source() -> Result<Option<Source>, ConfigError> {
    if let Some(path) = std::env::var_os("TEAM_AGENT_USER_INSTRUCTIONS") {
        if path.is_empty() { return Err(ConfigError::Unsupported("TEAM_AGENT_USER_INSTRUCTIONS must name a file")); }
        return Source::read(Path::new(&path)).map(Some);
    }
    match std::env::var_os("HOME") {
        Some(home) => Source::optional(&PathBuf::from(home).join(".pi/agent/AGENTS.md")),
        None => Ok(None),
    }
}

/// Selection follows the explicit project workspace, not the roles directory,
/// generated .kiro directory, current shell directory, or an ancestor guess.
pub fn project_source(workspace: &Path) -> Result<Option<Source>, ConfigError> {
    Source::optional(&workspace.join("AGENTS.md"))
}

/// instruction_files is an explicit path or ordered path list. instructions is
/// either inline text (retained verbatim in TEAM.md) or an ordered file-list
/// alias. Paths are relative to TEAM.md, never the generated native config.
pub fn team_sources(team: &Source, meta: &Value) -> Result<Vec<Source>, ConfigError> {
    let mut paths = Vec::new();
    for field in ["instruction_files", "instructions"] {
        match meta.get(field) {
            None | Some(Value::Null) => {},
            Some(Value::Str(_)) if field == "instructions" => {},
            Some(Value::Str(path)) => paths.push(path),
            Some(Value::List(values)) => {
                for value in values {
                    match value { Value::Str(path) => paths.push(path), _ => return Err(ConfigError::Invalid { path: team.path.clone(), field }) }
                }
            },
            _ => return Err(ConfigError::Invalid { path: team.path.clone(), field }),
        }
    }
    let mut sources = vec![team.clone()];
    for path in paths {
        if path.trim().is_empty() || path.contains('\0') { return Err(ConfigError::Invalid { path: team.path.clone(), field: "instruction file path" }); }
        let path = Path::new(path);
        let path = if path.is_absolute() { path.to_owned() } else { team.path.parent().ok_or(ConfigError::Unsupported("TEAM.md has no parent"))?.join(path) };
        sources.push(Source::read(&path)?);
    }
    Ok(sources)
}

/// Framework binding precedes the four ordered user-policy layers. Original
/// documents (including role front matter and CR/LF bytes) are copied whole;
/// only section separators are new bytes. Dynamic tasks are NOT repeated here:
/// they enter the same native context later through the one durable outbox.
pub fn assemble(
    id: &str, role_name: &str, mode: CommunicationMode,
    user: Option<&Source>, project: Option<&Source>, role: &Source, team: &[Source],
) -> Result<Prompt, ConfigError> {
    let mut text = [
        worker_identity_section(id, role_name),
        runtime_contract_section("send_message", "report_result", "get_team_status()"),
        mode.runtime_contract("send_message"),
        "The operations above are logical names. Use the bound MCP client's advertised tool names, not guessed prefixes. Original role-template metadata below does not change your bound worker identity or selected native launch parameters.\nFinal completion calls report_result exactly once. Durable storage and actual leader presentation are separate facts; do not claim delivery from registration or persistence alone.".into(),
    ].join("\n\n");
    let mut ordered = Vec::new();
    if let Some(source) = user { ordered.push((Layer::UserGlobal, source)); }
    if let Some(source) = project { ordered.push((Layer::Project, source)); }
    ordered.push((Layer::Role, role));
    ordered.extend(team.iter().map(|source| (Layer::TeamExplicit, source)));
    let mut sources = Vec::new();
    for (layer, source) in ordered {
        text.push_str(&format!("\n\n# Team Agent instruction layer: {layer:?}\n\n"));
        let start = text.len();
        text.push_str(&source.contents);
        if text.len() > MAX_PROMPT_BYTES { return Err(ConfigError::Unsupported("assembled prompt exceeds the bounded limit; no truncation is performed")); }
        sources.push(InstructionReceipt { layer, path: source.path.clone(), sha256: digest(source.contents.as_bytes()), bytes: source.contents.len(), start, end: text.len() });
    }
    Ok(Prompt { sha256: digest(text.as_bytes()), text, sources })
}
