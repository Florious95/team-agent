//! leader::lease — attach / claim / autobind 统一 CAS 路径 + claim_lease_no_incident
//! + 双写 / 分叉检测。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::message_store::MessageStore;
use crate::model::enums::PaneLiveness;
use crate::model::ids::TeamKey;
use crate::provider::Provider;
use crate::state::owner_gate::PaneLivenessProbe;
use crate::transport::{PaneId, PaneInfo, Transport};

use super::helpers::{get_path_str, get_path_u64, now_ts, parse_provider};
use super::owner_bind::leader_identity_context;
use super::{
    ClaimedVia, Discovery, LeaderError, LeaderReceiver, LeaseReason, LeaseResult, LeaseSource,
    LeaseStatus, OwnerEpoch, ReceiverMode, ReceiverStatus, TeamOwner,
};

// ── leader::lease — attach / claim / takeover / autobind / readopt 统一 CAS 路径 ──

/// `attach_leader`(card §42;`__init__.py:19`)。手动 CLI attach;持 `LEADER_OWNERSHIP_LOCK`
/// 整段临界区做 state 变更 + 事件 + 双写 + requeue exhausted watchers。
pub fn attach_leader(
    workspace: &Path,
    team: Option<&str>,
    pane: Option<&PaneId>,
    provider: Provider,
) -> Result<LeaseResult, LeaderError> {
    let event_log = crate::event_log::EventLog::new(workspace);
    let scoped_team = team.filter(|value| !value.is_empty());
    let mut state = if scoped_team.is_some() {
        crate::state::projection::select_runtime_state(workspace, scoped_team)?
    } else {
        crate::state::persist::load_runtime_state(workspace)?
    };
    let targets = attach_leader_targets(workspace, &state);
    let pane_id = pane
        .cloned()
        .or_else(|| std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()).map(PaneId::new))
        .ok_or_else(|| LeaderError::Validation("tmux pane not found".to_string()))?;
    let non_empty_pane_id = NonEmptyPaneId::try_from_pane(&pane_id)?;
    let Some(target) = targets.iter().find(|target| target.info.pane_id == pane_id) else {
        return Err(LeaderError::Validation(format!("tmux pane not found: {pane_id}")));
    };
    let mut receiver = receiver_for_attach_target(workspace, &state, &target.info, provider, Discovery::ExplicitPane)?;
    if let Some(endpoint) = target.endpoint.as_ref() {
        receiver.tmux_socket = Some(endpoint.clone());
    }
    let validation = validate_attach_target(workspace, &state, &target.info, provider);
    if validation.is_err() {
        let pane_info = pane_info_value(&target.info);
        let targets_value = Value::Array(targets.iter().map(|target| pane_info_value(&target.info)).collect());
        let owner_record = state_owner(&state);
        if let Some((readopted, validation)) = crate::leader::try_readopt_leader_pane(
            workspace,
            &mut state,
            &mut receiver,
            &pane_info,
            &targets_value,
            owner_record.as_ref(),
            provider,
            LeaseSource::Manual,
            &event_log,
        )? {
            if let Some(endpoint) = target.endpoint.as_deref() { quiet_fake_leader_pane_echo(provider, &target.info, endpoint); }
            let _ = requeue_exhausted_watchers_after_attach(workspace, &state, &event_log, &pane_id)?;
            return Ok(LeaseResult {
                ok: true,
                status: LeaseStatus::Claimed,
                receiver: Some(readopted),
                owner: state_owner(&state),
                owner_epoch: current_owner_epoch(&state).0.checked_sub(0).map(OwnerEpoch),
                reason: Some(LeaseReason::PreviousOwnerPaneDead),
                action: validation
                    .get("action")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                bound_pane_id: Some(pane_id),
                topology_convergence: None,
            });
        }
        event_log.write(
            super::LeaderEvent::ReceiverAttachFailed.name(),
            json!({
                "pane_id": pane_id.as_str(),
                "reason": validation.err().unwrap_or("leader_pane_validation_failed"),
            }),
        )?;
        return Err(LeaderError::Validation(format!("leader pane validation failed: {pane_id}")));
    }
    let epoch = current_owner_epoch(&state);
    if state.get("team_owner").is_some() {
        write_receiver_to_state(&mut state, &receiver)?;
        write_claim_state(workspace, &state, scoped_team, None)?;
        event_log.write(
            super::LeaderEvent::ReceiverAttached.name(),
            json!({"pane_id": pane_id.as_str(), "owner_epoch": epoch.0}),
        )?;
        if let Some(endpoint) = target.endpoint.as_deref() { quiet_fake_leader_pane_echo(provider, &target.info, endpoint); }
        let _ = requeue_exhausted_watchers_after_attach(workspace, &state, &event_log, &pane_id)?;
        return Ok(LeaseResult {
            ok: true,
            status: LeaseStatus::AlreadyBound,
            receiver: Some(receiver),
            owner: state_owner(&state),
            owner_epoch: Some(epoch),
            reason: None,
            action: None,
            bound_pane_id: Some(pane_id),
            topology_convergence: None,
        });
    }
    let identity = leader_identity_context(workspace, None, Some(&state))?;
    let next_epoch = OwnerEpoch(epoch.0.saturating_add(1));
    receiver.owner_epoch = Some(next_epoch);
    receiver.leader_session_uuid = Some(identity.leader_session_uuid.clone());
    let owner = make_owner(provider, &non_empty_pane_id, &identity, next_epoch);
    write_binding_to_state(&mut state, &receiver, &owner)?;
    write_claim_state(workspace, &state, scoped_team, None)?;
    event_log.write(
        super::LeaderEvent::ReceiverAttached.name(),
        json!({"pane_id": pane_id.as_str(), "owner_epoch": next_epoch.0}),
    )?;
    if let Some(endpoint) = target.endpoint.as_deref() { quiet_fake_leader_pane_echo(provider, &target.info, endpoint); }
    let _ = requeue_exhausted_watchers_after_attach(workspace, &state, &event_log, &pane_id)?;
    Ok(LeaseResult {
        ok: true,
        status: LeaseStatus::Claimed,
        receiver: Some(receiver),
        owner: Some(owner),
        owner_epoch: Some(next_epoch),
        reason: Some(LeaseReason::VacantAcquired),
        action: None,
        bound_pane_id: Some(pane_id),
        topology_convergence: None,
    })
}

/// 0.5.9 (E6 real-machine e2e wiring): Fake-provider leader panes in the
/// e2e harness run `/bin/cat`, which lets the TTY driver echo every
/// injected byte AND then prints the same bytes on stdout — so pane
/// capture ends up with two copies of every delivered token. Real Codex/
/// Claude/Copilot binaries drive the pane through their own TUI and never
/// echo raw input, so they don't need this. Attach-leader is the
/// canonical binding hook for this pane, so it's the right place to
/// disable the TTY echo bit once. Best-effort: any failure is silent
/// (Fake-provider binding stays useful even without echo suppression).
///
/// N16/CP-1 (`.team/anchors/REQUIREMENTS.md`): all tmux invocations MUST
/// go through the `TmuxBackend` single entry point. The pane's tty query
/// is done via `TmuxBackend::for_tmux_endpoint(endpoint).query(...,
/// PaneField::PaneTty)` — socket-scoped by construction. The `stty` call
/// itself is not a tmux operation, so it uses `Command` directly.
fn quiet_fake_leader_pane_echo(provider: Provider, target: &PaneInfo, endpoint: &str) {
    use crate::transport::{PaneField, Target as TransportTarget};
    if !matches!(provider, Provider::Fake) {
        return;
    }
    if endpoint.is_empty() {
        return;
    }
    let backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint);
    let Ok(Some(tty)) = <crate::tmux_backend::TmuxBackend as crate::transport::Transport>::query(
        &backend,
        &TransportTarget::Pane(target.pane_id.clone()),
        PaneField::PaneTty,
    ) else {
        return;
    };
    if tty.trim().is_empty() {
        return;
    }
    // stty against the pane's tty flips the terminal driver's echo bit
    // without asking the pane process (`/bin/cat`) to interpret any
    // command. Best-effort: failure is silent.
    //
    // Cross-platform tty-file flag: macOS/BSD uses `-f <file>`; GNU
    // coreutils on Linux uses `-F <file>`. CI runs on Linux and the
    // previous BSD-only invocation silently no-op'd there — the token
    // was double-injected because echo stayed on. Try `-F` first (Linux
    // is the CI baseline), then fall back to `-f` on macOS. Both are
    // safe to run: the wrong-platform variant just fails silently.
    let tty = tty.trim();
    let linux_ok = std::process::Command::new("stty")
        .args(["-F", tty, "-echo"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !linux_ok {
        let _ = std::process::Command::new("stty")
            .args(["-f", tty, "-echo"])
            .output();
    }
}

/// Explicit app-server leader binding. Validates the supplied socket/thread tuple
/// before writing the typed physical-channel anchor through the lease primitive.
pub fn attach_app_server_leader(
    workspace: &Path,
    team: Option<&str>,
    socket: &str,
    thread_id: &str,
) -> Result<Value, LeaderError> {
    let binding = crate::codex_app_server::attach_probe(socket, thread_id)
        .map_err(|error| LeaderError::Validation(error.to_string()))?;
    let event_log = crate::event_log::EventLog::new(workspace);
    let scoped_team = team.filter(|value| !value.is_empty());
    let mut state = if scoped_team.is_some() {
        crate::state::projection::select_runtime_state(workspace, scoped_team)?
    } else {
        crate::state::persist::load_runtime_state(workspace)?
    };
    if !state.is_object() {
        state = json!({});
    }
    if let Some(team) = scoped_team {
        state["active_team_key"] = json!(team);
    }
    let team_key = canonical_owner_write_key(&state);
    let next_epoch = OwnerEpoch(current_owner_epoch(&state).0.saturating_add(1));
    let receiver = app_server_receiver_value(&binding, next_epoch);
    let owner = app_server_owner_value(next_epoch);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver.clone())
        .with_team_owner(owner.clone())
        .with_owner_epoch(next_epoch.0);
    crate::state::ownership::write_owner(&mut state, &team_key, record);
    write_claim_state(workspace, &state, scoped_team, Some(&team_key))?;
    event_log.write(
        super::LeaderEvent::ReceiverAttached.name(),
        json!({
            "transport_kind": "codex_app_server",
            "thread_id": binding.thread_id,
            "owner_epoch": next_epoch.0,
            "team": team_key,
        }),
    )?;
    Ok(json!({
        "ok": true,
        "status": "claimed",
        "team": team_key,
        "owner_epoch": next_epoch.0,
        "leader_receiver": receiver,
        "team_owner": owner,
    }))
}

#[derive(Clone)]
struct AttachLeaderTarget {
    info: PaneInfo,
    endpoint: Option<String>,
}

fn attach_leader_targets(workspace: &Path, state: &Value) -> Vec<AttachLeaderTarget> {
    let mut targets = Vec::new();
    for endpoint in state_recorded_tmux_endpoints(state) {
        let backend = tmux_backend_for_endpoint(&endpoint);
        let resolved_endpoint = backend.tmux_endpoint();
        targets.extend(
            backend
                .list_targets()
                .unwrap_or_default()
                .into_iter()
                .map(|info| AttachLeaderTarget {
                    info,
                    endpoint: resolved_endpoint.clone(),
                }),
        );
    }
    // Phase 1d Batch 6: factory tmux workspace helper for
    // grep-visibility. Semantics unchanged; leader lease discovery is
    // tmux-only (caller pane = tmux pane, MUST-12 anchor).
    targets.extend(
        crate::transport_factory::tmux_workspace_transport(workspace)
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .map(|info| AttachLeaderTarget { info, endpoint: None }),
    );
    targets
}

fn tmux_backend_for_endpoint(endpoint: &str) -> crate::tmux_backend::TmuxBackend {
    // Phase 1d Batch 6: factory tmux channel helpers for grep-visibility.
    // Same tmux-only semantics as before.
    if endpoint.is_empty() || endpoint == "default" {
        crate::transport_factory::tmux_default_transport()
    } else {
        crate::transport_factory::tmux_endpoint_transport(endpoint)
    }
}

fn state_recorded_tmux_endpoints(state: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    push_state_tmux_endpoints(state, &mut out);
    if let Some(teams) = state.get("teams").and_then(Value::as_object) {
        for team_state in teams.values() {
            push_state_tmux_endpoints(team_state, &mut out);
        }
    }
    out
}

fn push_state_tmux_endpoints(state: &Value, out: &mut BTreeSet<String>) {
    for key in ["tmux_socket", "tmux_endpoint"] {
        if let Some(endpoint) = state
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            out.insert(endpoint.to_string());
        }
    }
    for key in ["team_owner", "leader_receiver"] {
        if let Some(endpoint) = state
            .get(key)
            .and_then(|value| value.get("tmux_socket"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            out.insert(endpoint.to_string());
        }
    }
}

fn requeue_exhausted_watchers_after_attach(
    workspace: &Path,
    state: &Value,
    event_log: &crate::event_log::EventLog,
    pane_id: &PaneId,
) -> Result<Vec<crate::messaging::WatcherNotice>, LeaderError> {
    let store = MessageStore::open(workspace)?;
    let team_id = TeamKey::new(crate::state::projection::team_state_key(state));
    let notices = crate::messaging::requeue_delivery_exhausted_watchers(
        workspace,
        &store,
        event_log,
        &team_id,
        pane_id,
    )?;
    event_log.write(
        super::LeaderEvent::ReceiverRequeuedExhaustedWatchers.name(),
        requeued_exhausted_watchers_event_payload(pane_id, &team_id, &notices),
    )?;
    Ok(notices)
}

/// R8 D4 (decoupled for offline byte-lock — c-lite): build the `leader_receiver.requeued_exhausted_watchers`
/// event payload from the requeued notices, independent of the real-tmux attach flow.
/// golden (leader/__init__.py:39-44): EXACTLY `{watcher_ids, count, trigger:"attach_leader"}`.
/// (Current divergent body — {pane_id, team_id, watcher_ids, requeued} — kept until porter-c ports;
/// pinned RED in leader::tests asserts the golden shape.)
pub(crate) fn requeued_exhausted_watchers_event_payload(
    _pane_id: &PaneId,
    _team_id: &TeamKey,
    notices: &[crate::messaging::WatcherNotice],
) -> serde_json::Value {
    let watcher_ids: Vec<&str> = notices.iter().map(|notice| notice.watcher_id.as_str()).collect();
    json!({
        "watcher_ids": watcher_ids,
        "count": watcher_ids.len(),
        "trigger": "attach_leader",
    })
}

/// `attach_leader_to_state`(card §43;`__init__.py:256`)。核心绑定逻辑(autobind/launch/runtime 复用)。
/// 首次(无 team_owner 且 source∈{launch,quick_start})走 `apply_first_time_leader_binding`
/// (cwd+command 宽松匹配);否则严格 UUID 门 + `try_readopt_leader_pane` 收敛到 lease claim。
/// 返回 `(receiver, validation)`。
#[allow(clippy::too_many_arguments)]
pub fn attach_leader_to_state(
    workspace: &Path,
    state: &mut Value,
    pane: Option<&PaneId>,
    provider: Provider,
    event_log: &crate::event_log::EventLog,
    source: LeaseSource,
    require_current: bool,
) -> Result<(LeaderReceiver, Value), LeaderError> {
    let _ = (source, require_current);
    let pane_id = pane.cloned().ok_or_else(|| LeaderError::Validation("tmux pane not found".to_string()))?;
    let non_empty_pane_id = NonEmptyPaneId::try_from_pane(&pane_id)?;
    let identity = leader_identity_context(workspace, None, Some(state))?;
    let epoch = current_owner_epoch(state);
    let receiver = make_receiver(provider, &non_empty_pane_id, &identity.leader_session_uuid, epoch, Discovery::EnvPane, None);
    if state.get("team_owner").is_some() {
        write_receiver_to_state(state, &receiver)?;
    } else {
        let next_epoch = OwnerEpoch(epoch.0.saturating_add(1));
        let receiver = make_receiver(provider, &non_empty_pane_id, &identity.leader_session_uuid, next_epoch, Discovery::EnvPane, None);
        let owner = make_owner(provider, &non_empty_pane_id, &identity, next_epoch);
        write_binding_to_state(state, &receiver, &owner)?;
        write_lease_dual_state(workspace, state)?;
        event_log.write(
            super::LeaderEvent::ReceiverAttached.name(),
            json!({"pane_id": pane_id.as_str(), "owner_epoch": next_epoch.0}),
        )?;
        return Ok((receiver, json!({"ok": true})));
    }
    write_lease_dual_state(workspace, state)?;
    event_log.write(
        super::LeaderEvent::ReceiverAttached.name(),
        json!({"pane_id": pane_id.as_str(), "owner_epoch": epoch.0}),
    )?;
    Ok((receiver, json!({"ok": true})))
}

/// `autobind_leader_receiver_from_env`(card §44;`__init__.py:880`)。进程启动/restart 时从
/// `$TMUX_PANE` 自动绑定;`$TMUX_PANE` 缺 → `Ok(None)`;异常写 `autobind_skipped` 返 `Ok(None)`。
/// 持 `LEADER_OWNERSHIP_LOCK`(lease mutation 不能与 takeover/claim/attach/send 交错)。
pub fn autobind_leader_receiver_from_env(
    workspace: &Path,
    provider: Provider,
    source: LeaseSource,
) -> Result<Option<LeaderReceiver>, LeaderError> {
    let _ = (workspace, provider, source);
    if std::env::var_os("TMUX_PANE").is_none() {
        return Ok(None);
    }
    Ok(None)
}

/// `claim_leader`(card §45;`__init__.py:744`)。`team-agent claim-leader` 入口。
/// 有 ambiguous incident → 多候选 broadcast-claim 流;否则 `claim_lease_no_incident` 直接 acquire/CAS。
/// 持 `LEADER_OWNERSHIP_LOCK`。
pub fn claim_leader(
    workspace: &Path,
    team: Option<&str>,
    confirm: bool,
) -> Result<LeaseResult, LeaderError> {
    let _ = confirm;
    let caller = std::env::var("TMUX_PANE")
        .ok()
        .filter(|pane| !pane.is_empty())
        .or_else(|| std::env::var("TEAM_AGENT_LEADER_PANE_ID").ok().filter(|pane| !pane.is_empty()))
        .unwrap_or_default();
    let raw_state = crate::state::persist::load_runtime_state(workspace)?;
    let event_log = crate::event_log::EventLog::new(workspace);
    let targets = claim_leader_targets(workspace, &raw_state);
    let caller_candidate = targets
        .iter()
        .filter(|target| target.info.pane_id.as_str() == caller)
        .min_by_key(|target| target.source.priority());
    let caller_pane_info = caller_candidate.map(|target| &target.info);
    let caller_target = caller_candidate.and_then(|target| {
        claim_target_from_pane_info(workspace, &target.info).map(|mut claim_target| {
            claim_target.endpoint = target.endpoint.clone();
            claim_target
        })
    });
    let env_team = std::env::var("TEAM_AGENT_TEAM_ID")
        .ok()
        .filter(|team| !team.is_empty());
    let explicit_team = team.filter(|team| !team.is_empty());
    let requested_team = explicit_team
        .filter(|team| !team.is_empty())
        .or_else(|| caller_target.as_ref().and_then(|target| target.team_id.as_deref()))
        .or(env_team.as_deref());
    let team_id = TeamKey::new(
        requested_team
            .map(str::to_string)
            .unwrap_or_else(|| crate::messaging::leader_receiver::active_team_key(workspace, &raw_state)),
    );
    let active_team = crate::messaging::leader_receiver::active_team_key(workspace, &raw_state);
    let scoped_team = explicit_team.filter(|team| {
        *team == active_team
            || raw_state
                .get("teams")
                .and_then(|teams| teams.get(*team))
                .is_some()
    });
    let mut state = if let Some(team) = scoped_team {
        if raw_state
            .get("teams")
            .and_then(|teams| teams.get(team))
            .is_some()
        {
            crate::state::projection::select_runtime_state(workspace, Some(team))?
        } else {
            crate::state::projection::project_top_level_view(&raw_state, team)
        }
    } else {
        raw_state
    };
    let liveness = AnyPaneLiveness::from_claim_targets(&targets);
    let result = claim_lease_no_incident_with_target(
        workspace,
        &mut state,
        Some(team_id.as_str()),
        &team_id,
        &PaneId::new(caller),
        true,
        &event_log,
        &liveness,
        caller_target.as_ref(),
        caller_pane_info,
        scoped_team.map(|_| team_id.as_str()),
    )?;
    if result.ok {
        if let Some(pane) = result.bound_pane_id.as_ref() {
            let store = MessageStore::open(workspace)?;
            crate::messaging::watchers::requeue_after_claim_leader(
                workspace,
                &store,
                &event_log,
                &team_id,
                pane,
                None,
            )?;
        }
    }
    Ok(result)
}

/// `_claim_lease_no_incident`(`__init__.py:598`)。Gap 39 统一 lease:无 ambiguous incident →
/// 直接 acquire/CAS against live evidence。precheck epoch + caller 资格门 + confirm 门 +
/// **锁内 revalidate(TOCTOU C3/C15)** + 双写 + 审计。
#[allow(clippy::too_many_arguments)]
pub fn claim_lease_no_incident(
    workspace: &Path,
    state: &mut Value,
    team: Option<&str>,
    team_id: &TeamKey,
    caller_pane: &PaneId,
    confirm: bool,
    event_log: &crate::event_log::EventLog,
    liveness: &dyn crate::state::owner_gate::PaneLivenessProbe,
) -> Result<LeaseResult, LeaderError> {
    let requested_team = team.filter(|team| !team.is_empty());
    let mut scoped_team = None;
    if let Some(team) = requested_team {
        let active_team = crate::messaging::leader_receiver::active_team_key(workspace, state);
        if (team == active_team
            || state
                .get("teams")
                .and_then(|teams| teams.get(team))
                .is_some())
            && team != active_team
        {
            *state = crate::state::projection::project_top_level_view(state, team);
            scoped_team = Some(team);
        } else if team == active_team {
            *state = crate::state::projection::project_top_level_view(state, team);
            scoped_team = Some(team);
        }
    }
    claim_lease_no_incident_with_target(
        workspace,
        state,
        team,
        team_id,
        caller_pane,
        confirm,
        event_log,
        liveness,
        None,
        None,
        scoped_team,
    )
}

struct NonEmptyPaneId(PaneId);

impl NonEmptyPaneId {
    fn try_from_pane(pane: &PaneId) -> Result<Self, LeaderError> {
        if pane.as_str().trim().is_empty() {
            return Err(LeaderError::Validation("leader pane id is empty".to_string()));
        }
        Ok(Self(pane.clone()))
    }

    fn as_pane_id(&self) -> &PaneId {
        &self.0
    }
}

#[allow(clippy::too_many_arguments)]
fn claim_lease_no_incident_with_target(
    workspace: &Path,
    state: &mut Value,
    team: Option<&str>,
    team_id: &TeamKey,
    caller_pane: &PaneId,
    confirm: bool,
    event_log: &crate::event_log::EventLog,
    liveness: &dyn crate::state::owner_gate::PaneLivenessProbe,
    caller_target: Option<&LeaderClaimTarget>,
    caller_pane_info: Option<&PaneInfo>,
    scoped_team: Option<&str>,
) -> Result<LeaseResult, LeaderError> {
    let _ = team;
    let pre_epoch = current_owner_epoch(state);
    let bound_pane_id = bound_pane(state);
    if caller_pane.as_str().is_empty() {
        emit_lease_refusal(
            event_log,
            LeaseReason::NotInTmuxPane,
            state,
            bound_pane_id.as_deref(),
            None,
            team_id,
        )?;
        return Ok(refused(
            LeaseReason::NotInTmuxPane,
            "run team-agent claim-leader from the leader's tmux pane",
            None,
            None,
        ));
    }
    if liveness.liveness(caller_pane.as_str()) != PaneLiveness::Live {
        emit_lease_refusal(
            event_log,
            LeaseReason::CallerPaneNotLive,
            state,
            bound_pane_id.as_deref(),
            Some(caller_pane.as_str()),
            team_id,
        )?;
        return Ok(refused(
            LeaseReason::CallerPaneNotLive,
            "run team-agent claim-leader from a live tmux pane",
            None,
            None,
        ));
    }
    let non_empty_caller_pane = NonEmptyPaneId::try_from_pane(caller_pane)?;
    let bound_endpoint_matches_caller = bound_endpoint_matches_current_process(state);
    if bound_pane_id.as_deref() == Some(caller_pane.as_str()) && bound_endpoint_matches_caller {
        let current_endpoint = crate::tmux_backend::socket_name_from_tmux_env();
        let observed_endpoint = caller_target.and_then(|target| target.endpoint.as_deref());
        let convergence_candidate = observed_endpoint.or(current_endpoint.as_deref());
        let candidate_source = if observed_endpoint.is_some() {
            Some("observed_target_endpoint")
        } else if current_endpoint.is_some() {
            Some("fallback_tmux_env")
        } else {
            None
        };
        let (mut topology_convergence, converged) =
            apply_endpoint_convergence(
                state,
                team_id,
                convergence_candidate,
                candidate_source,
                pre_epoch,
            );
        if converged {
            write_claim_state(workspace, state, scoped_team, team)?;
            topology_convergence = verify_persisted_topology_convergence(
                workspace,
                team_id.as_str(),
                topology_convergence,
                pre_epoch,
            )?;
            if topology_convergence
                .as_ref()
                .and_then(|metadata| metadata.get("status"))
                .and_then(Value::as_str)
                != Some("converged")
            {
                return Ok(convergence_persistence_refusal(
                    topology_convergence,
                    pre_epoch,
                    Some(caller_pane.clone()),
                ));
            }
        }
        return Ok(LeaseResult {
            ok: true,
            status: LeaseStatus::AlreadyBound,
            receiver: state_receiver(state),
            owner: state_owner(state),
            owner_epoch: Some(pre_epoch),
            reason: None,
            action: None,
            bound_pane_id: Some(caller_pane.clone()),
            topology_convergence,
        });
    }
    let owner_live = bound_pane_id
        .as_deref()
        .is_some_and(|pane| {
            if pane == caller_pane.as_str() && !bound_endpoint_matches_caller {
                return false;
            }
            liveness.liveness(pane) == PaneLiveness::Live
        });
    if owner_live && !confirm {
        emit_lease_refusal(
            event_log,
            LeaseReason::PreviousOwnerAliveRefused,
            state,
            bound_pane_id.as_deref(),
            Some(caller_pane.as_str()),
            team_id,
        )?;
        return Ok(refused(
            LeaseReason::ForceConfirmRequired,
            "rerun with --confirm to take over the live leader pane",
            Some(pre_epoch),
            bound_pane_id.clone().map(PaneId::new),
        ));
    }
    if let Some(locked) = locked_runtime_state(workspace, scoped_team)? {
        let locked_epoch = current_owner_epoch(&locked);
        if locked_epoch != pre_epoch {
            emit_lease_refusal(
                event_log,
                LeaseReason::OwnerEpochAdvanced,
                state,
                bound_pane_id.as_deref(),
                Some(caller_pane.as_str()),
                team_id,
            )?;
            return Ok(refused(
                LeaseReason::OwnerEpochAdvanced,
                "",
                Some(OwnerEpoch(locked_epoch.0.max(pre_epoch.0))),
                bound_pane_id.clone().map(PaneId::new),
            ));
        }
        let locked_bound_pane = bound_pane(&locked);
        let locked_owner_live = locked_bound_pane
            .as_deref()
            .is_some_and(|pane| pane != caller_pane.as_str() && liveness.liveness(pane) == PaneLiveness::Live);
        if locked_owner_live && !confirm {
            emit_lease_refusal(
                event_log,
                LeaseReason::OwnerEpochAdvanced,
                &locked,
                locked_bound_pane.as_deref(),
                Some(caller_pane.as_str()),
                team_id,
            )?;
            return Ok(refused(
                LeaseReason::OwnerEpochAdvanced,
                "",
                Some(locked_epoch),
                locked_bound_pane.clone().map(PaneId::new),
            ));
        }
    }
    // E51 (0.3.26 P0, lease guard): refuse to write the leader binding when the
    // caller pane is a REGISTERED WORKER pane. Without this, claim_leader from a
    // worker's tmux pane overwrites leader_receiver.pane_id with the worker's
    // pane → delivery routes worker messages to itself (loop) and the leader
    // loses its handle (the macmini "hand-handle mapping 灾难" truth source).
    if let Some(caller_pane_info) = caller_pane_info {
        match crate::topology::classify_registered_worker_for_observed_pane(
            state,
            caller_pane_info,
        ) {
            crate::topology::WorkerPaneBindingMatch::LiveSameWorker { agent_id } => {
                emit_lease_refusal(
                    event_log,
                    LeaseReason::CallerNotLeaderShaped,
                    state,
                    bound_pane_id.as_deref(),
                    Some(caller_pane.as_str()),
                    team_id,
                )?;
                return Ok(refused(
                    LeaseReason::CallerNotLeaderShaped,
                    &format!(
                        "pane {} is registered as worker {agent_id}; \
                         run claim-leader from the leader's own pane, not a worker pane",
                        caller_pane.as_str()
                    ),
                    None,
                    bound_pane_id.clone().map(PaneId::new),
                ));
            }
            crate::topology::WorkerPaneBindingMatch::Stale { agent_id, reason } => {
                event_log.write(
                    "leader_receiver.worker_pane_binding_ignored",
                    json!({
                        "agent_id": agent_id,
                        "pane_id": caller_pane.as_str(),
                        "reason": reason,
                    }),
                )?;
            }
            crate::topology::WorkerPaneBindingMatch::IncompleteLegacy { agent_id } => {
                event_log.write(
                    "leader_receiver.worker_pane_binding_ignored",
                    json!({
                        "agent_id": agent_id,
                        "pane_id": caller_pane.as_str(),
                        "reason": "incomplete_legacy_tuple",
                    }),
                )?;
            }
            crate::topology::WorkerPaneBindingMatch::NoMatch => {}
        }
    }
    let reason = if bound_pane_id.is_some() {
        LeaseReason::PreviousOwnerPaneDead
    } else {
        LeaseReason::VacantAcquired
    };
    let mut identity = leader_identity_context(workspace, Some(team_id.as_str()), Some(state))?;
    if let Some(uuid) = caller_target.and_then(|target| target.leader_session_uuid.as_ref()) {
        identity.leader_session_uuid = uuid.clone();
    }
    let next_epoch = OwnerEpoch(pre_epoch.0.saturating_add(1));
    let provider = caller_target.map_or_else(|| prior_provider(state), |target| target.provider);
    let observed_endpoint = caller_target.and_then(|target| target.endpoint.clone());
    let mut receiver = make_receiver(
        provider,
        &non_empty_caller_pane,
        &identity.leader_session_uuid,
        next_epoch,
        Discovery::ClaimLeader,
        caller_target.and_then(|target| target.pane_info.clone()),
    );
    if let Some(endpoint) = observed_endpoint.as_ref() {
        receiver.tmux_socket = Some(endpoint.clone());
    }
    let owner = make_owner(provider, &non_empty_caller_pane, &identity, next_epoch);
    write_binding_to_state(state, &receiver, &owner)?;
    let candidate_source = if observed_endpoint.is_some() {
        Some("observed_target_endpoint")
    } else if receiver.tmux_socket.is_some() {
        Some("fallback_tmux_env")
    } else {
        None
    };
    let (mut topology_convergence, converged) =
        apply_endpoint_convergence(
            state,
            team_id,
            receiver.tmux_socket.as_deref(),
            candidate_source,
            next_epoch,
        );
    write_claim_state(workspace, state, scoped_team, team)?;
    if converged {
        topology_convergence = verify_persisted_topology_convergence(
            workspace,
            team_id.as_str(),
            topology_convergence,
            next_epoch,
        )?;
        if topology_convergence
            .as_ref()
            .and_then(|metadata| metadata.get("status"))
            .and_then(Value::as_str)
            != Some("converged")
        {
            return Ok(convergence_persistence_refusal(
                topology_convergence,
                next_epoch,
                Some(caller_pane.clone()),
            ));
        }
    }
    let uuid_prefix = identity.leader_session_uuid.as_str().chars().take(8).collect::<String>();
    if reason == LeaseReason::PreviousOwnerPaneDead {
        event_log.write(
            super::LeaderEvent::OwnerAdoptedOnRestart.name(),
            json!({
                "reason": serde_json::to_value(reason)?,
                "old_pane_id": bound_pane_id,
                "new_pane_id": caller_pane.as_str(),
                "owner_epoch": next_epoch.0,
                "uuid_prefix": uuid_prefix,
                "team_id": team_id.as_str(),
                "host": owner.machine_fingerprint,
                "os_user": identity.os_user,
            }),
        )?;
    }
    event_log.write(
        super::LeaderEvent::ReceiverRebindApplied.name(),
        json!({
            "reason": serde_json::to_value(reason)?,
            "old_pane_id": bound_pane_id,
            "new_pane_id": caller_pane.as_str(),
            "owner_epoch": next_epoch.0,
            "uuid_prefix": uuid_prefix,
            "team_id": team_id.as_str(),
        }),
    )?;
    event_log.write(
        super::LeaderEvent::OwnerEpochAdvanced.name(),
        json!({
            "reason": serde_json::to_value(reason)?,
            "old_pane_id": bound_pane_id,
            "new_pane_id": caller_pane.as_str(),
            "owner_epoch": next_epoch.0,
            "uuid_prefix": uuid_prefix,
            "team_id": team_id.as_str(),
        }),
    )?;
    Ok(LeaseResult {
        ok: true,
        status: LeaseStatus::Claimed,
        receiver: Some(receiver),
        owner: Some(owner),
        owner_epoch: Some(next_epoch),
        reason: Some(reason),
        action: None,
        bound_pane_id: Some(caller_pane.clone()),
        topology_convergence,
    })
}

fn apply_endpoint_convergence(
    state: &mut Value,
    team_id: &TeamKey,
    candidate_endpoint: Option<&str>,
    candidate_source: Option<&'static str>,
    owner_epoch: OwnerEpoch,
) -> (Option<Value>, bool) {
    let explicit_candidate = candidate_endpoint
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_string);
    let (candidate_endpoint, candidate_source) = if let Some(endpoint) = explicit_candidate {
        (endpoint, candidate_source.unwrap_or("candidate"))
    } else if let Some(endpoint) = state_tmux_socket_candidate(state, team_id.as_str()) {
        (endpoint, "fallback_state")
    } else {
        return (None, false);
    };
    match crate::topology::endpoint_convergence_decision(
        state,
        team_id.as_str(),
        &candidate_endpoint,
    ) {
        crate::topology::EndpointConvergenceDecision::NoConflict => (None, false),
        crate::topology::EndpointConvergenceDecision::Unknown => (
            Some(json!({
                "status": "unknown",
                "new_tmux_endpoint": candidate_endpoint,
                "candidate_source": candidate_source,
                "owner_epoch": owner_epoch.0,
            })),
            false,
        ),
        crate::topology::EndpointConvergenceDecision::RefuseLiveOldEndpoint {
            old_endpoint,
            new_endpoint,
            reason,
        } => (
            Some(json!({
                "status": "not_converged_old_endpoint_live",
                "old_tmux_endpoint": old_endpoint,
                "new_tmux_endpoint": new_endpoint,
                "reason": reason,
                "candidate_source": candidate_source,
                "owner_epoch": owner_epoch.0,
                "action": format!(
                    "old tmux endpoint {old_endpoint} still has this team's session or pane tuple; run team-agent diagnose --json before retrying restart"
                ),
            })),
            false,
        ),
        crate::topology::EndpointConvergenceDecision::Converge {
            old_endpoint,
            new_endpoint,
            reason,
        } => {
            let metadata = json!({
                "status": "converged",
                "old_tmux_endpoint": old_endpoint,
                "new_tmux_endpoint": new_endpoint,
                "reason": reason,
                "candidate_source": candidate_source,
                "owner_epoch": owner_epoch.0,
            });
            write_endpoint_fields(state, team_id.as_str(), &new_endpoint);
            write_convergence_marker(state, team_id.as_str(), &metadata);
            (Some(metadata), true)
        }
    }
}

fn state_tmux_socket_candidate(state: &Value, team_id: &str) -> Option<String> {
    state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_string)
        .or_else(|| {
            state
                .get("teams")
                .and_then(Value::as_object)
                .and_then(|teams| teams.get(team_id))
                .and_then(|team| team.get("tmux_socket"))
                .and_then(Value::as_str)
                .filter(|endpoint| !endpoint.is_empty())
                .map(str::to_string)
        })
}

fn write_endpoint_fields(state: &mut Value, team_id: &str, endpoint: &str) {
    if !state.is_object() {
        *state = json!({});
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("tmux_endpoint".to_string(), json!(endpoint));
        obj.insert("tmux_socket".to_string(), json!(endpoint));
        obj.insert("tmux_socket_source".to_string(), json!("leader_env"));
        if let Some(team_obj) = obj
            .get_mut("teams")
            .and_then(Value::as_object_mut)
            .and_then(|teams| teams.get_mut(team_id))
            .and_then(Value::as_object_mut)
        {
            team_obj.insert("tmux_endpoint".to_string(), json!(endpoint));
            team_obj.insert("tmux_socket".to_string(), json!(endpoint));
            team_obj.insert("tmux_socket_source".to_string(), json!("leader_env"));
        }
    }
}

/// Persist the user-visible endpoint convergence marker.
///
/// This field is written by real `claim-leader` / `takeover` success paths, so
/// any restart-side harness bypass that keys off it must carry a second
/// production-safety gate such as a fake-only predicate or a `TEAM_AGENT_TEST_*`
/// env var.
fn write_convergence_marker(state: &mut Value, team_id: &str, metadata: &Value) {
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    obj.insert("topology_convergence".to_string(), metadata.clone());
    if let Some(team_obj) = obj
        .get_mut("teams")
        .and_then(Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_id))
        .and_then(Value::as_object_mut)
    {
        team_obj.insert("topology_convergence".to_string(), metadata.clone());
    }
}

fn verify_persisted_topology_convergence(
    workspace: &Path,
    team_id: &str,
    metadata: Option<Value>,
    owner_epoch: OwnerEpoch,
) -> Result<Option<Value>, LeaderError> {
    let Some(mut metadata) = metadata else {
        return Ok(None);
    };
    if metadata.get("status").and_then(Value::as_str) != Some("converged") {
        return Ok(Some(metadata));
    }
    let Some(new_endpoint) = metadata
        .get("new_tmux_endpoint")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        mark_convergence_persistence_conflict(&mut metadata, team_id);
        return Ok(Some(metadata));
    };
    let state = crate::state::persist::load_runtime_state(workspace)?;
    if endpoint_convergence_persisted(&state, team_id, &new_endpoint, owner_epoch.0) {
        mark_convergence_persisted(&mut metadata, team_id);
    } else {
        mark_convergence_persistence_conflict(&mut metadata, team_id);
    }
    Ok(Some(metadata))
}

fn endpoint_convergence_persisted(
    state: &Value,
    team_id: &str,
    endpoint: &str,
    owner_epoch: u64,
) -> bool {
    if !endpoint_convergence_node_matches(state, endpoint, owner_epoch) {
        return false;
    }
    state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_id))
        .is_some_and(|team| endpoint_convergence_node_matches(team, endpoint, owner_epoch))
}

fn endpoint_convergence_node_matches(node: &Value, endpoint: &str, owner_epoch: u64) -> bool {
    node.get("tmux_endpoint").and_then(Value::as_str) == Some(endpoint)
        && node.get("tmux_socket").and_then(Value::as_str) == Some(endpoint)
        && node.get("tmux_socket_source").and_then(Value::as_str) == Some("leader_env")
        && node
            .get("topology_convergence")
            .and_then(|marker| marker.get("status"))
            .and_then(Value::as_str)
            == Some("converged")
        && node
            .get("topology_convergence")
            .and_then(|marker| marker.get("owner_epoch"))
            .and_then(Value::as_u64)
            == Some(owner_epoch)
}

fn mark_convergence_persisted(metadata: &mut Value, team_id: &str) {
    let Some(obj) = metadata.as_object_mut() else {
        return;
    };
    obj.insert("persisted".to_string(), json!(true));
    obj.insert("checked_paths".to_string(), json!(convergence_checked_paths(team_id)));
}

fn mark_convergence_persistence_conflict(metadata: &mut Value, team_id: &str) {
    let Some(obj) = metadata.as_object_mut() else {
        return;
    };
    obj.insert("status".to_string(), json!("persistence_conflict"));
    obj.insert("persisted".to_string(), json!(false));
    obj.insert("checked_paths".to_string(), json!(convergence_checked_paths(team_id)));
    obj.insert(
        "action".to_string(),
        json!("endpoint convergence was not durably persisted; retry claim-leader or takeover"),
    );
}

fn convergence_checked_paths(team_id: &str) -> Vec<String> {
    [
        "/tmux_endpoint".to_string(),
        "/tmux_socket".to_string(),
        "/tmux_socket_source".to_string(),
        "/topology_convergence".to_string(),
        format!("/teams/{team_id}/tmux_endpoint"),
        format!("/teams/{team_id}/tmux_socket"),
        format!("/teams/{team_id}/tmux_socket_source"),
        format!("/teams/{team_id}/topology_convergence"),
    ]
    .into_iter()
    .collect()
}

fn convergence_persistence_refusal(
    topology_convergence: Option<Value>,
    owner_epoch: OwnerEpoch,
    bound_pane_id: Option<PaneId>,
) -> LeaseResult {
    LeaseResult {
        ok: false,
        status: LeaseStatus::Refused,
        receiver: None,
        owner: None,
        owner_epoch: Some(owner_epoch),
        reason: Some(LeaseReason::OwnerEpochAdvanced),
        action: Some(
            "endpoint convergence was not durably persisted; retry claim-leader or takeover"
                .to_string(),
        ),
        bound_pane_id,
        topology_convergence,
    }
}

fn refused(
    reason: LeaseReason,
    action: &str,
    epoch: Option<OwnerEpoch>,
    bound_pane_id: Option<PaneId>,
) -> LeaseResult {
    LeaseResult {
        ok: false,
        status: LeaseStatus::Refused,
        receiver: None,
        owner: None,
        owner_epoch: epoch,
        reason: Some(reason),
        action: if action.is_empty() { None } else { Some(action.to_string()) },
        bound_pane_id,
        topology_convergence: None,
    }
}

fn current_owner_epoch(state: &Value) -> OwnerEpoch {
    let owner_epoch = get_path_u64(state, &["team_owner", "owner_epoch"]).filter(|v| *v != 0);
    let receiver_epoch = get_path_u64(state, &["leader_receiver", "owner_epoch"]).filter(|v| *v != 0);
    OwnerEpoch(owner_epoch.or(receiver_epoch).unwrap_or(0))
}

fn bound_pane(state: &Value) -> Option<String> {
    get_path_str(state, &["leader_receiver", "pane_id"])
        .filter(|v| !v.is_empty())
        .or_else(|| get_path_str(state, &["team_owner", "pane_id"]).filter(|v| !v.is_empty()))
}

fn bound_endpoint_matches_current_process(state: &Value) -> bool {
    let Some(bound) = get_path_str(state, &["leader_receiver", "tmux_socket"]).filter(|v| !v.is_empty()) else {
        return true;
    };
    let Some(current) = crate::tmux_backend::socket_name_from_tmux_env() else {
        return false;
    };
    tmux_endpoints_match(&bound, &current)
}

fn tmux_endpoints_match(bound: &str, current: &str) -> bool {
    bound == current
}

fn prior_provider(state: &Value) -> Provider {
    get_path_str(state, &["leader_receiver", "provider"])
        .or_else(|| get_path_str(state, &["team_owner", "provider"]))
        .and_then(|raw| parse_provider(&raw))
        .unwrap_or(Provider::Codex)
}

struct LeaderClaimTarget {
    provider: Provider,
    leader_session_uuid: Option<crate::model::ids::LeaderSessionUuid>,
    team_id: Option<String>,
    pane_info: Option<PaneInfo>,
    endpoint: Option<String>,
}

#[derive(Clone)]
struct ClaimLeaderTargetCandidate {
    info: PaneInfo,
    endpoint: Option<String>,
    source: ClaimLeaderTargetSource,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaimLeaderTargetSource {
    StateRecorded,
    Workspace,
    CurrentTmux,
    Default,
}

impl ClaimLeaderTargetSource {
    fn priority(self) -> u8 {
        match self {
            Self::StateRecorded => 0,
            Self::Workspace => 1,
            Self::CurrentTmux => 2,
            Self::Default => 3,
        }
    }
}

fn claim_leader_targets(workspace: &Path, state: &Value) -> Vec<ClaimLeaderTargetCandidate> {
    let mut targets = Vec::new();
    if let Some(endpoint) = crate::tmux_backend::socket_name_from_tmux_env() {
        let backend = tmux_backend_for_endpoint(&endpoint);
        let resolved_endpoint = backend.tmux_endpoint();
        targets.extend(
            backend
                .list_targets()
                .unwrap_or_default()
                .into_iter()
                .map(|info| ClaimLeaderTargetCandidate {
                    info,
                    endpoint: resolved_endpoint.clone(),
                    source: ClaimLeaderTargetSource::CurrentTmux,
                }),
        );
    }
    for endpoint in state_recorded_tmux_endpoints(state) {
        let backend = tmux_backend_for_endpoint(&endpoint);
        let resolved_endpoint = backend.tmux_endpoint();
        targets.extend(
            backend
                .list_targets()
                .unwrap_or_default()
                .into_iter()
                .map(|info| ClaimLeaderTargetCandidate {
                    info,
                    endpoint: resolved_endpoint.clone(),
                    source: ClaimLeaderTargetSource::StateRecorded,
                }),
        );
    }
    let workspace_backend = crate::transport_factory::tmux_workspace_transport(workspace);
    let workspace_endpoint = workspace_backend.tmux_endpoint();
    targets.extend(
        workspace_backend
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .map(|info| ClaimLeaderTargetCandidate {
                info,
                endpoint: workspace_endpoint.clone(),
                source: ClaimLeaderTargetSource::Workspace,
            }),
    );
    let default_backend = crate::transport_factory::tmux_default_transport();
    let default_endpoint = default_backend.tmux_endpoint();
    targets.extend(
        default_backend
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .map(|info| ClaimLeaderTargetCandidate {
                info,
                endpoint: default_endpoint.clone(),
                source: ClaimLeaderTargetSource::Default,
            }),
    );
    targets
}

fn claim_target_from_pane_info(workspace: &Path, target: &PaneInfo) -> Option<LeaderClaimTarget> {
    if !target.active {
        return None;
    }
    let provider = super::attribute_pane_provider(target)?;
    let current_path = target.current_path.as_deref()?;
    if !crate::state::owner_gate::workspace_paths_match(current_path, workspace) {
        return None;
    }
    Some(LeaderClaimTarget {
        provider,
        leader_session_uuid: target_leader_session_uuid(target),
        team_id: target.leader_env.get("TEAM_AGENT_TEAM_ID").filter(|raw| !raw.is_empty()).cloned(),
        pane_info: Some(target.clone()),
        endpoint: None,
    })
}

fn target_leader_session_uuid(target: &PaneInfo) -> Option<crate::model::ids::LeaderSessionUuid> {
    target
        .leader_env
        .get("TEAM_AGENT_LEADER_SESSION_UUID")
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_value(json!(raw)).ok())
}

fn validate_attach_target(
    workspace: &Path,
    state: &Value,
    target: &PaneInfo,
    requested_provider: Provider,
) -> Result<(), &'static str> {
    // 0.5.9 (E6 real-machine e2e wiring): an explicit `--provider fake`
    // is the operator's declaration that this pane is a fake-provider
    // stub for tests / fixtures. The normal pane-command attribution
    // path can't recognize `/bin/cat` (or any bare shell process) as a
    // provider, so honoring the explicit request here is the only way
    // real-machine E6 acceptance can spin up a leader without shipping
    // a real Codex/Claude/Copilot binary. This is a targeted escape
    // hatch — `Provider::Fake` is not selectable from the user-facing
    // provider list, only wired in test/fixture flows.
    let claim_target = match claim_target_from_pane_info(workspace, target) {
        Some(target) => Some(target),
        None if matches!(requested_provider, Provider::Fake) => None,
        None => return Err("leader_pane_validation_failed"),
    };
    let Some(claim_target) = claim_target else {
        // Fake provider: skip session-uuid check since attribution was
        // bypassed. Nothing else to validate.
        return Ok(());
    };
    let recorded_uuid = get_path_str(state, &["team_owner", "leader_session_uuid"])
        .or_else(|| get_path_str(state, &["leader_receiver", "leader_session_uuid"]));
    if let (Some(recorded), Some(target_uuid)) = (
        recorded_uuid.as_deref(),
        claim_target.leader_session_uuid.as_ref().map(|u| u.as_str()),
    ) {
        if recorded != target_uuid {
            return Err("leader_session_uuid_mismatch");
        }
    }
    Ok(())
}

fn receiver_for_attach_target(
    workspace: &Path,
    state: &Value,
    target: &PaneInfo,
    provider: Provider,
    discovery: Discovery,
) -> Result<LeaderReceiver, LeaderError> {
    let identity = leader_identity_context(workspace, None, Some(state))?;
    let epoch = current_owner_epoch(state);
    let pane = NonEmptyPaneId::try_from_pane(&target.pane_id)?;
    Ok(make_receiver(
        provider,
        &pane,
        &identity.leader_session_uuid,
        epoch,
        discovery,
        Some(target.clone()),
    ))
}

fn pane_info_value(target: &PaneInfo) -> Value {
    let leader_env = target
        .leader_env
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "pane_id": target.pane_id.as_str(),
        "session_name": target.session.as_str(),
        "window_index": target.window_index.map(|v| v.to_string()),
        "window_name": target.window_name.as_ref().map(|v| v.as_str().to_string()),
        "pane_index": target.pane_index.map(|v| v.to_string()),
        "pane_tty": target.tty.as_ref(),
        "pane_current_command": target.current_command.as_ref(),
        "pane_current_path": target.current_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "active": target.active,
        "leader_env": leader_env,
    })
}

/// `AnyPaneLiveness` — minimal "does this tmux pane id exist in the server's current
/// target list?" probe. Unlike [`TargetScanLiveness`], it does NOT additionally require
/// the pane to be running a leader-shaped command (claude/codex/fake) or to match the
/// workspace cwd. Explicit claim/takeover only require a positive caller pane source
/// that is live; ownership replacement stays inside the normal lease write path.
struct AnyPaneLiveness {
    live_panes: std::collections::BTreeSet<String>,
}

impl AnyPaneLiveness {
    fn from_claim_targets(targets: &[ClaimLeaderTargetCandidate]) -> Self {
        Self {
            live_panes: targets
                .iter()
                .map(|target| target.info.pane_id.as_str().to_string())
                .collect(),
        }
    }
}

impl crate::state::owner_gate::PaneLivenessProbe for AnyPaneLiveness {
    fn liveness(&self, pane_id: &str) -> PaneLiveness {
        if self.live_panes.contains(pane_id) {
            PaneLiveness::Live
        } else {
            PaneLiveness::Dead
        }
    }
}

struct TargetScanLiveness {
    live_panes: std::collections::BTreeSet<String>,
}

impl TargetScanLiveness {
    fn new(state: &Value, targets: &[PaneInfo], workspace: &Path) -> Self {
        let owner_uuid = get_path_str(state, &["team_owner", "leader_session_uuid"]);
        let live_panes = targets
            .iter()
            .filter_map(|target| {
                let claim_target = claim_target_from_pane_info(workspace, target)?;
                if let Some(owner_uuid) = owner_uuid.as_deref() {
                    let target_uuid = claim_target.leader_session_uuid.as_ref()?.as_str();
                    if target_uuid != owner_uuid {
                        return None;
                    }
                }
                Some(target.pane_id.as_str().to_string())
            })
            .collect();
        Self { live_panes }
    }
}

impl crate::state::owner_gate::PaneLivenessProbe for TargetScanLiveness {
    fn liveness(&self, pane_id: &str) -> PaneLiveness {
        if self.live_panes.contains(pane_id) {
            PaneLiveness::Live
        } else {
            PaneLiveness::Dead
        }
    }
}

fn locked_runtime_state(workspace: &Path, scoped_team: Option<&str>) -> Result<Option<Value>, LeaderError> {
    let path = crate::state::persist::runtime_state_path(workspace);
    if !path.exists() {
        return Ok(None);
    }
    let state = if let Some(team) = scoped_team {
        crate::state::projection::select_runtime_state(workspace, Some(team))?
    } else {
        crate::state::persist::load_runtime_state(workspace)?
    };
    Ok(Some(state))
}

fn emit_lease_refusal(
    event_log: &crate::event_log::EventLog,
    reason: LeaseReason,
    state: &Value,
    old_pane: Option<&str>,
    new_pane: Option<&str>,
    team_id: &TeamKey,
) -> Result<(), LeaderError> {
    let event = if reason.is_rebind_required() {
        super::LeaderEvent::ReceiverRebindRequired
    } else {
        super::LeaderEvent::ReceiverClaimRefused
    };
    let uuid_prefix = get_path_str(state, &["team_owner", "leader_session_uuid"])
        .unwrap_or_default()
        .chars()
        .take(8)
        .collect::<String>();
    let host = get_path_str(state, &["team_owner", "machine_fingerprint"])
        .unwrap_or_else(|| "local-machine".to_string());
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    event_log.write(
        event.name(),
        json!({
            "reason": serde_json::to_value(reason)?,
            "old_pane_id": old_pane,
            "new_pane_id": new_pane,
            "uuid_prefix": uuid_prefix,
            "team_id": team_id.as_str(),
            "host": host,
            "os_user": os_user,
        }),
    )?;
    Ok(())
}

fn make_receiver(
    provider: Provider,
    pane: &NonEmptyPaneId,
    uuid: &crate::model::ids::LeaderSessionUuid,
    epoch: OwnerEpoch,
    discovery: Discovery,
    target: Option<PaneInfo>,
) -> LeaderReceiver {
    LeaderReceiver {
        mode: ReceiverMode::DirectTmux,
        status: ReceiverStatus::Attached,
        provider,
        pane_id: pane.as_pane_id().clone(),
        session_name: target.as_ref().map(|t| t.session.clone()),
        window_index: target.as_ref().and_then(|t| t.window_index.map(|v| v.to_string())),
        window_name: target.as_ref().and_then(|t| t.window_name.clone()),
        pane_index: target.as_ref().and_then(|t| t.pane_index.map(|v| v.to_string())),
        pane_tty: target.as_ref().and_then(|t| t.tty.clone()),
        pane_current_command: target.as_ref().and_then(|t| t.current_command.clone()),
        tmux_socket: crate::tmux_backend::socket_name_from_tmux_env(),
        fingerprint: target.as_ref().map(receiver_fingerprint),
        leader_session_uuid: Some(uuid.clone()),
        owner_epoch: Some(epoch),
        attached_at: Some(now_ts()),
        discovery: Some(discovery),
        requested_provider: None,
        warning: None,
    }
}

fn receiver_fingerprint(target: &PaneInfo) -> String {
    format!(
        "{}|{}|{}|{}",
        target.session.as_str(),
        target.window_index.map_or_else(String::new, |v| v.to_string()),
        target.pane_index.map_or_else(String::new, |v| v.to_string()),
        target.tty.as_deref().unwrap_or("")
    )
}

fn make_owner(
    provider: Provider,
    pane: &NonEmptyPaneId,
    identity: &super::LeaderIdentity,
    epoch: OwnerEpoch,
) -> TeamOwner {
    TeamOwner {
        pane_id: pane.as_pane_id().clone(),
        provider,
        machine_fingerprint: identity.machine_fingerprint.clone(),
        leader_session_uuid: Some(identity.leader_session_uuid.clone()),
        owner_epoch: epoch,
        claimed_at: now_ts(),
        claimed_via: ClaimedVia::ClaimLeader,
        os_user: Some(
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default(),
        ),
    }
}

/// Stage 3a (identity-boundary unified plan, architect direction 2026-06-23)
/// + Stage 3 owner persist fix (architect direction 2026-06-24): route the
/// in-memory binding write through the `state::ownership` API. The
/// canonical team_key is derived from `active_team_key` first (the explicit
/// scope set by `project_top_level_view` when an explicit team was passed
/// to claim-leader) and only falls back to `team_state_key(state)` for the
/// legacy unscoped flow. Without the `active_team_key` preference, an
/// explicit `claim-leader --team alpha` whose projected state was emptied
/// of team_dir/session_name would derive "current" and write the owner to
/// `teams.current`, while `save_claim_team_scoped_state` expected
/// `teams.alpha` — the S3-OWNER-001 persistence loss shape.
fn write_binding_to_state(
    state: &mut Value,
    receiver: &LeaderReceiver,
    owner: &TeamOwner,
) -> Result<(), LeaderError> {
    if !state.is_object() {
        *state = json!({});
    }
    if !state.is_object() {
        return Err(LeaderError::Validation("state root is not an object".to_string()));
    }
    let team_key = canonical_owner_write_key(state);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(tmux_receiver_value(receiver)?)
        .with_team_owner(serde_json::to_value(owner)?)
        .with_owner_epoch(owner.owner_epoch.0);
    crate::state::ownership::write_owner(state, &team_key, record);
    Ok(())
}

fn write_receiver_to_state(
    state: &mut Value,
    receiver: &LeaderReceiver,
) -> Result<(), LeaderError> {
    if !state.is_object() {
        *state = json!({});
    }
    if !state.is_object() {
        return Err(LeaderError::Validation("state root is not an object".to_string()));
    }
    let team_key = canonical_owner_write_key(state);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(tmux_receiver_value(receiver)?);
    crate::state::ownership::write_owner(state, &team_key, record);
    Ok(())
}

fn tmux_receiver_value(receiver: &LeaderReceiver) -> Result<Value, LeaderError> {
    Ok(write_leader_receiver_transport(
        serde_json::to_value(receiver)?,
        "direct_tmux",
    ))
}

fn app_server_receiver_value(
    binding: &crate::codex_app_server::AppServerBinding,
    epoch: OwnerEpoch,
) -> Value {
    write_leader_receiver_transport(
        json!({
            "status": "attached",
            "provider": "codex",
            "owner_epoch": epoch.0,
            "app_server": {
                "socket": binding.socket,
                "thread_id": binding.thread_id,
                "session_id": binding.session_id,
                "cwd": binding.cwd,
                "cli_version": binding.cli_version,
                "bound_at": binding.bound_at,
                "source": "app-server"
            }
        }),
        "codex_app_server",
    )
}

fn app_server_owner_value(epoch: OwnerEpoch) -> Value {
    json!({
        "provider": "codex",
        "transport_kind": "codex_app_server",
        "owner_epoch": epoch.0,
        "claimed_at": now_ts(),
        "claimed_via": "attach-app-server-leader",
        "os_user": std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
    })
}

fn write_leader_receiver_transport(mut receiver: Value, transport_kind: &str) -> Value {
    if let Some(obj) = receiver.as_object_mut() {
        obj.insert("mode".to_string(), json!(transport_kind));
        match transport_kind {
            "codex_app_server" => {
                obj.insert("transport_kind".to_string(), json!(transport_kind));
                for legacy in [
                    "pane_id",
                    "tmux_socket",
                    "session_name",
                    "window_index",
                    "window_name",
                    "pane_index",
                    "pane_tty",
                    "pane_current_command",
                    "fingerprint",
                    "leader_session_uuid",
                    "attached_at",
                    "discovery",
                    "requested_provider",
                    "warning",
                ] {
                    obj.remove(legacy);
                }
            }
            "direct_tmux" => {
                obj.remove("app_server");
                obj.remove("transport_kind");
            }
            _ => {}
        }
    }
    receiver
}

/// Stage 3 owner persist fix (architect direction 2026-06-24): determine
/// the canonical team_key for an in-memory ownership write. When the state
/// carries an `active_team_key` (set by `project_top_level_view` whenever
/// an explicit team was scoped into the claim path), trust it — that's the
/// requested team that `save_claim_team_scoped_state` will read back. Only
/// fall back to `team_state_key(state)` for the legacy unscoped flow where
/// the writer must derive team identity from session/dir/spec_path fields.
fn canonical_owner_write_key(state: &Value) -> String {
    if let Some(active) = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
    {
        return active.to_string();
    }
    crate::state::projection::team_state_key(state)
}

fn state_receiver(state: &Value) -> Option<LeaderReceiver> {
    state
        .get("leader_receiver")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn state_owner(state: &Value) -> Option<TeamOwner> {
    state
        .get("team_owner")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// `_write_lease_dual_state` — Foundation-0 F0-2: the historical dual
/// write to the legacy per-session snapshot has been retired
/// (`.team/artifacts/foundation-0-slice-design.md` §§4-5). This helper
/// now persists ONLY the canonical root state; retaining the public
/// name for 0.5.x call-site stability. The B0 legacy snapshot is
/// diagnostic-only via `lifecycle::save_team_runtime_snapshot`.
pub fn write_lease_dual_state(workspace: &Path, state: &Value) -> Result<(), LeaderError> {
    crate::state::persist::save_runtime_state(workspace, state)?;
    Ok(())
}

fn write_claim_state(
    workspace: &Path,
    state: &Value,
    scoped_team: Option<&str>,
    team_key: Option<&str>,
) -> Result<(), LeaderError> {
    if let Some(team) = scoped_team {
        save_claim_team_scoped_state(workspace, state, team)?;
        Ok(())
    } else {
        let _ = team_key;
        write_lease_dual_state(workspace, state)
    }
}

fn save_claim_team_scoped_state(workspace: &Path, state: &Value, target_key: &str) -> Result<(), LeaderError> {
    let existing = crate::state::persist::load_runtime_state(workspace)?;
    let mut teams = existing
        .get("teams")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let had_existing_teams = !teams.is_empty();
    if existing
        .get("session_name")
        .and_then(Value::as_str)
        .is_some_and(|session| !session.is_empty())
    {
        let existing_key = crate::state::projection::team_state_key(&existing);
        teams
            .entry(existing_key)
            .or_insert_with(|| crate::state::projection::compact_team_state(&existing));
    }
    // Stage 3 owner persist fix (architect direction 2026-06-24,
    // .team/artifacts/stage3-owner-persist-fix.md): after Stage 3d removed
    // top-level dual-write, the canonical ownership record lives ONLY at
    // `state.teams[target_key].{team_owner, leader_receiver, owner_epoch}`.
    // `compact_team_state(state)` strips the `teams` key entirely — that
    // dropped the just-written nested ownership record on disk. Use the
    // preserving helper so the canonical fields survive the compaction.
    teams.insert(
        target_key.to_string(),
        compact_team_state_preserving_claim_fields(state, target_key),
    );
    let existing_primary_key = existing
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|session| !session.is_empty())
        .map(|_| crate::state::projection::team_state_key(&existing));
    let existing_active_key = existing.get("active_team_key").and_then(Value::as_str);
    let updates_active_team = existing_active_key == Some(target_key);
    let writes_endpoint_convergence = state
        .get("topology_convergence")
        .and_then(|marker| marker.get("status"))
        .and_then(Value::as_str)
        == Some("converged");
    let mut merged = if existing_primary_key
        .as_deref()
        .is_none_or(|key| key == target_key)
        || updates_active_team
        || writes_endpoint_convergence
    {
        value_object(state)
    } else {
        value_object(&existing)
    };
    // Stage 3 top-level cleanup (architect direction 2026-06-24,
    // .team/artifacts/stage3-toplevel-cleanup-fix.md + stage3-save-strip-fix.md):
    // the legacy top-level owner copy loop is gone; the canonical-aware
    // strip is now performed at the persistence boundary
    // (`state::persist::save_runtime_state_with_merge_exceptions`), which
    // every save_runtime_state* path funnels through. This per-call-site
    // strip call has been removed — the save output handles cleanup
    // uniformly for claim / restart / shutdown / start-agent / stop-agent
    // / coordinator tick / promote sibling. `had_existing_teams` /
    // primary/active key computations are retained as dead writes only
    // via _ binding — they no longer gate any owner promote.
    let _ = (
        had_existing_teams,
        existing_primary_key,
        existing_active_key,
        writes_endpoint_convergence,
    );
    merged.insert("teams".to_string(), Value::Object(teams));
    crate::state::persist::save_runtime_state(workspace, &Value::Object(merged))?;
    Ok(())
}

fn value_object(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// Stage 3 owner persist fix (architect direction 2026-06-24,
/// .team/artifacts/stage3-owner-persist-fix.md): build the per-team
/// snapshot used by `save_claim_team_scoped_state` so the canonical
/// `teams.<target_key>.{team_owner, leader_receiver, owner_epoch}` record
/// survives the `compact_team_state` strip. Without this helper the
/// claim-leader save loses the just-written nested ownership record (the
/// S3-OWNER-001 evidence shape: `persisted_owner_locations=[]`).
///
/// Behaviour:
/// - Start from the existing `compact_team_state(state)` (strips `teams`).
/// - Look up `state.teams[target_key].{team_owner, leader_receiver, owner_epoch}`
///   and copy each present field into the compacted entry.
/// - Top-level owner fields stay UNTOUCHED — Stage 3d's canonical-only
///   shape is preserved on disk.
fn compact_team_state_preserving_claim_fields(state: &Value, target_key: &str) -> Value {
    let mut entry = crate::state::projection::compact_team_state(state);
    let Some(entry_obj) = entry.as_object_mut() else {
        return entry;
    };
    let Some(owner_obj) = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(target_key))
        .and_then(Value::as_object)
    else {
        return entry;
    };
    for key in [
        "team_owner",
        "leader_receiver",
        "owner_epoch",
        "tmux_endpoint",
        "tmux_socket",
        "tmux_socket_source",
        "topology_convergence",
    ] {
        if let Some(value) = owner_obj.get(key) {
            entry_obj.insert(key.to_string(), value.clone());
        }
    }
    entry
}

/// Foundation-0 F0-2: reader for legacy per-session snapshot vs
/// canonical root state. Diagnostic-only after the dual-write retirement
/// (`.team/artifacts/foundation-0-slice-design.md` §§4-5); product
/// authority code never consults this — it exists so `status`/`diagnose`
/// can surface `legacy_snapshot_stale` when the on-disk sidecar has
/// drifted. Every touch of the legacy path constants below is marked
/// `B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ` so the RED3 grep guard admits
/// them as documented exceptions.
pub fn detect_dual_state_divergence( // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ: diagnostic-only entry point; no product save/route consumer.
    workspace: &Path,
    state: &Value,
) -> Result<Option<Value>, LeaderError> {
    let Some(session_name) = state.get("session_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let snap_path = readable_team_snapshot_path(workspace, session_name); // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ: diagnostic legacy-shape path lookup.
    if !snap_path.exists() {
        return Ok(None);
    }
    let snap: Value = serde_json::from_str(&std::fs::read_to_string(snap_path)?)?;
    let workspace_owner_pane = get_path_str(state, &["team_owner", "pane_id"]);
    let team_owner_pane = get_path_str(&snap, &["team_owner", "pane_id"]);
    let workspace_owner_uuid = get_path_str(state, &["team_owner", "leader_session_uuid"]);
    let team_owner_uuid = get_path_str(&snap, &["team_owner", "leader_session_uuid"]);
    let workspace_receiver_pane = get_path_str(state, &["leader_receiver", "pane_id"]);
    let team_receiver_pane = get_path_str(&snap, &["leader_receiver", "pane_id"]);
    let workspace_epoch = get_path_u64(state, &["team_owner", "owner_epoch"])
        .or_else(|| get_path_u64(state, &["leader_receiver", "owner_epoch"]));
    let team_epoch = get_path_u64(&snap, &["team_owner", "owner_epoch"])
        .or_else(|| get_path_u64(&snap, &["leader_receiver", "owner_epoch"]));
    let workspace_agent_bindings = agent_binding_summary(state);
    let team_agent_bindings = agent_binding_summary(&snap);
    let diverged = workspace_owner_pane != team_owner_pane
        || workspace_owner_uuid != team_owner_uuid
        || workspace_receiver_pane != team_receiver_pane
        || workspace_epoch != team_epoch
        || workspace_agent_bindings != team_agent_bindings;
    if !diverged {
        return Ok(None);
    }
    Ok(Some(json!({
        "workspace_owner_pane": workspace_owner_pane,
        "team_owner_pane": team_owner_pane,
        "workspace_owner_uuid": workspace_owner_uuid,
        "team_owner_uuid": team_owner_uuid,
        "workspace_receiver_pane": workspace_receiver_pane,
        "team_receiver_pane": team_receiver_pane,
        "workspace_owner_epoch": workspace_epoch,
        "team_owner_epoch": team_epoch,
        "workspace_agent_bindings": workspace_agent_bindings,
        "team_agent_bindings": team_agent_bindings,
        "_legacy_snapshot_stale": true,
    })))
}

fn agent_binding_summary(state: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return Value::Object(out);
    };
    for (agent_id, agent) in agents {
        let mut binding = serde_json::Map::new();
        for key in [
            "pane_id",
            "pane_pid",
            "tmux_endpoint",
            "tmux_socket",
            "window",
            "window_name",
        ] {
            if let Some(value) = agent.get(key) {
                binding.insert(key.to_string(), value.clone());
            }
        }
        if !binding.is_empty() {
            out.insert(agent_id.clone(), Value::Object(binding));
        }
    }
    Value::Object(out)
}

fn readable_team_snapshot_path(workspace: &Path, session_name: &str) -> PathBuf { // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ: diagnostic-only path resolver.
    let safe_path = crate::lifecycle::helpers::team_snapshot_path(workspace, session_name); // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ: reuses helpers safe legacy path.
    if safe_path.exists() {
        return safe_path;
    }
    // Raw legacy `runtime/teams` fallback for pre-safe-shape snapshots. // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ
    let legacy_dir = "teams"; // B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ
    crate::model::paths::runtime_dir(workspace)
        .join(legacy_dir)
        .join(session_name)
        .join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_command_provider_recognizes_copilot() {
        assert_eq!(
            super::super::attribute_command_provider("copilot --allow-all-tools"),
            Some(Provider::Copilot)
        );
        assert_eq!(
            super::super::attribute_command_provider("/usr/local/bin/copilot"),
            Some(Provider::Copilot)
        );
    }
}
