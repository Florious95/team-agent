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
        add_at_selected(selected, agent_id, source, patch, &transport)
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
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    if selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))
        .is_some()
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    let spec = read_spec(selected)?;
    if spec_has_agent(&spec, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    let role_path = role_path(selected, agent_id);
    check_destination(&role_path, source)?;
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
        // Ordinary add compiles this actual file, registers it and uses standard start.
        super::launch::add_agent_with_transport_at_paths_locked(
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
        let mut spec = read_spec(selected)?;
        replace_spec_agent(&mut spec, agent_id, &compiled.agent)?;
        crate::model::spec::validate_spec(&spec, &selected.run_workspace)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        atomic_write(&snapshot.spec.path, yaml::dumps(&spec).as_bytes())?;
        update_agent_config(selected, agent_id, &compiled.agent, &role_path, patch)?;
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
    let opening = if text.starts_with("---\r\n") {
        5
    } else if text.starts_with("---\n") {
        4
    } else {
        return Ok((Value::Map(Vec::new()), text));
    };
    let mut offset = opening;
    for line in text[opening..].split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let header = &text[opening..offset];
            let meta = if header.trim().is_empty() {
                Value::Map(Vec::new())
            } else {
                yaml::loads(header).map_err(|error| LifecycleError::Compile(error.to_string()))?
            };
            if !meta.is_map() {
                return Err(LifecycleError::Compile(
                    "role frontmatter must be a map".into(),
                ));
            }
            return Ok((meta, &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(LifecycleError::Compile(
        "unterminated role frontmatter".into(),
    ))
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
    patch: &RoleConfigPatch,
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
                    if patch.profile.is_some() {
                        agent.insert(
                            "_profile_dir".into(),
                            selected
                                .team_dir
                                .join("profiles")
                                .to_string_lossy()
                                .to_string()
                                .into(),
                        );
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
