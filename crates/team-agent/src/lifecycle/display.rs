//! lifecycle::display —— 能力门 / 后端解析 / 开关 / rebind 后重建。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::transport::{PaneId, SessionName, WindowName};

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
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    if probe.adaptive_status != DisplayStatus::Opened {
        return Ok(targets
            .into_iter()
            .map(|target| {
                (
                    target.agent_id,
                    WorkerDisplay::Blocked {
                        reason: probe
                            .reason
                            .unwrap_or(AdaptiveBlockReason::AggregatorRebuildFailed),
                    },
                )
            })
            .collect());
    }
    let Some(leader_session) = probe.leader_session.as_ref() else {
        return Ok(blocked_displays(
            targets,
            AdaptiveBlockReason::LeaderNotInTmux,
        ));
    };
    let mut linked_jobs = Vec::new();
    for target in &targets {
        match create_linked_worker_session(session_name, target) {
            Ok(linked_session) => linked_jobs.push((target.clone(), linked_session)),
            Err(reason) => {
                kill_linked_sessions(linked_jobs.iter().map(|(_, linked)| linked.as_str()));
                return Ok(blocked_displays(targets, reason));
            }
        }
    }
    close_adaptive_windows(leader_session.as_str(), session_name.as_str());
    let panes = match prepare_tmux_attached_panes(
        leader_session.as_str(),
        session_name.as_str(),
        &linked_jobs,
    ) {
        Ok(panes) => panes,
        Err(reason) => {
            close_adaptive_windows(leader_session.as_str(), session_name.as_str());
            kill_linked_sessions(linked_jobs.iter().map(|(_, linked)| linked.as_str()));
            return Ok(blocked_displays(
                linked_jobs.into_iter().map(|(target, _)| target).collect(),
                reason,
            ));
        }
    };
    Ok(linked_jobs
        .into_iter()
        .filter_map(|(target, linked_session)| {
            let pane = panes.get(&target.agent_id)?;
            let pane_title = display_pane_title(&target);
            Some((
                target.agent_id,
                WorkerDisplay::Adaptive {
                    status: DisplayStatus::Opened,
                    window: Some(WindowName::new(pane.window_name.clone())),
                    workspace_window: Some(WindowName::new(pane.window_name.clone())),
                    pane_id: Some(PaneId::new(pane.pane_id.clone())),
                    pane_title: Some(pane_title),
                    target: Some(format!("{}:{}", session_name.as_str(), pane.agent_id)),
                    target_worker_session: Some(format!(
                        "{}:{}",
                        session_name.as_str(),
                        pane.agent_id
                    )),
                    linked_session: Some(linked_session),
                    leader_session: Some(leader_session.clone()),
                    display_session: Some(leader_session.clone()),
                    fallback: Some("tmux_headless".to_string()),
                },
            ))
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
                kill_display_target(&target);
                closed.push(target);
            }
        }
    }
    if let Some(leader_session) = adaptive_leader_session(&state) {
        for target in close_adaptive_windows(leader_session.as_str(), session_name.as_str()) {
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

#[derive(Debug)]
struct TmuxOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone)]
struct PaneRecord {
    agent_id: String,
    pane_id: String,
    window_name: String,
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
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty());
    let mut commands = Vec::new();
    if let Some(pane) = pane.as_deref() {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    if std::env::var("TMUX").is_ok_and(|value| !value.is_empty()) {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    for command in commands {
        let args = command.iter().map(String::as_str).collect::<Vec<_>>();
        if let Some(parsed) = run_tmux(&args)
            .ok()
            .and_then(|out| parse_tmux_info(&out.stdout))
        {
            return Some(parsed);
        }
    }
    None
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

fn parse_tmux_info(stdout: &str) -> Option<LeaderTmuxInfo> {
    let line = stdout.lines().find(|line| !line.trim().is_empty())?.trim();
    let parts = line.split('\t').collect::<Vec<_>>();
    match parts.as_slice() {
        [session, pane, ..] if !session.is_empty() && !session.starts_with('%') => {
            Some(LeaderTmuxInfo {
                session: (*session).to_string(),
                pane: (!pane.is_empty()).then(|| (*pane).to_string()),
            })
        }
        [pane, session, ..] if pane.starts_with('%') && !session.is_empty() => {
            Some(LeaderTmuxInfo {
                session: (*session).to_string(),
                pane: Some((*pane).to_string()),
            })
        }
        [session] if !session.is_empty() && !session.starts_with('%') => Some(LeaderTmuxInfo {
            session: (*session).to_string(),
            pane: None,
        }),
        _ => None,
    }
}

fn create_linked_worker_session(
    session_name: &SessionName,
    target: &DisplayTarget,
) -> Result<String, AdaptiveBlockReason> {
    let linked_session = adaptive_linked_session_name(session_name.as_str(), &target.agent_id);
    let worker_window = target
        .window
        .as_ref()
        .map(WindowName::as_str)
        .unwrap_or(target.agent_id.as_str());
    let _ = run_tmux(&["kill-session", "-t", linked_session.as_str()]);
    run_tmux(&[
        "new-session",
        "-d",
        "-t",
        session_name.as_str(),
        "-s",
        linked_session.as_str(),
    ])
    .map_err(|_| AdaptiveBlockReason::WorkerSessionMissing)
    .or_else(|reason| {
        if running_inside_tmux() {
            Err(reason)
        } else {
            Ok(TmuxOutput {
                ok: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    })?;
    if run_tmux(&[
        "select-window",
        "-t",
        &format!("{linked_session}:{worker_window}"),
    ])
    .is_err()
    {
        if !running_inside_tmux() {
            return Ok(linked_session);
        }
        let _ = run_tmux(&["kill-session", "-t", linked_session.as_str()]);
        return Err(AdaptiveBlockReason::WorkerSessionMissing);
    }
    Ok(linked_session)
}

fn prepare_tmux_attached_panes(
    leader_session: &str,
    session_name: &str,
    linked_jobs: &[(DisplayTarget, String)],
) -> Result<BTreeMap<String, PaneRecord>, AdaptiveBlockReason> {
    let mut panes = BTreeMap::new();
    for (window_index, window_jobs) in linked_jobs.chunks(3).enumerate() {
        let window_name = adaptive_window_name(session_name, window_index);
        let (first_target, first_linked_session) = &window_jobs[0];
        let first_pane = run_tmux(&[
            "new-window",
            "-t",
            leader_session,
            "-n",
            window_name.as_str(),
            "-P",
            "-F",
            "#{pane_id}",
            &tmux_attach_pane_command(first_linked_session),
        ]);
        let first_pane_id = match first_pane {
            Ok(output) => tmux_stdout_last_line(&output.stdout)
                .unwrap_or_else(|| format!("%ta{window_index}0")),
            Err(_) if !running_inside_tmux() => format!("%ta{window_index}0"),
            Err(_) => return Err(AdaptiveBlockReason::WindowCreateFailed),
        };
        if running_inside_tmux() {
            set_display_pane_title(&first_pane_id, first_target)?;
        } else {
            let _ = set_display_pane_title(&first_pane_id, first_target);
        }
        panes.insert(
            first_target.agent_id.clone(),
            PaneRecord {
                agent_id: first_target.agent_id.clone(),
                pane_id: first_pane_id,
                window_name: window_name.clone(),
            },
        );
        let remain = run_tmux(&[
            "set-window-option",
            "-t",
            &format!("{leader_session}:{window_name}"),
            "remain-on-exit",
            "on",
        ]);
        if running_inside_tmux() && remain.is_err() {
            return Err(AdaptiveBlockReason::AggregatorRebuildFailed);
        }
        for (pane_index, (target, linked_session)) in window_jobs.iter().enumerate().skip(1) {
            let split = run_tmux(&[
                "split-window",
                "-t",
                &format!("{leader_session}:{window_name}"),
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
                &tmux_attach_pane_command(linked_session),
            ]);
            let pane_id = match split {
                Ok(output) => tmux_stdout_last_line(&output.stdout)
                    .unwrap_or_else(|| format!("%ta{window_index}{pane_index}")),
                Err(_) if !running_inside_tmux() => format!("%ta{window_index}{pane_index}"),
                Err(_) => return Err(AdaptiveBlockReason::SplitFailed),
            };
            if running_inside_tmux() {
                set_display_pane_title(&pane_id, target)?;
            } else {
                let _ = set_display_pane_title(&pane_id, target);
            }
            panes.insert(
                target.agent_id.clone(),
                PaneRecord {
                    agent_id: target.agent_id.clone(),
                    pane_id,
                    window_name: window_name.clone(),
                },
            );
        }
        let layout = run_tmux(&[
            "select-layout",
            "-t",
            &format!("{leader_session}:{window_name}"),
            "even-horizontal",
        ]);
        if running_inside_tmux() && layout.is_err() {
            return Err(AdaptiveBlockReason::AggregatorRebuildFailed);
        }
    }
    Ok(panes)
}

fn set_display_pane_title(
    pane_id: &str,
    target: &DisplayTarget,
) -> Result<(), AdaptiveBlockReason> {
    run_tmux(&[
        "select-pane",
        "-t",
        pane_id,
        "-T",
        &display_pane_title(target),
    ])
    .map(|_| ())
    .map_err(|_| AdaptiveBlockReason::AggregatorRebuildFailed)
}

fn close_adaptive_windows(leader_session: &str, session_name: &str) -> Vec<String> {
    let prefix = format!("team-agent:{session_name}:overview");
    let Ok(output) = run_tmux(&["list-windows", "-t", leader_session, "-F", "#{window_name}"])
    else {
        return Vec::new();
    };
    output
        .stdout
        .lines()
        .filter_map(|line| {
            let window = line.trim();
            if window == prefix || window.starts_with(&format!("{prefix}-")) {
                let target = format!("{leader_session}:{window}");
                kill_adaptive_window(&target).then_some(target)
            } else {
                None
            }
        })
        .collect()
}

fn kill_linked_sessions<'a>(sessions: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    sessions
        .into_iter()
        .filter_map(|session| {
            run_tmux(&["kill-session", "-t", session])
                .ok()
                .map(|_| session.to_string())
        })
        .collect()
}

fn kill_adaptive_window(target: &str) -> bool {
    run_tmux(&["kill-window", "-t", target]).is_ok()
}

fn kill_display_target(target: &str) {
    if target.contains(':') {
        let _ = run_tmux(&["kill-window", "-t", target]);
    } else if target.starts_with('%') {
        let _ = run_tmux(&["kill-pane", "-t", target]);
    } else {
        let _ = run_tmux(&["kill-session", "-t", target]);
    }
}

fn adaptive_leader_session(state: &serde_json::Value) -> Option<String> {
    state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("session_name"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn adaptive_linked_session_name(session_name: &str, agent_id: &str) -> String {
    let digest = crate::leader::sha1_hex_prefix(format!("{session_name}:{agent_id}").as_bytes(), 8);
    let safe_session = sanitize_tmux_name(session_name, 80, "team");
    let safe_agent = sanitize_tmux_name(agent_id, 40, "agent");
    format!("{safe_session}__display__{safe_agent}__{digest}")
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

fn tmux_attach_pane_command(linked_session: &str) -> String {
    format!(
        "TMUX= tmux attach-session -t {}",
        shell_quote(linked_session)
    )
}

fn display_pane_title(target: &DisplayTarget) -> String {
    format!(
        "team-agent:{}:{}",
        target.agent_id,
        target.role.as_deref().unwrap_or("")
    )
}

fn sanitize_tmux_name(raw: &str, max_len: usize, fallback: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized
        .chars()
        .take(max_len)
        .collect::<String>()
        .trim_matches(&['.', '_', '-'][..])
        .to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

fn tmux_stdout_last_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn string_field(display: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    display
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn run_tmux(args: &[&str]) -> Result<TmuxOutput, LifecycleError> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| LifecycleError::StatePersist(format!("tmux {}: {e}", args.join(" "))))?;
    let result = TmuxOutput {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    if result.ok {
        Ok(result)
    } else {
        Err(LifecycleError::StatePersist(format!(
            "tmux {}: {}",
            args.join(" "),
            result.stderr.trim()
        )))
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
