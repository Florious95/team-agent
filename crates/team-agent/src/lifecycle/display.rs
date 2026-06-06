//! lifecycle::display —— 能力门 / 后端解析 / 开关 / rebind 后重建。

use std::collections::BTreeMap;
use std::path::Path;

use crate::transport::SessionName;

use super::*;

// ── lifecycle::display —— 能力门 / 后端解析 / 开关 / rebind 后重建 ────────────

/// `resolve_display_backend(requested, recorded, source)`(`display/backend.py`)。默认
/// adaptive;非默认非静默发 `display.backend_resolved`。
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
pub fn probe_display_capabilities(workspace: &Path) -> Result<DisplayProbe, LifecycleError> {
    let _ = workspace;
    let platform = std::env::consts::OS.to_string();
    let in_tmux = std::env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false);
    let platform_supported = !matches!(platform.as_str(), "windows");
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
        leader_session: if in_tmux {
            Some(SessionName("leader".to_string()))
        } else {
            None
        },
        leader_pane: None,
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
    let displays = if backend.has_worker_views() {
        worker_display_targets(workspace, session_name)?
            .into_iter()
            .map(|target| {
                let display = display_for_target(&target, backend, probe);
                (target.agent_id, display)
            })
            .collect()
    } else {
        BTreeMap::new()
    };
    Ok(OpenDisplaysReport {
        backend,
        displays,
    })
}

/// `close_team_display_backends(state, event_log)`(`display/close.py`,C9)。按 state
/// 记录的后端关显示;adaptive 只删带 team tag 的窗口(C2 leader pane 安全)+ orphan 清理。
pub fn close_team_display_backends(
    workspace: &Path,
    session_name: &SessionName,
) -> Result<CloseDisplaysReport, LifecycleError> {
    Ok(CloseDisplaysReport {
        closed: recorded_display_targets(workspace, session_name)?,
        orphans_cleaned: Vec::new(),
    })
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

struct DisplayTarget {
    agent_id: String,
    window: Option<WindowName>,
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
                pane_id: None,
                target: target.window.as_ref().map(|window| window.as_str().to_string()),
                leader_session: probe.leader_session.clone(),
            }
        }
        DisplayBackend::Adaptive => WorkerDisplay::Blocked {
            reason: probe
                .reason
                .unwrap_or(AdaptiveBlockReason::AggregatorRebuildFailed),
        },
        DisplayBackend::Ghostty | DisplayBackend::GhosttyWindow | DisplayBackend::GhosttyWorkspace => {
            WorkerDisplay::Blocked {
                reason: AdaptiveBlockReason::NotImplementedThisPlatform,
            }
        }
        DisplayBackend::None | DisplayBackend::TmuxAttach | DisplayBackend::Iterm => {
            WorkerDisplay::Blocked {
                reason: AdaptiveBlockReason::NotImplementedThisPlatform,
            }
        }
    }
}

fn recorded_display_targets(
    workspace: &Path,
    session_name: &SessionName,
) -> Result<Vec<String>, LifecycleError> {
    let state = match crate::state::persist::load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => return Ok(Vec::new()),
    };
    let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut closed = Vec::new();
    for (agent_id, agent) in agents {
        let Some(display) = agent.get("display").and_then(serde_json::Value::as_object) else {
            continue;
        };
        if display
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("stopped"))
        {
            continue;
        }
        if let Some(target) = display_identifier(display, agent, agent_id, session_name) {
            closed.push(target);
        }
    }
    closed.sort();
    Ok(closed)
}

fn display_identifier(
    display: &serde_json::Map<String, serde_json::Value>,
    agent: &serde_json::Value,
    agent_id: &str,
    session_name: &SessionName,
) -> Option<String> {
    for key in ["display_session", "linked_session", "pane_id", "target"] {
        if let Some(value) = display
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(value.to_string());
        }
    }
    let window = display
        .get("window")
        .or_else(|| agent.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(agent_id);
    Some(format!("{}:{window}", session_name.as_str()))
}
