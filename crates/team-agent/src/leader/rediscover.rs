//! leader::rediscover — attach-leader readopt and leader receiver rediscovery.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::model::ids::{LeaderSessionUuid, OwnerEpoch};
use crate::provider::Provider;
use crate::transport::{PaneId, PaneInfo, SessionName, Transport, WindowName};

use super::helpers::{get_path_str, now_ts, parse_provider, prefix, resolve_workspace_for_hash, sha1_hex_prefix};
use super::{
    ClaimedVia, Discovery, LeaderError, LeaderEvent, LeaderReceiver, LeaseSource, ReceiverMode,
    ReceiverStatus, TeamOwner,
};

/// `_try_readopt_leader_pane`: attach-leader can re-adopt a live leader pane when
/// the strict uuid validation failed but the pane is still a usable leader in this
/// workspace. Returns the same `(receiver, validation)` shape that attach wiring
/// needs; `None` means caller must continue to the normal refusal/takeover path.
#[allow(clippy::too_many_arguments)]
pub fn try_readopt_leader_pane(
    workspace: &Path,
    state: &mut Value,
    receiver: &mut LeaderReceiver,
    pane_info: &Value,
    targets: &Value,
    owner_record: Option<&TeamOwner>,
    receiver_provider: Provider,
    source: LeaseSource,
    event_log: &crate::event_log::EventLog,
) -> Result<Option<(LeaderReceiver, Value)>, LeaderError> {
    let Some(candidate) = target_from_value(pane_info) else {
        event_log.write(
            LeaderEvent::ReceiverRebindRequired.name(),
            json!({"reason": "leader_pane_missing"}),
        )?;
        return Ok(None);
    };
    if !readopt_candidate_is_usable_leader(workspace, &candidate) {
        event_log.write(
            LeaderEvent::ReceiverRebindRequired.name(),
            json!({"reason": "leader_pane_unusable"}),
        )?;
        return Ok(None);
    }
    if different_live_owner_uuid_mismatch(owner_record, &candidate, targets) {
        return Ok(None);
    }
    let owner_epoch = next_owner_epoch(state, receiver, owner_record);
    let uuid = candidate
        .leader_session_uuid
        .clone()
        .or_else(|| owner_record.and_then(|owner| owner.leader_session_uuid.clone()))
        .or_else(|| receiver.leader_session_uuid.clone());
    let provider = candidate.provider.unwrap_or(receiver_provider);
    let rebound = receiver_from_candidate(&candidate, receiver, provider, uuid.clone(), owner_epoch, Discovery::AttachReadopt);
    let owner = owner_from_candidate(&candidate, provider, uuid, owner_epoch, ClaimedVia::AttachLeader);
    let mut owner_identity = OwnerIdentity::from_state(workspace, state)?;
    if let Some(record) = owner_record {
        owner_identity.pane_id = Some(record.pane_id.as_str().to_string());
        owner_identity.leader_session_uuid = record.leader_session_uuid.clone();
        owner_identity.machine_fingerprint = record.machine_fingerprint.clone();
        owner_identity.provider = Some(record.provider);
    }
    let old_pane_id = old_pane_id(receiver, owner_record);
    let uuid_prefix = uuid_prefix(owner.leader_session_uuid.as_ref());
    write_readopt_state(workspace, state, &rebound, &owner)?;
    *receiver = rebound.clone();
    event_log.write(
        LeaderEvent::OwnerAdoptedOnRestart.name(),
        json!({
            "reason": "attach_readopt",
            "old_pane_id": old_pane_id.as_deref(),
            "new_pane_id": rebound.pane_id.as_str(),
            "owner_epoch": owner_epoch.0,
            "uuid_prefix": uuid_prefix.as_str(),
            "team_id": owner_identity.team_id.as_str(),
        }),
    )?;
    event_log.write(
        LeaderEvent::ReceiverRebindApplied.name(),
        json!({
            "old_pane_id": old_pane_id.as_deref(),
            "new_pane_id": rebound.pane_id.as_str(),
            "reason": "attach_readopt",
            "owner_epoch": owner_epoch.0,
            "uuid_prefix": uuid_prefix.as_str(),
            "team_id": owner_identity.team_id.as_str(),
        }),
    )?;
    event_log.write(
        LeaderEvent::ReceiverAttached.name(),
        json!({
            "target": rebound.pane_id.as_str(),
            "session_name": rebound.session_name.as_ref().map(SessionName::as_str),
            "provider": provider_wire(provider),
            "discovery": "attach_readopt",
            "source": serde_json::to_value(source)?,
            "owner_epoch": owner_epoch.0,
            "uuid_prefix": uuid_prefix.as_str(),
        }),
    )?;
    Ok(Some((
        rebound.clone(),
        json!({
            "ok": true,
            "pane": pane_info,
            "readopted": true,
            "warning": Value::Null,
        }),
    )))
}

/// `_rediscover_leader_receiver`: scan live targets and repair the receiver when
/// exactly one usable pane matches the recorded owner identity.
pub fn rediscover_leader_receiver(
    workspace: &Path,
    state: &mut Value,
    transport: &dyn Transport,
    event_log: &crate::event_log::EventLog,
) -> Result<Value, LeaderError> {
    let identity = OwnerIdentity::from_state(workspace, state)?;
    rediscover_leader_receiver_with_identity(workspace, state, transport, event_log, &identity, None)
}

pub fn rediscover_leader_receiver_with_owner_identity(
    workspace: &Path,
    state: &mut Value,
    transport: &dyn Transport,
    event_log: &crate::event_log::EventLog,
    owner_identity: &Value,
    invalidation_reason: Option<&str>,
) -> Result<Value, LeaderError> {
    let identity = OwnerIdentity::from_raw(owner_identity, true);
    rediscover_leader_receiver_with_identity(
        workspace,
        state,
        transport,
        event_log,
        &identity,
        invalidation_reason,
    )
}

fn rediscover_leader_receiver_with_identity(
    workspace: &Path,
    state: &mut Value,
    transport: &dyn Transport,
    event_log: &crate::event_log::EventLog,
    identity: &OwnerIdentity,
    invalidation_reason: Option<&str>,
) -> Result<Value, LeaderError> {
    let targets = match transport.list_targets() {
        Ok(targets) => targets,
        Err(err) => {
            let error = err.to_string();
            event_log.write(
                "leader_receiver.rediscover_failed",
                json!({
                    "provider": identity.provider.map(provider_wire),
                    "error": error.as_str(),
                }),
            )?;
            emit_failed_rebind_required(event_log, state, identity, invalidation_reason, error.as_str())?;
            return Ok(json!({
                "status": "failed",
                "error": error,
            }));
        }
    };
    rediscover_leader_receiver_from_targets_with_identity(
        workspace,
        state,
        &targets,
        event_log,
        identity,
        invalidation_reason,
    )
}

pub fn rediscover_leader_receiver_from_targets(
    workspace: &Path,
    state: &mut Value,
    targets: &[PaneInfo],
    event_log: &crate::event_log::EventLog,
) -> Result<Value, LeaderError> {
    let identity = OwnerIdentity::from_state(workspace, state)?;
    rediscover_leader_receiver_from_targets_with_identity(
        workspace,
        state,
        targets,
        event_log,
        &identity,
        None,
    )
}

pub fn rediscover_leader_receiver_from_targets_with_owner_identity(
    workspace: &Path,
    state: &mut Value,
    targets: &[PaneInfo],
    event_log: &crate::event_log::EventLog,
    owner_identity: &Value,
    invalidation_reason: Option<&str>,
) -> Result<Value, LeaderError> {
    let identity = OwnerIdentity::from_raw(owner_identity, true);
    rediscover_leader_receiver_from_targets_with_identity(
        workspace,
        state,
        targets,
        event_log,
        &identity,
        invalidation_reason,
    )
}

fn rediscover_leader_receiver_from_targets_with_identity(
    workspace: &Path,
    state: &mut Value,
    targets: &[PaneInfo],
    event_log: &crate::event_log::EventLog,
    identity: &OwnerIdentity,
    invalidation_reason: Option<&str>,
) -> Result<Value, LeaderError> {
    let mut command_candidates: Vec<LeaderTarget> = targets
        .iter()
        .filter_map(LeaderTarget::from_pane_info)
        .filter(rediscover_candidate_is_usable_leader)
        .collect();
    command_candidates.sort_by(|a, b| a.pane_id.as_str().cmp(b.pane_id.as_str()));
    let has_owner_identity = identity.has_match_identity();
    let mut candidates: Vec<LeaderTarget> = if has_owner_identity {
        command_candidates
            .iter()
            .filter(|target| target_matches_owner_identity(target, identity))
            .cloned()
            .collect()
    } else {
        command_candidates.clone()
    };
    if candidates.len() > 1 {
        candidates.sort_by(|a, b| a.pane_id.as_str().cmp(b.pane_id.as_str()));
        let panes = candidate_pane_ids(&candidates);
        let incident_id = ambiguous_incident_id(identity, &panes);
        let deduped = ambiguous_candidates_already_broadcast(event_log, incident_id.as_str())?;
        event_log.write(
            "leader_receiver.rediscover_ambiguous",
            json!({
                "provider": event_provider(identity),
                "old_target": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
                "candidates": panes,
                "owner_identity": owner_identity_value(identity),
                "incident_id": incident_id.as_str(),
                "deduped": deduped,
            }),
        )?;
        if !deduped {
            emit_ambiguous_candidates(
                event_log,
                state,
                identity,
                &candidates,
                incident_id.as_str(),
                identity.from_caller,
            )?;
        }
        if has_owner_identity {
            emit_rebind_required(event_log, "ambiguous", state, identity, invalidation_reason, "confirm rediscover leader receiver")?;
            return Ok(json!({
            "status": "ambiguous",
            "owner_candidates": candidate_values(&candidates, identity.from_caller),
            "owner_identity": owner_identity_value(identity),
            "incident_id": incident_id,
            "deduped": deduped,
            }));
        }
        emit_no_owner_rebind_required(event_log, "ambiguous", state, identity, None)?;
        return Ok(json!({
            "status": "ambiguous",
            "candidates": candidate_values(&candidates, true),
            "incident_id": incident_id,
            "deduped": deduped,
        }));
    }
    let Some(candidate) = candidates.pop() else {
        if has_owner_identity {
            event_log.write(
                "leader_receiver.rediscover_missing",
                json!({
                    "provider": event_provider(identity),
                    "old_target": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
                    "owner_identity": owner_identity_value(identity),
                    "candidate_count": command_candidates.len(),
                }),
            )?;
            emit_owner_missing_rebind_required(event_log, state, identity, invalidation_reason)?;
        } else {
            event_log.write(
                "leader_receiver.rediscover_missing",
                json!({
                    "provider": event_provider(identity),
                    "old_target": old_target_from_state(state),
                }),
            )?;
            emit_no_owner_rebind_required(event_log, "missing", state, identity, None)?;
        }
        return Ok(json!({
            "status": "missing",
            "owner_identity": owner_identity_value(identity),
        }));
    };
    let prior = state_receiver(state);
    let epoch = prior
        .as_ref()
        .and_then(|receiver| receiver.owner_epoch)
        .or_else(|| state_owner_epoch(state))
        .unwrap_or(OwnerEpoch::FIRST);
    let provider = candidate
        .provider
        .or_else(|| prior.as_ref().map(|receiver| receiver.provider))
        .unwrap_or(Provider::Codex);
    let uuid = candidate
        .leader_session_uuid
        .clone()
        .or(identity.leader_session_uuid.clone());
    let old_pane_id = prior
        .as_ref()
        .map(|receiver| receiver.pane_id.as_str().to_string())
        .or_else(|| identity.pane_id.clone());
    let uuid_prefix = uuid_prefix(uuid.as_ref());
    let discovery = if identity.from_caller {
        Discovery::StaleRediscoveryOwnerIdentity
    } else {
        Discovery::StaleRediscoveryUniqueCandidate
    };
    let receiver = receiver_from_candidate(
        &candidate,
        prior.as_ref().unwrap_or(&empty_prior(provider, epoch)),
        provider,
        uuid,
        epoch,
        discovery,
    );
    write_receiver_state(workspace, state, &receiver)?;
    event_log.write(
        LeaderEvent::ReceiverRebindApplied.name(),
        json!({
            "old_pane_id": old_pane_id.as_deref(),
            "new_pane_id": receiver.pane_id.as_str(),
            "reason": invalidation_reason,
            "owner_identity": owner_identity_value(identity),
            "uuid_prefix": uuid_prefix.as_str(),
        }),
    )?;
    event_log.write(
        "leader_receiver.rediscovered",
        json!({
            "provider": provider_wire(provider),
            "old_target": old_pane_id.as_deref(),
            "new_target": receiver.pane_id.as_str(),
            "candidate_count": 1,
            "owner_identity": owner_identity_value(identity),
        }),
    )?;
    Ok(json!({
        "status": "updated",
        "receiver": receiver,
        "owner_identity": owner_identity_value(identity),
    }))
}

#[derive(Clone)]
struct LeaderTarget {
    raw: Value,
    pane_id: PaneId,
    session: Option<SessionName>,
    window_index: Option<String>,
    window_name: Option<WindowName>,
    pane_index: Option<String>,
    tty: Option<String>,
    current_command: Option<String>,
    current_path: Option<PathBuf>,
    active: bool,
    leader_env: BTreeMap<String, String>,
    provider: Option<Provider>,
    leader_session_uuid: Option<LeaderSessionUuid>,
    fingerprint: Option<String>,
}

impl LeaderTarget {
    fn from_pane_info(info: &PaneInfo) -> Option<Self> {
        if !info.active {
            return None;
        }
        let provider = leader_command_provider(info.current_command.as_deref().unwrap_or(""));
        let leader_session_uuid = info
            .leader_env
            .get("TEAM_AGENT_LEADER_SESSION_UUID")
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| serde_json::from_value(Value::String(raw.clone())).ok());
        Some(Self {
            raw: pane_info_value(info, provider, leader_session_uuid.as_ref()),
            pane_id: info.pane_id.clone(),
            session: Some(info.session.clone()),
            window_index: info.window_index.map(|value| value.to_string()),
            window_name: info.window_name.clone(),
            pane_index: info.pane_index.map(|value| value.to_string()),
            tty: info.tty.clone(),
            current_command: info.current_command.clone(),
            current_path: info.current_path.clone(),
            active: info.active,
            leader_env: info.leader_env.clone(),
            provider,
            leader_session_uuid,
            fingerprint: info
                .leader_env
                .get("TEAM_AGENT_MACHINE_FINGERPRINT")
                .filter(|raw| !raw.is_empty())
                .cloned(),
        })
    }
}

struct OwnerIdentity {
    raw: Value,
    from_caller: bool,
    pane_id: Option<String>,
    leader_session_uuid: Option<LeaderSessionUuid>,
    machine_fingerprint: String,
    provider: Option<Provider>,
    team_id: String,
}

impl OwnerIdentity {
    fn from_state(workspace: &Path, state: &Value) -> Result<Self, LeaderError> {
        let identity = super::owner_bind::leader_identity_context(workspace, None, Some(state))?;
        let pane_id = get_path_str(state, &["team_owner", "pane_id"])
            .or_else(|| get_path_str(state, &["leader_receiver", "pane_id"]));
        let leader_session_uuid = get_path_str(state, &["team_owner", "leader_session_uuid"])
            .or_else(|| get_path_str(state, &["leader_receiver", "leader_session_uuid"]))
            .and_then(|raw| serde_json::from_value(Value::String(raw)).ok());
        let provider = get_path_str(state, &["team_owner", "provider"])
            .or_else(|| get_path_str(state, &["leader_receiver", "provider"]))
            .and_then(|raw| parse_provider(&raw));
        let machine_fingerprint = identity.machine_fingerprint;
        let team_id = identity.team_id.as_str().to_string();
        Ok(Self {
            raw: json!({
                "pane_id": pane_id.as_deref(),
                "leader_session_uuid": leader_session_uuid.as_ref().map(LeaderSessionUuid::as_str),
                "machine_fingerprint": machine_fingerprint.as_str(),
                "provider": provider.map(provider_wire),
                "team_id": team_id.as_str(),
            }),
            from_caller: false,
            pane_id,
            leader_session_uuid,
            machine_fingerprint,
            provider,
            team_id,
        })
    }

    fn from_raw(raw: &Value, from_caller: bool) -> Self {
        let pane_id = raw
            .get("pane_id")
            .or_else(|| raw.get("leader_pane_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let leader_session_uuid = raw
            .get("leader_session_uuid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .and_then(|value| serde_json::from_value(Value::String(value.to_string())).ok());
        let machine_fingerprint = raw
            .get("machine_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let provider = raw
            .get("provider")
            .and_then(Value::as_str)
            .and_then(parse_provider);
        let team_id = raw
            .get("team_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Self {
            raw: raw.clone(),
            from_caller,
            pane_id,
            leader_session_uuid,
            machine_fingerprint,
            provider,
            team_id,
        }
    }

    fn has_match_identity(&self) -> bool {
        self.pane_id.is_some() || self.leader_session_uuid.is_some() || !self.machine_fingerprint.is_empty()
    }
}

fn target_from_value(value: &Value) -> Option<LeaderTarget> {
    let pane_id = get_str(value, "pane_id").or_else(|| get_str(value, "pane"))?;
    let current_command = get_str(value, "pane_current_command").or_else(|| get_str(value, "current_command"));
    let provider = current_command.as_deref().and_then(leader_command_provider);
    let leader_env = map_env(value.get("leader_env").or_else(|| value.get("env")));
    let leader_session_uuid = get_str(value, "leader_session_uuid")
        .or_else(|| leader_env.get("TEAM_AGENT_LEADER_SESSION_UUID").cloned())
        .and_then(|raw| serde_json::from_value(Value::String(raw)).ok());
    Some(LeaderTarget {
        raw: value.clone(),
        pane_id: PaneId::new(pane_id),
        session: get_str(value, "session_name")
            .or_else(|| get_str(value, "session"))
            .map(SessionName::new),
        window_index: get_str(value, "window_index"),
        window_name: get_str(value, "window_name").map(WindowName::new),
        pane_index: get_str(value, "pane_index"),
        tty: get_str(value, "pane_tty").or_else(|| get_str(value, "tty")),
        current_command,
        current_path: get_str(value, "pane_current_path")
            .or_else(|| get_str(value, "current_path"))
            .map(PathBuf::from),
        active: value.get("active").and_then(Value::as_bool).unwrap_or(true),
        leader_env,
        provider,
        leader_session_uuid,
        fingerprint: get_str(value, "fingerprint").or_else(|| get_str(value, "machine_fingerprint")),
    })
}

fn map_env(value: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(obj) = value.and_then(Value::as_object) else {
        return out;
    };
    for (key, value) in obj {
        if let Some(text) = value.as_str().filter(|s| !s.is_empty()) {
            out.insert(key.clone(), text.to_string());
        }
    }
    out
}

fn target_iter(targets: &Value) -> Vec<LeaderTarget> {
    if let Some(items) = targets.as_array() {
        return items.iter().filter_map(target_from_value).collect();
    }
    for key in ["targets", "panes"] {
        if let Some(items) = targets.get(key).and_then(Value::as_array) {
            return items.iter().filter_map(target_from_value).collect();
        }
    }
    Vec::new()
}

fn readopt_candidate_is_usable_leader(workspace: &Path, target: &LeaderTarget) -> bool {
    if !target.active || target.provider.is_none() {
        return false;
    }
    target
        .current_path
        .as_deref()
        .is_some_and(|path| path_in_workspace(path, workspace))
}

fn rediscover_candidate_is_usable_leader(target: &LeaderTarget) -> bool {
    target.active
        && target
            .current_command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty())
}

fn path_in_workspace(path: &Path, workspace: &Path) -> bool {
    let cwd = resolve_workspace_for_hash(path);
    let root = resolve_workspace_for_hash(workspace);
    cwd == root || cwd.starts_with(root)
}

fn leader_command_provider(command: &str) -> Option<Provider> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("claude") {
        Some(Provider::ClaudeCode)
    } else if lower.contains("codex") {
        Some(Provider::Codex)
    } else if lower.contains("fake") {
        Some(Provider::Fake)
    } else {
        None
    }
}

fn target_matches_owner_identity(target: &LeaderTarget, identity: &OwnerIdentity) -> bool {
    identity
        .pane_id
        .as_deref()
        .is_some_and(|pane| pane == target.pane_id.as_str())
        || identity
            .leader_session_uuid
            .as_ref()
            .is_some_and(|uuid| target.leader_session_uuid.as_ref() == Some(uuid))
        || env_triple_matches(target, identity)
}

fn pane_info_value(
    info: &PaneInfo,
    provider: Option<Provider>,
    leader_session_uuid: Option<&LeaderSessionUuid>,
) -> Value {
    json!({
        "pane_id": info.pane_id.as_str(),
        "session_name": info.session.as_str(),
        "window_index": info.window_index,
        "window_name": info.window_name.as_ref().map(WindowName::as_str),
        "pane_index": info.pane_index,
        "pane_tty": info.tty,
        "pane_current_command": info.current_command,
        "pane_current_path": info.current_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "fingerprint": info.leader_env.get("TEAM_AGENT_MACHINE_FINGERPRINT"),
        "provider": provider.map(provider_wire),
        "leader_session_uuid": leader_session_uuid.map(LeaderSessionUuid::as_str),
    })
}

fn candidate_pane_ids(candidates: &[LeaderTarget]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.pane_id.as_str().to_string())
        .collect()
}

fn ambiguous_incident_id(identity: &OwnerIdentity, panes: &[String]) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(identity.provider.map(provider_wire).unwrap_or("").as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(identity.team_id.as_bytes());
    bytes.push(0);
    if let Some(uuid) = &identity.leader_session_uuid {
        bytes.extend_from_slice(uuid.as_str().as_bytes());
    }
    for pane in panes {
        bytes.push(0);
        bytes.extend_from_slice(pane.as_bytes());
    }
    format!("rediscover_{}", sha1_hex_prefix(&bytes, 12))
}

fn old_target_from_state(state: &Value) -> Option<String> {
    state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("pane_id"))
        .and_then(Value::as_str)
        .filter(|pane| !pane.is_empty())
        .map(str::to_string)
}

fn emit_failed_rebind_required(
    event_log: &crate::event_log::EventLog,
    state: &Value,
    identity: &OwnerIdentity,
    invalidation_reason: Option<&str>,
    error: &str,
) -> Result<(), LeaderError> {
    event_log.write(
        LeaderEvent::ReceiverRebindRequired.name(),
        json!({
            "old_pane_id": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
            "reason": invalidation_reason,
            "provider": identity.provider.map(provider_wire),
            "team_id": empty_to_null(identity.team_id.as_str()),
            "rediscovery_status": "failed",
            "error": error,
        }),
    )?;
    Ok(())
}

fn emit_ambiguous_candidates(
    event_log: &crate::event_log::EventLog,
    state: &Value,
    identity: &OwnerIdentity,
    candidates: &[LeaderTarget],
    incident_id: &str,
    golden_shape: bool,
) -> Result<(), LeaderError> {
    let panes = candidate_pane_ids(candidates);
    if !golden_shape {
        event_log.write(
            LeaderEvent::ReceiverAmbiguousCandidates.name(),
            json!({
                "incident_id": incident_id,
                "pane_ids": panes,
                "provider": identity.provider.map(provider_wire),
                "team_id": empty_to_null(identity.team_id.as_str()),
                "uuid_prefix": uuid_prefix(identity.leader_session_uuid.as_ref()).as_str(),
                "debounce_bucket": incident_id,
                "reason": "force_confirm_required",
                "old_pane_id": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
                "owner_identity": owner_identity_value(identity),
                "invalidation_reason": Value::Null,
                "queued": candidate_queue_values(candidates),
            }),
        )?;
        return Ok(());
    }
    event_log.write(
        LeaderEvent::ReceiverAmbiguousCandidates.name(),
        json!({
            "incident_id": incident_id,
            "candidates": panes,
            "provider": event_provider(identity),
            "team_id": empty_to_null(identity.team_id.as_str()),
            "uuid_prefix": uuid_prefix(identity.leader_session_uuid.as_ref()).as_str(),
            "debounce_bucket": incident_id,
            "reason": "force_confirm_required",
            "old_pane_id": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
        }),
    )?;
    for candidate in candidates {
        event_log.write(
            "leader_receiver.ambiguous_candidate_queued",
            json!({
                "incident_id": incident_id,
                "pane_id": candidate.pane_id.as_str(),
                "ok": true,
                "error": Value::Null,
            }),
        )?;
    }
    Ok(())
}

fn ambiguous_candidates_already_broadcast(
    event_log: &crate::event_log::EventLog,
    incident_id: &str,
) -> Result<bool, LeaderError> {
    Ok(event_log.tail(200)?.iter().any(|event| {
        event.get("event").and_then(Value::as_str) == Some(LeaderEvent::ReceiverAmbiguousCandidates.name())
            && event.get("incident_id").and_then(Value::as_str) == Some(incident_id)
    }))
}

fn emit_no_owner_rebind_required(
    event_log: &crate::event_log::EventLog,
    rediscovery_status: &str,
    state: &Value,
    identity: &OwnerIdentity,
    reason: Option<&str>,
) -> Result<(), LeaderError> {
    event_log.write(
        LeaderEvent::ReceiverRebindRequired.name(),
        json!({
            "old_pane_id": old_target_from_state(state),
            "reason": reason,
            "provider": event_provider(identity),
            "team_id": empty_to_null(identity.team_id.as_str()),
            "rediscovery_status": rediscovery_status,
        }),
    )?;
    Ok(())
}

fn emit_owner_missing_rebind_required(
    event_log: &crate::event_log::EventLog,
    state: &Value,
    identity: &OwnerIdentity,
    invalidation_reason: Option<&str>,
) -> Result<(), LeaderError> {
    let recovery_action = if identity.from_caller {
        "open the owning leader pane or run team-agent claim-leader --confirm from a matching pane"
    } else {
        "run team-agent attach-leader or claim-leader"
    };
    event_log.write(
        LeaderEvent::ReceiverRebindRequired.name(),
        json!({
            "old_pane_id": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
            "reason": invalidation_reason,
            "provider": event_provider(identity),
            "team_id": empty_to_null(identity.team_id.as_str()),
            "owner_identity": owner_identity_value(identity),
            "uuid_prefix": uuid_prefix(identity.leader_session_uuid.as_ref()).as_str(),
            "recovery_action": recovery_action,
        }),
    )?;
    Ok(())
}

fn emit_rebind_required(
    event_log: &crate::event_log::EventLog,
    reason: &str,
    state: &Value,
    identity: &OwnerIdentity,
    invalidation_reason: Option<&str>,
    recovery_action: &str,
) -> Result<(), LeaderError> {
    event_log.write(
        LeaderEvent::ReceiverRebindRequired.name(),
        json!({
            "old_pane_id": old_target_from_state(state).or_else(|| identity.pane_id.clone()),
            "reason": invalidation_reason,
            "provider": identity.provider.map(provider_wire),
            "team_id": empty_to_null(identity.team_id.as_str()),
            "owner_identity": owner_identity_value(identity),
            "uuid_prefix": uuid_prefix(identity.leader_session_uuid.as_ref()).as_str(),
            "rediscovery_status": reason,
            "recovery_action": recovery_action,
        }),
    )?;
    Ok(())
}

fn owner_identity_value(identity: &OwnerIdentity) -> Value {
    identity.raw.clone()
}

fn candidate_values(candidates: &[LeaderTarget], golden_spelling: bool) -> Value {
    Value::Array(
        candidates
            .iter()
            .map(|candidate| {
                if golden_spelling {
                    candidate.raw.clone()
                } else {
                    legacy_candidate_value(candidate)
                }
            })
            .collect(),
    )
}

fn legacy_candidate_value(candidate: &LeaderTarget) -> Value {
    json!({
        "pane_id": candidate.pane_id.as_str(),
        "session_name": candidate.session.as_ref().map(SessionName::as_str),
        "window_index": optional_numeric_string_value(candidate.window_index.as_deref()),
        "window_name": candidate.window_name.as_ref().map(WindowName::as_str),
        "pane_index": optional_numeric_string_value(candidate.pane_index.as_deref()),
        "pane_tty": candidate.tty,
        "current_command": candidate.current_command,
        "current_path": candidate.current_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "fingerprint": candidate.fingerprint,
        "provider": candidate.provider.map(provider_wire),
        "leader_session_uuid": candidate.leader_session_uuid.as_ref().map(LeaderSessionUuid::as_str),
    })
}

fn optional_numeric_string_value(value: Option<&str>) -> Value {
    match value {
        Some(raw) => raw
            .parse::<u64>()
            .map_or_else(|_| Value::String(raw.to_string()), |parsed| json!(parsed)),
        None => Value::Null,
    }
}

fn candidate_queue_values(candidates: &[LeaderTarget]) -> Value {
    Value::Array(
        candidates
            .iter()
            .map(|candidate| {
                json!({
                    "pane_id": candidate.pane_id.as_str(),
                    "queued": true,
                })
            })
            .collect(),
    )
}

fn empty_to_null(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn old_pane_id(receiver: &LeaderReceiver, owner_record: Option<&TeamOwner>) -> Option<String> {
    owner_record
        .map(|owner| owner.pane_id.as_str().to_string())
        .or_else(|| Some(receiver.pane_id.as_str().to_string()).filter(|pane| !pane.is_empty()))
}

fn uuid_prefix(uuid: Option<&LeaderSessionUuid>) -> String {
    uuid.map_or_else(String::new, |value| prefix(value.as_str(), 8).to_string())
}

fn provider_wire(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini_cli",
        Provider::Fake => "fake",
    }
}

fn event_provider(identity: &OwnerIdentity) -> &'static str {
    provider_wire(identity.provider.unwrap_or(Provider::Codex))
}

fn env_triple_matches(target: &LeaderTarget, identity: &OwnerIdentity) -> bool {
    target
        .leader_env
        .get("TEAM_AGENT_LEADER_PANE_ID")
        .is_some_and(|value| identity.pane_id.as_deref() == Some(value.as_str()))
        && target
            .leader_env
            .get("TEAM_AGENT_LEADER_PROVIDER")
            .is_some_and(|value| identity.provider.is_some_and(|provider| value == provider_wire(provider)))
        && target
        .leader_env
        .get("TEAM_AGENT_MACHINE_FINGERPRINT")
        .is_some_and(|value| value == &identity.machine_fingerprint)
}

fn different_live_owner_uuid_mismatch(
    owner_record: Option<&TeamOwner>,
    candidate: &LeaderTarget,
    targets: &Value,
) -> bool {
    let Some(owner) = owner_record else {
        return false;
    };
    if owner.pane_id.as_str() == candidate.pane_id.as_str() {
        return false;
    }
    let Some(owner_uuid) = &owner.leader_session_uuid else {
        return false;
    };
    if candidate.leader_session_uuid.as_ref() == Some(owner_uuid) {
        return false;
    }
    target_iter(targets)
        .iter()
        .any(|target| target.active && target.pane_id.as_str() == owner.pane_id.as_str())
}

fn next_owner_epoch(
    state: &Value,
    receiver: &LeaderReceiver,
    owner_record: Option<&TeamOwner>,
) -> OwnerEpoch {
    let current = owner_record
        .map(|owner| owner.owner_epoch.0)
        .or_else(|| state_owner_epoch(state).map(|epoch| epoch.0))
        .or_else(|| receiver.owner_epoch.map(|epoch| epoch.0))
        .unwrap_or(0);
    OwnerEpoch(current.saturating_add(1))
}

fn state_owner_epoch(state: &Value) -> Option<OwnerEpoch> {
    state
        .get("team_owner")
        .and_then(|owner| owner.get("owner_epoch"))
        .and_then(Value::as_u64)
        .or_else(|| {
            state
                .get("leader_receiver")
                .and_then(|receiver| receiver.get("owner_epoch"))
                .and_then(Value::as_u64)
        })
        .map(OwnerEpoch)
}

fn owner_from_candidate(
    candidate: &LeaderTarget,
    provider: Provider,
    uuid: Option<LeaderSessionUuid>,
    epoch: OwnerEpoch,
    claimed_via: ClaimedVia,
) -> TeamOwner {
    TeamOwner {
        pane_id: candidate.pane_id.clone(),
        provider,
        machine_fingerprint: candidate
            .fingerprint
            .clone()
            .or_else(|| candidate.leader_env.get("TEAM_AGENT_MACHINE_FINGERPRINT").cloned())
            .unwrap_or_default(),
        leader_session_uuid: uuid,
        owner_epoch: epoch,
        claimed_at: now_ts(),
        claimed_via,
        os_user: None,
    }
}

fn receiver_from_candidate(
    target: &LeaderTarget,
    prior: &LeaderReceiver,
    provider: Provider,
    uuid: Option<LeaderSessionUuid>,
    epoch: OwnerEpoch,
    discovery: Discovery,
) -> LeaderReceiver {
    LeaderReceiver {
        mode: ReceiverMode::DirectTmux,
        status: ReceiverStatus::Attached,
        provider,
        pane_id: target.pane_id.clone(),
        session_name: target.session.clone(),
        window_index: target.window_index.clone(),
        window_name: target.window_name.clone(),
        pane_index: target.pane_index.clone(),
        pane_tty: target.tty.clone(),
        pane_current_command: target.current_command.clone(),
        fingerprint: target.fingerprint.clone(),
        leader_session_uuid: uuid,
        owner_epoch: Some(epoch),
        attached_at: Some(now_ts()),
        discovery: Some(discovery),
        requested_provider: prior.requested_provider,
        warning: prior.warning.clone(),
    }
}

fn empty_prior(provider: Provider, epoch: OwnerEpoch) -> LeaderReceiver {
    LeaderReceiver {
        mode: ReceiverMode::DirectTmux,
        status: ReceiverStatus::Attached,
        provider,
        pane_id: PaneId::new(""),
        session_name: None,
        window_index: None,
        window_name: None,
        pane_index: None,
        pane_tty: None,
        pane_current_command: None,
        fingerprint: None,
        leader_session_uuid: None,
        owner_epoch: Some(epoch),
        attached_at: None,
        discovery: None,
        requested_provider: None,
        warning: None,
    }
}

fn state_receiver(state: &Value) -> Option<LeaderReceiver> {
    state
        .get("leader_receiver")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn write_readopt_state(
    workspace: &Path,
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
    crate::leader::write_lease_dual_state(workspace, state)
}

fn write_receiver_state(
    workspace: &Path,
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
    crate::leader::write_lease_dual_state(workspace, state)
}

fn get_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|raw| {
            raw.as_str()
                .map(str::to_string)
                .or_else(|| raw.as_u64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests;
