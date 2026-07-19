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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayoutPlacement {
    pub agent_id: AgentId,
    pub layout_window: WindowName,
    pub layout_index: usize,
    pub pane_index: usize,
    pub starts_window: bool,
}

pub(crate) fn adaptive_layout_plan(
    agent_ids: &[AgentId],
    max_per_window: usize,
) -> Vec<LayoutPlacement> {
    let max_per_window = max_per_window.max(1);
    agent_ids
        .iter()
        .enumerate()
        .map(|(idx, agent_id)| {
            let layout_index = idx / max_per_window;
            let pane_index = idx % max_per_window;
            LayoutPlacement {
                agent_id: agent_id.clone(),
                layout_window: WindowName::new(format!("team-w{}", layout_index + 1)),
                layout_index,
                pane_index,
                starts_window: pane_index == 0,
            }
        })
        .collect()
}

pub(crate) const ADAPTIVE_LAYOUT_MAX_PER_WINDOW: usize = 3;

pub(crate) fn state_uses_adaptive_layout(state: &serde_json::Value) -> bool {
    state
        .get("display_backend")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|backend| backend == "adaptive")
        || state
            .get("runtime")
            .and_then(|runtime| runtime.get("display_backend"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|backend| backend == "adaptive")
        || state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| {
                agents.values().any(|agent| {
                    agent
                        .get("layout_window")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|window| !window.is_empty())
                })
            })
}

pub(crate) fn adaptive_placement_for_agent(
    state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    agent_id: &AgentId,
) -> Option<LayoutPlacement> {
    if !state_uses_adaptive_layout(state) {
        return None;
    }
    // E43 Fix A (0.3.24 bug#3, demo-director blocker): cross-check live panes
    // by both pane_id AND window_name. Real-machine state can carry a stale
    // `layout_window` residue (e.g. "team-w2") while the live pane for that
    // agent actually lives under a DIFFERENT window_name (e.g. "architect").
    // The prior pane_id-only check let the stale claim slip through, and the
    // resulting placement asked spawn_agent_window to split a phantom window.
    let live_targets = transport.list_targets().unwrap_or_default();
    let live_panes: BTreeSet<String> = live_targets
        .iter()
        .map(|pane| pane.pane_id.as_str().to_string())
        .collect();
    // Map pane_id → window_name for the window-name cross-check.
    let live_pane_window: BTreeMap<String, String> = live_targets
        .iter()
        .filter_map(|pane| {
            pane.window_name
                .as_ref()
                .map(|name| (pane.pane_id.as_str().to_string(), name.as_str().to_string()))
        })
        .collect();
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|window| window.as_str().to_string())
        .collect();
    let mut windows: BTreeMap<usize, (String, usize)> = BTreeMap::new();
    if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
        for (id, agent) in agents {
            if id == agent_id.as_str() {
                continue;
            }
            let Some(window) = agent
                .get("layout_window")
                .or_else(|| agent.get("window"))
                .and_then(serde_json::Value::as_str)
                .filter(|window| !window.is_empty())
            else {
                continue;
            };
            // E45 (0.3.24 bug#4): only canonical `team-w<N>[-suffix]` windows
            // count as adaptive layout windows. Per-agent windows (named
            // after agent_id like `developer`) are NOT layout windows even if
            // the state's `layout_window`/`layout_index` carries them — those
            // are residue from a prior launch or explicit per-agent topology.
            // Treating them as adaptive caused add-agent to split into the
            // developer worker's pane on macmini (split-window -t :developer
            // succeeded → 2 panes in developer window, no new window for
            // demo-director).
            if !is_adaptive_layout_window(window) {
                continue;
            }
            let pane_id = agent.get("pane_id").and_then(serde_json::Value::as_str);
            let pane_live = pane_id.is_some_and(|pane| live_panes.contains(pane));
            // E43 Fix A: window_name match — when the agent's pane is live,
            // its live window_name MUST equal the claimed `window`; otherwise
            // the claim is stale residue from a respawn/rename and must not
            // count toward the layout map.
            let pane_window_matches = pane_id
                .and_then(|pane| live_pane_window.get(pane))
                .is_some_and(|name| name == window);
            if pane_live && !pane_window_matches {
                continue;
            }
            if !pane_live && (!live_panes.is_empty() || !live_windows.contains(window)) {
                continue;
            }
            let layout_index = agent
                .get("layout_index")
                .and_then(serde_json::Value::as_u64)
                .map(|idx| idx as usize)
                .or_else(|| parse_team_layout_index(window))
                .unwrap_or(windows.len());
            let entry = windows
                .entry(layout_index)
                .or_insert_with(|| (window.to_string(), 0));
            entry.1 = entry.1.saturating_add(1);
        }
    }
    if let Some((&layout_index, (window, count))) = windows.iter().next_back() {
        if *count < ADAPTIVE_LAYOUT_MAX_PER_WINDOW {
            return Some(LayoutPlacement {
                agent_id: agent_id.clone(),
                layout_window: WindowName::new(window.clone()),
                layout_index,
                pane_index: *count,
                starts_window: false,
            });
        }
    }
    // E45 (0.3.24 bug#4): when the live session has NO real adaptive layout
    // window (the topology is effectively per-agent, even though state says
    // display_backend=adaptive), DO NOT synthesise a fresh `team-w<N>`
    // window — that would force the new agent into an adaptive-layout pane
    // shape the rest of the session does not actually use. Return None so
    // the caller (`start_agent_at_paths` → `spawn_agent_window`) falls back
    // to its non-placement path, which opens a new window named after the
    // agent_id (canonical per-agent pattern, matches the existing 7 workers).
    if windows.is_empty() {
        return None;
    }
    let next_index = windows.keys().next_back().map(|idx| idx + 1).unwrap_or(0);
    let base = format!("team-w{}", next_index + 1);
    Some(LayoutPlacement {
        agent_id: agent_id.clone(),
        layout_window: unique_layout_window_name(&base, &live_windows),
        layout_index: next_index,
        pane_index: 0,
        starts_window: true,
    })
}

pub(crate) fn adaptive_existing_placement_for_agent(
    state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    agent_id: &AgentId,
) -> Option<LayoutPlacement> {
    if !state_uses_adaptive_layout(state) {
        return None;
    }
    let agent = state.get("agents")?.get(agent_id.as_str())?;
    let window = agent
        .get("layout_window")
        .or_else(|| agent.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|window| !window.is_empty())?;
    // E45 (0.3.24 bug#4): existing-placement is meaningless for a per-agent
    // window name (e.g. `developer`). Return None and let the caller fall
    // back to its non-placement spawn path, which opens / reuses a window
    // named after the agent_id. Only canonical `team-w<N>` windows are
    // honored as existing adaptive placements.
    if !is_adaptive_layout_window(window) {
        return None;
    }
    let layout_index = agent
        .get("layout_index")
        .and_then(serde_json::Value::as_u64)
        .map(|idx| idx as usize)
        .or_else(|| parse_team_layout_index(window))
        .unwrap_or(0);
    let desired_pane_index = agent
        .get("pane_index")
        .and_then(serde_json::Value::as_u64)
        .map(|idx| idx as usize)
        .unwrap_or(0);
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|window| window.as_str().to_string())
        .collect();
    if !live_windows.contains(window) {
        // E43 Fix B (0.3.24 bug#3): the claimed `layout_window` is NOT in
        // live_windows — it's stale residue. The session is effectively
        // per-agent (live windows are named after agent_ids), so fall back
        // to a new window named after the agent_id itself instead of
        // synthesising a fresh phantom-named window the next spawn would
        // try (and fail) to split.
        return Some(LayoutPlacement {
            agent_id: agent_id.clone(),
            layout_window: WindowName::new(agent_id.as_str()),
            layout_index,
            pane_index: 0,
            starts_window: true,
        });
    }
    let existing_panes = transport
        .list_targets()
        .unwrap_or_default()
        .into_iter()
        .filter(|pane| {
            pane.window_name
                .as_ref()
                .is_some_and(|name| name.as_str() == window)
                && agent
                    .get("pane_id")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|agent_pane| agent_pane != pane.pane_id.as_str())
        })
        .count();
    Some(LayoutPlacement {
        agent_id: agent_id.clone(),
        layout_window: WindowName::new(window),
        layout_index,
        pane_index: existing_panes,
        starts_window: false,
    })
}

pub(super) fn parse_team_layout_index(window: &str) -> Option<usize> {
    window
        .strip_prefix("team-w")
        .and_then(|rest| rest.split('-').next())
        .and_then(|raw| raw.parse::<usize>().ok())
        .and_then(|idx| idx.checked_sub(1))
}

/// E45 (0.3.24 bug#4, demo-director second-layer drift): a window name is a
/// REAL adaptive layout window only when it matches the canonical
/// `team-w<N>[-suffix]` shape (i.e. `parse_team_layout_index` returns Some).
/// Per-agent window names like `developer` / `architect` / `demo-director`
/// are NOT adaptive layout windows even if state happens to carry them in
/// the agent's `layout_window` or `layout_index` field — they are residue
/// from a prior launch / explicit per-agent topology. Treating them as
/// adaptive caused the macmini repro: add-agent demo-director split into
/// the developer worker's window @453 instead of opening its own.
pub(super) fn is_adaptive_layout_window(window: &str) -> bool {
    parse_team_layout_index(window).is_some()
}

/// Crate-public wrapper for the defensive guard at
/// `restart/common.rs::spawn_agent_window`. Same semantics as the private
/// helper above; promoted to `pub(crate)` so the spawn-time defence-in-depth
/// layer can refuse to split into a per-agent window even if a stale
/// placement asks for it.
pub(crate) fn is_adaptive_layout_window_pub(window: &str) -> bool {
    is_adaptive_layout_window(window)
}

pub(super) fn unique_layout_window_name(base: &str, live_windows: &BTreeSet<String>) -> WindowName {
    if !live_windows.contains(base) {
        return WindowName::new(base);
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !live_windows.contains(&candidate) {
            return WindowName::new(candidate);
        }
    }
    unreachable!("unbounded suffix search always returns")
}
