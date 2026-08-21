//! ---
//! purpose: leader pane 环境变量的校验，以及 quick-start 场景下 owner 与 provider 的归因
//! contract:
//!   provides:
//!     - name: validate_active_leader_pane_env
//!       what: TEAM_AGENT_LEADER_PANE_ID 指向死或不存在的 pane 时 fail-fast
//!     - name: active_leader_pane_state_across_transports
//!       what: 跨多个 tmux socket 汇总某 pane 的存活判定
//!     - name: seed_unbound_launched_owner
//!       what: 无 caller pane 时种入一份 unbound owner 与 receiver
//!     - name: attributed_provider_for_pane_across_tmux_sockets
//!       what: 跨所有 tmux socket 找到该 pane 并归因它的 provider
//!   depends:
//!     - crate::tmux_backend
//!     - crate::transport::Transport
//!     - crate::state::ownership
//!     - crate::event_log::EventLog
//!     - crate::leader
//!     - crate::compiler
//! boundary:
//!   - 只在显式判定为 dead 或 absent 时拦截，Unknown 一律放行
//!   - 归因不出 provider 时留空，绝不默认成某个 provider
//!   - 只校验与归因，不 spawn、不改 pane
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::profile_launch::parse_provider;
use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

/// ---
/// purpose: 校验 TEAM_AGENT_LEADER_PANE_ID 指向的 pane 是否可用，不带 workspace 上下文
/// returns: 通过返回空值
/// errors: pane 明确为 dead 或 absent 时返回 RequirementUnmet，消息含 error/action/log 三行
/// contract_id: lifecycle.leader_context.validate_leader_pane_env
/// ---
/// B-7 / 036b — TEAM_AGENT_LEADER_PANE_ID 主动路径 fail-fast helper。
/// 入口形态(N38 三行式):
///   error  : `TEAM_AGENT_LEADER_PANE_ID points at a dead/absent pane: %<id>`
///   action : `unset TEAM_AGENT_LEADER_PANE_ID, or set it to a live tmux pane id`
///   log    : `TEAM_AGENT_LEADER_PANE_ID=%<id>`
/// env 未设(或空)→ Ok(())。
/// env 设了但 pane 可判定为 Dead/Absent → Err(RequirementUnmet)。
/// 真实 tmux 后端跨所有现存 tmux socket server 探测:TEAM_AGENT_LEADER_PANE_ID 是用户
/// override 指针,不归属当前 team socket。
/// probe 返 Unknown 不挡(被动路径降级):本主动路径只对【显式 Dead/Absent】fail-fast,
/// MUST-17 不过度设计 / unset 走 pass-through(b7_unset_leader_pane_env_passes_through 守)。
pub(crate) fn validate_active_leader_pane_env(
    transport: &dyn Transport,
) -> Result<(), LifecycleError> {
    validate_active_leader_pane_env_with_workspaces(transport, &[])
}

/// ---
/// purpose: 同上，附带一个 workspace 用于写告警事件
/// returns: 通过返回空值
/// errors: 同 validate_active_leader_pane_env
/// contract_id: lifecycle.leader_context.validate_leader_pane_env
/// ---
pub(crate) fn validate_active_leader_pane_env_with_workspace(
    transport: &dyn Transport,
    workspace: Option<&Path>,
) -> Result<(), LifecycleError> {
    let workspaces = workspace.into_iter().collect::<Vec<_>>();
    validate_active_leader_pane_env_with_workspaces(transport, &workspaces)
}

/// ---
/// purpose: 校验 leader pane 环境变量的实体实现
/// params:
///   workspaces: 写格式告警事件的候选 workspace 列表，可为空
/// returns: 环境变量未设或为空、pane 格式不合法、判定为 live 或 unknown 时都通过
/// errors: pane 明确 dead 或 absent 时返回 RequirementUnmet
/// contract_id: lifecycle.leader_context.validate_leader_pane_env
/// ---
pub(crate) fn validate_active_leader_pane_env_with_workspaces(
    transport: &dyn Transport,
    workspaces: &[&Path],
) -> Result<(), LifecycleError> {
    let pane_id_raw = match std::env::var("TEAM_AGENT_LEADER_PANE_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };
    let pane = crate::transport::PaneId::new(&pane_id_raw);
    if !is_tmux_pane_id_format(&pane) {
        write_invalid_leader_pane_env_warning(workspaces, &pane_id_raw);
        return Ok(());
    }
    let failure = match leader_pane_env_state_for_validation(transport, &pane) {
        LeaderPaneEnvState::Dead => Some("dead"),
        LeaderPaneEnvState::Absent => Some("absent"),
        LeaderPaneEnvState::Live | LeaderPaneEnvState::Unknown => None,
    };
    let Some(reason) = failure else {
        return Ok(());
    };
    Err(LifecycleError::RequirementUnmet(format!(
        "TEAM_AGENT_LEADER_PANE_ID points at a {reason} pane: {pane_id_raw}\n\
         action: unset TEAM_AGENT_LEADER_PANE_ID, or set it to a live tmux pane id\n\
         log: TEAM_AGENT_LEADER_PANE_ID={pane_id_raw}"
    )))
}

/// ---
/// purpose: pane id 格式不合法时写一条告警事件
/// params:
///   workspaces: 逐个去重后尝试写事件
/// returns: 所有 workspace 都没写成时退回 stderr 打印
/// ---
pub(super) fn write_invalid_leader_pane_env_warning(workspaces: &[&Path], pane_id_raw: &str) {
    let message = "invalid pane id format, skipping validation";
    let mut wrote = false;
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for workspace in workspaces {
        let key = workspace.to_string_lossy().to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        match crate::event_log::EventLog::new(workspace).write(
            "leader_pane_env.validation_warning",
            serde_json::json!({
                "env": "TEAM_AGENT_LEADER_PANE_ID",
                "value": pane_id_raw,
                "warning": message,
            }),
        ) {
            Ok(_) => wrote = true,
            Err(err) => errors.push(format!("{key}: {err}")),
        }
    }
    if !wrote {
        eprintln!("TEAM_AGENT_LEADER_PANE_ID={pane_id_raw}: {message}");
        if !errors.is_empty() {
            eprintln!(
                "TEAM_AGENT_LEADER_PANE_ID warning event write failed: {}",
                errors.join("; ")
            );
        }
    }
}

/// ---
/// purpose: TEAM.md 里写了 owner_team_id 时告知它被忽略
/// params:
///   runtime_team_key: 真正生效的 canonical 团队键
/// returns: TEAM.md 没写该字段时什么都不做；否则打印三行提示并写 spec.field_ignored 事件
/// ---
pub(super) fn warn_ignored_owner_team_id(
    workspace: &Path,
    team_dir: &Path,
    runtime_team_key: &str,
) {
    let Ok(Some(ignored)) = crate::compiler::ignored_owner_team_id_from_team_md(team_dir) else {
        return;
    };
    eprintln!(
        "Warning: ignored TEAM.md {}={}",
        ignored.field, ignored.value
    );
    eprintln!("Reason: owner identity is the canonical runtime team key ({runtime_team_key}), not TEAM.md front matter");
    eprintln!("Action: remove {} from TEAM.md", ignored.field);
    if let Err(err) = crate::event_log::EventLog::new(workspace).write(
        "spec.field_ignored",
        serde_json::json!({
            "field": ignored.field,
            "source": team_dir.join("TEAM.md").to_string_lossy().to_string(),
            "value": ignored.value,
            "warning": "ignored user-set owner_team_id",
            "reason": "owner identity is derived from the canonical runtime team key",
            "action": "remove owner_team_id from TEAM.md",
            "runtime_team_key": runtime_team_key,
        }),
    ) {
        eprintln!("Warning: spec.field_ignored event write failed: {err}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaderPaneEnvState {
    Live,
    Dead,
    Absent,
    Unknown,
}

/// ---
/// purpose: 定出校验用的 pane 状态
/// returns: 格式不合法返回 Unknown；transport 探真实 socket 时跨 socket 判定，否则只问该 transport
/// ---
pub(super) fn leader_pane_env_state_for_validation(
    transport: &dyn Transport,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    if !is_tmux_pane_id_format(pane) {
        return LeaderPaneEnvState::Unknown;
    }
    if transport.probes_real_tmux_socket_roots() {
        return active_leader_pane_state_across_tmux_sockets(pane);
    }
    active_leader_pane_state(transport, pane)
}

/// ---
/// purpose: 判断字符串是否是 tmux pane id 形状
/// returns: 以百分号起头且其后全为数字时为 true
/// ---
pub(super) fn is_tmux_pane_id_format(pane: &crate::transport::PaneId) -> bool {
    let pane = pane.as_str();
    pane.len() > 1 && pane.starts_with('%') && pane[1..].chars().all(|ch| ch.is_ascii_digit())
}

/// ---
/// purpose: 枚举本机所有 tmux socket，跨 server 判定该 pane 的状态
/// returns: 汇总后的状态
/// ---
pub(super) fn active_leader_pane_state_across_tmux_sockets(
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    let endpoints = crate::tmux_backend::tmux_socket_endpoints();
    let transports = endpoints
        .iter()
        .map(|endpoint| crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint))
        .collect::<Vec<_>>();
    active_leader_pane_state_across_transports(
        transports
            .iter()
            .map(|transport| transport as &dyn Transport),
        pane,
    )
}

/// ---
/// purpose: 在多个 transport 上汇总某 pane 的状态
/// returns: 任一为 Live 即 Live；否则有 Dead 报 Dead，有 Absent 报 Absent，全 Unknown 报 Unknown
/// ---
pub(crate) fn active_leader_pane_state_across_transports<'a>(
    transports: impl IntoIterator<Item = &'a dyn Transport>,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    let mut found_absent = false;
    let mut found_dead = false;
    for transport in transports {
        match active_leader_pane_state(transport, pane) {
            LeaderPaneEnvState::Live => return LeaderPaneEnvState::Live,
            LeaderPaneEnvState::Dead => found_dead = true,
            LeaderPaneEnvState::Absent => found_absent = true,
            LeaderPaneEnvState::Unknown => {}
        }
    }
    if found_dead {
        LeaderPaneEnvState::Dead
    } else if found_absent {
        LeaderPaneEnvState::Absent
    } else {
        LeaderPaneEnvState::Unknown
    }
}

/// ---
/// purpose: 在单个 transport 上判定 pane 状态
/// returns: has_pane 给出确定答案时直接用，否则退到 liveness；两者都判不出时为 Unknown
/// ---
pub(super) fn active_leader_pane_state(
    transport: &dyn Transport,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    match transport.has_pane(pane) {
        Ok(Some(true)) => return LeaderPaneEnvState::Live,
        Ok(Some(false)) => return LeaderPaneEnvState::Absent,
        Ok(None) | Err(_) => {}
    }
    match transport.liveness(pane) {
        Ok(crate::transport::PaneLiveness::Live) => LeaderPaneEnvState::Live,
        Ok(crate::transport::PaneLiveness::Dead) => LeaderPaneEnvState::Dead,
        Ok(crate::transport::PaneLiveness::Unknown) | Err(_) => LeaderPaneEnvState::Unknown,
    }
}

/// ---
/// purpose: 在没有 caller pane 的情况下种入一份 unbound owner 与 receiver
/// params:
///   launched: 待改写的 state
///   launched_key: 团队键
/// returns: 归因不出 provider 时直接返回不写，避免默认成某个 provider
/// ---
pub(super) fn seed_unbound_launched_owner(launched: &mut serde_json::Value, launched_key: &str) {
    let Some(owner) = unbound_launched_owner(launched, launched_key) else {
        return;
    };
    let Some(provider) = owner
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.is_empty())
    else {
        return;
    };
    let owner_epoch = 1u64;
    let receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "unbound",
        "provider": provider,
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    // Stage 3a (identity-boundary unified plan, architect direction 2026-06-23):
    // route quick-start unbound seed through ownership repository.
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(owner_epoch);
    crate::state::ownership::write_owner(launched, launched_key, record);
}

/// ---
/// purpose: 构造 unbound owner 记录
/// returns: provider 归因成功且能派生出 leader session uuid 时给出 owner JSON，否则 None
/// ---
pub(super) fn unbound_launched_owner(
    launched: &serde_json::Value,
    launched_key: &str,
) -> Option<serde_json::Value> {
    let provider = unbound_launched_provider(launched)?;
    let machine_fingerprint = launched
        .get("team_owner")
        .and_then(|owner| owner.get("machine_fingerprint"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let workspace = launched
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let uuid = crate::model::ids::LeaderSessionUuid::derive(
        machine_fingerprint,
        workspace,
        &os_user,
        launched_key,
    )
    .ok()?;
    Some(serde_json::json!({
        "provider": provider,
        "machine_fingerprint": machine_fingerprint,
        "leader_session_uuid": uuid.as_str(),
        "owner_epoch": 1u64,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": os_user,
    }))
}

/// ---
/// purpose: 归因 unbound owner 的 provider
/// returns: 先用 state 里已写的 provider，其次按 owner 的 pane 跨 socket 归因；都失败为 None
/// ---
pub(super) fn unbound_launched_provider(launched: &serde_json::Value) -> Option<String> {
    if let Some(provider) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("provider"))
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.is_empty())
        .and_then(parse_provider)
        .and_then(provider_wire_string)
    {
        return Some(provider);
    }
    let pane = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())?;
    let target = PaneId::new(pane);
    attributed_provider_for_pane_across_tmux_sockets(&target).and_then(provider_wire_string)
}

/// ---
/// purpose: 把 provider 枚举转成它的 wire 字符串
/// returns: 序列化得到的字符串，转换失败为 None
/// ---
pub(super) fn provider_wire_string(provider: Provider) -> Option<String> {
    serde_json::to_value(provider)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// ---
/// purpose: 跨本机所有 tmux socket 找到该 pane 并归因它的 provider
/// returns: 找到且能归因时给出 provider，否则 None
/// ---
pub(super) fn attributed_provider_for_pane_across_tmux_sockets(pane: &PaneId) -> Option<Provider> {
    crate::tmux_backend::tmux_socket_endpoints()
        .into_iter()
        .filter_map(|endpoint| {
            crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&endpoint)
                .list_targets()
                .ok()
        })
        .flatten()
        .find(|info| info.pane_id == *pane)
        .and_then(|info| crate::leader::attribute_pane_provider(&info))
}

/// ---
/// purpose: 定出 caller 的 provider wire 名
/// params:
///   lookup_pane_provider: 由 pane 反查 provider 的函数，便于测试替换
/// returns: caller 自报的 provider 优先，其次按 caller pane 反查；都不成为 None
/// ---
pub(super) fn caller_provider_for_seed_with_lookup(
    caller: &crate::state::owner_gate::CallerIdentity,
    lookup_pane_provider: impl Fn(&PaneId) -> Option<Provider>,
) -> Option<String> {
    if !caller.provider.is_empty() {
        if let Some(provider) = parse_provider(&caller.provider).and_then(provider_wire_string) {
            return Some(provider);
        }
    }
    (!caller.pane_id.is_empty())
        .then(|| PaneId::new(&caller.pane_id))
        .and_then(|pane| lookup_pane_provider(&pane))
        .and_then(provider_wire_string)
}

#[cfg(test)]
mod e22_unbound_owner_provider_tests {
    use super::*;
    use crate::state::owner_gate::CallerIdentity;

    #[test]
    fn unbound_owner_preserves_explicit_copilot_provider() {
        let mut launched = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "team_owner": {
                "provider": "copilot",
                "machine_fingerprint": "machine"
            }
        });

        seed_unbound_launched_owner(&mut launched, "team-e22");

        // Stage 3d: canonical owner/receiver at teams.team-e22.
        assert_eq!(
            launched
                .pointer("/teams/team-e22/team_owner/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
        assert_eq!(
            launched
                .pointer("/teams/team-e22/leader_receiver/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
    }

    #[test]
    fn unbound_owner_without_attributed_provider_does_not_default_codex() {
        let mut launched = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "team_owner": {
                "machine_fingerprint": "machine"
            }
        });

        seed_unbound_launched_owner(&mut launched, "team-e22");

        // Stage 3d: no receiver should be seeded at canonical location.
        assert!(
            launched
                .pointer("/teams/team-e22/leader_receiver")
                .is_none(),
            "unattributed unbound owner must not seed a codex receiver: {launched}"
        );
        assert!(
            launched
                .pointer("/teams/team-e22/team_owner/provider")
                .and_then(serde_json::Value::as_str)
                != Some("codex"),
            "unattributed unbound owner must not silently become codex: {launched}"
        );
    }

    fn caller(provider: &str, pane_id: &str) -> CallerIdentity {
        CallerIdentity {
            pane_id: pane_id.to_string(),
            provider: provider.to_string(),
            machine_fingerprint: "machine".to_string(),
            leader_session_uuid: "leader-uuid".to_string(),
            leader_session_uuid_source: "derived".to_string(),
        }
    }

    #[test]
    fn env_seed_attributes_in_tmux_node_form_copilot_from_caller_pane() {
        let mut state = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "leader": {"provider": "copilot"},
        });

        assert!(seed_launched_owner_from_caller_with_provider_lookup(
            &mut state,
            caller("", "%0"),
            |pane| (pane.as_str() == "%0").then_some(Provider::Copilot),
        ));

        // Stage 3d: canonical owner/receiver at teams.<team_state_key>.
        // State has no team_key/team_dir/spec_path → fallback "current".
        let team_key = crate::state::projection::team_state_key(&state);
        assert_eq!(
            state["teams"][&team_key]["team_owner"]["provider"].as_str(),
            Some("copilot")
        );
        assert_eq!(
            state["teams"][&team_key]["leader_receiver"]["provider"].as_str(),
            Some("copilot")
        );
    }

    #[test]
    fn env_seed_unknown_caller_pane_does_not_default_codex() {
        let mut state = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "leader": {"provider": "copilot"},
        });

        assert!(seed_launched_owner_from_caller_with_provider_lookup(
            &mut state,
            caller("", "%0"),
            |_| None,
        ));
        let team_key = crate::state::projection::team_state_key(&state);
        assert_eq!(
            state["teams"][&team_key]["team_owner"]["pane_id"].as_str(),
            Some("%0")
        );
        assert_eq!(
            state["teams"][&team_key]["leader_receiver"]["pane_id"].as_str(),
            Some("%0")
        );
        assert!(
            state["teams"][&team_key]["leader_receiver"]["provider"].as_str() != Some("codex"),
            "unknown caller pane must not silently seed a codex receiver: {state}"
        );
        assert!(
            state["teams"][&team_key]["team_owner"]["provider"].as_str() != Some("codex"),
            "unknown caller pane must not silently become codex: {state}"
        );
    }
}

/// ---
/// purpose: 判断该 pane 是否已经是别的团队的 owner pane
/// params:
///   launched_key: 本次团队键，比较时被排除
/// returns: 存在另一团队的 team_owner.pane_id 等于它则为 true
/// ---
pub(super) fn owner_pane_belongs_to_other_team(
    existing: &serde_json::Value,
    launched_key: &str,
    pane: &str,
) -> bool {
    existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| {
            teams.iter().any(|(key, team)| {
                key != launched_key
                    && team
                        .get("team_owner")
                        .and_then(|owner| owner.get("pane_id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(pane)
            })
        })
}
