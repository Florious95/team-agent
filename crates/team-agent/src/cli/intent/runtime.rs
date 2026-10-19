//! Read-only planning; execution remains in the original typed lifecycle ports.
use super::*;
use crate::model::yaml::{self, Value as Yaml};
use crate::state::selector::{resolve_active_team_readonly, SelectedTeam, SelectorMode};
use crate::transport::{PaneId, Target, Transport};
use std::collections::BTreeSet;
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

fn checked<T, E: std::fmt::Display>(result: Result<T, E>) -> Result<T, CliError> {
    result.map_err(|e| refusal(e.to_string()))
}
fn complete(path: &Path) -> bool {
    path.join("TEAM.md").is_file() && path.join("agents").is_dir()
}
fn discover(
    workspace: &Path,
    explicit: Option<&Path>,
    selected: Option<&SelectedTeam>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return if complete(path) {
            Ok(path.to_owned())
        } else {
            Err(usage("source requires TEAM.md and agents/"))
        };
    }
    if let Some(selected) = selected {
        return if complete(&selected.team_dir) {
            Ok(selected.team_dir.clone())
        } else {
            Err(refusal(
                "selected role source is incomplete; repair it before start",
            ))
        };
    }
    if complete(workspace) {
        return Ok(workspace.to_owned());
    }
    let dir = workspace.join(".team");
    let mut candidates = BTreeSet::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() && complete(&path) {
                candidates.insert(path.canonicalize()?);
            }
        }
    }
    if candidates.len() != 1 {
        return Err(usage(format!(
            "action_required: found {} complete team sources; supply --from TEAMDIR",
            candidates.len()
        )));
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| usage("missing team source"))
}
fn remember(plan: &mut Plan, path: &Path) -> Result<(), CliError> {
    if !plan.files.iter().any(|(p, _)| p == path) {
        plan.files.push((path.to_owned(), std::fs::read(path)?));
    }
    Ok(())
}
fn json_yaml(value: &Yaml) -> Value {
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(b) => json!(b),
        Yaml::Int(n) => json!(n),
        Yaml::Float(n) => json!(n),
        Yaml::Str(s) => json!(s),
        Yaml::List(values) => Value::Array(values.iter().map(json_yaml).collect()),
        Yaml::Map(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), json_yaml(v)))
                .collect(),
        ),
    }
}
fn role(
    plan: &mut Plan,
    selected: &SelectedTeam,
    id: &str,
    source: Option<&Path>,
) -> Result<(PathBuf, Value), CliError> {
    let path = match source {
        Some(path) => path.to_owned(),
        None => checked(crate::lifecycle::launch::role_source::resolve_role_source(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.state,
            &AgentId::new(id),
        ))?,
    };
    let team_path = selected.team_dir.join("TEAM.md");
    remember(plan, &team_path)?;
    remember(plan, &path)?;
    let (team, _) = checked(crate::compiler::read_front_matter(&team_path))?;
    let compiled = checked(crate::compiler::compile_role_agent(
        &path,
        &team,
        &selected.run_workspace.to_string_lossy(),
    ))?;
    if compiled.id != id {
        return Err(refusal(format!("role identity mismatch for {id}")));
    }
    Ok((path, json_yaml(&compiled.agent)))
}
fn provider(row: &Value) -> &str {
    row.get("provider").and_then(Value::as_str).unwrap_or("pi")
}
fn topology(state: &Value) -> Value {
    let mut owner = serde_json::Map::new();
    for key in ["pane_id", "provider", "owner_epoch", "machine_fingerprint"] {
        owner.insert(key.into(), state.get("team_owner").and_then(|o| o.get(key)).cloned().unwrap_or(Value::Null));
    }
    json!({"tmux_socket": state.get("tmux_socket"), "transport": state.get("transport"), "owner": owner, "owner_epoch": state.get("owner_epoch")})
}
fn relevant(row: &Value) -> Value {
    let mut object = serde_json::Map::new();
    for key in [
        "provider",
        "status",
        "explicit_stop",
        "pane_id",
        "pane_pid",
        "window",
        "session_id",
        "contract_instance",
        "generation",
        "dynamic_role_file",
        "owner_team_id",
        "spawn_epoch",
        "spawned_at",
    ] {
        object.insert(key.into(), row.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(object)
}
fn drift(row: &Value, compiled: &Value) -> bool {
    provider(row) != provider(compiled)
        || row
            .get("as_launched_dangerously_skip_permissions")
            .and_then(Value::as_bool)
            .zip(
                compiled
                    .get("dangerously_skip_permissions")
                    .and_then(Value::as_bool),
            )
            .is_some_and(|(old, new)| old != new)
        || row
            .get("model")
            .filter(|v| !v.is_null())
            .zip(compiled.get("model").filter(|v| !v.is_null()))
            .is_some_and(|(old, new)| old != new)
}
fn paused(row: &Value) -> bool {
    row.get("status").and_then(Value::as_str) == Some("paused")
}

/// Positive native/store or exact carrier+pane+provider observations only.
fn observe(selected: &SelectedTeam, id: &str, row: &Value) -> Result<Observation, CliError> {
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row) {
        use team_agent_contract::{
            contract::types::SeatId,
            host::process::{sample_process, ProcessState},
            orchestration::store::SeatStatus,
        };
        if crate::contract_runtime::framework::is_paused_unstarted(row) {
            return Ok(Observation::Unstarted);
        }
        let backend = checked(crate::contract_runtime::backend::Backend::open_readonly(
            &selected.run_workspace,
            &selected.team_key,
        ))?;
        let seat = checked(backend.store.seat(&checked(SeatId::new(id))?))?
            .ok_or_else(|| refusal("missing canonical native seat"))?;
        if row.get("contract_instance").and_then(Value::as_str)
            != Some(seat.identity.instance.as_str())
            || row.get("generation").and_then(Value::as_u64) != Some(seat.identity.generation.0)
        {
            return Ok(Observation::Unknown);
        }
        let Some(target) = seat.physical.as_ref() else {
            return Ok(Observation::Unknown);
        };
        return Ok(match (seat.status, sample_process(&target.process)) {
            (SeatStatus::Ready, ProcessState::Alive) => Observation::Live,
            (SeatStatus::Stopped, ProcessState::Exited) => Observation::StoppedVerified,
            _ => Observation::Unknown,
        });
    }
    let Some(pid) = row
        .get("pane_pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 1)
    else {
        return Ok(Observation::Unknown);
    };
    match checked(crate::platform::process::pid_liveness(pid))? {
        crate::platform::process::ProcessLiveness::Dead => return Ok(Observation::StoppedVerified),
        crate::platform::process::ProcessLiveness::Unknown { .. } => {
            return Ok(Observation::Unknown)
        }
        crate::platform::process::ProcessLiveness::Live => (),
    }
    let backend = checked(
        crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        ),
    )?
    .backend;
    let panes = checked(backend.list_targets())?;
    let pane = row.get("pane_id").and_then(Value::as_str);
    let session = selected.state.get("session_name").and_then(Value::as_str);
    let window = row.get("window").and_then(Value::as_str);
    let matches: Vec<_> = panes
        .iter()
        .filter(|p| {
            Some(p.pane_id.as_str()) == pane
                && p.pane_pid == Some(pid)
                && Some(p.session.as_str()) == session
                && p.window_name.as_ref().map(|w| w.as_str()) == window
        })
        .collect();
    if matches.len() != 1 {
        return Ok(Observation::Unknown);
    }
    let native = crate::provider::wire::parse_provider(provider(row))
        .ok_or_else(|| refusal("unknown provider"))?;
    let command = matches[0].current_command.as_deref().unwrap_or("");
    let positive_command = command == crate::provider::wire::command_name(native)
        || crate::leader::provider_attribution::command_matches_provider(native, command);
    let positive_surface = if matches!(
        native,
        crate::model::enums::Provider::Pi | crate::model::enums::Provider::CursorAgent
    ) && !matches!(command, "sh" | "bash" | "zsh" | "fish" | "")
    {
        checked(
            backend.observe_native_input(&Target::Pane(PaneId::new(pane.unwrap_or(""))), native),
        )?
        .is_some()
    } else {
        false
    };
    if !positive_command && !positive_surface {
        return Ok(Observation::Unknown);
    }
    #[cfg(unix)]
    checked(team_agent_contract::host::process::process_birth_identity(
        pid,
    ))?;
    Ok(Observation::Live)
}
fn start_args(plan: &Plan, id: &str, patch: RoleConfigPatch) -> StartAgentArgs {
    StartAgentArgs {
        agent: id.into(),
        workspace: plan.scope.workspace.clone(),
        team: plan.scope.team.clone(),
        role_config: patch,
        force: false,
        allow_fresh: plan.intent.session == SessionPolicy::Auto,
        json: true,
    }
}
fn stop_args(plan: &Plan, id: &str) -> StopAgentArgs {
    StopAgentArgs {
        agent: id.into(),
        workspace: plan.scope.workspace.clone(),
        team: plan.scope.team.clone(),
        json: true,
    }
}
fn preflight_start(selected: &SelectedTeam, id: &str, row: &Value, session_policy: SessionPolicy) -> Result<(), CliError> {
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row) {
        use team_agent_contract::contract::types::SeatId;
        let config = checked(crate::contract_runtime::framework::team_config(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.team_key,
        ))?
        .ok_or_else(|| refusal("missing native role config"))?;
        let role = config
            .roles
            .iter()
            .find(|r| r.id.as_str() == id)
            .ok_or_else(|| refusal("missing native role"))?;
        checked(crate::contract_runtime::backend::prepare_resume(
            &selected.run_workspace,
            &selected.team_key,
            &checked(SeatId::new(id))?,
            Some(role),
        ))?;
        return Ok(());
    }
    let provider = crate::provider::wire::parse_provider(provider(row)).ok_or_else(|| refusal("unknown provider before restart"))?;
    let session = row.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(crate::provider::SessionId::new);
    let rollout = row.get("rollout_path").and_then(Value::as_str).filter(|s| !s.is_empty()).map(crate::provider::RolloutPath::new);
    let (exists, checked_paths) = session.as_ref().map(|session| crate::lifecycle::restart::surface_resume_backing_probe(
        &selected.run_workspace, &AgentId::new(id), row, provider, session, rollout.as_ref(),
    )).unwrap_or_default();
    let mode = crate::lifecycle::restart::decide_start_mode(
        crate::provider::wire::provider_wire(provider), session.as_ref(), rollout.as_ref(), exists, session_policy == SessionPolicy::Auto,
    );
    if mode == crate::lifecycle::StartMode::Noop {
        return Err(refusal(format!("resume_not_ready before stop: {id}; backing_checked={checked_paths:?}; choose --session auto only if fresh fallback is acceptable")));
    }
    Ok(())
}
fn add(
    plan: &mut Plan,
    selected: &SelectedTeam,
    id: &str,
    source: Option<PathBuf>,
) -> Result<(), CliError> {
    if selected
        .state
        .get("agent_lifecycle")
        .and_then(|a| a.get(id))
        .and_then(|a| a.get("state"))
        .and_then(Value::as_str)
        == Some("retired")
    {
        return Err(refusal(
            "removed agent history is not an absent role; use explicit add",
        ));
    }
    let patch = plan.intent.patch.clone();
    let role_file = match source {
        Some(path) => {
            let (meta, _) = checked(crate::compiler::read_front_matter(&path))?;
            if (meta.get("provider").and_then(Yaml::as_str).is_none() && patch.provider.is_none())
                || (!matches!(meta.get("dangerously_skip_permissions"), Some(Yaml::Bool(_))) && patch.bypass.is_none())
            {
                return Err(usage("role creation requires explicit provider and dangerously_skip_permissions decisions"));
            }
            let (_, compiled) = role(plan, selected, id, Some(&path))?;
            #[cfg(unix)]
            if crate::contract_runtime::registry::descriptor(provider(&compiled)).is_some() {
                return Err(refusal(
                    "native dynamic add is not admitted; no legacy fallback",
                ));
            }
            path.to_string_lossy().into_owned()
        }
        None => {
            if patch.provider.is_none() || patch.bypass.is_none() {
                return Err(usage(
                    "unknown agent: supply a complete --role-file or --provider and --bypass",
                ));
            }
            #[cfg(unix)]
            if patch
                .provider
                .as_deref()
                .is_some_and(|p| crate::contract_runtime::registry::descriptor(p).is_some())
            {
                return Err(refusal(
                    "native dynamic add is not admitted; no legacy fallback",
                ));
            }
            String::new()
        }
    };
    plan.steps.push(Step::AddAgent(AddAgentArgs {
        agent: id.into(),
        workspace: plan.scope.workspace.clone(),
        team: plan.scope.team.clone(),
        role_config: patch,
        role_file,
        force: false,
        json: true,
    }));
    Ok(())
}

pub(super) fn plan(intent: Intent) -> Result<Plan, CliError> {
    plan_with_observer(intent, observe)
}

fn plan_with_observer(
    intent: Intent,
    observe: impl Fn(&SelectedTeam, &str, &Value) -> Result<Observation, CliError>,
) -> Result<Plan, CliError> {
    let mut plan = Plan {
        scope: ResolvedScope {
            workspace: intent.workspace.clone(),
            team: intent.team.clone(),
        },
        intent,
        steps: vec![],
        skipped: vec![],
        files: vec![],
        expected: BTreeMap::new(),
        topology: None,
    };
    let runtime_workspace = checked(crate::model::paths::canonical_run_workspace(&plan.scope.workspace))?;
    let has_state = crate::state::persist::runtime_state_path(&runtime_workspace).is_file();
    if has_state { plan.scope.workspace = runtime_workspace; }
    let raw = if has_state {
        Some(crate::state::persist::load_runtime_state_without_migrations(&plan.scope.workspace)?)
    } else {
        None
    };
    if raw.as_ref().is_some_and(|state| !state.is_object() || state.get("teams").is_some_and(|teams| !teams.is_object())) {
        return Err(refusal("invalid runtime state; creation is not a repair fallback"));
    }
    let new_named_team =
        raw.as_ref()
            .zip(plan.intent.team.as_deref())
            .is_some_and(|(raw, team)| {
                team != "current" && !crate::lifecycle::launch::surface_has_runtime_team(raw, team)
                    && !raw
                        .get("teams")
                        .and_then(|t| t.get(team))
                        .is_some_and(crate::lifecycle::launch::surface_is_leader_bootstrap)
            });
    let selected = if has_state && !new_named_team {
        Some(resolve_active_team_readonly(
            &plan.scope.workspace,
            plan.intent.team.as_deref(),
            SelectorMode::RuntimeOnly,
        )?)
    } else {
        None
    };
    let initialized = selected
        .as_ref()
        .is_some_and(|s| crate::lifecycle::launch::surface_has_runtime_team(&s.state, &s.team_key));
    if !initialized {
        if plan.intent.verb != Verb::Start || !matches!(plan.intent.subject, Subject::Team { .. }) {
            return Err(refusal("no initialized team; use start --from TEAMDIR"));
        }
        let explicit = match &plan.intent.subject {
            Subject::Team { source } => source.as_deref(),
            _ => None,
        };
        if new_named_team && explicit.is_none() {
            return Err(usage(
                "new --team requires explicit --from; existing runtime is not a creation source",
            ));
        }
        let source = discover(&plan.scope.workspace, explicit, None)?;
        let _ = checked(crate::compiler::compile_team(&source))?;
        remember(&mut plan, &source.join("TEAM.md"))?;
        for entry in std::fs::read_dir(source.join("agents"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "md") {
                remember(&mut plan, &path)?;
            }
        }
        plan.steps.push(Step::CreateTeam(QuickStartArgs {
            workspace: plan.scope.workspace.clone(),
            agents_dir: source,
            name: None,
            team_id: plan.intent.team.clone(),
            yes: false,
            json: true,
            detail: true,
            backend: None,
        }));
        return Ok(plan);
    }
    let selected = selected.ok_or_else(|| refusal("missing selected runtime"))?;
    plan.scope = ResolvedScope {
        workspace: selected.run_workspace.clone(),
        team: Some(selected.team_key.clone()),
    };
    plan.topology = Some(topology(&selected.state));
    if let Subject::Team {
        source: Some(source),
    } = &plan.intent.subject
    {
        if source.canonicalize()? != selected.team_dir.canonicalize()? {
            return Err(refusal("source differs from selected team's role source"));
        }
    }
    let target = match &plan.intent.subject {
        Subject::Agent(id) => Some(id.clone()),
        _ => None,
    };
    checked(crate::lifecycle::launch::ensure_owner_allowed_for_state(
        &selected.state,
        target.as_ref(),
    ))?;
    if plan.intent.verb == Verb::Stop && target.is_none() {
        plan.steps.push(Step::Shutdown(ShutdownArgs {
            workspace: plan.scope.workspace.clone(),
            team: plan.scope.team.clone(),
            keep_logs: true,
            json: true,
        }));
        return Ok(plan);
    }
    let spec = selected.spec_path.as_ref().filter(|p| p.is_file());
    if spec.is_none() {
        if plan.intent.verb == Verb::Restart && target.is_none() {
            // Original active restart owns spec reconstruction and pre-destroy validation.
            let rows = selected
                .state
                .get("agents")
                .and_then(Value::as_object)
                .ok_or_else(|| refusal("missing agents"))?;
            let mut active = false;
            for (id, row) in rows {
                if !paused(row)
                    && !crate::state::projection::agent_explicit_stop(row)
                    && row.get("status").and_then(Value::as_str) != Some("stopped")
                {
                    let obs = observe(&selected, id, row)?;
                    if obs == Observation::Unknown {
                        return Err(refusal("unknown physical identity"));
                    }
                    active = true;
                }
            }
            if !active {
                return Err(refusal(
                    "no_eligible_agents; missing spec cannot be rebuilt by start",
                ));
            }
            plan.steps.push(Step::RestartTeam(restart_args(&plan)));
            return Ok(plan);
        }
        return Err(refusal(
            "runtime spec missing; use explicit team restart to request supported reconstruction",
        ));
    }
    let spec = spec.ok_or_else(|| refusal("missing spec"))?;
    let spec_value = checked(yaml::loads(&std::fs::read_to_string(spec)?))?;
    let spec_ids: BTreeSet<String> = spec_value
        .get("agents")
        .and_then(Yaml::as_list)
        .ok_or_else(|| refusal("invalid runtime roster"))?
        .iter()
        .map(|a| {
            a.get("id")
                .and_then(Yaml::as_str)
                .map(str::to_string)
                .ok_or_else(|| refusal("invalid runtime role ID"))
        })
        .collect::<Result<_, _>>()?;
    let rows = selected
        .state
        .get("agents")
        .and_then(Value::as_object)
        .ok_or_else(|| refusal("missing runtime agents"))?;
    let ids = if let Some(id) = target.as_ref() {
        vec![id.as_str().to_owned()]
    } else {
        let source = discover(&plan.scope.workspace, None, Some(&selected))?;
        let compiled = checked(crate::compiler::compile_team(&source))?;
        let mut ids: BTreeSet<String> = rows.keys().cloned().collect();
        if let Some(agents) = compiled.get("agents").and_then(Yaml::as_list) {
            for agent in agents {
                if let Some(id) = agent.get("id").and_then(Yaml::as_str) {
                    ids.insert(id.into());
                }
            }
        }
        for id in &spec_ids {
            if !rows.contains_key(id) {
                return Err(refusal(format!("SpecOnly {id}: state/spec mismatch")));
            }
        }
        ids.into_iter().collect()
    };
    let team = target.is_none();
    let mut observations = Vec::new();
    for id in ids {
        let row = rows.get(&id);
        if row.is_some() != spec_ids.contains(&id) {
            return Err(refusal(format!("state/spec mismatch for {id}")));
        }
        let Some(row) = row else {
            if team
                && selected
                    .state
                    .pointer(&format!("/agent_lifecycle/{id}/state"))
                    .and_then(Value::as_str)
                    == Some("retired")
            {
                plan.skipped.push(json!({"agent":id,"reason":"retired"}));
                continue;
            }
            if plan.intent.verb != Verb::Start {
                return Err(refusal("restart/stop never implicitly add agents"));
            }
            let source = plan.intent.role_file.clone().or_else(|| {
                let p = selected.team_dir.join("agents").join(format!("{id}.md"));
                p.is_file().then_some(p)
            });
            add(&mut plan, &selected, &id, source)?;
            continue;
        };
        if let Some(source) = plan.intent.role_file.as_ref() {
            let expected = checked(crate::lifecycle::launch::role_source::resolve_role_source(
                &selected.run_workspace,
                &selected.team_dir,
                &selected.state,
                &AgentId::new(&id),
            ))?;
            if source.canonicalize()? != expected.canonicalize()? {
                return Err(refusal("registered agent cannot replace its role source"));
            }
        }
        let explicit = crate::state::projection::agent_explicit_stop(row);
        let obs = if paused(row) || (team && plan.intent.verb == Verb::Restart && explicit) {
            Observation::Unstarted
        } else {
            observe(&selected, &id, row)?
        };
        let actions = seat_actions(
            plan.intent.verb,
            obs,
            paused(row),
            explicit,
            plan.intent.patch != RoleConfigPatch::default() || plan.intent.role_file.is_some(),
            team,
        )?;
        if actions.is_empty() {
            if obs == Observation::Live && plan.intent.verb == Verb::Start {
                let (_, compiled) = role(&mut plan, &selected, &id, None)?;
                if drift(row, &compiled) {
                    return Err(refusal(format!(
                        "known launch configuration drift for running {id}; stop before applying"
                    )));
                }
            }
            plan.skipped.push(json!({"agent":id,"reason":if paused(row){"paused"}else if team&&plan.intent.verb==Verb::Restart&&explicit{"explicit_stop"}else if obs==Observation::Live{"already_running"}else{"already_stopped"},"observation":obs}));
        } else {
            if plan.intent.verb != Verb::Stop {
                let (_, compiled) = role(&mut plan, &selected, &id, None)?;
                if provider(row) != provider(&compiled) {
                    return Err(refusal("registered provider cannot change"));
                }
                preflight_start(&selected, &id, row, plan.intent.session)?;
            }
            for action in actions {
                match action {
                    "stop" => plan.steps.push(Step::StopAgent(stop_args(&plan, &id))),
                    _ => plan.steps.push(Step::StartAgent(start_args(
                        &plan,
                        &id,
                        plan.intent.patch.clone(),
                    ))),
                }
            }
        }
        let mut expected = relevant(row);
        #[cfg(unix)]
        if obs == Observation::Live {
            if let Some(pid) = row
                .get("pane_pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
            {
                expected["_birth"] = json!(checked(
                    team_agent_contract::host::process::process_birth_identity(pid)
                )?);
            }
        }
        plan.expected.insert(id.clone(), expected);
        observations.push((id, obs, explicit, paused(row)));
    }
    if team && plan.intent.verb == Verb::Restart {
        let eligible: Vec<_> = observations
            .iter()
            .filter(|(_, _, explicit, paused)| !explicit && !paused)
            .collect();
        if eligible.is_empty() {
            return Err(refusal(
                "no_eligible_agents; use team start to explicitly activate stopped roster",
            ));
        }
        let shutdown = selected.state.get("status").and_then(Value::as_str) == Some("shutdown");
        if !shutdown
            || eligible
                .iter()
                .any(|(_, obs, _, _)| *obs == Observation::Live)
        {
            plan.steps.clear();
            plan.steps.push(Step::RestartTeam(restart_args(&plan)));
        }
    }
    if team
        && plan.steps.is_empty()
        && !plan
            .skipped
            .iter()
            .any(|s| s.get("reason").and_then(Value::as_str) == Some("already_running"))
    {
        return Err(refusal("no_eligible_agents"));
    }
    Ok(plan)
}
fn restart_args(plan: &Plan) -> RestartArgs {
    RestartArgs {
        workspace: plan.scope.workspace.clone(),
        team: plan.scope.team.clone(),
        allow_fresh: plan.intent.session == SessionPolicy::Auto,
        session_converge_deadline_ms: plan.intent.session_converge_deadline_ms,
        json: true,
        detail: true,
    }
}

pub(super) fn recheck(plan: &Plan, step: &Step, after_stop: bool) -> Result<(), CliError> {
    for (path, bytes) in &plan.files {
        if std::fs::read(path)? != *bytes {
            return Err(refusal(format!(
                "role/spec changed since planning: {}",
                path.display()
            )));
        }
    }
    if let Step::CreateTeam(args) = step {
        let role_dir = args.agents_dir.join("agents");
        let current: BTreeSet<_> = std::fs::read_dir(&role_dir)?.map(|entry| entry.map(|e| e.path())).collect::<Result<Vec<_>, _>>()?.into_iter().filter(|path| path.extension().is_some_and(|s| s == "md")).collect();
        let planned: BTreeSet<_> = plan.files.iter().map(|(path, _)| path).filter(|path| path.parent() == Some(role_dir.as_path())).cloned().collect();
        if current != planned { return Err(refusal("creation roster changed since planning")); }
        return Ok(());
    }
    let selected = resolve_active_team_readonly(
        &plan.scope.workspace,
        plan.scope.team.as_deref(),
        SelectorMode::RuntimeOnly,
    )?;
    if Some(selected.team_key.as_str()) != plan.scope.team.as_deref()
        || selected.run_workspace != plan.scope.workspace
    {
        return Err(refusal("canonical scope changed"));
    }
    if plan.topology.as_ref().is_some_and(|expected| topology(&selected.state) != *expected) {
        return Err(refusal("owner or transport binding changed since planning"));
    }
    let id = step.agent().map(AgentId::new);
    checked(crate::lifecycle::launch::ensure_owner_allowed_for_state(
        &selected.state,
        id.as_ref(),
    ))?;
    let targets: Vec<_> = if let Some(id) = step.agent() {
        vec![id]
    } else {
        plan.expected.keys().map(String::as_str).collect()
    };
    for id in targets {
        let row = selected.state.get("agents").and_then(|a| a.get(id));
        if matches!(step, Step::AddAgent(_)) {
            if row.is_some() {
                return Err(refusal("agent was registered after planning"));
            }
            continue;
        }
        let row = row.ok_or_else(|| refusal("planned agent disappeared"))?;
        let expected = plan
            .expected
            .get(id)
            .ok_or_else(|| refusal("missing original incarnation"))?;
        if after_stop && plan.intent.verb == Verb::Restart && matches!(step, Step::StartAgent(_)) {
            if provider(row) != provider(expected)
                || row.get("generation").unwrap_or(&Value::Null)
                    != expected.get("generation").unwrap_or(&Value::Null)
                || observe(&selected, id, row)? != Observation::StoppedVerified
            {
                return Err(refusal(
                    "post-stop incarnation is not the planned stopped agent",
                ));
            }
        } else {
            let mut expected = expected.clone();
            let birth = expected.as_object_mut().and_then(|o| o.remove("_birth"));
            if relevant(row) != expected {
                return Err(refusal("planned agent incarnation changed"));
            }
            #[cfg(unix)]
            if let Some(birth) = birth {
                let pid = row
                    .get("pane_pid")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .ok_or_else(|| refusal("missing original carrier"))?;
                if json!(checked(
                    team_agent_contract::host::process::process_birth_identity(pid)
                )?) != birth
                {
                    return Err(refusal("carrier replaced since planning"));
                }
            }
        }
    }
    Ok(())
}
