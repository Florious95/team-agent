//! ---
//! purpose: 起队后的就绪判定，含 worker 状态汇总与 leader 收件可达性
//! contract:
//!   provides:
//!     - name: quick_start_worker_readiness
//!       what: 汇总各席位状态给出就绪结论
//!     - name: quick_start_session_capture_incomplete_agents
//!       what: 列出会话捕获尚未收敛的席位
//!     - name: launched_team_receiver_is_attached
//!       what: 以宿主注册表为准判断 leader 收件是否已挂上
//!   depends:
//!     - crate::state::persist
//!     - crate::session_capture
//!     - crate::leader::registry
//! boundary:
//!   - 可达性以宿主注册表为权威，workspace state 只是副本
//!   - 判不出来一律当作未挂上，不乐观放行
//! maturity: wired
//! ---
//! unit-8 (Stage 3) — `lifecycle::launch::readiness` phase boundary.
//!
//! Dedicated home for coordinator-start + readiness-verdict computation.
//! Future commits migrate the inline phase fns at launch.rs:2928-2944
//! here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// purpose: 由 runtime state 汇总该团队的就绪结论
/// params:
///   team_key: 团队键，teams 表里没有时退到顶层 state
/// returns: 有非 running 席位时返回 Degraded 并列出它们，否则返回 PendingToolLoad
/// ---
pub(super) fn quick_start_worker_readiness(
    workspace: &Path,
    team_key: &str,
) -> QuickStartReadiness {
    let Ok(state) = load_runtime_state(workspace) else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    let Some(agents) = team_state
        .get("agents")
        .and_then(serde_json::Value::as_object)
    else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let all_spawned = !agents.is_empty();
    let leader_receiver_attached = launched_team_receiver_is_attached(workspace, team_key);
    let all_attached_receiver = leader_receiver_attached;
    let mut unhealthy: Vec<String> = agents
        .iter()
        .filter_map(|(id, agent)| {
            let status = agent.get("status").and_then(serde_json::Value::as_str);
            match status {
                Some("running") => None,
                _ => Some(id.clone()),
            }
        })
        .collect();
    if !unhealthy.is_empty() {
        unhealthy.sort();
        unhealthy.dedup();
        QuickStartReadiness::Degraded {
            unhealthy_agents: unhealthy,
        }
    } else {
        let incomplete_agents =
            crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state);
        let all_resumable_have_session = incomplete_agents.is_empty();
        let _readiness_ready = all_spawned && all_attached_receiver && all_resumable_have_session;
        QuickStartReadiness::PendingToolLoad
    }
}

/// ---
/// purpose: 列出会话捕获尚未收敛的席位
/// returns: 席位 id 列表；读不出 state 时为空
/// ---
pub(super) fn quick_start_session_capture_incomplete_agents(
    workspace: &Path,
    team_key: &str,
) -> Vec<String> {
    let Ok(state) = load_runtime_state(workspace) else {
        return Vec::new();
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state)
}

/// ---
/// purpose: 判断该团队的 leader 收件端是否真的挂上了
/// returns: 注册表明确记为 attached 且 state 可读时为 true；未绑定或判不出一律 false
/// ---
/// Host registry is the deliverability authority. Workspace `state.json`
/// is only a copy. Detection failure is unbound, never attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderBindingClass {
    Attached,
    IndexMissing,
    Unknown,
    Unbound,
}

impl LeaderBindingClass {
    pub fn issue_id(self) -> &'static str {
        match self {
            Self::Attached => "leader_receiver_attached",
            Self::IndexMissing => "leader_registry_index_missing",
            Self::Unknown => "leader_binding_unknown",
            Self::Unbound => "leader_receiver_unbound",
        }
    }

    pub fn repair(self) -> &'static str {
        match self {
            Self::Attached => "",
            Self::IndexMissing => {
                "publish the leader registry index for the selected team; do not claim-leader"
            }
            Self::Unknown => {
                "inspect the selected-team registry file and live leader channel; do not claim-leader"
            }
            Self::Unbound => "team-agent claim-leader --confirm --json",
        }
    }
}

pub fn selected_team_leader_receiver<'a>(
    state: &'a serde_json::Value,
    team_key: &str,
) -> Option<&'a serde_json::Value> {
    if let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) {
        if let Some(team) = teams.get(team_key) {
            return team.get("leader_receiver");
        }
    }
    state.get("leader_receiver")
}

fn owner_record<'a>(state: &'a serde_json::Value, team_key: &str) -> Option<&'a serde_json::Value> {
    let owner = if let Some(team) = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
    {
        team.get("team_owner")
    } else {
        state.get("team_owner")
    }?;
    owner.as_object().map(|_| owner)
}

fn canonical_identity_aligned(
    workspace: &Path,
    team_key: &str,
    state: &serde_json::Value,
    receiver: &serde_json::Value,
    entry: &crate::leader::registry::LeaderRegistryEntry,
) -> bool {
    let Some(owner) = owner_record(state, team_key) else {
        return false;
    };
    let owner_pane = owner.get("pane_id").and_then(serde_json::Value::as_str);
    let receiver_pane = receiver.get("pane_id").and_then(serde_json::Value::as_str);
    let owner_epoch = owner.get("owner_epoch").and_then(serde_json::Value::as_u64);
    let receiver_epoch = receiver
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64);
    let channel_pane = entry
        .channel
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| entry.channel.get("pane").and_then(serde_json::Value::as_str));
    entry.team_key == team_key
        && entry.status == "attached"
        && receiver.get("status").and_then(serde_json::Value::as_str) == Some("attached")
        && owner_pane == receiver_pane
        && owner_pane.is_some()
        && owner_epoch == receiver_epoch
        && owner_epoch == Some(entry.owner_epoch)
        && channel_pane.is_none_or(|pane| Some(pane) == owner_pane)
        && crate::leader::registry::workspace_hash(workspace) == entry.workspace_hash
}

pub fn classify_leader_binding(workspace: &Path, team_key: &str) -> LeaderBindingClass {
    let state = match load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => return LeaderBindingClass::Unknown,
    };
    let receiver = selected_team_leader_receiver(&state, team_key);
    let transport: Box<dyn crate::transport::Transport> = match receiver
        .and_then(|value| value.get("tmux_socket"))
        .and_then(serde_json::Value::as_str)
        .filter(|endpoint| !endpoint.is_empty() && *endpoint != "default")
    {
        Some(endpoint) => Box::new(crate::transport_factory::tmux_endpoint_transport(endpoint)),
        None => return classify_leader_binding_at(workspace, team_key, &state, None),
    };
    classify_leader_binding_at(workspace, team_key, &state, Some(transport.as_ref()))
}

pub(crate) fn classify_leader_binding_at(
    workspace: &Path,
    team_key: &str,
    state: &serde_json::Value,
    transport: Option<&dyn crate::transport::Transport>,
) -> LeaderBindingClass {
    let registry = registry_deliverability(workspace, team_key);
    let receiver = selected_team_leader_receiver(state, team_key);
    let receiver_attached = receiver
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("attached");
    let has_owner = owner_record(state, team_key).is_some();
    match registry {
        RegistryDeliverability::Undecidable => LeaderBindingClass::Unknown,
        RegistryDeliverability::Attached => {
            let Some(receiver) = receiver else {
                return LeaderBindingClass::Unknown;
            };
            let dir = crate::leader::registry::registry_dir();
            let entry = dir.and_then(|dir| {
                let path = dir.join(format!(
                    "{}__{team_key}.json",
                    crate::leader::registry::workspace_hash(workspace)
                ));
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|text| {
                        serde_json::from_str::<crate::leader::registry::LeaderRegistryEntry>(&text)
                            .ok()
                    })
            });
            let Some(entry) = entry else {
                return LeaderBindingClass::Unknown;
            };
            if !canonical_identity_aligned(workspace, team_key, state, receiver, &entry) {
                return LeaderBindingClass::Unknown;
            }
            let Some(transport) = transport else {
                return LeaderBindingClass::Unknown;
            };
            match crate::messaging::leader_channel::resolve_live_leader_channel(
                workspace, receiver, transport,
            ) {
                crate::messaging::leader_channel::LeaderChannelResolution::Live(_) => {
                    LeaderBindingClass::Attached
                }
                _ => LeaderBindingClass::Unknown,
            }
        }
        RegistryDeliverability::Unbound if has_owner || receiver_attached => {
            LeaderBindingClass::IndexMissing
        }
        RegistryDeliverability::Unbound => LeaderBindingClass::Unbound,
    }
}

pub fn launched_team_receiver_is_attached(workspace: &Path, team_key: &str) -> bool {
    classify_leader_binding(workspace, team_key) == LeaderBindingClass::Attached
}

#[cfg(test)]
mod claim_rework_class_tests {
    use super::*;
    use crate::transport::test_support::OfflineTransport;
    use crate::transport::{PaneId, PaneInfo, SessionName};
    use serde_json::json;

    fn isolated() -> (std::path::PathBuf, std::path::PathBuf, Option<std::ffi::OsString>) {
        let root = std::env::temp_dir().join(format!(
            "ta-class-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let workspace = root.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        (root, workspace, previous)
    }

    fn restore_home(previous: Option<std::ffi::OsString>, root: &Path) {
        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[serial_test::serial(env)]
    fn classify_bad_registry_json_is_unknown() {
        let (root, workspace, previous) = isolated();
        crate::state::persist::save_runtime_state(
            &workspace,
            &json!({"teams": {"alpha": {"team_owner": {"pane_id": "%1", "owner_epoch": 1}}}}),
        )
        .unwrap();
        let dir = crate::leader::registry::registry_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "{}__alpha.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        std::fs::write(&path, "{not json").unwrap();
        let class = classify_leader_binding(&workspace, "alpha");
        restore_home(previous, &root);
        assert_eq!(class, LeaderBindingClass::Unknown);
    }

    #[test]
    #[serial_test::serial(env)]
    fn classify_null_owner_is_not_index_missing() {
        let (root, workspace, previous) = isolated();
        crate::state::persist::save_runtime_state(
            &workspace,
            &json!({"teams": {"alpha": {"team_owner": null}}}),
        )
        .unwrap();
        let class = classify_leader_binding(&workspace, "alpha");
        restore_home(previous, &root);
        assert_eq!(class, LeaderBindingClass::Unbound);
    }

    #[test]
    #[serial_test::serial(env)]
    fn classify_attached_requires_identity_and_live() {
        let (root, workspace, previous) = isolated();
        let endpoint = "/tmp/ta-class-live.sock";
        let state = json!({
            "teams": {
                "alpha": {
                    "team_owner": {"pane_id": "%1", "owner_epoch": 3, "provider": "codex"},
                    "leader_receiver": {
                        "status": "attached",
                        "pane_id": "%1",
                        "owner_epoch": 3,
                        "tmux_socket": endpoint
                    }
                }
            }
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let entry = crate::leader::registry::build_entry(
            &workspace,
            "alpha",
            "direct_tmux",
            json!({"pane_id": "%1"}),
            3,
            "test",
            "2026-01-01T00:00:00Z".to_string(),
        );
        let dir = crate::leader::registry::registry_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "{}__alpha.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        let transport = OfflineTransport::default()
            .with_tmux_endpoint(endpoint)
            .with_targets(vec![PaneInfo {
                pane_id: PaneId::new("%1"),
                session: SessionName::new("s"),
                window_index: None,
                window_name: None,
                pane_index: None,
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: Some(workspace.clone()),
                active: true,
                pane_pid: None,
                leader_env: Default::default(),
            }]);
        let class = classify_leader_binding_at(
            &workspace,
            "alpha",
            &state,
            Some(&transport),
        );
        restore_home(previous, &root);
        assert_eq!(class, LeaderBindingClass::Attached);
    }
}

enum RegistryDeliverability {
    Attached,
    Unbound,
    Undecidable,
}

fn registry_deliverability(workspace: &Path, team_key: &str) -> RegistryDeliverability {
    let Some(dir) = crate::leader::registry::registry_dir() else {
        return RegistryDeliverability::Undecidable;
    };
    if team_key.is_empty() {
        return RegistryDeliverability::Unbound;
    }
    let hash = crate::leader::registry::workspace_hash(workspace);
    let path = dir.join(format!("{hash}__{team_key}.json"));
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            RegistryDeliverability::Unbound
        }
        Err(_) => RegistryDeliverability::Undecidable,
        Ok(text) => {
            match serde_json::from_str::<crate::leader::registry::LeaderRegistryEntry>(&text) {
                Err(_) => RegistryDeliverability::Undecidable,
                Ok(entry) => {
                    if entry.status != "attached" {
                        return RegistryDeliverability::Unbound;
                    }
                    if let Some(authorized) = entry
                        .channel
                        .get("authorized_team_workspace")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        if !same_workspace_path(Path::new(authorized), workspace) {
                            return RegistryDeliverability::Unbound;
                        }
                    }
                    RegistryDeliverability::Attached
                }
            }
        }
    }
}

fn same_workspace_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}
