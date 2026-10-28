//! Generic contract-native seat fork (avatar): one pipeline for every leaf.
//! The descriptor's fork facet selects the admitted mode, the leaf's H2/H7
//! supply native argv and stage intent, and the shared `Lifecycle::fork` runs
//! F0-F5 with its own compensation. Only the registry dispatches per leaf; a
//! refused provider leaves no role, spec, state, pane or native file behind.
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use team_agent_contract::contract::{
    fork::{resolve_fork, select_new_seat_mode, ForkMode, ForkRequest},
    plan::{resolve_launch, validate_launch_plan},
    types::{ContractError, Operation, OperationId, SeatId},
};
use team_agent_contract::host::{clock::RealClock, command::RealCommandRunner, materialize::ScopedMaterializer};
use team_agent_contract::orchestration::{
    lifecycle::{Adapter, Lifecycle, Routing},
    physical::{PhysicalRuntime, Registration},
    store::{OperationRecord, Outcome, SeatStatus},
    Error,
};

use crate::cli::CliError;
use crate::lifecycle::launch::add_agent_state::{inject_agent_into_spec, rollback_add_agent_atomic, upsert_agent_entry};
use crate::model::ids::AgentId;
use crate::model::yaml::Value as Yaml;
use crate::state::selector::SelectedTeam;

use super::{
    backend::{self, Backend, BackendError},
    commands,
    config::{MemberRoute, RoleConfig},
    discovery, framework, registry,
};

fn error(error: impl std::fmt::Display) -> CliError {
    CliError::Runtime(error.to_string())
}

fn refused(provider: &str, refusal: ContractError) -> CliError {
    CliError::Runtime(format!(
        "{provider} does not support session forking: {refusal}; no seat, pane or file was created"
    ))
}

/// `fork-agent <source> --as <target>` for a contract seat. None: the source
/// is not a contract seat and keeps its own runtime's fork path.
pub fn fork_agent(
    workspace: &Path,
    team: Option<&str>,
    source: &str,
    target: &str,
    label: Option<&str>,
) -> Result<Option<Value>, CliError> {
    commands::with_agent(workspace, team, source, "fork-agent", |selected, source_id| {
        fork_locked(selected, source_id, target, label)
    })
}

/// Called under the source's lifecycle lock (`with_agent`).
fn fork_locked(
    selected: &SelectedTeam,
    source_id: &SeatId,
    target: &str,
    label: Option<&str>,
) -> Result<Value, CliError> {
    let workspace = selected.run_workspace.as_path();
    let team = selected.team_key.as_str();
    // F0 is read-only: nothing below writes until the role file.
    let parent = Backend::open_readonly(workspace, team).map_err(error)?
        .store.seat(source_id).map_err(error)?
        .ok_or_else(|| error("contract source seat is not in its native store"))?;
    let provider = parent.provider.as_str().to_owned();
    let descriptor = registry::provider(&provider).map_err(error)?.descriptor();
    if target == source_id.as_str() {
        // The same seat can only branch in place, and only when admitted.
        descriptor.fork.in_window.require("in-window branch").map_err(|refusal| refused(&provider, refusal))?;
        return Err(error("contract_capability_unverified: in-window branch dispatch is not wired; no key was sent"));
    }
    if select_new_seat_mode(descriptor).map_err(|refusal| refused(&provider, refusal))? != ForkMode::NativeNewSeat {
        return Err(error(format!(
            "contract_capability_unverified: {provider} full-snapshot staging is not wired; no seat was created"
        )));
    }
    let target_id = SeatId::new(target).map_err(|_| error(format!("invalid fork target id: {target}")))?;
    if target == "leader" {
        return Err(error("fork target id `leader` is reserved"));
    }
    let source_session = parent.session.as_ref()
        .filter(|_| parent.status != SeatStatus::Unknown)
        .map(|session| session.native_session.as_str().to_owned())
        .ok_or_else(|| error("contract source seat has no captured native session to fork"))?;
    if selected.state.get("agents").and_then(|agents| agents.get(target)).is_some() {
        return Err(error(format!("agent id already exists: {target}")));
    }
    let role_path = selected.team_dir.join("agents").join(format!("{target}.md"));
    if std::fs::symlink_metadata(&role_path).is_ok() {
        return Err(error(format!("role file already exists: {}", role_path.display())));
    }
    let config = framework::team_config(workspace, &selected.team_dir, team).map_err(error)?
        .ok_or_else(|| error("contract team configuration is unavailable"))?;
    let source_role = config.roles.iter().find(|role| &role.id == source_id)
        .ok_or_else(|| error("contract source role is no longer configured"))?;
    if source_role.provider != parent.provider {
        return Err(error("contract source role now declares another provider; a fork keeps the parent's"));
    }
    let role_text = fork_role_text(&std::fs::read_to_string(&source_role.source).map_err(error)?, target, label);
    let spec_path = crate::model::paths::runtime_spec_path(workspace, team);
    let pre_spec = match std::fs::read_to_string(&spec_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(error(e)),
    };
    write_new(&role_path, role_text.as_bytes()).map_err(error)?;
    let undo = Undo {
        workspace: workspace.to_path_buf(), team: team.to_owned(), target: target_id.clone(),
        role_path: role_path.clone(), role_bytes: role_text.into_bytes(), spec_path: spec_path.clone(), pre_spec,
    };
    let result = (|| -> Result<Value, CliError> {
        let config = framework::team_config(workspace, &selected.team_dir, team).map_err(error)?
            .ok_or_else(|| error("contract team configuration is unavailable"))?;
        let role = config.roles.iter().find(|role| role.id == target_id)
            .ok_or_else(|| error("forked contract role was not configured"))?;
        if role.paused || role.provider != parent.provider {
            return Err(error("forked contract role is paused or changed provider; nothing was started"));
        }
        register_spec(selected, &role_path, target, &spec_path, undo.pre_spec.as_deref())?;
        let record = native_fork(workspace, team, source_id, role, &config.members).map_err(error)?;
        if record.outcome != Outcome::Committed {
            return Err(error(BackendError::Shutdown {
                operation: record.id, phase: record.phase, outcome: record.outcome, failure: record.failure,
            }));
        }
        // The roster row appears only once the native seat has committed: a
        // half-built target never enters the coordinator tick's projection.
        upsert_agent_entry(workspace, team, &AgentId::new(target), json!({
            "agent_id": target, "provider": role.provider.as_str(), "runtime_family": "contract",
            "status": "starting", "contract_instance": null, "pane_id": null, "forked_from": source_id.as_str(),
        })).map_err(error)?;
        let projection = commands::publish_at(workspace, team, &target_id, true)?;
        let coordinator = crate::coordinator::start_coordinator_with_team(
            &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()), Some(team),
        ).map_err(error)?;
        Ok(json!({"ok":coordinator.ok,"status":"forked","runtime":"contract","mode":"native_new_seat",
            "provider":provider,"source_agent_id":source_id.as_str(),"new_agent_id":target,"target":target,
            "agent_id":target,"operation":record.id,"source_session_id":source_session,
            "session_id":projection.get("session_id"),"new_session_id":projection.get("session_id"),
            "generation":projection.get("generation"),"backing_state":"verified",
            "coordinator":crate::lifecycle::coordinator_start_summary_value(
                &crate::lifecycle::CoordinatorStartSummary::from_start_report(&coordinator))}))
    })();
    result.map_err(|failure| undo.run(failure))
}

/// The provider-neutral F1-F5: H2 plans the native fork argv for the target,
/// H7 states the intent, and `Lifecycle::fork` registers, spawns, captures the
/// child binding and commits, compensating its own effects on failure.
fn native_fork(
    workspace: &Path,
    team: &str,
    source: &SeatId,
    role: &RoleConfig,
    members: &[MemberRoute],
) -> Result<OperationRecord, BackendError> {
    let mut backend = Backend::open(workspace, team)?;
    backend::require_candidate(&backend.binding)?;
    if !backend::seat_unclaimed(&backend, &role.id)? {
        return Err(Error::Conflict.into());
    }
    let parent = backend.store.seat(source)?.ok_or(Error::Invalid("unknown native seat"))?;
    let session = parent.session.clone().ok_or(ContractError::Invalid("native fork source session"))?;
    if parent.status == SeatStatus::Unknown || parent.provider != role.provider {
        return Err(Error::Fence.into());
    }
    let selected = registry::provider(role.provider.as_str())?;
    let descriptor = selected.descriptor();
    let discovered = discovery::discover_runtime(&role.provider, workspace)?;
    backend::publish_framework_routes(&mut backend, members)?;
    let identity = backend.store.propose_identity(&role.id)?;
    let server_key = selected.server_key_for(&identity)?;
    backend::write_role(&backend.binding, &identity, role)?;
    let adapter = backend::launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let mut launch = backend::request(&backend.binding, role, identity, &discovered)?;
    launch.operation = Operation::NativeNewSeat;
    if !descriptor.workspace.shared_cwd {
        launch.paths.cwd = super::workspace::prepare(&backend.binding, &role.id)?;
    }
    backend::require_private_primary(selected, &launch.paths.cwd)?;
    backend::preassign_session(&mut launch, &discovered)?;
    let operation = OperationId::new(format!("fork-{}", launch.identity.instance.as_str()))?;
    let request = ForkRequest {
        mode: ForkMode::NativeNewSeat,
        auth: launch.auth,
        source: session,
        expected_source: parent.identity.clone(),
        target: launch.identity.clone(),
        target_native_session: None,
        target_backing: None,
        channel: launch.channel,
        input_profile: launch.input_profile.clone(),
        cwd: launch.paths.cwd.clone(),
        native: launch.native.clone(),
        evidence_kind: launch.evidence_kind,
        selected_turn: None,
        operation_id: operation.clone(),
    };
    launch.fork = Some(Box::new(resolve_fork(descriptor, &hooks, &request)?));
    let resolved = resolve_launch(descriptor, &hooks, &launch, Some(&discovered.catalog))?;
    let plan = adapter.plan(&resolved)?;
    validate_launch_plan(descriptor, &resolved, &plan)?;
    let (workspace_topology, _, _) = backend::workspace_tmux(&backend.binding, &role.id, None)?;
    let mut io = ScopedMaterializer::new(descriptor, &resolved, &plan, operation.clone(), RealCommandRunner::default())?;
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor,
            hooks: &hooks,
            controls: selected.controls(),
            startup: selected.startup(),
            session_panel: selected.session_panel(),
        },
        backend::settings(&backend.binding, selected),
        operation.clone(),
        &clock,
    )?;
    physical.evidence = backend::policy_evidence(&role.provider, &launch.native, backend.binding.candidate_sha256)?;
    physical.workspace_tmux = Some(workspace_topology);
    let routing = Routing {
        pane: "unpublished".into(),
        binding_key: format!("binding-{}", launch.identity.instance.as_str()),
        server_key,
    };
    let record = Lifecycle { store: &mut backend.store, host: &mut physical, io: &mut io }.fork(
        &Adapter { descriptor, hooks: &hooks, catalog: Some(&discovered.catalog) },
        &request,
        Some((&launch, routing)),
    )?;
    Ok(record)
}

/// The target's runtime spec entry and route, compiled exactly as add-agent does.
fn register_spec(
    selected: &SelectedTeam,
    role_path: &Path,
    target: &str,
    spec_path: &Path,
    pre_spec: Option<&str>,
) -> Result<(), CliError> {
    let mut spec = crate::compiler::compile_team(&selected.team_dir).map_err(error)?;
    crate::lifecycle::launch::spec_state::override_spec_workspace(&mut spec, &selected.run_workspace);
    let workspace_text = spec.get("team").and_then(|team| team.get("workspace")).and_then(Yaml::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| selected.run_workspace.to_string_lossy().into_owned());
    let team_meta = crate::compiler::read_front_matter(&selected.team_dir.join("TEAM.md"))
        .map(|(meta, _)| meta)
        .unwrap_or(Yaml::Null);
    let compiled = crate::compiler::compile_role_agent(role_path, &team_meta, &workspace_text).map_err(error)?;
    if compiled.id != target {
        return Err(error(format!("forked role compiles to '{}', not '{target}'", compiled.id)));
    }
    let mut latest = pre_spec.and_then(|text| crate::model::yaml::loads(text).ok()).unwrap_or(spec);
    inject_agent_into_spec(&mut latest, compiled.agent, &compiled.id).map_err(error)?;
    crate::lifecycle::launch::spec_state::write_spec_atomic(spec_path, &latest).map_err(error)
}

/// The parent's role document under the target's identity: its own name, the
/// optional label as role, every other field and the body unchanged.
fn fork_role_text(source: &str, target: &str, label: Option<&str>) -> String {
    let (mut meta, body) = crate::compiler::split_front_matter(source);
    if let Yaml::Map(pairs) = &mut meta {
        pairs.retain(|(key, _)| key != "agent_id");
        set_field(pairs, "name", target);
        if let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) {
            set_field(pairs, "role", label);
        }
    }
    format!("---\n{}---\n\n{}", crate::lifecycle::launch::role_source::dump_role_frontmatter(&meta), body)
}

fn set_field(pairs: &mut Vec<(String, Yaml)>, key: &str, value: &str) {
    match pairs.iter_mut().find(|(name, _)| name == key) {
        Some((_, old)) => *old = Yaml::Str(value.to_owned()),
        None => pairs.push((key.to_owned(), Yaml::Str(value.to_owned()))),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Compensation for this fork's own registration. A started but unpublished
/// native seat is stopped first; if that fails, the role/spec/state stay so
/// status and stop can still reach the live seat.
struct Undo {
    workspace: PathBuf,
    team: String,
    target: SeatId,
    role_path: PathBuf,
    role_bytes: Vec<u8>,
    spec_path: PathBuf,
    pre_spec: Option<String>,
}

impl Undo {
    fn run(&self, failure: CliError) -> CliError {
        let started = Backend::open_readonly(&self.workspace, &self.team)
            .ok()
            .and_then(|backend| backend.store.seat(&self.target).ok().flatten())
            .is_some_and(|seat| seat.status != SeatStatus::Stopped && seat.physical.is_some());
        if started {
            if let Err(stop) = backend::stop(&self.workspace, &self.team, &self.target) {
                return error(format!(
                    "{failure}; stopping the forked native seat failed: {stop}; its role, spec and state were kept"
                ));
            }
        }
        rollback_add_agent_atomic(
            &self.workspace, &self.spec_path, self.pre_spec.as_deref(), None,
            Some(self.team.as_str()), &AgentId::new(self.target.as_str()), "fork_agent_failed",
        );
        let residual = match std::fs::read(&self.role_path) {
            Ok(bytes) if bytes == self.role_bytes => std::fs::remove_file(&self.role_path)
                .err()
                .map(|e| format!("role file {}: {e}", self.role_path.display())),
            Ok(_) => Some(format!("role file {} changed; preserved", self.role_path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(format!("role file {}: {e}", self.role_path.display())),
        };
        match residual {
            None => failure,
            Some(residual) => error(format!("{failure}; compensation incomplete: {residual}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fork_role_text;
    use crate::model::yaml::Value;

    #[test]
    fn forked_role_keeps_its_body_and_takes_the_target_identity() {
        let source = "---\nname: opus-architect\nagent_id: opus-architect\nprovider: claude\nmodel: claude-opus-5-5\n---\n\nYou design the system.\n";
        let forked = fork_role_text(source, "opus-fork", Some(" avatar "));
        let (meta, body) = crate::compiler::split_front_matter(&forked);
        assert_eq!(meta.get("name").and_then(Value::as_str), Some("opus-fork"));
        assert!(meta.get("agent_id").is_none());
        assert_eq!(meta.get("role").and_then(Value::as_str), Some("avatar"));
        assert_eq!(meta.get("provider").and_then(Value::as_str), Some("claude"));
        assert_eq!(meta.get("model").and_then(Value::as_str), Some("claude-opus-5-5"));
        assert_eq!(body.trim(), "You design the system.");
        let unlabeled = fork_role_text("plain body\n", "opus-fork", None);
        let (meta, body) = crate::compiler::split_front_matter(&unlabeled);
        assert_eq!(meta.get("name").and_then(Value::as_str), Some("opus-fork"));
        assert!(meta.get("role").is_none());
        assert_eq!(body.trim(), "plain body");
    }
}
