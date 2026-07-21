use std::path::{Path, PathBuf};

use crate::lifecycle::LifecycleError;
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value};

use super::set_yaml_map_value;

pub(super) struct MaterializedRole {
    path: PathBuf,
    keep: bool,
}

impl MaterializedRole {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for MaterializedRole {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) fn materialize_latest_role(
    run_workspace: &Path,
    team_dir: &Path,
    state: &serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
) -> Result<MaterializedRole, LifecycleError> {
    let source_path = resolve_role_source(run_workspace, team_dir, state, source_agent_id)?;
    let (mut meta, body) = crate::compiler::read_front_matter(&source_path)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    let declared = meta
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            LifecycleError::Compile(format!(
                "source role file does not declare name: {}",
                source_path.display()
            ))
        })?;
    if declared != source_agent_id.as_str() {
        return Err(LifecycleError::Compile(format!(
            "source role file declares name '{}' but source agent is '{}'",
            declared, source_agent_id
        )));
    }
    set_yaml_map_value(
        &mut meta,
        "name",
        Value::Str(as_agent_id.as_str().to_string()),
    )?;
    if let Some(label) = label.filter(|value| !value.is_empty()) {
        set_yaml_map_value(&mut meta, "role", Value::Str(label.to_string()))?;
    }

    let managed_dir = run_workspace.join(".team").join("dynamic-role-files");
    std::fs::create_dir_all(&managed_dir)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    let path = managed_dir.join(format!("{}.md", as_agent_id.as_str()));
    if path.exists() {
        return Err(LifecycleError::RequirementUnmet(format!(
            "managed role file already exists: {}",
            path.display()
        )));
    }
    let rendered = format!("---\n{}---\n\n{}", yaml::dumps(&meta), body);
    let temp = path.with_extension(format!("md.tmp-{}", std::process::id()));
    std::fs::write(&temp, rendered.as_bytes())
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    Ok(MaterializedRole { path, keep: false })
}

pub(super) fn clamp_materialized_role_to_leader(
    materialized: &Path,
    spec: &Value,
) -> Result<(), LifecycleError> {
    let leader_tools = spec
        .get("leader")
        .and_then(|leader| leader.get("tools"))
        .and_then(Value::as_list)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (mut meta, body) = crate::compiler::read_front_matter(materialized)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    let requested = meta
        .get("tools")
        .and_then(Value::as_list)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let ceiling = crate::model::permissions::expand_tool_strings(&leader_tools)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let effective = crate::model::permissions::expand_tool_strings(&requested)
        .into_iter()
        .filter(|tool| ceiling.contains(tool))
        .map(Value::Str)
        .collect::<Vec<_>>();
    set_yaml_map_value(&mut meta, "tools", Value::List(effective))?;
    let rendered = format!("---\n{}---\n\n{}", yaml::dumps(&meta), body);
    std::fs::write(materialized, rendered.as_bytes())
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))
}

fn resolve_role_source(
    run_workspace: &Path,
    team_dir: &Path,
    state: &serde_json::Value,
    source_agent_id: &AgentId,
) -> Result<PathBuf, LifecycleError> {
    if let Some(raw) = state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .and_then(|agent| agent.get("dynamic_role_file"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(raw);
        let resolved = if path.is_absolute() {
            path
        } else {
            run_workspace.join(path)
        };
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(LifecycleError::Compile(format!(
            "source dynamic role file not found: {}",
            resolved.display()
        )));
    }
    let path = team_dir
        .join("agents")
        .join(format!("{}.md", source_agent_id.as_str()));
    if path.is_file() {
        Ok(path)
    } else {
        Err(LifecycleError::Compile(format!(
            "source role file not found: {}",
            path.display()
        )))
    }
}
