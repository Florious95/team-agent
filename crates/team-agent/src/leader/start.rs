//! leader::start — leader_start_plan / start_leader / leader_session_name(派生 tmux session 名)。

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::provider::{get_adapter, Provider};
use crate::tmux_backend::TmuxBackend;
use crate::transport::{
    PaneId, PaneLiveness, SessionName, SpawnResult, Target, Transport, WindowName,
};

use super::helpers::{
    provider_wire, resolve_workspace_for_hash, sanitize_session_folder, sha1_hex_prefix,
};
use super::owner_bind::leader_identity_context;
use super::{
    LeaderError, LeaderIdentity, LeaderLaunchOutcome, LeaderLaunchSocket, LeaderLaunchStatus,
    LeaderStartMode, LeaderStartPlan,
};

// ── leader::start — leader_start_plan / start_leader / session 名 ──

/// `leader_start_plan`(card §46;`__init__.py:82`)。计算 leader 启动计划
/// (exec in-TMUX / new tmux session / attach existing)。provider 未安装 → `Err(Start)`。
pub fn leader_start_plan(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
) -> Result<LeaderStartPlan, LeaderError> {
    if attach_session.is_some() && !confirm_attach {
        return Err(LeaderError::Start(
            "--attach-session requires --confirm".to_string(),
        ));
    }
    if attach_existing && !confirm_attach {
        return Err(LeaderError::Start(
            "attach existing leader session requires confirm".to_string(),
        ));
    }
    let adapter = get_adapter(provider);
    if !adapter.is_installed() {
        let command_name = provider_command_name(provider);
        return Err(LeaderError::Start(format!(
            "Provider {} command '{}' not found",
            provider_wire(provider),
            command_name
        )));
    }
    let state = crate::state::persist::load_runtime_state(workspace).ok();
    let identity = leader_identity_context(workspace, None, state.as_ref())?;
    let external_path = external_leader || attach_existing || attach_session.is_some();
    // 0.3.28 Step 2: managed mode now uses the SAME dedicated leader session
    // as the external path (`team-agent-leader-<provider>-<folder>-<sha1[:8]>`)
    // — Python parity. Pre-0.3.28 the managed branch used
    // `managed_team_session_name(identity) = team-<team_id>` which is the
    // worker session — that co-location is the structural root of
    // E49/E51/E53/E57-3/E60.
    let session_name = if external_path {
        attach_session
            .cloned()
            .or_else(|| Some(leader_session_name(provider, workspace)))
    } else {
        Some(leader_session_name(provider, workspace))
    };
    let in_tmux = std::env::var_os("TMUX").is_some();
    if !in_tmux {
        ensure_tmux_installed()?;
    }
    let existing_session = if external_path && !in_tmux && !attach_existing && attach_session.is_none() {
        match session_name.as_ref() {
            Some(session) => tmux_session_exists(workspace, session)?,
            None => false,
        }
    } else {
        false
    };
    let mode = if !external_path && in_tmux {
        LeaderStartMode::ExecProvider
    } else if !external_path {
        LeaderStartMode::ManagedTmuxClient
    } else if in_tmux {
        LeaderStartMode::ExecProvider
    } else if attach_existing || attach_session.is_some() || existing_session {
        LeaderStartMode::AttachExisting
    } else {
        LeaderStartMode::NewTmuxSession
    };
    let leader_env = leader_env_for_identity(provider, &identity);
    let argv = start_argv(
        mode,
        provider,
        provider_args,
        workspace,
        session_name.as_ref(),
        &leader_env,
    )?;
    let plan_session_name = if mode == LeaderStartMode::ExecProvider && !external_path {
        None
    } else {
        session_name
    };
    let plan_env = if mode == LeaderStartMode::ExecProvider {
        merged_exec_env(&leader_env)
    } else {
        leader_env.clone()
    };
    let provider_argv = provider_command_argv(provider, provider_args);
    Ok(LeaderStartPlan {
        mode,
        provider,
        workspace: resolve_workspace_for_hash(workspace),
        socket: LeaderLaunchSocket::Workspace,
        session_name: plan_session_name,
        argv,
        provider_argv,
        // 0.3.28 Step 2: leader window inside the dedicated leader session is
        // named after the provider wire (e.g. `claude`, `codex`, `copilot`),
        // never the literal string `leader`. Python parity (see
        // `leader/__init__.py:114-131`). This eliminates the `WorkerWindowNamedLeader`
        // topology violation surface — the worker session never has a window
        // named `leader` either, because the leader session is disjoint.
        leader_window: (mode == LeaderStartMode::ManagedTmuxClient)
            .then(|| WindowName::new(provider_wire(provider))),
        is_external_leader: external_path,
        leader_env: plan_env,
        identity: Some(identity),
        detached: false,
    })
}

pub(crate) fn leader_env_for_identity(
    provider: Provider,
    identity: &LeaderIdentity,
) -> BTreeMap<String, String> {
    let mut leader_env = BTreeMap::new();
    leader_env.insert(
        "TEAM_AGENT_LEADER_PROVIDER".to_string(),
        provider_wire(provider).to_string(),
    );
    leader_env.insert(
        "TEAM_AGENT_LEADER_SESSION_UUID".to_string(),
        identity.leader_session_uuid.as_str().to_string(),
    );
    leader_env.insert(
        "TEAM_AGENT_MACHINE_FINGERPRINT".to_string(),
        identity.machine_fingerprint.clone(),
    );
    leader_env.insert(
        "TEAM_AGENT_WORKSPACE".to_string(),
        identity.workspace_abspath.to_string_lossy().into_owned(),
    );
    leader_env.insert(
        "TEAM_AGENT_TEAM_ID".to_string(),
        identity.team_id.as_str().to_string(),
    );
    if provider == Provider::Copilot {
        leader_env.insert(
            "COPILOT_DISABLE_TERMINAL_TITLE".to_string(),
            "1".to_string(),
        );
    }
    leader_env
}

/// `start_leader`(card §46;`__init__.py:60`)。计算并执行 leader 启动计划(spawn + 信号处理)。
/// 进程退出后 `Err`/退出码经 caller 处理(此处返 `Result` 替代 Python 的 `SystemExit`)。
pub fn start_leader(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
) -> Result<(), LeaderError> {
    let plan = leader_start_plan(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
        external_leader,
    )?;
    crate::event_log::EventLog::new(workspace).write(
        super::LeaderEvent::LeaderStart.name(),
        serde_json::json!({
            "provider": super::helpers::provider_wire(plan.provider),
            "mode": serde_json::to_value(plan.mode)?,
            "session_name": plan.session_name.as_ref().map(|s| s.as_str().to_string()),
        }),
    )?;
    execute_leader_plan(&plan, workspace).map(|_| ())
}

/// Execute a precomputed leader launch plan.
///
/// S0 exposes the seam and return model only. Lane 2 owns the real provider/tmux
/// execution and workspace-socket enforcement.
pub fn execute_leader_plan(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    if plan.mode == LeaderStartMode::ManagedTmuxClient {
        return execute_managed_leader_plan(plan, workspace);
    }
    let mut argv = plan.argv.clone();
    let detached = plan.mode == LeaderStartMode::NewTmuxSession
        && !std::io::stdin().is_terminal()
        && insert_detach_flag(&mut argv);
    if plan.mode == LeaderStartMode::ExecProvider && !plan.is_external_leader {
        persist_exec_provider_leader_binding(plan, workspace)?;
    } else if plan.is_external_leader {
        persist_external_leader_topology_marker(plan, workspace)?;
    }
    let status = run_leader_argv(&argv, &plan.leader_env, plan, workspace)?;
    let code = status.code();
    if !status.success() {
        return Err(LeaderError::Start(format!(
            "leader launcher exited with status {}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )));
    }
    if detached {
        Ok(LeaderLaunchOutcome {
            status: LeaderLaunchStatus::Detached,
            exit_code: code,
            session_name: plan.session_name.clone(),
            reason: None,
        })
    } else {
        let _ = workspace;
        Ok(LeaderLaunchOutcome {
            status: LeaderLaunchStatus::Exited,
            exit_code: code,
            session_name: plan.session_name.clone(),
            reason: None,
        })
    }
}

/// B5: the deterministic leader-session naming prefix IS the ownership truth source —
/// shutdown's socket teardown spares sessions carrying it (no separate registry).
pub const LEADER_SESSION_PREFIX: &str = "team-agent-leader-";

/// `leader_session_name`(card §48;`__init__.py:186`)。确定派生 tmux session 名
/// `team-agent-leader-<provider>-<folder>-<sha1[:8]>`(workspace.resolve() 的 sha1 前 8 hex)。
pub fn leader_session_name(provider: Provider, workspace: &Path) -> SessionName {
    let resolved = resolve_workspace_for_hash(workspace);
    let folder_raw = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let folder = sanitize_session_folder(folder_raw);
    let hash = sha1_hex_prefix(resolved.to_string_lossy().as_bytes(), 8);
    SessionName::new(format!(
        "{LEADER_SESSION_PREFIX}{}-{folder}-{hash}",
        provider_wire(provider)
    ))
}

fn start_argv(
    mode: LeaderStartMode,
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    session_name: Option<&SessionName>,
    leader_env: &BTreeMap<String, String>,
) -> Result<Vec<String>, LeaderError> {
    let provider_cmd = provider_command_name(provider).to_string();
    match mode {
        LeaderStartMode::ExecProvider => {
            let mut argv = vec![provider_cmd];
            argv.extend(normalized_provider_args(provider_args));
            Ok(argv)
        }
        LeaderStartMode::ManagedTmuxClient => {
            let Some(session) = session_name else {
                return Err(LeaderError::Start("managed leader session missing".to_string()));
            };
            managed_client_argv(workspace, session, provider)
        }
        LeaderStartMode::AttachExisting => {
            let Some(session) = session_name else {
                return Err(LeaderError::Start("attach session missing".to_string()));
            };
            let argv = vec![
                "tmux".to_string(),
                "attach-session".to_string(),
                "-t".to_string(),
                session.as_str().to_string(),
            ];
            Ok(TmuxBackend::argv_for_workspace(workspace, &argv))
        }
        LeaderStartMode::NewTmuxSession => {
            let Some(session) = session_name else {
                return Err(LeaderError::Start("leader session missing".to_string()));
            };
            let resolved_workspace = resolve_workspace_for_hash(workspace);
            let mut exports = leader_export_assignments(leader_env);
            if let Some(path) = std::env::var_os("PATH").and_then(|p| p.into_string().ok()) {
                exports.push(shlex_quote(&format!("PATH={path}")));
            }
            let mut provider_argv = vec![provider_cmd];
            provider_argv.extend(normalized_provider_args(provider_args));
            let shell = format!(
                "cd {} && export {} && exec {}",
                shlex_quote(&resolved_workspace.to_string_lossy()),
                exports.join(" "),
                shell_join(&provider_argv)
            );
            let argv = vec![
                "tmux".to_string(),
                "new-session".to_string(),
                "-s".to_string(),
                session.as_str().to_string(),
                "-n".to_string(),
                provider_wire(provider).to_string(),
                "-c".to_string(),
                resolved_workspace.to_string_lossy().into_owned(),
                "sh".to_string(),
                "-lc".to_string(),
                shell,
            ];
            Ok(TmuxBackend::argv_for_workspace(workspace, &argv))
        }
    }
}

fn provider_command_argv(provider: Provider, provider_args: &[String]) -> Vec<String> {
    let mut argv = vec![provider_command_name(provider).to_string()];
    argv.extend(normalized_provider_args(provider_args));
    argv
}

fn normalized_provider_args(provider_args: &[String]) -> impl Iterator<Item = String> + '_ {
    provider_args
        .iter()
        .skip(usize::from(provider_args.first().is_some_and(|arg| arg == "--")))
        .cloned()
}

// 0.3.28 Step 2: `managed_team_session_name` deleted. Both managed and
// external paths now compute the dedicated leader session via
// `leader_session_name(provider, workspace)` directly. The old function
// returned `team-<team_id>` which is the WORKER session — the structural
// root of E49/E51/E53/E57-3/E60.

fn managed_client_argv(
    workspace: &Path,
    session: &SessionName,
    provider: Provider,
) -> Result<Vec<String>, LeaderError> {
    // 0.3.28 Step 2: leader window inside the dedicated leader session is
    // named after `provider_wire(provider)` (e.g. `claude`, `codex`, `fake`),
    // never the literal `leader`. Pre-0.3.28 this hardcoded `:leader`.
    let target = format!("{}:{}", session.as_str(), provider_wire(provider));
    let argv = if std::env::var_os("TMUX").is_some() {
        vec![
            "tmux".to_string(),
            "switch-client".to_string(),
            "-t".to_string(),
            target,
        ]
    } else {
        vec![
            "tmux".to_string(),
            "attach-session".to_string(),
            "-t".to_string(),
            target,
        ]
    };
    Ok(TmuxBackend::argv_for_workspace(workspace, &argv))
}

fn execute_managed_leader_plan(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    let Some(session) = plan.session_name.as_ref() else {
        return Err(LeaderError::Start("managed leader session missing".to_string()));
    };
    let Some(window) = plan.leader_window.as_ref() else {
        return Err(LeaderError::Start("managed leader window missing".to_string()));
    };
    let transport = TmuxBackend::for_workspace(workspace);
    let spawned = ensure_managed_leader_pane(&transport, session, window, plan, workspace)?;
    persist_managed_leader_binding(plan, workspace, &spawned)?;
    spawn_managed_provider_startup_prompt_handler(
        plan.provider,
        workspace.to_path_buf(),
        spawned.pane_id.as_str().to_string(),
    );
    let status = run_leader_argv(&plan.argv, &BTreeMap::new(), plan, workspace)?;
    let code = status.code();
    if !status.success() {
        return Err(LeaderError::Start(format!(
            "leader launcher exited with status {}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )));
    }
    ensure_managed_provider_live_after_attach(&transport, &spawned)?;
    Ok(LeaderLaunchOutcome {
        status: LeaderLaunchStatus::Exited,
        exit_code: code,
        session_name: plan.session_name.clone(),
        reason: None,
    })
}

fn ensure_managed_leader_pane(
    transport: &dyn Transport,
    session: &SessionName,
    window: &WindowName,
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<SpawnResult, LeaderError> {
    if transport.has_session(session).unwrap_or(false) {
        if transport
            .list_windows(session)
            .unwrap_or_default()
            .iter()
            .any(|existing| existing.as_str() == window.as_str())
        {
            if let Some(existing) = transport
                .list_targets()
                .unwrap_or_default()
                .into_iter()
                .find(|pane| {
                    pane.session.as_str() == session.as_str()
                        && pane.window_name.as_ref().map(WindowName::as_str)
                            == Some(window.as_str())
                })
            {
                return Ok(SpawnResult {
                    pane_id: existing.pane_id,
                    session: session.clone(),
                    window: window.clone(),
                    child_pid: existing.pane_pid,
                });
            }
        }
        transport
            .spawn_into(session, window, &plan.provider_argv, workspace, &plan.leader_env)
            .map_err(|error| LeaderError::Start(error.to_string()))
    } else {
        transport
            .spawn_first(session, window, &plan.provider_argv, workspace, &plan.leader_env)
            .map_err(|error| LeaderError::Start(error.to_string()))
    }
}

fn ensure_managed_provider_live_after_attach(
    transport: &dyn Transport,
    spawned: &SpawnResult,
) -> Result<(), LeaderError> {
    let live = match transport.liveness(&spawned.pane_id) {
        Ok(PaneLiveness::Live) => true,
        Ok(PaneLiveness::Dead) => false,
        Ok(PaneLiveness::Unknown) | Err(_) => managed_spawned_pane_in_targets(transport, spawned),
    };
    if live {
        return Ok(());
    }
    Err(LeaderError::Start(format!(
        "managed leader provider pane is not running after tmux client returned: {} {}:{}",
        spawned.pane_id.as_str(),
        spawned.session.as_str(),
        spawned.window.as_str()
    )))
}

fn managed_spawned_pane_in_targets(transport: &dyn Transport, spawned: &SpawnResult) -> bool {
    transport
        .list_targets()
        .unwrap_or_default()
        .iter()
        .any(|pane| {
            pane.pane_id.as_str() == spawned.pane_id.as_str()
                && pane.session.as_str() == spawned.session.as_str()
                && pane.window_name.as_ref().map(WindowName::as_str)
                    == Some(spawned.window.as_str())
        })
}

fn persist_managed_leader_binding(
    plan: &LeaderStartPlan,
    workspace: &Path,
    spawned: &SpawnResult,
) -> Result<(), LeaderError> {
    let identity = plan
        .identity
        .as_ref()
        .ok_or_else(|| LeaderError::Start("managed leader identity missing".to_string()))?;
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let owner_epoch = state
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            state
                .get("team_owner")
                .and_then(|owner| owner.get("owner_epoch"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0)
        .saturating_add(1);
    let now = chrono::Utc::now().to_rfc3339();
    let socket = crate::tmux_backend::socket_path_for_workspace(workspace)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| crate::tmux_backend::socket_name_for_workspace(workspace));
    let provider = serde_json::to_value(plan.provider)?;
    let session = spawned.session.as_str().to_string();
    let window = spawned.window.as_str().to_string();
    let pane = spawned.pane_id.as_str().to_string();
    let receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": provider.clone(),
        "pane_id": pane,
        "pane": pane,
        "session_name": session,
        "window_name": window,
        "tmux_socket": socket,
        "leader_session_uuid": identity.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "attached_at": now,
        "discovery": "managed_launcher",
    });
    let owner = serde_json::json!({
        "pane_id": pane,
        "provider": provider.clone(),
        "machine_fingerprint": identity.machine_fingerprint,
        "leader_session_uuid": identity.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": now,
        "claimed_via": "claim-leader",
        "os_user": identity.os_user,
    });
    if let Some(obj) = state.as_object_mut() {
        // unit-4 (Stage 1) ROOT CAUSE FIX of 0.3.39 leader mis-kill:
        //
        // BEFORE: `obj.insert("session_name", json!(session));` wrote the
        //   leader launcher session (always `team-agent-leader-*`) into the
        //   top-level worker-session-name field, hijacking the identity used
        //   by restart/shutdown when they decided what tmux session to kill.
        //
        // AFTER: the launcher session is recorded ONLY in
        //   `leader_receiver.session_name` (the `receiver` block above) and
        //   `team_owner.pane_id`. The top-level `state.session_name` keeps
        //   whatever value the worker quick-start put there (the real worker
        //   session). If the workspace has never been quick-started yet
        //   (no `session_name` field at all), we leave the field absent —
        //   restart and shutdown have safe default branches for that case.
        //
        // unit-3's preflight is the belt-and-suspenders backstop: even if a
        // future regression reintroduces this overwrite, restart now refuses
        // before killing a leader-prefixed session_name.
        if !crate::layout::sessions::LEADER_SESSION_PREFIX.is_empty()
            && session.starts_with(crate::layout::sessions::LEADER_SESSION_PREFIX)
        {
            // Explicit: skip the overwrite for leader-prefixed launcher
            // sessions. The receiver block records the launcher session in
            // its proper home (`leader_receiver.session_name`).
        } else {
            obj.insert("session_name".to_string(), serde_json::json!(session));
        }
        obj.insert(
            "active_team_key".to_string(),
            serde_json::json!(identity.team_id.as_str()),
        );
        obj.insert("tmux_socket".to_string(), serde_json::json!(socket));
        obj.insert("is_external_leader".to_string(), serde_json::json!(false));
        obj.insert(
            "leader_client".to_string(),
            serde_json::json!({
                "diagnostic_only": true,
                "attach_mode": if std::env::var_os("TMUX").is_some() { "switch-client" } else { "attach-session" },
                "tmux": std::env::var("TMUX").ok(),
            }),
        );
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
    let entry = crate::state::projection::compact_team_state(&state);
    if let Some(obj) = state.as_object_mut() {
        let teams = obj
            .entry("teams".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(teams) = teams.as_object_mut() {
            teams.insert(identity.team_id.as_str().to_string(), entry);
        }
    }
    crate::state::persist::save_runtime_state(workspace, &state)?;
    Ok(())
}

fn persist_exec_provider_leader_binding(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<(), LeaderError> {
    let identity = plan
        .identity
        .as_ref()
        .ok_or_else(|| LeaderError::Start("exec provider leader identity missing".to_string()))?;
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| LeaderError::Start("exec provider leader pane missing".to_string()))?;
    let pane_id = PaneId::new(pane.clone());
    let target = current_tmux_pane_info(&pane_id);
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let owner_epoch = state
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            state
                .get("team_owner")
                .and_then(|owner| owner.get("owner_epoch"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0)
        .saturating_add(1);
    let now = chrono::Utc::now().to_rfc3339();
    let socket = crate::tmux_backend::socket_name_from_tmux_env();
    let provider = serde_json::to_value(plan.provider)?;
    let mut receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": provider.clone(),
        "pane_id": pane,
        "pane": pane,
        "leader_session_uuid": identity.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "attached_at": now,
        "discovery": "current_pane",
    });
    if let Some(target) = target.as_ref() {
        if let Some(obj) = receiver.as_object_mut() {
            obj.insert("session_name".to_string(), serde_json::json!(target.session.as_str()));
            if let Some(window_name) = target.window_name.as_ref() {
                obj.insert("window_name".to_string(), serde_json::json!(window_name.as_str()));
            }
        }
    }
    if let Some(socket) = socket.as_ref() {
        if let Some(obj) = receiver.as_object_mut() {
            obj.insert("tmux_socket".to_string(), serde_json::json!(socket));
        }
    }
    let owner = serde_json::json!({
        "pane_id": pane,
        "provider": provider.clone(),
        "machine_fingerprint": identity.machine_fingerprint,
        "leader_session_uuid": identity.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": now,
        "claimed_via": "claim-leader",
        "os_user": identity.os_user,
    });
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "active_team_key".to_string(),
            serde_json::json!(identity.team_id.as_str()),
        );
        if let Some(target) = target.as_ref() {
            obj.insert("session_name".to_string(), serde_json::json!(target.session.as_str()));
        }
        if let Some(socket) = socket.as_ref() {
            obj.insert("tmux_endpoint".to_string(), serde_json::json!(socket));
            obj.insert("tmux_socket".to_string(), serde_json::json!(socket));
        }
        obj.insert("is_external_leader".to_string(), serde_json::json!(false));
        obj.insert(
            "leader_client".to_string(),
            serde_json::json!({
                "diagnostic_only": true,
                "attach_mode": "exec-provider",
                "tmux": std::env::var("TMUX").ok(),
            }),
        );
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
    let entry = crate::state::projection::compact_team_state(&state);
    if let Some(obj) = state.as_object_mut() {
        let teams = obj
            .entry("teams".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(teams) = teams.as_object_mut() {
            teams.insert(identity.team_id.as_str().to_string(), entry);
        }
    }
    crate::state::persist::save_runtime_state(workspace, &state)?;
    Ok(())
}

fn current_tmux_pane_info(pane_id: &PaneId) -> Option<crate::transport::PaneInfo> {
    tmux_transport_for_current_pane()
        .list_targets()
        .ok()?
        .into_iter()
        .find(|target| target.pane_id == *pane_id)
}

fn persist_external_leader_topology_marker(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<(), LeaderError> {
    let identity = plan
        .identity
        .as_ref()
        .ok_or_else(|| LeaderError::Start("external leader identity missing".to_string()))?;
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = state.as_object_mut() {
        obj.entry("workspace".to_string()).or_insert_with(|| {
            serde_json::json!(resolve_workspace_for_hash(workspace).to_string_lossy().to_string())
        });
        obj.entry("active_team_key".to_string())
            .or_insert_with(|| serde_json::json!(identity.team_id.as_str()));
        if let Some(session) = plan.session_name.as_ref() {
            obj.entry("session_name".to_string())
                .or_insert_with(|| serde_json::json!(session.as_str()));
        }
        obj.insert("is_external_leader".to_string(), serde_json::json!(true));
    }
    let entry = crate::state::projection::compact_team_state(&state);
    if let Some(obj) = state.as_object_mut() {
        let teams = obj
            .entry("teams".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(teams) = teams.as_object_mut() {
            teams.insert(identity.team_id.as_str().to_string(), entry);
        }
    }
    crate::state::persist::save_runtime_state(workspace, &state)?;
    Ok(())
}

fn insert_detach_flag(argv: &mut Vec<String>) -> bool {
    if argv.iter().any(|arg| arg == "-d") {
        return false;
    }
    let Some(pos) = argv.iter().position(|arg| arg == "new-session") else {
        return false;
    };
    argv.insert(pos + 1, "-d".to_string());
    true
}

fn run_leader_argv(
    argv: &[String],
    env: &BTreeMap<String, String>,
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<std::process::ExitStatus, LeaderError> {
    let Some(program) = argv.first() else {
        return Err(LeaderError::Start(
            "leader launch argv is empty".to_string(),
        ));
    };
    let mut child = Command::new(program)
        .args(argv.iter().skip(1))
        .envs(env)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    if plan.mode == LeaderStartMode::ExecProvider {
        spawn_exec_provider_startup_prompt_handler(plan.provider, workspace.to_path_buf());
    }
    child.wait().map_err(LeaderError::Io)
}

fn spawn_exec_provider_startup_prompt_handler(provider: Provider, workspace: PathBuf) {
    let Some(pane_id) = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        write_leader_startup_prompt_event(
            &workspace,
            "leader.startup_prompt_skipped",
            serde_json::json!({
                "provider": provider_wire(provider),
                "reason": "tmux_pane_missing",
                "action": "skip",
            }),
        );
        return;
    };
    std::thread::spawn(move || {
        let transport = tmux_transport_for_current_pane();
        let _ = handle_exec_provider_startup_prompts(
            provider, &workspace, &pane_id, &transport, 30, 0.5,
        );
    });
}

fn spawn_managed_provider_startup_prompt_handler(
    provider: Provider,
    workspace: PathBuf,
    pane_id: String,
) {
    std::thread::spawn(move || {
        let transport = TmuxBackend::for_workspace(&workspace);
        let _ = handle_exec_provider_startup_prompts(
            provider,
            &workspace,
            &pane_id,
            &transport,
            30,
            0.5,
        );
    });
}

fn tmux_transport_for_current_pane() -> TmuxBackend {
    crate::tmux_backend::socket_name_from_tmux_env()
        .map(|endpoint| TmuxBackend::for_tmux_endpoint(&endpoint))
        .unwrap_or_else(TmuxBackend::new)
}

pub fn handle_exec_provider_startup_prompts(
    provider: Provider,
    workspace: &Path,
    pane_id: &str,
    transport: &dyn Transport,
    checks: usize,
    sleep_s: f64,
) -> crate::provider::StartupPromptOutcome {
    let target = Target::Pane(PaneId::new(pane_id.to_string()));
    let outcome =
        get_adapter(provider).handle_startup_prompts_outcome(transport, &target, checks, sleep_s);
    for handled in &outcome.handled {
        write_leader_startup_prompt_event(
            workspace,
            "leader.startup_prompt_handled",
            serde_json::json!({
                "provider": provider_wire(provider),
                "pane_id": pane_id,
                "prompt": handled.prompt,
                "action": handled.action,
            }),
        );
    }
    if let Some(error) = &outcome.capture_error {
        write_leader_startup_prompt_event(
            workspace,
            "leader.startup_prompt_capture_failed",
            serde_json::json!({
                "provider": provider_wire(provider),
                "pane_id": pane_id,
                "action": "capture",
                "error": error,
            }),
        );
    }
    outcome
}

fn write_leader_startup_prompt_event(workspace: &Path, event: &str, fields: serde_json::Value) {
    let _ = crate::event_log::EventLog::new(workspace).write(event, fields);
}

fn ensure_tmux_installed() -> Result<(), LeaderError> {
    match Command::new("tmux").arg("-V").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) | Err(_) => Err(LeaderError::Start(
            "tmux is not installed; install tmux 3.3+ or start the leader from an existing tmux pane"
                .to_string(),
        )),
    }
}

fn provider_command_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        // §B leader 入口接缝(设计 design.md line 40):`team-agent copilot` 启 leader
        // 即 spawn 真 copilot 命令;B5 session 名前缀 `team-agent-leader-copilot-*`
        // (leader/start.rs:192-204 派生)自动覆盖前缀保护。
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "fake",
    }
}

fn tmux_session_exists(workspace: &Path, session: &SessionName) -> Result<bool, LeaderError> {
    TmuxBackend::for_workspace(workspace)
        .has_session(session)
        .map_err(|e| LeaderError::Start(format!("tmux has-session failed: {e}")))
}

fn leader_export_assignments(leader_env: &BTreeMap<String, String>) -> Vec<String> {
    [
        "TEAM_AGENT_LEADER_PROVIDER",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_MACHINE_FINGERPRINT",
        "TEAM_AGENT_WORKSPACE",
        "TEAM_AGENT_TEAM_ID",
    ]
    .iter()
    .filter_map(|key| {
        leader_env
            .get(*key)
            .map(|value| shlex_quote(&format!("{key}={value}")))
    })
    .collect()
}

fn merged_exec_env(leader_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.extend(
        leader_env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    env
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shlex_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shlex_quote(raw: &str) -> String {
    if !raw.is_empty()
        && raw.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'@' | b'%' | b'_' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        })
    {
        raw.to_string()
    } else {
        format!("'{}'", raw.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;

    use crate::leader::{
        LeaderIdentity, LeaderLaunchSocket, LeaderSessionUuidSource, LeaderStartMode,
        LeaderStartPlan,
    };
    use crate::model::enums::PaneLiveness;
    use crate::model::ids::{LeaderSessionUuid, TeamKey};
    use crate::provider::{Provider, COPILOT_READY_MARKER, COPILOT_TRUST_PROMPT_MARKER};
    use crate::transport::{
        AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
        InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
        SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
        TurnVerification, WindowName,
    };

    use super::{
        ensure_managed_provider_live_after_attach, execute_leader_plan,
        handle_exec_provider_startup_prompts, shlex_quote,
    };

    struct ScriptedTransport {
        screens: Mutex<Vec<String>>,
        sent: Mutex<Vec<(Target, Vec<Key>)>>,
        liveness: PaneLiveness,
        targets: Vec<PaneInfo>,
    }

    impl ScriptedTransport {
        fn new(screens: Vec<String>) -> Self {
            Self {
                screens: Mutex::new(screens),
                sent: Mutex::new(Vec::new()),
                liveness: PaneLiveness::Unknown,
                targets: Vec::new(),
            }
        }

        fn with_liveness(liveness: PaneLiveness) -> Self {
            Self {
                screens: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                liveness,
                targets: Vec::new(),
            }
        }

        fn with_liveness_and_targets(liveness: PaneLiveness, targets: Vec<PaneInfo>) -> Self {
            Self {
                screens: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                liveness,
                targets,
            }
        }

        fn sent(&self) -> Vec<(Target, Vec<Key>)> {
            match self.sent.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    impl Transport for ScriptedTransport {
        fn kind(&self) -> BackendKind {
            BackendKind::Tmux
        }

        fn spawn_first(
            &self,
            _session: &SessionName,
            _window: &WindowName,
            _argv: &[String],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            Err(TransportError::Io(std::io::Error::other(
                "spawn_first not used by startup-prompt test",
            )))
        }

        fn spawn_into(
            &self,
            _session: &SessionName,
            _window: &WindowName,
            _argv: &[String],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            Err(TransportError::Io(std::io::Error::other(
                "spawn_into not used by startup-prompt test",
            )))
        }

        fn inject(
            &self,
            _target: &Target,
            _payload: &InjectPayload,
            _submit: Key,
            _bracketed: bool,
        ) -> Result<InjectReport, TransportError> {
            Ok(InjectReport {
                stage_reached: InjectStage::Submit,
                inject_verification: InjectVerification::CaptureContainsToken,
                submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
                turn_verification: TurnVerification::NotRequired,
                attempts: 1,
                submit_diagnostics: None,
            })
        }

        fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError> {
            match self.sent.lock() {
                Ok(mut guard) => guard.push((target.clone(), keys.to_vec())),
                Err(poisoned) => poisoned.into_inner().push((target.clone(), keys.to_vec())),
            }
            Ok(())
        }

        fn capture(
            &self,
            _target: &Target,
            range: CaptureRange,
        ) -> Result<CapturedText, TransportError> {
            let text = match self.screens.lock() {
                Ok(mut guard) => {
                    if guard.is_empty() {
                        String::new()
                    } else {
                        guard.remove(0)
                    }
                }
                Err(poisoned) => {
                    let mut guard = poisoned.into_inner();
                    if guard.is_empty() {
                        String::new()
                    } else {
                        guard.remove(0)
                    }
                }
            };
            Ok(CapturedText { text, range })
        }

        fn query(
            &self,
            _target: &Target,
            _field: PaneField,
        ) -> Result<Option<String>, TransportError> {
            Ok(None)
        }

        fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
            Ok(self.liveness)
        }

        fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
            Ok(self.targets.clone())
        }

        fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
            Ok(false)
        }

        fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
            Ok(Vec::new())
        }

        fn set_session_env(
            &self,
            _session: &SessionName,
            _key: &str,
            _value: &str,
        ) -> Result<SetEnvOutcome, TransportError> {
            Ok(SetEnvOutcome::Applied)
        }

        fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
            Ok(())
        }

        fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
            Ok(())
        }

        fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
            Ok(AttachOutcome::Unsupported {
                reason: "not used by startup-prompt test".to_string(),
            })
        }
    }

    fn managed_spawn_result() -> SpawnResult {
        SpawnResult {
            pane_id: PaneId::new("%42"),
            session: SessionName::new("team-agent-leader-claude_code-demo"),
            window: WindowName::new("claude_code"),
            child_pid: Some(1234),
        }
    }

    fn managed_pane_info(spawned: &SpawnResult) -> PaneInfo {
        PaneInfo {
            pane_id: spawned.pane_id.clone(),
            session: spawned.session.clone(),
            window_index: Some(0),
            window_name: Some(spawned.window.clone()),
            pane_index: Some(0),
            tty: None,
            current_command: Some("claude".to_string()),
            current_path: None,
            active: true,
            pane_pid: spawned.child_pid,
            leader_env: BTreeMap::new(),
        }
    }

    #[test]
    fn managed_attach_success_requires_live_provider_pane() {
        let spawned = managed_spawn_result();
        let transport = ScriptedTransport::with_liveness(PaneLiveness::Dead);

        let err = ensure_managed_provider_live_after_attach(&transport, &spawned)
            .expect_err("dead provider pane must fail managed launch");

        let text = err.to_string();
        assert!(
            text.contains("managed leader provider pane is not running"),
            "{text}"
        );
        assert!(text.contains("%42"), "{text}");
        assert!(text.contains("claude_code"), "{text}");
    }

    #[test]
    fn managed_attach_success_accepts_live_provider_pane() {
        let spawned = managed_spawn_result();
        let transport = ScriptedTransport::with_liveness(PaneLiveness::Live);

        ensure_managed_provider_live_after_attach(&transport, &spawned)
            .expect("live provider pane keeps managed launch successful");
    }

    #[test]
    fn managed_attach_success_uses_target_scan_when_liveness_unknown() {
        let spawned = managed_spawn_result();
        let transport = ScriptedTransport::with_liveness_and_targets(
            PaneLiveness::Unknown,
            vec![managed_pane_info(&spawned)],
        );

        ensure_managed_provider_live_after_attach(&transport, &spawned)
            .expect("target scan can prove provider pane is still live");
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for (key, value) in vars {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..).rev() {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    #[test]
    fn external_exec_provider_persists_topology_before_provider_exec() {
        let workspace = std::env::temp_dir().join(format!(
            "ta-external-pre-exec-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        let command = format!(
            "test -f {path} && grep -q is_external_leader {path}",
            path = shlex_quote(&state_path.to_string_lossy())
        );
        let identity = LeaderIdentity {
            leader_session_uuid: LeaderSessionUuid::derive(
                "fp",
                &workspace.to_string_lossy(),
                "tester",
                "current",
            )
            .unwrap(),
            leader_session_uuid_source: LeaderSessionUuidSource::Derived,
            machine_fingerprint: "fp".to_string(),
            workspace_abspath: workspace.clone(),
            os_user: "tester".to_string(),
            team_id: TeamKey::new("current"),
        };
        let plan = LeaderStartPlan {
            mode: LeaderStartMode::ExecProvider,
            provider: Provider::Codex,
            workspace: workspace.clone(),
            socket: LeaderLaunchSocket::Workspace,
            session_name: None,
            argv: vec!["sh".to_string(), "-c".to_string(), command],
            provider_argv: vec!["codex".to_string()],
            leader_window: None,
            is_external_leader: true,
            leader_env: BTreeMap::new(),
            identity: Some(identity),
            detached: false,
        };

        let outcome = execute_leader_plan(&plan, &workspace)
            .expect("external marker must be present before provider argv runs");

        assert_eq!(outcome.status, crate::leader::LeaderLaunchStatus::Exited);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(state["is_external_leader"], serde_json::json!(true));
        assert_eq!(state["teams"]["current"]["is_external_leader"], serde_json::json!(true));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    #[serial_test::serial(env)]
    fn default_exec_provider_persists_current_pane_binding_before_provider_exec() {
        let workspace = std::env::temp_dir().join(format!(
            "ta-current-pane-pre-exec-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        let command = format!(
            "test -f {path} && grep -q leader_receiver {path}",
            path = shlex_quote(&state_path.to_string_lossy())
        );
        let _env = EnvGuard::set(&[
            ("TMUX", Some("/private/tmp/tmux-501/default,88432,187")),
            ("TMUX_PANE", Some("%77")),
        ]);
        let identity = LeaderIdentity {
            leader_session_uuid: LeaderSessionUuid::derive(
                "fp",
                &workspace.to_string_lossy(),
                "tester",
                "current",
            )
            .unwrap(),
            leader_session_uuid_source: LeaderSessionUuidSource::Derived,
            machine_fingerprint: "fp".to_string(),
            workspace_abspath: workspace.clone(),
            os_user: "tester".to_string(),
            team_id: TeamKey::new("current"),
        };
        let plan = LeaderStartPlan {
            mode: LeaderStartMode::ExecProvider,
            provider: Provider::Fake,
            workspace: workspace.clone(),
            socket: LeaderLaunchSocket::Workspace,
            session_name: None,
            argv: vec!["sh".to_string(), "-c".to_string(), command],
            provider_argv: vec!["fake".to_string()],
            leader_window: None,
            is_external_leader: false,
            leader_env: BTreeMap::new(),
            identity: Some(identity),
            detached: false,
        };

        let outcome = execute_leader_plan(&plan, &workspace)
            .expect("current pane binding must be present before provider argv runs");

        assert_eq!(outcome.status, crate::leader::LeaderLaunchStatus::Exited);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(state["is_external_leader"], serde_json::json!(false));
        assert_eq!(state["leader_receiver"]["pane_id"], serde_json::json!("%77"));
        assert_eq!(
            state["leader_receiver"]["tmux_socket"],
            serde_json::json!("/private/tmp/tmux-501/default")
        );
        assert_eq!(state["team_owner"]["pane_id"], serde_json::json!("%77"));
        assert_eq!(
            state["teams"]["current"]["leader_receiver"]["pane_id"],
            serde_json::json!("%77")
        );
        assert_eq!(
            state["teams"]["current"]["team_owner"]["pane_id"],
            serde_json::json!("%77")
        );
        assert_eq!(
            state["leader_client"]["attach_mode"],
            serde_json::json!("exec-provider")
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn exec_provider_leader_startup_prompt_handler_reuses_copilot_adapter() {
        let transport = ScriptedTransport::new(vec![
            COPILOT_TRUST_PROMPT_MARKER.to_string(),
            COPILOT_READY_MARKER.to_string(),
        ]);
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_red2_leader_startup_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workspace);

        let outcome = handle_exec_provider_startup_prompts(
            Provider::Copilot,
            &workspace,
            "%0",
            &transport,
            5,
            0.0,
        );

        assert_eq!(outcome.handled.len(), 1);
        assert_eq!(outcome.handled[0].prompt, "copilot_workspace_trust");
        assert_eq!(outcome.handled[0].action, "sent_enter_yes_session");
        let sent = transport.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, Target::Pane(PaneId::new("%0")));
        assert_eq!(sent[0].1, vec![Key::Enter]);
    }
}
