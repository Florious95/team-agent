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

pub(crate) fn quick_start_tmux_backend(workspace: &Path) -> crate::tmux_backend::TmuxBackend {
    if let Some(endpoint) = crate::tmux_backend::socket_name_from_tmux_env() {
        crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&endpoint)
    } else {
        crate::tmux_backend::TmuxBackend::for_workspace(workspace)
    }
}

pub(crate) fn selected_tmux_socket_source(
    transport: &dyn Transport,
    workspace: &Path,
) -> Option<&'static str> {
    let endpoint = transport.tmux_endpoint()?;
    if crate::tmux_backend::socket_name_from_tmux_env().as_deref() == Some(endpoint.as_str()) {
        Some("leader_env")
    } else if endpoint == crate::tmux_backend::socket_name_for_workspace(workspace) {
        Some("workspace")
    } else {
        None
    }
}

pub(crate) fn configure_adaptive_pane_title(
    workspace: &Path,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
    pane: &PaneId,
    agent_id: &str,
) {
    if let Err(error) =
        transport.configure_adaptive_pane_title(session_name, window, pane, agent_id)
    {
        let message = format!("adaptive layout pane title failed for {agent_id}: {error}");
        eprintln!("Warning: {message}");
        if let Err(event_error) = crate::event_log::EventLog::new(workspace).write(
            "adaptive_layout.pane_title_failed",
            serde_json::json!({
                "agent_id": agent_id,
                "session": session_name.as_str(),
                "window": window.as_str(),
                "pane_id": pane.as_str(),
                "warning": message,
            }),
        ) {
            eprintln!(
                "Warning: adaptive_layout.pane_title_failed event write failed: {event_error}"
            );
        }
    }
}

pub(super) fn explicit_quick_start_workspace(workspace: &Path) -> PathBuf {
    std::fs::canonicalize(workspace).unwrap_or_else(|_| {
        if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(workspace)
        }
    })
}

/// `quick_start` with an injected transport — tests inject a recording mock so the REAL spawn path
/// (launch dry_run=false → spawn_agents) is asserted without a live tmux; prod uses the real TmuxBackend.
/// Annotate `state.tmux_endpoint` / `state.tmux_socket` (and `tmux_socket_source`)
/// from the active transport. Originally only called at `launch_with_transport`
/// init time; **`0.3.24` opened this to `pub(crate)` so restart/add/fork can keep
/// the persisted endpoint synchronized with the transport they actually used,
/// closing the silent socket-drift gap** (single state-save path; no parallel
/// "annotate after spawn" race with coordinator).
/// 0.5.x Phase 1d Batch 2: generic runtime-transport annotator.
///
/// Writes `state.transport = { kind, source }` for every backend
/// (kind = wire string `"tmux"` | `"conpty"`; source = wire string
/// from `ResolvedTransport.source` when known, else `"unknown"`).
///
/// For tmux, ALSO forwards to `annotate_runtime_tmux_endpoint` so the
/// existing `tmux_endpoint` / `tmux_socket` / `tmux_socket_source`
/// fields remain populated byte-equivalent to today's shape.
///
/// The tmux-specific fields are NOT written for ConPTY (CR C-4: no
/// tmux-only fields under a conpty state; keeps compact status
/// stable). ConPTY discovery fields (`pipe_name`, `shim_pid`) are
/// added in Batch 3 when the shim boot path lands; this function just
/// pins the top-level `transport.kind`/`source`.
pub fn annotate_runtime_transport(
    state: &mut serde_json::Value,
    transport: &dyn Transport,
    workspace: &Path,
    source: Option<&str>,
) {
    use crate::transport::BackendKind;
    let kind_wire = match transport.kind() {
        BackendKind::Tmux => "tmux",
        BackendKind::WezTerm => "wezterm",
        BackendKind::ConPty => "conpty",
    };
    if let Some(obj) = state.as_object_mut() {
        let transport_block = serde_json::json!({
            "kind": kind_wire,
            "source": source.unwrap_or("unknown"),
        });
        obj.insert("transport".to_string(), transport_block);
    }
    // Tmux: preserve the existing tmux endpoint annotation shape.
    if matches!(transport.kind(), BackendKind::Tmux) {
        annotate_runtime_tmux_endpoint(state, transport, workspace);
    }
}

pub(crate) fn annotate_runtime_tmux_endpoint(
    state: &mut serde_json::Value,
    transport: &dyn Transport,
    workspace: &Path,
) {
    let Some(endpoint) = transport.tmux_endpoint() else {
        return;
    };
    let endpoint_for_state = if Path::new(&endpoint).is_absolute() || endpoint == "default" {
        endpoint.clone()
    } else if endpoint == crate::tmux_backend::socket_name_for_workspace(workspace) {
        crate::tmux_backend::socket_path_for_workspace(workspace)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| endpoint.clone())
    } else {
        crate::tmux_backend::socket_path_for_name(&endpoint)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| endpoint.clone())
    };
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "tmux_endpoint".to_string(),
            serde_json::json!(endpoint_for_state),
        );
        obj.insert(
            "tmux_socket".to_string(),
            obj.get("tmux_endpoint")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        if let Some(source) = selected_tmux_socket_source(transport, workspace) {
            obj.insert("tmux_socket_source".to_string(), serde_json::json!(source));
        }
    }
}

pub(super) fn attach_commands_for_runtime_windows<'a>(
    endpoint: Option<&str>,
    workspace: &Path,
    session_name: &SessionName,
    window_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let windows = window_names.into_iter().collect::<Vec<_>>();
    let attach = if let Some(endpoint) = endpoint.filter(|endpoint| !endpoint.is_empty()) {
        if Path::new(endpoint).is_absolute() {
            windows
                .iter()
                .map(|window_name| {
                    format!(
                        "tmux -S {} attach -t {}:{}",
                        endpoint,
                        session_name.as_str(),
                        window_name
                    )
                })
                .collect::<Vec<_>>()
        } else {
            crate::tmux_backend::attach_commands_for_windows(
                workspace,
                session_name,
                windows.iter().copied(),
            )
        }
    } else {
        crate::tmux_backend::attach_commands_for_windows(
            workspace,
            session_name,
            windows.iter().copied(),
        )
    };
    attach
}

pub(super) fn started_attach_window_names(started: &[StartedAgent]) -> Vec<String> {
    let mut windows = started
        .iter()
        .map(|started| {
            started
                .layout_window
                .as_ref()
                .map(|window| window.as_str().to_string())
                .unwrap_or_else(|| started.agent_id.as_str().to_string())
        })
        .collect::<Vec<_>>();
    windows.sort();
    windows.dedup();
    windows
}

pub(crate) fn attach_window_names_for_state_agents<'a>(
    state: &serde_json::Value,
    agent_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let windows = agent_ids
        .into_iter()
        .map(|agent_id| {
            state
                .get("agents")
                .and_then(serde_json::Value::as_object)
                .and_then(|agents| agents.get(agent_id))
                .and_then(|agent| {
                    agent
                        .get("layout_window")
                        .or_else(|| agent.get("window"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|window| !window.is_empty())
                })
                .unwrap_or(agent_id)
                .to_string()
        })
        .collect::<Vec<_>>();
    attach_window_names_with_managed_leader(state, windows)
}

pub(super) fn quick_start_attach_window_names(state: &serde_json::Value) -> Vec<String> {
    let windows = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|(agent_id, agent)| {
                    agent
                        .get("window")
                        .and_then(serde_json::Value::as_str)
                        .filter(|window| !window.is_empty())
                        .map(str::to_string)
                        .or_else(|| Some(agent_id.clone()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    attach_window_names_with_managed_leader(state, windows)
}

pub(super) fn attach_window_names_with_managed_leader(
    state: &serde_json::Value,
    mut windows: Vec<String>,
) -> Vec<String> {
    if state_uses_managed_leader(state) {
        windows.push("leader".to_string());
    }
    windows.sort();
    windows.dedup();
    windows
}

pub(super) fn state_uses_managed_leader(state: &serde_json::Value) -> bool {
    crate::state::projection::state_is_managed_leader(state)
}
