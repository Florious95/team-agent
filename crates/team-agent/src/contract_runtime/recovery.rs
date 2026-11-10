//! Explicit fresh recovery of a purged temporary native store. Never fabricates
//! a session/ownership receipt, adopts an old process, or deletes the old root.
use super::{
    backend::{self, BackendError},
    framework,
};
use crate::{
    cli::{CliError, CmdResult},
    state::selector::{resolve_active_team_readonly, SelectorMode},
    transport::{SessionName, Transport},
};
use serde_json::json;
use std::{io::Write, path::Path};
use team_agent_contract::{host::digest, host::digest_hex, orchestration::store::ProjectionStore};

fn error(value: impl std::fmt::Display) -> CliError {
    CliError::Runtime(value.to_string())
}

/// A contract seat may have only a native endpoint/pane receipt, not a legacy
/// pane PID. Unknown probe failures never count as proof that it is gone.
pub(super) fn carrier_absent(id: &str, row: &serde_json::Value, tmux: &Path) -> Result<(), CliError> {
    let mut proved = false;
    if let Some(value) = row.get("pane_pid").filter(|value| !value.is_null()) {
        let pid = value
            .as_u64()
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(|| error(format!("invalid saved carrier PID for {id}")))?;
        if unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(error(format!(
                "native recovery refused: old carrier for {id} is live or unobservable"
            )));
        }
        proved = true;
    }
    if row
        .get("native_endpoint")
        .is_some_and(|value| !value.is_null())
        || row.get("native_pane").is_some_and(|value| !value.is_null())
    {
        let endpoint = row
            .get("native_endpoint")
            .and_then(serde_json::Value::as_str)
            .filter(|value| Path::new(value).is_absolute())
            .ok_or_else(|| error(format!("invalid saved native endpoint for {id}")))?;
        let pane = row
            .get("native_pane")
            .and_then(serde_json::Value::as_str)
            .filter(|value| {
                value
                    .strip_prefix('%')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .ok_or_else(|| error(format!("invalid saved native pane for {id}")))?;
        use team_agent_contract::host::{
            command::{OutputLimits, RealCommandRunner},
            tmux::shared_server_absent,
        };
        let absent = shared_server_absent(
            tmux,
            Path::new(endpoint),
            &mut RealCommandRunner::default(),
            std::time::Duration::from_millis(500),
            OutputLimits {
                stdout: 65536,
                stderr: 4096,
                stdin: 1,
            },
        )
        .map_err(error)?;
        if !absent {
            let targets = crate::transport_factory::bounded_endpoint_transport(
                endpoint,
                std::time::Instant::now() + std::time::Duration::from_millis(500),
            )
            .list_targets()
            .map_err(error)?;
            if targets.iter().any(|target| target.pane_id.as_str() == pane) {
                return Err(error(format!(
                    "native recovery refused: saved native pane for {id} still exists"
                )));
            }
        }
        proved = true;
    }
    if !proved {
        return Err(error(format!(
            "native recovery cannot prove old carrier death for {id}"
        )));
    }
    Ok(())
}

/// Preserve the provider route/model/prompt captured for the exact old instance.
/// Editable Markdown aliases may have acquired a different meaning since launch.
fn frozen_team(
    binding: &backend::Binding,
    selected: &crate::state::selector::SelectedTeam,
) -> Result<super::config::TeamConfig, CliError> {
    use super::config::{MemberRoute, RuntimeFamily, TeamConfig};
    use team_agent_contract::contract::types::{Generation, InstanceId, InstanceIdentity, SeatId};
    let agents = selected
        .state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| error("native recovery requires the saved roster"))?;
    let paused = crate::lifecycle::role_config::paused_agents(
        &selected.run_workspace, &selected.team_dir, &selected.state,
    ).map_err(error)?;
    let mut roles = Vec::new();
    let mut members = Vec::new();
    for (id, row) in agents {
        let seat = SeatId::new(id.as_str()).map_err(error)?;
        let native = framework::is_contract(row);
        let provider = row
            .get("contract_provider")
            .and_then(serde_json::Value::as_str)
            .or_else(|| row.get("provider").and_then(serde_json::Value::as_str))
            .ok_or_else(|| error("saved provider route missing"))?;
        let mut source = selected.team_dir.join("agents").join(format!("{id}.md"));
        if native {
            if row
                .get("contract_scope")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|scope| scope != binding.scope.as_str())
            {
                return Err(error("saved native role scope disagrees with its binding"));
            }
            if let Some(instance) = row
                .get("contract_instance")
                .and_then(serde_json::Value::as_str)
            {
                let generation = row
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|n| *n > 0)
                    .ok_or_else(|| error(format!("saved native generation missing for {id}")))?;
                let identity = InstanceIdentity {
                    scope: binding.scope.clone(),
                    seat,
                    instance: InstanceId::new(instance).map_err(error)?,
                    generation: Generation(generation),
                };
                let mut role = backend::read_role(binding, &identity).map_err(error)?;
                if role.provider.as_str() != provider {
                    return Err(error(
                        "frozen role provider disagrees with saved native route",
                    ));
                }
                role.paused |= paused.contains(id);
                source = role.source.clone();
                roles.push(role);
            } else if !paused.contains(id)
                && !crate::state::projection::agent_explicit_stop(row)
            {
                return Err(error(format!(
                    "no frozen native role identity for {id}; no editable role was substituted"
                )));
            }
        }
        members.push(MemberRoute {
            id: id.clone(),
            provider: provider.into(),
            source,
            runtime: if native {
                RuntimeFamily::Contract
            } else {
                RuntimeFamily::Legacy
            },
        });
    }
    Ok(TeamConfig {
        selector: selected.team_key.clone(),
        label: selected.team_key.clone(),
        scope: binding.scope.clone(),
        team_dir: selected.team_dir.clone(),
        workspace: binding.workspace.clone(),
        objective: selected
            .state
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .into(),
        members,
        roles,
    })
}

/// None means ordinary lifecycle planning still owns this request. Both the
/// dry run and admission use the same whole-scope dead-carrier checks.
pub(crate) fn restart(
    workspace: &Path,
    team: Option<&str>,
    allow_fresh: bool,
    dry_run: bool,
    as_json: bool,
) -> Result<Option<CmdResult>, CliError> {
    let selected = match resolve_active_team_readonly(workspace, team, SelectorMode::RuntimeOnly) {
        Ok(selected) => selected,
        Err(_) => return Ok(None), // Preserve the normal selector diagnostics.
    };
    let workspace = selected.run_workspace.as_path();
    let team = selected.team_key.as_str();
    match backend::authority_check(workspace, team) {
        Err(BackendError::AuthorityLost { .. }) => {}
        _ => return Ok(None),
    }
    let mut binding = backend::read_binding(workspace, team).map_err(error)?;
    let old_root = binding.transport_root.clone();
    let temp = Path::new("/tmp").canonicalize()?;
    if old_root.parent().is_none_or(|parent| parent != temp && parent != Path::new("/tmp") && parent != Path::new("/private/tmp"))
        || !old_root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("ta-"))
    {
        return Err(error("native_authority_lost: non-temporary authority is missing; restore it from backup; no replacement was created"));
    }
    if !allow_fresh {
        return Err(error(format!(
            "native_authority_lost: {} was purged; native session/ownership receipts are unavailable. To explicitly start fresh from the saved roles, run team-agent team restart --allow-fresh --workspace {} --team {}; old session files and the original binding will be retained",
            old_root.display(), crate::cli::shell_quote(&workspace.to_string_lossy()), crate::cli::shell_quote(team))));
    }
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&selected.state, None)
        .map_err(error)?;
    let agents = selected
        .state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| error("native recovery requires the saved roster"))?;
    // Refuse PID reuse too. No signal is sent and no stale PID is trusted as a
    // cleanup target. A live/unobservable old carrier forbids replacement.
    let dead = || -> Result<(), CliError> {
        for (id, row) in agents.iter().filter(|(_, row)| framework::is_contract(row)) {
            if row
                .get("contract_instance")
                .and_then(serde_json::Value::as_str)
                .is_none()
                && row
                    .get("native_endpoint")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
                && row
                    .get("native_pane")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
            {
                continue;
            }
            carrier_absent(id, row, &binding.tmux)?;
        }
        let transport =
            crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
                workspace,
                &selected.state,
            )
            .map_err(error)?
            .backend;
        let session = selected
            .state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .filter(|session| !session.is_empty())
            .ok_or_else(|| error("native recovery requires the saved tmux session"))?;
        if transport
            .has_session(&SessionName::new(session))
            .map_err(error)?
        {
            return Err(error("native recovery refused: the old worker tmux session still exists; no session was changed"));
        }
        Ok(())
    };
    dead()?;
    let config = frozen_team(&binding, &selected)?;
    let eligible: Vec<_> = config
        .roles
        .iter()
        .filter(|role| {
            !role.paused
                && agents
                    .get(role.id.as_str())
                    .is_some_and(|row| !crate::state::projection::agent_explicit_stop(row))
        })
        .collect();
    if !eligible.is_empty() {
        let mut admission = config.clone();
        admission.roles = eligible.iter().map(|role| (**role).clone()).collect();
        backend::preflight(&admission).map_err(error)?;
    }
    let root = backend::root_for(workspace, &binding.scope);
    if dry_run {
        return Ok(Some(CmdResult::from_json(
            json!({"ok":true,"status":"dry_run",
            "action":"recover_native_authority","start_mode":"fresh","session_preserved":false,
            "old_root":old_root,"durable_parent":root,"agents":eligible.iter().map(|role| role.id.as_str()).collect::<Vec<_>>(),
            "effects":{"native_processes":"not_attempted","authority":"not_written"}}),
            as_json,
        )));
    }
    let skipped: Vec<_> = agents.iter().filter(|(id, row)| framework::is_contract(row)
        && !eligible.iter().any(|role| role.id.as_str() == id.as_str())).map(|(id, _)| id.as_str()).collect();
    let mut skipped_locks = Vec::new();
    for id in &skipped {
        let seat = team_agent_contract::contract::types::SeatId::new(*id).map_err(error)?;
        skipped_locks.push(backend::native_seat_lock(workspace, team, &seat, true).map_err(error)?);
    }
    crate::coordinator::retire_legacy_coordinator(workspace).map_err(error)?;
    // Same cutover lock as migrate. Archive evidence before publishing the new
    // binding; a failed spawn retains its normal native operation receipts.
    let directory = std::fs::File::open(&root)?;
    directory.lock()?;
    if backend::read_binding(workspace, team)
        .map_err(error)?
        .transport_root
        != old_root
        || !matches!(
            backend::authority_check(workspace, team),
            Err(BackendError::AuthorityLost { .. })
        )
    {
        return Err(error(
            "native recovery scope changed since admission; retry the read-only plan",
        ));
    }
    dead()?;
    let latest = resolve_active_team_readonly(workspace, Some(team), SelectorMode::RuntimeOnly)
        .map_err(error)?;
    let roles = frozen_team(&binding, &latest)?;
    if latest.state.get("agents") != selected.state.get("agents")
        || latest.state.get("team_owner") != selected.state.get("team_owner")
        || serde_json::to_vec(&roles).map_err(error)?
            != serde_json::to_vec(&config).map_err(error)?
    {
        return Err(error(
            "native recovery roster or role sources changed since admission",
        ));
    }
    let mut nonce = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let nonce = digest_hex(digest(&nonce));
    let archive = root.join(format!("recovery-{nonce}.json"));
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&archive)?;
    serde_json::to_writer(
        &mut file,
        &json!({"binding":binding,"state":selected.state,
        "reason":"temporary_authority_lost","session_preserved":false}),
    )
    .map_err(error)?;
    file.sync_all()?;
    // The archive durably retains lost identities. Detach skipped rows before
    // publishing the fresh store: a crash here remains fail-closed on the old
    // missing authority, rather than stranding stale identities in a new store.
    let mut detached = latest.state.clone();
    for id in &skipped {
        let row = detached["agents"][*id].as_object_mut().ok_or_else(|| error("saved native row missing"))?;
        for field in ["contract_scope", "contract_instance", "generation", "native_pane", "native_endpoint", "native_pid",
            "session_id", "pane_id", "pane_pid", "tmux_endpoint", "window", "session_name"] {
            row.insert(field.into(), serde_json::Value::Null);
        }
        row.insert("native_recovery_archive".into(), json!(archive));
        row.insert("session_observation".into(), json!("lost"));
        row.insert("mcp_binding".into(), json!("unverified"));
        row.insert("mcp_tools".into(), json!({}));
        crate::station::projection::publish(workspace, &mut detached, team, &[*id]).map_err(error)?;
        crate::state::repository::StateRepository::new(workspace).save(
            crate::state::repository::StateWriteIntent::ResetAgent { team_key: team, agent_id: id, discard_session: true },
            &detached,
        ).map_err(error)?;
    }
    binding.candidate = std::env::current_exe()?.canonicalize()?;
    binding.candidate_sha256 = team_agent_contract::host::process::fingerprint_file(
        &binding.candidate,
        1024 * 1024 * 1024,
        std::time::Duration::from_secs(10),
    )
    .map_err(error)?;
    binding.transport_root = root.join(format!("authority-{nonce}"));
    ProjectionStore::create(
        &binding.transport_root,
        binding.scope.clone(),
        &binding.endpoint,
    )
    .map_err(error)?;
    let staged = root.join(format!("binding-{nonce}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)?;
    file.write_all(&serde_json::to_vec(&binding).map_err(error)?)?;
    file.sync_all()?;
    std::fs::rename(&staged, root.join("framework-binding.json"))?;
    directory.sync_all()?;
    backend::retire_inactive_siblings(&binding, &selected.state).map_err(error)?;
    drop(directory);
    let mut outcomes = Vec::new();
    for role in eligible {
        let result = super::commands::with_agent(
            workspace,
            Some(team),
            role.id.as_str(),
            "recover-native",
            |latest, _| {
                let row = &latest.state["agents"][role.id.as_str()];
                let admitted = &agents[role.id.as_str()];
                if [
                    "provider",
                    "contract_provider",
                    "contract_instance",
                    "generation",
                    "paused",
                    "explicit_stop",
                ]
                .iter()
                .any(|key| row.get(*key) != admitted.get(*key))
                {
                    return Err(error("native recovery seat changed after admission"));
                }
                framework::start_role_config(workspace, team, role, &config.members)
            },
        )
        .and_then(|receipt| receipt.ok_or_else(|| error("native recovery seat route changed")));
        outcomes.push(json!({"agent":role.id.as_str(),"ok":result.is_ok(),
            "error":result.err().map(|e| e.to_string())}));
        if outcomes.last().is_some_and(|row| row["ok"] == false) {
            break;
        }
    }
    let ok = outcomes.iter().all(|row| row["ok"] == true);
    Ok(Some(CmdResult::from_json(
        json!({"ok":ok,
        "status":if ok {"native_authority_recovered"} else {"partial"},
        "start_mode":"fresh","session_preserved":false,"archive":archive,
        "root":binding.transport_root,"agents":outcomes,"legacy_workers":"unchanged"}),
        as_json,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_recovery_does_not_reinterpret_editable_provider_aliases() {
        use super::super::{config::RoleConfig, prompt::Prompt};
        use team_agent_contract::contract::types::{
            Generation, InstanceId, InstanceIdentity, ProviderId, SeatId,
        };
        let root = std::env::temp_dir().join(format!(
            "ta-frozen-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let cwd = team_agent_contract::host::process::resolve_cwd(&root).unwrap();
        let scope = backend::scope_for(&cwd, "saved-team").unwrap();
        let binding = backend::Binding {
            version: 1,
            workspace: cwd.clone(),
            team: "saved-team".into(),
            scope: scope.clone(),
            endpoint: format!("framework:{}", scope.as_str()),
            candidate: "/fixture/team-agent".into(),
            candidate_sha256: team_agent_contract::contract::types::Digest([0; 32]),
            tmux: "/fixture/tmux".into(),
            transport_root: "/tmp/ta-purged-fixture".into(),
        };
        let source_dir = cwd.path.join("roles");
        std::fs::create_dir_all(source_dir.join("agents")).unwrap();
        let source = source_dir.join("agents/worker.md");
        let editable = "---\nprovider: pi\n---\nThis is not the frozen prompt.\n";
        std::fs::write(&source, editable).unwrap();
        let mut selected = crate::state::selector::SelectedTeam {
            run_workspace: cwd.path.clone(),
            team_key: binding.team.clone(),
            team_dir: source_dir,
            spec_workspace: None,
            spec_path: None,
            state: json!({"agents":{"worker":{"provider":"pi_tui","runtime_family":"contract",
                "contract_instance":"instance-original","generation":1}}}),
        };
        let identity = InstanceIdentity {
            scope,
            seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance-original").unwrap(),
            generation: Generation(1),
        };
        let role = RoleConfig {
            id: identity.seat.clone(),
            provider: ProviderId::new("pi_tui").unwrap(),
            source: source.clone(),
            role: "worker".into(),
            model: "frozen-model".into(),
            effort: None,
            bypass: false,
            paused: false,
            body: "frozen prompt".into(),
            prompt: Prompt {
                text: "frozen prompt".into(),
                sha256: digest(b"frozen prompt"),
                sources: vec![],
            },
        };
        std::fs::create_dir_all(backend::root_for(&cwd.path, &binding.scope)).unwrap();
        backend::write_role(&binding, &identity, &role).unwrap();
        let config = frozen_team(&binding, &selected).unwrap();
        assert_eq!(config.roles[0].provider.as_str(), "pi_tui");
        assert_eq!(config.roles[0].model, "frozen-model");
        assert_eq!(config.roles[0].prompt.text, "frozen prompt");
        assert_eq!(config.members[0].provider, "pi_tui");
        assert_eq!(std::fs::read_to_string(&source).unwrap(), editable);
        std::fs::write(&source, editable.replacen("---\n", "---\npaused: true\n", 1)).unwrap();
        let paused = frozen_team(&binding, &selected).unwrap();
        assert!(paused.roles[0].paused);
        assert_eq!(paused.roles[0].model, "frozen-model");
        assert_eq!(paused.roles[0].prompt.text, "frozen prompt");
        std::fs::write(&source, editable).unwrap();
        selected.state["agents"]["worker"]["provider"] = json!("claude_tui");
        assert!(frozen_team(&binding, &selected).is_err());
        selected.state["agents"]["worker"]["provider"] = json!("pi_tui");
        selected.state["agents"]["worker"]["contract_instance"] = json!("instance-other");
        assert!(
            frozen_team(&binding, &selected).is_err(),
            "must not scan for a newer/other snapshot"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_receipt_without_legacy_pid_can_prove_absence() {
        let endpoint = Path::new("/tmp").join(format!(
            "ta-recovery-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!endpoint.exists());
        let missing_tmux = Path::new("/fixture/not-an-executable");
        let row = json!({"native_endpoint":endpoint,"native_pane":"%8"});
        assert!(carrier_absent("seat", &row, missing_tmux).is_ok());
        assert!(
            !endpoint.exists(),
            "absence observation must not start a server"
        );
        assert!(carrier_absent("seat", &json!({}), missing_tmux).is_err());
        assert!(carrier_absent(
            "seat",
            &json!({"pane_pid":std::process::id()}),
            missing_tmux
        )
        .is_err());
        assert!(carrier_absent(
            "seat",
            &json!({"native_endpoint":endpoint,"native_pane":"not-a-pane"}),
            missing_tmux
        )
        .is_err());
        let socket = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        assert!(
            carrier_absent("seat", &row, missing_tmux).is_err(),
            "unknown probe failure is not death proof"
        );
        drop(socket);
        std::fs::remove_file(endpoint).unwrap();
    }
}
