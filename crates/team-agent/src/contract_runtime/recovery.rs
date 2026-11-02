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
    if old_root.parent() != Some(temp.as_path())
        || !old_root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("ta-"))
    {
        return Err(error("native_authority_lost: non-temporary authority is missing; restore it from backup; no replacement was created"));
    }
    if !allow_fresh {
        return Err(error(format!(
            "native_authority_lost: {} was purged; native session/ownership receipts are unavailable. To explicitly start fresh from the saved roles, run team-agent restart --allow-fresh --workspace {} --team {}; old session files and the original binding will be retained",
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
            {
                continue;
            }
            let pid = row
                .get("pane_pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| i32::try_from(pid).ok())
                .filter(|pid| *pid > 0)
                .ok_or_else(|| {
                    error(format!(
                        "native recovery cannot prove old carrier death for {id}"
                    ))
                })?;
            if unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                return Err(error(format!(
                    "native recovery refused: old carrier for {id} is live or unobservable"
                )));
            }
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
    let config = framework::team_config(workspace, &selected.team_dir, team, None)
        .map_err(error)?
        .ok_or_else(|| error("native recovery requires the saved role sources"))?;
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
    if eligible.is_empty() {
        return Err(error("native recovery has no eligible nonpaused roles"));
    }
    for role in &eligible {
        if agents[role.id.as_str()]
            .get("provider")
            .and_then(serde_json::Value::as_str)
            != Some(role.provider.as_str())
        {
            return Err(error(
                "native recovery refuses a provider change in saved roles",
            ));
        }
    }
    let mut admission = config.clone();
    admission.roles = eligible.iter().map(|role| (**role).clone()).collect();
    backend::preflight(&admission).map_err(error)?;
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
    let roles = framework::team_config(workspace, &selected.team_dir, team, None).map_err(error)?;
    if latest.state.get("agents") != selected.state.get("agents")
        || latest.state.get("team_owner") != selected.state.get("team_owner")
        || serde_json::to_vec(&roles).map_err(error)?
            != serde_json::to_vec(&Some(&config)).map_err(error)?
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
    let mut outcomes = Vec::new();
    for role in eligible {
        let result = framework::start_role(workspace, &selected.team_dir, team, role.id.as_str());
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
