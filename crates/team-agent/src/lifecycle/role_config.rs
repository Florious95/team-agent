//! CLI convenience is a role-document transaction, not a provider override.
//! Existing file compilation and process/session lifecycle remain authoritative.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value};
use crate::state::repository::{StateRepository, StateWriteIntent};
use crate::state::selector::{SelectedTeam, SelectorMode};

use super::lock::{acquire_seat_lifecycle_lock, LifecycleLockRequest};
use super::LifecycleError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleConfigPatch {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub bypass: Option<bool>,
    pub prompt: Option<String>,
    pub profile: Option<String>,
}


fn select(workspace: &Path, team: Option<&str>) -> Result<SelectedTeam, LifecycleError> {
    crate::state::selector::resolve_active_team_readonly(workspace, team, SelectorMode::RuntimeOnly)
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))
}

pub(super) fn with_selected<T>(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    operation: impl FnOnce(&SelectedTeam) -> Result<T, LifecycleError>,
) -> Result<T, LifecycleError> {
    validate_id(agent_id)?;
    let _lock = acquire_seat_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "role-config",
        team: Some(&selected.team_key),
        agent_id: Some(agent_id),
    })?;
    let latest = select(&selected.run_workspace, Some(&selected.team_key))?;
    super::launch::ensure_owner_allowed_for_state(&latest.state, Some(agent_id))?;
    operation(&latest)
}

pub(super) fn validate_id(agent_id: &AgentId) -> Result<(), LifecycleError> {
    let id = agent_id.as_str();
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']) {
        return Err(LifecycleError::RequirementUnmet("invalid agent id".into()));
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, LifecycleError> {
    fs::read_to_string(path)
        .map_err(|error| LifecycleError::Compile(format!("{}: {error}", path.display())))
}

/// Pause is lifecycle policy; the compiled provider declaration can omit it.
pub(crate) fn declaration_paused(text: &str) -> Result<bool, LifecycleError> {
    let (meta, _) = role_parts(text)?;
    match meta.get("paused") {
        None => Ok(false),
        Some(Value::Bool(paused)) => Ok(*paused),
        _ => Err(LifecycleError::RequirementUnmet("paused must be a boolean".into())),
    }
}

pub(crate) fn paused_agents(
    workspace: &Path,
    team_dir: &Path,
    state: &serde_json::Value,
) -> Result<std::collections::BTreeSet<String>, LifecycleError> {
    let mut paused = std::collections::BTreeSet::new();
    if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
        for (id, row) in agents {
            if row["paused"] == true || row["status"].as_str()
                .is_some_and(|status| status.eq_ignore_ascii_case("paused"))
            {
                paused.insert(id.clone());
                continue;
            }
            let source = super::launch::role_source::role_source_path(
                workspace, team_dir, state, &AgentId::new(id),
            );
            let text = match fs::read_to_string(&source) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(LifecycleError::Compile(format!("{}: {error}", source.display()))),
            };
            if declaration_paused(&text)? {
                paused.insert(id.clone());
            }
        }
    }
    Ok(paused)
}

// Unlike read_front_matter, this returns the unnormalised body slice, including blank lines.
pub(super) fn role_parts(text: &str) -> Result<(Value, &str), LifecycleError> {
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

pub(crate) fn patched_role(
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

pub(super) fn read_spec(selected: &SelectedTeam) -> Result<Value, LifecycleError> {
    let path = selected
        .spec_path
        .as_ref()
        .ok_or_else(|| LifecycleError::Compile("missing spec".into()))?;
    yaml::loads(&read_text(path)?).map_err(|error| LifecycleError::Compile(error.to_string()))
}

pub(super) fn replace_spec_agent(
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

pub(super) fn to_json(value: &Value) -> serde_json::Value {
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

pub(super) fn update_agent_config(
    selected: &SelectedTeam,
    agent_id: &AgentId,
    compiled: &Value,
    role: &Path,
) -> Result<(), LifecycleError> {
    StateRepository::new(&selected.run_workspace).commit(
        StateWriteIntent::StartAgent { team_key: &selected.team_key, agent_id: agent_id.as_str() },
        |state| apply_agent_config(state, selected, agent_id, compiled, role),
    ).map(|_| ()).map_err(|error| LifecycleError::StatePersist(error.to_string()))
}

pub(super) fn apply_agent_config(
    state: &mut serde_json::Value, selected: &SelectedTeam, agent_id: &AgentId, compiled: &Value, role: &Path,
) {
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
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LifecycleError> {
    if fs::read(path).is_ok_and(|current| current == bytes) { return Ok(()); }
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
    #[cfg(unix)]
    fn a15_identical_bytes_leave_the_symlink_and_master_untouched() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        let master = temp.path().join("master.md");
        let link = temp.path().join("role.md");
        fs::write(&master, b"original\n").unwrap();
        symlink(&master, &link).unwrap();
        let inode = fs::symlink_metadata(&link).unwrap().ino();
        atomic_write(&link, b"original\n").unwrap();
        let metadata = fs::symlink_metadata(&link).unwrap();
        assert!(metadata.file_type().is_symlink());
        assert_eq!(metadata.ino(), inode);
        assert_eq!(fs::read_link(&link).unwrap(), master);
        assert_eq!(fs::read(&master).unwrap(), b"original\n");
    }

    #[test]
    #[cfg(unix)]
    fn a15_changed_bytes_refuse_without_replacing_the_symlink_or_master() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        let master = temp.path().join("master.md");
        let link = temp.path().join("role.md");
        fs::write(&master, b"original\n").unwrap();
        symlink(&master, &link).unwrap();
        let inode = fs::symlink_metadata(&link).unwrap().ino();
        let error = atomic_write(&link, b"changed\n").unwrap_err();
        assert!(error.to_string().contains("refusing to replace symlink"), "{error}");
        let metadata = fs::symlink_metadata(&link).unwrap();
        assert!(metadata.file_type().is_symlink());
        assert_eq!(metadata.ino(), inode);
        assert_eq!(fs::read_link(&link).unwrap(), master);
        assert_eq!(fs::read(&master).unwrap(), b"original\n");
    }

    #[test]
    fn pause_policy_reads_only_strict_frontmatter_booleans() {
        assert!(declaration_paused("---\npaused: true\n---\nRole\n").unwrap());
        assert!(declaration_paused("---\r\npaused: true\r\n---\r\nRole\r\n").unwrap());
        assert!(!declaration_paused("---\npaused: false\n---\npaused: true\n").unwrap());
        assert!(!declaration_paused("Body says paused: true\n").unwrap());
        for value in ["null", "1", "\"true\""] {
            assert!(declaration_paused(&format!("---\npaused: {value}\n---\nRole\n")).is_err());
        }
        // Preserve the repository YAML dialect's last-value-wins semantics.
        assert!(declaration_paused("---\npaused: false\npaused: true\n---\nRole\n").unwrap());
    }

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
    fn explicit_bypass_overrides_role_file_in_both_directions() {
        for &provider in crate::provider::wire::all_providers() {
            let provider = crate::provider::wire::provider_wire(provider);
            for declared in [true, false] {
                let text = format!("---\nprovider: {provider}\ndangerously_skip_permissions: {declared}\n---\nbody");
                let patch = RoleConfigPatch {
                    bypass: Some(!declared),
                    ..Default::default()
                };
                let next = patched_role(&text, &AgentId::new("w"), &patch, false).unwrap();
                assert_eq!(
                    role_parts(&next).unwrap().0.get("dangerously_skip_permissions"),
                    Some(&Value::Bool(!declared))
                );
            }
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


}
