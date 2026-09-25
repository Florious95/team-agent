//! Pi 的锁内新席位 fork：稳定读取 source JSONL，写入独立 session backing 后精确 resume。
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::lifecycle::launch::add_agent_state::{
    fork_upsert_agent_state_from_role, inject_agent_into_spec,
};
use crate::lifecycle::launch::role_source::{
    materialize_latest_role, resolve_role_source, MaterializedRole,
};
use crate::lifecycle::launch::ForkCaptureSeed;
use crate::lifecycle::{
    AgentActionEnvelope, ForkAgentReport, ForkBackingState, LifecycleError, PiForkDetails,
    StartMode,
};
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value as YamlValue};
use crate::provider::{CaptureVia, CapturedSession, Confidence, Provider, RolloutPath, SessionId};
use crate::state::selector::SelectedTeam;
use crate::transport::{BackendKind, SessionName, Transport, WindowName};

use super::common::state_session_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone)]
struct SourceBinding {
    session_id: SessionId,
    backing_path: PathBuf,
    sessions_root: PathBuf,
    spawn_cwd: PathBuf,
    captured_at: String,
    captured_via: CaptureVia,
    spawned_at: String,
    spawn_epoch: Option<u64>,
    identity: FileIdentity,
}

struct SourceSnapshot {
    body: Vec<u8>,
}

struct StagedPiSession {
    seat_paths: crate::lifecycle::launch::pi_mcp::PiSeatPaths,
    backing_path: PathBuf,
    bytes: Vec<u8>,
    identity: FileIdentity,
    created_dirs: Vec<PathBuf>,
    wrapper_identity: Option<FileIdentity>,
    session_id: SessionId,
    captured_at: String,
    sha256: String,
}

struct SpecRegistration {
    path: PathBuf,
    previous_bytes: Option<Vec<u8>>,
    written_bytes: Vec<u8>,
    target_agent: YamlValue,
    identity: Option<FileIdentity>,
}

/// Called only while `fork_agent_with_transport` holds the selected team's lifecycle lock.
pub(crate) fn fork_pi_new_seat_locked(
    selected: &SelectedTeam,
    source_agent_id: &AgentId,
    target_agent_id: &AgentId,
    label: Option<&str>,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let run_workspace = &selected.run_workspace;
    let team_key = selected.team_key.as_str();
    let mut phase = "p0_preflight";
    let mut staged = None;
    let mut role: Option<MaterializedRole> = None;
    let mut spec_registration: Option<SpecRegistration> = None;
    let mut state_registered = false;
    let mut spawn_attempted = false;
    let mut spawned = None;
    let mut panes_before = BTreeSet::new();
    let mut audit = serde_json::json!({
        "team": team_key,
        "source_agent_id": source_agent_id.as_str(),
        "target_agent_id": target_agent_id.as_str(),
        "source_session_id": null,
        "target_session_id": null,
        "source_backing_path": null,
        "backing_path": null,
        "snapshot_bytes": null,
        "snapshot_sha256": null,
        "target_pane_id": null,
        "tmux_endpoint": transport.tmux_endpoint(),
    });

    let operation = (|| -> Result<ForkAgentReport, LifecycleError> {
        validate_path_component(source_agent_id.as_str(), "source agent id")?;
        validate_path_component(target_agent_id.as_str(), "target agent id")?;
        validate_path_component(team_key, "team id")?;
        if source_agent_id == target_agent_id {
            return Err(LifecycleError::RequirementUnmet(
                "Pi fork target must differ from source".to_string(),
            ));
        }
        if transport.kind() != BackendKind::Tmux {
            return Err(LifecycleError::TeamSelect(
                "Pi fork requires the selected team's tmux transport".to_string(),
            ));
        }
        let expected_transport = super::common::lifecycle_worker_tmux_backend_selection_for_state(
            run_workspace,
            &selected.state,
        )?
        .backend;
        if transport.probes_real_tmux_socket_roots()
            && transport.tmux_endpoint() != expected_transport.tmux_endpoint()
        {
            return Err(LifecycleError::TeamSelect(
                "selected team tmux endpoint changed while fork was acquiring its lifecycle lock"
                    .to_string(),
            ));
        }
        crate::lifecycle::launch::ensure_owner_allowed_for_state(
            &selected.state,
            Some(source_agent_id),
        )?;
        if selected
            .state
            .get("agents")
            .and_then(|agents| agents.get(target_agent_id.as_str()))
            .is_some()
        {
            return Err(LifecycleError::RequirementUnmet(format!(
                "target agent already exists in selected team: {target_agent_id}"
            )));
        }
        let session_name = state_session_name(&selected.state);
        if !state_has_session_name(&selected.state) {
            return Err(LifecycleError::TeamSelect(
                "selected team has no persisted tmux session name".to_string(),
            ));
        }
        let source_agent = selected
            .state
            .get("agents")
            .and_then(|agents| agents.get(source_agent_id.as_str()))
            .ok_or_else(|| {
                LifecycleError::RequirementUnmet(format!(
                    "unknown worker agent id: {source_agent_id}"
                ))
            })?;
        if source_agent.get("provider").and_then(JsonValue::as_str) != Some("pi")
            || source_agent.get("auth_mode").and_then(JsonValue::as_str) != Some("subscription")
        {
            return Err(LifecycleError::Provider(
                "Pi session fork requires a Pi subscription source seat".to_string(),
            ));
        }
        let binding = source_binding(selected, source_agent_id)?;
        audit["source_session_id"] = serde_json::json!(binding.session_id.as_str());
        audit["source_backing_path"] =
            serde_json::json!(binding.backing_path.to_string_lossy().to_string());
        audit["source_captured_at"] = serde_json::json!(binding.captured_at.as_str());
        audit["source_captured_via"] = serde_json::json!(binding.captured_via);
        audit["source_attribution_confidence"] = serde_json::json!("high");
        audit["source_spawned_at"] = serde_json::json!(binding.spawned_at.as_str());
        audit["source_spawn_epoch"] = serde_json::json!(binding.spawn_epoch);
        let source_role_path = resolve_role_source(
            run_workspace,
            &selected.team_dir,
            &selected.state,
            source_agent_id,
        )?;
        let (source_role_meta, _) = crate::compiler::read_front_matter(&source_role_path)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        for field in ["working_directory", "cwd"] {
            if let Some(cwd) = source_role_meta.get(field).and_then(YamlValue::as_str) {
                if Path::new(cwd).canonicalize().ok().as_deref()
                    != Some(binding.spawn_cwd.as_path())
                {
                    return Err(LifecycleError::Compile(format!(
                        "Pi source role {field} differs from the captured source cwd"
                    )));
                }
            }
        }
        let team_meta = crate::compiler::read_front_matter(&selected.team_dir.join("TEAM.md"))
            .map(|(meta, _)| meta)
            .unwrap_or(YamlValue::Null);
        let workspace_text = run_workspace.to_string_lossy().to_string();
        let source_compiled =
            crate::compiler::compile_role_agent(&source_role_path, &team_meta, &workspace_text)
                .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        if source_compiled.id != source_agent_id.as_str() {
            return Err(LifecycleError::Compile(
                "latest source role id does not match the source seat".to_string(),
            ));
        }
        if source_compiled
            .agent
            .get("provider")
            .and_then(YamlValue::as_str)
            != Some("pi")
            || source_compiled
                .agent
                .get("auth_mode")
                .and_then(YamlValue::as_str)
                != Some("subscription")
        {
            return Err(LifecycleError::Compile(
                "latest source role must resolve to Pi subscription".to_string(),
            ));
        }

        let mut base_spec = crate::compiler::compile_team(&selected.team_dir)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        crate::lifecycle::launch::spec_state::override_spec_workspace(
            &mut base_spec,
            run_workspace,
        );
        let spec_path = crate::model::paths::runtime_spec_path(run_workspace, team_key);
        ensure_no_symlink_components(&spec_path, true)?;
        let previous_spec_bytes = read_optional_bytes(&spec_path)?;
        let runtime_spec_has_target = match previous_spec_bytes.as_deref() {
            Some(bytes) => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|error| LifecycleError::Compile(error.to_string()))?;
                let spec = yaml::loads(text)
                    .map_err(|error| LifecycleError::Compile(error.to_string()))?;
                spec_has_target(&spec, target_agent_id.as_str())
            }
            None => false,
        };
        if spec_has_target(&base_spec, target_agent_id.as_str()) || runtime_spec_has_target {
            return Err(LifecycleError::RequirementUnmet(format!(
                "target agent or route already exists in selected team spec: {target_agent_id}"
            )));
        }
        let managed_role_path = run_workspace
            .join(".team")
            .join("dynamic-role-files")
            .join(format!("{}.md", target_agent_id.as_str()));
        ensure_no_symlink_components(&managed_role_path, true)?;
        refuse_existing_path(&managed_role_path, "target managed role file")?;
        let target_paths = crate::lifecycle::launch::pi_mcp::pi_seat_paths(
            run_workspace,
            team_key,
            target_agent_id.as_str(),
        );
        ensure_no_symlink_components(&target_paths.runtime_root, true)?;
        refuse_existing_path(&target_paths.runtime_root, "target Pi runtime seat")?;
        let targets = transport
            .list_targets()
            .map_err(|error| LifecycleError::Transport(error.to_string()))?;
        if targets.iter().any(|pane| {
            pane.session.as_str() == session_name.as_str()
                && pane.window_name.as_ref().map(WindowName::as_str)
                    == Some(target_agent_id.as_str())
        }) {
            return Err(LifecycleError::RequirementUnmet(format!(
                "target window already exists: {}:{}",
                session_name.as_str(),
                target_agent_id.as_str()
            )));
        }
        panes_before.extend(
            targets
                .into_iter()
                .map(|pane| pane.pane_id.as_str().to_string()),
        );

        phase = "p1_snapshot";
        let snapshot = read_stable_source_snapshot(&binding)?;
        revalidate_source_binding(selected, source_agent_id, &binding)?;
        let target_session_id = crate::lifecycle::launch::pi_mcp::new_pi_session_id();
        let full_state = crate::state::persist::load_runtime_state(run_workspace)
            .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
        if target_session_id == binding.session_id
            || session_id_in_use(&full_state, &target_session_id)
        {
            return Err(LifecycleError::RequirementUnmet(
                "generated Pi fork session id is not unique in selected team".to_string(),
            ));
        }
        let captured_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let staged_file = stage_pi_session(
            run_workspace,
            &target_paths,
            &target_session_id,
            &captured_at,
            &binding.spawn_cwd,
            &binding.backing_path,
            &snapshot.body,
        )?;
        audit["target_session_id"] = serde_json::json!(target_session_id.as_str());
        audit["backing_path"] =
            serde_json::json!(staged_file.backing_path.to_string_lossy().to_string());
        audit["snapshot_bytes"] = serde_json::json!(staged_file.bytes.len());
        audit["snapshot_sha256"] = serde_json::json!(staged_file.sha256.as_str());
        staged = Some(staged_file);

        let target_role = materialize_latest_role(
            run_workspace,
            &selected.team_dir,
            &selected.state,
            source_agent_id,
            target_agent_id,
            label,
        )?;
        role = Some(target_role);
        let target_role_path = role
            .as_ref()
            .ok_or_else(|| {
                LifecycleError::StatePersist("materialized role disappeared".to_string())
            })?
            .path()
            .to_path_buf();
        let (target_meta, _) = crate::compiler::read_front_matter(&target_role_path)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        if let Some(cwd) = target_meta
            .get("working_directory")
            .and_then(YamlValue::as_str)
        {
            if Path::new(cwd).canonicalize().ok().as_deref() != Some(binding.spawn_cwd.as_path()) {
                return Err(LifecycleError::Compile(
                    "Pi fork source role working_directory differs from the source cohort cwd"
                        .to_string(),
                ));
            }
        }
        let compiled =
            crate::compiler::compile_role_agent(&target_role_path, &team_meta, &workspace_text)
                .map_err(|error| LifecycleError::Compile(error.to_string()))?;
        if compiled.id != target_agent_id.as_str() {
            return Err(LifecycleError::Compile(format!(
                "materialized role id '{}' does not match target '{}'",
                compiled.id, target_agent_id
            )));
        }
        #[cfg(test)]
        eprintln!(
            "pi-fork role trace label={label:?} meta.name={:?} meta.role={:?} meta.label={:?} compiled.id={} compiled.role={:?}",
            target_meta.get("name").and_then(YamlValue::as_str),
            target_meta.get("role").and_then(YamlValue::as_str),
            target_meta.get("label").and_then(YamlValue::as_str),
            compiled.id,
            compiled.agent.get("role").and_then(YamlValue::as_str),
        );
        let compiled_provider = compiled.agent.get("provider").and_then(YamlValue::as_str);
        let compiled_auth = compiled.agent.get("auth_mode").and_then(YamlValue::as_str);
        if compiled_provider != Some("pi") || compiled_auth != Some("subscription") {
            return Err(LifecycleError::Compile(
                "compiled target role must remain Pi subscription".to_string(),
            ));
        }
        let cwd = compiled
            .agent
            .get("working_directory")
            .and_then(YamlValue::as_str)
            .ok_or_else(|| {
                LifecycleError::Compile("Pi target role has no working_directory".to_string())
            })?;
        if Path::new(cwd).canonicalize().ok().as_deref() != Some(binding.spawn_cwd.as_path()) {
            return Err(LifecycleError::Compile(
                "Pi fork target role working_directory differs from the source cohort cwd"
                    .to_string(),
            ));
        }
        let staged_file = staged.as_ref().ok_or_else(|| {
            LifecycleError::StatePersist("Pi fork snapshot staging was lost".to_string())
        })?;
        let seed = ForkCaptureSeed {
            source_agent_id: source_agent_id.clone(),
            captured: CapturedSession {
                session_id: Some(staged_file.session_id.clone()),
                rollout_path: Some(RolloutPath::new(staged_file.backing_path.clone())),
                captured_via: CaptureVia::ForkSnapshot,
                attribution_confidence: Confidence::High,
                spawn_cwd: binding.spawn_cwd.clone(),
            },
            captured_at: staged_file.captured_at.clone(),
            pi_sessions_root: target_paths.sessions.clone(),
            profile_dir: selected.team_dir.join("profiles"),
        };

        phase = "p2_register";
        revalidate_source_binding(selected, source_agent_id, &binding)?;
        ensure_no_symlink_components(&spec_path, true)?;
        let current_spec_bytes = read_optional_bytes(&spec_path)?;
        if previous_spec_bytes.is_some() && current_spec_bytes.is_none() {
            return Err(LifecycleError::RequirementUnmet(
                "runtime spec disappeared during Pi fork preparation".to_string(),
            ));
        }
        let mut latest_spec = match current_spec_bytes.as_deref() {
            Some(bytes) => yaml::loads(std::str::from_utf8(bytes).map_err(|error| {
                LifecycleError::Compile(format!("runtime spec is not UTF-8: {error}"))
            })?)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?,
            None => base_spec,
        };
        if spec_has_target(&latest_spec, target_agent_id.as_str()) {
            return Err(LifecycleError::RequirementUnmet(format!(
                "target agent or route appeared in runtime spec during fork preparation: {target_agent_id}"
            )));
        }
        inject_agent_into_spec(
            &mut latest_spec,
            compiled.agent.clone(),
            target_agent_id.as_str(),
        )?;
        let written_bytes = yaml::dumps(&latest_spec).into_bytes();
        if read_optional_bytes(&spec_path)? != current_spec_bytes {
            return Err(LifecycleError::RequirementUnmet(
                "runtime spec changed while registering Pi fork target".to_string(),
            ));
        }
        spec_registration = Some(SpecRegistration {
            path: spec_path.clone(),
            previous_bytes: current_spec_bytes,
            written_bytes,
            target_agent: compiled.agent.clone(),
            identity: None,
        });
        crate::lifecycle::launch::spec_state::write_spec_atomic(&spec_path, &latest_spec)?;
        let spec_identity = file_identity(&spec_path).map_err(LifecycleError::StatePersist)?;
        if let Some(registration) = spec_registration.as_mut() {
            registration.identity = Some(spec_identity);
        }
        match fork_upsert_agent_state_from_role(
            run_workspace,
            team_key,
            target_agent_id,
            &target_meta,
            &compiled.agent,
            &target_role_path,
            &seed,
        ) {
            Ok(()) => state_registered = true,
            Err(error) => {
                state_registered = staged.as_ref().is_some_and(|staged| {
                    fork_target_state_registered(
                        run_workspace,
                        team_key,
                        target_agent_id,
                        source_agent_id,
                        &staged.session_id,
                        &staged.backing_path,
                        &target_role_path,
                    )
                });
                return Err(error);
            }
        }

        phase = "p3_spawn";
        spawn_attempted = true;
        let latest_state =
            crate::state::projection::select_runtime_state(run_workspace, Some(team_key))
                .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
        let target_row = latest_state
            .get("agents")
            .and_then(|agents| agents.get(target_agent_id.as_str()))
            .ok_or_else(|| {
                LifecycleError::StatePersist(
                    "registered Pi fork target row disappeared".to_string(),
                )
            })?;
        let safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn_json(
            target_row,
            Provider::Pi,
        )?;
        let spawn = super::common::spawn_agent_window(
            run_workspace,
            &session_name,
            target_agent_id,
            target_row,
            Some(
                &staged
                    .as_ref()
                    .ok_or_else(|| {
                        LifecycleError::StatePersist(
                            "Pi fork snapshot staging was lost".to_string(),
                        )
                    })?
                    .session_id,
            ),
            true,
            transport,
            Some(&safety),
            None,
            Some(run_workspace),
            Some("fork-agent"),
            Some(team_key),
        );
        let spawn = match spawn {
            Ok(spawn) => spawn,
            Err(error) => {
                capture_target_wrapper_identity(staged.as_mut());
                return Err(error);
            }
        };
        capture_target_wrapper_identity(staged.as_mut());
        audit["target_pane_id"] = serde_json::json!(spawn.spawn.pane_id.as_str());
        spawned = Some(spawn);
        let spawned_ref = spawned.as_ref().ok_or_else(|| {
            LifecycleError::StatePersist("Pi fork spawn result disappeared".to_string())
        })?;
        super::agent::verify_spawned_pane_matches_target(
            transport,
            &spawned_ref.spawn.pane_id,
            &session_name,
            &WindowName::new(target_agent_id.as_str()),
        )?;

        phase = "p4_persist";
        let mut updated_state =
            crate::state::projection::select_runtime_state(run_workspace, Some(team_key))
                .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
        super::agent::mark_agent_started(
            &mut updated_state,
            target_agent_id,
            target_agent_id.as_str(),
            spawned_ref,
            transport,
            &safety,
            StartMode::Resumed,
        )?;
        let updated_target = updated_state
            .get("agents")
            .and_then(|agents| agents.get(target_agent_id.as_str()))
            .cloned()
            .ok_or_else(|| {
                LifecycleError::StatePersist("started Pi fork target row disappeared".to_string())
            })?;
        crate::state::repository::StateRepository::new(run_workspace)
            .commit_fork_agent(
                crate::state::repository::StateWriteIntent::ForkAgent {
                    team_key,
                    agent_id: target_agent_id.as_str(),
                },
                &updated_target,
            )
            .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
        let _ = crate::db::agent_health_capture::clear_agent_health_observation(
            run_workspace,
            team_key,
            target_agent_id,
        );
        let coordinator =
            super::common::start_coordinator_for_workspace(run_workspace, Some(team_key))?;
        if !coordinator.ok {
            return Err(LifecycleError::StatePersist(format!(
                "coordinator did not become available after Pi fork: {}",
                coordinator.status
            )));
        }

        phase = "p5_receipt";
        let staged_file = staged.as_ref().ok_or_else(|| {
            LifecycleError::StatePersist("Pi fork snapshot staging was lost".to_string())
        })?;
        Ok(ForkAgentReport {
            source_agent_id: source_agent_id.clone(),
            new_agent_id: target_agent_id.clone(),
            env: AgentActionEnvelope {
                agent_id: target_agent_id.clone(),
                state_file: crate::state::persist::runtime_state_path(run_workspace),
                coordinator_started: coordinator.ok,
            },
            session_id: Some(staged_file.session_id.clone()),
            backing_state: ForkBackingState::Verified,
            pi_fork: Some(PiForkDetails {
                source_session_id: binding.session_id.clone(),
                session_id: staged_file.session_id.clone(),
                backing_path: RolloutPath::new(staged_file.backing_path.clone()),
            }),
        })
    })();

    match operation {
        Ok(report) => {
            if let Err(error) = write_fork_audit(
                run_workspace,
                &mut audit,
                "succeeded",
                "complete",
                "not_required",
            ) {
                let compensation = compensate_fork(
                    run_workspace,
                    team_key,
                    target_agent_id,
                    source_agent_id,
                    transport,
                    &session_name_for_audit(&selected.state),
                    &panes_before,
                    spawn_attempted,
                    spawned.as_ref(),
                    spec_registration.as_ref(),
                    state_registered,
                    staged.as_ref(),
                    role.as_mut(),
                );
                return Err(LifecycleError::StatePersist(if compensation.is_empty() {
                    format!("fork succeeded but audit receipt failed: {error}")
                } else {
                    format!("fork succeeded but audit receipt failed: {error}; compensation incomplete: {compensation}")
                }));
            }
            if let Some(role) = role.as_mut() {
                role.keep();
            }
            Ok(report)
        }
        Err(error) => {
            let compensation = compensate_fork(
                run_workspace,
                team_key,
                target_agent_id,
                source_agent_id,
                transport,
                &session_name_for_audit(&selected.state),
                &panes_before,
                spawn_attempted,
                spawned.as_ref(),
                spec_registration.as_ref(),
                state_registered,
                staged.as_ref(),
                role.as_mut(),
            );
            let result = if compensation.is_empty() {
                "complete"
            } else {
                "incomplete"
            };
            audit["failure"] = serde_json::json!(phase);
            let audit_error =
                write_fork_audit(run_workspace, &mut audit, "failed", phase, result).err();
            let audit_detail = audit_error
                .map(|audit_error| format!("; failure receipt failed: {audit_error}"))
                .unwrap_or_default();
            if compensation.is_empty() && audit_detail.is_empty() {
                Err(error)
            } else if compensation.is_empty() {
                Err(LifecycleError::StatePersist(format!(
                    "{error}{audit_detail}"
                )))
            } else {
                Err(LifecycleError::StatePersist(format!(
                    "{error}; compensation incomplete: {compensation}{audit_detail}"
                )))
            }
        }
    }
}

fn source_binding(
    selected: &SelectedTeam,
    source_agent_id: &AgentId,
) -> Result<SourceBinding, LifecycleError> {
    let source = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .ok_or_else(|| LifecycleError::RequirementUnmet("Pi source row disappeared".to_string()))?;
    if source.get("provider").and_then(JsonValue::as_str) != Some("pi")
        || source.get("auth_mode").and_then(JsonValue::as_str) != Some("subscription")
        || source.get("capture_state").and_then(JsonValue::as_str) != Some("captured")
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source does not have a complete captured subscription session".to_string(),
        ));
    }
    if source
        .get("owner_team_id")
        .and_then(JsonValue::as_str)
        .is_some_and(|owner| owner != selected.team_key)
    {
        return Err(LifecycleError::TeamSelect(
            "Pi source owner does not match the selected team".to_string(),
        ));
    }
    let session_id = source
        .get("session_id")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(SessionId::new)
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet("Pi source session_id is missing".to_string())
        })?;
    let attribution_confidence = source
        .get("attribution_confidence")
        .and_then(JsonValue::as_str);
    if attribution_confidence.is_some_and(|confidence| confidence != "high")
        || source
            .get("attribution_ambiguous")
            .and_then(JsonValue::as_bool)
            == Some(true)
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source capture attribution is explicitly low-confidence or ambiguous".to_string(),
        ));
    }
    let captured_at = required_state_string(source, "captured_at")?;
    let captured_via: CaptureVia = serde_json::from_value(serde_json::json!(
        required_state_string(source, "captured_via")?
    ))
    .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let spawned_at = required_state_string(source, "spawned_at")?;
    chrono::DateTime::parse_from_rfc3339(&captured_at)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    chrono::DateTime::parse_from_rfc3339(&spawned_at)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let spawn_cwd = PathBuf::from(required_state_string(source, "spawn_cwd")?);
    if !spawn_cwd.is_absolute() {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source spawn_cwd is not absolute".to_string(),
        ));
    }
    let canonical_cwd = fs::canonicalize(&spawn_cwd)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if spawn_cwd != selected.run_workspace || canonical_cwd != selected.run_workspace {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source spawn_cwd differs from the selected team workspace".to_string(),
        ));
    }
    for field in ["cwd", "working_directory"] {
        if let Some(cwd) = source.get(field).and_then(JsonValue::as_str) {
            if Path::new(cwd).canonicalize().ok().as_deref()
                != Some(selected.run_workspace.as_path())
            {
                return Err(LifecycleError::RequirementUnmet(format!(
                    "Pi source {field} differs from the selected target working directory"
                )));
            }
        }
    }
    let backing_path = PathBuf::from(required_state_string(source, "rollout_path")?);
    if backing_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("jsonl")
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source backing is not a .jsonl session file".to_string(),
        ));
    }
    if !backing_path.is_absolute() {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source rollout_path is not absolute".to_string(),
        ));
    }
    ensure_no_symlink_components(&backing_path, false)?;
    let canonical_backing = fs::canonicalize(&backing_path)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if canonical_backing != backing_path {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source rollout_path is not its canonical exact path".to_string(),
        ));
    }
    let expected_root = crate::lifecycle::launch::pi_mcp::pi_seat_paths(
        &selected.run_workspace,
        &selected.team_key,
        source_agent_id.as_str(),
    )
    .sessions;
    ensure_no_symlink_components(&expected_root, false)?;
    let canonical_root = fs::canonicalize(&expected_root)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if source
        .get("claude_projects_root")
        .and_then(JsonValue::as_str)
        .map(PathBuf::from)
        .and_then(|path| fs::canonicalize(path).ok())
        .as_ref()
        != Some(&canonical_root)
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source recorded session root does not match its managed seat".to_string(),
        ));
    }
    if !canonical_backing.starts_with(&canonical_root) {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source backing is outside its recorded session root".to_string(),
        ));
    }
    crate::provider::session_scan::pi::validate_exact_backing(
        &backing_path,
        &session_id,
        &canonical_cwd,
    )
    .map_err(|error| LifecycleError::Provider(error.to_string()))?;
    let identity = file_identity(&backing_path).map_err(LifecycleError::RequirementUnmet)?;
    Ok(SourceBinding {
        session_id,
        backing_path,
        sessions_root: canonical_root,
        spawn_cwd: canonical_cwd,
        captured_at: captured_at.to_string(),
        captured_via,
        spawned_at: spawned_at.to_string(),
        spawn_epoch: source.get("spawn_epoch").and_then(JsonValue::as_u64),
        identity,
    })
}

fn revalidate_source_binding(
    selected: &SelectedTeam,
    source_agent_id: &AgentId,
    expected: &SourceBinding,
) -> Result<(), LifecycleError> {
    let current = crate::state::projection::select_runtime_state(
        &selected.run_workspace,
        Some(&selected.team_key),
    )
    .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let latest = source_binding_from_state(&current, selected, source_agent_id)?;
    if latest.session_id != expected.session_id
        || latest.backing_path != expected.backing_path
        || latest.sessions_root != expected.sessions_root
        || latest.spawn_cwd != expected.spawn_cwd
        || latest.captured_at != expected.captured_at
        || latest.captured_via != expected.captured_via
        || latest.spawned_at != expected.spawned_at
        || latest.spawn_epoch != expected.spawn_epoch
        || latest.identity != expected.identity
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source session cohort changed during fork preparation".to_string(),
        ));
    }
    Ok(())
}

fn source_binding_from_state(
    state: &JsonValue,
    selected: &SelectedTeam,
    source_agent_id: &AgentId,
) -> Result<SourceBinding, LifecycleError> {
    let mut snapshot = selected.clone();
    snapshot.state = state.clone();
    source_binding(&snapshot, source_agent_id)
}

fn read_stable_source_snapshot(binding: &SourceBinding) -> Result<SourceSnapshot, LifecycleError> {
    ensure_no_symlink_components(&binding.backing_path, false)?;
    let before_path = fs::symlink_metadata(&binding.backing_path)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if !before_path.file_type().is_file()
        || identity_from_metadata(&before_path) != binding.identity
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source backing is not the captured regular file".to_string(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&binding.backing_path)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let open_before = file
        .metadata()
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if identity_from_metadata(&open_before) != binding.identity {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source backing changed while opening".to_string(),
        ));
    }
    let mut first = Vec::new();
    file.read_to_end(&mut first)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let middle = file
        .metadata()
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let mut second = Vec::new();
    file.read_to_end(&mut second)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let after = file
        .metadata()
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    let after_path = fs::symlink_metadata(&binding.backing_path)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if first != second
        || identity_from_metadata(&middle) != binding.identity
        || identity_from_metadata(&after) != binding.identity
        || identity_from_metadata(&after_path) != binding.identity
        || middle.len() != after.len()
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi source JSONL changed during stable snapshot read".to_string(),
        ));
    }
    let body = validate_session_bytes(&first, &binding.session_id, &binding.spawn_cwd, None)?;
    Ok(SourceSnapshot { body })
}

fn validate_session_bytes(
    bytes: &[u8],
    expected_id: &SessionId,
    expected_cwd: &Path,
    expected_parent: Option<&Path>,
) -> Result<Vec<u8>, LifecycleError> {
    let header_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet("Pi session header is incomplete".to_string())
        })?;
    let header_line = trim_line_ending(&bytes[..header_end]);
    let header: JsonValue = serde_json::from_slice(header_line)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if header.get("type").and_then(JsonValue::as_str) != Some("session")
        || header.get("version").and_then(JsonValue::as_u64) != Some(3)
        || header.get("id").and_then(JsonValue::as_str) != Some(expected_id.as_str())
    {
        return Err(LifecycleError::RequirementUnmet(
            "Pi session header type/version/id mismatch".to_string(),
        ));
    }
    let cwd = header
        .get("cwd")
        .and_then(JsonValue::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| LifecycleError::RequirementUnmet("Pi session cwd is missing".to_string()))?;
    if !cwd.is_absolute() || cwd.as_path() != expected_cwd {
        return Err(LifecycleError::RequirementUnmet(
            "Pi session header cwd does not match the captured cohort".to_string(),
        ));
    }
    let timestamp = header
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet("Pi session timestamp is missing".to_string())
        })?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
    if let Some(expected_parent) = expected_parent {
        if header
            .get("parentSession")
            .and_then(JsonValue::as_str)
            .map(PathBuf::from)
            .as_deref()
            != Some(expected_parent)
        {
            return Err(LifecycleError::RequirementUnmet(
                "Pi fork parentSession does not match the exact source backing".to_string(),
            ));
        }
    }
    let body = &bytes[header_end + 1..];
    if !body.is_empty() && !body.ends_with(b"\n") {
        return Err(LifecycleError::RequirementUnmet(
            "Pi session has an incomplete trailing JSONL record".to_string(),
        ));
    }
    let mut ids = HashSet::new();
    for record in body.split_inclusive(|byte| *byte == b'\n') {
        if record.is_empty() || !record.ends_with(b"\n") {
            return Err(LifecycleError::RequirementUnmet(
                "Pi session contains an incomplete JSONL record".to_string(),
            ));
        }
        let json: JsonValue = serde_json::from_slice(trim_line_ending(record))
            .map_err(|error| LifecycleError::RequirementUnmet(error.to_string()))?;
        if json
            .get("type")
            .and_then(JsonValue::as_str)
            .is_none_or(|kind| kind.is_empty() || kind == "session")
        {
            return Err(LifecycleError::RequirementUnmet(
                "Pi session entry type is missing or invalid".to_string(),
            ));
        }
        let id = json
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                LifecycleError::RequirementUnmet("Pi session entry id is missing".to_string())
            })?;
        if ids.contains(id) {
            return Err(LifecycleError::RequirementUnmet(format!(
                "Pi session entry id is duplicated: {id}"
            )));
        }
        let parent = json.get("parentId").ok_or_else(|| {
            LifecycleError::RequirementUnmet("Pi session entry parentId is missing".to_string())
        })?;
        match parent {
            JsonValue::Null => {}
            JsonValue::String(parent) if ids.contains(parent) => {}
            JsonValue::String(parent) => {
                return Err(LifecycleError::RequirementUnmet(format!(
                    "Pi session parentId does not reference an earlier entry: {parent}"
                )))
            }
            _ => {
                return Err(LifecycleError::RequirementUnmet(
                    "Pi session parentId must be string or null".to_string(),
                ))
            }
        }
        ids.insert(id.to_string());
    }
    Ok(body.to_vec())
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn stage_pi_session(
    workspace: &Path,
    paths: &crate::lifecycle::launch::pi_mcp::PiSeatPaths,
    session_id: &SessionId,
    timestamp: &str,
    cwd: &Path,
    parent_session: &Path,
    body: &[u8],
) -> Result<StagedPiSession, LifecycleError> {
    let file_timestamp = timestamp.replace(':', "-").replace('.', "-");
    let backing_path = paths
        .sessions
        .join(format!("{file_timestamp}_{}.jsonl", session_id.as_str()));
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": session_id.as_str(),
        "timestamp": timestamp,
        "cwd": cwd.to_string_lossy(),
        "parentSession": parent_session.to_string_lossy(),
    });
    let mut bytes = serde_json::to_vec(&header)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    bytes.push(b'\n');
    bytes.extend_from_slice(body);
    validate_session_bytes(&bytes, session_id, cwd, Some(parent_session))?;
    let created_dirs = create_target_session_dirs(workspace, paths)?;
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&backing_path)
    {
        Ok(file) => file,
        Err(error) => {
            cleanup_created_dirs(&created_dirs);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                LifecycleError::RequirementUnmet(format!(
                    "target Pi backing already exists: {}",
                    backing_path.display()
                ))
            } else {
                LifecycleError::StatePersist(error.to_string())
            });
        }
    };
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&backing_path);
        cleanup_created_dirs(&created_dirs);
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    drop(file);
    let stored = match fs::read(&backing_path) {
        Ok(stored) => stored,
        Err(error) => {
            let _ = fs::remove_file(&backing_path);
            cleanup_created_dirs(&created_dirs);
            return Err(LifecycleError::StatePersist(error.to_string()));
        }
    };
    if stored != bytes {
        let _ = fs::remove_file(&backing_path);
        cleanup_created_dirs(&created_dirs);
        return Err(LifecycleError::RequirementUnmet(
            "Pi fork backing changed before registration".to_string(),
        ));
    }
    if let Err(error) = validate_session_bytes(&stored, session_id, cwd, Some(parent_session)) {
        let _ = fs::remove_file(&backing_path);
        cleanup_created_dirs(&created_dirs);
        return Err(error);
    }
    let identity = match file_identity(&backing_path) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = fs::remove_file(&backing_path);
            cleanup_created_dirs(&created_dirs);
            return Err(LifecycleError::StatePersist(error));
        }
    };
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok(StagedPiSession {
        seat_paths: paths.clone(),
        backing_path,
        bytes,
        identity,
        created_dirs,
        wrapper_identity: None,
        session_id: session_id.clone(),
        captured_at: timestamp.to_string(),
        sha256,
    })
}

fn create_target_session_dirs(
    workspace: &Path,
    paths: &crate::lifecycle::launch::pi_mcp::PiSeatPaths,
) -> Result<Vec<PathBuf>, LifecycleError> {
    let relative = paths
        .sessions
        .strip_prefix(workspace)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    let mut current = workspace.to_path_buf();
    let mut created = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            cleanup_created_dirs(&created);
            return Err(LifecycleError::StatePersist(
                "target Pi session root contains an invalid path component".to_string(),
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                cleanup_created_dirs(&created);
                return Err(LifecycleError::RequirementUnmet(format!(
                    "target Pi session path is not a real directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Err(error) = fs::create_dir(&current) {
                    cleanup_created_dirs(&created);
                    return Err(LifecycleError::StatePersist(error.to_string()));
                }
                created.push(current.clone());
                if let Err(error) = fs::set_permissions(&current, fs::Permissions::from_mode(0o700))
                {
                    cleanup_created_dirs(&created);
                    return Err(LifecycleError::StatePersist(error.to_string()));
                }
            }
            Err(error) => {
                cleanup_created_dirs(&created);
                return Err(LifecycleError::StatePersist(error.to_string()));
            }
        }
    }
    let canonical = match fs::canonicalize(&paths.sessions) {
        Ok(canonical) => canonical,
        Err(error) => {
            cleanup_created_dirs(&created);
            return Err(LifecycleError::StatePersist(error.to_string()));
        }
    };
    if canonical != paths.sessions {
        cleanup_created_dirs(&created);
        return Err(LifecycleError::RequirementUnmet(
            "target Pi session root resolves outside its managed path".to_string(),
        ));
    }
    Ok(created)
}

fn session_id_in_use(state: &JsonValue, session_id: &SessionId) -> bool {
    match state {
        JsonValue::Object(values) => values.iter().any(|(key, value)| {
            (matches!(key.as_str(), "session_id" | "_pending_session_id")
                && value.as_str() == Some(session_id.as_str()))
                || session_id_in_use(value, session_id)
        }),
        JsonValue::Array(values) => values
            .iter()
            .any(|value| session_id_in_use(value, session_id)),
        _ => false,
    }
}

fn required_state_string<'a>(state: &'a JsonValue, key: &str) -> Result<&'a str, LifecycleError> {
    state
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| LifecycleError::RequirementUnmet(format!("Pi source {key} is missing")))
}

fn file_identity(path: &Path) -> Result<FileIdentity, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    Ok(identity_from_metadata(&metadata))
}

fn identity_from_metadata(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn validate_path_component(value: &str, label: &str) -> Result<(), LifecycleError> {
    let mut components = Path::new(value).components();
    if value.is_empty()
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "{label} is not a safe path component"
        )));
    }
    Ok(())
}

fn ensure_no_symlink_components(path: &Path, allow_missing: bool) -> Result<(), LifecycleError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "path is not a normalized absolute path: {}",
            path.display()
        )));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LifecycleError::RequirementUnmet(format!(
                    "symlink path component is not allowed: {}",
                    current.display()
                )))
            }
            Ok(_) => {}
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(())
            }
            Err(error) => return Err(LifecycleError::RequirementUnmet(error.to_string())),
        }
    }
    Ok(())
}

fn refuse_existing_path(path: &Path, label: &str) -> Result<(), LifecycleError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(LifecycleError::RequirementUnmet(format!(
            "{label} already exists: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LifecycleError::StatePersist(error.to_string())),
    }
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, LifecycleError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LifecycleError::StatePersist(error.to_string())),
    }
}

fn spec_has_target(spec: &YamlValue, target: &str) -> bool {
    if !spec.is_map() {
        return true;
    }
    if spec
        .get("agents")
        .and_then(YamlValue::as_list)
        .is_some_and(|agents| {
            agents
                .iter()
                .any(|agent| yaml_agent_id(agent) == Some(target))
        })
    {
        return true;
    }
    spec.get("routing")
        .and_then(|routing| routing.get("rules"))
        .and_then(YamlValue::as_list)
        .is_some_and(|rules| rules.iter().any(|rule| yaml_route_mentions(rule, target)))
}

fn yaml_agent_id(agent: &YamlValue) -> Option<&str> {
    agent.get("id").and_then(YamlValue::as_str)
}

fn yaml_route_mentions(rule: &YamlValue, target: &str) -> bool {
    rule.get("assign_to").and_then(YamlValue::as_str) == Some(target)
        || rule
            .get("match")
            .and_then(|matcher| matcher.get("assignee"))
            .and_then(YamlValue::as_list)
            .is_some_and(|assignees| {
                assignees
                    .iter()
                    .any(|assignee| assignee.as_str() == Some(target))
            })
}

fn compensate_fork(
    workspace: &Path,
    team_key: &str,
    target: &AgentId,
    source: &AgentId,
    transport: &dyn Transport,
    session: &SessionName,
    panes_before: &BTreeSet<String>,
    spawn_attempted: bool,
    spawned: Option<&super::common::SpawnedAgentWindow>,
    spec_registration: Option<&SpecRegistration>,
    state_registered: bool,
    staged: Option<&StagedPiSession>,
    role: Option<&mut MaterializedRole>,
) -> String {
    if let Some(staged) = staged {
        if let Err(error) = verify_no_target_writer(
            transport,
            session,
            target,
            panes_before,
            spawn_attempted,
            spawned,
        ) {
            if let Some(role) = role {
                role.keep();
            }
            return error;
        }
        match read_optional_bytes(&staged.backing_path) {
            Ok(Some(bytes)) if bytes == staged.bytes => {}
            Ok(Some(_)) => {
                if let Some(role) = role {
                    role.keep();
                }
                return format!(
                    "target Pi session has post-snapshot records; preserved {}",
                    staged.backing_path.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(role) = role {
                    role.keep();
                }
                return format!("cannot verify target Pi backing before cleanup: {error}");
            }
        }
    } else if spawn_attempted {
        if let Err(error) = verify_no_target_writer(
            transport,
            session,
            target,
            panes_before,
            spawn_attempted,
            spawned,
        ) {
            if let Some(role) = role {
                role.keep();
            }
            return error;
        }
    }

    if state_registered {
        let Some((staged, role_ref)) = staged.zip(role.as_deref()) else {
            if let Some(role) = role {
                role.keep();
            }
            return "fork state identity is unavailable; resources preserved".to_string();
        };
        if let Err(error) = rollback_fork_state(
            workspace,
            team_key,
            target,
            source,
            &staged.session_id,
            &staged.backing_path,
            role_ref.path(),
        ) {
            if let Some(role) = role {
                role.keep();
            }
            return format!("state rollback failed: {error}");
        }
    }
    if let Some(registration) = spec_registration {
        if let Err(error) = rollback_fork_spec(registration, target.as_str()) {
            if let Some(role) = role {
                role.keep();
            }
            return format!("spec rollback failed: {error}");
        }
    }
    if let Some(staged) = staged {
        if let Err(error) = remove_staged_session(staged, spawn_attempted) {
            if let Some(role) = role {
                role.keep();
            }
            return error;
        }
    }
    String::new()
}

fn verify_no_target_writer(
    transport: &dyn Transport,
    session: &SessionName,
    target: &AgentId,
    panes_before: &BTreeSet<String>,
    spawn_attempted: bool,
    spawned: Option<&super::common::SpawnedAgentWindow>,
) -> Result<(), String> {
    let pane_id = if let Some(spawned) = spawned {
        spawned.spawn.pane_id.clone()
    } else if spawn_attempted {
        let targets = transport
            .list_targets()
            .map_err(|error| error.to_string())?;
        let new_panes: Vec<_> = targets
            .iter()
            .filter(|pane| !panes_before.contains(pane.pane_id.as_str()))
            .collect();
        match new_panes.as_slice() {
            [] => return Ok(()),
            [pane]
                if pane.session.as_str() == session.as_str()
                    && pane.window_name.as_ref().map(WindowName::as_str)
                        == Some(target.as_str()) =>
            {
                pane.pane_id.clone()
            }
            _ => return Err("new pane ownership is ambiguous; resources preserved".to_string()),
        }
    } else {
        return Ok(());
    };
    let targets = transport
        .list_targets()
        .map_err(|error| error.to_string())?;
    let exact = targets.iter().any(|pane| {
        pane.pane_id == pane_id
            && pane.session.as_str() == session.as_str()
            && pane.window_name.as_ref().map(WindowName::as_str) == Some(target.as_str())
    });
    if !exact {
        let still_present = targets.iter().any(|pane| pane.pane_id == pane_id);
        if !panes_before.contains(pane_id.as_str()) {
            transport
                .kill_pane(&pane_id)
                .map_err(|error| format!("could not stop newly spawned pane: {error}"))?;
            let remaining = transport
                .list_targets()
                .map_err(|error| error.to_string())?;
            if remaining.iter().any(|pane| pane.pane_id == pane_id) {
                return Err(
                    "newly spawned pane remains after kill; resources preserved".to_string()
                );
            }
            return Ok(());
        }
        if still_present {
            return Err(
                "spawned pane is not owned by the target window; resources preserved".to_string(),
            );
        }
        return Ok(());
    }
    transport
        .kill_pane(&pane_id)
        .map_err(|error| format!("could not stop exact target pane: {error}"))?;
    let remaining = transport
        .list_targets()
        .map_err(|error| error.to_string())?;
    if remaining.iter().any(|pane| pane.pane_id == pane_id) {
        return Err("exact target pane remains after kill; resources preserved".to_string());
    }
    Ok(())
}

fn fork_target_state_registered(
    workspace: &Path,
    team_key: &str,
    target: &AgentId,
    source: &AgentId,
    session_id: &SessionId,
    backing_path: &Path,
    role_path: &Path,
) -> bool {
    let Ok(state) = crate::state::projection::select_runtime_state(workspace, Some(team_key))
    else {
        return false;
    };
    let Some(agent) = state
        .get("agents")
        .and_then(|agents| agents.get(target.as_str()))
    else {
        return false;
    };
    let backing_text = backing_path.to_string_lossy().to_string();
    let role_text = role_path.to_string_lossy().to_string();
    agent.get("owner_team_id").and_then(JsonValue::as_str) == Some(team_key)
        && agent.get("forked_from").and_then(JsonValue::as_str) == Some(source.as_str())
        && agent.get("session_id").and_then(JsonValue::as_str) == Some(session_id.as_str())
        && agent.get("rollout_path").and_then(JsonValue::as_str) == Some(backing_text.as_str())
        && agent.get("dynamic_role_file").and_then(JsonValue::as_str) == Some(role_text.as_str())
        && agent.get("captured_via").and_then(JsonValue::as_str) == Some("fork_snapshot")
        && agent.get("capture_state").and_then(JsonValue::as_str) == Some("captured")
}

fn rollback_fork_state(
    workspace: &Path,
    team_key: &str,
    target: &AgentId,
    source: &AgentId,
    session_id: &SessionId,
    backing_path: &Path,
    role_path: &Path,
) -> Result<(), String> {
    let mut state = crate::state::projection::select_runtime_state(workspace, Some(team_key))
        .map_err(|error| error.to_string())?;
    let agent = state
        .get("agents")
        .and_then(|agents| agents.get(target.as_str()))
        .ok_or_else(|| "fork target row disappeared before rollback".to_string())?;
    let backing_text = backing_path.to_string_lossy().to_string();
    let role_text = role_path.to_string_lossy().to_string();
    if agent.get("owner_team_id").and_then(JsonValue::as_str) != Some(team_key)
        || agent.get("forked_from").and_then(JsonValue::as_str) != Some(source.as_str())
        || agent.get("session_id").and_then(JsonValue::as_str) != Some(session_id.as_str())
        || agent.get("rollout_path").and_then(JsonValue::as_str) != Some(backing_text.as_str())
        || agent.get("dynamic_role_file").and_then(JsonValue::as_str) != Some(role_text.as_str())
    {
        return Err("fork target row identity changed; left untouched".to_string());
    }
    state
        .get_mut("agents")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| "runtime agents map is missing".to_string())?
        .remove(target.as_str());
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::AgentRollback {
                team_key: Some(team_key),
                agent_id: target.as_str(),
            },
            &state,
        )
        .map_err(|error| error.to_string())
}

fn rollback_fork_spec(registration: &SpecRegistration, target: &str) -> Result<(), String> {
    ensure_no_symlink_components(&registration.path, true).map_err(|error| error.to_string())?;
    let current = read_optional_bytes(&registration.path).map_err(|error| error.to_string())?;
    if current.as_deref() == registration.previous_bytes.as_deref() {
        return Ok(());
    }
    if current.as_deref() == Some(registration.written_bytes.as_slice())
        && registration.identity.is_some()
        && file_identity(&registration.path).ok() == registration.identity
    {
        return match &registration.previous_bytes {
            Some(bytes) => write_bytes_atomic(&registration.path, bytes),
            None => fs::remove_file(&registration.path).map_err(|error| error.to_string()),
        };
    }
    let Some(bytes) = current else {
        return if registration.previous_bytes.is_none() {
            Ok(())
        } else {
            Err("runtime spec disappeared during rollback".to_string())
        };
    };
    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let mut spec = yaml::loads(text).map_err(|error| error.to_string())?;
    remove_target_from_spec(&mut spec, target, &registration.target_agent);
    if spec_has_target(&spec, target) {
        return Err("runtime spec target changed during operation; left untouched".to_string());
    }
    let bytes = yaml::dumps(&spec).into_bytes();
    write_bytes_atomic(&registration.path, &bytes)
}

fn remove_target_from_spec(spec: &mut YamlValue, target: &str, expected_agent: &YamlValue) {
    let YamlValue::Map(root) = spec else {
        return;
    };
    if let Some((_, YamlValue::List(agents))) = root.iter_mut().find(|(key, _)| key == "agents") {
        agents.retain(|agent| yaml_agent_id(agent) != Some(target) || agent != expected_agent);
    }
    if let Some((_, YamlValue::Map(fields))) = root.iter_mut().find(|(key, _)| key == "routing") {
        if let Some((_, YamlValue::List(rules))) = fields.iter_mut().find(|(key, _)| key == "rules")
        {
            let route_id = format!("route-{target}");
            let expected_route = YamlValue::Map(vec![
                ("id".to_string(), YamlValue::Str(route_id.clone())),
                (
                    "match".to_string(),
                    YamlValue::Map(vec![(
                        "assignee".to_string(),
                        YamlValue::List(vec![YamlValue::Str(target.to_string())]),
                    )]),
                ),
                ("assign_to".to_string(), YamlValue::Str(target.to_string())),
                ("priority".to_string(), YamlValue::Int(10)),
            ]);
            rules.retain(|rule| {
                !(rule.get("id").and_then(YamlValue::as_str) == Some(route_id.as_str())
                    && rule.get("assign_to").and_then(YamlValue::as_str) == Some(target)
                    && rule == &expected_route)
            });
        }
    }
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "runtime spec path has no parent".to_string())?;
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let temp = parent.join(format!(
        ".fork-spec-rollback-{}-{nonce}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| error.to_string())?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(())
}

fn capture_target_wrapper_identity(staged: Option<&mut StagedPiSession>) {
    if let Some(staged) = staged {
        staged.wrapper_identity = ensure_no_symlink_components(&staged.seat_paths.wrapper, false)
            .ok()
            .and_then(|_| file_identity(&staged.seat_paths.wrapper).ok());
    }
}

fn remove_staged_session(staged: &StagedPiSession, remove_wrapper: bool) -> Result<(), String> {
    ensure_no_symlink_components(&staged.backing_path, true).map_err(|error| error.to_string())?;
    match fs::symlink_metadata(&staged.backing_path) {
        Ok(_) => {
            if file_identity(&staged.backing_path).ok() != Some(staged.identity)
                || read_optional_bytes(&staged.backing_path)
                    .map_err(|error| error.to_string())?
                    .as_deref()
                    != Some(staged.bytes.as_slice())
            {
                return Err(format!(
                    "target Pi backing is no longer the staged file; preserved {}",
                    staged.backing_path.display()
                ));
            }
            fs::remove_file(&staged.backing_path).map_err(|error| error.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    if remove_wrapper {
        ensure_no_symlink_components(&staged.seat_paths.runtime_root, false)
            .map_err(|error| error.to_string())?;
        match fs::symlink_metadata(&staged.seat_paths.wrapper) {
            Ok(metadata)
                if metadata.file_type().is_file()
                    && staged.wrapper_identity == Some(identity_from_metadata(&metadata)) =>
            {
                fs::remove_file(&staged.seat_paths.wrapper).map_err(|error| error.to_string())?;
            }
            Ok(_) => return Err("target Pi wrapper is not a regular file; preserved".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    cleanup_created_dirs_checked(&staged.created_dirs)
}

fn cleanup_created_dirs(dirs: &[PathBuf]) {
    for dir in dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

fn cleanup_created_dirs_checked(dirs: &[PathBuf]) -> Result<(), String> {
    for dir in dirs.iter().rev() {
        match fs::remove_dir(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                return Err(format!(
                    "target Pi runtime directory is not empty; preserved {}",
                    dir.display()
                ))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn write_fork_audit(
    workspace: &Path,
    fields: &mut JsonValue,
    outcome: &str,
    failure_stage: &str,
    compensation: &str,
) -> Result<(), String> {
    fields["outcome"] = serde_json::json!(outcome);
    fields["failure_stage"] = if outcome == "succeeded" {
        JsonValue::Null
    } else {
        serde_json::json!(failure_stage)
    };
    fields["compensation"] = serde_json::json!(compensation);
    crate::event_log::EventLog::new(workspace)
        .write(crate::lifecycle::event_names::CONTEXT_FORK, fields.clone())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn state_has_session_name(state: &JsonValue) -> bool {
    state
        .get("session_name")
        .and_then(JsonValue::as_str)
        .is_some_and(|value| !value.is_empty())
        || state
            .get("leader_receiver")
            .and_then(|receiver| receiver.get("session_name"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| !value.is_empty())
}

fn session_name_for_audit(state: &JsonValue) -> SessionName {
    state_session_name(state)
}
