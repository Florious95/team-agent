//! ---
//! purpose: 生命周期操作前的 owner 门，判断调用方是否有权动这个团队
//! contract:
//!   provides:
//!     - name: ensure_owner_allowed
//!       what: 读 runtime state 后过 owner 门
//!     - name: ensure_owner_allowed_for_state
//!       what: 对给定 state 过 owner 门，席位对自己的操作直接放行
//!     - name: state_path
//!       what: 给出该 workspace 的 runtime state 路径
//!   depends:
//!     - crate::state::owner_gate
//!     - crate::state::identity
//!     - crate::state::projection
//!     - crate::state::persist
//! boundary:
//!   - 只判权限，不改 state
//!   - 本文件的 pane 存活探针恒报存活，存活判定不在这一层
//! maturity: wired
//! ---
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
/// purpose: 读出 runtime state 后过 owner 门
/// returns: 通过返回空值
/// errors: 读 state 失败返回 StatePersist，owner 门拒绝返回 OwnerRefused
/// ---
pub(crate) fn ensure_owner_allowed(workspace: &Path) -> Result<(), LifecycleError> {
    let state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    ensure_owner_allowed_for_state(&state, None)
}

/// ---
/// purpose: 对给定 state 过 owner 门
/// params:
///   target_role: 操作目标席位；调用方就是该席位且团队相符时直接放行
/// returns: 通过返回空值
/// errors: 取调用方身份失败返回 StatePersist，owner 门给出拒绝理由时返回 OwnerRefused
/// ---
pub(crate) fn ensure_owner_allowed_for_state(
    state: &serde_json::Value,
    target_role: Option<&AgentId>,
) -> Result<(), LifecycleError> {
    struct NoopLiveness;
    impl crate::state::owner_gate::PaneLivenessProbe for NoopLiveness {
        fn liveness(&self, _pane_id: &str) -> crate::model::enums::PaneLiveness {
            crate::model::enums::PaneLiveness::Live
        }
    }

    let target_team = crate::state::projection::team_state_key(state);
    if caller_is_target_role_in_team(&target_team, target_role) {
        return Ok(());
    }
    let caller = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&target_team),
        None,
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) =
        crate::state::owner_gate::check_team_owner(state, &caller, false, &NoopLiveness)
    {
        return Err(LifecycleError::OwnerRefused(refusal.to_string()));
    }
    Ok(())
}

/// ---
/// purpose: 判断调用方是否就是目标席位本人
/// returns: 环境里的席位 id 与团队 id 同时与目标相符时为 true
/// ---
pub(super) fn caller_is_target_role_in_team(
    target_team: &str,
    target_role: Option<&AgentId>,
) -> bool {
    let Some(target_role) = target_role else {
        return false;
    };
    std::env::var("TEAM_AGENT_ID").ok().as_deref() == Some(target_role.as_str())
        && std::env::var("TEAM_AGENT_TEAM_ID").ok().as_deref() == Some(target_team)
}

/// ---
/// purpose: 给出该 workspace 的 runtime state 文件路径
/// returns: runtime state 路径
/// ---
pub(crate) fn state_path(workspace: &Path) -> std::path::PathBuf {
    crate::state::persist::runtime_state_path(workspace)
}
