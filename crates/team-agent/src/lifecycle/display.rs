//! lifecycle::display —— 能力门 / 后端解析 / 开关 / rebind 后重建。
//!
//! 0.5.39 Slice 1 (tmux-server-death-locate §7 Slice 1): all tmux
//! session/window/pane operations here MUST go through the workspace's
//! scoped `TmuxBackend` — raw `Command::new("tmux")` / ambient
//! `run_tmux(...)` helpers in this file inherit ambient `$TMUX` and can
//! kill sessions on the wrong socket. Ambient leader-pane env probes
//! (`display-message` reading $TMUX to discover which session invoked
//! us) live in `tmux_backend::probe_ambient_leader_pane_info` — that
//! probe is definitionally ambient (its whole job is "which session am I
//! in") and stays outside this module.

use std::collections::BTreeMap;
use std::path::Path;

use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use super::*;

// ── lifecycle::display —— 能力门 / 后端解析 / 开关 / rebind 后重建 ────────────

/// `resolve_display_backend(requested, recorded, source)`(`display/backend.py`)。默认
/// adaptive;显式 none 是逃生口。
pub fn resolve_display_backend(
    requested: Option<DisplayBackend>,
    recorded: Option<DisplayBackend>,
) -> ResolvedBackend {
    let backend = requested.or(recorded).unwrap_or(DisplayBackend::Adaptive);
    ResolvedBackend {
        backend,
        non_default: backend != DisplayBackend::Adaptive,
    }
}

/// `probe_display_capabilities(...)`(`display/adaptive.py:31`,C13)。能力探测,分支只看
/// 结果不看 `cfg!(target_os)`;Windows/WSL → `NotImplementedThisPlatform`。
pub fn probe_display_capabilities(_workspace: &Path) -> Result<DisplayProbe, LifecycleError> {
    let platform = std::env::consts::OS.to_string();
    let platform_supported = !matches!(platform.as_str(), "windows") && !in_wsl();
    let live_tmux_info = current_leader_tmux_info();
    let in_tmux = platform_supported && (running_inside_tmux() || live_tmux_info.is_some());
    let tmux_info = live_tmux_info.or_else(env_leader_tmux_info);
    let opened = in_tmux && platform_supported;
    let reason = if opened {
        None
    } else if platform_supported {
        Some(AdaptiveBlockReason::LeaderNotInTmux)
    } else {
        Some(AdaptiveBlockReason::NotImplementedThisPlatform)
    };
    Ok(DisplayProbe {
        in_tmux,
        platform,
        leader_session: tmux_info
            .as_ref()
            .map(|info| SessionName::new(info.session.clone())),
        leader_pane: tmux_info.and_then(|info| info.pane.map(PaneId::new)),
        caps: CapsFlags {
            tmux_append_windows: opened,
            adaptive_display: opened,
        },
        adaptive_status: if opened {
            DisplayStatus::Opened
        } else {
            DisplayStatus::Blocked
        },
        reason,
    })
}

/// `open_worker_displays(workspace, session_name, jobs, backend, capability_probe)`
/// (`display/worker_window.py`)。按后端分派 adaptive / ghostty_workspace / ghostty_window;
/// 显示失败**不阻塞** team readiness(C14)。
pub fn open_worker_displays(
    workspace: &Path,
    session_name: &SessionName,
    backend: DisplayBackend,
    probe: &DisplayProbe,
) -> Result<OpenDisplaysReport, LifecycleError> {
    let displays = match backend {
        DisplayBackend::Adaptive => open_adaptive_worker_displays(workspace, session_name, probe)?,
        backend if backend.has_worker_views() => worker_display_targets(workspace, session_name)?
            .into_iter()
            .map(|target| {
                let display = display_for_target(&target, backend, probe);
                (target.agent_id, display)
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    Ok(OpenDisplaysReport { backend, displays })
}

/// `close_team_display_backends(state, event_log)`(`display/close.py`,C9)。按 state
/// 记录的后端关显示;adaptive 只删带 team tag 的窗口(C2 leader pane 安全)+ orphan 清理。
pub fn close_team_display_backends(
    workspace: &Path,
    session_name: &SessionName,
) -> Result<CloseDisplaysReport, LifecycleError> {
    close_adaptive_displays(workspace, session_name)
}

/// `rebuild_adaptive_display_after_rebind(...)`(`display/rebuild.py`,C12)。restart 在
/// leader rebind **之后**重建 adaptive 窗口(读 `leader_receiver.rebind_applied` 事件)。
pub fn rebuild_adaptive_display_after_rebind(
    workspace: &Path,
    session_name: &SessionName,
    probe: &DisplayProbe,
) -> Result<OpenDisplaysReport, LifecycleError> {
    open_worker_displays(workspace, session_name, DisplayBackend::Adaptive, probe)
}

#[derive(Debug, Clone)]
struct DisplayTarget {
    agent_id: String,
    window: Option<WindowName>,
    pane_id: Option<PaneId>,
    role: Option<String>,
}

fn worker_display_targets(
    workspace: &Path,
    session_name: &SessionName,
) -> Result<Vec<DisplayTarget>, LifecycleError> {
    let state = match crate::state::persist::load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => return Ok(Vec::new()),
    };
    let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut targets = Vec::new();
    for (agent_id, agent) in agents {
        let window = agent
            .get("window")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_id);
        targets.push(DisplayTarget {
            agent_id: agent_id.clone(),
            window: Some(WindowName::new(window)),
            pane_id: agent
                .get("pane_id")
                .and_then(serde_json::Value::as_str)
                .filter(|pane| !pane.is_empty())
                .map(PaneId::new),
            role: agent
                .get("role")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    let _ = session_name;
    targets.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    Ok(targets)
}

fn display_for_target(
    target: &DisplayTarget,
    backend: DisplayBackend,
    probe: &DisplayProbe,
) -> WorkerDisplay {
    match backend {
        DisplayBackend::Adaptive if probe.adaptive_status == DisplayStatus::Opened => {
            WorkerDisplay::Adaptive {
                status: DisplayStatus::Opened,
                window: target.window.clone(),
                workspace_window: target.window.clone(),
                pane_id: None,
                pane_title: target.window.as_ref().map(|_| display_pane_title(target)),
                target: target
                    .window
                    .as_ref()
                    .map(|window| window.as_str().to_string()),
                target_worker_session: target
                    .window
                    .as_ref()
                    .map(|window| window.as_str().to_string()),
                linked_session: None,
                leader_session: probe.leader_session.clone(),
                display_session: probe.leader_session.clone(),
                fallback: Some("tmux_headless".to_string()),
            }
        }
        DisplayBackend::Adaptive => WorkerDisplay::Blocked {
            reason: probe
                .reason
                .unwrap_or(AdaptiveBlockReason::AggregatorRebuildFailed),
        },
        DisplayBackend::Ghostty
        | DisplayBackend::GhosttyWindow
        | DisplayBackend::GhosttyWorkspace => WorkerDisplay::Blocked {
            reason: AdaptiveBlockReason::NotImplementedThisPlatform,
        },
        DisplayBackend::None | DisplayBackend::TmuxAttach | DisplayBackend::Iterm => {
            WorkerDisplay::Blocked {
                reason: AdaptiveBlockReason::NotImplementedThisPlatform,
            }
        }
    }
}

fn open_adaptive_worker_displays(
    workspace: &Path,
    session_name: &SessionName,
    probe: &DisplayProbe,
) -> Result<BTreeMap<String, WorkerDisplay>, LifecycleError> {
    let targets = worker_display_targets(workspace, session_name)?;
    Ok(targets
        .into_iter()
        .map(|target| {
            let agent_id = target.agent_id.clone();
            (
                agent_id.clone(),
                WorkerDisplay::Adaptive {
                    status: probe.adaptive_status,
                    window: target.window.clone(),
                    workspace_window: target.window.clone(),
                    pane_id: target.pane_id.clone(),
                    pane_title: Some(agent_id),
                    target: target.pane_id.as_ref().map(|pane| pane.as_str().to_string()),
                    target_worker_session: Some(session_name.as_str().to_string()),
                    linked_session: None,
                    leader_session: Some(session_name.clone()),
                    display_session: Some(session_name.clone()),
                    fallback: None,
                },
            )
        })
        .collect())
}

fn close_adaptive_displays(
    workspace: &Path,
    session_name: &SessionName,
) -> Result<CloseDisplaysReport, LifecycleError> {
    let state = match crate::state::persist::load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => {
            return Ok(CloseDisplaysReport {
                closed: Vec::new(),
                orphans_cleaned: Vec::new(),
            })
        }
    };
    let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) else {
        return Ok(CloseDisplaysReport {
            closed: Vec::new(),
            orphans_cleaned: Vec::new(),
        });
    };
    // 0.5.39 Slice 1: build workspace-scoped tmux transport once and
    // route every destructive tmux op below through it (no ambient
    // $TMUX). The kill_* Transport methods carry the workspace socket
    // (`tmux -L <socket>`) so display cleanup cannot cross-kill a session
    // on the user's default tmux server.
    let transport = crate::transport_factory::tmux_workspace_transport(workspace);
    let mut closed = Vec::new();
    let mut orphans_cleaned = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (agent_id, agent) in agents {
        let Some(display) = agent.get("display").and_then(serde_json::Value::as_object) else {
            continue;
        };
        if display.get("backend").and_then(serde_json::Value::as_str) != Some("adaptive") {
            continue;
        }
        if display
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("stopped"))
        {
            continue;
        }
        for target in display_identifiers(display, agent, agent_id, session_name, &state) {
            if seen.insert(target.clone()) {
                kill_display_target(&transport, &target);
                closed.push(target);
            }
        }
    }
    if let Some(leader_session) = adaptive_leader_session(&state) {
        for target in close_adaptive_windows(&transport, leader_session.as_str(), session_name.as_str()) {
            orphans_cleaned.push(target.clone());
            if seen.insert(target.clone()) {
                closed.push(target);
            }
        }
    }
    closed.sort();
    if orphans_cleaned.is_empty() {
        let overview_prefix = format!(":team-agent:{}:overview", session_name.as_str());
        orphans_cleaned.extend(
            closed
                .iter()
                .filter(|target| target.contains(&overview_prefix))
                .cloned(),
        );
    }
    orphans_cleaned.sort();
    orphans_cleaned.dedup();
    Ok(CloseDisplaysReport {
        closed,
        orphans_cleaned,
    })
}

fn display_identifiers(
    display: &serde_json::Map<String, serde_json::Value>,
    agent: &serde_json::Value,
    agent_id: &str,
    session_name: &SessionName,
    state: &serde_json::Value,
) -> Vec<String> {
    let mut targets = Vec::new();
    let leader_session = display
        .get("leader_session")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| adaptive_leader_session(state));
    let window = display
        .get("workspace_window")
        .or_else(|| display.get("window"))
        .or_else(|| agent.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    if let (Some(leader_session), Some(window)) = (leader_session, window) {
        if adaptive_window_is_tagged(session_name.as_str(), window) {
            targets.push(format!("{leader_session}:{window}"));
        } else {
            targets.push(format!(
                "{leader_session}:{}",
                adaptive_window_name(session_name.as_str(), 0)
            ));
        }
    }
    for key in ["linked_session", "pane_id"] {
        if let Some(value) = string_field(display, key) {
            targets.push(value);
        }
    }
    if targets.is_empty() {
        let window = agent
            .get("window")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(agent_id);
        targets.push(format!("{}:{window}", session_name.as_str()));
    }
    targets
}

fn blocked_displays(
    targets: Vec<DisplayTarget>,
    reason: AdaptiveBlockReason,
) -> BTreeMap<String, WorkerDisplay> {
    targets
        .into_iter()
        .map(|target| (target.agent_id, WorkerDisplay::Blocked { reason }))
        .collect()
}

#[derive(Debug, Clone)]
struct LeaderTmuxInfo {
    session: String,
    pane: Option<String>,
}

fn in_wsl() -> bool {
    std::env::var("WSL_DISTRO_NAME").is_ok_and(|value| !value.is_empty())
        || std::env::var("WSL_INTEROP").is_ok_and(|value| !value.is_empty())
}

fn running_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok_and(|value| !value.is_empty())
        || std::env::var("TMUX_PANE").is_ok_and(|value| !value.is_empty())
}

fn current_leader_tmux_info() -> Option<LeaderTmuxInfo> {
    // 0.5.39 Slice 1: ambient probe lives in `tmux_backend` — the single
    // controlled exception to the "all tmux ops through socket-scoped
    // transport" rule (its whole job is "which session am I in?").
    crate::tmux_backend::probe_ambient_leader_pane_info()
        .map(|(session, pane)| LeaderTmuxInfo { session, pane })
}

fn env_leader_tmux_info() -> Option<LeaderTmuxInfo> {
    let session = std::env::var("TEAM_AGENT_LEADER_SESSION_NAME")
        .ok()
        .filter(|value| !value.is_empty())?;
    let pane = std::env::var("TEAM_AGENT_LEADER_PANE_ID")
        .ok()
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|value| !value.is_empty());
    Some(LeaderTmuxInfo { session, pane })
}

fn close_adaptive_windows(
    transport: &dyn Transport,
    leader_session: &str,
    session_name: &str,
) -> Vec<String> {
    let prefix = format!("team-agent:{session_name}:overview");
    // 0.5.39 Slice 1: `Transport::list_windows` uses the workspace socket
    // — if `leader_session` doesn't exist on that socket, list_windows
    // returns Err/empty and we bail without touching another server.
    let Ok(windows) = transport.list_windows(&SessionName::new(leader_session.to_string())) else {
        return Vec::new();
    };
    windows
        .into_iter()
        .filter_map(|window| {
            let window = window.as_str().to_string();
            if window == prefix || window.starts_with(&format!("{prefix}-")) {
                let target_string = format!("{leader_session}:{window}");
                let target = Target::SessionWindow {
                    session: SessionName::new(leader_session.to_string()),
                    window: WindowName::new(window),
                };
                transport.kill_window(&target).ok().map(|_| target_string)
            } else {
                None
            }
        })
        .collect()
}

fn kill_display_target(transport: &dyn Transport, target: &str) {
    // 0.5.39 Slice 1: only kill window/pane — never fall back to
    // kill-session on an ambiguous string shape. Locate §7 Slice 1:
    // "Remove the string-shape `kill-session` fallback unless target is a
    // proven display-only session." The old code parsed any bare
    // non-`%`/non-`:` target as a session and issued `kill-session`,
    // which is exactly the unbounded blast radius we're closing.
    if let Some((session, window)) = target.split_once(':') {
        let _ = transport.kill_window(&Target::SessionWindow {
            session: SessionName::new(session.to_string()),
            window: WindowName::new(window.to_string()),
        });
    } else if target.starts_with('%') {
        let _ = transport.kill_pane(&PaneId::new(target.to_string()));
    }
    // Bare session-name form is intentionally a no-op: display cleanup
    // must not kill a whole session on shape guesswork.
}

fn adaptive_leader_session(state: &serde_json::Value) -> Option<String> {
    state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("session_name"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn adaptive_window_name(session_name: &str, index: usize) -> String {
    if index == 0 {
        format!("team-agent:{session_name}:overview")
    } else {
        format!("team-agent:{session_name}:overview-{}", index + 1)
    }
}

fn adaptive_window_is_tagged(session_name: &str, window: &str) -> bool {
    let prefix = format!("team-agent:{session_name}:overview");
    window == prefix || window.starts_with(&format!("{prefix}-"))
}

fn display_pane_title(target: &DisplayTarget) -> String {
    format!(
        "team-agent:{}:{}",
        target.agent_id,
        target.role.as_deref().unwrap_or("")
    )
}

fn string_field(display: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    display
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

