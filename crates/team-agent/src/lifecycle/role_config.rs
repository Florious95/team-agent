//! CLI convenience is a role-document transaction, not a provider override.
//! Existing file compilation and process/session lifecycle remain authoritative.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value};
use crate::state::repository::{StateRepository, StateWriteIntent};
use crate::state::selector::{SelectedTeam, SelectorMode};
use crate::transport::Transport;

use super::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};
use super::{AddAgentReport, LifecycleError, StartAgentOutcome};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleConfigPatch {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub bypass: Option<bool>,
    pub prompt: Option<String>,
    pub profile: Option<String>,
}

pub fn add_agent_from_role(
    workspace: &Path,
    agent_id: &AgentId,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
    force: bool,
    team: Option<&str>,
) -> Result<AddAgentReport, LifecycleError> {
    if let Some(path) = source {
        if !path.is_file() {
            return Err(LifecycleError::Compile(format!(
                "role file not found: {}",
                path.display()
            )));
        }
    }
    let selected = select(workspace, team)?;
    with_selected(&selected, agent_id, |selected| {
        let transport = super::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        )?
        .backend;
        add_at_selected(selected, agent_id, source, patch, force, &transport)
    })
}

pub fn start_agent_from_role(
    workspace: &Path,
    agent_id: &AgentId,
    patch: &RoleConfigPatch,
    allow_fresh: bool,
    team: Option<&str>,
) -> Result<StartAgentOutcome, LifecycleError> {
    let selected = select(workspace, team)?;
    with_selected(&selected, agent_id, |selected| {
        let transport = super::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        )?
        .backend;
        start_at_selected(selected, agent_id, patch, allow_fresh, &transport)
    })
}

fn select(workspace: &Path, team: Option<&str>) -> Result<SelectedTeam, LifecycleError> {
    crate::state::selector::resolve_active_team_readonly(workspace, team, SelectorMode::RuntimeOnly)
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))
}

fn with_selected<T>(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    operation: impl FnOnce(&SelectedTeam) -> Result<T, LifecycleError>,
) -> Result<T, LifecycleError> {
    validate_id(agent_id)?;
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "role-config",
        team: Some(&selected.team_key),
        agent_id: Some(agent_id),
    })?;
    let latest = select(&selected.run_workspace, Some(&selected.team_key))?;
    super::launch::ensure_owner_allowed_for_state(&latest.state, Some(agent_id))?;
    operation(&latest)
}

fn add_at_selected(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
    force: bool,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let recreate = admit_add(&selected.state, agent_id, force, transport)?;
    let spec = read_spec(selected)?;
    if !recreate && spec_has_agent(&spec, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    let role_path = role_path(selected, agent_id);
    if !recreate {
        check_destination(&role_path, source)?;
    }
    let text = match source {
        Some(path) => read_text(path)?,
        None => String::new(),
    };
    let reconciled = reconcile_creation(&text, patch)?;
    let next = patched_role(&text, agent_id, &reconciled, source.is_none())?;
    let snapshot = RoleSnapshot::capture(selected, &role_path)?;
    let result = (|| {
        if snapshot.role.bytes.as_deref() != Some(next.as_bytes()) {
            atomic_write(&role_path, next.as_bytes())?;
        }
        // Reuse existing lifecycle transactions; force never admits a live seat.
        let add = if recreate {
            super::launch::force_recreate_with_transport_locked
        } else {
            super::launch::add_agent_with_transport_at_paths_locked
        };
        add(
            &selected.run_workspace,
            &selected.team_dir,
            agent_id,
            &role_path,
            true,
            Some(&selected.team_key),
            transport,
        )
    })();
    match result {
        Ok(report) => Ok(report),
        Err(error) => Err(snapshot.rollback(selected, agent_id, error)),
    }
}

// Disaster recovery is opt-in and requires positive death of a recorded pane.
// An absent/unknown pane binding is not permission to replace an existing seat.
fn admit_add(
    state: &serde_json::Value,
    agent_id: &AgentId,
    force: bool,
    transport: &dyn Transport,
) -> Result<bool, LifecycleError> {
    let Some(agent) = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
    else {
        return Ok(false);
    };
    let duplicate =
        || LifecycleError::RequirementUnmet(format!("agent id already exists: {agent_id}"));
    if !force {
        return Err(duplicate());
    }
    super::restart::ensure_agent_not_running(state, agent_id, transport)?;
    let Some(pane) = agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Err(duplicate());
    };
    let pane = crate::transport::PaneId::new(pane);
    let dead = match transport.has_pane(&pane) {
        Ok(Some(present)) => !present,
        Ok(None) | Err(_) => matches!(
            transport.liveness(&pane),
            Ok(crate::model::enums::PaneLiveness::Dead)
        ),
    };
    if dead {
        Ok(true)
    } else {
        Err(duplicate())
    }
}

fn start_at_selected(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    patch: &RoleConfigPatch,
    allow_fresh: bool,
    transport: &dyn Transport,
) -> Result<StartAgentOutcome, LifecycleError> {
    let old_agent = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .ok_or_else(|| LifecycleError::RequirementUnmet(format!("agent {agent_id} not found")))?;
    super::restart::ensure_agent_not_running(&selected.state, agent_id, transport)?;
    if old_agent.get("paused").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent {agent_id} is paused"
        )));
    }
    let source = super::launch::role_source::resolve_role_source(
        &selected.run_workspace,
        &selected.team_dir,
        &selected.state,
        agent_id,
    )?;
    let role_path = role_path(selected, agent_id);
    check_destination(&role_path, Some(&source))?;
    let text = read_text(&source)?;
    let next = patched_role(&text, agent_id, patch, false)?;
    // Compare to the established engine, never to a freshly edited spec or model name.
    let old_provider = old_agent
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::provider::wire::parse_provider)
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet("established provider is unknown".into())
        })?;
    let meta = crate::compiler::read_front_matter(&source)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?
        .0;
    // Preserve the traditional compiler's interpretation of omitted metadata.
    let provider = patch
        .provider
        .as_deref()
        .or_else(|| meta.get("provider").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("pi");
    let provider = crate::provider::wire::parse_provider(provider)
        .ok_or_else(|| LifecycleError::Compile("unknown role provider".into()))?;
    if provider != old_provider {
        return Err(LifecycleError::RequirementUnmet(format!(
            "provider change is not allowed for agent {agent_id}: {} -> {}",
            crate::provider::wire::provider_wire(old_provider),
            crate::provider::wire::provider_wire(provider),
        )));
    }
    let snapshot = RoleSnapshot::capture(selected, &role_path)?;
    let result = (|| {
        if snapshot.role.bytes.as_deref() != Some(next.as_bytes()) {
            atomic_write(&role_path, next.as_bytes())?;
        }
        let team_meta = crate::compiler::read_front_matter(&selected.team_dir.join("TEAM.md"))
            .map_err(|error| LifecycleError::Compile(error.to_string()))?
            .0;
        let compiled = crate::compiler::compile_role_agent(
            &role_path,
            &team_meta,
            &selected.run_workspace.to_string_lossy(),
        )
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        if compiled.id != agent_id.as_str() {
            return Err(LifecycleError::Compile(
                "role identity does not match agent id".into(),
            ));
        }
        if compiled
            .agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(crate::provider::wire::parse_provider)
            != Some(old_provider)
        {
            return Err(LifecycleError::RequirementUnmet(
                "compiled provider does not match established provider".into(),
            ));
        }
        let mut spec = read_spec(selected)?;
        replace_spec_agent(&mut spec, agent_id, &compiled.agent)?;
        crate::model::spec::validate_spec(&spec, &selected.run_workspace)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        atomic_write(&snapshot.spec.path, yaml::dumps(&spec).as_bytes())?;
        update_agent_config(selected, agent_id, &compiled.agent, &role_path)?;
        let outcome = super::restart::start_agent_at_paths(
            &selected.run_workspace,
            snapshot.spec.path.parent().unwrap_or(&selected.team_dir),
            agent_id,
            false,
            true,
            allow_fresh,
            Some(&selected.team_key),
            transport,
        )?;
        match outcome {
            StartAgentOutcome::Running { .. } => Ok(outcome),
            StartAgentOutcome::Noop { .. } => Err(LifecycleError::RequirementUnmet(format!(
                "agent {agent_id} is already running; use stop-agent first"
            ))),
            StartAgentOutcome::Paused { .. } => Err(LifecycleError::RequirementUnmet(format!(
                "agent {agent_id} is paused"
            ))),
        }
    })();
    match result {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(snapshot.rollback(selected, agent_id, error)),
    }
}

// A file is an explicit configuration source, not a template to silently override.
// Equal supplied values are omitted from the patch to preserve existing file bytes.
fn reconcile_creation(
    text: &str,
    patch: &RoleConfigPatch,
) -> Result<RoleConfigPatch, LifecycleError> {
    let (meta, body) = role_parts(text)?;
    for field in ["provider", "model", "effort", "profile"] {
        if meta
            .get(field)
            .is_some_and(|value| !matches!(value, Value::Null | Value::Str(_)))
        {
            return Err(LifecycleError::Compile(format!(
                "role {field} must be a string"
            )));
        }
    }
    let mut next = patch.clone();
    let conflict = |field: &str| {
        LifecycleError::RequirementUnmet(format!(
            "conflicting {field}: --role-file and CLI values differ"
        ))
    };
    let file_provider = meta
        .get("provider")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if let Some(file_provider) = file_provider {
        let file_provider = crate::provider::wire::parse_provider(file_provider)
            .ok_or_else(|| LifecycleError::Compile("unknown role provider".into()))?;
        if let Some(provider) = &next.provider {
            if crate::provider::wire::parse_provider(provider) != Some(file_provider) {
                return Err(conflict("provider"));
            }
            next.provider = None;
        }
    } else if next.provider.is_none() {
        return Err(LifecycleError::RequirementUnmet(
            "add-agent requires provider in --role-file or --provider <name>".into(),
        ));
    }
    match meta.get("dangerously_skip_permissions") {
        Some(Value::Bool(value)) => {
            if let Some(bypass) = next.bypass {
                if bypass != *value {
                    return Err(conflict("bypass"));
                }
                next.bypass = None;
            }
        }
        Some(_) => {
            return Err(LifecycleError::Compile(
                "role dangerously_skip_permissions must be a boolean".into(),
            ))
        }
        None if next.bypass.is_none() => {
            return Err(LifecycleError::RequirementUnmet(
                "add-agent requires bypass in --role-file or --bypass <true|false>".into(),
            ))
        }
        None => {}
    }
    for (field, supplied) in [
        ("model", &mut next.model),
        ("effort", &mut next.effort),
        ("profile", &mut next.profile),
    ] {
        if let Some(file_value) = meta
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            if let Some(value) = supplied.as_ref() {
                let equal = if field == "effort" {
                    crate::model::enums::ProviderEffort::parse(file_value)
                        == crate::model::enums::ProviderEffort::parse(value)
                } else {
                    value == file_value
                };
                if !equal {
                    return Err(conflict(field));
                }
                *supplied = None;
            }
        }
    }
    if let Some(prompt) = &next.prompt {
        if !body.trim().is_empty() {
            if prompt.trim() != body.trim() {
                return Err(conflict("prompt"));
            }
            next.prompt = None;
        }
    }
    Ok(next)
}

fn role_path(selected: &SelectedTeam, agent_id: &AgentId) -> PathBuf {
    selected
        .team_dir
        .join("agents")
        .join(format!("{agent_id}.md"))
}

fn validate_id(agent_id: &AgentId) -> Result<(), LifecycleError> {
    let id = agent_id.as_str();
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']) {
        return Err(LifecycleError::RequirementUnmet("invalid agent id".into()));
    }
    Ok(())
}

fn check_destination(path: &Path, source: Option<&Path>) -> Result<(), LifecycleError> {
    if fs::symlink_metadata(path).is_ok() {
        if !source.is_some_and(|source| {
            fs::canonicalize(source)
                .ok()
                .zip(fs::canonicalize(path).ok())
                .is_some_and(|(source, target)| source == target)
        }) {
            return Err(LifecycleError::RequirementUnmet(format!(
                "role file already exists: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, LifecycleError> {
    fs::read_to_string(path)
        .map_err(|error| LifecycleError::Compile(format!("{}: {error}", path.display())))
}

// Unlike read_front_matter, this returns the unnormalised body slice, including blank lines.
fn role_parts(text: &str) -> Result<(Value, &str), LifecycleError> {
    let (end, opening) = raw_line_end(text, 0);
    if &text[..end] != "---" || end == opening {
        return Ok((Value::Map(Vec::new()), text));
    }
    let mut offset = opening;
    while offset < text.len() {
        let (end, next) = raw_line_end(text, offset);
        if &text[offset..end] == "---" {
            let header = text[opening..offset]
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            let meta = if header.trim().is_empty() {
                Value::Map(Vec::new())
            } else {
                yaml::loads(&header).map_err(|error| LifecycleError::Compile(error.to_string()))?
            };
            if !meta.is_map() {
                return Err(LifecycleError::Compile(
                    "role frontmatter must be a map".into(),
                ));
            }
            return Ok((meta, &text[next..]));
        }
        offset = next;
    }
    Err(LifecycleError::Compile(
        "unterminated role frontmatter".into(),
    ))
}

fn raw_line_end(text: &str, start: usize) -> (usize, usize) {
    match text[start..].find(['\r', '\n']) {
        Some(relative) => {
            let end = start + relative;
            (
                end,
                end + if text[end..].starts_with("\r\n") {
                    2
                } else {
                    1
                },
            )
        }
        None => (text.len(), text.len()),
    }
}

fn patched_role(
    text: &str,
    agent_id: &AgentId,
    patch: &RoleConfigPatch,
    generated: bool,
) -> Result<String, LifecycleError> {
    if !generated && patch == &RoleConfigPatch::default() {
        return Ok(text.to_string());
    }
    let (mut meta, body) = role_parts(text)?;
    for key in ["agent_id", "name"] {
        if meta
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|id| id != agent_id.as_str())
        {
            return Err(LifecycleError::Compile(format!(
                "role {key} does not match {agent_id}"
            )));
        }
    }
    let before = meta.clone();
    if generated {
        set(&mut meta, "name", Value::Str(agent_id.as_str().into()))?;
    }
    for (key, value) in [
        ("provider", patch.provider.as_ref()),
        ("model", patch.model.as_ref()),
        ("effort", patch.effort.as_ref()),
        ("profile", patch.profile.as_ref()),
    ] {
        if let Some(value) = value {
            set(&mut meta, key, Value::Str(value.clone()))?;
        }
    }
    if let Some(value) = patch.bypass {
        set(
            &mut meta,
            "dangerously_skip_permissions",
            Value::Bool(value),
        )?;
    }
    if before == meta && patch.prompt.is_none() && !generated {
        return Ok(text.to_string());
    }
    let body = patch.prompt.as_deref().unwrap_or(body);
    Ok(format!(
        "---\n{}---\n{}",
        super::launch::role_source::dump_role_frontmatter(&meta),
        body
    ))
}

fn set(map: &mut Value, key: &str, value: Value) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = map else {
        return Err(LifecycleError::Compile("expected map".into()));
    };
    if pairs.iter().filter(|(name, _)| name == key).count() > 1 {
        return Err(LifecycleError::Compile(format!(
            "duplicate role field: {key}"
        )));
    }
    if let Some((_, old)) = pairs.iter_mut().find(|(name, _)| name == key) {
        *old = value;
    } else {
        pairs.push((key.into(), value));
    }
    Ok(())
}

fn read_spec(selected: &SelectedTeam) -> Result<Value, LifecycleError> {
    let path = selected
        .spec_path
        .as_ref()
        .ok_or_else(|| LifecycleError::Compile("missing spec".into()))?;
    yaml::loads(&read_text(path)?).map_err(|error| LifecycleError::Compile(error.to_string()))
}

fn spec_has_agent(spec: &Value, agent_id: &AgentId) -> bool {
    spec.get("agents")
        .and_then(Value::as_list)
        .is_some_and(|agents| {
            agents
                .iter()
                .any(|agent| agent.get("id").and_then(Value::as_str) == Some(agent_id.as_str()))
        })
}

fn replace_spec_agent(
    spec: &mut Value,
    agent_id: &AgentId,
    next: &Value,
) -> Result<(), LifecycleError> {
    let Value::Map(fields) = spec else {
        return Err(LifecycleError::Compile("invalid spec".into()));
    };
    let Some((_, Value::List(agents))) = fields.iter_mut().find(|(key, _)| key == "agents") else {
        return Err(LifecycleError::Compile("spec agents missing".into()));
    };
    let target = agents
        .iter_mut()
        .find(|agent| agent.get("id").and_then(Value::as_str) == Some(agent_id.as_str()))
        .ok_or_else(|| LifecycleError::Compile("agent missing from spec".into()))?;
    *target = next.clone();
    Ok(())
}

fn to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(value) => (*value).into(),
        Value::Int(value) => (*value).into(),
        Value::Float(value) => serde_json::json!(value),
        Value::Str(value) => value.clone().into(),
        Value::List(values) => values.iter().map(to_json).collect(),
        Value::Map(values) => values
            .iter()
            .map(|(key, value)| (key.clone(), to_json(value)))
            .collect(),
    }
}

fn update_agent_config(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    compiled: &Value,
    role: &Path,
) -> Result<(), LifecycleError> {
    StateRepository::new(&selected.run_workspace)
        .commit(
            StateWriteIntent::StartAgent {
                team_key: &selected.team_key,
                agent_id: agent_id.as_str(),
            },
            |state| {
                if let Some(agent) = state
                    .get_mut("agents")
                    .and_then(|agents| agents.get_mut(agent_id.as_str()))
                    .and_then(serde_json::Value::as_object_mut)
                {
                    for field in [
                        "provider",
                        "model",
                        "role",
                        "system_prompt",
                        "output_contract",
                        "auth_mode",
                        "effort",
                        "profile",
                        "dangerously_skip_permissions",
                        "communication_mode",
                    ] {
                        if let Some(value) = compiled.get(field) {
                            agent.insert(field.into(), to_json(value));
                        } else {
                            agent.remove(field);
                        }
                    }
                    agent.insert(
                        "dynamic_role_file".into(),
                        role.to_string_lossy().to_string().into(),
                    );
                    agent.insert("role_source_ownership".into(), "external".into());
                    agent.insert(
                        "model_source".into(),
                        if compiled.get("model").and_then(Value::as_str).is_some() {
                            "role".into()
                        } else {
                            "default".into()
                        },
                    );
                    // Imported roles are self-contained in this Team, including
                    // profile lookup; never retain an external credential root.
                    if compiled.get("profile").and_then(Value::as_str).is_some() {
                        agent.insert(
                            "_profile_dir".into(),
                            selected
                                .team_dir
                                .join("profiles")
                                .to_string_lossy()
                                .to_string()
                                .into(),
                        );
                    } else {
                        agent.remove("_profile_dir");
                    }
                }
            },
        )
        .map(|_| ())
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))
}

struct FileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}
impl FileSnapshot {
    fn capture(path: PathBuf) -> Result<Self, LifecycleError> {
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
        };
        Ok(Self { path, bytes })
    }
    fn restore(&self) -> Result<(), LifecycleError> {
        if self.bytes.is_some() && fs::read(&self.path).ok().as_ref() == self.bytes.as_ref() {
            return Ok(());
        }
        match self.bytes.as_ref() {
            Some(bytes) => atomic_write(&self.path, bytes),
            None => fs::remove_file(&self.path)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| LifecycleError::StatePersist(error.to_string())),
        }
    }
}

struct RoleSnapshot {
    role: FileSnapshot,
    spec: FileSnapshot,
}
impl RoleSnapshot {
    fn capture(selected: &SelectedTeam, role: &Path) -> Result<Self, LifecycleError> {
        Ok(Self {
            role: FileSnapshot::capture(role.to_path_buf())?,
            spec: FileSnapshot::capture(crate::model::paths::runtime_spec_path(
                &selected.run_workspace,
                &selected.team_key,
            ))?,
        })
    }
    fn rollback(
        &self,
        selected: &SelectedTeam,
        agent_id: &AgentId,
        error: LifecycleError,
    ) -> LifecycleError {
        let mut failures = Vec::new();
        for snapshot in [&self.role, &self.spec] {
            if let Err(error) = snapshot.restore() {
                failures.push(error.to_string());
            }
        }
        let restored = StateRepository::new(&selected.run_workspace).commit(
            StateWriteIntent::StartAgent {
                team_key: &selected.team_key,
                agent_id: agent_id.as_str(),
            },
            |state| {
                for field in ["agents", "agent_lifecycle"] {
                    if let Some(rows) = state
                        .get_mut(field)
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        if let Some(old) = selected
                            .state
                            .get(field)
                            .and_then(|rows| rows.get(agent_id.as_str()))
                        {
                            rows.insert(agent_id.as_str().into(), old.clone());
                        } else {
                            rows.remove(agent_id.as_str());
                        }
                    }
                }
            },
        );
        if let Err(error) = restored {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            error
        } else {
            LifecycleError::StatePersist(format!(
                "{error}; rollback failed: {}",
                failures.join("; ")
            ))
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LifecycleError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(persist_error)?;
    }
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(LifecycleError::StatePersist(format!(
            "refusing to replace symlink: {}",
            path.display()
        )));
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        created = true;
        if let Ok(meta) = fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(persist_error)
}

fn persist_error(error: std::io::Error) -> LifecycleError {
    LifecycleError::StatePersist(error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn pane(id: &str, session: &str, window: &str) -> crate::transport::PaneInfo {
        crate::transport::PaneInfo {
            pane_id: crate::transport::PaneId::new(id),
            session: crate::transport::SessionName::new(session),
            window_name: Some(crate::transport::WindowName::new(window)),
            window_index: None,
            pane_index: None,
            tty: None,
            current_command: None,
            current_path: None,
            active: true,
            pane_pid: None,
            leader_env: Default::default(),
        }
    }

    #[test]
    fn live_guard_does_not_borrow_a_peer_pane_or_ignore_own_live_cohort() {
        use crate::transport::test_support::OfflineTransport;
        let state = serde_json::json!({"session_name": "s", "agents": {
            "a": {"pane_id": "%b", "window": "a", "status": "running"},
            "b": {"pane_id": "%b", "window": "b", "status": "running"}
        }});
        let id = AgentId::new("a");
        let transport = OfflineTransport::new()
            .with_targets(vec![pane("%b", "s", "b")])
            .with_pane_presence("%b", true);
        super::super::restart::ensure_agent_not_running(&state, &id, &transport).unwrap();
        let transport = transport
            .with_targets(vec![pane("%a", "s", "a"), pane("%b", "s", "b")])
            .with_pane_presence("%a", true);
        let error = super::super::restart::ensure_agent_not_running(&state, &id, &transport)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already running") && error.contains("cohort proof"));
        assert!(error.contains("window=a pane=%a"));
        assert!(!error.contains("pane=%b"));
        let transport = transport.with_pane_presence("%a", false);
        super::super::restart::ensure_agent_not_running(&state, &id, &transport).unwrap();
    }

    #[test]
    fn force_admission_requires_positive_death_and_never_replaces_a_live_seat() {
        use crate::transport::test_support::OfflineTransport;
        let state = serde_json::json!({"session_name": "s", "agents": {
            "a": {"pane_id": "%a", "window": "a", "status": "running"}
        }});
        let id = AgentId::new("a");
        let dead = OfflineTransport::new()
            .with_targets(vec![pane("%other", "other-team", "a")])
            .with_pane_presence("%a", false)
            .with_pane_presence("%other", true);
        assert!(admit_add(&state, &id, false, &dead)
            .unwrap_err()
            .to_string()
            .contains("agent id already exists"));
        assert!(admit_add(&state, &id, true, &dead).unwrap());
        let live = OfflineTransport::new()
            .with_targets(vec![pane("%a", "s", "a")])
            .with_pane_presence("%a", true);
        assert!(admit_add(&state, &id, true, &live)
            .unwrap_err()
            .to_string()
            .contains("already running"));
        assert!(admit_add(&state, &id, true, &OfflineTransport::new()).is_err());
        let absent_binding = serde_json::json!({"agents": {"a": {"status": "stopped"}}});
        assert!(admit_add(&absent_binding, &id, true, &dead).is_err());
        assert!(!admit_add(&serde_json::json!({"agents": {}}), &id, true, &dead).unwrap());
        assert!(admit_add(
            &state,
            &id,
            true,
            &dead.with_list_targets_error("unknown topology")
        )
        .is_err());
    }

    #[test]
    fn file_decisions_need_no_repeated_flags_and_conflicts_fail_closed() {
        let text = "---\nprovider: codex\ndangerously_skip_permissions: false\nmodel: native-model\neffort: ultra\nprofile: local\n---\n\nrole body\n";
        assert_eq!(
            reconcile_creation(text, &RoleConfigPatch::default()).unwrap(),
            RoleConfigPatch::default()
        );
        let same = RoleConfigPatch {
            provider: Some("codex".into()),
            bypass: Some(false),
            model: Some("native-model".into()),
            effort: Some("ultra".into()),
            profile: Some("local".into()),
            prompt: Some("role body".into()),
        };
        assert_eq!(
            reconcile_creation(text, &same).unwrap(),
            RoleConfigPatch::default()
        );
        for different in [
            RoleConfigPatch {
                provider: Some("pi".into()),
                ..Default::default()
            },
            RoleConfigPatch {
                bypass: Some(true),
                ..Default::default()
            },
            RoleConfigPatch {
                model: Some("other".into()),
                ..Default::default()
            },
            RoleConfigPatch {
                effort: Some("high".into()),
                ..Default::default()
            },
            RoleConfigPatch {
                prompt: Some("other".into()),
                ..Default::default()
            },
            RoleConfigPatch {
                profile: Some("other".into()),
                ..Default::default()
            },
        ] {
            assert!(reconcile_creation(text, &different).is_err());
        }
        let supplied = RoleConfigPatch {
            provider: Some("pi".into()),
            bypass: Some(false),
            ..Default::default()
        };
        assert_eq!(reconcile_creation("body", &supplied).unwrap(), supplied);
        assert!(reconcile_creation("body", &RoleConfigPatch::default()).is_err());
        // An invalid existing declaration is not a missing field to overwrite.
        for field in ["provider", "model", "effort", "profile"] {
            let text =
                format!("---\n{field}: true\ndangerously_skip_permissions: false\n---\nbody");
            assert!(reconcile_creation(&text, &supplied).is_err());
        }
    }

    #[test]
    fn legacy_cr_only_header_does_not_change_engine_when_patched() {
        let text = "---\rprovider: codex\ndangerously_skip_permissions: false\r---\r\rbody\r";
        let patch = RoleConfigPatch {
            effort: Some("ultra".into()),
            ..Default::default()
        };
        let next = patched_role(text, &AgentId::new("w"), &patch, false).unwrap();
        let (meta, body) = role_parts(&next).unwrap();
        assert_eq!(meta.get("provider"), Some(&Value::Str("codex".into())));
        assert_eq!(body, "\rbody\r");
        assert!(reconcile_creation(text, &RoleConfigPatch::default()).is_ok());
    }

    #[test]
    fn partial_patch_preserves_body_and_false_presence() {
        let body = "\r\n\r\n中文\r\n```yaml\r\nx: y\r\n```\r\n";
        let original = format!("---\r\nname: worker\r\nprovider: pi\r\ncustom: kept\r\neffort: max\r\ndangerously_skip_permissions: true\r\n---\r\n{body}");
        let patch = RoleConfigPatch {
            effort: Some("high".into()),
            bypass: Some(false),
            ..Default::default()
        };
        let next = patched_role(&original, &AgentId::new("worker"), &patch, false).unwrap();
        let (meta, actual_body) = role_parts(&next).unwrap();
        assert_eq!(actual_body, body);
        assert_eq!(meta.get("custom"), Some(&Value::Str("kept".into())));
        assert_eq!(meta.get("effort"), Some(&Value::Str("high".into())));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn equal_patch_is_byte_identical() {
        let text = "---\n# keep formatting\nprovider: pi\ndangerously_skip_permissions: false\n---\n\nbody\n";
        let patch = RoleConfigPatch {
            provider: Some("pi".into()),
            bypass: Some(false),
            ..Default::default()
        };
        assert_eq!(
            patched_role(text, &AgentId::new("w"), &patch, false).unwrap(),
            text
        );
    }

    #[test]
    fn prompt_replaces_instead_of_appending() {
        let patch = RoleConfigPatch {
            prompt: Some("new\n---\nbody".into()),
            ..Default::default()
        };
        let text = "---\nprovider: codex\n---\nold body";
        let next = patched_role(text, &AgentId::new("w"), &patch, false).unwrap();
        assert_eq!(role_parts(&next).unwrap().1, "new\n---\nbody");
        assert!(!next.contains("old body"));
    }

    #[test]
    fn traditional_plain_and_empty_header_remain_supported() {
        for text in ["plain\r\nrole", "---\n---\n\nbody"] {
            assert_eq!(
                patched_role(text, &AgentId::new("w"), &RoleConfigPatch::default(), false).unwrap(),
                text
            );
        }
    }

    #[test]
    fn imported_profile_is_team_local_and_failed_update_restores_target_only() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("role-projection-{}-{nonce}", std::process::id()));
        let team_dir = root.join("roles");
        let old = serde_json::json!({
            "provider": "codex", "model": "old", "effort": "high",
            "profile": "local", "_profile_dir": "/external/profiles",
            "session_id": "preserved-session", "status": "stopped"
        });
        let sibling = serde_json::json!({"provider": "pi", "model": "keep"});
        let selected_state = serde_json::json!({"agents": {"w": old.clone()}});
        crate::state::persist::save_runtime_state(
            &root,
            &serde_json::json!({
                "teams": {
                    "t1": selected_state.clone(),
                    "t2": {"agents": {"w": sibling.clone()}}
                }
            }),
        )
        .unwrap();
        let spec_path = crate::model::paths::runtime_spec_path(&root, "t1");
        atomic_write(&spec_path, b"old spec\r\n").unwrap();
        let selected = SelectedTeam {
            run_workspace: root.clone(),
            team_key: "t1".into(),
            state: selected_state,
            team_dir: team_dir.clone(),
            spec_workspace: spec_path.parent().map(Path::to_path_buf),
            spec_path: Some(spec_path.clone()),
        };
        let id = AgentId::new("w");
        let role = role_path(&selected, &id);
        let snapshot = RoleSnapshot::capture(&selected, &role).unwrap();
        let compiled = yaml::loads("provider: codex\nmodel: new\neffort: ultra\nprofile: local\ndangerously_skip_permissions: false\nsystem_prompt: new instructions\n").unwrap();
        atomic_write(&role, b"new role").unwrap();
        atomic_write(&spec_path, b"new spec").unwrap();
        update_agent_config(&selected, &id, &compiled, &role).unwrap();
        let updated = crate::state::persist::load_runtime_state(&root).unwrap();
        let target = &updated["teams"]["t1"]["agents"]["w"];
        assert_eq!(target["model"], "new");
        assert_eq!(target["effort"], "ultra");
        assert_eq!(target["system_prompt"], "new instructions");
        assert_eq!(target["dangerously_skip_permissions"], false);
        assert_eq!(target["session_id"], "preserved-session");
        assert_eq!(
            target["_profile_dir"],
            team_dir.join("profiles").to_string_lossy().as_ref()
        );
        assert_eq!(updated["teams"]["t2"]["agents"]["w"], sibling);
        let error = snapshot.rollback(
            &selected,
            &id,
            LifecycleError::RequirementUnmet("injected failure".into()),
        );
        assert!(error.to_string().contains("injected failure"));
        assert!(!error.to_string().contains("rollback failed"));
        assert!(!role.exists());
        assert_eq!(fs::read(&spec_path).unwrap(), b"old spec\r\n");
        let restored = crate::state::persist::load_runtime_state(&root).unwrap();
        assert_eq!(restored["teams"]["t1"]["agents"]["w"], old);
        assert_eq!(restored["teams"]["t2"]["agents"]["w"], sibling);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_snapshot_restores_original_bytes_and_absence() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("role-config-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("role.md");
        let absent = FileSnapshot::capture(path.clone()).unwrap();
        atomic_write(&path, b"old\r\n\n").unwrap();
        let existing = FileSnapshot::capture(path.clone()).unwrap();
        atomic_write(&path, b"new").unwrap();
        existing.restore().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old\r\n\n");
        absent.restore().unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(&root).unwrap();
    }
}
