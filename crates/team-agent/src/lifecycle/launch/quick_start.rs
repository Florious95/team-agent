//! ---
//! purpose: 零配置一键起队入口，编译角色目录、起全队、判嵌套层级并给出 attach 指引
//! contract:
//!   provides:
//!     - name: quick_start
//!       what: 由角色目录一键起队
//!     - name: quick_start_in_workspace_with_display_and_backend
//!       what: 带显示开关与后端选择的起队入口
//!     - name: quick_start_with_transport_in_workspace_with_display
//!       what: 起队的实体实现，含 leader pane 校验、层级门与已有 runtime 的早退
//!   depends:
//!     - crate::compiler
//!     - crate::state::persist
//!     - crate::transport_factory
//!     - crate::lifecycle::launch::identity
//!     - crate::lifecycle::launch::quick_start_transport
//! boundary:
//!   - 只管初次起队，已有 runtime 时给出 restart 指引而不接管
//!   - 显式指定非 tmux 后端时不静默退回 tmux，不可用就如实报错
//!   - 团队嵌套超过两层直接拒绝
//! maturity: wired
//! ---
#[cfg(test)]
#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneField, PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

trait FreshQuickStartLeaderBindingOps {
    fn caller_pane(&mut self) -> Option<String>;
    fn caller_resolution(&mut self) -> crate::layout::worker_env::CallerProviderResolution {
        crate::layout::worker_env::CallerProviderResolution::Absent
    }
    fn explicit_provider(&mut self) -> Option<String>;
    fn tmux_endpoint(&mut self) -> Option<String>;
    fn observe_command(&mut self, pane: &PaneId) -> Option<String>;
    fn attach(
        &mut self,
        workspace: &Path,
        state: &mut serde_json::Value,
        pane: &PaneId,
        provider: crate::provider::Provider,
    ) -> bool;
    fn attach_failure_reason(&self) -> Option<&'static str> {
        None
    }
    fn attach_cas_conflict(&self) -> bool {
        false
    }
    fn applied_grant(&self) -> Option<(serde_json::Value, serde_json::Value)> {
        None
    }
    fn registry_receipt(&self) -> Option<crate::leader::registry::RegistryWriteReceipt> {
        None
    }
    fn register(&mut self, workspace: &Path, team_key: &str) -> bool;
    fn register_failure_reason(&self) -> Option<&'static str> {
        None
    }
    fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool;
    fn readback_failure_reason(&self) -> Option<&'static str> {
        None
    }
}

struct RuntimeFreshQuickStartLeaderBindingOps<'a> {
    transport: &'a dyn Transport,
    last_failure_reason: Option<&'static str>,
    cas_conflict: bool,
    applied_grant: Option<(serde_json::Value, serde_json::Value)>,
    registry_receipt: Option<crate::leader::registry::RegistryWriteReceipt>,
    nonce_writer:
        Option<&'a dyn Fn(&str, &PaneId, &str) -> Result<String, crate::leader::LeaderError>>,
}

impl FreshQuickStartLeaderBindingOps for RuntimeFreshQuickStartLeaderBindingOps<'_> {
    fn caller_pane(&mut self) -> Option<String> {
        std::env::var("TMUX_PANE")
            .ok()
            .filter(|pane| !pane.is_empty())
    }

    fn caller_resolution(&mut self) -> crate::layout::worker_env::CallerProviderResolution {
        let current_pane = std::env::var("TMUX_PANE")
            .ok()
            .filter(|pane| !pane.is_empty());
        let current_endpoint = crate::tmux_backend::socket_name_from_tmux_env();
        crate::layout::worker_env::caller_provider_resolution(
            current_pane.as_deref(),
            current_endpoint.as_deref(),
            self.transport.tmux_endpoint().as_deref(),
        )
    }

    fn explicit_provider(&mut self) -> Option<String> {
        std::env::var("TEAM_AGENT_LEADER_PROVIDER")
            .ok()
            .filter(|provider| !provider.is_empty())
    }

    fn tmux_endpoint(&mut self) -> Option<String> {
        self.transport.tmux_endpoint()
    }

    fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
        self.transport
            .query(&Target::Pane(pane.clone()), PaneField::PaneCurrentCommand)
            .ok()
            .flatten()
            .filter(|command| !command.trim().is_empty())
    }

    fn attach(
        &mut self,
        workspace: &Path,
        state: &mut serde_json::Value,
        pane: &PaneId,
        provider: crate::provider::Provider,
    ) -> bool {
        self.cas_conflict = false;
        self.applied_grant = None;
        self.registry_receipt = None;
        let event_log = crate::event_log::EventLog::new(workspace);
        let targets = match self.transport.list_targets() {
            Ok(targets) => targets,
            Err(_) => {
                self.last_failure_reason = Some("caller_pane_unobservable");
                return false;
            }
        };
        let Some(caller_target) = targets
            .into_iter()
            .find(|target| target.pane_id.as_str() == pane.as_str())
        else {
            self.last_failure_reason = Some("caller_pane_not_live");
            return false;
        };
        if !caller_target.active
            || caller_target
                .current_path
                .as_ref()
                .is_none_or(|path| path.as_os_str().is_empty())
        {
            self.last_failure_reason = Some("caller_cwd_unobservable");
            return false;
        }
        let expected_owner = state.get("team_owner").cloned();
        let expected_receiver = state.get("leader_receiver").cloned();
        match crate::leader::attach_leader_to_state_with_target_and_controls(
            workspace,
            state,
            Some(pane),
            provider,
            &event_log,
            crate::leader::LeaseSource::QuickStart,
            true,
            Some(&caller_target),
            expected_owner.as_ref(),
            expected_receiver.as_ref(),
            self.nonce_writer,
        ) {
            Ok((receiver, _)) => {
                let owner = state.get("team_owner").cloned();
                let receiver = serde_json::to_value(receiver).ok();
                self.applied_grant = owner.zip(receiver);
                self.last_failure_reason = None;
                true
            }
            Err(error) => {
                let (error, applied_grant) = error.into_parts();
                self.applied_grant = applied_grant.map(|grant| (grant.owner, grant.receiver));
                self.cas_conflict = match &error {
                    crate::leader::LeaderError::State(crate::state::StateError::SaveConflict(_)) => true,
                    _ => false,
                };
                self.last_failure_reason = Some(if self.cas_conflict {
                    "attach_cas_conflict"
                } else {
                    match &error {
                        crate::leader::LeaderError::Validation(_) => "attach_validation_failed",
                        crate::leader::LeaderError::Tmux(_) => "attach_transport_failed",
                        crate::leader::LeaderError::State(_) => "attach_state_failed",
                        crate::leader::LeaderError::Identity(_) => "attach_identity_failed",
                        crate::leader::LeaderError::Io(_)
                        | crate::leader::LeaderError::Json(_)
                        | crate::leader::LeaderError::MessageStore(_)
                        | crate::leader::LeaderError::Start(_) => "attach_failed",
                        crate::leader::LeaderError::EventLog(_) => "attach_event_log_failed",
                        crate::leader::LeaderError::Messaging(_) => "attach_watcher_failed",
                    }
                });
                false
            }
        }
    }

    fn attach_failure_reason(&self) -> Option<&'static str> {
        self.last_failure_reason
    }

    fn attach_cas_conflict(&self) -> bool {
        self.cas_conflict
    }

    fn applied_grant(&self) -> Option<(serde_json::Value, serde_json::Value)> {
        self.applied_grant.clone()
    }

    fn registry_receipt(&self) -> Option<crate::leader::registry::RegistryWriteReceipt> {
        self.registry_receipt.clone()
    }

    fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
        match crate::leader::registry::register_binding_from_state_best_effort(
            workspace,
            Some(team_key),
            "quick-start",
        ) {
            Some(outcome) if outcome.status == "registered" && outcome.path.is_some() => {
                self.registry_receipt = outcome.receipt;
                self.last_failure_reason = None;
                true
            }
            Some(outcome) => {
                self.last_failure_reason = Some(if outcome.status == "write_failed" {
                    "registry_write_failed"
                } else {
                    "registry_registration_failed"
                });
                false
            }
            None => {
                self.last_failure_reason = Some("registry_state_unavailable");
                false
            }
        }
    }

    fn register_failure_reason(&self) -> Option<&'static str> {
        self.last_failure_reason
    }

    fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
        if !launched_team_receiver_is_attached(workspace, team_key) {
            self.last_failure_reason = Some("registry_readback_unavailable");
            return false;
        }
        let Ok(resolved) = crate::state::projection::resolve_runtime_team_scope(
            workspace,
            Some(team_key),
        ) else {
            self.last_failure_reason = Some("receiver_scope_unavailable");
            return false;
        };
        let Some(receiver) =
            super::selected_team_leader_receiver(&resolved.state, team_key)
        else {
            self.last_failure_reason = Some("receiver_missing");
            return false;
        };
        if crate::lifecycle::launch::classify_leader_binding(workspace, team_key)
            != super::LeaderBindingClass::Attached
        {
            self.last_failure_reason = Some("registry_identity_mismatch");
            return false;
        }
        if !matches!(
            crate::messaging::resolve_live_leader_channel(
                workspace,
                receiver,
                self.transport,
            ),
            crate::messaging::LeaderChannelResolution::Live(_)
        ) {
            self.last_failure_reason = Some("receiver_live_channel_unavailable");
            return false;
        }
        self.last_failure_reason = None;
        true
    }

    fn readback_failure_reason(&self) -> Option<&'static str> {
        self.last_failure_reason
    }
}

const FRESH_BIND_REFUSAL_EVENT: &str = "quick_start.leader_bind_refused";

// The event log is already scoped to `workspace`; `team_key` keeps a
// multi-team log unambiguous without copying paths or caller details.
fn record_fresh_bind_refusal(
    workspace: &Path,
    team_key: &str,
    stage: &'static str,
    reason: &'static str,
) {
    LAST_FRESH_BIND_REFUSAL.with(|cell| {
        *cell.borrow_mut() = Some((stage, reason));
    });
    let _ = crate::event_log::EventLog::new(workspace).write(
        FRESH_BIND_REFUSAL_EVENT,
        serde_json::json!({
            "operation": "fresh_quick_start_leader_bind",
            "team_key": team_key,
            "stage": stage,
            "reason": reason,
        }),
    );
}

thread_local! {
    static LAST_FRESH_BIND_REFUSAL: std::cell::RefCell<Option<(&'static str, &'static str)>> =
        const { std::cell::RefCell::new(None) };
}

fn take_fresh_bind_refusal() -> Option<(&'static str, &'static str)> {
    LAST_FRESH_BIND_REFUSAL.with(|cell| cell.borrow_mut().take())
}

fn refuse_fresh_bind(
    workspace: &Path,
    team_key: &str,
    stage: &'static str,
    reason: &'static str,
) -> Result<bool, LifecycleError> {
    LAST_FRESH_BIND_REFUSAL.with(|cell| {
        *cell.borrow_mut() = Some((stage, reason));
    });
    record_fresh_bind_refusal(workspace, team_key, stage, reason);
    Ok(false)
}

struct BindingFileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl BindingFileSnapshot {
    fn capture(path: PathBuf) -> Self {
        let bytes = std::fs::read(&path).ok();
        Self { path, bytes }
    }

    fn restore(&self) {
        match &self.bytes {
            Some(bytes) => {
                let tmp = self.path.with_extension(format!(
                    "rollback-{}",
                    std::process::id()
                ));
                if std::fs::write(&tmp, bytes).is_ok()
                    && std::fs::rename(&tmp, &self.path).is_err()
                {
                    let _ = std::fs::remove_file(tmp);
                }
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

}

fn binding_snapshots(workspace: &Path, team_key: &str) -> Vec<BindingFileSnapshot> {
    let mut snapshots = vec![BindingFileSnapshot::capture(
        crate::state::persist::runtime_state_path(workspace),
    )];
    if let Some(dir) = crate::leader::registry::registry_dir() {
        snapshots.push(BindingFileSnapshot::capture(dir.join(format!(
            "{}__{team_key}.json",
            crate::leader::registry::workspace_hash(workspace)
        ))));
    }
    snapshots
}

fn same_workspace(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf())
        == std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf())
}

fn persisted_binding_matches_verified_pane(
    state: &serde_json::Value,
    workspace: &Path,
    pane: &PaneId,
    provider: crate::provider::Provider,
    endpoint: Option<&str>,
) -> bool {
    let Some(endpoint) = endpoint.filter(|value| !value.is_empty()) else {
        return false;
    };
    if state
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| !same_workspace(Path::new(value), workspace))
    {
        return false;
    }
    let Some(owner) = state.get("team_owner").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let Some(receiver) = state
        .get("leader_receiver")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let provider = serde_json::to_value(provider)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string));
    let Some(provider) = provider.as_deref() else {
        return false;
    };
    let owner_epoch = owner.get("owner_epoch").and_then(serde_json::Value::as_u64);
    let receiver_epoch = receiver
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64);
    let owner_uuid = owner
        .get("leader_session_uuid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    let receiver_uuid = receiver
        .get("leader_session_uuid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    receiver.get("status").and_then(serde_json::Value::as_str) == Some("attached")
        && receiver.get("mode").and_then(serde_json::Value::as_str) == Some("direct_tmux")
        && owner.get("pane_id").and_then(serde_json::Value::as_str) == Some(pane.as_str())
        && receiver.get("pane_id").and_then(serde_json::Value::as_str) == Some(pane.as_str())
        && owner.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
        && receiver.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
        && owner_epoch.is_some_and(|epoch| epoch > 0)
        && owner_epoch == receiver_epoch
        && owner_uuid.is_some()
        && owner_uuid == receiver_uuid
        && receiver
            .get("tmux_socket")
            .and_then(serde_json::Value::as_str)
            == Some(endpoint)
        && receiver
            .get("authorized_team_workspace")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .is_none_or(|value| same_workspace(Path::new(value), workspace))
}

fn bind_fresh_quick_start_leader_with<O: FreshQuickStartLeaderBindingOps>(
    workspace: &Path,
    team_key: &str,
    seeded_owner: Option<&serde_json::Value>,
    ops: &mut O,
) -> Result<bool, LifecycleError> {
    let Ok(resolved) = crate::state::projection::resolve_runtime_team_scope(
        workspace,
        Some(team_key),
    ) else {
        return refuse_fresh_bind(workspace, team_key, "scope", "scope_unresolved");
    };
    if resolved.canonical_team_key != team_key {
        return refuse_fresh_bind(workspace, team_key, "scope", "scope_key_mismatch");
    }
    let mut state = resolved.state;
    let Some(pane) = ops.caller_pane().filter(|pane| !pane.is_empty()) else {
        return refuse_fresh_bind(workspace, team_key, "caller_pane", "caller_pane_missing");
    };
    let pane = PaneId::new(pane);
    let Some(command) = ops
        .observe_command(&pane)
        .filter(|command| !command.trim().is_empty())
    else {
        return refuse_fresh_bind(
            workspace,
            team_key,
            "command_observation",
            "command_unobservable",
        );
    };
    let provider = match ops.caller_resolution() {
        crate::layout::worker_env::CallerProviderResolution::Valid(provider) => provider,
        crate::layout::worker_env::CallerProviderResolution::Invalid => {
            return refuse_fresh_bind(
                workspace,
                team_key,
                "strict_provider",
                "invalid_caller_tuple",
            );
        }
        crate::layout::worker_env::CallerProviderResolution::Absent => {
            match crate::leader::owner_bind::strict_owner_bind_provider(
                ops.explicit_provider().as_deref(),
                &command,
            ) {
                Some(provider) => provider,
                None => {
                    return refuse_fresh_bind(
                        workspace,
                        team_key,
                        "strict_provider",
                        "provider_unresolved",
                    );
                }
            }
        }
    };
    let endpoint = ops.tmux_endpoint();
    let has_persisted_binding = ["team_owner", "leader_receiver"]
        .iter()
        .any(|key| state.get(*key).is_some_and(|value| !value.is_null()));
    // `claimed_via` is historical state, not proof that this invocation seeded
    // the owner. Cleanup is allowed only when the caller supplies the exact
    // in-memory seed created immediately before this bind attempt and the
    // projected state still contains that seed.
    let current_receiver = state.get("leader_receiver");
    let pending_seed_receiver = current_receiver.is_some_and(|receiver| {
        receiver.get("status").and_then(serde_json::Value::as_str) == Some("pending")
            && receiver
                .get("discovery")
                .and_then(serde_json::Value::as_str)
                == Some("quick_start_seed")
    });
    let fresh_seeded_binding = seeded_owner.is_some_and(|seed| {
        state
            .get("team_owner")
            .is_some_and(|current| current == seed)
            && pending_seed_receiver
    });
    let seeded_receiver = if fresh_seeded_binding {
        current_receiver.cloned()
    } else {
        None
    };
    // Same-Team owner collision remains fail-closed. Cross-Team registry rows
    // are intentionally irrelevant: pane ids are scoped to their tmux server,
    // and independent endpoints may reuse the same numeric id.
    if has_persisted_binding
        && !fresh_seeded_binding
        && !persisted_binding_matches_verified_pane(
            &state,
            workspace,
            &pane,
            provider,
            endpoint.as_deref(),
        )
    {
        return refuse_fresh_bind(
            workspace,
            team_key,
            "persisted_binding",
            "persisted_binding_mismatch",
        );
    }
    // Registry publication is the commit point. Restore persisted surfaces on
    // failure, except for the caller-seeded owner that must be cleared.
    let snapshots = binding_snapshots(workspace, team_key);
    let attached = if fresh_seeded_binding {
        ops.attach(workspace, &mut state, &pane, provider)
    } else {
        has_persisted_binding || ops.attach(workspace, &mut state, &pane, provider)
    };
    let cleanup_grant = ops.applied_grant().or_else(|| {
        seeded_owner
            .filter(|seed| {
                fresh_seeded_binding
                    && state
                        .get("team_owner")
                        .is_some_and(|current| current == *seed)
            })
            .zip(seeded_receiver.clone())
            .map(|(owner, receiver)| (owner.clone(), receiver))
    });
    let committed = if !attached {
        record_fresh_bind_refusal(
            workspace,
            team_key,
            "attach",
            ops.attach_failure_reason().unwrap_or("attach_failed"),
        );
        false
    } else if !ops.register(workspace, team_key) {
        record_fresh_bind_refusal(
            workspace,
            team_key,
            "registry_register",
            ops.register_failure_reason()
                .unwrap_or("registry_register_failed"),
        );
        false
    } else if !ops.canonical_readback(workspace, team_key) {
        record_fresh_bind_refusal(
            workspace,
            team_key,
            "registry_readback",
            ops.readback_failure_reason()
                .unwrap_or("registry_readback_failed"),
        );
        false
    } else {
        true
    };
    if !committed {
        if ops.attach_cas_conflict() {
            return Ok(false);
        }
        if fresh_seeded_binding {
            // The registry receipt is the exact bytes this writer published;
            // conditional restore compares and mutates under the registry
            // owner's lock, so a concurrent winner is never rolled back.
            let registry_rollback_error = ops
                .registry_receipt()
                .and_then(|receipt| restore_registry_receipt(&snapshots, &receipt).err());
            if let Some((expected_owner, expected_receiver)) = cleanup_grant.as_ref() {
                clear_fresh_binding_on_refusal(
                    workspace,
                    &mut state,
                    team_key,
                    expected_owner,
                    Some((expected_owner, expected_receiver)),
                )?;
            } else if let Some(seed) = seeded_owner.filter(|seed| {
                state
                    .get("team_owner")
                    .is_some_and(|current| current == *seed)
            }) {
                clear_fresh_binding_on_refusal(workspace, &mut state, team_key, seed, None)?;
            }
            if let Some(error) = registry_rollback_error {
                return Err(error);
            }
        } else {
            // No caller-seeded owner was ours to clean up. Restore only the
            // runtime snapshot; a registry write is reverted only with its
            // exact receipt under the registry owner's lock.
            let registry_rollback_error = ops
                .registry_receipt()
                .and_then(|receipt| restore_registry_receipt(&snapshots, &receipt).err());
            if let Some(snapshot) = snapshots.first() {
                snapshot.restore();
            }
            if let Some(error) = registry_rollback_error {
                return Err(error);
            }
        }
    }
    Ok(committed)
}

fn restore_registry_receipt(
    snapshots: &[BindingFileSnapshot],
    receipt: &crate::leader::registry::RegistryWriteReceipt,
) -> Result<(), LifecycleError> {
    let previous = snapshots
        .iter()
        .find(|snapshot| snapshot.path == receipt.path)
        .and_then(|snapshot| snapshot.bytes.as_deref());
    match crate::leader::registry::restore_entry_if_current_matches(receipt, previous) {
        Ok(crate::leader::registry::RegistryRollback::Restored)
        | Ok(crate::leader::registry::RegistryRollback::Superseded) => Ok(()),
        Err(error) => Err(LifecycleError::StatePersist(format!(
            "quick-start registry rollback failed: {error}"
        ))),
    }
}

fn should_emit_workspace_socket_missing_hint(
    selected_source: Option<&str>,
    workspace_socket_missing: bool,
) -> bool {
    workspace_socket_missing && selected_source != Some("leader_env")
}

fn clear_fresh_binding_on_refusal(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    seed: &serde_json::Value,
    expected_grant: Option<(&serde_json::Value, &serde_json::Value)>,
) -> Result<(), LifecycleError> {
    // Tombstone only this attempt's exact grant. The lock-held persist merge
    // compares the on-disk owner and applied receiver; a concurrent owner or
    // receiver update is copied back and the epoch is not bumped.
    if let Some(obj) = state.as_object_mut() {
        obj.remove("leader_receiver");
        obj.remove("team_owner");
        obj.remove("owner_epoch");
    }
    if let Some(team) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_key))
        .and_then(serde_json::Value::as_object_mut)
    {
        team.insert("leader_receiver".to_string(), serde_json::Value::Null);
        team.insert("team_owner".to_string(), serde_json::Value::Null);
    }
    let intent = if let Some((expected_owner, expected_receiver)) = expected_grant {
        crate::state::repository::StateWriteIntent::ClearExactTeamOwnerAndReceiver {
            team_key,
            seed: expected_owner,
            expected_receiver,
        }
    } else {
        crate::state::repository::StateWriteIntent::ClearExactTeamOwner { team_key, seed }
    };
    crate::state::repository::StateRepository::new(workspace)
        .save(intent, state)
        .map_err(|error| LifecycleError::StatePersist(format!(
            "quick-start binding cleanup failed: {error}"
        )))
}

fn preflight_fresh_leader_identity(
    transport: &dyn Transport,
) -> Option<(String, Vec<String>, Vec<String>)> {
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|pane| !pane.is_empty());
    let current_endpoint = crate::tmux_backend::socket_name_from_tmux_env();
    match crate::layout::worker_env::caller_provider_resolution(
        pane.as_deref(),
        current_endpoint.as_deref(),
        transport.tmux_endpoint().as_deref(),
    ) {
        crate::layout::worker_env::CallerProviderResolution::Invalid => Some((
            "quick-start refused before spawn: invalid caller tuple".to_string(),
            vec!["stage=strict_provider reason=invalid_caller_tuple".to_string()],
            vec![
                "fix conflicting CALLER_* / TEAM_AGENT_LEADER_* identity; do not run claim-leader"
                    .to_string(),
            ],
        )),
        crate::layout::worker_env::CallerProviderResolution::Valid(_) => None,
        crate::layout::worker_env::CallerProviderResolution::Absent => {
            let Some(pane) = pane else {
                return None;
            };
            let command = transport
                .query(
                    &Target::Pane(PaneId::new(pane)),
                    PaneField::PaneCurrentCommand,
                )
                .ok()
                .flatten()
                .unwrap_or_default();
            let explicit = std::env::var("TEAM_AGENT_LEADER_PROVIDER")
                .ok()
                .filter(|provider| !provider.is_empty());
            if command.trim().is_empty() {
                return Some((
                    "quick-start refused before spawn: command unobservable".to_string(),
                    vec!["stage=command_observation reason=command_unobservable".to_string()],
                    vec![
                        "run from a live provider pane; do not run claim-leader".to_string(),
                    ],
                ));
            }
            if crate::leader::owner_bind::strict_owner_bind_provider(
                explicit.as_deref(),
                &command,
            )
            .is_some()
            {
                None
            } else {
                Some((
                    "quick-start refused before spawn: provider unresolved".to_string(),
                    vec!["stage=strict_provider reason=provider_unresolved".to_string()],
                    vec![
                        "set TEAM_AGENT_LEADER_PROVIDER or run from a provider pane; do not run claim-leader"
                            .to_string(),
                    ],
                ))
            }
        }
    }
}

fn bind_fresh_quick_start_leader(
    workspace: &Path,
    team_key: &str,
    seeded_owner: Option<&serde_json::Value>,
    transport: &dyn Transport,
) -> Result<bool, LifecycleError> {
    bind_fresh_quick_start_leader_with(
        workspace,
        team_key,
        seeded_owner,
        &mut RuntimeFreshQuickStartLeaderBindingOps {
            transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: None,
        },
    )
}

/// ---
/// purpose: 由角色目录推出 workspace 后一键起队
/// params:
///   agents_dir: 角色定义目录
///   name: 请求的团队名
///   yes: 免确认标志
///   team_id: 显式团队键，优先于 name
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
/// `quick_start(agents_dir, name, yes, team_id)`(`diagnose/quick_start.py:18`)。
/// 面向用户的零配置入口:编译 team_dir → `launch` → autobind leader receiver → 起
/// coordinator → `wait_ready` 轮询就绪。归入 lifecycle module(不与 diagnose 混)。
pub fn quick_start(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_in_workspace(&workspace, agents_dir, name, yes, team_id)
}

/// ---
/// purpose: 在指定 workspace 起队，transport 优先复用调用方所在 tmux socket
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    let transport = quick_start_tmux_backend(&workspace);
    quick_start_with_transport_in_workspace(&workspace, agents_dir, name, yes, team_id, &transport)
}

/// ---
/// purpose: 带显示开关与后端字面量的起队入口
/// params:
///   open_display: 为假时把 spec 的显示后端改成 none
///   backend: 未给或为 tmux 时走既有 tmux 路径，其余字面量经 transport 工厂解析
/// returns: 起队报告
/// errors: 后端字面量不认识时返回 TeamSelect；工厂拒绝或后端不可用时如实报错，不退回 tmux
/// ---
/// 0.5.x Phase 1d Batch 2: quick-start with an optional
/// `--backend <tmux|conpty>` override. When `backend` is `None` or
/// `Some("tmux")` the transport is the same one the legacy entrypoint
/// built (byte-equivalent to Phase 1c), so tmux users see no
/// behavioral change.
///
/// When `backend` is `Some("conpty")`, this call routes through the
/// factory. On a host without a live shim client (i.e. every
/// non-Windows host today), the resulting `ConPtyBackend` degrades
/// its spawn/inject/capture calls to `TransportError::MuxUnavailable`
/// honestly — MUST-NOT-13 + CR C-1 ①. Users see a real "conpty
/// unavailable" error rather than a silent tmux fallback.
pub fn quick_start_in_workspace_with_display_and_backend(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    open_display: bool,
    backend: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    // Default / `tmux` literal: preserve the existing tmux path
    // BYTE-FOR-BYTE. This is the CR §Batch 2 Verification anchor:
    // `quick-start` without `--backend` produces byte-equivalent tmux
    // behavior.
    let literal = backend.map(str::trim);
    let is_tmux_or_default = matches!(literal, None | Some("") | Some("tmux") | Some("TMUX"));
    if is_tmux_or_default {
        let transport = quick_start_tmux_backend(&workspace);
        return quick_start_with_transport_in_workspace_with_display(
            &workspace,
            agents_dir,
            name,
            yes,
            team_id,
            &transport,
            open_display,
        );
    }
    // Explicit non-tmux backend: route through the factory. Parse the
    // literal, then delegate. On unsupported literals or refused
    // preconditions we return `LifecycleError` — never silently pick
    // tmux (CR C-1 ①/②).
    let requested = crate::transport_factory::RequestedTransportBackend::parse_literal(
        literal.unwrap_or_default(),
    )
    .ok_or_else(|| {
        LifecycleError::TeamSelect(format!(
            "unsupported --backend literal {literal:?}; expected `tmux` or `conpty` \
             (Phase 1d does not auto-map `pty` to conpty — CR C-1 ②)"
        ))
    })?;
    let team_key_for_factory = team_id;
    // 0.5.x Windows portability Batch 8 F7 (leader msg_590b4dce0f68):
    // shim ownership moves to the coordinator. quick-start no longer
    // calls `spawn_shim_and_handshake` directly. Instead we ensure
    // the coordinator daemon is running; the coordinator's boot
    // path calls `conpty_shim::ensure_shim_running` (idempotent —
    // spawns if no live shim, reconnects if one is recorded).
    //
    // Rationale (from Batch 7 gate report F7): quick-start is a
    // one-shot process; if it owns the shim, the shim dies when
    // quick-start exits. Moving ownership to the coord daemon
    // gives us the "coord can die, shim survives" invariant the
    // design's §Shim Lifecycle chapter requires.
    //
    // The `ensure_coordinator_running` call is idempotent per
    // `coordinator/health.rs::start_coordinator`. On non-Windows
    // this whole block is cfg'd out.
    #[cfg(windows)]
    if matches!(
        requested,
        crate::transport_factory::RequestedTransportBackend::ConPty
    ) {
        let team_key_str = team_key_for_factory.ok_or_else(|| {
            LifecycleError::TeamSelect(
                "team_key required for --backend conpty on Windows (Batch 9 F8)".to_string(),
            )
        })?;
        // 0.5.x Windows portability Batch 9 F8 (leader msg_2a4cc1fa54c0):
        // Batch 8's seed-state pattern (writing active_team_key +
        // transport.kind to state.json before start_coordinator)
        // caused downstream launch code to see "existing runtime, use
        // restart" and skip spec compile. F8 fix: pass `--team`
        // directly to the coord daemon via `start_coordinator_with_team`
        // so state doesn't need pre-seeding.
        //
        // The coord daemon's `run_daemon_with_coordinator_and_boot_tmux`
        // then calls `ensure_shim_running` with the CLI-supplied
        // team_key. When quick-start's downstream code runs, state.json
        // still has whatever it had before (empty on fresh launch), so
        // the spec-compile + worker-spawn path runs normally.
        let run_ws = crate::coordinator::WorkspacePath::new(workspace.clone());
        let start_report =
            crate::coordinator::health::start_coordinator_with_team(&run_ws, Some(team_key_str))
                .map_err(|e| LifecycleError::TeamSelect(format!("coordinator start: {e}")))?;
        if !start_report.ok {
            return Err(LifecycleError::TeamSelect(format!(
                "coordinator start failed: schema_error={:?}, action={:?}",
                start_report.schema_error, start_report.action
            )));
        }
        // Give the coordinator a beat to write its transport.shim
        // block so the factory's `pipe_ready` gate opens on the
        // next resolve. The coordinator's `run_daemon` calls
        // `ensure_shim_running` inside its boot code path (see
        // `coordinator::backoff::run_daemon`).
        std::thread::sleep(std::time::Duration::from_millis(2500));
    }
    let input = crate::transport_factory::TransportFactoryInput::new(
        &workspace,
        crate::transport_factory::TransportPurpose::Launch,
    )
    .with_team_key(team_key_for_factory)
    .with_explicit_backend(Some(requested));
    // On Windows the factory needs to SEE the freshly-persisted
    // `state.transport.shim.pipe_ready = true` marker so its
    // `conpty_pipe_ready` gate opens.
    #[cfg(windows)]
    let state_value = crate::state::repository::StateRepository::new(&workspace)
        .load_workspace_if_exists_without_migrations()
        .ok()
        .flatten();
    #[cfg(windows)]
    let input = match state_value.as_ref() {
        Some(v) => input.with_state(Some(v)),
        None => input,
    };
    let resolved = crate::transport_factory::resolve_transport(input)
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // Hand the boxed backend down as `&dyn Transport`.
    quick_start_with_transport_in_workspace_with_display(
        &workspace,
        agents_dir,
        name,
        yes,
        team_id,
        &*resolved.backend,
        open_display,
    )
}

/// ---
/// purpose: 带注入 transport 的起队入口，由角色目录推 workspace
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_with_transport(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_with_transport_in_workspace(&workspace, agents_dir, name, yes, team_id, transport)
}

/// ---
/// purpose: 带注入 transport 与显式 workspace 的起队入口，默认开显示
/// returns: 起队报告
/// errors: 透传实体实现的错误
/// contract_id: lifecycle.quick_start.entry
/// ---
pub(crate) fn quick_start_with_transport_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    quick_start_with_transport_in_workspace_with_display(
        workspace, agents_dir, name, yes, team_id, transport, true,
    )
}

/// ---
/// purpose: 起队的实体实现，校验 leader pane、编译 spec、定团队键、判层级、必要时早退，然后起队并给 attach 指引
/// params:
///   open_display: 为假时把显示后端改成 none
/// returns: 起队报告，含 session 名与 attach 命令
/// errors: leader pane 环境无效返回 RequirementUnmet；角色目录不存在或编译失败返回 Compile；嵌套层级超限返回 RequirementUnmet；读 state 失败返回 StatePersist
/// ---
pub(crate) fn quick_start_with_transport_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    let mut discover = |requested: &str| {
        crate::lifecycle::launch::pi_mcp::pi_model_candidates(requested).map_err(|_| ())
    };
    quick_start_with_transport_in_workspace_with_display_pi_preflight(
        workspace,
        agents_dir,
        name,
        yes,
        team_id,
        transport,
        open_display,
        &mut discover,
    )
}

pub(crate) fn quick_start_with_transport_in_workspace_with_display_pi_preflight(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
    discover: &mut dyn FnMut(&str) -> Result<Vec<String>, ()>,
) -> Result<QuickStartReport, LifecycleError> {
    if !agents_dir.exists() {
        return Err(LifecycleError::Compile(format!(
            "agents dir not found: {}",
            agents_dir.display()
        )));
    }
    crate::compiler::preflight_pi_models_in_team_with(agents_dir, discover).map_err(|error| {
        LifecycleError::PiModelPreflight {
            requested: error.requested,
            candidates: error.candidates,
            action: error.action,
            not_ready: error.not_ready,
        }
    })?;
    let workspace = workspace.to_path_buf();
    let mut spec = crate::compiler::compile_team(agents_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    // B-7 / 036b N38 三行 fail-fast — TEAM_AGENT_LEADER_PANE_ID 主动路径在 quick-start
    // 入口验活;死/缺(Dead)的 pane 必须明确报错,不可 silent bind 到 spawner /
    // owner_bind / lease / display 任一消费点。被动路径(display/seed 等)各自走
    // 降级+event,不在这里挡。错误三行式:error(含 pane id 字面)/action(unset
    // 或修 env)/log(env var 名)。 Role/schema and Pi model admission above is pure and
    // must finish before this validation can emit a warning event.
    let team_workspace = team_workspace(agents_dir);
    let warning_workspaces = [workspace.as_path(), team_workspace.as_path()];
    validate_active_leader_pane_env_with_workspaces(transport, &warning_workspaces)?;
    override_spec_workspace(&mut spec, &workspace);
    if !open_display {
        override_spec_display_backend(&mut spec, "none");
    }
    let explicit_team_key = quick_start_requested_team_key(team_id, name).map(str::to_string);
    let canonical_team_key = explicit_team_key
        .clone()
        .or_else(|| spec_team_id(&spec).filter(|team| !team.is_empty()))
        .unwrap_or_else(|| {
            runtime_team_key_for_spec(
                &agents_dir.join("team.spec.yaml"),
                &spec,
                &spec_session_name(&spec),
            )
        });
    let requested_team = Some(canonical_team_key.clone());
    let team_depth = quick_start_depth_guard(
        &workspace,
        agents_dir,
        requested_team.as_deref(),
        matches!(transport.kind(), crate::transport::BackendKind::Tmux),
    )?;
    if team_depth.team_depth > 2 {
        let parent = team_depth.parent_team_key.as_deref().unwrap_or("");
        return Err(LifecycleError::RequirementUnmet(format!(
            "team nesting depth limit exceeded: parent_team_key={parent} parent_depth={} max_depth=2",
            team_depth.team_depth.saturating_sub(1)
        )));
    }
    let state_path = crate::state::persist::runtime_state_path(&workspace);
    if state_path.exists() {
        let state = crate::state::persist::load_runtime_state(&workspace)
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        if requested_team
            .as_deref()
            .is_none_or(|team| runtime_state_has_quick_start_team(&state, team))
        {
            let session_name = state
                .get("session_name")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(SessionName::new);
            let attach_commands = session_name
                .as_ref()
                .map(|session| {
                    let windows = quick_start_attach_window_names(&state);
                    attach_commands_for_runtime_windows(
                        state
                            .get("tmux_endpoint")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| {
                                state.get("tmux_socket").and_then(serde_json::Value::as_str)
                            }),
                        &workspace,
                        session,
                        windows.iter().map(String::as_str),
                    )
                })
                .unwrap_or_default();
            // Stage QR (design doc .team/artifacts/quickstart-restart-separation-design.md):
            // quick-start is initial-creation-only. When the team
            // already has runtime state, do NOT mention `--fresh`
            // (it's been removed); steer the operator to the
            // restart flow which owns resume + reset semantics.
            let mut next_actions = vec![
                "this team already has runtime state — use `team-agent restart` \
                 to resume it (quick-start is for first-time creation only). \
                 If recovery is impossible and the operator EXPLICITLY \
                 accepts losing context, restart accepts `--allow-fresh`."
                    .to_string(),
            ];
            if session_name.is_some() {
                if should_emit_workspace_socket_missing_hint(
                    state
                        .get("tmux_socket_source")
                        .and_then(serde_json::Value::as_str),
                    crate::tmux_backend::socket_probe_missing_for_workspace(&workspace),
                ) {
                    next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(
                        &workspace,
                    ));
                }
                next_actions.extend(attach_commands.iter().cloned());
            }
            let agent_ids = state
                .get("agents")
                .and_then(serde_json::Value::as_object)
                .map(|agents| agents.keys().cloned().collect::<Vec<_>>())
                .or_else(|| {
                    state
                        .get("teams")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|teams| requested_team.as_ref().and_then(|team| teams.get(team)))
                        .and_then(|team| team.get("agents"))
                        .and_then(serde_json::Value::as_object)
                        .map(|agents| agents.keys().cloned().collect())
                })
                .unwrap_or_default();
            return Ok(QuickStartReport::ExistingRuntime {
                team: requested_team.clone(),
                session_name,
                state_path: Some(state_path),
                next_actions,
                attach_commands,
                agent_ids,
            });
        }
    }
    // CR-040/042: repeated quick-start from one template with distinct --team-id/--name
    // must NOT collide on the template-derived tmux session. Override the compiled
    // spec's runtime.session_name with one derived from the REQUESTED team identity
    // so launch_with_transport (which reads runtime.session_name) spawns into an
    // isolated session per requested team.
    if let Some(requested) = explicit_team_key.as_deref() {
        override_spec_session_name(&mut spec, &format!("team-{requested}"));
    }
    let session_name = spec_session_name(&spec);
    // team_key 已在入口由显式 id/name 或 compiled spec identity 单次确定。
    let state_team_key = canonical_team_key;
    warn_ignored_owner_team_id(workspace.as_path(), agents_dir, &state_team_key);
    if let Some((summary, blockers, next_actions)) = preflight_fresh_leader_identity(transport) {
        return Ok(QuickStartReport::PreflightBlocked {
            summary,
            blockers,
            next_actions,
            attach_commands: Vec::new(),
        });
    }
    // E5 spec 迁移:spec 写到 .team/runtime/<team_key>/(中间产物,绝不落用户目录 agents_dir)。
    // Bug2:原子写(tmp+rename),避免半截 spec。
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, &state_team_key);
    write_spec_atomic(&spec_path, &spec)?;
    let _store = crate::message_store::MessageStore::open(&workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let resolved_spec_path =
        std::fs::canonicalize(&spec_path).unwrap_or_else(|_| spec_path.clone());
    let scoped_endpoint = transport.tmux_endpoint();
    let mut state = initial_runtime_state(
        &spec,
        &resolved_spec_path,
        &workspace,
        agents_dir,
        &state_team_key,
        scoped_endpoint.as_deref(),
    );
    // Keep this attempt-local seed separate from persisted `claimed_via`;
    // historical quick-start rows are never sufficient cleanup authority.
    let seeded_owner = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(&state_team_key))
        .and_then(|team| team.get("team_owner"))
        .filter(|owner| {
            owner
                .get("claimed_via")
                .and_then(serde_json::Value::as_str)
                == Some("quick-start")
        })
        .cloned();
    // 0.5.x Phase 1d hot-path 接线(裁决1 msg_76e1d98202b8): use the
    // generic annotator that writes `state.transport = { kind, source }`
    // for every backend AND (for tmux) preserves the existing
    // `tmux_endpoint`/`tmux_socket`/`tmux_socket_source` fields via the
    // inner `annotate_runtime_tmux_endpoint` call. Source is threaded
    // from the caller-side factory selection when available; the
    // legacy quick-start entrypoint currently passes `None` (defaults
    // to "unknown" in the payload) — Phase 2 refactor will thread
    // ResolvedTransport.source through the launch signature.
    annotate_runtime_transport(&mut state, transport, &workspace, None);
    save_launched_team_state_for_key(&workspace, &state, Some(&state_team_key), None)?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    // FIX (rt-host-a real-machine finding): dry_run=false so launch_with_transport calls spawn_agents
    // and really creates the tmux session + worker windows (was hardcoded true → never spawned, which
    // also starved the coordinator: no session → first tick TmuxSessionMissing → run_daemon loop exits).
    // 0.5.38 (`.team/artifacts/startup-latency-locate.md` §5): quick-start
    // owns the outer `launch.phase` timer so `coordinator_start` /
    // `readiness_wait` / `completed` fire monotonically after the inner
    // launch_with_transport's own `compile_spec` / `spawn_all` events.
    let quick_start_phase_timer = crate::lifecycle::restart::RestartPhaseTimer::start();
    let mut launch =
        launch_with_transport_in_workspace(&workspace, &spec_path, false, yes, true, transport)?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    // Fresh initialization owns this one fail-closed bind attempt. It is
    // independent of display layout, so --no-display never suppresses receiver
    // binding. Readiness receives true only after canonical registry readback.
    let _ = take_fresh_bind_refusal();
    launch.leader_receiver_attached = bind_fresh_quick_start_leader(
        &workspace,
        &state_team_key,
        seeded_owner.as_ref(),
        transport,
    )?;
    if !launch.leader_receiver_attached {
        if let Some((stage, reason)) = take_fresh_bind_refusal() {
            launch.leader_bind_stage = Some(stage.to_string());
            launch.leader_bind_reason = Some(reason.to_string());
        }
    }
    launch.session_capture_incomplete_agents =
        quick_start_session_capture_incomplete_agents(&workspace, &state_team_key);
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.clone());
    let coordinator_started = crate::coordinator::start_coordinator(&coordinator_workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    quick_start_phase_timer.emit(&workspace, "launch.phase", "coordinator_start");
    let coordinator_action = if coordinator_started {
        "coordinator started"
    } else {
        "coordinator not started"
    };
    // BUG-7: build an honest readiness verdict from the post-spawn runtime state.
    // - If persist_spawn_agent_state (BUG-2 fix) marked any agent non-running, the
    //   team is observably Degraded.
    // - Otherwise the framework cannot itself verify that the worker's MCP tool set
    //   loaded successfully (provider-side codex/claude schema rejections happen
    //   asynchronously after spawn), so the verdict is PendingToolLoad — never
    //   bare Ready.
    quick_start_phase_timer.emit(&workspace, "launch.phase", "readiness_wait");
    let worker_readiness = quick_start_worker_readiness(&workspace, &state_team_key);
    let attach_windows = load_runtime_state(&workspace)
        .ok()
        .map(|state| {
            attach_window_names_with_managed_leader(
                &state,
                started_attach_window_names(&launch.started),
            )
        })
        .unwrap_or_else(|| started_attach_window_names(&launch.started));
    let attach_commands = attach_commands_for_runtime_windows(
        launch.tmux_endpoint.as_deref(),
        &workspace,
        &session_name,
        attach_windows.iter().map(String::as_str),
    );
    let mut next_actions = vec![format!(
        "team compiled; real spawn is behind the transport/provider boundary; {coordinator_action}"
    )];
    // Stage QR (design doc
    // .team/artifacts/quickstart-restart-separation-design.md):
    // quick-start is initial-creation-only. After success, remind the
    // operator that subsequent starts must go through restart so the
    // user's context (sessions, ownership, tasks) survives.
    next_actions.push(
        "quick-start initialized this team; for all subsequent starts use \
         `team-agent restart` to resume context (quick-start refuses on \
         already-initialised teams)"
            .to_string(),
    );
    if should_emit_workspace_socket_missing_hint(
        selected_tmux_socket_source(transport, &workspace),
        crate::tmux_backend::socket_probe_missing_for_workspace(&workspace),
    ) {
        next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(
            &workspace,
        ));
    }
    next_actions.extend(attach_commands.iter().cloned());
    let display_backend = state
        .get("display_backend")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none")
        .to_string();
    quick_start_phase_timer.emit(&workspace, "launch.phase", "completed");
    Ok(QuickStartReport::Ready {
        session_name,
        launch: Box::new(launch),
        next_actions,
        attach_commands,
        display_backend,
        worker_readiness,
        team: state_team_key,
    })
}

#[cfg(test)]
mod fresh_quick_start_leader_binding_tests {
    use super::*;
    use crate::layout::worker_env::{CALLER_ENDPOINT_ENV, CALLER_PANE_ENV, CALLER_PROVIDER_ENV};
    use crate::transport::test_support::OfflineTransport;
    use crate::transport::{PaneInfo, SessionName, WindowName};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::hermetic::HermeticTestEnv;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn workspace(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ta-quick-start-bind-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        crate::state::persist::save_runtime_state(
            &path,
            &json!({
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}}
            }),
        )
        .unwrap();
        path
    }

    struct MockOps {
        pane: Option<String>,
        explicit_provider: Option<String>,
        endpoint: Option<String>,
        command: Option<String>,
        attach_ok: bool,
        register_ok: bool,
        readback_ok: bool,
        attach_calls: usize,
        register_calls: usize,
        readback_calls: usize,
        attached_provider: Option<crate::provider::Provider>,
        applied_grant: Option<(serde_json::Value, serde_json::Value)>,
    }

    impl Default for MockOps {
        fn default() -> Self {
            Self {
                pane: Some("%42".to_string()),
                explicit_provider: Some("pi".to_string()),
                endpoint: Some("/private/tmp/tmux-test/default".to_string()),
                command: Some("pi".to_string()),
                attach_ok: true,
                register_ok: true,
                readback_ok: true,
                attach_calls: 0,
                register_calls: 0,
                readback_calls: 0,
                attached_provider: None,
                applied_grant: None,
            }
        }
    }

    #[test]
    fn invalid_pi_tool_category_is_rejected_before_runtime_persistence() {
        let root = std::env::temp_dir().join(format!(
            "ta-quick-start-pi-tools-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let team = root.join(".team/current");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: pi-tools\nprovider: pi\n---\n\nPi team.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents/worker.md"),
            "---\nname: worker\nrole: Worker\nprovider: pi\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n  - fs_read\n  - provider_builtin\n---\n\nWorker.\n",
        )
        .unwrap();

        let transport = crate::transport::test_support::OfflineTransport::new();
        let mut discover = |_requested: &str| -> Result<Vec<String>, ()> { Err(()) };
        let error = quick_start_with_transport_in_workspace_with_display_pi_preflight(
            &root,
            &team,
            None,
            true,
            None,
            &transport,
            false,
            &mut discover,
        )
        .expect_err("invalid Pi category must fail before quick-start persistence");
        let error = error.to_string();
        assert!(error.contains("Pi does not support Team Agent tool category \"provider_builtin\""));
        assert!(error.contains("remove it from the role's tools"));
        assert!(
            !crate::state::persist::runtime_state_path(&root).exists(),
            "quick-start validation failure must not persist runtime state"
        );
        assert!(
            !root.join(".team/runtime").exists(),
            "quick-start validation failure must not create runtime artifacts"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    impl FreshQuickStartLeaderBindingOps for MockOps {
        fn caller_pane(&mut self) -> Option<String> {
            self.pane.clone()
        }

        fn explicit_provider(&mut self) -> Option<String> {
            self.explicit_provider.clone()
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            self.endpoint.clone()
        }

        fn observe_command(&mut self, _pane: &PaneId) -> Option<String> {
            self.command.clone()
        }

        fn attach(
            &mut self,
            workspace: &Path,
            state: &mut serde_json::Value,
            pane: &PaneId,
            provider: crate::provider::Provider,
        ) -> bool {
            self.applied_grant = None;
            self.attach_calls += 1;
            self.attached_provider = Some(provider);
            if !self.attach_ok {
                state["failed_attach_mutation"] = json!(true);
                let _ = crate::state::persist::save_runtime_state(workspace, state);
                return false;
            }
            let team_key = state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("fresh")
                .to_string();
            let expected_owner = state.get("team_owner").cloned();
            let expected_receiver = state.get("leader_receiver").cloned();
            let owner = expected_owner.clone().unwrap_or_else(|| {
                json!({
                    "pane_id": pane.as_str(),
                    "provider": "pi",
                    "leader_session_uuid": "uuid-fresh",
                    "owner_epoch": 1
                })
            });
            let owner_epoch = owner
                .get("owner_epoch")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1);
            let receiver = json!({
                "mode": "direct_tmux",
                "pane_id": pane.as_str(),
                "status": "attached",
                "provider": "pi",
                "leader_session_uuid": "uuid-fresh",
                "owner_epoch": owner_epoch,
                "tmux_socket": self.endpoint
            });
            state["leader_receiver"] = receiver.clone();
            state["team_owner"] = owner.clone();
            state["owner_epoch"] = json!(owner_epoch);
            state["teams"][team_key.as_str()]["leader_receiver"] = receiver.clone();
            state["teams"][team_key.as_str()]["team_owner"] = owner.clone();
            state["teams"][team_key.as_str()]["owner_epoch"] = json!(owner_epoch);
            let saved = match (expected_owner.as_ref(), expected_receiver.as_ref()) {
                (Some(expected_owner), Some(expected_receiver)) => {
                    crate::state::persist::save_runtime_state_with_receiver_authority_and_expected(
                        workspace,
                        state,
                        &team_key,
                        expected_owner,
                        expected_receiver,
                    )
                }
                _ => crate::state::persist::save_runtime_state_with_receiver_authority(
                    workspace,
                    state,
                    &team_key,
                    None,
                ),
            };
            if saved.is_ok() {
                self.applied_grant = Some((owner, receiver));
                true
            } else {
                false
            }
        }

        fn applied_grant(&self) -> Option<(serde_json::Value, serde_json::Value)> {
            self.applied_grant.clone()
        }

        fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.register_calls += 1;
            let mut persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            assert_eq!(
                persisted
                    .get("teams")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|teams| teams.get(team_key))
                    .and_then(|team| team.get("leader_receiver"))
                    .and_then(|receiver| receiver.get("pane_id"))
                    .and_then(serde_json::Value::as_str),
                Some("%42")
            );
            if !self.register_ok {
                persisted["failed_registry_mutation"] = json!(true);
                crate::state::persist::save_runtime_state(workspace, &persisted).unwrap();
            }
            self.register_ok
        }

        fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.readback_calls += 1;
            let mut persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
            if !self.readback_ok {
                persisted["failed_readback_mutation"] = json!(true);
                crate::state::persist::save_runtime_state(workspace, &persisted).unwrap();
            }
            self.readback_ok
                && persisted
                    .get("teams")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|teams| teams.get(team_key))
                    .and_then(|team| team.get("leader_receiver"))
                    .is_some_and(|receiver| !receiver.is_null())
        }
    }

    #[test]
    fn fresh_binding_persists_then_registers_then_requires_canonical_readback() {
        let workspace = workspace("positive");
        let mut ops = MockOps::default();
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(ops.attached_provider, Some(crate::provider::Provider::Pi));
        assert_eq!(ops.attach_calls, 1);
        assert_eq!(ops.register_calls, 1);
        assert_eq!(ops.readback_calls, 1);
        let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(
            state
                .get("teams")
                .and_then(serde_json::Value::as_object)
                .and_then(|teams| teams.get("fresh"))
                .and_then(|team| team.get("leader_receiver"))
                .and_then(|receiver| receiver.get("pane_id"))
                .and_then(serde_json::Value::as_str),
            Some("%42")
        );
        assert!(
            crate::event_log::EventLog::new(&workspace)
                .tail(0)
                .unwrap()
                .is_empty(),
            "successful bind must not emit a refusal event"
        );
    }

    #[test]
    fn fresh_bind_refusal_event_keeps_first_safe_stage_and_reason() {
        for (case, expected_stage, expected_reason) in [
            ("missing_pane", "caller_pane", "caller_pane_missing"),
            ("empty_command", "command_observation", "command_unobservable"),
            ("unknown_provider", "strict_provider", "provider_unresolved"),
            (
                "existing_owner",
                "persisted_binding",
                "persisted_binding_mismatch",
            ),
            ("attach_failure", "attach", "attach_failed"),
            (
                "registry_failure",
                "registry_register",
                "registry_register_failed",
            ),
            (
                "readback_failure",
                "registry_readback",
                "registry_readback_failed",
            ),
        ] {
            let workspace = workspace(case);
            let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
            let mut ops = MockOps::default();
            match case {
                "missing_pane" => ops.pane = None,
                "empty_command" => ops.command = Some("  ".to_string()),
                "unknown_provider" => {
                    ops.explicit_provider = None;
                    ops.command = Some("node".to_string());
                }
                "existing_owner" => state["team_owner"] = json!({"pane_id": "%old"}),
                "attach_failure" => ops.attach_ok = false,
                "registry_failure" => ops.register_ok = false,
                "readback_failure" => ops.readback_ok = false,
                _ => unreachable!(),
            }
            crate::state::persist::save_runtime_state(&workspace, &state).unwrap();

            assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
                .unwrap());
            let events = crate::event_log::EventLog::new(&workspace).tail(0).unwrap();
            assert_eq!(events.len(), 1, "{case} must emit one first-refusal event");
            assert_eq!(events[0]["event"], json!(FRESH_BIND_REFUSAL_EVENT));
            assert_eq!(events[0]["operation"], json!("fresh_quick_start_leader_bind"));
            assert_eq!(events[0]["team_key"], json!("fresh"));
            assert_eq!(events[0]["stage"], json!(expected_stage));
            assert_eq!(events[0]["reason"], json!(expected_reason));
            assert!(events[0].get("command").is_none());
            assert!(events[0].get("pane_id").is_none());
        }
    }

    #[test]
    fn scope_refusal_event_is_safe_and_targeted() {
        let empty_workspace = std::env::temp_dir().join(format!(
            "ta-quick-start-bind-scope-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&empty_workspace).unwrap();
        let mut ops = MockOps::default();
        assert!(!bind_fresh_quick_start_leader_with(&empty_workspace, "fresh", None, &mut ops)
            .unwrap());
        let events = crate::event_log::EventLog::new(&empty_workspace).tail(0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["stage"], json!("scope"));
        assert_eq!(events[0]["reason"], json!("scope_unresolved"));
        assert_eq!(events[0]["team_key"], json!("fresh"));
        assert!(events[0].get("error").is_none());
        assert!(events[0].get("argv").is_none());
        let _ = std::fs::remove_dir_all(empty_workspace);

        let workspace = workspace("scope-key-mismatch");
        let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        state["active_team_key"] = json!("fresh");
        state["teams"] = json!({"fresh": {"team_key": "fresh", "agents": {}}});
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let mut ops = MockOps::default();
        assert!(!bind_fresh_quick_start_leader_with(&workspace, "current", None, &mut ops)
            .unwrap());
        let events = crate::event_log::EventLog::new(&workspace).tail(0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["stage"], json!("scope"));
        assert_eq!(events[0]["reason"], json!("scope_key_mismatch"));
    }

    #[test]
    fn every_pre_attach_refusal_is_byte_preserving_and_never_registers() {
        for case in [
            "missing_pane",
            "unverifiable_pane",
            "empty_command",
            "unknown_provider",
            "unknown_explicit_provider",
            "team_mismatch",
            "existing_owner",
            "existing_receiver",
            "provider_mismatch",
            "socket_mismatch",
            "workspace_mismatch",
            "dual_state_mismatch",
        ] {
            let workspace = workspace(case);
            let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
            let mut ops = MockOps::default();
            let team_key = if case == "team_mismatch" {
                "other"
            } else {
                "fresh"
            };
            match case {
                "missing_pane" => ops.pane = None,
                "unverifiable_pane" => ops.command = None,
                "empty_command" => ops.command = Some("  ".to_string()),
                "unknown_provider" => {
                    ops.explicit_provider = None;
                    ops.command = Some("node".to_string());
                }
                "unknown_explicit_provider" => {
                    ops.explicit_provider = Some("unknown".to_string());
                    ops.command = Some("codex".to_string());
                }
                "existing_owner" => state["team_owner"] = json!({"pane_id": "%old"}),
                "existing_receiver" => {
                    state["leader_receiver"] = json!({"pane_id": "%old"})
                }
                "provider_mismatch" | "socket_mismatch" | "workspace_mismatch"
                | "dual_state_mismatch" => {
                    state["workspace"] = json!(workspace);
                    state["team_owner"] = json!({
                        "pane_id": "%42", "provider": "pi",
                        "leader_session_uuid": "uuid-fresh", "owner_epoch": 1
                    });
                    state["leader_receiver"] = json!({
                        "mode": "direct_tmux", "status": "attached", "pane_id": "%42",
                        "provider": "pi", "leader_session_uuid": "uuid-fresh", "owner_epoch": 1,
                        "tmux_socket": "/private/tmp/tmux-test/default"
                    });
                    match case {
                        "provider_mismatch" => state["leader_receiver"]["provider"] = json!("codex"),
                        "socket_mismatch" => state["leader_receiver"]["tmux_socket"] = json!("/tmp/other"),
                        "workspace_mismatch" => state["workspace"] = json!(workspace.join("other")),
                        "dual_state_mismatch" => state["team_owner"]["owner_epoch"] = json!(2),
                        _ => unreachable!(),
                    }
                }
                "team_mismatch" => {}
                _ => unreachable!(),
            }
            crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
            let before = std::fs::read(crate::state::persist::runtime_state_path(&workspace))
                .unwrap();
            assert!(
                !bind_fresh_quick_start_leader_with(&workspace, team_key, None, &mut ops)
                    .unwrap(),
                "{case} must refuse"
            );
            assert_eq!(
                before,
                std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap(),
                "{case} changed state bytes"
            );
            assert_eq!(ops.attach_calls, 0, "{case} reached attach");
            assert_eq!(ops.register_calls, 0, "{case} reached registry write");
            assert_eq!(ops.readback_calls, 0, "{case} reached readback");
        }
    }

    struct HomeGuard(Option<std::ffi::OsString>);

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self(old)
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                std::env::set_var("HOME", value);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn ambient_dual_state_workspace(tag: &str) -> PathBuf {
        let workspace = workspace(tag);
        let ambient = "/private/tmp/tmux-test/default";
        crate::state::persist::save_runtime_state(
            &workspace,
            &json!({
                "workspace": workspace,
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}},
                "teams": {
                    "current": {
                        "team_key": "current",
                        "session_name": "team-parent",
                        "agents": {}
                    },
                    "fresh": {
                        "workspace": workspace,
                        "team_key": "fresh",
                        "session_name": "team-fresh",
                        "agents": {"sol": {"status": "running", "provider": "pi"}},
                        "team_owner": {
                            "pane_id": "%42",
                            "provider": "pi",
                            "leader_session_uuid": "uuid-fresh",
                            "owner_epoch": 1
                        },
                        "leader_receiver": {
                            "mode": "direct_tmux",
                            "status": "attached",
                            "pane_id": "%42",
                            "provider": "pi",
                            "leader_session_uuid": "uuid-fresh",
                            "owner_epoch": 1,
                            "tmux_socket": ambient
                        },
                        "owner_epoch": 1
                    }
                }
            }),
        )
        .unwrap();
        workspace
    }

    struct AmbientRegistryOps {
        readback_ok: bool,
        attach_calls: usize,
        registry_receipt: Option<crate::leader::registry::RegistryWriteReceipt>,
    }

    impl FreshQuickStartLeaderBindingOps for AmbientRegistryOps {
        fn caller_pane(&mut self) -> Option<String> {
            Some("%42".to_string())
        }

        fn explicit_provider(&mut self) -> Option<String> {
            Some("pi".to_string())
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            Some("/private/tmp/tmux-test/default".to_string())
        }

        fn observe_command(&mut self, _pane: &PaneId) -> Option<String> {
            Some("pi".to_string())
        }

        fn attach(
            &mut self,
            _workspace: &Path,
            _state: &mut serde_json::Value,
            _pane: &PaneId,
            _provider: crate::provider::Provider,
        ) -> bool {
            self.attach_calls += 1;
            false
        }

        fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
            let Some(outcome) = crate::leader::registry::register_binding_from_state_best_effort(
                workspace,
                Some(team_key),
                "quick-start",
            ) else {
                return false;
            };
            self.registry_receipt = outcome.receipt;
            outcome.status == "registered" && outcome.path.is_some()
        }

        fn registry_receipt(&self) -> Option<crate::leader::registry::RegistryWriteReceipt> {
            self.registry_receipt.clone()
        }

        fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.readback_ok && launched_team_receiver_is_attached(workspace, team_key)
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn ambient_dual_state_registers_and_reads_back_when_workspace_socket_is_missing() {
        let workspace = ambient_dual_state_workspace("ambient-dual-state");
        let home = workspace.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = HomeGuard::set(&home);
        assert!(crate::tmux_backend::socket_probe_missing_for_workspace(
            &workspace
        ));
        let mut ops = AmbientRegistryOps {
            readback_ok: true,
            attach_calls: 0,
            registry_receipt: None,
        };
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(ops.attach_calls, 0, "preseeded dual state must not reattach");
        let path = crate::leader::registry::registry_dir().unwrap().join(format!(
            "{}__fresh.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        let entry: crate::leader::registry::LeaderRegistryEntry =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            entry.channel.get("tmux_socket").and_then(serde_json::Value::as_str),
            Some("/private/tmp/tmux-test/default")
        );
    }

    #[test]
    fn historical_quick_start_owner_is_not_cleanup_authority() {
        let workspace = workspace("historical-quick-start-owner");
        let mut state = crate::state::persist::load_runtime_state(&workspace).unwrap();
        state["workspace"] = json!(workspace);
        state["team_owner"] = json!({
            "pane_id": "%old",
            "provider": "pi",
            "leader_session_uuid": "uuid-old",
            "owner_epoch": 4,
            "claimed_via": "quick-start"
        });
        state["leader_receiver"] = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%old",
            "provider": "pi",
            "leader_session_uuid": "uuid-old",
            "owner_epoch": 4,
            "tmux_socket": "/private/tmp/tmux-test/old"
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let before = std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap();
        let mut ops = MockOps::default();

        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(
            before,
            std::fs::read(crate::state::persist::runtime_state_path(&workspace)).unwrap(),
            "historical claimed_via alone must not authorize cleanup"
        );
    }

    fn matching_seed() -> serde_json::Value {
        json!({
            "pane_id": "%42",
            "provider": "pi",
            "leader_session_uuid": "uuid-fresh",
            "owner_epoch": 1,
            "claimed_via": "quick-start"
        })
    }

    fn matching_receiver() -> serde_json::Value {
        json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%42",
            "provider": "pi",
            "leader_session_uuid": "uuid-fresh",
            "owner_epoch": 1,
            "tmux_socket": "/private/tmp/tmux-test/default"
        })
    }

    fn workspace_with_matching_seed(tag: &str) -> (PathBuf, serde_json::Value) {
        let workspace = workspace(tag);
        let seed = matching_seed();
        crate::state::persist::save_runtime_state(
            &workspace,
            &json!({
                "workspace": workspace,
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}},
                "teams": {
                    "fresh": {
                        "workspace": workspace,
                        "team_key": "fresh",
                        "session_name": "team-fresh",
                        "agents": {"sol": {"status": "running", "provider": "pi"}},
                        "team_owner": seed,
                        "leader_receiver": matching_receiver(),
                        "owner_epoch": 1
                    }
                }
            }),
        )
        .unwrap();
        let seed = crate::state::persist::load_runtime_state(&workspace).unwrap()["teams"]["fresh"]
            ["team_owner"]
            .clone();
        (workspace, seed)
    }

    struct ConcurrentTakeover {
        owner: serde_json::Value,
        receiver: serde_json::Value,
        epoch: u64,
    }

    fn takeover_binding(epoch: u64) -> ConcurrentTakeover {
        ConcurrentTakeover {
            owner: json!({
                "pane_id": "%99",
                "provider": "codex",
                "leader_session_uuid": "uuid-other",
                "owner_epoch": epoch,
                "claimed_via": "claim-leader"
            }),
            receiver: json!({
                "mode": "direct_tmux",
                "status": "attached",
                "pane_id": "%99",
                "provider": "codex",
                "leader_session_uuid": "uuid-other",
                "owner_epoch": epoch,
                "tmux_socket": "/private/tmp/tmux-test/other"
            }),
            epoch,
        }
    }

    struct SeedCleanupOps {
        inner: MockOps,
        takeover: Option<ConcurrentTakeover>,
        poison_state_file: bool,
    }

    impl FreshQuickStartLeaderBindingOps for SeedCleanupOps {
        fn caller_pane(&mut self) -> Option<String> {
            self.inner.caller_pane()
        }

        fn explicit_provider(&mut self) -> Option<String> {
            self.inner.explicit_provider()
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            self.inner.tmux_endpoint()
        }

        fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
            self.inner.observe_command(pane)
        }

        fn attach(
            &mut self,
            workspace: &Path,
            state: &mut serde_json::Value,
            pane: &PaneId,
            provider: crate::provider::Provider,
        ) -> bool {
            self.inner.attach(workspace, state, pane, provider)
        }

        fn applied_grant(&self) -> Option<(serde_json::Value, serde_json::Value)> {
            self.inner.applied_grant()
        }

        fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.inner.register_calls += 1;
            if let Some(takeover) = &self.takeover {
                let mut persisted = crate::state::persist::load_runtime_state(workspace).unwrap();
                persisted["teams"][team_key]["team_owner"] = takeover.owner.clone();
                persisted["teams"][team_key]["leader_receiver"] = takeover.receiver.clone();
                persisted["teams"][team_key]["owner_epoch"] = json!(takeover.epoch);
                crate::state::persist::save_runtime_state_with_receiver_authority(
                    workspace,
                    &persisted,
                    team_key,
                    None,
                )
                .unwrap();
                assert_fresh_binding_kept(
                    &crate::state::persist::load_runtime_state(workspace).unwrap(),
                    takeover,
                );
            }
            if self.poison_state_file {
                let path = crate::state::persist::runtime_state_path(workspace);
                let _ = std::fs::remove_file(&path);
                std::fs::create_dir_all(&path).unwrap();
            }
            false
        }

        fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.inner.canonical_readback(workspace, team_key)
        }
    }

    fn failed_register_ops(takeover: Option<ConcurrentTakeover>, poison: bool) -> SeedCleanupOps {
        SeedCleanupOps {
            inner: MockOps {
                register_ok: false,
                ..MockOps::default()
            },
            takeover,
            poison_state_file: poison,
        }
    }

    fn assert_fresh_binding_kept(
        persisted: &serde_json::Value,
        takeover: &ConcurrentTakeover,
    ) {
        assert_eq!(
            persisted["teams"]["fresh"]["team_owner"],
            takeover.owner,
            "concurrent team_owner must survive seeded cleanup"
        );
        assert_eq!(
            persisted["teams"]["fresh"]["leader_receiver"],
            takeover.receiver,
            "concurrent leader_receiver must survive seeded cleanup"
        );
        assert_eq!(
            persisted["teams"]["fresh"]["owner_epoch"],
            json!(takeover.epoch),
            "concurrent owner_epoch must survive seeded cleanup"
        );
    }

    #[test]
    fn seeded_cleanup_clears_exact_seed_through_bind_repository_entry() {
        let (workspace, seed) = workspace_with_matching_seed("seed-cleanup-success");
        let mut ops = failed_register_ops(None, false);
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        assert_eq!(ops.inner.attach_calls, 1);
        assert_eq!(ops.inner.register_calls, 1);
        assert!(ops
            .inner
            .applied_grant
            .as_ref()
            .is_some_and(|(owner, receiver)| {
                owner == &seed && receiver == &matching_receiver()
            }));
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert!(
            persisted
                .pointer("/teams/fresh/team_owner")
                .is_some_and(serde_json::Value::is_null),
            "matching seed must be cleared via bind/repository; got {}",
            persisted["teams"]["fresh"]["team_owner"]
        );
        assert!(
            persisted
                .pointer("/teams/fresh/leader_receiver")
                .is_some_and(serde_json::Value::is_null)
        );
        assert_eq!(
            persisted["teams"]["fresh"]["owner_epoch"],
            json!(1),
            "seed cleanup must not bump owner_epoch"
        );
    }

    #[test]
    fn seeded_cleanup_preserves_equal_epoch_persisted_concurrent_owner() {
        let (workspace, seed) = workspace_with_matching_seed("seed-cleanup-concurrent");
        let takeover = takeover_binding(1);
        let mut ops = failed_register_ops(Some(takeover_binding(1)), false);
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        assert_fresh_binding_kept(
            &crate::state::persist::load_runtime_state(&workspace).unwrap(),
            &takeover,
        );
    }

    #[test]
    fn seeded_cleanup_preserves_epoch2_takeover_owner_receiver_epoch() {
        let (workspace, seed) = workspace_with_matching_seed("seed-cleanup-epoch2");
        let takeover = takeover_binding(2);
        let mut ops = failed_register_ops(Some(takeover_binding(2)), false);
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        assert_fresh_binding_kept(
            &crate::state::persist::load_runtime_state(&workspace).unwrap(),
            &takeover,
        );
    }

    #[test]
    fn seeded_cleanup_propagates_persist_error() {
        let (workspace, seed) = workspace_with_matching_seed("seed-cleanup-persist-err");
        let mut ops = failed_register_ops(None, true);
        let error = bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops,
        )
        .expect_err("poisoned state.json must surface cleanup persist failure");
        assert_eq!(ops.inner.attach_calls, 1);
        assert_eq!(ops.inner.register_calls, 1);
        assert!(ops
            .inner
            .applied_grant
            .as_ref()
            .is_some_and(|(owner, receiver)| {
                owner == &seed && receiver == &matching_receiver()
            }));
        match error {
            LifecycleError::StatePersist(text) => {
                assert!(
                    text.contains("quick-start binding cleanup failed"),
                    "unexpected persist error: {text}"
                );
            }
            other => panic!("expected StatePersist, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(crate::state::persist::runtime_state_path(&workspace));
    }

    #[test]
    fn attach_registry_or_readback_failure_rolls_back_exact_state_bytes() {
        for (case, mut ops) in [
            (
                "attach-failure",
                MockOps {
                    attach_ok: false,
                    ..MockOps::default()
                },
            ),
            (
                "registry-failure",
                MockOps {
                    register_ok: false,
                    ..MockOps::default()
                },
            ),
            (
                "readback-failure",
                MockOps {
                    readback_ok: false,
                    ..MockOps::default()
                },
            ),
        ] {
            let workspace = workspace(case);
            let state_path = crate::state::persist::runtime_state_path(&workspace);
            let before = std::fs::read(&state_path).unwrap();
            assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
                .unwrap());
            assert_eq!(
                std::fs::read(&state_path).unwrap(),
                before,
                "{case} left partial state bytes"
            );
            match case {
                "attach-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 0);
                    assert_eq!(ops.readback_calls, 0);
                }
                "registry-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 1);
                    assert_eq!(ops.readback_calls, 0);
                }
                "readback-failure" => {
                    assert_eq!(ops.attach_calls, 1);
                    assert_eq!(ops.register_calls, 1);
                    assert_eq!(ops.readback_calls, 1);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn socket_missing_hint_is_suppressed_only_for_selected_leader_env_transport() {
        assert!(!should_emit_workspace_socket_missing_hint(
            Some("leader_env"),
            true
        ));
        assert!(should_emit_workspace_socket_missing_hint(
            Some("workspace"),
            true
        ));
        assert!(should_emit_workspace_socket_missing_hint(None, true));
        assert!(!should_emit_workspace_socket_missing_hint(
            Some("workspace"),
            false
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn failed_canonical_readback_removes_new_registry_bytes_and_preserves_dual_state_bytes() {
        let workspace = ambient_dual_state_workspace("ambient-readback-rollback");
        let home = workspace.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home = HomeGuard::set(&home);
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        let before = std::fs::read(&state_path).unwrap();
        let registry_path = crate::leader::registry::registry_dir().unwrap().join(format!(
            "{}__fresh.json",
            crate::leader::registry::workspace_hash(&workspace)
        ));
        let mut ops = AmbientRegistryOps {
            readback_ok: false,
            attach_calls: 0,
            registry_receipt: None,
        };
        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops)
            .unwrap());
        assert_eq!(std::fs::read(state_path).unwrap(), before);
        assert!(!registry_path.exists(), "failed readback left registry bytes");
    }

    struct RecordingRuntimeOps<'a> {
        inner: RuntimeFreshQuickStartLeaderBindingOps<'a>,
        attached_provider: Option<crate::provider::Provider>,
        attach_calls: usize,
    }

    impl<'a> RecordingRuntimeOps<'a> {
        fn new(transport: &'a dyn Transport) -> Self {
            Self {
                inner: RuntimeFreshQuickStartLeaderBindingOps {
                    transport,
                    last_failure_reason: None,
                    cas_conflict: false,
                    applied_grant: None,
                    registry_receipt: None,
                    nonce_writer: None,
                },
                attached_provider: None,
                attach_calls: 0,
            }
        }
    }

    impl FreshQuickStartLeaderBindingOps for RecordingRuntimeOps<'_> {
        fn caller_pane(&mut self) -> Option<String> {
            self.inner.caller_pane()
        }

        fn caller_resolution(&mut self) -> crate::layout::worker_env::CallerProviderResolution {
            self.inner.caller_resolution()
        }

        fn explicit_provider(&mut self) -> Option<String> {
            self.inner.explicit_provider()
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            self.inner.tmux_endpoint()
        }

        fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
            self.inner.observe_command(pane)
        }

        fn attach(
            &mut self,
            workspace: &Path,
            state: &mut serde_json::Value,
            pane: &PaneId,
            provider: crate::provider::Provider,
        ) -> bool {
            self.attach_calls += 1;
            self.attached_provider = Some(provider);
            let wire = crate::provider::wire::provider_wire(provider);
            state["leader_receiver"] = json!({
                "mode": "direct_tmux",
                "pane_id": pane.as_str(),
                "status": "attached",
                "provider": wire,
                "leader_session_uuid": "uuid-fresh",
                "owner_epoch": 1,
                "tmux_socket": self.inner.tmux_endpoint()
            });
            state["team_owner"] = json!({
                "pane_id": pane.as_str(),
                "provider": wire,
                "leader_session_uuid": "uuid-fresh",
                "owner_epoch": 1
            });
            crate::state::persist::save_runtime_state(workspace, state).is_ok()
        }

        fn register(&mut self, _workspace: &Path, _team_key: &str) -> bool {
            true
        }

        fn canonical_readback(&mut self, _workspace: &Path, _team_key: &str) -> bool {
            true
        }
    }

    fn runtime_workspace(hermetic: &HermeticTestEnv, tag: &str) -> PathBuf {
        let path = hermetic.workspace(tag);
        crate::state::persist::save_runtime_state(
            &path,
            &json!({
                "active_team_key": "fresh",
                "team_key": "fresh",
                "session_name": "team-fresh",
                "agents": {"sol": {"status": "running", "provider": "pi"}}
            }),
        )
        .unwrap();
        path
    }

    fn refusal_reason(workspace: &Path) -> Option<(String, String)> {
        let events = crate::event_log::EventLog::new(workspace).tail(0).unwrap();
        let event = events.last()?;
        Some((
            event
                .get("stage")
                .and_then(serde_json::Value::as_str)?
                .to_string(),
            event
                .get("reason")
                .and_then(serde_json::Value::as_str)?
                .to_string(),
        ))
    }

    fn seeded_runtime_owner(workspace: &Path) -> serde_json::Value {
        let mut state = crate::state::persist::load_runtime_state(workspace).unwrap();
        assert!(super::spec_state::seed_launched_owner_from_env(
            &mut state,
            Some("/tmp/tmux.sock")
        ));
        crate::state::persist::save_runtime_state(workspace, &state).unwrap();
        crate::state::persist::load_runtime_state(workspace)
            .unwrap()
            .pointer("/teams/fresh/team_owner")
            .cloned()
            .expect("seeded team owner")
    }

    fn caller_target(path: PathBuf, nonce: Option<&str>) -> PaneInfo {
        let mut leader_env = BTreeMap::new();
        if let Some(nonce) = nonce {
            leader_env.insert(
                crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY.to_string(),
                nonce.to_string(),
            );
        }
        PaneInfo {
            pane_id: PaneId::new("%1"),
            session: SessionName::new("parent"),
            window_index: Some(0),
            window_name: Some(WindowName::new("caller")),
            pane_index: Some(0),
            tty: Some("/dev/pts/1".to_string()),
            current_command: Some("bash".to_string()),
            current_path: Some(path),
            active: true,
            pane_pid: Some(101),
            leader_env,
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn runtime_valid_caller_binds_typed_pi_through_bash() {
        let hermetic = HermeticTestEnv::enter("runtime-valid-pi-bash");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash");
        let mut ops = RecordingRuntimeOps::new(&transport);
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap());
        assert_eq!(ops.attached_provider, Some(crate::provider::Provider::Pi));
        assert_eq!(ops.attach_calls, 1);
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_bind_publishes_fresh_caller_authority_for_live_parent_pane() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-fresh-authority");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let target = caller_target(parent, Some("shared-live-nonce"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target.clone()]);
        let mut ops = RuntimeFreshQuickStartLeaderBindingOps {
            transport: &transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: None,
        };
        assert!(bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        let receiver = persisted
            .pointer("/teams/fresh/leader_receiver")
            .cloned()
            .expect("published receiver");
        assert_eq!(receiver["scope_authority"], json!("fresh_caller"));
        assert_eq!(receiver["binding_nonce"], json!("shared-live-nonce"));
        assert_eq!(
            receiver["authorized_team_workspace"],
            json!(workspace.to_string_lossy().to_string())
        );
        assert_eq!(receiver["owner_epoch"], json!(1));
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        let registry: crate::leader::registry::LeaderRegistryEntry =
            serde_json::from_slice(&std::fs::read(registry_path).unwrap()).unwrap();
        assert_eq!(registry.channel["scope_authority"], json!("fresh_caller"));
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(&workspace, &receiver, &transport),
            crate::messaging::LeaderChannelResolution::Live(_)
        ));

        let mut stale_target = target.clone();
        stale_target.leader_env.insert(
            crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY.to_string(),
            "stale-live-nonce".to_string(),
        );
        let stale_transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_targets(vec![stale_target]);
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(
                &workspace,
                &receiver,
                &stale_transport
            ),
            crate::messaging::LeaderChannelResolution::Unbound(
                crate::messaging::LeaderChannelUnbound::PaneWorkspaceMismatch(_)
            )
        ));

        let missing_target = caller_target(hermetic.root().join("parent"), None);
        let missing_transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_targets(vec![missing_target]);
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(
                &workspace,
                &receiver,
                &missing_transport
            ),
            crate::messaging::LeaderChannelResolution::Unbound(
                crate::messaging::LeaderChannelUnbound::PaneWorkspaceMismatch(_)
            )
        ));

        let mut missing_authority_receiver = receiver.clone();
        missing_authority_receiver
            .as_object_mut()
            .unwrap()
            .remove("scope_authority");
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(
                &workspace,
                &missing_authority_receiver,
                &transport
            ),
            crate::messaging::LeaderChannelResolution::Unbound(
                crate::messaging::LeaderChannelUnbound::PaneWorkspaceMismatch(_)
            )
        ));

        let mut foreign_receiver = receiver.clone();
        foreign_receiver["authorized_team_workspace"] =
            json!(hermetic.root().join("foreign").to_string_lossy().to_string());
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(&workspace, &foreign_receiver, &transport),
            crate::messaging::LeaderChannelResolution::Unbound(
                crate::messaging::LeaderChannelUnbound::PaneWorkspaceMismatch(_)
            )
        ));

        let wrong_socket_transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/other.sock")
            .with_targets(vec![target]);
        assert_eq!(
            crate::messaging::resolve_live_leader_channel(
                &workspace,
                &receiver,
                &wrong_socket_transport
            ),
            crate::messaging::LeaderChannelResolution::Unbound(
                crate::messaging::LeaderChannelUnbound::EndpointMismatch
            )
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_children_reuse_live_pane_nonce_without_overwrite() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-shared-nonce");
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let target_without_nonce = caller_target(parent.clone(), None);
        let target_with_winner = caller_target(parent, Some("writer-winner"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target_with_winner.clone()])
            .with_target_snapshots(vec![
                vec![target_without_nonce.clone()],
                vec![target_with_winner.clone()],
                vec![target_without_nonce],
                vec![target_with_winner],
            ]);
        let first_workspace = runtime_workspace(&hermetic, "child-a");
        let second_workspace = runtime_workspace(&hermetic, "child-b");
        let first_seed = seeded_runtime_owner(&first_workspace);
        let second_seed = seeded_runtime_owner(&second_workspace);
        let mut first_ops = RuntimeFreshQuickStartLeaderBindingOps {
            transport: &transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: Some(&nonce_writer_winner),
        };
        assert!(bind_fresh_quick_start_leader_with(
            &first_workspace,
            "fresh",
            Some(&first_seed),
            &mut first_ops
        )
        .unwrap());
        let mut second_ops = RuntimeFreshQuickStartLeaderBindingOps {
            transport: &transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: Some(&nonce_writer_winner),
        };
        assert!(bind_fresh_quick_start_leader_with(
            &second_workspace,
            "fresh",
            Some(&second_seed),
            &mut second_ops
        )
        .unwrap());
        let first_receiver = crate::state::persist::load_runtime_state(&first_workspace).unwrap();
        let second_receiver = crate::state::persist::load_runtime_state(&second_workspace).unwrap();
        assert_eq!(
            first_receiver
                .pointer("/teams/fresh/leader_receiver/binding_nonce")
                .and_then(serde_json::Value::as_str),
            Some("writer-winner")
        );
        assert_eq!(
            second_receiver
                .pointer("/teams/fresh/leader_receiver/binding_nonce")
                .and_then(serde_json::Value::as_str),
            Some("writer-winner")
        );
        let first_channel = first_receiver
            .pointer("/teams/fresh/leader_receiver")
            .expect("first receiver");
        let second_channel = second_receiver
            .pointer("/teams/fresh/leader_receiver")
            .expect("second receiver");
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(
                &first_workspace,
                first_channel,
                &transport
            ),
            crate::messaging::LeaderChannelResolution::Live(_)
        ));
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(
                &second_workspace,
                second_channel,
                &transport
            ),
            crate::messaging::LeaderChannelResolution::Live(_)
        ));
    }

    struct RuntimeCommitFailureOps<'a> {
        inner: RuntimeFreshQuickStartLeaderBindingOps<'a>,
        register_ok: bool,
        readback_ok: bool,
        publish_winner_after_register: bool,
        break_event_log: bool,
        break_registry_lock: bool,
        break_registry_read: bool,
    }

    impl FreshQuickStartLeaderBindingOps for RuntimeCommitFailureOps<'_> {
        fn caller_pane(&mut self) -> Option<String> {
            self.inner.caller_pane()
        }

        fn caller_resolution(&mut self) -> crate::layout::worker_env::CallerProviderResolution {
            self.inner.caller_resolution()
        }

        fn explicit_provider(&mut self) -> Option<String> {
            self.inner.explicit_provider()
        }

        fn tmux_endpoint(&mut self) -> Option<String> {
            self.inner.tmux_endpoint()
        }

        fn observe_command(&mut self, pane: &PaneId) -> Option<String> {
            self.inner.observe_command(pane)
        }

        fn attach(
            &mut self,
            workspace: &Path,
            state: &mut serde_json::Value,
            pane: &PaneId,
            provider: crate::provider::Provider,
        ) -> bool {
            if self.break_event_log {
                let event_path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
                let _ = std::fs::remove_file(&event_path);
                std::fs::create_dir_all(&event_path).unwrap();
            }
            self.inner.attach(workspace, state, pane, provider)
        }

        fn attach_cas_conflict(&self) -> bool {
            self.inner.attach_cas_conflict()
        }

        fn applied_grant(&self) -> Option<(serde_json::Value, serde_json::Value)> {
            self.inner.applied_grant()
        }

        fn registry_receipt(&self) -> Option<crate::leader::registry::RegistryWriteReceipt> {
            self.inner.registry_receipt()
        }

        fn register(&mut self, workspace: &Path, team_key: &str) -> bool {
            let registered = self.inner.register(workspace, team_key);
            if !self.register_ok {
                return false;
            }
            if self.break_registry_lock {
                let lock_path = crate::leader::registry::registry_dir()
                    .expect("registry dir")
                    .join(".registry.lock");
                let _ = std::fs::remove_file(&lock_path);
                std::fs::create_dir_all(&lock_path).unwrap();
            }
            if self.break_registry_read {
                if let Some(receipt) = self.inner.registry_receipt() {
                    let _ = std::fs::remove_file(&receipt.path);
                    std::fs::create_dir_all(&receipt.path).unwrap();
                }
            }
            if registered && self.publish_winner_after_register {
                let mut winner = crate::state::persist::load_runtime_state(workspace).unwrap();
                winner["teams"][team_key]["leader_receiver"]["pane_id"] = json!("%winner");
                crate::state::persist::save_runtime_state_with_receiver_authority(
                    workspace,
                    &winner,
                    team_key,
                    None,
                )
                .unwrap();
                let _ = crate::leader::registry::register_binding_from_state_best_effort(
                    workspace,
                    Some(team_key),
                    "concurrent",
                );
            }
            registered
        }

        fn canonical_readback(&mut self, workspace: &Path, team_key: &str) -> bool {
            self.readback_ok && self.inner.canonical_readback(workspace, team_key)
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_register_failure_rolls_back_child_authority() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-rollback");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let target_without_nonce = caller_target(parent.clone(), None);
        let target_with_nonce = caller_target(parent, Some("writer-winner"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target_with_nonce.clone()])
            .with_target_snapshots(vec![
                vec![target_without_nonce],
                vec![target_with_nonce],
            ]);
        let mut ops = RuntimeCommitFailureOps {
            inner: RuntimeFreshQuickStartLeaderBindingOps {
                transport: &transport,
                last_failure_reason: None,
                cas_conflict: false,
                applied_grant: None,
                registry_receipt: None,
                nonce_writer: Some(&nonce_writer_winner),
            },
            register_ok: false,
            readback_ok: true,
            publish_winner_after_register: false,
            break_event_log: false,
            break_registry_lock: false,
            break_registry_read: false,
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert!(persisted
            .pointer("/teams/fresh/team_owner")
            .is_some_and(serde_json::Value::is_null));
        assert!(persisted
            .pointer("/teams/fresh/leader_receiver")
            .is_some_and(serde_json::Value::is_null));
        assert!(!persisted.to_string().contains("fresh_caller"));
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        assert!(!registry_path.exists(), "failed child grant left registry row");
        let observed = transport.list_targets().unwrap();
        assert_eq!(
            observed[0]
                .leader_env
                .get(crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY)
                .map(String::as_str),
            Some("writer-winner"),
            "failed child bind must not remove the shared first-writer marker"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_post_save_event_error_rolls_back_applied_grant() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-post-save-event-error");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let target = caller_target(parent, Some("post-save-event-nonce"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target]);
        let mut ops = RuntimeCommitFailureOps {
            inner: RuntimeFreshQuickStartLeaderBindingOps {
                transport: &transport,
                last_failure_reason: None,
                cas_conflict: false,
                applied_grant: None,
                registry_receipt: None,
                nonce_writer: None,
            },
            register_ok: true,
            readback_ok: true,
            publish_winner_after_register: false,
            break_event_log: true,
            break_registry_lock: false,
            break_registry_read: false,
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops,
        )
        .unwrap());
        assert_eq!(ops.inner.last_failure_reason, Some("attach_event_log_failed"));
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert!(persisted
            .pointer("/teams/fresh/team_owner")
            .is_some_and(serde_json::Value::is_null));
        assert!(persisted
            .pointer("/teams/fresh/leader_receiver")
            .is_some_and(serde_json::Value::is_null));
        assert!(!persisted.to_string().contains("fresh_caller"));
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        assert!(!registry_path.exists());
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_nonce_change_before_readback_rolls_back_child_authority() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-nonce-drift");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let first_target = caller_target(parent.clone(), Some("before-readback"));
        let mut changed_target = first_target.clone();
        changed_target.leader_env.insert(
            crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY.to_string(),
            "after-readback".to_string(),
        );
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_target_snapshots(vec![vec![first_target], vec![changed_target]]);
        let mut ops = RuntimeFreshQuickStartLeaderBindingOps {
            transport: &transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: None,
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        assert_eq!(ops.last_failure_reason, Some("receiver_live_channel_unavailable"));
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert!(persisted
            .pointer("/teams/fresh/team_owner")
            .is_some_and(serde_json::Value::is_null));
        assert!(persisted
            .pointer("/teams/fresh/leader_receiver")
            .is_some_and(serde_json::Value::is_null));
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        assert!(!registry_path.exists());
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_readback_failure_rolls_back_registry_and_child_authority() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-readback-rollback");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let target = caller_target(parent, Some("readback-nonce"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target]);
        let mut ops = RuntimeCommitFailureOps {
            inner: RuntimeFreshQuickStartLeaderBindingOps {
                transport: &transport,
                last_failure_reason: None,
                cas_conflict: false,
                applied_grant: None,
                registry_receipt: None,
                nonce_writer: None,
            },
            register_ok: true,
            readback_ok: false,
            publish_winner_after_register: false,
            break_event_log: false,
            break_registry_lock: false,
            break_registry_read: false,
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert!(persisted
            .pointer("/teams/fresh/team_owner")
            .is_some_and(serde_json::Value::is_null));
        assert!(persisted
            .pointer("/teams/fresh/leader_receiver")
            .is_some_and(serde_json::Value::is_null));
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        assert!(!registry_path.exists(), "readback failure left registry row");
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_readback_rollback_preserves_concurrent_registry_winner() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-readback-winner");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let target = caller_target(parent, Some("readback-winner-nonce"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target]);
        let mut ops = RuntimeCommitFailureOps {
            inner: RuntimeFreshQuickStartLeaderBindingOps {
                transport: &transport,
                last_failure_reason: None,
                cas_conflict: false,
                applied_grant: None,
                registry_receipt: None,
                nonce_writer: None,
            },
            register_ok: true,
            readback_ok: false,
            publish_winner_after_register: true,
            break_event_log: false,
            break_registry_lock: false,
            break_registry_read: false,
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops,
        )
        .unwrap());
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(
            persisted
                .pointer("/teams/fresh/team_owner/pane_id")
                .and_then(serde_json::Value::as_str),
            Some("%1"),
            "same-owner winner must retain the owner"
        );
        assert_eq!(
            persisted
                .pointer("/teams/fresh/leader_receiver/pane_id")
                .and_then(serde_json::Value::as_str),
            Some("%winner"),
            "rollback must not clear a newer receiver"
        );
        assert_eq!(
            persisted
                .pointer("/teams/fresh/leader_receiver/owner_epoch")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "rollback must preserve the winner epoch"
        );
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        let winner: crate::leader::registry::LeaderRegistryEntry =
            serde_json::from_slice(&std::fs::read(registry_path).unwrap()).unwrap();
        assert_eq!(winner.channel["pane_id"], json!("%winner"));
        assert_eq!(winner.source, "concurrent");
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_runtime_registry_rollback_errors_are_visible_after_state_cleanup() {
        for (tag, break_registry_lock, break_registry_read) in [
            ("lock", true, false),
            ("read", false, true),
        ] {
            let hermetic = HermeticTestEnv::enter(&format!(
                "runtime-seeded-registry-rollback-{tag}"
            ));
            let workspace = runtime_workspace(&hermetic, "fresh");
            let parent = hermetic.root().join("parent");
            std::fs::create_dir_all(&parent).unwrap();
            let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
            let _pane = hermetic.with_env("TMUX_PANE", "%1");
            let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
            let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
            let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
            let seed = seeded_runtime_owner(&workspace);
            let target = caller_target(parent, Some("rollback-error-nonce"));
            let transport = OfflineTransport::new()
                .with_tmux_endpoint("/tmp/tmux.sock")
                .with_pane_current_command("%1", "bash")
                .with_targets(vec![target]);
            let mut ops = RuntimeCommitFailureOps {
                inner: RuntimeFreshQuickStartLeaderBindingOps {
                    transport: &transport,
                    last_failure_reason: None,
                    cas_conflict: false,
                    applied_grant: None,
                    registry_receipt: None,
                    nonce_writer: None,
                },
                register_ok: true,
                readback_ok: false,
                publish_winner_after_register: false,
                break_event_log: false,
                break_registry_lock,
                break_registry_read,
            };
            let error = bind_fresh_quick_start_leader_with(
                &workspace,
                "fresh",
                Some(&seed),
                &mut ops,
            )
            .expect_err("registry rollback failure must be visible");
            assert!(error
                .to_string()
                .contains("quick-start registry rollback failed"));
            let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
            assert!(persisted
                .pointer("/teams/fresh/team_owner")
                .is_some_and(serde_json::Value::is_null));
            assert!(persisted
                .pointer("/teams/fresh/leader_receiver")
                .is_some_and(serde_json::Value::is_null));
            assert!(!persisted.to_string().contains("fresh_caller"));
        }
    }

    fn nonce_writer_winner(
        _endpoint: &str,
        _pane: &PaneId,
        _candidate: &str,
    ) -> Result<String, crate::leader::LeaderError> {
        Ok("writer-winner".to_string())
    }

    fn nonce_writer_failure(
        _endpoint: &str,
        _pane: &PaneId,
        _candidate: &str,
    ) -> Result<String, crate::leader::LeaderError> {
        Err(crate::leader::LeaderError::Tmux(
            "conditional nonce writer failed".to_string(),
        ))
    }

    #[test]
    #[serial_test::serial(env)]
    fn runtime_missing_verified_pane_observation_refuses_and_cleans_seed() {
        for (tag, observation, expected_reason) in [
            ("list-error", "list-error", "caller_pane_unobservable"),
            ("pane-absent", "pane-absent", "caller_pane_not_live"),
            ("cwd-missing", "cwd-missing", "caller_cwd_unobservable"),
        ] {
            let hermetic = HermeticTestEnv::enter(&format!("runtime-observation-{tag}"));
            let workspace = runtime_workspace(&hermetic, "fresh");
            let parent = hermetic.root().join("parent");
            std::fs::create_dir_all(&parent).unwrap();
            let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
            let _pane = hermetic.with_env("TMUX_PANE", "%1");
            let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
            let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
            let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
            let seed = seeded_runtime_owner(&workspace);
            let mut transport = OfflineTransport::new()
                .with_tmux_endpoint("/tmp/tmux.sock")
                .with_pane_current_command("%1", "bash");
            match observation {
                "list-error" => {
                    transport = transport.with_list_targets_error("pane query failed");
                }
                "pane-absent" => {}
                "cwd-missing" => {
                    let mut target = caller_target(parent, None);
                    target.current_path = None;
                    transport = transport.with_targets(vec![target]);
                }
                _ => unreachable!(),
            }
            let mut ops = RuntimeFreshQuickStartLeaderBindingOps {
                transport: &transport,
                last_failure_reason: None,
                cas_conflict: false,
                applied_grant: None,
                registry_receipt: None,
                nonce_writer: None,
            };
            assert!(!bind_fresh_quick_start_leader_with(
                &workspace,
                "fresh",
                Some(&seed),
                &mut ops
            )
            .unwrap());
            assert_eq!(ops.last_failure_reason, Some(expected_reason));
            assert_eq!(refusal_reason(&workspace).map(|(_, reason)| reason), Some(expected_reason.to_string()));
            let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
            assert!(persisted
                .pointer("/teams/fresh/team_owner")
                .is_some_and(serde_json::Value::is_null));
            assert!(persisted
                .pointer("/teams/fresh/leader_receiver")
                .is_some_and(serde_json::Value::is_null));
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn first_nonce_writer_is_product_generated_and_consumer_live() {
        let hermetic = HermeticTestEnv::enter("runtime-first-nonce-writer");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let mut state = crate::state::projection::resolve_runtime_team_scope(
            &workspace,
            Some("fresh"),
        )
        .unwrap()
        .state;
        let expected_owner = state["team_owner"].clone();
        let expected_receiver = state["leader_receiver"].clone();
        let target = caller_target(parent.clone(), None);
        let event_log = crate::event_log::EventLog::new(&workspace);
        let (receiver, _) = crate::leader::attach_leader_to_state_with_target_and_controls(
            &workspace,
            &mut state,
            Some(&PaneId::new("%1")),
            crate::provider::Provider::Pi,
            &event_log,
            crate::leader::LeaseSource::QuickStart,
            true,
            Some(&target),
            Some(&expected_owner),
            Some(&expected_receiver),
            Some(&nonce_writer_winner),
        )
        .unwrap();
        let receiver = serde_json::to_value(receiver).unwrap();
        assert_eq!(receiver["binding_nonce"], json!("writer-winner"));
        let observed = caller_target(parent, Some("writer-winner"));
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_targets(vec![observed]);
        assert!(matches!(
            crate::messaging::resolve_live_leader_channel(&workspace, &receiver, &transport),
            crate::messaging::LeaderChannelResolution::Live(_)
        ));

        let workspace = runtime_workspace(&hermetic, "writer-failure");
        let seed = seeded_runtime_owner(&workspace);
        let mut state = crate::state::projection::resolve_runtime_team_scope(
            &workspace,
            Some("fresh"),
        )
        .unwrap()
        .state;
        let expected_owner = state["team_owner"].clone();
        let expected_receiver = state["leader_receiver"].clone();
        let target = caller_target(hermetic.root().join("parent"), None);
        let result = crate::leader::attach_leader_to_state_with_target_and_controls(
            &workspace,
            &mut state,
            Some(&PaneId::new("%1")),
            crate::provider::Provider::Pi,
            &crate::event_log::EventLog::new(&workspace),
            crate::leader::LeaseSource::QuickStart,
            true,
            Some(&target),
            Some(&expected_owner),
            Some(&expected_receiver),
            Some(&nonce_writer_failure),
        );
        assert!(result.is_err());
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(
            persisted.pointer("/teams/fresh/leader_receiver/binding_nonce"),
            None
        );
        assert!(!persisted.to_string().contains("fresh_caller"));
        let _ = seed;
    }

    #[test]
    #[serial_test::serial(env)]
    fn seeded_cas_conflict_preserves_same_owner_receiver_and_registry_through_bind_rollback() {
        let hermetic = HermeticTestEnv::enter("runtime-seeded-cas-winner");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let parent = hermetic.root().join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let seed = seeded_runtime_owner(&workspace);
        let concurrent_writer = |_: &str,
                                 _: &PaneId,
                                 _: &str|
         -> Result<String, crate::leader::LeaderError> {
            let mut winner = crate::state::persist::load_runtime_state(&workspace)
                .map_err(crate::leader::LeaderError::State)?;
            winner["teams"]["fresh"]["leader_receiver"]["pane_id"] = json!("%winner");
            crate::state::persist::save_runtime_state_with_receiver_authority(
                &workspace,
                &winner,
                "fresh",
                None,
            )
            .map_err(crate::leader::LeaderError::State)?;
            let _ = crate::leader::registry::register_binding_from_state_best_effort(
                &workspace,
                Some("fresh"),
                "concurrent",
            );
            Ok("writer-winner".to_string())
        };
        let target = caller_target(parent, None);
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash")
            .with_targets(vec![target]);
        let mut ops = RuntimeFreshQuickStartLeaderBindingOps {
            transport: &transport,
            last_failure_reason: None,
            cas_conflict: false,
            applied_grant: None,
            registry_receipt: None,
            nonce_writer: Some(&concurrent_writer),
        };
        assert!(!bind_fresh_quick_start_leader_with(
            &workspace,
            "fresh",
            Some(&seed),
            &mut ops
        )
        .unwrap());
        assert!(ops.cas_conflict);
        assert_eq!(ops.last_failure_reason, Some("attach_cas_conflict"));
        let persisted = crate::state::persist::load_runtime_state(&workspace).unwrap();
        assert_eq!(
            persisted.pointer("/teams/fresh/team_owner/pane_id").and_then(serde_json::Value::as_str),
            Some("%1")
        );
        assert_eq!(
            persisted.pointer("/teams/fresh/leader_receiver/pane_id").and_then(serde_json::Value::as_str),
            Some("%winner")
        );
        assert_eq!(
            persisted.pointer("/teams/fresh/team_owner/owner_epoch").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            persisted.pointer("/teams/fresh/leader_receiver/owner_epoch").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        let registry_path = crate::leader::registry::registry_dir()
            .unwrap()
            .join(format!(
                "{}__fresh.json",
                crate::leader::registry::workspace_hash(&workspace)
            ));
        assert!(registry_path.exists(), "concurrent winner registry was removed");
        let winner_entry: crate::leader::registry::LeaderRegistryEntry =
            serde_json::from_slice(&std::fs::read(registry_path).unwrap()).unwrap();
        assert_eq!(winner_entry.channel["pane_id"], json!("%winner"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn runtime_invalid_caller_refuses_even_when_command_is_pi() {
        let hermetic = HermeticTestEnv::enter("runtime-invalid-command-pi");
        let workspace = runtime_workspace(&hermetic, "fresh");
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
        let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%9");
        let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, "/tmp/tmux.sock");
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "pi");
        let mut ops = RecordingRuntimeOps::new(&transport);
        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap());
        assert_eq!(ops.attach_calls, 0);
        assert_eq!(ops.attached_provider, None);
        assert_eq!(
            refusal_reason(&workspace),
            Some((
                "strict_provider".to_string(),
                "provider_unresolved".to_string()
            ))
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn runtime_socket_and_explicit_conflicts_refuse_without_command_fallback() {
        for (tag, endpoint, leader, scoped) in [
            (
                "socket",
                "/tmp/other.sock",
                None,
                "/tmp/tmux.sock",
            ),
            (
                "scoped",
                "/tmp/tmux.sock",
                None,
                "/tmp/product.sock",
            ),
            (
                "leader",
                "/tmp/tmux.sock",
                Some("codex"),
                "/tmp/tmux.sock",
            ),
        ] {
            let hermetic = HermeticTestEnv::enter(&format!("runtime-conflict-{tag}"));
            let workspace = runtime_workspace(&hermetic, tag);
            let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
            let _pane = hermetic.with_env("TMUX_PANE", "%1");
            let _provider = hermetic.with_env(CALLER_PROVIDER_ENV, "pi");
            let _caller_pane = hermetic.with_env(CALLER_PANE_ENV, "%1");
            let _caller_endpoint = hermetic.with_env(CALLER_ENDPOINT_ENV, endpoint);
            let _leader = leader.map(|value| hermetic.with_env("TEAM_AGENT_LEADER_PROVIDER", value));
            let transport = OfflineTransport::new()
                .with_tmux_endpoint(scoped)
                .with_pane_current_command("%1", "pi");
            let mut ops = RecordingRuntimeOps::new(&transport);
            assert!(
                !bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap(),
                "{tag} must refuse"
            );
            assert_eq!(ops.attach_calls, 0, "{tag}");
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn runtime_absent_keeps_explicit_and_direct_paths() {
        let hermetic = HermeticTestEnv::enter("runtime-absent-compat");
        let workspace = runtime_workspace(&hermetic, "explicit");
        let _tmux = hermetic.with_env("TMUX", "/tmp/tmux.sock,1,0");
        let _pane = hermetic.with_env("TMUX_PANE", "%1");
        let _leader = hermetic.with_env("TEAM_AGENT_LEADER_PROVIDER", "pi");
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash");
        let mut ops = RecordingRuntimeOps::new(&transport);
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap());
        assert_eq!(ops.attached_provider, Some(crate::provider::Provider::Pi));

        let workspace = runtime_workspace(&hermetic, "direct");
        drop(_leader);
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "pi");
        let mut ops = RecordingRuntimeOps::new(&transport);
        assert!(bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap());
        assert_eq!(ops.attached_provider, Some(crate::provider::Provider::Pi));

        let workspace = runtime_workspace(&hermetic, "unknown-shell");
        let transport = OfflineTransport::new()
            .with_tmux_endpoint("/tmp/tmux.sock")
            .with_pane_current_command("%1", "bash");
        let mut ops = RecordingRuntimeOps::new(&transport);
        assert!(!bind_fresh_quick_start_leader_with(&workspace, "fresh", None, &mut ops).unwrap());
        assert_eq!(ops.attach_calls, 0);
        assert_eq!(
            refusal_reason(&workspace),
            Some((
                "strict_provider".to_string(),
                "provider_unresolved".to_string()
            ))
        );
    }
}
