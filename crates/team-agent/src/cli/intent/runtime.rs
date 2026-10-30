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
    if candidates.is_empty() {
        return Err(usage(format!(
            "action_required: prepare {}/TEAM.md and {}/agents/<id>.md, then use --from TEAMDIR. Nothing was written or started. Role template (fill your decisions): provider: <choose>; dangerously_skip_permissions: <true|false>; model/effort: <optional>.",
            workspace.display(), workspace.display()
        )));
    }
    if candidates.len() != 1 {
        return Err(usage(format!(
            "action_required: multiple complete team sources {candidates:?}; supply --from TEAMDIR"
        )));
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| usage("missing team source"))
}
fn validate_roster(spec: &Yaml, directory: &Path) -> Result<(), CliError> {
    checked(crate::model::spec::validate_spec(spec, directory))?;
    if let Some(agents) = spec.get("agents").and_then(Yaml::as_list) {
        for agent in agents {
            if let Some(id) = agent.get("id").and_then(Yaml::as_str) {
                logical_agent_id(id)
                    .map_err(|_| refusal("invalid agent identity in selected roster"))?;
            }
        }
    }
    Ok(())
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
        owner.insert(
            key.into(),
            state
                .get("team_owner")
                .and_then(|o| o.get(key))
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    json!({"tmux_socket": state.get("tmux_socket"), "transport": state.get("transport"), "owner": owner, "owner_epoch": state.get("owner_epoch")})
}
fn has_native_incarnation(row: &Value) -> bool {
    crate::contract_runtime::registry::descriptor(provider(row)).is_some()
        && row.get("contract_instance").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
}
fn relevant(row: &Value) -> Value {
    let mut object = serde_json::Map::new();
    let native_identity = has_native_incarnation(row);
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
        // Native incarnation is authoritative even after its carrier dies. Keep
        // intent/config fences; the mutator separately validates native sessions
        // and physical receipts. Legacy seats still need carrier observations.
        if native_identity && matches!(key, "status" | "pane_id" | "pane_pid" | "window" | "session_id" | "spawned_at") {
            continue;
        }
        object.insert(key.into(), row.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(object)
}
fn same_provider(left: &str, right: &str) -> bool {
    match (
        crate::provider::wire::parse_provider(left),
        crate::provider::wire::parse_provider(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => crate::contract_runtime::registry::same_provider(left, right),
    }
}
fn drift(row: &Value, compiled: &Value) -> bool {
    !same_provider(provider(row), provider(compiled))
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
#[cfg(unix)]
fn native_settings_changed(
    old: &crate::contract_runtime::config::RoleConfig,
    current: &crate::contract_runtime::config::RoleConfig,
) -> bool {
    old.provider != current.provider
        || old.model != current.model
        || old.effort != current.effort
        || old.bypass != current.bypass
        || old.paused != current.paused
}

fn check_native_noop_settings(
    selected: &SelectedTeam,
    id: &str,
    row: &Value,
) -> Result<(), CliError> {
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row) {
        use team_agent_contract::contract::types::SeatId;
        let backend = checked(crate::contract_runtime::backend::Backend::open_readonly(
            &selected.run_workspace,
            &selected.team_key,
        ))?;
        let seat = checked(backend.store.seat(&checked(SeatId::new(id))?))?
            .ok_or_else(|| refusal("missing native snapshot identity"))?;
        if row.get("contract_instance").and_then(Value::as_str)
            != Some(seat.identity.instance.as_str())
            || row.get("generation").and_then(Value::as_u64) != Some(seat.identity.generation.0)
        {
            return Err(refusal("native incarnation changed during no-op planning"));
        }
        let old = checked(crate::contract_runtime::backend::read_role(
            &backend.binding,
            &seat.identity,
        ))?;
        let config = checked(crate::contract_runtime::framework::team_config(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.team_key,
            Some(id),
        ))?
        .ok_or_else(|| refusal("missing native configuration"))?;
        let current = config
            .roles
            .iter()
            .find(|role| role.id.as_str() == id)
            .ok_or_else(|| refusal("native role no longer configured"))?;
        if native_settings_changed(&old, current) {
            return Err(refusal(format!(
                "native launch configuration drift for {id}; stop before applying settings"
            )));
        }
        checked(crate::contract_runtime::backend::preflight_start_role(
            &selected.run_workspace,
            &selected.team_key,
            current,
        ))?;
    }
    Ok(())
}

fn paused(row: &Value) -> bool {
    row.get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("paused"))
}

#[cfg(unix)]
fn native_observation(
    status: team_agent_contract::orchestration::store::SeatStatus,
    process: team_agent_contract::host::process::ProcessState,
    root_status: Option<&str>,
) -> Observation {
    use team_agent_contract::{host::process::ProcessState, orchestration::store::SeatStatus};
    match (status, process) {
        (SeatStatus::Ready, ProcessState::Alive) if root_status == Some("running") => {
            Observation::Live
        }
        (SeatStatus::Ready, ProcessState::Exited) => Observation::StoppedVerified,
        (SeatStatus::Stopped, ProcessState::Exited) if root_status == Some("stopped") => {
            Observation::StoppedVerified
        }
        _ => Observation::Unknown,
    }
}

fn shell_carrier(command: &str) -> bool {
    matches!(command, "sh" | "bash" | "zsh" | "fish")
}
fn surface_liveness(command: &str, native_frame: bool, live_child: bool) -> bool {
    !command.is_empty() && native_frame && (!shell_carrier(command) || live_child)
}
fn argv0_names(argv0: &str, native: crate::provider::Provider) -> bool {
    Path::new(argv0).file_name().and_then(|name| name.to_str())
        == Some(crate::provider::wire::command_name(native))
}
/// Native installers may exec a version-named file (Claude Code:
/// `~/.local/bin/claude` -> `versions/<semver>`), so tmux reports the version
/// as the pane command. The exact argv[0] of the pane process, or of a live
/// child under its carrier, still names the provider. Unobservable is not
/// positive evidence.
fn launched_as_provider(pid: u32, native: crate::provider::Provider) -> bool {
    let named = |pid: u32| {
        crate::platform::argv::argv_tokens(pid)
            .and_then(|argv| argv.into_iter().next())
            .is_some_and(|argv0| argv0_names(&argv0, native))
    };
    named(pid)
        || crate::platform::process::process_tree(pid)
            .unwrap_or_default()
            .into_iter()
            .any(|child| {
                child != pid && named(child) && crate::platform::process::pid_is_alive(child)
            })
}

/// Positive native/store or exact carrier+pane+provider observations only.
fn observe(selected: &SelectedTeam, id: &str, row: &Value) -> Result<Observation, CliError> {
    if !row.is_object()
        || row
            .get("provider")
            .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(refusal("invalid persisted provider metadata"));
    }
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row) {
        use team_agent_contract::{contract::types::SeatId, host::process::sample_process};
        if crate::contract_runtime::framework::is_paused_unstarted(row) {
            return Ok(Observation::Unstarted);
        }
        if checked(
            crate::contract_runtime::backend::start_needs_reconciliation(
                &selected.run_workspace,
                &selected.team_key,
                &checked(SeatId::new(id))?,
                row,
            ),
        )? {
            return Ok(Observation::NativeReconcile);
        }
        let backend = checked(crate::contract_runtime::backend::Backend::open_readonly(
            &selected.run_workspace,
            &selected.team_key,
        ))?;
        let seat = checked(backend.store.seat(&checked(SeatId::new(id))?))?
            .ok_or_else(|| refusal("missing canonical native seat"))?;
        if !same_provider(provider(row), seat.provider.as_str())
            || row.get("contract_scope").and_then(Value::as_str)
                != Some(seat.identity.scope.as_str())
            || row.get("contract_instance").and_then(Value::as_str)
                != Some(seat.identity.instance.as_str())
            || row.get("generation").and_then(Value::as_u64) != Some(seat.identity.generation.0)
        {
            return Ok(Observation::Unknown);
        }
        let Some(target) = seat.physical.as_ref() else {
            return Ok(Observation::Unknown);
        };
        return Ok(native_observation(
            seat.status,
            sample_process(&target.process),
            row.get("status").and_then(Value::as_str),
        ));
    }
    let native = crate::provider::wire::parse_provider(provider(row))
        .ok_or_else(|| refusal("unknown provider"))?;
    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    if !["running", "stopped", "dead"]
        .iter()
        .any(|known| status.eq_ignore_ascii_case(known))
    {
        return Ok(Observation::Unknown);
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
    if !status.eq_ignore_ascii_case("running") {
        return Ok(Observation::Unknown);
    }
    #[cfg(unix)]
    let carrier_birth = checked(team_agent_contract::host::process::process_birth_identity(
        pid,
    ))?;
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
    let Some(session) = session else {
        return Ok(Observation::Unknown);
    };
    let owner = checked(backend.session_owner(&crate::transport::SessionName::new(session)))?;
    let generation = crate::state::projection::session_generation(&selected.state);
    if owner.as_ref().is_none_or(|owner| {
        Path::new(&owner.workspace) != selected.run_workspace
            || owner.team != selected.team_key
            || Some(owner.generation.as_str()) != generation
    }) {
        return Ok(Observation::Unknown);
    }
    let command = matches[0].current_command.as_deref().unwrap_or("");
    let positive_command = command == crate::provider::wire::command_name(native)
        || crate::leader::provider_attribution::command_matches_provider(native, command)
        // A shell-supervised worker retains its native argv[0] in this exact
        // pane's live process tree; shell-only/stale-screen carriers still fail.
        || (!command.is_empty() && launched_as_provider(pid, native));
    let positive_surface = if !positive_command && !command.is_empty() {
        let frame = checked(
            backend.observe_native_input(&Target::Pane(PaneId::new(pane.unwrap_or(""))), native),
        )?;
        // Original start/restart supervises the native child with a shell carrier.
        // A shell alone (including its inert post-exit tail) is never Live.
        let mut live_child = false;
        if frame.is_some() && shell_carrier(command) {
            for child in checked(crate::platform::process::process_tree(pid))? {
                if child != pid
                    && matches!(
                        checked(crate::platform::process::pid_liveness(child))?,
                        crate::platform::process::ProcessLiveness::Live
                    )
                {
                    live_child = true;
                    break;
                }
            }
        }
        surface_liveness(command, frame.is_some(), live_child)
    } else {
        false
    };
    if !positive_command && !positive_surface {
        return Ok(Observation::Unknown);
    }
    #[cfg(unix)]
    if checked(team_agent_contract::host::process::process_birth_identity(
        pid,
    ))? != carrier_birth
    {
        return Ok(Observation::Unknown);
    }
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
fn check_incarnation_cwd(selected: &SelectedTeam, row: &Value) -> Result<(), CliError> {
    if let Some(cwd) = row
        .get("spawn_cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        let cwd = checked(Path::new(cwd).canonicalize())?;
        if !cwd.is_dir() || !cwd.starts_with(&selected.run_workspace) {
            return Err(refusal("legacy incarnation cwd is outside the selected workspace; no context or scope fallback"));
        }
    }
    Ok(())
}
fn admit_seat_preflight(plan: &mut Plan, id: &str, team: bool, result: Result<(), CliError>) -> Result<bool, CliError> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if team && plan.intent.verb == Verb::Start => {
            plan.skipped.push(json!({"agent":id,"reason":"preflight_blocked",
                "error":crate::redaction::redact_external_text(&error.to_string())}));
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn preflight_start(
    selected: &SelectedTeam,
    id: &str,
    row: &Value,
    session_policy: SessionPolicy,
) -> Result<(), CliError> {
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row) {
        let config = checked(crate::contract_runtime::framework::team_config(
            &selected.run_workspace,
            &selected.team_dir,
            &selected.team_key,
            Some(id),
        ))?
        .ok_or_else(|| refusal("missing native role config"))?;
        let role = config
            .roles
            .iter()
            .find(|r| r.id.as_str() == id)
            .ok_or_else(|| refusal("missing native role"))?;
        if crate::contract_runtime::framework::is_paused_unstarted(row) {
            checked(crate::contract_runtime::backend::preflight(&config))?;
        } else {
            if checked(crate::contract_runtime::backend::preflight_start_role(
                &selected.run_workspace,
                &selected.team_key,
                role,
            ))? {
                checked(crate::contract_runtime::backend::preflight(&config))?;
            }
        }
        return Ok(());
    }
    let provider = crate::provider::wire::parse_provider(provider(row))
        .ok_or_else(|| refusal("unknown provider before restart"))?;
    for field in ["session_id", "rollout_path", "last_result_at", "spawn_cwd"] {
        if row
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(refusal(format!("invalid resume metadata: {field}")));
        }
    }
    if crate::lifecycle::restart::classify_first_send_at(
        row.get("first_send_at").unwrap_or(&Value::Null),
    ) == crate::lifecycle::FirstSendAtState::Corrupt
    {
        return Err(refusal("invalid first_send_at metadata before stop"));
    }
    let session = row
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(crate::provider::SessionId::new);
    if session_policy == SessionPolicy::Preserve
        && !crate::lifecycle::restart::restart_agent_never_captured(
            row,
            session.as_ref().map(|sid| sid.as_str()),
        )
        && session.is_none()
    {
        return Err(refusal(
            "resume_not_ready before stop: captured session is missing for existing task context",
        ));
    }
    let rollout = row
        .get("rollout_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(crate::provider::RolloutPath::new);
    if session.is_some()
        && row
            .get("spawn_cwd")
            .and_then(Value::as_str)
            .is_none_or(|cwd| cwd.is_empty())
    {
        return Err(refusal("resume metadata lacks the scoped spawn cwd"));
    }
    check_incarnation_cwd(selected, row)?;
    if row
        .get("task_prompt_delivered")
        .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return Err(refusal("invalid task context metadata before stop"));
    }
    let (exists, checked_paths) = session
        .as_ref()
        .map(|session| {
            crate::lifecycle::restart::surface_resume_backing_probe(
                &selected.run_workspace,
                &AgentId::new(id),
                row,
                provider,
                session,
                rollout.as_ref(),
            )
        })
        .unwrap_or_default();
    let mode = crate::lifecycle::restart::decide_start_mode(
        crate::provider::wire::provider_wire(provider),
        session.as_ref(),
        rollout.as_ref(),
        exists,
        session_policy == SessionPolicy::Auto,
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
    // An explicit start re-admits a removed seat: the add consumes its retired
    // tombstone. Only lifecycle history this planner cannot classify is refused.
    if selected
        .state
        .get("agent_lifecycle")
        .and_then(|lifecycle| lifecycle.get(id))
        .is_some_and(|entry| entry.get("state").and_then(Value::as_str) != Some("retired"))
    {
        return Err(refusal(format!(
            "agent {id} has unrecognized lifecycle history; refusing to re-admit it"
        )));
    }
    let patch = plan.intent.patch.clone();
    let role_file = match source {
        Some(path) => {
            let (meta, _) = checked(crate::compiler::read_front_matter(&path))?;
            if (meta.get("provider").and_then(Yaml::as_str).is_none() && patch.provider.is_none())
                || (!matches!(
                    meta.get("dangerously_skip_permissions"),
                    Some(Yaml::Bool(_))
                ) && patch.bypass.is_none())
            {
                return Err(usage("role creation requires explicit provider and dangerously_skip_permissions decisions"));
            }
            role(plan, selected, id, Some(&path))?;
            path.to_string_lossy().into_owned()
        }
        None => {
            if patch.provider.is_none() || patch.bypass.is_none() {
                return Err(usage(
                    "unknown agent: supply a complete --role-file or --provider and --bypass",
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
fn no_eligible(mut plan: Plan, reason: &'static str) -> Plan {
    plan.steps.clear();
    plan.no_eligible = Some(reason);
    plan
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
        no_eligible: None,
        files: vec![],
        expected: BTreeMap::new(),
        topology: None,
    };
    let runtime_workspace = if plan.intent.pinned_workspace {
        plan.scope.workspace.clone()
    } else {
        checked(crate::model::paths::canonical_run_workspace(
            &plan.scope.workspace,
        ))?
    };
    let has_state = crate::state::persist::runtime_state_path(&runtime_workspace).is_file();
    if has_state {
        plan.scope.workspace = runtime_workspace;
    }
    if !has_state {
        let runtime = plan.scope.workspace.join(".team/runtime");
        let mut spec_only = plan.scope.workspace.join("team.spec.yaml").is_file();
        if runtime.is_dir() {
            for entry in std::fs::read_dir(runtime)? {
                spec_only |= entry?.path().join("team.spec.yaml").is_file();
            }
        }
        if spec_only {
            return Err(refusal("runtime spec exists without its state; select the owning workspace or repair it explicitly"));
        }
    }
    let raw = if has_state {
        Some(crate::state::persist::load_runtime_state_without_migrations(&plan.scope.workspace)?)
    } else {
        None
    };
    if raw.as_ref().is_some_and(|state| {
        !state.is_object() || state.get("teams").is_some_and(|teams| !teams.is_object())
    }) {
        return Err(refusal(
            "invalid runtime state; creation is not a repair fallback",
        ));
    }
    let new_named_team =
        raw.as_ref()
            .zip(plan.intent.team.as_deref())
            .is_some_and(|(raw, team)| {
                team != "current"
                    && !crate::lifecycle::launch::surface_has_runtime_team(raw, team)
                    && !(crate::state::projection::team_state_key(raw) == team
                        && crate::lifecycle::launch::surface_is_leader_bootstrap(raw))
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
        let compiled = checked(crate::compiler::compile_team(&source))?;
        validate_roster(&compiled, &source)?;
        let create = QuickStartArgs {
            workspace: plan.scope.workspace.clone(),
            agents_dir: source.clone(),
            name: None,
            team_id: plan.intent.team.clone(),
            yes: false,
            json: true,
            detail: true,
            backend: None,
        };
        #[cfg(unix)]
        if let Some(config) = checked(crate::contract_runtime::config::read_team(&create))? {
            if config.roles.len() == config.members.len()
                && config.roles.iter().all(|role| role.paused)
            {
                plan.skipped.extend(
                    config
                        .roles
                        .iter()
                        .map(|role| json!({"agent":role.id.as_str(),"reason":"paused"})),
                );
                return Ok(no_eligible(plan, "all configured native roles are paused"));
            }
        }
        remember(&mut plan, &source.join("TEAM.md"))?;
        for entry in std::fs::read_dir(source.join("agents"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "md") {
                remember(&mut plan, &path)?;
            }
        }
        plan.steps.push(Step::CreateTeam(create));
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
    let missing_spec = spec.is_none();
    if missing_spec && plan.intent.verb == Verb::Restart && target.is_none() {
        // Keep restart's stopped-roster fence ahead of spec/source admission.
        // Its original locked handler owns reconstruction for active rosters.
        let rows = selected
            .state
            .get("agents")
            .and_then(Value::as_object)
            .ok_or_else(|| refusal("missing agents"))?;
        let mut active = false;
        for (id, row) in rows {
            if !paused(row)
                && !crate::state::projection::agent_explicit_stop(row)
                && !crate::state::projection::agent_stopped_for_restart(row)
            {
                if observe(&selected, id, row)? == Observation::Unknown {
                    return Err(refusal("unknown physical identity"));
                }
                active = true;
            } else {
                plan.skipped.push(json!({"agent":id,"reason":if paused(row){"paused"}else if crate::state::projection::agent_explicit_stop(row){"explicit_stop"}else{"stopped_preserved_by_original_restart"}}));
            }
        }
        if !active {
            return Ok(no_eligible(
                plan,
                "stopped roster requires explicit team start",
            ));
        }
        plan.steps.push(Step::RestartTeam(restart_args(&plan)));
        return Ok(plan);
    }
    let runtime_spec =
        crate::model::paths::runtime_spec_path(&selected.run_workspace, &selected.team_key);
    let spec_value = match spec {
        Some(path) => checked(yaml::loads(&std::fs::read_to_string(path)?))?,
        None if plan.intent.verb != Verb::Stop => checked(
            crate::lifecycle::restart::compile_runtime_spec_from_roles(
                &selected.run_workspace, &selected.team_key, &selected.state,
            ),
        )?,
        None => return Err(refusal("runtime spec missing; team stop can inspect and stop the selected runtime without rebuilding it")),
    };
    validate_roster(
        &spec_value,
        spec.unwrap_or(&runtime_spec)
            .parent()
            .unwrap_or(&selected.run_workspace),
    )?;
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
        validate_roster(&compiled, &source)?;
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
            let admission = checked(crate::contract_runtime::framework::team_config(
                &selected.run_workspace, &selected.team_dir, &selected.team_key, Some(&id))).map(|_| ());
            if !admit_seat_preflight(&mut plan, &id, team, admission)? {
                continue;
            }
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
        let preserve_stop = team && explicit && matches!(plan.intent.verb, Verb::Start | Verb::Restart);
        let obs = if paused(row) || preserve_stop {
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
                let admission = check_native_noop_settings(&selected, &id, row);
                if !admit_seat_preflight(&mut plan, &id, team, admission)? {
                    continue;
                }
                if drift(row, &compiled) {
                    return Err(refusal(format!(
                        "known launch configuration drift for running {id}; stop before applying"
                    )));
                }
            }
            plan.skipped.push(json!({"agent":id,"reason":if paused(row){"paused"}else if preserve_stop{"explicit_stop"}else if obs==Observation::Live{"already_running"}else{"already_stopped"},"observation":obs}));
        } else {
            if plan.intent.verb != Verb::Stop {
                let (_, compiled) = role(&mut plan, &selected, &id, None)?;
                if !same_provider(provider(row), provider(&compiled)) {
                    return Err(refusal("registered provider cannot change"));
                }
                let admission = preflight_start(&selected, &id, row, plan.intent.session);
                if !admit_seat_preflight(&mut plan, &id, team, admission)? {
                    continue;
                }
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
        if obs == Observation::Live && !has_native_incarnation(row) {
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
            return Ok(no_eligible(
                plan,
                "use team start to explicitly activate a nonpaused roster",
            ));
        }
        let shutdown = selected.state.get("status").and_then(Value::as_str) == Some("shutdown");
        if !shutdown
            && !eligible.iter().any(|(id, _, _, _)| {
                rows.get(id.as_str())
                    .is_some_and(|row| !crate::state::projection::agent_stopped_for_restart(row))
            })
        {
            plan.skipped.extend(eligible.iter().map(|(id,obs,_,_)| json!({"agent":id,"observation":obs,"reason":"stopped_preserved_by_original_restart"})));
            return Ok(no_eligible(
                plan,
                "stopped roster requires explicit team start",
            ));
        }
        if !shutdown
            || eligible
                .iter()
                .any(|(_, obs, _, _)| *obs == Observation::Live)
        {
            for (id, obs, _, _) in &eligible {
                if rows
                    .get(id.as_str())
                    .is_some_and(crate::state::projection::agent_stopped_for_restart)
                {
                    plan.skipped.push(json!({"agent":id,"observation":obs,"reason":"stopped_preserved_by_original_restart"}));
                }
            }
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
        return Ok(no_eligible(plan, "no eligible nonpaused roster"));
    }
    if missing_spec {
        plan.steps.insert(
            0,
            Step::RestoreSpec {
                workspace: selected.run_workspace,
                team: selected.team_key,
                compiled: spec_value,
            },
        );
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
        let current: BTreeSet<_> = std::fs::read_dir(&role_dir)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| path.extension().is_some_and(|s| s == "md"))
            .collect();
        let planned: BTreeSet<_> = plan
            .files
            .iter()
            .map(|(path, _)| path)
            .filter(|path| path.parent() == Some(role_dir.as_path()))
            .cloned()
            .collect();
        if current != planned {
            return Err(refusal("creation roster changed since planning"));
        }
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
    if plan
        .topology
        .as_ref()
        .is_some_and(|expected| topology(&selected.state) != *expected)
    {
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
            if !same_provider(provider(row), provider(expected))
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
