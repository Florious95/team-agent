//! The seat start contract: resolve one declaration, then converge one owned seat.
use super::role_config::{self, RoleConfigPatch};
use super::{LifecycleError, QuickStartReport, StartAgentOutcome};
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value as Yaml};
use crate::state::repository::{StateRepository, StateWriteIntent};
use crate::state::selector::{resolve_active_team_readonly, SelectedTeam, SelectorMode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    #[default]
    Preserve,
    Auto,
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub agent: AgentId,
    pub role_file: Option<PathBuf>,
    pub patch: RoleConfigPatch,
    pub session: SessionPolicy,
}

pub struct Declaration {
    pub source: Option<PathBuf>,
    pub destination: PathBuf,
    pub text: String,
    pub patch: RoleConfigPatch,
    pub compiled: crate::compiler::CompiledRole,
    pub defaulted: Value,
}

fn refused(message: impl Into<String>) -> LifecycleError {
    LifecycleError::RequirementUnmet(message.into())
}
fn failed(error: impl std::fmt::Display) -> LifecycleError {
    LifecycleError::StatePersist(error.to_string())
}

/// Defaults are applied once, and only to genuinely omitted declaration values.
pub fn resolve(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
) -> Result<Declaration, LifecycleError> {
    role_config::validate_id(&request.agent)?;
    let directory = selected
        .map(|s| s.team_dir.as_path())
        .unwrap_or(&request.workspace);
    let destination = directory
        .join("agents")
        .join(format!("{}.md", request.agent));
    let source = if let Some(source) = &request.role_file {
        Some(source.clone())
    } else if destination.try_exists().map_err(failed)? {
        Some(destination.clone())
    } else if let Some(selected) =
        selected.filter(|s| s.state["agents"].get(request.agent.as_str()).is_some())
    {
        Some(super::launch::role_source::resolve_role_source(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.state,
            &request.agent,
        )?)
    } else {
        None
    };
    let original = source
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(failed)?
        .unwrap_or_default();
    let (meta, body) = crate::compiler::split_front_matter(&original);
    let mut patch = request.patch.clone();
    let mut defaulted = Vec::new();
    if patch.provider.is_none() && meta.get("provider").and_then(Yaml::as_str).is_none() {
        let provider = selected
            .and_then(|s| {
                s.state["agents"][request.agent.as_str()]["provider"]
                    .as_str()
                    .or_else(|| {
                        s.state
                            .pointer("/team_owner/provider")
                            .and_then(Value::as_str)
                    })
            })
            .filter(|provider| known_provider(provider))
            .unwrap_or("pi")
            .to_string();
        defaulted.push("provider");
        patch.provider = Some(provider);
    }
    if patch.bypass.is_none() && meta.get("dangerously_skip_permissions").is_none() {
        patch.bypass = Some(false);
        defaulted.push("bypass");
    }
    if patch.model.is_none() && meta.get("model").is_none() {
        defaulted.push("model");
    }
    if patch.effort.is_none() && meta.get("effort").is_none() {
        defaulted.push("effort");
    }
    if patch.prompt.is_none() && body.trim().is_empty() {
        let prompt = format!(
            "You are agent {}. Follow the leader's instructions.",
            request.agent
        );
        patch.prompt = Some(prompt);
    }
    let text = role_config::patched_role(&original, &request.agent, &patch, source.is_none())?;
    let team_meta = if directory.join("TEAM.md").try_exists().map_err(failed)? {
        crate::compiler::read_front_matter(&directory.join("TEAM.md"))
            .map_err(failed)?
            .0
    } else {
        Yaml::Map(Vec::new())
    };
    let compiled = crate::compiler::compile_role_text(
        &destination,
        &text,
        &team_meta,
        &request.workspace.to_string_lossy(),
    )
    .map_err(failed)?;
    if !known_provider(provider(&compiled.agent)) {
        return Err(refused("unknown provider in the effective declaration"));
    }
    if compiled.id != request.agent.as_str() {
        return Err(refused(
            "role identity does not match the requested agent id",
        ));
    }
    Ok(Declaration {
        source,
        destination,
        text,
        patch,
        compiled,
        defaulted: json!(defaulted),
    })
}

fn known_provider(provider: &str) -> bool {
    #[cfg(unix)]
    if crate::contract_runtime::registry::declared(provider).is_some() {
        return true;
    }
    crate::provider::wire::parse_provider(provider).is_some()
}
fn provider(agent: &Yaml) -> &str {
    agent.get("provider").and_then(Yaml::as_str).unwrap_or("")
}
fn same_provider(left: &str, right: &str) -> bool {
    #[cfg(unix)]
    {
        crate::contract_runtime::registry::same_provider(left, right)
    }
    #[cfg(not(unix))]
    {
        left == right
    }
}
fn native(provider: &str) -> bool {
    #[cfg(unix)]
    {
        crate::contract_runtime::registry::declared(provider).is_some()
    }
    #[cfg(not(unix))]
    {
        let _ = provider;
        false
    }
}

#[derive(Debug, Clone)]
pub struct StartDescription {
    pub op: &'static str,
    pub declaration: Value,
    pub defaulted: Value,
}

/// The same pure declaration/state decision serves CLI preview and locked execution.
pub fn describe(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    live: bool,
) -> Result<StartDescription, LifecycleError> {
    let resolved = resolve(request, selected)?;
    describe_resolved(request, selected, live, &resolved)
}

fn describe_resolved(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    live: bool,
    resolved: &Declaration,
) -> Result<StartDescription, LifecycleError> {
    let desired = &resolved.compiled.agent;
    let declaration = json!({
        "provider":provider(desired),
        "model":desired.get("model").map(role_config::to_json),
        "effort":desired.get("effort").map(role_config::to_json),
        "bypass":desired.get("dangerously_skip_permissions").map(role_config::to_json).unwrap_or(json!(false)),
    });
    let row = selected.and_then(|s| s.state["agents"].get(request.agent.as_str()));
    let op = if let (Some(selected), Some(row)) = (selected, row) {
        let migration = !same_provider(row["provider"].as_str().unwrap_or(""), provider(desired));
        if live {
            let spec = role_config::read_spec(selected)?;
            let old = spec.get("agents").and_then(Yaml::as_list).and_then(|rows| {
                rows.iter().find(|row| {
                    row.get("id").and_then(Yaml::as_str) == Some(request.agent.as_str())
                })
            });
            let changed = old.is_none_or(|old| {
                [
                    "provider",
                    "model",
                    "effort",
                    "dangerously_skip_permissions",
                    "profile",
                    "system_prompt",
                    "communication_mode",
                ]
                .iter()
                .any(|key| old.get(key) != desired.get(key))
            });
            if changed || request.session == SessionPolicy::Fresh {
                return Err(refused(format!("agent {} is running; run `team-agent stop {}` before changing its configuration or session", request.agent, request.agent)));
            }
            "already_running"
        } else if migration {
            if request.session != SessionPolicy::Fresh {
                return Err(refused("provider_change_requires_fresh: repeat start with --session fresh; the logical seat and history are retained"));
            }
            "provider_changed"
        } else if request.session == SessionPolicy::Fresh {
            "fresh"
        } else {
            "resumed"
        }
    } else if resolved.source.is_some() {
        "adopted"
    } else {
        "created"
    };
    Ok(StartDescription {
        op,
        declaration,
        defaulted: resolved.defaulted.clone(),
    })
}

/// Run preview without a lock/write; real admission is re-resolved under the seat lock.
pub fn start(request: &StartRequest, dry_run: bool) -> Result<Value, LifecycleError> {
    let workspace =
        crate::model::paths::canonical_run_workspace(&request.workspace).map_err(failed)?;
    let selected = if crate::state::persist::runtime_state_path(&workspace).is_file() {
        let raw = crate::state::persist::load_runtime_state_without_migrations(&workspace)
            .map_err(failed)?;
        if !raw.is_object() || raw.get("teams").is_some_and(|teams| !teams.is_object()) {
            return Err(refused(
                "invalid runtime state; creation is not a repair fallback",
            ));
        }
        let new_team = request.team.as_deref().is_some_and(|team| {
            team != "current"
                && !super::launch::surface_has_runtime_team(&raw, team)
                && !(crate::state::projection::team_state_key(&raw) == team
                    && super::launch::surface_is_leader_bootstrap(&raw))
                && !raw
                    .get("teams")
                    .and_then(|teams| teams.get(team))
                    .is_some_and(super::launch::surface_is_leader_bootstrap)
        });
        if new_team {
            None
        } else {
            Some(
                resolve_active_team_readonly(
                    &workspace,
                    request.team.as_deref(),
                    SelectorMode::RuntimeOnly,
                )
                .map_err(failed)?,
            )
        }
    } else {
        let runtime = workspace.join(".team/runtime");
        if workspace.join("team.spec.yaml").is_file()
            || (runtime.is_dir()
                && std::fs::read_dir(&runtime).map_err(failed)?.any(|entry| {
                    entry.is_ok_and(|entry| entry.path().join("team.spec.yaml").is_file())
                }))
        {
            return Err(refused(
                "runtime spec exists without its state; repair ownership before starting",
            ));
        }
        None
    };
    if let Some(selected) = &selected {
        super::launch::ensure_owner_allowed_for_state(&selected.state, Some(&request.agent))?;
    }
    if dry_run {
        return converge(request, selected.as_ref(), true);
    }
    crate::coordinator::retire_legacy_coordinator(&workspace).map_err(failed)?;
    match selected.as_ref() {
        Some(selected)
            if super::launch::surface_has_runtime_team(&selected.state, &selected.team_key) =>
        {
            role_config::with_selected(selected, &request.agent, |selected| {
                converge(request, Some(selected), false)
            })
        }
        selected => converge(request, selected, false),
    }
}

fn converge(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    dry_run: bool,
) -> Result<Value, LifecycleError> {
    let declaration = resolve(request, selected)?;
    let mut result = json!({"ok":true,"action":"start","agent_id":request.agent.as_str(),
        "defaulted":declaration.defaulted,"session_policy":request.session,"dry_run":dry_run});
    let row = selected.and_then(|s| s.state["agents"].get(request.agent.as_str()));
    if let (Some(selected), Some(row)) = (selected, row) {
        let transport = super::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        )?
        .backend;
        use super::seat_observation::SeatObservation;
        let live = match super::seat_observation::observe(selected, request.agent.as_str(), row)? {
            SeatObservation::Live => true,
            SeatObservation::StoppedVerified | SeatObservation::Unstarted => false,
            SeatObservation::NativeReconcile | SeatObservation::Unknown => {
                return Err(refused(
                    "agent ownership/liveness is unverified; inspect status before start",
                ))
            }
        };
        let description = describe_resolved(request, Some(selected), live, &declaration)?;
        result["op"] = json!(description.op);
        result["declaration"] = description.declaration;
        if live {
            result["status"] = json!("already_running");
            return Ok(result);
        }
        if row["paused"].as_bool() == Some(true) {
            return Err(refused(
                "agent is paused; change its declaration before starting",
            ));
        }
        if !native(row["provider"].as_str().unwrap_or("")) {
            super::restart::ensure_agent_not_running(&selected.state, &request.agent, &transport)?;
        }
        result["status"] = json!(if dry_run { "planned" } else { "running" });
        if dry_run {
            return Ok(result);
        }
        let started = start_existing(request, selected, &declaration, &transport)?;
        for (key, value) in started.as_object().into_iter().flatten() {
            result[key] = value.clone();
        }
        return Ok(result);
    }
    let description = describe_resolved(request, selected, false, &declaration)?;
    result["op"] = json!(description.op);
    result["declaration"] = description.declaration;
    result["status"] = json!(if dry_run { "planned" } else { "created" });
    if dry_run {
        return Ok(result);
    }
    if let Some(selected) =
        selected.filter(|s| super::launch::surface_has_runtime_team(&s.state, &s.team_key))
    {
        let transport = super::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        )?
        .backend;
        // The canonical resolver owns precedence; the private creation leaf sees
        // one complete document, not a second set of competing CLI decisions.
        role_config::atomic_write(&declaration.destination, declaration.text.as_bytes())?;
        let mut spec = role_config::read_spec(selected)?;
        if spec
            .get("agents")
            .and_then(Yaml::as_list)
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row.get("id").and_then(Yaml::as_str) == Some(request.agent.as_str()))
            })
        {
            role_config::replace_spec_agent(
                &mut spec,
                &request.agent,
                &declaration.compiled.agent,
            )?;
            crate::model::spec::validate_spec(&spec, &selected.run_workspace).map_err(failed)?;
            role_config::atomic_write(
                &crate::model::paths::runtime_spec_path(
                    &selected.run_workspace,
                    &selected.team_key,
                ),
                yaml::dumps(&spec).as_bytes(),
            )?;
        }
        let report = super::launch::add_agent_with_transport_at_paths_locked(
            &selected.run_workspace,
            &selected.team_dir,
            &request.agent,
            &declaration.destination,
            true,
            Some(&selected.team_key),
            &transport,
        )?;
        result["role_file"] = json!(report.role_file);
        result["start_mode"] = json!(report.start_mode);
    } else {
        bootstrap(request, &declaration, &mut result)?;
    }
    Ok(result)
}

fn start_existing(
    request: &StartRequest,
    selected: &SelectedTeam,
    declaration: &Declaration,
    transport: &dyn crate::transport::Transport,
) -> Result<Value, LifecycleError> {
    let mut spec = role_config::read_spec(selected)?;
    role_config::replace_spec_agent(&mut spec, &request.agent, &declaration.compiled.agent)?;
    crate::model::spec::validate_spec(&spec, &selected.run_workspace).map_err(failed)?;
    #[cfg(unix)]
    if native(provider(&declaration.compiled.agent)) {
        let config = crate::contract_runtime::config::read_start_declaration(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.team_key,
            &declaration.destination,
            &declaration.text,
            request.agent.as_str(),
        )
        .map_err(failed)?
        .ok_or_else(|| refused("native role is missing from its team declaration"))?;
        if request.session == SessionPolicy::Fresh {
            crate::contract_runtime::backend::preflight(&config).map_err(failed)?;
        } else {
            let role = config
                .roles
                .iter()
                .find(|r| r.id.as_str() == request.agent.as_str())
                .ok_or_else(|| refused("native role not found"))?;
            if request.session == SessionPolicy::Preserve {
                let backend = crate::contract_runtime::backend::Backend::open_readonly(
                    &selected.run_workspace,
                    &selected.team_key,
                )
                .map_err(failed)?;
                if backend.store.seat(&role.id).map_err(failed)?.is_some() {
                    let resume = crate::contract_runtime::backend::prepare_resume(
                        &selected.run_workspace,
                        &selected.team_key,
                        &role.id,
                        Some(role),
                    )
                    .map_err(failed)?;
                    if resume.native_launch()
                        != team_agent_contract::contract::types::Operation::Resume
                    {
                        return Err(refused("session_preserve_unavailable: this provider requires a new native session; repeat start with --session fresh"));
                    }
                } else if selected.state["agents"][request.agent.as_str()]["contract_instance"]
                    .as_str()
                    .is_some()
                {
                    return Err(refused("session_preserve_unavailable: the prior native identity is unavailable; repeat start with --session fresh only after reviewing retained history"));
                }
            }
            if crate::contract_runtime::backend::preflight_start_role(
                &selected.run_workspace,
                &selected.team_key,
                role,
            )
            .map_err(failed)?
            {
                crate::contract_runtime::backend::preflight(&config).map_err(failed)?;
            }
        }
    }
    if request.session == SessionPolicy::Fresh {
        #[cfg(unix)]
        if native(
            selected.state["agents"][request.agent.as_str()]["provider"]
                .as_str()
                .unwrap_or(""),
        ) {
            let id = team_agent_contract::contract::types::SeatId::new(request.agent.as_str())
                .map_err(failed)?;
            crate::contract_runtime::backend::retire(
                &selected.run_workspace,
                &selected.team_key,
                &id,
            )
            .map_err(failed)?;
        }
        archive_session(selected, &request.agent)?;
    }
    role_config::atomic_write(&declaration.destination, declaration.text.as_bytes())?;
    let spec_path =
        crate::model::paths::runtime_spec_path(&selected.run_workspace, &selected.team_key);
    role_config::atomic_write(&spec_path, yaml::dumps(&spec).as_bytes())?;
    role_config::update_agent_config(
        selected,
        &request.agent,
        &declaration.compiled.agent,
        &declaration.destination,
    )?;
    #[cfg(unix)]
    if native(provider(&declaration.compiled.agent)) {
        let id = team_agent_contract::contract::types::SeatId::new(request.agent.as_str())
            .map_err(failed)?;
        let projection = crate::contract_runtime::framework::start_configured(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.team_key,
            &id,
        )
        .map_err(failed)?;
        return Ok(
            json!({"status":"running","runtime":"contract","session_id":projection.get("session_id"),
            "generation":projection.get("generation"),"configuration_committed":true}),
        );
    }
    let outcome = super::restart::start_agent_at_paths(
        &selected.run_workspace,
        spec_path.parent().unwrap_or(&selected.team_dir),
        &request.agent,
        false,
        true,
        request.session != SessionPolicy::Preserve,
        Some(&selected.team_key),
        transport,
    )?;
    Ok(match outcome {
        StartAgentOutcome::Running {
            start_mode,
            target,
            session_id,
            new_session_id,
            startup_receipt,
            ..
        } => json!({"status":"running","start_mode":start_mode,"target":target,
                "session_id":session_id.map(|s|s.to_string()),"new_session_id":new_session_id.map(|s|s.to_string()),
                "startup_receipt":startup_receipt,"configuration_committed":true}),
        StartAgentOutcome::Noop { target, .. } => {
            json!({"status":"already_running","target":target})
        }
        StartAgentOutcome::Paused { .. } => {
            json!({"ok":false,"status":"paused","reason":"agent_paused"})
        }
    })
}

fn archive_session(selected: &SelectedTeam, agent: &AgentId) -> Result<(), LifecycleError> {
    StateRepository::new(&selected.run_workspace)
        .commit(
            StateWriteIntent::StartAgent {
                team_key: &selected.team_key,
                agent_id: agent.as_str(),
            },
            |state| {
                if let Some(row) = state["agents"][agent.as_str()].as_object_mut() {
                    let mut previous = row.clone();
                    previous.remove("session_history");
                    let history = row.entry("session_history").or_insert_with(|| json!([]));
                    if let Some(history) = history.as_array_mut() {
                        history.push(Value::Object(previous));
                    }
                    for key in [
                        "session_id",
                        "pending_session_id",
                        "_pending_session_id",
                        "rollout_path",
                        "captured_at",
                        "captured_via",
                        "attribution_confidence",
                        "attribution_ambiguous",
                        "contract_scope",
                        "contract_instance",
                        "contract_provider",
                        "runtime_family",
                        "generation",
                        "native_endpoint",
                        "native_pane",
                        "pane_id",
                        "pane_pid",
                        "tmux_endpoint",
                        "window",
                    ] {
                        row.remove(key);
                    }
                    row.insert("first_send_at".into(), Value::Null);
                    row.insert("status".into(), json!("stopped"));
                }
            },
        )
        .map_err(failed)?;
    Ok(())
}

fn bootstrap(
    request: &StartRequest,
    declaration: &Declaration,
    result: &mut Value,
) -> Result<(), LifecycleError> {
    let team = request.team.as_deref().unwrap_or("current");
    role_config::validate_id(&AgentId::new(team))?;
    let directory = request
        .workspace
        .join(".team")
        .join("start-source")
        .join(team);
    std::fs::create_dir_all(directory.join("agents")).map_err(failed)?;
    let team_source = request.workspace.join("TEAM.md");
    let team_text = if team_source.is_file() {
        std::fs::read_to_string(&team_source).map_err(failed)?
    } else {
        format!(
            "---\n{}---\n",
            yaml::dumps(&Yaml::Map(vec![("name".into(), Yaml::Str(team.into()))]))
        )
    };
    role_config::atomic_write(&directory.join("TEAM.md"), team_text.as_bytes())?;
    role_config::atomic_write(
        &directory
            .join("agents")
            .join(format!("{}.md", request.agent)),
        declaration.text.as_bytes(),
    )?;
    match super::quick_start_in_workspace_with_backend(
        &request.workspace,
        &directory,
        Some(team),
        true,
        Some(team),
        None,
    )? {
        QuickStartReport::Ready { session_name, .. } => {
            result["session_name"] = json!(session_name.as_str());
            result["readiness"] = json!({"ready":false,"state":"unverified"});
        }
        QuickStartReport::Partial { error, .. } => {
            result["ok"] = json!(false);
            result["status"] = json!("partial");
            result["error"] = json!(error);
        }
        QuickStartReport::PreflightBlocked {
            summary, blockers, ..
        } => {
            result["ok"] = json!(false);
            result["status"] = json!("blocked");
            result["error"] = json!(summary);
            result["blockers"] = json!(blockers);
        }
        QuickStartReport::ExistingRuntime { .. } => {
            return Err(refused(
                "team context appeared during bootstrap; inspect status before retrying start",
            ))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn request(workspace: &Path) -> StartRequest {
        StartRequest {
            workspace: workspace.into(),
            team: None,
            agent: AgentId::new("reviewer"),
            role_file: None,
            patch: RoleConfigPatch::default(),
            session: SessionPolicy::Preserve,
        }
    }

    #[test]
    fn empty_workspace_resolution_is_pure_and_reports_safe_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let request = request(temp.path());
        let description = describe(&request, None, false).unwrap();
        assert_eq!(description.op, "created");
        assert_eq!(description.declaration["provider"], "pi");
        assert_eq!(description.declaration["bypass"], false);
        assert!(description
            .defaulted
            .as_array()
            .unwrap()
            .contains(&json!("provider")));
        assert!(description
            .defaulted
            .as_array()
            .unwrap()
            .contains(&json!("model")));
        assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());
    }

    #[test]
    fn explicit_declaration_has_no_defaulted_launch_decisions() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = request(temp.path());
        request.patch.provider = Some("pi".into());
        request.patch.model = Some("openai-codex/gpt-6-luna".into());
        request.patch.effort = Some("high".into());
        request.patch.bypass = Some(false);
        let description = describe(&request, None, false).unwrap();
        assert_eq!(description.defaulted, json!([]));
        assert_eq!(description.declaration["model"], "openai-codex/gpt-6-luna");
    }

    #[test]
    fn adoption_overlays_explicit_fields_without_mutating_the_source() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("agents")).unwrap();
        let source = temp.path().join("agents/reviewer.md");
        let original = "---\nprovider: pi\ndangerously_skip_permissions: true\n---\nReview.\n";
        std::fs::write(&source, original).unwrap();
        let mut request = request(temp.path());
        request.patch.bypass = Some(false);
        let description = describe(&request, None, false).unwrap();
        assert_eq!(description.op, "adopted");
        assert_eq!(description.declaration["bypass"], false);
        assert!(!description
            .defaulted
            .as_array()
            .unwrap()
            .contains(&json!("bypass")));
        assert_eq!(std::fs::read_to_string(source).unwrap(), original);
    }
}
