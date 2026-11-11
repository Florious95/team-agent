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
    pub compiled: crate::compiler::CompiledRole,
    pub defaulted: Value,
}

fn refused(message: impl Into<String>) -> LifecycleError {
    LifecycleError::RequirementUnmet(message.into())
}
fn failed(error: impl std::fmt::Display) -> LifecycleError {
    LifecycleError::StatePersist(error.to_string())
}
fn rejected(error: impl std::fmt::Display) -> LifecycleError {
    refused(error.to_string())
}

/// Defaults are applied once, and only to genuinely omitted declaration values.
pub fn resolve(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
) -> Result<Declaration, LifecycleError> {
    role_config::validate_id(&request.agent)?;
    if selected.is_some_and(|selected| {
        selected.state["agents"]
            .get(request.agent.as_str())
            .is_none()
            && selected
                .state
                .get("agent_lifecycle")
                .and_then(|history| history.get(request.agent.as_str()))
                .is_some_and(|entry| entry.get("state").and_then(Value::as_str) != Some("retired"))
    }) {
        return Err(refused(format!(
            "agent {} has unrecognized lifecycle history; run `{}` before re-admission",
            request.agent,
            status_command(request)
        )));
    }
    let directory = selected
        .map(|s| s.team_dir.as_path())
        .unwrap_or(&request.workspace);
    let destination = directory
        .join("agents")
        .join(format!("{}.md", request.agent));
    let source = if let Some(source) = &request.role_file {
        Some(source.clone())
    } else if destination.try_exists().map_err(rejected)? {
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
        .map_err(rejected)?
        .unwrap_or_default();
    let (mut meta, body) = role_config::role_parts(&original)?;
    let declaration_paused = role_config::declaration_paused(&original)?;
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
            if name == "dangerously_skip_permissions"
                && !matches!(value, Yaml::Bool(_))
            {
                return Err(refused(format!("{name} must be a boolean")));
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
    let team_meta = if directory.join("TEAM.md").try_exists().map_err(rejected)? {
        crate::compiler::read_front_matter(&directory.join("TEAM.md"))
            .map_err(rejected)?
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
    .map_err(rejected)?;
    if !known_provider(provider(&compiled.agent)) {
        return Err(refused("unknown provider in the effective declaration"));
    }
    if compiled.id != request.agent.as_str() {
        return Err(refused(
            "role identity does not match the requested agent id",
        ));
    }
    let row = selected.and_then(|selected| selected.state["agents"].get(request.agent.as_str()));
    if declaration_paused
        || compiled.agent.get("paused") == Some(&Yaml::Bool(true))
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
    scoped_seat_command(request, "status")
}

pub(crate) fn unverified_start_guidance(
    request: &StartRequest,
    row: &Value,
    observation: super::seat_observation::SeatObservation,
) -> String {
    let detail = super::carrier::pane_pid(row)
        .map(|pid| super::carrier::observe(row, pid).detail)
        .unwrap_or_else(|| "recorded carrier PID is missing or invalid".into());
    let (action, command) = if observation == super::seat_observation::SeatObservation::NativeReconcile {
        ("stop the scoped seat with", scoped_seat_command(request, "stop"))
    } else {
        ("inspect with", status_command(request))
    };
    format!("agent ownership/liveness is unverified: {detail}; no unique live pane/carrier ownership match; {action} `{command}` before retrying")
}

fn scoped_seat_command(request: &StartRequest, action: &str) -> String {
    let mut command = format!(
        "team-agent {action} {} --workspace {}",
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
    pub creates_team: bool,
    pub declaration: Value,
    pub defaulted: Value,
}

/// The same pure declaration/state decision serves CLI preview and locked execution.
#[cfg(test)]
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
    let op = if op == "resumed"
        && continuation_admission(request, selected, resolved)?
            .is_some_and(|(_, mode)| mode != super::StartMode::Resumed)
    {
        "fresh"
    } else {
        op
    };
    #[cfg(unix)]
    let op =
        match selected.filter(|_| op == "resumed" && native(provider(&resolved.compiled.agent))) {
            Some(selected) => {
                let id = team_agent_contract::contract::types::SeatId::new(request.agent.as_str())
                    .map_err(rejected)?;
                if crate::contract_runtime::backend::seat_authority_absent(
                    &selected.run_workspace,
                    &selected.team_key,
                    &id,
                )
                .map_err(rejected)?
                {
                    "fresh"
                } else {
                    op
                }
            }
            None => op,
        };
    Ok(StartDescription {
        op,
        creates_team: selected.is_none_or(|team| !super::launch::surface_has_runtime_team(&team.state, &team.team_key)),
        declaration,
        defaulted: resolved.defaulted.clone(),
    })
}

/// Read-only continuation admission. Preserve verified context when possible;
/// a contextless or non-resumable legacy seat starts fresh, with an honest receipt.
fn continuation_admission(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    declaration: &Declaration,
) -> Result<Option<(Value, super::StartMode)>, LifecycleError> {
    if request.session == SessionPolicy::Fresh || native(provider(&declaration.compiled.agent)) {
        return Ok(None);
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let Some(old) = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(request.agent.as_str()))
    else {
        return Ok(None);
    };
    if !same_provider(
        old.get("provider").and_then(Value::as_str).unwrap_or("pi"),
        provider(&declaration.compiled.agent),
    ) {
        return Ok(None);
    }
    // Matching live starts are no-ops; they do not require a captured transcript.
    let dead = super::carrier::pane_pid(old).filter(|pid| *pid > 1)
        .is_some_and(|pid| super::carrier::observe(old, pid).state.gone());
    if old.get("status").and_then(Value::as_str) == Some("running") && !dead {
        return Ok(None);
    }
    for field in [
        "session_id",
        "_pending_session_id",
        "rollout_path",
        "spawn_cwd",
        "last_result_at",
    ] {
        if old
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(refused(format!("invalid continuation metadata: {field}")));
        }
    }
    if super::restart::classify_first_send_at(old.get("first_send_at").unwrap_or(&Value::Null))
        == super::FirstSendAtState::Corrupt
    {
        return Err(refused("invalid first_send_at metadata"));
    }
    if old
        .get("task_prompt_delivered")
        .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return Err(refused("invalid task_prompt_delivered metadata"));
    }
    let provider = crate::provider::wire::parse_provider(provider(&declaration.compiled.agent))
        .ok_or_else(|| refused("unsupported continuation provider"))?;
    if !crate::provider::get_adapter(provider).caps().resume {
        return Ok(Some((old.clone(), super::StartMode::Fresh)));
    }
    let row = crate::provider::session::capture::captured_start_row(
        &selected.state,
        request.agent.as_str(),
    )
    .map_err(rejected)?;
    let session = row
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(crate::provider::SessionId::new);
    let rollout = row
        .get("rollout_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(crate::provider::RolloutPath::new);
    let backing = session.as_ref().is_some_and(|session| {
        super::restart::surface_resume_backing_probe(
            &selected.run_workspace,
            &request.agent,
            &row,
            provider,
            session,
            rollout.as_ref(),
        )
        .0
    });
    let identity = super::restart::session_identity_probe_for_agent_with_fork_ancestry(
        &selected.run_workspace,
        &selected.state,
        &request.agent,
        &row,
        provider,
        rollout.as_ref(),
    );
    let resumable = backing
        && identity.identity_ok != Some(false)
        && row.get("attribution_ambiguous").and_then(Value::as_bool) != Some(true)
        && row.get("capture_state").and_then(Value::as_str) != Some("attribution_ambiguous");
    let mode = if resumable {
        super::StartMode::Resumed
    } else if super::restart::restart_agent_never_captured(&row, None)
        || request.session == SessionPolicy::Auto
    {
        super::StartMode::Fresh
    } else {
        return Err(refused(format!("session_preserve_unavailable: {} has no verified resumable backing; retain its history and retry capture, or explicitly run `team-agent start {} --workspace {} --team {} --session fresh`",
                request.agent, command_arg(request.agent.as_str()), command_arg(&selected.run_workspace.to_string_lossy()), command_arg(&selected.team_key))));
    };
    Ok(Some((row, mode)))
}

/// Pure target-specific native admission; never launch or repair a peer.
pub(crate) fn preflight(
    request: &StartRequest,
    selected: Option<&SelectedTeam>,
    declaration: &Declaration,
) -> Result<(), LifecycleError> {
    if selected
        .and_then(|selected| {
            selected.state["agents"][request.agent.as_str()].get("session_history")
        })
        .is_some_and(|history| !history.is_null() && !history.is_array())
    {
        return Err(refused(
            "invalid session history; refusing to replace retained context",
        ));
    }
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
                    .map_err(rejected)?;
            }
            let config = crate::contract_runtime::config::read_start_declaration(
                &selected.run_workspace,
                &selected.team_dir,
                &selected.team_key,
                &declaration.destination,
                &declaration.text,
                request.agent.as_str(),
            )
            .map_err(rejected)?
            .ok_or_else(|| refused("native role is missing from its team declaration"))?;
            crate::contract_runtime::backend::preflight(&config).map_err(rejected)?;
            let row = &selected.state["agents"][request.agent.as_str()];
            if row.is_object()
                && row["contract_instance"].is_null()
                && same_provider(
                    row["provider"].as_str().unwrap_or(""),
                    provider(&declaration.compiled.agent),
                )
            {
                let role = config
                    .roles
                    .iter()
                    .find(|role| role.id.as_str() == request.agent.as_str())
                    .ok_or_else(|| refused("native role is missing"))?;
                native_continuation_admission(request, selected, role)?;
            }
        }
    }
    #[cfg(unix)]
    if !native(provider(&declaration.compiled.agent)) {
        if let Some(selected) = selected {
            let id = team_agent_contract::contract::types::SeatId::new(request.agent.as_str())
                .map_err(rejected)?;
            if (request.session != SessionPolicy::Fresh
                || selected.state["agents"][request.agent.as_str()]["contract_instance"]
                    .as_str()
                    .is_none())
                && !crate::contract_runtime::backend::seat_authority_absent(
                    &selected.run_workspace,
                    &selected.team_key,
                    &id,
                )
                .map_err(rejected)?
            {
                return Err(refused("native authority still owns this seat; reconcile or stop that native attempt before starting a legacy provider"));
            }
        }
    }
    continuation_admission(request, selected, declaration)?;
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
        "pane_birth_identity",
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
    if let Some(pid) = super::carrier::pane_pid(&selected.state["agents"][id.as_str()])
        .filter(|pid| *pid > 1)
    {
        let observation = super::carrier::observe(&selected.state["agents"][id.as_str()], pid);
        if observation.state.gone() { return Ok(Value::Null); }
        if observation.state == super::carrier::CarrierState::Unknown {
            return Err(refused(observation.detail));
        }
        return Ok(json!(observation.birth));
    }
    let _ = (selected, id);
    Ok(Value::Null)
}

/// Run preview without a lock/write; real admission is re-resolved under the seat lock.
pub fn start(request: &StartRequest, dry_run: bool) -> Result<Value, LifecycleError> {
    let workspace =
        crate::model::paths::canonical_run_workspace(&request.workspace).map_err(rejected)?;
    let selected = if crate::state::persist::runtime_state_path(&workspace).is_file() {
        let raw = crate::state::persist::load_runtime_state_without_migrations(&workspace)
            .map_err(rejected)?;
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
                .map_err(rejected)?,
            )
        }
    } else {
        let runtime = workspace.join(".team/runtime");
        if workspace.join("team.spec.yaml").is_file()
            || (runtime.is_dir()
                && std::fs::read_dir(&runtime).map_err(rejected)?.any(|entry| {
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
            SeatObservation::ProviderExited => {
                result["status"] = json!(if dry_run { "planned" } else { "recovering" });
                if dry_run { return Ok(result); }
                super::restart::stop_agent(&selected.run_workspace, &request.agent, Some(&selected.team_key))?;
                let refreshed = crate::state::selector::resolve_active_team_readonly(
                    &selected.run_workspace, Some(&selected.team_key), crate::state::selector::SelectorMode::RuntimeOnly,
                ).map_err(failed)?;
                return converge(request, Some(&refreshed), false);
            }
            observation @ (SeatObservation::NativeReconcile | SeatObservation::Unknown) => {
                return Err(refused(unverified_start_guidance(request, row, observation)))
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
        if result["op"] == "resumed"
            && result["start_mode"]
                .as_str()
                .is_some_and(|mode| mode.starts_with("fresh"))
        {
            result["op"] = json!("fresh");
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
    crate::model::spec::validate_spec(&spec, &selected.run_workspace).map_err(rejected)?;
    #[cfg(unix)]
    if native(provider(&declaration.compiled.agent))
        && selected.state["agents"][request.agent.as_str()]["contract_instance"].is_null()
    {
        return start_unmaterialized_native(request, selected, declaration, &spec);
    }
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
        .map_err(rejected)?
        .ok_or_else(|| refused("native role is missing from its team declaration"))?;
        if request.session == SessionPolicy::Fresh {
            crate::contract_runtime::backend::preflight(&config).map_err(rejected)?;
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
                .map_err(rejected)?;
                if backend.store.seat(&role.id).map_err(rejected)?.is_some() {
                    let resume = crate::contract_runtime::backend::prepare_resume(
                        &selected.run_workspace,
                        &selected.team_key,
                        &role.id,
                        Some(role),
                    )
                    .map_err(rejected)?;
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
            .map_err(rejected)?
            {
                crate::contract_runtime::backend::preflight(&config).map_err(rejected)?;
            }
        }
    }
    let continuation = continuation_admission(request, Some(selected), declaration)?;
    if let Some((row, super::StartMode::Resumed)) = &continuation {
        let mut applied = false;
        StateRepository::new(&selected.run_workspace)
            .commit(
                StateWriteIntent::StartAgent {
                    team_key: &selected.team_key,
                    agent_id: request.agent.as_str(),
                },
                |state| {
                    if let Some(current) = state
                        .get_mut("agents")
                        .and_then(|agents| agents.get_mut(request.agent.as_str()))
                    {
                        if crate::provider::session::capture::same_capture_incarnation(
                            current,
                            &selected.state["agents"][request.agent.as_str()],
                        ) {
                            crate::provider::session::capture::apply_capture_delta(current, row);
                            applied = true;
                        }
                    }
                },
            )
            .map_err(failed)?;
        if !applied {
            return Err(refused(
                "session incarnation changed during continuation admission",
            ));
        }
    }
    if request.session == SessionPolicy::Fresh
        || continuation.as_ref().is_some_and(|(_, mode)| {
            *mode == super::StartMode::Fresh
                && super::restart::has_prior_launch(
                    &selected.state["agents"][request.agent.as_str()],
                )
        })
    {
        #[cfg(unix)]
        if selected.state["agents"][request.agent.as_str()]["contract_instance"]
            .as_str()
            .is_some()
        {
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
    let outcome = super::restart::start_agent_at_paths_with_session(
        &selected.run_workspace,
        spec_path.parent().unwrap_or(&selected.team_dir),
        &request.agent,
        false,
        true,
        if continuation
            .as_ref()
            .is_some_and(|(_, mode)| *mode == super::StartMode::Fresh)
        {
            SessionPolicy::Fresh
        } else {
            request.session
        },
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

#[cfg(unix)]
fn native_continuation_admission(
    request: &StartRequest,
    selected: &SelectedTeam,
    role: &crate::contract_runtime::config::RoleConfig,
) -> Result<Option<team_agent_contract::contract::types::InstanceIdentity>, LifecycleError> {
    use team_agent_contract::{
        host::process::{sample_process, ProcessState},
        orchestration::store::SeatStatus,
    };
    let row = &selected.state["agents"][request.agent.as_str()];
    if crate::contract_runtime::backend::seat_authority_absent(
        &selected.run_workspace,
        &selected.team_key,
        &role.id,
    )
    .map_err(rejected)?
    {
        if request.session == SessionPolicy::Preserve
            && (row["native_recovery_archive"].is_string()
                || !super::restart::restart_agent_never_captured(row, None))
        {
            return Err(refused("session_preserve_unavailable: prior launch context has no native continuation authority; choose --session fresh explicitly"));
        }
        return Ok(None);
    }
    let backend = crate::contract_runtime::backend::Backend::open_readonly(
        &selected.run_workspace,
        &selected.team_key,
    )
    .map_err(rejected)?;
    let current = backend.store.seat(&role.id).map_err(rejected)?;
    if request.session == SessionPolicy::Fresh
        && current.is_some()
        && same_provider(
            row["provider"].as_str().unwrap_or(""),
            role.provider.as_str(),
        )
    {
        return Err(refused(format!("native identity needs reconciliation first; run `team-agent start {} --workspace {} --team {} --session preserve`, then stop it before requesting fresh",
            command_arg(request.agent.as_str()), command_arg(&selected.run_workspace.to_string_lossy()), command_arg(&selected.team_key))));
    }
    if request.session == SessionPolicy::Preserve {
        let current = current.as_ref().ok_or_else(|| refused("session_preserve_unavailable: incomplete native startup has no verified continuation; recover with explicit --session fresh"))?;
        if current.status != SeatStatus::Ready
            || !current.physical.as_ref().is_some_and(|target| {
                matches!(sample_process(&target.process), ProcessState::Alive)
            })
        {
            let resume = crate::contract_runtime::backend::prepare_resume(
                &selected.run_workspace,
                &selected.team_key,
                &role.id,
                Some(role),
            )
            .map_err(rejected)?;
            if resume.native_launch() != team_agent_contract::contract::types::Operation::Resume {
                return Err(refused("session_preserve_unavailable: native continuation requires fresh; choose --session fresh explicitly"));
            }
        }
    }
    Ok(current.map(|seat| seat.identity))
}

/// Keep legacy declarations and session history unchanged until the native
/// transaction commits. F0 owns peer conversion and restores it on compensation.
#[cfg(unix)]
fn start_unmaterialized_native(
    request: &StartRequest,
    selected: &SelectedTeam,
    declaration: &Declaration,
    spec: &Yaml,
) -> Result<Value, LifecycleError> {
    use team_agent_contract::orchestration::store::Outcome;
    let mut config = crate::contract_runtime::config::read_start_declaration(
        &selected.run_workspace,
        &selected.team_dir,
        &selected.team_key,
        &declaration.destination,
        &declaration.text,
        request.agent.as_str(),
    )
    .map_err(rejected)?
    .ok_or_else(|| refused("native role is missing"))?;
    crate::contract_runtime::framework::retain_registered_routes(&mut config, &selected.state);
    let role = config
        .roles
        .iter()
        .find(|role| role.id.as_str() == request.agent.as_str())
        .ok_or_else(|| refused("native role is missing"))?;
    let previous = native_continuation_admission(request, selected, role)?;
    let (_, record) = crate::contract_runtime::backend::start_with_peer_transfer(
        &selected.run_workspace,
        &selected.team_key,
        role,
        &config.members,
        true,
    )
    .map_err(failed)?;
    if record.outcome != Outcome::Committed {
        return Err(failed(format!("native startup operation={} phase={:?} outcome={:?}: {}; declaration and state unchanged",
            record.id.as_str(), record.phase, record.outcome, record.failure.as_deref().unwrap_or(""))));
    }
    let publication = (|| {
        role_config::atomic_write(&declaration.destination, declaration.text.as_bytes())?;
        role_config::atomic_write(
            &crate::model::paths::runtime_spec_path(&selected.run_workspace, &selected.team_key),
            yaml::dumps(spec).as_bytes(),
        )?;
        Ok::<(), LifecycleError>(())
    })();
    // Once committed, never report a rollback while a native process still owns
    // the seat. Publish its authority even if the editable declaration failed.
    let projected = crate::contract_runtime::commands::publish_at_with_change(
        &selected.run_workspace,
        &selected.team_key,
        &role.id,
        true,
        |state| {
            if request.session == SessionPolicy::Fresh {
                if let Some(row) = state["agents"][request.agent.as_str()].as_object_mut() {
                    archive_row(row);
                }
            }
            role_config::apply_agent_config(
                state,
                selected,
                &request.agent,
                &declaration.compiled.agent,
                &declaration.destination,
            );
        },
    );
    if let Err(error) = publication {
        return Err(failed(format!("native startup committed operation={} instance={}; declaration publication failed: {error}; projection={:?}",
            record.id.as_str(), record.target.identity.instance.as_str(), projected.as_ref().err())));
    }
    let projection = projected.map_err(|error| failed(format!(
        "native startup committed operation={} instance={}; state publication failed: {error}; retry the same start request to reconcile, do not remove the seat",
        record.id.as_str(), record.target.identity.instance.as_str(),
    )))?;
    let resumed =
        record.native_launch == Some(team_agent_contract::contract::types::Operation::Resume);
    let repaired = previous.as_ref() == Some(&record.target.identity);
    Ok(
        json!({"status":if repaired {"already_running"} else {"running"},"runtime":"contract","session_id":projection.get("session_id"),
        "generation":projection.get("generation"),"metadata_repaired":repaired,
        "start_mode":if repaired {"noop"} else if resumed {"resumed"} else {"fresh"},
        "operation_id":record.id.as_str(),"configuration_committed":true}),
    )
}

fn archive_session(selected: &SelectedTeam, agent: &AgentId) -> Result<(), LifecycleError> {
    // An intentional cleared tuple must not be filled back from the old disk row.
    StateRepository::new(&selected.run_workspace)
        .commit(
            StateWriteIntent::RestartTeam {
                team_key: &selected.team_key,
                topology_authority_agent_ids: &[agent.as_str()],
                skip_capture_backfill_agent_ids: &[agent.as_str()],
            },
            |state| {
                if let Some(row) = state["agents"][agent.as_str()].as_object_mut() {
                    archive_row(row);
                }
            },
        )
        .map_err(failed)?;
    Ok(())
}

fn archive_row(row: &mut serde_json::Map<String, Value>) {
    let mut previous = row.clone();
    previous.remove("session_history");
    let history = row.entry("session_history").or_insert_with(|| json!([]));
    if history.is_null() {
        *history = json!([]);
    }
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
        "capture_state",
        "claude_projects_root",
        "profile_launch",
        "managed_mcp_config",
        "spawn_cwd",
        "spawned_at",
        "spawn_epoch_ms",
        "last_result_at",
        "task_prompt_delivered",
        crate::lifecycle::launch::add_agent_state::FORK_PARENT_SESSION_ID,
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
        "pane_birth_identity",
        "tmux_endpoint",
        "window",
    ] {
        row.remove(key);
    }
    row.insert("first_send_at".into(), Value::Null);
    row.insert("status".into(), json!("stopped"));
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
        let (_, catalog) =
            super::launch::pi_mcp::resolve_pi_executable_chain().map_err(rejected)?;
        if let Some(model) = meta.get("model").and_then(Yaml::as_str) {
            super::launch::pi_mcp::select_exact_pi_model(&catalog, model).map_err(rejected)?;
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
                    summary, blockers, next_actions, ..
                } => {
                    result["ok"] = json!(false);
                    result["status"] = json!("blocked");
                    result["error"] = json!(summary);
                    result["blockers"] = json!(blockers);
                    result["next_actions"] = json!(next_actions);
                    result["effects"] = json!({
                        "coordinator_start": "not_attempted",
                        "worker_panes_started": [],
                    });
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

    #[test]
    fn archival_retains_the_previous_tuple_when_legacy_history_was_null() {
        let mut row = json!({"provider":"pi","session_id":"previous","rollout_path":"/old",
            "session_history":null,"explicit_stop":true})
        .as_object()
        .unwrap()
        .clone();
        archive_row(&mut row);
        assert_eq!(row["session_history"][0]["session_id"], "previous");
        assert_eq!(row["session_history"][0]["rollout_path"], "/old");
        assert!(!row.contains_key("session_id"));
        assert_eq!(row["explicit_stop"], true);
        row.insert("provider".into(), json!("cursor"));
        row.insert("session_id".into(), json!("native"));
        assert_eq!(row["session_history"][0]["provider"], "pi");
        assert_eq!(row["session_id"], "native");
    }

    #[test]
    fn fresh_archive_does_not_backfill_the_previous_capture_or_touch_siblings() {
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        let captured = json!({"provider":"pi", "status":"stopped", "explicit_stop":true,
            "session_id":"previous", "rollout_path":"old.jsonl", "captured_at":"2026-01-01T00:00:00Z",
            "captured_via":"file", "capture_state":"captured", "claude_projects_root":"old-pi-root",
            "spawn_cwd":"old-cwd", "spawned_at":"2026-01-01T00:00:00Z", "task_prompt_delivered":true});
        let state = json!({"active_team_key":"current", "team_key":"current", "session_name":"team-current",
            "agents":{"reviewer":captured.clone(), "sibling":{"provider":"fake","status":"stopped"}}});
        let path = crate::state::persist::runtime_state_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let selected = SelectedTeam {
            run_workspace: temp.path().into(),
            team_key: "current".into(),
            team_dir: temp.path().into(),
            spec_workspace: None,
            spec_path: None,
            state: state.clone(),
        };
        archive_session(&selected, &AgentId::new("reviewer")).unwrap();
        let (after, refusal) = crate::state::projection::resolve_team_scoped_state(
            temp.path(), Some("current"),
        ).unwrap();
        assert!(refusal.is_none());
        let after = after.unwrap();
        let row = &after["agents"]["reviewer"];
        // Loading normalizes absent session fields to null; compare everything else exactly.
        let non_null = |value: &Value| {
            let mut value = value.clone();
            value.as_object_mut().unwrap().retain(|_, field| !field.is_null());
            value
        };
        assert_eq!(non_null(&row["session_history"][0]), captured);
        for key in [
            "session_id",
            "rollout_path",
            "capture_state",
            "claude_projects_root",
            "spawn_cwd",
            "spawned_at",
            "task_prompt_delivered",
        ] {
            assert!(
                row.get(key).is_none_or(Value::is_null),
                "{key} must not leak into the new incarnation: {row}"
            );
        }
        assert_eq!(non_null(&after["agents"]["sibling"]), state["agents"]["sibling"]);
        assert_eq!(row["explicit_stop"], true);
    }

    #[cfg(unix)]
    #[test]
    fn paused_unstarted_native_observation_does_not_require_materialized_authority() {
        let workspace = super::super::tests::TestWorkspace::new();
        let row = json!({"provider":"cursor","runtime_family":"contract",
            "status":"paused","contract_instance":null});
        let selected = SelectedTeam {
            run_workspace: workspace.path().into(),
            team_key: "current".into(),
            state: json!({"agents":{"reviewer":row.clone()}}),
            team_dir: workspace.path().into(),
            spec_workspace: None,
            spec_path: None,
        };
        assert_eq!(
            super::super::seat_observation::observe(&selected, "reviewer", &row).unwrap(),
            super::super::seat_observation::SeatObservation::Unstarted
        );
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

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
    #[cfg(unix)]
    #[serial_test::serial(env)]
    fn stopped_carrier_birth_is_null_and_live_carrier_is_fenced() {
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        let id = AgentId::new("reviewer");
        let mut selected = SelectedTeam {
            run_workspace: temp.path().into(),
            team_key: "current".into(),
            team_dir: temp.path().into(),
            spec_workspace: None,
            spec_path: None,
            state: json!({"agents":{"reviewer":{"status":"stopped"}}}),
        };
        assert_eq!(carrier_birth(&selected, &id).unwrap(), Value::Null);
        // Reap our own child, rather than guessing a host PID or touching a worker.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        selected.state["agents"]["reviewer"]["pane_pid"] = json!(pid);
        assert_eq!(carrier_birth(&selected, &id).unwrap(), Value::Null);
        selected.state["agents"]["reviewer"]["pane_pid"] = json!(std::process::id());
        assert!(!carrier_birth(&selected, &id).unwrap().is_null());
    }

    #[test]
    fn admission_failure_is_a_refusal_not_a_persistence_failure() {
        assert!(matches!(
            rejected("native admission failed"),
            LifecycleError::RequirementUnmet(_)
        ));
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
    fn nonresumable_legacy_seat_reports_fresh_even_with_context_markers() {
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        std::fs::create_dir(temp.path().join("agents")).unwrap();
        std::fs::write(
            temp.path().join("agents/reviewer.md"),
            "---\nprovider: fake\n---\nReview.\n",
        )
        .unwrap();
        let mut request = request(temp.path());
        request.patch.provider = Some("fake".into());
        let selected = SelectedTeam {
            run_workspace: temp.path().into(),
            team_key: "current".into(),
            team_dir: temp.path().into(),
            spec_workspace: None,
            spec_path: None,
            state: json!({"agents":{"reviewer":{"provider":"fake","status":"stopped",
                "session_id":"not-resumable","first_send_at":"2026-01-01T00:00:00Z"}}}),
        };
        let before = selected.state.clone();
        let declaration = resolve(&request, Some(&selected)).unwrap();
        let (_, mode) = continuation_admission(&request, Some(&selected), &declaration)
            .unwrap()
            .unwrap();
        assert_eq!(mode, crate::lifecycle::StartMode::Fresh);
        assert_eq!(
            describe_resolved(&request, Some(&selected), false, &declaration)
                .unwrap()
                .op,
            "fresh"
        );
        assert_eq!(selected.state, before);
        let mut invalid = selected;
        invalid.state["agents"]["reviewer"]["task_prompt_delivered"] = json!("unknown");
        assert!(continuation_admission(&request, Some(&invalid), &declaration).is_err());
        let entries = |dir: &Path| std::fs::read_dir(dir).unwrap().count();
        assert_eq!(
            (entries(temp.path()), entries(&temp.path().join("agents"))),
            (1, 1),
            "planning writes nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unmaterialized_native_seat_reports_fresh_without_reserving_authority() {
        let temp = crate::lifecycle::tests::TestWorkspace::new();
        std::fs::create_dir(temp.path().join("agents")).unwrap();
        std::fs::write(
            temp.path().join("agents/reviewer.md"),
            "---\nprovider: cursor\nmodel: composer-2.5\n---\nReview.\n",
        )
        .unwrap();
        let mut request = request(temp.path());
        request.patch.provider = Some("cursor".into());
        request.patch.model = Some("composer-2.5".into());
        let selected = SelectedTeam {
            run_workspace: temp.path().into(),
            team_key: "current".into(),
            team_dir: temp.path().into(),
            spec_workspace: None,
            spec_path: None,
            state: json!({"agents":{"reviewer":{"provider":"cursor","status":"stopped","explicit_stop":true}}}),
        };
        // Host-independent: native CLI preflight is not part of the receipt decision.
        let declaration = resolve(&request, Some(&selected)).unwrap();
        assert_eq!(
            describe_resolved(&request, Some(&selected), false, &declaration)
                .unwrap()
                .op,
            "fresh"
        );
        let entries = |dir: &Path| std::fs::read_dir(dir).unwrap().count();
        assert_eq!(
            (entries(temp.path()), entries(&temp.path().join("agents"))),
            (1, 1),
            "planning writes nothing"
        );
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
