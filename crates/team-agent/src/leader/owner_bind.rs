//! leader::owner_bind — Family A 正源 owner 绑定(bind_owner_from_caller_pane / emit_owner_bound_event)
//! + leader 身份上下文派生(override / state / derive)。

use std::path::Path;

use serde_json::{json, Value};

use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TeamKey};
use crate::provider::Provider;
use crate::tmux_backend::TmuxBackend;
use crate::transport::{PaneField, PaneId, Target, Transport};

use super::helpers::{get_path_str, now_ts, prefix, resolve_workspace_for_hash};
use super::{
    ClaimedVia, LeaderError, LeaderEvent, LeaderIdentity, LeaderSessionUuidSource, LeaseReason,
    OwnerBindResult, TeamOwner,
};

// ── leader::identity — leader_identity / 身份上下文 ──

/// `leader_identity`(card §47;`__init__.py:355`)。`team-agent identity` 入口。
/// 返回 uuid_prefix + 身份字段(JSON dict,CLI 直出)。
pub fn leader_identity(workspace: &Path, team: Option<&str>) -> Result<Value, LeaderError> {
    let state = crate::state::persist::load_runtime_state(workspace)?;
    let identity = leader_identity_context(workspace, team, Some(&state))?;
    Ok(json!({
        "ok": true,
        "uuid_prefix": prefix(identity.leader_session_uuid.as_str(), 12),
        "machine_fingerprint": identity.machine_fingerprint,
        "workspace_abspath": identity.workspace_abspath.to_string_lossy(),
        "os_user": identity.os_user,
        "team_id": identity.team_id.as_str(),
        "current_pane_id": std::env::var("TEAM_AGENT_LEADER_PANE_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("TMUX_PANE").ok().filter(|v| !v.is_empty())),
        "last_seen_at": get_path_str(&state, &["leader_receiver", "attached_at"])
            .or_else(|| get_path_str(&state, &["leader_receiver", "last_seen_at"])),
        "source": serde_json::to_value(identity.leader_session_uuid_source)?,
    }))
}

/// `_leader_identity_context`(`__init__.py:192`)。派生 leader 身份上下文(override / state / derive)。
pub fn leader_identity_context(
    workspace: &Path,
    team: Option<&str>,
    state: Option<&Value>,
) -> Result<LeaderIdentity, LeaderError> {
    let team_id = TeamKey::new(match team {
        Some(t) => t.to_string(),
        None => state
            .map(crate::state::projection::team_state_key)
            .unwrap_or_else(|| "current".to_string()),
    });
    let workspace_abspath = resolve_workspace_for_hash(workspace);
    let machine_fingerprint = state
        .and_then(|s| get_path_str(s, &["team_owner", "machine_fingerprint"]))
        .or_else(|| std::env::var("TEAM_AGENT_MACHINE_FINGERPRINT").ok())
        .unwrap_or_default();
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    if let Ok(raw) = std::env::var("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") {
        if !raw.is_empty() {
            return Ok(LeaderIdentity {
                leader_session_uuid: serde_json::from_value(Value::String(raw))?,
                leader_session_uuid_source: LeaderSessionUuidSource::Override,
                machine_fingerprint,
                workspace_abspath,
                os_user,
                team_id,
            });
        }
    }
    if let Some(state_uuid) = state
        .and_then(|s| get_path_str(s, &["team_owner", "leader_session_uuid"]))
        .or_else(|| state.and_then(|s| get_path_str(s, &["leader_receiver", "leader_session_uuid"])))
    {
        return Ok(LeaderIdentity {
            leader_session_uuid: serde_json::from_value(Value::String(state_uuid))?,
            leader_session_uuid_source: LeaderSessionUuidSource::Derived,
            machine_fingerprint,
            workspace_abspath,
            os_user,
            team_id,
        });
    }
    let leader_session_uuid = LeaderSessionUuid::derive(
        &machine_fingerprint,
        &workspace_abspath.to_string_lossy(),
        &os_user,
        team_id.as_str(),
    )?;
    Ok(LeaderIdentity {
        leader_session_uuid,
        leader_session_uuid_source: LeaderSessionUuidSource::Derived,
        machine_fingerprint,
        workspace_abspath,
        os_user,
        team_id,
    })
}

// ── leader::binding — Family A 正源 owner 绑定 + derive_leader_session_uuid ──

/// `bind_owner_from_caller_pane`(card §49;`leader_binding.py:46`)。Family A 正源 owner 绑定:
/// 身份只来自 `$TMUX_PANE` + 一次定向 `tmux display-message` 查 `pane_current_command`。
/// 缺 `$TMUX_PANE` → refuse + `owner.bind_refused`(`reason=caller_pane_missing`)。
pub fn bind_owner_from_caller_pane(
    workspace: &Path,
    team_id: &TeamKey,
    override_uuid: Option<&LeaderSessionUuid>,
) -> Result<OwnerBindResult, LeaderError> {
    let event_log = crate::event_log::EventLog::new(workspace);
    let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else {
        let hint = "run team-agent from inside your leader pane (the tmux pane you want to own this team).";
        event_log.write(
            LeaderEvent::OwnerBindRefused.name(),
            json!({
                "reason": serde_json::to_value(LeaseReason::CallerPaneMissing)?,
                "caller_pane_id": "",
                "caller_current_command": "",
                "team_id": team_id.as_str(),
                "hint": hint,
            }),
        )?;
        return Ok(OwnerBindResult {
            ok: false,
            owner: None,
            caller_pane_id: PaneId::new(""),
            caller_current_command: String::new(),
            team_id: team_id.clone(),
            reason: Some(LeaseReason::CallerPaneMissing),
            hint: Some(hint.to_string()),
        });
    };
    let caller_current_command = tmux_pane_current_command(workspace, &pane).unwrap_or_default();
    let provider = bind_provider_from_env_or_command(&caller_current_command);
    let machine_fingerprint = std::env::var("TEAM_AGENT_MACHINE_FINGERPRINT").unwrap_or_default();
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let identity = family_a_identity(workspace, team_id, override_uuid, &machine_fingerprint, &os_user)?;
    let owner = TeamOwner {
        pane_id: PaneId::new(pane.clone()),
        provider,
        machine_fingerprint: machine_fingerprint.clone(),
        leader_session_uuid: Some(identity.leader_session_uuid),
        owner_epoch: OwnerEpoch::FIRST,
        claimed_at: now_ts(),
        claimed_via: ClaimedVia::ClaimLeader,
        os_user: Some(os_user),
    };
    Ok(OwnerBindResult {
        ok: true,
        owner: Some(owner),
        caller_pane_id: PaneId::new(pane),
        caller_current_command,
        team_id: team_id.clone(),
        reason: None,
        hint: None,
    })
}

/// `emit_owner_bound_event`(`leader_binding.py:162`)。成功绑定后的审计 hook
/// (`owner.bound_from_caller_pane`;只写 uuid 短前缀,不泄全 uuid)。
pub fn emit_owner_bound_event(
    workspace: &Path,
    caller_pane_id: &PaneId,
    caller_current_command: &str,
    derived_leader_session_uuid: &LeaderSessionUuid,
    team_id: &TeamKey,
    old_leader_session_uuid: Option<&LeaderSessionUuid>,
) -> Result<(), LeaderError> {
    crate::event_log::EventLog::new(workspace).write(
        LeaderEvent::OwnerBoundFromCallerPane.name(),
        json!({
            "caller_pane_id": caller_pane_id.as_str(),
            "caller_current_command": caller_current_command,
            "derived_uuid_prefix": prefix(derived_leader_session_uuid.as_str(), 12),
            "old_uuid_prefix": old_leader_session_uuid.map_or("", |u| prefix(u.as_str(), 12)),
            "team_id": team_id.as_str(),
        }),
    )?;
    Ok(())
}

fn bind_provider_from_env_or_command(command: &str) -> Provider {
    std::env::var("TEAM_AGENT_LEADER_PROVIDER")
        .ok()
        .and_then(|raw| super::helpers::parse_provider(&raw))
        .unwrap_or_else(|| provider_from_command(command))
}

fn provider_from_command(command: &str) -> Provider {
    match exact_command_name(command).as_deref() {
        Some("claude") | Some("claude.exe") => Provider::Claude,
        Some("codex") => Provider::Codex,
        Some("fake") => Provider::Fake,
        _ => Provider::Codex,
    }
}

fn exact_command_name(command: &str) -> Option<String> {
    let last = command
        .split_whitespace()
        .next()
        .unwrap_or(command)
        .rsplit(['/', '\\'])
        .next()?;
    let lower = last.to_ascii_lowercase();
    if lower.is_empty() { None } else { Some(lower) }
}

pub fn owner_bind_provider_wire(command: &str) -> &'static str {
    if let Ok(raw) = std::env::var("TEAM_AGENT_LEADER_PROVIDER") {
        return match raw.as_str() {
            "claude" => "claude",
            "claude_code" => "claude_code",
            "codex" => "codex",
            "gemini_cli" => "gemini_cli",
            "fake" => "fake",
            _ => "",
        };
    }
    match exact_command_name(command).as_deref() {
        Some("claude") | Some("claude.exe") => "claude",
        Some("codex") => "codex",
        Some("fake") => "fake",
        _ => "",
    }
}

fn family_a_identity(
    workspace: &Path,
    team_id: &TeamKey,
    override_uuid: Option<&LeaderSessionUuid>,
    machine_fingerprint: &str,
    os_user: &str,
) -> Result<LeaderIdentity, LeaderError> {
    if let Some(uuid) = override_uuid {
        return Ok(LeaderIdentity {
            leader_session_uuid: uuid.clone(),
            leader_session_uuid_source: LeaderSessionUuidSource::Override,
            machine_fingerprint: machine_fingerprint.to_string(),
            workspace_abspath: resolve_workspace_for_hash(workspace),
            os_user: os_user.to_string(),
            team_id: team_id.clone(),
        });
    }
    if let Ok(raw) = std::env::var("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") {
        if !raw.is_empty() {
            return Ok(LeaderIdentity {
                leader_session_uuid: serde_json::from_value(Value::String(raw))?,
                leader_session_uuid_source: LeaderSessionUuidSource::Override,
                machine_fingerprint: machine_fingerprint.to_string(),
                workspace_abspath: resolve_workspace_for_hash(workspace),
                os_user: os_user.to_string(),
                team_id: team_id.clone(),
            });
        }
    }
    let workspace_abspath = resolve_workspace_for_hash(workspace);
    let leader_session_uuid = LeaderSessionUuid::derive(
        machine_fingerprint,
        &workspace_abspath.to_string_lossy(),
        os_user,
        team_id.as_str(),
    )?;
    Ok(LeaderIdentity {
        leader_session_uuid,
        leader_session_uuid_source: LeaderSessionUuidSource::Derived,
        machine_fingerprint: machine_fingerprint.to_string(),
        workspace_abspath,
        os_user: os_user.to_string(),
        team_id: team_id.clone(),
    })
}

fn tmux_pane_current_command(workspace: &Path, pane: &str) -> Result<String, LeaderError> {
    TmuxBackend::for_workspace(workspace)
        .query(
            &Target::Pane(PaneId::new(pane)),
            PaneField::PaneCurrentCommand,
        )
        .map(|value| value.unwrap_or_default())
        .map_err(|e| LeaderError::Tmux(e.to_string()))
}

// NOTE: `derive_leader_session_uuid`(`leader_binding.py:146`)已由
// `model::ids::LeaderSessionUuid::derive` 字节对齐实现(含 NUL 拒绝 + golden 测试)——
// 此 lane REUSE 之,不重声明。
