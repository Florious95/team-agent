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
    let mut original = source
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(failed)?
        .unwrap_or_default();
    let (mut meta, body) = role_config::role_parts(&original)?;
    let body = body.to_owned();
    if let Yaml::Map(fields) = &meta {
        let mut names = std::collections::BTreeSet::new();
        for (name, value) in fields {
            if !names.insert(name) {
                return Err(refused(format!("duplicate role field: {name}")));
            }
            if matches!(name.as_str(), "provider" | "name" | "agent_id")
                && value.as_str().is_none_or(|value| value.trim().is_empty())
            {
                return Err(refused(format!("invalid role field: {name}")));
            }
            if matches!(name.as_str(), "model" | "effort" | "profile")
                && !matches!(value, Yaml::Null)
                && value.as_str().is_none_or(|value| value.trim().is_empty())
            {
                return Err(refused(format!("invalid role field: {name}")));
            }
            if name == "dangerously_skip_permissions" && !matches!(value, Yaml::Bool(_)) {
                return Err(refused("dangerously_skip_permissions must be a boolean"));
            }
        }
    }
    let previous = selected
        .and_then(|s| s.state["agents"].get(request.agent.as_str()))
        .and_then(|row| row["provider"].as_str());
    let desired = request
        .patch
        .provider
        .as_deref()
        .or_else(|| meta.get("provider").and_then(Yaml::as_str));
    if previous
        .zip(desired)
        .is_some_and(|(old, new)| !same_provider(old, new))
        && previous
            .zip(meta.get("provider").and_then(Yaml::as_str))
            .is_some_and(|(old, base)| same_provider(old, base))
    {
        // A provider switch cannot accidentally carry the old provider's model,
        // effort or credentials policy. Explicit new CLI values are applied next.
        if let Yaml::Map(fields) = &mut meta {
            fields.retain(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "model" | "effort" | "profile" | "auth_mode" | "contract_runtime"
                )
            });
        }
        original = format!(
            "---\n{}---\n{}",
            super::launch::role_source::dump_role_frontmatter(&meta),
            body
        );
    }
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
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("TEAM_AGENT_LEADER_PROVIDER")
                    .ok()
                    .filter(|provider| known_provider(provider))
            })
            .unwrap_or_else(|| "pi".to_string());
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
    let row = selected.and_then(|selected| selected.state["agents"].get(request.agent.as_str()));
    if compiled.agent.get("paused") == Some(&Yaml::Bool(true))
        || row.is_some_and(|row| {
            row["paused"] == true
                || row["status"]
                    .as_str()
                    .is_some_and(|status| status.eq_ignore_ascii_case("paused"))
        })
    {
        return Err(refused(format!("agent is paused; edit paused: false in its role declaration, then run `team-agent start {}`", command_arg(request.agent.as_str()))));
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
    if let (Some(left), Some(right)) = (
        crate::provider::wire::parse_provider(left),
        crate::provider::wire::parse_provider(right),
    ) {
        return left == right;
    }
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

pub(crate) fn command_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./:@+=".contains(&byte))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub fn status_command(request: &StartRequest) -> String {
    let mut command = format!(
        "team-agent status {} --workspace {}",
        command_arg(request.agent.as_str()),
        command_arg(&request.workspace.to_string_lossy())
    );
    if let Some(team) = &request.team {
        command.push_str(&format!(" --team {}", command_arg(team)));
    }
    command
}

fn fresh_command(request: &StartRequest) -> String {
    let mut args = vec![
        "team-agent".into(),
        "start".into(),
        request.agent.to_string(),
    ];
    for (flag, value) in [
        ("--provider", &request.patch.provider),
        ("--model", &request.patch.model),
        ("--effort", &request.patch.effort),
        ("--profile", &request.patch.profile),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value.clone()]);
        }
    }
    if let Some(bypass) = request.patch.bypass {
        args.extend(["--bypass".into(), bypass.to_string()]);
    }
    if let Some(source) = &request.role_file {
        args.extend(["--role-file".into(), source.to_string_lossy().into_owned()]);
    }
    args.extend([
        "--workspace".into(),
        request.workspace.to_string_lossy().into_owned(),
    ]);
    if let Some(team) = &request.team {
        args.extend(["--team".into(), team.clone()]);
    }
    args.extend(["--session".into(), "fresh".into()]);
    args.iter()
        .map(|arg| command_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
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
    preflight(request, selected, &resolved)?;
    describe_resolved(request, selected, live, &resolved)
}

pub(crate) fn describe_resolved(
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
        if row["paused"].as_bool() == Some(true) || desired.get("paused") == Some(&Yaml::Bool(true))
        {
            return Err(refused(
                format!("agent is paused; edit paused: false in its role declaration, then run `team-agent start {}`", command_arg(request.agent.as_str())),
            ));
        }
        let migration = !same_provider(row["provider"].as_str().unwrap_or(""), provider(desired));
        if live {
            let spec = if selected
                .spec_path
                .as_ref()
                .is_some_and(|path| path.is_file())
            {
                role_config::read_spec(selected)?
            } else {
                super::restart::compile_runtime_spec_from_roles(
                    &selected.run_workspace,
                    &selected.team_key,
                    &selected.state,
                )?
            };
            let old = spec.get("agents").and_then(Yaml::as_list).and_then(|rows| {
                rows.iter().find(|row| {
                    row.get("id").and_then(Yaml::as_str) == Some(request.agent.as_str())
                })
            });
            let changed = old.is_none_or(|old| {
                !same_provider(provider(old), provider(desired))
                    || [
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
                return Err(refused(format!("provider_change_requires_fresh: run `{}` to retain the logical seat and history; retain any explicit prompt from the original request", fresh_command(request))));
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

/// Pure target-specific native admission; never launch or repair a peer.
pub(crate) fn preflight(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    declaration: &Declaration,
) -> Result<(), LifecycleError> {
    #[cfg(unix)]
    if native(provider(&declaration.compiled.agent)) {
        let (meta, _) = role_config::role_parts(&declaration.text)?;
        if meta
            .get("profile")
            .is_some_and(|value| !matches!(value, Yaml::Null))
            || meta
                .get("auth_mode")
                .and_then(Yaml::as_str)
                .is_some_and(|mode| mode != "subscription")
        {
            return Err(refused("contract native providers use an existing subscription; API/profile mappings are unavailable"));
        }
        if meta.get("model").and_then(Yaml::as_str).is_none() {
            return Err(refused("this native provider requires an explicit --model"));
        }
        if let Some(selected) =
            selected.filter(|selected| !super::launch::surface_is_leader_bootstrap(&selected.state))
        {
            if selected
                .spec_path
                .as_ref()
                .is_some_and(|path| path.is_file())
            {
                let spec = role_config::read_spec(selected)?;
                crate::model::spec::validate_spec(&spec, &selected.run_workspace)
                    .map_err(failed)?;
            }
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
            crate::contract_runtime::backend::preflight(&config).map_err(failed)?;
        }
    }
    let _ = (request, selected, declaration);
    Ok(())
}

fn incarnation(selected: &SelectedTeam, id: &AgentId) -> Value {
    let row = &selected.state["agents"][id.as_str()];
    let identity = [
        "instance_id",
        "contract_instance",
        "generation",
        "provider",
        "pane_id",
        "window",
        "tmux_session",
        "session_id",
        "pane_pid",
        "process_id",
        "pid",
        "spawn_epoch",
        "spawned_at",
    ]
    .into_iter()
    .map(|key| {
        (
            key.to_string(),
            row.get(key).cloned().unwrap_or(Value::Null),
        )
    })
    .collect::<serde_json::Map<_, _>>();
    let owner = [
        "pane_id",
        "provider",
        "owner_epoch",
        "machine_fingerprint",
        "leader_session_uuid",
        "tmux_socket",
        "session_name",
    ]
    .into_iter()
    .map(|key| {
        (
            key.to_string(),
            selected.state["team_owner"]
                .get(key)
                .cloned()
                .unwrap_or(Value::Null),
        )
    })
    .collect::<serde_json::Map<_, _>>();
    json!({"agent": identity, "owner": owner, "transport": selected.state.get("transport"), "tmux_socket": selected.state.get("tmux_socket"), "owner_epoch": selected.state.get("owner_epoch")})
}

fn carrier_birth(selected: &SelectedTeam, id: &AgentId) -> Result<Value, LifecycleError> {
    #[cfg(unix)]
    if let Some(pid) = selected.state["agents"][id.as_str()]["pane_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
    {
        return team_agent_contract::host::process::process_birth_identity(pid)
            .map(|birth| json!(birth))
            .map_err(failed);
    }
    let _ = (selected, id);
    Ok(Value::Null)
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
    match selected.as_ref() {
        Some(selected)
            if super::launch::surface_has_runtime_team(&selected.state, &selected.team_key) =>
        {
            let identity = incarnation(selected, &request.agent);
            let birth = carrier_birth(selected, &request.agent)?;
            role_config::with_selected(selected, &request.agent, |current| {
                if incarnation(current, &request.agent) != identity
                    || carrier_birth(current, &request.agent)? != birth
                {
                    return Err(refused(format!(
                        "agent incarnation/carrier changed during admission; run `{}`",
                        status_command(request)
                    )));
                }
                converge(request, Some(current), false)
            })
        }
        selected => {
            let _lock =
                super::lock::acquire_seat_lifecycle_lock(super::lock::LifecycleLockRequest {
                    workspace: &workspace,
                    operation: "unified-start",
                    team: request.team.as_deref(),
                    agent_id: Some(&request.agent),
                })?;
            converge(request, selected, false)
        }
    }
}

fn converge(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    dry_run: bool,
) -> Result<Value, LifecycleError> {
    let declaration = resolve(request, selected)?;
    preflight(request, selected, &declaration)?;
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
                return Err(refused(format!(
                    "agent ownership/liveness is unverified; run `{}` before starting",
                    status_command(request)
                )))
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
        crate::coordinator::retire_legacy_coordinator(&selected.run_workspace).map_err(failed)?;
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
    if !selected.is_some_and(|selected| {
        super::launch::surface_has_runtime_team(&selected.state, &selected.team_key)
    }) {
        preflight_bootstrap(&declaration.text)?;
    }
    crate::coordinator::retire_legacy_coordinator(&request.workspace).map_err(failed)?;
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

pub(crate) fn bootstrap_directory(request: &StartRequest) -> PathBuf {
    request
        .workspace
        .join(".team/start-source")
        .join(request.team.as_deref().unwrap_or("current"))
}

pub(crate) fn preflight_bootstrap(role: &str) -> Result<(), LifecycleError> {
    let (meta, _) = role_config::role_parts(role)?;
    let key = meta.get("provider").and_then(Yaml::as_str).unwrap_or("pi");
    if native(key) {
        return Ok(());
    }
    let provider =
        crate::provider::wire::parse_provider(key).ok_or_else(|| refused("unknown provider"))?;
    if !crate::provider::get_adapter(provider).is_installed() {
        return Err(refused(format!(
            "Provider {key} command '{}' not found on PATH",
            crate::provider::wire::command_name(provider)
        )));
    }
    if provider == crate::model::enums::Provider::Pi {
        let (_, catalog) = super::launch::pi_mcp::resolve_pi_executable_chain().map_err(failed)?;
        if let Some(model) = meta.get("model").and_then(Yaml::as_str) {
            super::launch::pi_mcp::select_exact_pi_model(&catalog, model).map_err(failed)?;
        }
    }
    Ok(())
}

/// Materialization has one owner. Failed admission removes only unchanged files
/// created by this attempt; partial/unknown runtime commits retain recovery input.
pub(crate) fn stage_sources<T>(
    workspace: &Path,
    directory: &Path,
    team: &str,
    agent: &str,
    role: &str,
    launch: impl FnOnce() -> Result<(bool, T), LifecycleError>,
) -> Result<T, LifecycleError> {
    use std::io::Write;
    let team_source = workspace.join("TEAM.md");
    let team_text = if team_source.is_file() {
        std::fs::read_to_string(team_source).map_err(failed)?
    } else {
        format!(
            "---\n{}---\n",
            yaml::dumps(&Yaml::Map(vec![("name".into(), Yaml::Str(team.into()))]))
        )
    };
    let role_dir = directory.join("agents");
    let absent: Vec<_> = [directory, role_dir.as_path()]
        .into_iter()
        .filter(|path| !path.exists())
        .collect();
    let mut created = Vec::new();
    let result = (|| {
        std::fs::create_dir_all(&role_dir).map_err(failed)?;
        for (path, text) in [
            (directory.join("TEAM.md"), team_text.as_str()),
            (role_dir.join(format!("{agent}.md")), role),
        ] {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(text.as_bytes())
                        .and_then(|()| file.sync_all())
                    {
                        drop(file);
                        std::fs::remove_file(&path).map_err(failed)?;
                        return Err(failed(error));
                    }
                    created.push((path, text));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !std::fs::symlink_metadata(&path).map_err(failed)?.is_file()
                        || std::fs::read(&path).map_err(failed)? != text.as_bytes()
                    {
                        return Err(refused(format!(
                            "managed declaration differs: {}; existing input was preserved",
                            path.display()
                        )));
                    }
                }
                Err(error) => return Err(failed(error)),
            }
        }
        launch()
    })();
    let adopted = crate::state::persist::load_runtime_state_without_migrations(workspace)
        .map(|state| {
            let view = crate::state::projection::project_top_level_view(&state, team);
            let shared_source_adopted = view
                .get("team_dir")
                .and_then(Value::as_str)
                .and_then(|source| std::fs::canonicalize(source).ok())
                .zip(directory.canonicalize().ok())
                .is_some_and(|(source, directory)| source == directory)
                && view
                    .get("agents")
                    .and_then(Value::as_object)
                    .is_some_and(|agents| !agents.is_empty());
            !state.is_object()
                || shared_source_adopted
                || view
                    .get("agents")
                    .and_then(|agents| agents.get(agent))
                    .is_some()
        })
        .unwrap_or(true);
    if !result.as_ref().is_ok_and(|(ok, _)| *ok) && !adopted {
        for (path, text) in created.into_iter().rev() {
            if std::fs::read(&path).is_ok_and(|bytes| bytes == text.as_bytes()) {
                std::fs::remove_file(path).map_err(failed)?;
            }
        }
        for path in absent.into_iter().rev() {
            match std::fs::remove_dir(path) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(failed(error)),
            }
        }
    }
    result.map(|(_, result)| result)
}

fn bootstrap(
    request: &StartRequest,
    declaration: &Declaration,
    result: &mut Value,
) -> Result<(), LifecycleError> {
    let team = request.team.as_deref().unwrap_or("current");
    role_config::validate_id(&AgentId::new(team))?;
    let directory = bootstrap_directory(request);
    let staged = stage_sources(
        &request.workspace,
        &directory,
        team,
        request.agent.as_str(),
        &declaration.text,
        || {
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
                QuickStartReport::ExistingRuntime { .. } => return Err(refused(
                    "team context appeared during bootstrap; inspect status before retrying start",
                )),
            }
            Ok((result["ok"] == true, result.clone()))
        },
    )?;
    *result = staged;
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
        let temp = crate::lifecycle::tests::TestWorkspace::new();
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
        let temp = crate::lifecycle::tests::TestWorkspace::new();
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
        let temp = crate::lifecycle::tests::TestWorkspace::new();
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
