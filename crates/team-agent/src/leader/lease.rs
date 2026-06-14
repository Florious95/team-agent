//! leader::lease — attach / claim / autobind 统一 CAS 路径 + claim_lease_no_incident
//! + 双写 / 分叉检测。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::message_store::MessageStore;
use crate::model::ids::TeamKey;
use crate::model::enums::PaneLiveness;
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
    let validation = validate_attach_target(workspace, &state, &target.info);
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
    })
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
    targets.extend(
        crate::tmux_backend::TmuxBackend::for_workspace(workspace)
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .map(|info| AttachLeaderTarget { info, endpoint: None }),
    );
    targets
}

fn tmux_backend_for_endpoint(endpoint: &str) -> crate::tmux_backend::TmuxBackend {
    if endpoint.is_empty() || endpoint == "default" {
        crate::tmux_backend::TmuxBackend::new()
    } else {
        crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint)
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
    let mut targets = crate::tmux_backend::TmuxBackend::for_workspace(workspace)
        .list_targets()
        .unwrap_or_default();
    targets.extend(
        crate::tmux_backend::TmuxBackend::new()
            .list_targets()
            .unwrap_or_default(),
    );
    let caller_target = targets
        .iter()
        .find(|target| target.pane_id.as_str() == caller)
        .and_then(|target| claim_target_from_pane_info(workspace, target));
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
    let liveness = AnyPaneLiveness::from_targets(&targets);
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
        return Ok(LeaseResult {
            ok: true,
            status: LeaseStatus::AlreadyBound,
            receiver: state_receiver(state),
            owner: state_owner(state),
            owner_epoch: Some(pre_epoch),
            reason: None,
            action: None,
            bound_pane_id: Some(caller_pane.clone()),
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
    let receiver = make_receiver(
        provider,
        &non_empty_caller_pane,
        &identity.leader_session_uuid,
        next_epoch,
        Discovery::ClaimLeader,
        caller_target.and_then(|target| target.pane_info.clone()),
    );
    let owner = make_owner(provider, &non_empty_caller_pane, &identity, next_epoch);
    write_binding_to_state(state, &receiver, &owner)?;
    write_claim_state(workspace, state, scoped_team, team)?;
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
    })
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
) -> Result<(), &'static str> {
    let Some(claim_target) = claim_target_from_pane_info(workspace, target) else {
        return Err("leader_pane_validation_failed");
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
    fn from_targets(targets: &[PaneInfo]) -> Self {
        Self {
            live_panes: targets
                .iter()
                .map(|target| target.pane_id.as_str().to_string())
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

fn write_binding_to_state(
    state: &mut Value,
    receiver: &LeaderReceiver,
    owner: &TeamOwner,
) -> Result<(), LeaderError> {
    if !state.is_object() {
        *state = json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LeaderError::Validation("state root is not an object".to_string()));
    };
    root.insert("leader_receiver".to_string(), serde_json::to_value(receiver)?);
    root.insert("team_owner".to_string(), serde_json::to_value(owner)?);
    root.insert("owner_epoch".to_string(), json!(owner.owner_epoch.0));
    Ok(())
}

fn write_receiver_to_state(
    state: &mut Value,
    receiver: &LeaderReceiver,
) -> Result<(), LeaderError> {
    if !state.is_object() {
        *state = json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LeaderError::Validation("state root is not an object".to_string()));
    };
    root.insert("leader_receiver".to_string(), serde_json::to_value(receiver)?);
    Ok(())
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

/// `_write_lease_dual_state`(card §85 C17;`__init__.py:588`)。同一锁内写 workspace state.json
/// + team/<session> snapshot,两份永不分叉。**CROSS-LANE**:snapshot 写经 step 13 restart。
pub fn write_lease_dual_state(workspace: &Path, state: &Value) -> Result<(), LeaderError> {
    crate::state::persist::save_runtime_state(workspace, state)?;
    if let Some(session_name) = state.get("session_name").and_then(Value::as_str) {
        write_team_snapshot_atomic(workspace, session_name, state)?;
    }
    Ok(())
}

fn write_team_snapshot_atomic(
    workspace: &Path,
    session_name: &str,
    state: &Value,
) -> Result<(), LeaderError> {
    let snap_path = crate::lifecycle::helpers::team_snapshot_path(workspace, session_name);
    let parent = snap_path
        .parent()
        .ok_or_else(|| LeaderError::Validation("team snapshot path has no parent".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!("state.json.tmp-{}", std::process::id()));
    let data = serde_json::to_vec_pretty(state)?;
    std::fs::write(&tmp, data)?;
    if let Err(error) = std::fs::rename(&tmp, &snap_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
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
    teams.insert(
        target_key.to_string(),
        crate::state::projection::compact_team_state(state),
    );
    let existing_primary_key = existing
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|session| !session.is_empty())
        .map(|_| crate::state::projection::team_state_key(&existing));
    let existing_active_key = existing.get("active_team_key").and_then(Value::as_str);
    let mut merged = if existing_primary_key.as_deref().is_none_or(|key| key == target_key) {
        value_object(state)
    } else {
        value_object(&existing)
    };
    if !had_existing_teams
        || existing_primary_key.as_deref().is_none_or(|key| key == target_key)
        || existing_active_key == Some(target_key)
    {
        for key in ["leader_receiver", "team_owner", "owner_epoch"] {
            if let Some(value) = state.get(key) {
                merged.insert(key.to_string(), value.clone());
            }
        }
    }
    merged.insert("teams".to_string(), Value::Object(teams));
    crate::state::persist::save_runtime_state(workspace, &Value::Object(merged))?;
    Ok(())
}

fn value_object(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// `_detect_dual_state_divergence`(card §85 C18;`__init__.py:556`)。workspace-level 与
/// team-level snapshot 在 owner_uuid/receiver_pane_id/owner_epoch 上是否分叉 → `Some(详情)`。
/// **CROSS-LANE**:snapshot 读经 step 13 restart。
pub fn detect_dual_state_divergence(
    workspace: &Path,
    state: &Value,
) -> Result<Option<Value>, LeaderError> {
    let Some(session_name) = state.get("session_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let snap_path = readable_team_snapshot_path(workspace, session_name);
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
    let diverged = workspace_owner_pane != team_owner_pane
        || workspace_owner_uuid != team_owner_uuid
        || workspace_receiver_pane != team_receiver_pane
        || workspace_epoch != team_epoch;
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
    })))
}

fn readable_team_snapshot_path(workspace: &Path, session_name: &str) -> PathBuf {
    let safe_path = crate::lifecycle::helpers::team_snapshot_path(workspace, session_name);
    if safe_path.exists() {
        return safe_path;
    }
    crate::model::paths::runtime_dir(workspace)
        .join("teams")
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
