//!
//! leader::start — leader_start_plan / leader_session_name(派生 tmux session 名)。

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};

use crate::model::pane_authority_refusal as pane_refusal;
use crate::provider::{get_adapter, Provider};
use crate::tmux_backend::TmuxBackend;
use crate::transport::{
    PaneId, PaneInfo, PaneLiveness, SessionName, SpawnResult, Target, Transport, WindowName,
};

use super::helpers::{
    provider_wire, resolve_workspace_for_hash, sanitize_session_folder, sha1_hex_prefix,
};
use super::owner_bind::leader_identity_context;
use super::{
    LeaderError, LeaderIdentity, LeaderLaunchOutcome, LeaderLaunchSocket, LeaderLaunchStatus,
    LeaderStartMode, LeaderStartPlan,
};

// ── leader::start — leader_start_plan / session 名 ──

pub(crate) struct PreparedLeaderStart {
    plan: LeaderStartPlan,
    ambient_authority: Option<VerifiedAmbientPaneAuthority>,
}

#[derive(Clone, Copy)]
enum ManagedClientAttachMode {
    AttachSession,
    SwitchClient,
}

const DIFFERENT_TMUX_SERVER_PREFIX: &str =
    "managed launcher refuses a different ambient tmux server";

impl PreparedLeaderStart {
    pub(crate) fn plan(&self) -> &LeaderStartPlan {
        &self.plan
    }
}

pub(crate) enum PrepareLeaderStartError {
    PaneAuthorityRefused(pane_refusal::PaneAuthorityRefusal),
    Leader(LeaderError),
}

impl PrepareLeaderStartError {
    fn into_leader_error(self) -> LeaderError {
        match self {
            Self::PaneAuthorityRefused(refusal) => LeaderError::Validation(format!(
                "{}; open a new terminal outside the current tmux/pane or run \
                 `team-agent attach-leader` from the intended workspace pane",
                refusal.reason().as_str()
            )),
            Self::Leader(error) => error,
        }
    }
}

impl From<LeaderError> for PrepareLeaderStartError {
    fn from(error: LeaderError) -> Self {
        Self::Leader(error)
    }
}

/// `leader_start_plan`(card §46;`__init__.py:82`)。计算 leader 启动计划
/// (exec in-TMUX / new tmux session / attach existing)。provider 未安装 → `Err(Start)`。
pub(crate) fn leader_start_plan(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
) -> Result<LeaderStartPlan, LeaderError> {
    prepare_leader_start(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
        external_leader,
    )
    .map(|prepared| prepared.plan)
    .map_err(PrepareLeaderStartError::into_leader_error)
}

pub(crate) fn prepare_leader_start(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
) -> Result<PreparedLeaderStart, PrepareLeaderStartError> {
    prepare_leader_start_with_nested_attach(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
        external_leader,
        false,
    )
}

pub(crate) fn prepare_leader_start_with_nested_attach(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
    allow_nested_attach: bool,
) -> Result<PreparedLeaderStart, PrepareLeaderStartError> {
    let explicit_external_path = external_leader || attach_existing || attach_session.is_some();
    let state_external_path = workspace_state_uses_external_leader(workspace);
    let (ambient_authority, managed_client_attach_mode, managed_provider_reentry) =
        if explicit_external_path || state_external_path {
            (ambient_pane_authority_preflight(workspace)?, None, false)
        } else if workspace_state_is_managed_provider_reentry(workspace) {
            (ambient_pane_authority_preflight(workspace)?, None, true)
        } else {
            let (authority, attach_mode) =
                managed_launcher_ambient_route(workspace, allow_nested_attach)?;
            (authority, attach_mode, false)
        };
    let plan = leader_start_plan_with_ambient_authority(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
        external_leader,
        std::env::var_os("TMUX").is_some(),
        managed_client_attach_mode,
        managed_provider_reentry,
    )?;
    Ok(PreparedLeaderStart {
        plan,
        ambient_authority,
    })
}

pub(crate) fn leader_start_plan_after_ambient_authority_check(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
) -> Result<LeaderStartPlan, LeaderError> {
    let in_tmux = std::env::var_os("TMUX").is_some();
    let explicit_external_path = external_leader || attach_existing || attach_session.is_some();
    let managed_client_attach_mode = (!explicit_external_path).then_some(if in_tmux {
        ManagedClientAttachMode::SwitchClient
    } else {
        ManagedClientAttachMode::AttachSession
    });
    leader_start_plan_with_ambient_authority(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
        external_leader,
        in_tmux,
        managed_client_attach_mode,
        false,
    )
}

fn leader_start_plan_with_ambient_authority(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
    external_leader: bool,
    in_tmux: bool,
    managed_client_attach_mode: Option<ManagedClientAttachMode>,
    managed_provider_reentry: bool,
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
    let external_path = external_leader
        || attach_existing
        || attach_session.is_some()
        || state
            .as_ref()
            .is_some_and(crate::state::projection::state_is_external_leader);
    // 0.3.28 Step 2: managed mode now uses the SAME dedicated leader session
    // as the external path (`team-agent-leader-<provider>-<folder>-<sha1[:8]>`)
    // — Python parity. Pre-0.3.28 the managed branch used
    // `managed_team_session_name(identity) = team-<team_id>` which is the
    // worker session — that co-location is the structural root of
    // E49/E51/E53/E57-3/E60.
    // A-22: managed mode reuses a live provider leader session by prefix;
    // when no candidate exists, it falls back to a per-launch nonce. The
    // external/attach paths keep the stable workspace-keyed name because a
    // nonce would break `--attach-session` reattach.
    let session_name = if external_path {
        attach_session
            .cloned()
            .or_else(|| Some(leader_session_name(provider, workspace)))
    } else {
        Some(managed_leader_session_for_launch(provider, workspace))
    };
    if !in_tmux {
        ensure_tmux_installed()?;
    }
    let existing_session =
        if external_path && !in_tmux && !attach_existing && attach_session.is_none() {
            match session_name.as_ref() {
                Some(session) => tmux_session_exists(workspace, session)?,
                None => false,
            }
        } else {
            false
        };
    let mode = if managed_provider_reentry {
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
        managed_client_attach_mode,
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

pub(crate) fn execute_prepared_leader_start(
    prepared: &PreparedLeaderStart,
    workspace: &Path,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    execute_leader_plan_after_ambient_authority(
        &prepared.plan,
        workspace,
        prepared.ambient_authority.as_ref(),
    )
}

fn execute_leader_plan_after_ambient_authority(
    plan: &LeaderStartPlan,
    workspace: &Path,
    ambient_authority: Option<&VerifiedAmbientPaneAuthority>,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    if plan.mode == LeaderStartMode::ManagedTmuxClient {
        return execute_managed_leader_plan(plan, workspace);
    }
    let mut argv = plan.argv.clone();
    let detached = plan.mode == LeaderStartMode::NewTmuxSession
        && !std::io::stdin().is_terminal()
        && insert_detach_flag(&mut argv);
    let bind_verified_ambient = plan.mode == LeaderStartMode::ExecProvider
        && (!plan.is_external_leader || workspace_state_uses_external_leader(workspace));
    if bind_verified_ambient {
        let authority = ambient_authority.ok_or_else(|| {
            LeaderError::Validation("exec provider ambient pane authority missing".to_string())
        })?;
        // 0.5.35 (`.team/artifacts/managed-leader-provider-reentry-locate.md`
        // §5/§6): three-way classify BEFORE persist. Same physical pane =
        // provider process replacement (refresh only). Different pane with
        // an existing canonical binding = no canonical rewrite (claim /
        // takeover is the recovery path, §7). Unbound = first-time write.
        match classify_exec_provider_binding(workspace, plan, authority)? {
            ExecProviderBinding::ManagedReentry => {
                refresh_managed_leader_provider_binding(plan, workspace, authority)?;
            }
            ExecProviderBinding::DifferentPaneAlreadyBound => {
                // No canonical state write. Provider still runs below.
            }
            ExecProviderBinding::Unbound => {
                persist_exec_provider_leader_binding(
                    plan,
                    workspace,
                    authority,
                    plan.is_external_leader,
                )?;
            }
        }
    } else if plan.is_external_leader {
        persist_external_leader_topology_marker(plan, workspace)?;
    }
    let process = run_leader_argv(&argv, &plan.leader_env, plan, workspace)?;
    let code = process.status.code();
    if !process.status.success() {
        return Err(LeaderError::Start(leader_launcher_failure(&process)));
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

struct VerifiedAmbientPaneAuthority {
    pane_id: PaneId,
    observed: PaneInfo,
    endpoint: String,
    tmux: String,
}

fn workspace_state_uses_external_leader(workspace: &Path) -> bool {
    crate::state::persist::load_runtime_state_without_migrations(workspace)
        .ok()
        .as_ref()
        .is_some_and(crate::state::projection::state_is_external_leader)
}

fn workspace_state_is_managed_provider_reentry(workspace: &Path) -> bool {
    let Ok(state) = crate::state::persist::load_runtime_state_without_migrations(workspace) else {
        return false;
    };
    if crate::state::projection::state_is_external_leader(&state) {
        return false;
    }
    let Some(team_key) = state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(receiver) = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .and_then(|team| team.get("leader_receiver"))
    else {
        return false;
    };
    let Some(pane) = std::env::var_os("TMUX_PANE") else {
        return false;
    };
    let Some(tmux) = std::env::var_os("TMUX") else {
        return false;
    };
    let tmux = tmux.to_string_lossy();
    let endpoint = tmux.split(',').next().unwrap_or("").trim();
    receiver.get("status").and_then(serde_json::Value::as_str) == Some("attached")
        && receiver.get("pane_id").and_then(serde_json::Value::as_str) == pane.to_str()
        && receiver
            .get("tmux_socket")
            .and_then(serde_json::Value::as_str)
            == Some(endpoint)
        && receiver
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session.starts_with(LEADER_SESSION_PREFIX))
}

fn ambient_pane_authority_preflight(
    workspace: &Path,
) -> Result<Option<VerifiedAmbientPaneAuthority>, PrepareLeaderStartError> {
    let Some(tmux) = std::env::var_os("TMUX") else {
        return Ok(None);
    };
    let tmux = tmux.into_string().map_err(|_| {
        PrepareLeaderStartError::PaneAuthorityRefused(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::TmuxValueNotUnicode,
        ))
    })?;
    verified_ambient_pane_authority(workspace, &tmux)
        .map(Some)
        .map_err(PrepareLeaderStartError::PaneAuthorityRefused)
}

fn managed_launcher_ambient_route(
    workspace: &Path,
    allow_nested_attach: bool,
) -> Result<
    (
        Option<VerifiedAmbientPaneAuthority>,
        Option<ManagedClientAttachMode>,
    ),
    PrepareLeaderStartError,
> {
    let Some(tmux) = std::env::var_os("TMUX") else {
        return Ok((None, Some(ManagedClientAttachMode::AttachSession)));
    };
    let tmux = tmux.into_string().map_err(|_| {
        PrepareLeaderStartError::PaneAuthorityRefused(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::TmuxValueNotUnicode,
        ))
    })?;
    let observed_endpoint = validated_ambient_tmux_endpoint(workspace, &tmux)
        .map_err(PrepareLeaderStartError::PaneAuthorityRefused)?;
    let target_socket_name = TmuxBackend::for_workspace(workspace)
        .tmux_endpoint()
        .ok_or_else(|| LeaderError::Start("workspace tmux endpoint missing".to_string()))?;
    let target_endpoint = crate::tmux_backend::socket_path_for_workspace(workspace);
    let same_server = target_endpoint.as_ref().is_some_and(|target| {
        let observed = Path::new(&observed_endpoint)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&observed_endpoint));
        let target = target.canonicalize().unwrap_or_else(|_| target.clone());
        observed == target
    });
    if same_server {
        let authority = verified_ambient_pane_authority(workspace, &tmux)
            .map_err(PrepareLeaderStartError::PaneAuthorityRefused)?;
        return Ok((Some(authority), Some(ManagedClientAttachMode::SwitchClient)));
    }
    if !allow_nested_attach {
        return Err(LeaderError::Validation(format!(
            "{DIFFERENT_TMUX_SERVER_PREFIX}: observed_endpoint={observed_endpoint}; \
             requested_workspace_socket={target_socket_name}"
        ))
        .into());
    }
    let authority = verified_ambient_pane_authority(workspace, &tmux)
        .map_err(PrepareLeaderStartError::PaneAuthorityRefused)?;
    Ok((
        Some(authority),
        Some(ManagedClientAttachMode::AttachSession),
    ))
}

fn validated_ambient_tmux_endpoint(
    workspace: &Path,
    tmux: &str,
) -> Result<String, pane_refusal::PaneAuthorityRefusal> {
    let tuple = tmux.split(',').map(str::trim).collect::<Vec<_>>();
    let [endpoint, server_pid, session_index] = tuple.as_slice() else {
        return Err(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::TmuxTupleFieldCountInvalid,
        ));
    };
    if endpoint.is_empty() {
        return Err(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::EndpointMissing,
        ));
    }
    if !Path::new(endpoint).is_absolute() {
        return Err(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::EndpointNotAbsolute,
        ));
    }
    if server_pid.parse::<u32>().is_err() {
        return Err(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::ServerPidInvalid,
        ));
    }
    if session_index.parse::<u32>().is_err() {
        return Err(ambient_tmux_endpoint_refusal(
            workspace,
            pane_refusal::AmbientTmuxEndpointUnavailableCause::SessionIndexInvalid,
        ));
    }
    Ok((*endpoint).to_string())
}

fn verified_ambient_pane_authority(
    workspace: &Path,
    tmux: &str,
) -> Result<VerifiedAmbientPaneAuthority, pane_refusal::PaneAuthorityRefusal> {
    let requested_workspace = resolve_workspace_for_hash(workspace);
    let endpoint = validated_ambient_tmux_endpoint(workspace, tmux)?;
    let pane_id = match std::env::var_os("TMUX_PANE") {
        None => {
            return Err(pane_refusal::PaneAuthorityRefusal::new(
                pane_refusal::PaneAuthorityRefusalFacts::AmbientPaneIdUnavailable(
                    pane_refusal::AmbientPaneIdUnavailableFacts {
                        requested_workspace,
                        observed_pane_id: pane_refusal::UnavailablePaneAuthorityFact::new(
                            pane_refusal::AmbientPaneIdUnavailableCause::EnvironmentValueMissing,
                        ),
                        endpoint,
                    },
                ),
            ));
        }
        Some(value) => match value.into_string() {
            Ok(value) if value.trim().is_empty() => {
                return Err(pane_refusal::PaneAuthorityRefusal::new(
                    pane_refusal::PaneAuthorityRefusalFacts::AmbientPaneIdUnavailable(
                        pane_refusal::AmbientPaneIdUnavailableFacts {
                            requested_workspace,
                            observed_pane_id: pane_refusal::UnavailablePaneAuthorityFact::new(
                                pane_refusal::AmbientPaneIdUnavailableCause::EnvironmentValueEmpty,
                            ),
                            endpoint,
                        },
                    ),
                ));
            }
            Ok(value) => PaneId::new(value.trim().to_string()),
            Err(_) => {
                return Err(pane_refusal::PaneAuthorityRefusal::new(
                    pane_refusal::PaneAuthorityRefusalFacts::AmbientPaneIdUnavailable(
                        pane_refusal::AmbientPaneIdUnavailableFacts {
                            requested_workspace,
                            observed_pane_id: pane_refusal::UnavailablePaneAuthorityFact::new(
                                pane_refusal::AmbientPaneIdUnavailableCause::EnvironmentValueNotUnicode,
                            ),
                            endpoint,
                        },
                    ),
                ));
            }
        },
    };
    let observed = current_tmux_pane_info(&pane_id, &endpoint).map_err(|error| {
        let cause = match error {
            CurrentTmuxPaneInfoError::QueryFailed => {
                pane_refusal::AmbientPaneWorkspaceUnavailableCause::PaneQueryFailed
            }
            CurrentTmuxPaneInfoError::PaneNotFound => {
                pane_refusal::AmbientPaneWorkspaceUnavailableCause::PaneNotFound
            }
        };
        pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::AmbientPaneWorkspaceUnavailable(
                pane_refusal::AmbientPaneWorkspaceUnavailableFacts {
                    requested_workspace: requested_workspace.clone(),
                    observed_pane_id: pane_id.as_str().to_string(),
                    observed_pane_workspace: pane_refusal::UnavailablePaneAuthorityFact::new(cause),
                    endpoint: endpoint.clone(),
                },
            ),
        )
    })?;
    let pane_workspace = observed.current_path.clone().ok_or_else(|| {
        pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::AmbientPaneWorkspaceUnavailable(
                pane_refusal::AmbientPaneWorkspaceUnavailableFacts {
                    requested_workspace: requested_workspace.clone(),
                    observed_pane_id: pane_id.as_str().to_string(),
                    observed_pane_workspace: pane_refusal::UnavailablePaneAuthorityFact::new(
                        pane_refusal::AmbientPaneWorkspaceUnavailableCause::CurrentPathMissing,
                    ),
                    endpoint: endpoint.clone(),
                },
            ),
        )
    })?;
    let caller_tty = caller_controlling_tty().map_err(|cause| {
        pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::CallerControllingTtyUnavailable(
                pane_refusal::CallerControllingTtyUnavailableFacts {
                    requested_workspace: requested_workspace.clone(),
                    observed_pane_id: pane_id.as_str().to_string(),
                    endpoint: endpoint.clone(),
                    caller_controlling_tty: pane_refusal::UnavailablePaneAuthorityFact::new(cause),
                },
            ),
        )
    })?;
    let pane_tty_path = observed.tty.as_deref().ok_or_else(|| {
        pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::ObservedPaneTtyUnavailable(
                pane_refusal::ObservedPaneTtyUnavailableFacts {
                    requested_workspace: requested_workspace.clone(),
                    observed_pane_id: pane_id.as_str().to_string(),
                    endpoint: endpoint.clone(),
                    observed_pane_tty: pane_refusal::UnavailablePaneAuthorityFact::new(
                        pane_refusal::ObservedPaneTtyUnavailableCause::PaneTtyMissing,
                    ),
                },
            ),
        )
    })?;
    let pane_tty = pane_tty_device(pane_tty_path).map_err(|cause| {
        pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::ObservedPaneTtyUnavailable(
                pane_refusal::ObservedPaneTtyUnavailableFacts {
                    requested_workspace: requested_workspace.clone(),
                    observed_pane_id: pane_id.as_str().to_string(),
                    endpoint: endpoint.clone(),
                    observed_pane_tty: pane_refusal::UnavailablePaneAuthorityFact::new(cause),
                },
            ),
        )
    })?;
    if caller_tty != pane_tty {
        return Err(pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::PaneTtyMismatch(
                pane_refusal::PaneTtyMismatchFacts {
                    requested_workspace,
                    observed_pane_id: pane_id.as_str().to_string(),
                    endpoint,
                    caller_controlling_tty: caller_tty,
                    observed_pane_tty: pane_tty,
                },
            ),
        ));
    }
    if !crate::messaging::leader_channel::path_is_in_workspace(&pane_workspace, workspace) {
        return Err(pane_refusal::PaneAuthorityRefusal::new(
            pane_refusal::PaneAuthorityRefusalFacts::PaneWorkspaceMismatch(
                pane_refusal::PaneWorkspaceMismatchFacts {
                    requested_workspace,
                    observed_pane_id: pane_id.as_str().to_string(),
                    observed_pane_workspace: pane_workspace,
                    endpoint,
                },
            ),
        ));
    }
    Ok(VerifiedAmbientPaneAuthority {
        pane_id,
        observed,
        endpoint,
        tmux: tmux.to_string(),
    })
}

fn ambient_tmux_endpoint_refusal(
    workspace: &Path,
    cause: pane_refusal::AmbientTmuxEndpointUnavailableCause,
) -> pane_refusal::PaneAuthorityRefusal {
    pane_refusal::PaneAuthorityRefusal::new(
        pane_refusal::PaneAuthorityRefusalFacts::AmbientTmuxEndpointUnavailable(
            pane_refusal::AmbientTmuxEndpointUnavailableFacts {
                requested_workspace: resolve_workspace_for_hash(workspace),
                endpoint: pane_refusal::UnavailablePaneAuthorityFact::new(cause),
            },
        ),
    )
}

#[cfg(target_os = "macos")]
fn caller_controlling_tty() -> Result<u64, pane_refusal::CallerControllingTtyUnavailableCause> {
    // `/dev/tty` proves that this process has a controlling terminal, but its
    // own path and rdev remain the generic alias. Query the process table for
    // the actual controlling device identity.
    let _controlling_tty = std::fs::File::open("/dev/tty")
        .map_err(|_| pane_refusal::CallerControllingTtyUnavailableCause::NoControllingTty)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            info_size as libc::c_int,
        )
    };
    if read != info_size as libc::c_int {
        return Err(pane_refusal::CallerControllingTtyUnavailableCause::DeviceIdentityUnresolvable);
    }
    Ok(u64::from(unsafe { info.assume_init() }.e_tdev))
}

#[cfg(target_os = "linux")]
fn caller_controlling_tty() -> Result<u64, pane_refusal::CallerControllingTtyUnavailableCause> {
    let controlling_tty = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open("/dev/tty")
        .map_err(|_| pane_refusal::CallerControllingTtyUnavailableCause::NoControllingTty)?;
    let mut device = 0 as libc::c_uint;
    let result = unsafe { libc::ioctl(controlling_tty.as_raw_fd(), libc::TIOCGDEV, &mut device) };
    if result != 0 {
        return Err(pane_refusal::CallerControllingTtyUnavailableCause::DeviceIdentityUnresolvable);
    }
    Ok(u64::from(device))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn caller_controlling_tty() -> Result<u64, pane_refusal::CallerControllingTtyUnavailableCause> {
    Err(pane_refusal::CallerControllingTtyUnavailableCause::PlatformUnsupported)
}

#[cfg(unix)]
fn pane_tty_device(path: &str) -> Result<u64, pane_refusal::ObservedPaneTtyUnavailableCause> {
    let pane_tty = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| pane_refusal::ObservedPaneTtyUnavailableCause::PaneTtyUnresolvable)?;
    let metadata = pane_tty
        .metadata()
        .map_err(|_| pane_refusal::ObservedPaneTtyUnavailableCause::PaneTtyUnresolvable)?;
    if !metadata.file_type().is_char_device() {
        return Err(pane_refusal::ObservedPaneTtyUnavailableCause::PaneTtyNotCharacterDevice);
    }
    Ok(metadata.rdev())
}

#[cfg(not(unix))]
fn pane_tty_device(_path: &str) -> Result<u64, pane_refusal::ObservedPaneTtyUnavailableCause> {
    Err(pane_refusal::ObservedPaneTtyUnavailableCause::PaneTtyUnresolvable)
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

/// 0.4.10+ mirror-session fix v2: managed-mode session name with a
/// per-launch nonce.
///
/// Format: `team-agent-leader-<provider>-<folder>-<hash>-<nonce>`.
/// `nonce` = `<pid_hex>-<epoch_nanos_hex>`. The pid distinguishes
/// concurrent launches; the epoch_nanos distinguishes sequential ones.
///
/// Each `team-agent <provider>` entry in the managed (non-tmux) path gets
/// its OWN session, matching the user expectation that `tmux new-session
/// + claude` is independent every time. The workspace-keyed prefix is
/// preserved so existing leader-session-anchored protections
/// (`LEADER_SESSION_PREFIX` matchers in shutdown/cli/mod.rs) still
/// classify these sessions as leader sessions.
///
/// External / attach paths keep the stable `leader_session_name` —
/// per-launch nonce would break `--attach-session <name>` reattach
/// semantics.
fn managed_leader_session_name(provider: Provider, workspace: &Path) -> SessionName {
    let resolved = resolve_workspace_for_hash(workspace);
    let folder_raw = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let folder = sanitize_session_folder(folder_raw);
    let hash = sha1_hex_prefix(resolved.to_string_lossy().as_bytes(), 8);
    let pid = std::process::id();
    let epoch_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    SessionName::new(format!(
        "{LEADER_SESSION_PREFIX}{}-{folder}-{hash}-{pid:x}-{epoch_nanos:x}",
        provider_wire(provider)
    ))
}

fn managed_leader_session_for_launch(provider: Provider, workspace: &Path) -> SessionName {
    let candidates = crate::transport_factory::tmux_workspace_transport(workspace)
        .list_targets()
        .unwrap_or_default()
        .into_iter()
        .map(|target| target.session.as_str().to_string());
    managed_leader_session_from_candidates(provider, workspace, candidates)
}

fn managed_leader_session_from_candidates(
    provider: Provider,
    workspace: &Path,
    candidates: impl IntoIterator<Item = String>,
) -> SessionName {
    let prefix = format!("{LEADER_SESSION_PREFIX}{}-", provider_wire(provider));
    let existing = candidates
        .into_iter()
        .filter(|name| name.starts_with(&prefix))
        .collect::<std::collections::BTreeSet<_>>();
    existing
        .into_iter()
        .next()
        .map(SessionName::new)
        .unwrap_or_else(|| managed_leader_session_name(provider, workspace))
}

fn start_argv(
    mode: LeaderStartMode,
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    session_name: Option<&SessionName>,
    leader_env: &BTreeMap<String, String>,
    managed_client_attach_mode: Option<ManagedClientAttachMode>,
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
                return Err(LeaderError::Start(
                    "managed leader session missing".to_string(),
                ));
            };
            let attach_mode = managed_client_attach_mode.ok_or_else(|| {
                LeaderError::Start("managed client attach mode missing".to_string())
            })?;
            managed_client_argv(workspace, session, provider, attach_mode)
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
        .skip(usize::from(
            provider_args.first().is_some_and(|arg| arg == "--"),
        ))
        .cloned()
}

// 0.3.28 Step 2: `managed_team_session_name` deleted. Both paths use the
// dedicated leader-session namespace; managed launches select a live prefix
// candidate before falling back to the nonce helper, while external/attach
// launches use the stable `leader_session_name`.

fn managed_client_argv(
    workspace: &Path,
    session: &SessionName,
    provider: Provider,
    attach_mode: ManagedClientAttachMode,
) -> Result<Vec<String>, LeaderError> {
    // 0.3.28 Step 2: leader window inside the dedicated leader session is
    // named after `provider_wire(provider)` (e.g. `claude`, `codex`, `fake`),
    // never the literal `leader`. Pre-0.3.28 this hardcoded `:leader`.
    let target = format!("{}:{}", session.as_str(), provider_wire(provider));
    let argv = match attach_mode {
        ManagedClientAttachMode::SwitchClient => vec![
            "tmux".to_string(),
            "switch-client".to_string(),
            "-t".to_string(),
            target,
        ],
        ManagedClientAttachMode::AttachSession => vec![
            "tmux".to_string(),
            "attach-session".to_string(),
            "-t".to_string(),
            target,
        ],
    };
    Ok(TmuxBackend::argv_for_workspace(workspace, &argv))
}

fn execute_managed_leader_plan(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    execute_managed_leader_plan_attempt(plan, workspace, true)
}

fn execute_managed_leader_plan_attempt(
    plan: &LeaderStartPlan,
    workspace: &Path,
    allow_attach_recovery: bool,
) -> Result<LeaderLaunchOutcome, LeaderError> {
    let Some(session) = plan.session_name.as_ref() else {
        return Err(LeaderError::Start(
            "managed leader session missing".to_string(),
        ));
    };
    let Some(window) = plan.leader_window.as_ref() else {
        return Err(LeaderError::Start(
            "managed leader window missing".to_string(),
        ));
    };
    // 0.5.x Phase 1d Batch 6: use the factory tmux channel helper
    // (thin wrapper over `TmuxBackend::for_workspace`) for
    // grep-visibility of every intentional tmux-only leader-launcher
    // site. Semantics unchanged; managed leader hosting stays
    // tmux-only per design §Batch 6.
    let transport = crate::transport_factory::tmux_workspace_transport(workspace);
    let session_existed_before = transport.has_session(session).map_err(|error| {
        LeaderError::Start(format!("managed leader session probe failed: {error}"))
    })?;
    let spawned = ensure_managed_leader_pane(
        &transport,
        session,
        window,
        plan,
        workspace,
        session_existed_before,
    )?;
    if let Err(error) = persist_managed_leader_binding(plan, workspace, &spawned) {
        cleanup_managed_leader_resources(
            &transport,
            session,
            &spawned,
            session_existed_before,
            workspace,
            "binding_persist_failed",
        );
        return Err(error);
    }
    spawn_managed_provider_startup_prompt_handler(
        plan.provider,
        workspace.to_path_buf(),
        spawned.pane_id.as_str().to_string(),
    );
    let process = match run_leader_argv(&plan.argv, &BTreeMap::new(), plan, workspace) {
        Ok(process) => process,
        Err(error) => {
            cleanup_managed_leader_resources(
                &transport,
                session,
                &spawned,
                session_existed_before,
                workspace,
                "launcher_spawn_failed",
            );
            return Err(error);
        }
    };
    let code = process.status.code();
    if !process.status.success() {
        cleanup_managed_leader_resources(
            &transport,
            session,
            &spawned,
            session_existed_before,
            workspace,
            "launcher_exit_failed",
        );
        let failure = LeaderError::Start(leader_launcher_failure(&process));
        if allow_attach_recovery {
            if let Ok(retry_plan) = managed_leader_attach_recovery_plan(plan, workspace) {
                return execute_managed_leader_plan_attempt(&retry_plan, workspace, false);
            }
        }
        return Err(failure);
    }
    if let Err(error) = ensure_managed_provider_live_after_attach(&transport, &spawned) {
        cleanup_managed_leader_resources(
            &transport,
            session,
            &spawned,
            session_existed_before,
            workspace,
            "provider_not_live_after_attach",
        );
        return Err(error);
    }
    Ok(LeaderLaunchOutcome {
        status: LeaderLaunchStatus::Exited,
        exit_code: code,
        session_name: plan.session_name.clone(),
        reason: None,
    })
}

fn managed_leader_attach_recovery_plan(
    plan: &LeaderStartPlan,
    workspace: &Path,
) -> Result<LeaderStartPlan, LeaderError> {
    let session = managed_leader_session_for_launch(plan.provider, workspace);
    let attach_mode = if plan.argv.iter().any(|arg| arg == "switch-client") {
        ManagedClientAttachMode::SwitchClient
    } else {
        ManagedClientAttachMode::AttachSession
    };
    let argv = managed_client_argv(workspace, &session, plan.provider, attach_mode)?;
    let mut retry_plan = plan.clone();
    retry_plan.session_name = Some(session);
    retry_plan.argv = argv;
    Ok(retry_plan)
}

fn ensure_managed_leader_pane(
    transport: &dyn Transport,
    session: &SessionName,
    window: &WindowName,
    plan: &LeaderStartPlan,
    workspace: &Path,
    session_existed_before: bool,
) -> Result<SpawnResult, LeaderError> {
    // A-22: a managed launch may reuse an existing provider leader session;
    // only a newly selected nonce session is absent before this probe.
    // Reusing the existing session adds one pane; a new session uses the
    // first-spawn path.
    if session_existed_before {
        // 0.4.x (CR C-1 + C-2): leader env_unset reuses the worker
        // provider_env_unsets (single source of truth) + spawn through the
        // leader shell wrapper so provider exit returns to a shell, not
        // `[exited]`.
        let env_unset = leader_env_unset_for_provider(plan.provider);
        let provider_label = provider_command_name(plan.provider);
        transport
            .spawn_into_with_leader_shell_wrapper(
                session,
                window,
                &plan.provider_argv,
                workspace,
                &plan.leader_env,
                &env_unset,
                provider_label,
            )
            .map_err(|error| LeaderError::Start(error.to_string()))
    } else {
        let env_unset = leader_env_unset_for_provider(plan.provider);
        let provider_label = provider_command_name(plan.provider);
        transport
            .spawn_first_with_leader_shell_wrapper(
                session,
                window,
                &plan.provider_argv,
                workspace,
                &plan.leader_env,
                &env_unset,
                provider_label,
            )
            .map_err(|error| LeaderError::Start(error.to_string()))
    }
}

fn cleanup_managed_leader_resources(
    transport: &dyn Transport,
    session: &SessionName,
    spawned: &SpawnResult,
    session_existed_before: bool,
    workspace: &Path,
    reason: &str,
) {
    let (action, result) = if session_existed_before {
        (
            "kill_pane",
            transport
                .kill_pane(&spawned.pane_id)
                .map(|_| "ok".to_string())
                .unwrap_or_else(|error| error.to_string()),
        )
    } else {
        (
            "kill_session",
            transport
                .kill_session(session)
                .map(|_| "ok".to_string())
                .unwrap_or_else(|error| error.to_string()),
        )
    };
    write_leader_startup_prompt_event(
        workspace,
        "leader.launcher.rollback",
        serde_json::json!({
            "reason": reason,
            "session": session.as_str(),
            "pane_id": spawned.pane_id.as_str(),
            "session_preexisted": session_existed_before,
            "action": action,
            "result": result,
        }),
    );
}

/// 0.4.x (CR C-1): leader provider env-unset list — SINGLE SOURCE OF TRUTH
/// reused from worker spawn (`profile_launch::provider_env_unsets`).
/// Audit grep guard: this function MUST be the only place in
/// `crates/team-agent/src/leader/` that constructs a Claude/Codex/Copilot
/// env-unset list. Any new code path that needs it must call this function
/// or the underlying `provider_env_unsets`. Use `AuthMode::Subscription` —
/// the leader is the user's interactive provider, never CompatibleApi/
/// OfficialApi which are worker-only auth modes today.
fn leader_env_unset_for_provider(provider: Provider) -> Vec<String> {
    crate::lifecycle::profile_launch::provider_env_unsets(
        provider,
        crate::model::enums::AuthMode::Subscription,
    )
    .into_iter()
    .collect()
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

/// 0.4.x (CR C-3 P0): leader provider health reconciliation. The default
/// `liveness()` check only proves the pane is ADDRESSABLE via tmux — it
/// returns `Live` even when the provider has exited and the wrapper shell
/// remains at its inert tail. This function distinguishes
/// `provider_alive` from `provider_exited` by:
///   1. Reading `pane_current_command` (tmux `#{pane_current_command}`).
///   2. If the current command matches the expected provider binary (or
///      one of its known aliases), report `Alive`.
///   3. If the current command is a shell tail process AND the pane
///      content contains the exit marker `[team-agent] <provider> exited`
///      (emitted by `leader_shell_wrapper_command`), report
///      `ProviderExited`.
///   4. Otherwise (Unknown shell, no marker), report `Alive` as the
///      conservative default — avoid false-positive exit alarms.
///
/// Note: when the leader shell wrapper is used (CR C-2), a provider exit
/// leaves the pane as an inert `/bin/sh -c` tail with the exit marker in
/// scrollback. Pre-wrapper code that hit `exec claude` would have left the
/// pane as `[exited]` and `liveness()` would have returned `Dead`. The new
/// failure mode requires this richer health check to surface
/// `leader_provider_exited` as a distinct status.
pub fn leader_provider_health(
    transport: &dyn Transport,
    pane_id: &PaneId,
    expected_provider_label: &str,
) -> LeaderProviderHealth {
    use crate::transport::{CaptureRange, PaneField, Target};
    let liveness = transport.liveness(pane_id).ok();
    if matches!(liveness, Some(PaneLiveness::Dead)) {
        return LeaderProviderHealth::Unreachable;
    }
    let target = Target::Pane(pane_id.clone());
    let current_command = transport
        .query(&target, PaneField::PaneCurrentCommand)
        .ok()
        .flatten()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if !current_command.is_empty()
        && (current_command == expected_provider_label
            || current_command.contains(expected_provider_label))
    {
        return LeaderProviderHealth::Alive;
    }
    // Current command is NOT the provider — likely fell back to shell.
    let is_shell = is_interactive_shell_basename(&current_command);
    if is_shell {
        // CR R6: marker text from single-source `leader_provider_exit_marker`
        // so the wrapper printf and the health-check substring cannot drift.
        let exit_marker_substr =
            crate::tmux_backend::leader_provider_exit_marker(expected_provider_label);
        if let Ok(cap) = transport.capture(&target, CaptureRange::Tail(200)) {
            if cap.text.contains(&exit_marker_substr) {
                return LeaderProviderHealth::ProviderExited;
            }
        }
    }
    // Conservative default — pane addressable, but couldn't positively
    // confirm provider exit. Treat as Alive.
    LeaderProviderHealth::Alive
}

/// Health status reported by [`leader_provider_health`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderProviderHealth {
    /// Provider process appears to be the pane's current command.
    Alive,
    /// Pane has fallen back to a shell with the exit marker present —
    /// provider has exited.
    ProviderExited,
    /// Pane is dead / unaddressable.
    Unreachable,
}

/// 0.4.x (CR R6 + R3): single-source shell-tail detection.
/// Used by:
///   - `leader_provider_health` to decide "pane is at the shell tail"
///   - shutdown logic to recognise a leader pane in shell-tail mode
///     as still owned by the leader (not stray).
///
/// Matches by basename (case-insensitive) — `pane_current_command` returns
/// the basename of the running binary. Conservative whitelist of POSIX +
/// common shell processes; missing entries here are false negatives
/// (shell tail looks like provider absent → health says Alive) which is the
/// safe default per the CR R6 conservative-Alive rule.
fn is_interactive_shell_basename(name: &str) -> bool {
    let trimmed = name.trim().to_ascii_lowercase();
    let basename = std::path::Path::new(&trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&trimmed);
    matches!(
        basename,
        "zsh"
            | "bash"
            | "sh"
            | "fish"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "ash"
            | "mksh"
            | "yash"
            | "elvish"
            | "nu"
            | "nushell"
            | "xonsh"
    )
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
    let attach_mode = if plan.argv.iter().any(|arg| arg == "switch-client") {
        "switch-client"
    } else {
        "attach-session"
    };
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
                "attach_mode": attach_mode,
                "tmux": std::env::var("TMUX").ok(),
            }),
        );
    }
    // Stage 3a/d (identity-boundary unified plan, architect direction
    // 2026-06-23): build the teams.<key> entry from the non-owner fields
    // of state (compact_team_state strips the `teams` key), then publish
    // the canonical owner record AFTER teams.insert so write_owner's
    // teams.<key>.{team_owner,leader_receiver,owner_epoch} is the final
    // word and doesn't get overwritten by the compacted entry.
    let entry = crate::state::projection::compact_team_state(&state);
    if let Some(obj) = state.as_object_mut() {
        let teams = obj
            .entry("teams".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(teams) = teams.as_object_mut() {
            teams.insert(identity.team_id.as_str().to_string(), entry);
        }
    }
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(owner_epoch);
    crate::state::ownership::write_owner(&mut state, identity.team_id.as_str(), record);
    crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::LeaderStartBinding {
            team_key: identity.team_id.as_str(),
            transport_kind: "managed",
        },
        &state,
    )?;
    Ok(())
}

fn persist_exec_provider_leader_binding(
    plan: &LeaderStartPlan,
    workspace: &Path,
    authority: &VerifiedAmbientPaneAuthority,
    is_external_leader: bool,
) -> Result<(), LeaderError> {
    let identity = plan
        .identity
        .as_ref()
        .ok_or_else(|| LeaderError::Start("exec provider leader identity missing".to_string()))?;
    let pane = authority.pane_id.as_str();
    let target = &authority.observed;
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
    let socket = authority.endpoint.as_str();
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
    if let Some(obj) = receiver.as_object_mut() {
        obj.insert(
            "session_name".to_string(),
            serde_json::json!(target.session.as_str()),
        );
        if let Some(window_name) = target.window_name.as_ref() {
            obj.insert(
                "window_name".to_string(),
                serde_json::json!(window_name.as_str()),
            );
        }
        obj.insert("tmux_socket".to_string(), serde_json::json!(socket));
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
        obj.insert(
            "session_name".to_string(),
            serde_json::json!(target.session.as_str()),
        );
        obj.insert("tmux_endpoint".to_string(), serde_json::json!(socket));
        obj.insert("tmux_socket".to_string(), serde_json::json!(socket));
        obj.insert(
            "is_external_leader".to_string(),
            serde_json::json!(is_external_leader),
        );
        obj.insert(
            "leader_client".to_string(),
            serde_json::json!({
                "diagnostic_only": true,
                "attach_mode": "exec-provider",
                "tmux": authority.tmux.as_str(),
            }),
        );
    }
    // Stage 3a/d (identity-boundary unified plan, architect direction
    // 2026-06-23): compact-then-write-owner ordering as in
    // persist_managed_leader_binding above. write_owner must be the final
    // write so the canonical teams.<key> owner record survives.
    let entry = crate::state::projection::compact_team_state(&state);
    if let Some(obj) = state.as_object_mut() {
        let teams = obj
            .entry("teams".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(teams) = teams.as_object_mut() {
            teams.insert(identity.team_id.as_str().to_string(), entry);
        }
    }
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(owner_epoch);
    crate::state::ownership::write_owner(&mut state, identity.team_id.as_str(), record);
    crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::LeaderStartBinding {
            team_key: identity.team_id.as_str(),
            transport_kind: "exec_provider",
        },
        &state,
    )?;
    Ok(())
}

/// 0.5.35 (`.team/artifacts/managed-leader-provider-reentry-locate.md` §5/§6):
/// three-way classification of an in-tmux ExecProvider invocation. Physical-
/// channel-first — never consults `pane_current_command` or gates on
/// `leader_session_uuid` equality (§6 forbids uuid-mismatch hard refusal).
enum ExecProviderBinding {
    ManagedReentry,
    DifferentPaneAlreadyBound,
    Unbound,
}

fn classify_exec_provider_binding(
    workspace: &Path,
    plan: &LeaderStartPlan,
    authority: &VerifiedAmbientPaneAuthority,
) -> Result<ExecProviderBinding, LeaderError> {
    let pane = authority.pane_id.as_str();
    let state = match crate::state::persist::load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => return Ok(ExecProviderBinding::Unbound),
    };
    let team_key = plan
        .identity
        .as_ref()
        .map(|identity| identity.team_id.as_str().to_string())
        .or_else(|| {
            state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    let Some(team_key) = team_key else {
        return Ok(ExecProviderBinding::Unbound);
    };
    let Some(receiver) = state
        .pointer(&format!("/teams/{team_key}/leader_receiver"))
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(ExecProviderBinding::Unbound);
    };
    let receiver_pane = receiver
        .get("pane_id")
        .or_else(|| receiver.get("pane"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if receiver_pane.is_empty() {
        return Ok(ExecProviderBinding::Unbound);
    }
    if receiver_pane != pane {
        return Ok(ExecProviderBinding::DifferentPaneAlreadyBound);
    }
    let leader_prefixed_session = authority
        .observed
        .session
        .as_str()
        .starts_with(LEADER_SESSION_PREFIX);
    if !leader_prefixed_session {
        return Ok(ExecProviderBinding::DifferentPaneAlreadyBound);
    }
    let recorded_socket = receiver
        .get("tmux_socket")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(recorded_socket) = recorded_socket {
        if authority.endpoint != recorded_socket {
            return Ok(ExecProviderBinding::DifferentPaneAlreadyBound);
        }
    }
    Ok(ExecProviderBinding::ManagedReentry)
}

/// 0.5.35: same-pane provider re-entry refresh path. Preserves canonical
/// team runtime identity (`session_name`/`team_dir`/`spec_path`/`agents`/
/// `tasks`) and epoch non-regressively; updates only diagnostic receiver
/// fields. Stage 3 strip preserved (root `team_owner`/`leader_receiver`/
/// `owner_epoch` are NOT reintroduced — writes go through
/// `state::ownership::write_owner` into the canonical teams.<key> slot).
fn refresh_managed_leader_provider_binding(
    plan: &LeaderStartPlan,
    workspace: &Path,
    authority: &VerifiedAmbientPaneAuthority,
) -> Result<(), LeaderError> {
    let identity = plan.identity.as_ref().ok_or_else(|| {
        LeaderError::Start("managed leader re-entry identity missing".to_string())
    })?;
    let pane = authority.pane_id.as_str();
    let target = &authority.observed;
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let team_key = identity.team_id.as_str();
    let existing_receiver = state
        .pointer(&format!("/teams/{team_key}/leader_receiver"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let existing_owner = crate::state::ownership::read_owner_value(&state, team_key)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let existing_epoch = state
        .pointer(&format!("/teams/{team_key}/owner_epoch"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            existing_owner
                .get("owner_epoch")
                .and_then(serde_json::Value::as_u64)
        })
        .or_else(|| {
            existing_receiver
                .get("owner_epoch")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    let provider = serde_json::to_value(plan.provider)?;
    let mut receiver = match existing_receiver.as_object() {
        Some(map) => serde_json::Value::Object(map.clone()),
        None => serde_json::json!({}),
    };
    if let Some(obj) = receiver.as_object_mut() {
        obj.insert("mode".to_string(), serde_json::json!("direct_tmux"));
        obj.insert("status".to_string(), serde_json::json!("attached"));
        obj.insert("provider".to_string(), provider.clone());
        obj.insert("pane_id".to_string(), serde_json::json!(pane));
        obj.insert("pane".to_string(), serde_json::json!(pane));
        obj.insert("attached_at".to_string(), serde_json::json!(now.clone()));
        obj.insert("discovery".to_string(), serde_json::json!("managed_leader"));
        obj.insert("owner_epoch".to_string(), serde_json::json!(existing_epoch));
        obj.entry("leader_session_uuid".to_string())
            .or_insert_with(|| serde_json::json!(identity.leader_session_uuid.as_str()));
        obj.insert(
            "session_name".to_string(),
            serde_json::json!(target.session.as_str()),
        );
        if let Some(window_name) = target.window_name.as_ref() {
            obj.insert(
                "window_name".to_string(),
                serde_json::json!(window_name.as_str()),
            );
        }
        if let Some(pane_pid) = target.pane_pid {
            obj.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
        }
    }
    let mut owner = match existing_owner.as_object() {
        Some(map) => serde_json::Value::Object(map.clone()),
        None => serde_json::json!({}),
    };
    if let Some(obj) = owner.as_object_mut() {
        obj.insert("pane_id".to_string(), serde_json::json!(pane));
        obj.insert("provider".to_string(), provider);
        obj.insert("owner_epoch".to_string(), serde_json::json!(existing_epoch));
        obj.entry("machine_fingerprint".to_string())
            .or_insert_with(|| serde_json::json!(identity.machine_fingerprint.as_str()));
        obj.entry("leader_session_uuid".to_string())
            .or_insert_with(|| serde_json::json!(identity.leader_session_uuid.as_str()));
        obj.entry("claimed_via".to_string())
            .or_insert_with(|| serde_json::json!("claim-leader"));
        obj.entry("os_user".to_string())
            .or_insert_with(|| serde_json::json!(identity.os_user.as_str()));
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("active_team_key".to_string(), serde_json::json!(team_key));
        obj.insert(
            "leader_client".to_string(),
            serde_json::json!({
                "diagnostic_only": true,
                "attach_mode": "exec-provider",
                "tmux": authority.tmux.as_str(),
                "reentry": true,
            }),
        );
    }
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(existing_epoch);
    crate::state::ownership::write_owner(&mut state, team_key, record);
    crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::LeaderStartBinding {
            team_key,
            transport_kind: "managed_reentry",
        },
        &state,
    )?;
    Ok(())
}

enum CurrentTmuxPaneInfoError {
    QueryFailed,
    PaneNotFound,
}

fn current_tmux_pane_info(
    pane_id: &PaneId,
    endpoint: &str,
) -> Result<crate::transport::PaneInfo, CurrentTmuxPaneInfoError> {
    crate::transport_factory::tmux_endpoint_transport(endpoint)
        .list_targets()
        .map_err(|_| CurrentTmuxPaneInfoError::QueryFailed)?
        .into_iter()
        .find(|target| target.pane_id == *pane_id)
        .ok_or(CurrentTmuxPaneInfoError::PaneNotFound)
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
            serde_json::json!(resolve_workspace_for_hash(workspace)
                .to_string_lossy()
                .to_string())
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
    crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::LeaderStartBinding {
            team_key: identity.team_id.as_str(),
            transport_kind: "external",
        },
        &state,
    )?;
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
) -> Result<LeaderProcessExit, LeaderError> {
    let Some(program) = argv.first() else {
        return Err(LeaderError::Start(
            "leader launch argv is empty".to_string(),
        ));
    };
    let diagnostics_path = launcher_diagnostics_path(workspace);
    if let Err(error) = write_launcher_diagnostics_header(&diagnostics_path, plan, argv) {
        return Err(LeaderError::Start(format!(
            "leader launcher diagnostics unavailable; startup_stage=diagnostics_create; launcher_diagnostics={}; error={error}",
            diagnostics_path.display()
        )));
    }
    // 0.4.x regression fix (env-leak scenario 1, in-tmux ExecProvider path):
    // the managed-tmux path got the provider env-unset block via the shell
    // wrapper (cb9c217), but the ExecProvider in-tmux path here is a direct
    // Command::spawn().wait() — it inherits the parent process's full env
    // including any CLAUDE_CODE_SESSION_ID / CLAUDE_CODE_CHILD_SESSION /
    // CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS / CLAUDE_CODE_ENTRYPOINT /
    // CLAUDE_CODE_EXECPATH / CLAUDECODE that the launching shell carries.
    // Apply the SAME env-unset list used by the managed path (CR R6 single
    // source: profile_launch::provider_env_unsets via
    // leader_env_unset_for_provider).
    let env_unset = leader_env_unset_for_provider(plan.provider);
    let mut command = Command::new(program);
    command.args(argv.iter().skip(1)).stdin(Stdio::inherit());
    let capture_stdout = !std::io::stdout().is_terminal();
    command.stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    let capture_stderr =
        plan.mode != LeaderStartMode::ExecProvider || !std::io::stderr().is_terminal();
    command.stderr(if capture_stderr {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    // 0.4.x order fix: env_remove MUST run AFTER envs(). The `env` map comes
    // from `merged_exec_env` which seeds with `std::env::vars().collect()` —
    // calling `.envs(env)` re-adds every inherited CLAUDE_CODE_* the launching
    // shell carried, overwriting any prior env_remove. By removing AFTER the
    // bulk envs() call, the final Command env table has the leak keys
    // structurally absent. Verified by the regression grep guard that the
    // env_remove call appears AFTER `command.envs(env)`.
    command.envs(env);
    for key in &env_unset {
        command.env_remove(key);
    }
    if plan.mode == LeaderStartMode::ManagedTmuxClient
        && argv.iter().any(|arg| arg == "attach-session")
    {
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
    }
    let mut child = command.spawn().map_err(|error| {
        LeaderError::Start(format!(
            "leader launcher spawn failed; startup_stage=spawn; launcher_diagnostics={}; error={error}",
            diagnostics_path.display()
        ))
    })?;
    append_launcher_diagnostics(&diagnostics_path, "startup_stage=spawned\n").map_err(|error| {
        LeaderError::Start(format!(
            "leader launcher diagnostics write failed; startup_stage=spawned; launcher_diagnostics={}; error={error}",
            diagnostics_path.display()
        ))
    })?;
    let stdout_reader = child.stdout.take().map(|mut child_stdout| {
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            let mut buf = [0_u8; 4096];
            loop {
                let Ok(read) = child_stdout.read(&mut buf) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                let _ = std::io::stdout().write_all(&buf[..read]);
                captured.extend_from_slice(&buf[..read]);
            }
            captured
        })
    });
    let stderr_reader = child.stderr.take().map(|mut child_stderr| {
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            let mut buf = [0_u8; 4096];
            loop {
                let Ok(read) = child_stderr.read(&mut buf) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                let _ = std::io::stderr().write_all(&buf[..read]);
                captured.extend_from_slice(&buf[..read]);
            }
            captured
        })
    });
    if plan.mode == LeaderStartMode::ExecProvider {
        spawn_exec_provider_startup_prompt_handler(plan.provider, workspace.to_path_buf());
    }
    let status = child.wait().map_err(LeaderError::Io)?;
    let stdout = match stdout_reader {
        Some(reader) => match reader.join() {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        },
        None => String::new(),
    };
    let stderr = match stderr_reader {
        Some(reader) => match reader.join() {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        },
        None => String::new(),
    };
    append_launcher_diagnostics(
        &diagnostics_path,
        &format!(
            "startup_stage=exited\nexit_code={}\nchild_stdout={}\nchild_stderr={}\n",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            crate::redaction::redact_external_text(&stdout),
            crate::redaction::redact_external_text(&stderr),
        ),
    )
    .map_err(|error| {
        LeaderError::Start(format!(
            "leader launcher diagnostics write failed; startup_stage=exited; launcher_diagnostics={}; error={error}",
            diagnostics_path.display()
        ))
    })?;
    Ok(LeaderProcessExit {
        status,
        stderr,
        diagnostics_path,
    })
}

fn launcher_diagnostics_path(workspace: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.6f");
    workspace.join(".team").join("logs").join(format!(
        "launcher-diagnostics-{stamp}-{}.log",
        std::process::id()
    ))
}

fn write_launcher_diagnostics_header(
    path: &Path,
    plan: &LeaderStartPlan,
    argv: &[String],
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        format!(
            "startup_stage=prepare\nmode={:?}\nprovider={}\nargv={}\n",
            plan.mode,
            provider_wire(plan.provider),
            crate::redaction::redact_external_text(&argv.join(" ")),
        ),
    )
}

fn append_launcher_diagnostics(path: &Path, text: &str) -> Result<(), std::io::Error> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(text.as_bytes())
}

const LEADER_STDERR_LIMIT: usize = 8192;

struct LeaderProcessExit {
    status: std::process::ExitStatus,
    stderr: String,
    diagnostics_path: PathBuf,
}

fn push_bounded_stderr(captured: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= LEADER_STDERR_LIMIT {
        captured.clear();
        captured.extend_from_slice(&chunk[chunk.len() - LEADER_STDERR_LIMIT..]);
        return;
    }
    let excess = captured
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(LEADER_STDERR_LIMIT);
    if excess > 0 {
        captured.drain(..excess);
    }
    captured.extend_from_slice(chunk);
}

fn leader_launcher_failure(process: &LeaderProcessExit) -> String {
    let status = process
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    format!(
        "{}; startup_stage=exited; launcher_diagnostics={}",
        format_launcher_failure(&status, process.status.code().is_none(), &process.stderr),
        process.diagnostics_path.display()
    )
}

fn format_launcher_failure(status: &str, signaled: bool, raw_stderr: &str) -> String {
    let stderr_class = classify_launcher_stderr(raw_stderr, signaled);
    let stderr = bounded_stderr_text(crate::redaction::redact_external_text(raw_stderr.trim()));
    let stderr = if stderr.is_empty() {
        "<empty>".to_string()
    } else {
        stderr
    };
    format!(
        "leader launcher exited with status {status}; stderr_class={stderr_class}; stderr_excerpt={stderr}"
    )
}

fn bounded_stderr_text(stderr: String) -> String {
    if stderr.len() <= LEADER_STDERR_LIMIT {
        return stderr;
    }
    let mut start = stderr.len() - LEADER_STDERR_LIMIT;
    while !stderr.is_char_boundary(start) {
        start += 1;
    }
    stderr[start..].to_string()
}

fn classify_launcher_stderr(stderr: &str, signaled: bool) -> &'static str {
    let stderr = stderr.to_ascii_lowercase();
    if stderr.contains("server exited") {
        "server_exited"
    } else if stderr.contains("no server running") || stderr.contains("failed to connect to server")
    {
        "no_server"
    } else if stderr.contains("not a terminal") {
        "not_a_terminal"
    } else if stderr.contains("can't find session") || stderr.contains("no sessions") {
        "target_missing"
    } else if signaled {
        "signal"
    } else {
        "non_zero"
    }
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
        // Phase 1d Batch 6: factory tmux workspace helper for
        // grep-visibility. Semantics unchanged.
        let transport = crate::transport_factory::tmux_workspace_transport(&workspace);
        let _ = handle_exec_provider_startup_prompts(
            provider, &workspace, &pane_id, &transport, 30, 0.5,
        );
    });
}

fn tmux_transport_for_current_pane() -> TmuxBackend {
    // Phase 1d Batch 6: factory tmux channel helpers for
    // grep-visibility. Semantics unchanged; this is intentionally
    // tmux-only (caller pane = tmux `$TMUX` endpoint or default socket,
    // MUST-12 anchor).
    crate::tmux_backend::socket_name_from_tmux_env()
        .map(|endpoint| crate::transport_factory::tmux_endpoint_transport(&endpoint))
        .unwrap_or_else(crate::transport_factory::tmux_default_transport)
}

fn handle_exec_provider_startup_prompts(
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
    // Phase 1d Batch 6: factory tmux workspace helper for
    // grep-visibility. Tmux-only anchor (managed leader session lookup).
    crate::transport_factory::tmux_workspace_transport(workspace)
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
        ensure_managed_provider_live_after_attach, execute_leader_plan_after_ambient_authority,
        format_launcher_failure, handle_exec_provider_startup_prompts,
        is_interactive_shell_basename, push_bounded_stderr, shlex_quote,
        VerifiedAmbientPaneAuthority, LEADER_STDERR_LIMIT,
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
        let workspace =
            std::env::temp_dir().join(format!("ta-external-pre-exec-{}", std::process::id()));
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

        let outcome = execute_leader_plan_after_ambient_authority(&plan, &workspace, None)
            .expect("external marker must be present before provider argv runs");

        assert_eq!(outcome.status, crate::leader::LeaderLaunchStatus::Exited);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(state["is_external_leader"], serde_json::json!(true));
        assert_eq!(
            state["teams"]["current"]["is_external_leader"],
            serde_json::json!(true)
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    #[serial_test::serial(env)]
    fn default_exec_provider_persists_current_pane_binding_before_provider_exec() {
        let workspace =
            std::env::temp_dir().join(format!("ta-current-pane-pre-exec-{}", std::process::id()));
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

        let authority = VerifiedAmbientPaneAuthority {
            pane_id: PaneId::new("%77"),
            observed: PaneInfo {
                pane_id: PaneId::new("%77"),
                session: SessionName::new("team-agent-leader-current"),
                window_index: Some(0),
                window_name: Some(WindowName::new("codex")),
                pane_index: Some(0),
                tty: Some("/dev/ttys077".to_string()),
                current_command: Some("codex".to_string()),
                current_path: Some(workspace.clone()),
                active: true,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
            endpoint: "/private/tmp/tmux-501/default".to_string(),
            tmux: "/private/tmp/tmux-501/default,123,0".to_string(),
        };
        let outcome =
            execute_leader_plan_after_ambient_authority(&plan, &workspace, Some(&authority))
                .expect("current pane binding must be present before provider argv runs");

        assert_eq!(outcome.status, crate::leader::LeaderLaunchStatus::Exited);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(state["is_external_leader"], serde_json::json!(false));
        // Stage 3d: canonical owner/receiver at teams.<team_key>.
        assert_eq!(
            state["teams"]["current"]["leader_receiver"]["pane_id"],
            serde_json::json!("%77")
        );
        assert_eq!(
            state["teams"]["current"]["leader_receiver"]["tmux_socket"],
            serde_json::json!("/private/tmp/tmux-501/default")
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

    #[test]
    fn interactive_shell_basename_recognizes_supported_shells() {
        let shells = [
            "zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "ash", "mksh", "yash",
            "elvish", "nu", "nushell", "xonsh",
        ];
        for shell in shells {
            assert!(is_interactive_shell_basename(shell));
            assert!(is_interactive_shell_basename(&shell.to_uppercase()));
        }
        for provider in ["claude", "codex", "copilot", "gemini"] {
            assert!(!is_interactive_shell_basename(provider));
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // 0.4.10+ mirror-session fix v2 (option B: independent session per launch).
    //
    // The managed selector reuses a live provider leader prefix when present;
    // its no-candidate fallback appends a per-launch nonce.
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn managed_leader_session_name_carries_leader_prefix() {
        // Protection invariant: every session name returned by the
        // managed path must still match LEADER_SESSION_PREFIX so the
        // shutdown / cli/mod.rs prefix matchers still classify it as a
        // leader session.
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_mgr_prefix_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workspace);
        let name = super::managed_leader_session_name(Provider::ClaudeCode, &workspace);
        assert!(
            name.as_str()
                .starts_with(super::super::LEADER_SESSION_PREFIX),
            "managed session name must carry LEADER_SESSION_PREFIX (`{}`); \
             got `{}`",
            super::super::LEADER_SESSION_PREFIX,
            name.as_str()
        );
        assert!(
            name.as_str().contains("claude_code"),
            "managed session name must include provider wire; got `{}`",
            name.as_str()
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn managed_leader_session_name_is_unique_per_call() {
        // Two consecutive calls must return DIFFERENT names — the
        // per-launch nonce defeats the mirror-session bug.
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_mgr_unique_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workspace);
        let a = super::managed_leader_session_name(Provider::ClaudeCode, &workspace);
        // Sleep > 1ns to ensure epoch_nanos advances even on coarse clocks.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = super::managed_leader_session_name(Provider::ClaudeCode, &workspace);
        assert_ne!(
            a.as_str(),
            b.as_str(),
            "two managed launches in the same workspace must get \
             DIFFERENT session names (mirror-session fix v2); got \
             `{}` vs `{}`",
            a.as_str(),
            b.as_str()
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn leader_session_name_is_stable_across_calls() {
        // External / attach paths must keep the stable workspace-keyed
        // name — `--attach-session <name>` reattach would break if the
        // name varied per call.
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_lsn_stable_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workspace);
        let a = super::leader_session_name(Provider::ClaudeCode, &workspace);
        let b = super::leader_session_name(Provider::ClaudeCode, &workspace);
        assert_eq!(
            a.as_str(),
            b.as_str(),
            "leader_session_name must be deterministic per workspace \
             (external/attach paths depend on stability); got `{}` vs `{}`",
            a.as_str(),
            b.as_str()
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    // Tombstone for `managed_path_uses_nonce_session_name_grep_guard`: A-22
    // replaced that implementation-shape assertion with the contract guard
    // below. Keep the old name here for audit/search provenance.
    #[test]
    fn a22_launch_session_reuse_contract() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let start_rs = manifest.join("src").join("leader").join("start.rs");
        let contents = std::fs::read_to_string(&start_rs).expect("read leader/start.rs");
        let start = contents
            .find("fn leader_start_plan_with_ambient_authority(")
            .expect("leader_start_plan_with_ambient_authority must exist");
        let end = contents[start + 1..]
            .find("\nfn ")
            .map(|offset| start + 1 + offset)
            .unwrap_or(contents.len());
        let body = &contents[start..end];
        assert!(
            body.contains("managed_leader_session_for_launch(provider, workspace)"),
            "managed launch must select from the A-22 candidate contract; body excerpt: {body}"
        );
        assert!(
            body.contains("leader_session_name(provider, workspace)"),
            "leader_start_plan must keep leader_session_name on the \
             external/attach paths; body excerpt: {body}"
        );
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_a22_contract_{}", std::process::id()));
        let reused = super::managed_leader_session_from_candidates(
            Provider::ClaudeCode,
            &workspace,
            vec![
                "team-worker-not-a-leader".to_string(),
                "team-agent-leader-claude_code-z".to_string(),
                "team-agent-leader-codex-other".to_string(),
                "team-agent-leader-claude_code-a".to_string(),
            ],
        );
        assert_eq!(
            reused.as_str(),
            "team-agent-leader-claude_code-a",
            "managed launch must deterministically reuse a matching leader prefix session"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn a22_launch_session_without_candidate_gets_nonce() {
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_a22_nonce_{}", std::process::id()));
        let fresh = super::managed_leader_session_from_candidates(
            Provider::ClaudeCode,
            &workspace,
            std::iter::empty(),
        );
        assert!(
            fresh.as_str().starts_with("team-agent-leader-claude_code-"),
            "no candidate must create a leader-prefixed managed session: {}",
            fresh.as_str()
        );
        assert_ne!(
            fresh.as_str(),
            super::leader_session_name(Provider::ClaudeCode, &workspace).as_str(),
            "managed fallback must carry a per-launch nonce"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn a22_external_attach_session_name_remains_stable() {
        let workspace =
            std::env::temp_dir().join(format!("ta_rs_a22_external_{}", std::process::id()));
        let stable = super::leader_session_name(Provider::ClaudeCode, &workspace);
        assert_eq!(
            stable.as_str(),
            super::leader_session_name(Provider::ClaudeCode, &workspace).as_str(),
            "external/attach paths must keep the stable workspace-keyed session name"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn launcher_failure_redacts_credentials_before_persistence() {
        let failure = format_launcher_failure(
            "1",
            false,
            "open terminal failed: not a terminal API_TOKEN=secret-value \
             Authorization: Bearer abcdefghijklmnopqrstuvwxyz",
        );

        assert!(failure.contains("stderr_class=not_a_terminal"));
        assert!(failure.contains("[REDACTED]"));
        assert!(!failure.contains("secret-value"));
        assert!(!failure.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn launcher_stderr_capture_is_bounded_to_tail() {
        let mut captured = vec![b'a'; LEADER_STDERR_LIMIT - 2];
        push_bounded_stderr(&mut captured, b"XYZ");

        assert_eq!(captured.len(), LEADER_STDERR_LIMIT);
        assert!(captured.ends_with(b"XYZ"));
    }
}
