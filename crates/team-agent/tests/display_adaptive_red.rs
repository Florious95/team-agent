#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::lifecycle::{
    close_team_display_backends, open_worker_displays, probe_display_capabilities,
    AdaptiveBlockReason, CapsFlags, DisplayBackend, DisplayProbe, DisplayStatus, WorkerDisplay,
};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{PaneId, SessionName};

#[test]
fn adaptive_open_creates_real_tmux_pane_records_instead_of_placeholder_state() {
    let fixture = DisplayFixture::new("open-records");
    fixture.seed_running_workers(["alpha", "bravo"]);
    let probe = DisplayProbe {
        in_tmux: true,
        platform: std::env::consts::OS.to_string(),
        leader_session: Some(SessionName::new("leader-live")),
        leader_pane: Some(PaneId::new("%leader")),
        caps: CapsFlags {
            tmux_append_windows: true,
            adaptive_display: true,
        },
        adaptive_status: DisplayStatus::Opened,
        reason: None,
    };

    let report = open_worker_displays(
        &fixture.workspace,
        &SessionName::new("team-adaptive"),
        DisplayBackend::Adaptive,
        &probe,
    )
    .expect("adaptive display opening should return a typed report");

    assert_eq!(report.backend, DisplayBackend::Adaptive);
    assert_eq!(report.displays.len(), 2, "fixture has two worker panes; report={report:?}");
    for (agent_id, display) in &report.displays {
        let value = serde_json::to_value(display).unwrap();
        let pane_id = value.get("pane_id").and_then(Value::as_str);
        assert!(
            pane_id.is_some_and(|pane| pane.starts_with('%')),
            "adaptive display must create a real tmux overview/split pane and persist pane_id; agent={agent_id} display={value}"
        );
        for field in ["leader_session", "display_session", "linked_session", "pane_title", "target_worker_session"] {
            assert!(
                value.get(field).and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
                "adaptive display metadata must include cleanup/status field `{field}`; agent={agent_id} display={value}"
            );
        }
        assert_eq!(
            value.get("fallback").and_then(Value::as_str),
            Some("tmux_headless"),
            "adaptive display must record the tmux fallback mode used for cleanup/status; agent={agent_id} display={value}"
        );
    }
}

#[test]
fn adaptive_close_uses_recorded_display_metadata_to_close_windows_and_linked_sessions() {
    let fixture = DisplayFixture::new("close-records");
    fixture.seed_state_with_open_adaptive_display();

    let report = close_team_display_backends(&fixture.workspace, &SessionName::new("team-adaptive"))
        .expect("adaptive close should return concrete cleanup targets");

    for expected in [
        "leader-live:team-agent:team-adaptive:overview",
        "team-agent:team-adaptive:alpha:linked",
        "%11",
    ] {
        assert!(
            report.closed.iter().any(|closed| closed == expected),
            "adaptive close must include every recorded cleanup target `{expected}`, not only the first identifier per agent; report={report:?}"
        );
    }
    assert!(
        report.orphans_cleaned.iter().any(|closed| closed.contains("team-agent:team-adaptive:overview")),
        "adaptive close must scan and clean stale tagged overview windows/sessions; report={report:?}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux workspace socket and tagged adaptive overview cleanup"]
#[file_serial(tmux)]
fn real_tmux_adaptive_display_creates_tiled_overview_and_close_removes_only_tagged_windows() {
    let fixture = DisplayFixture::new("real-tmux");
    let team_dir = fixture.write_fake_team(["alpha", "bravo"]);
    let socket = format!("ta-display-{}-{}", std::process::id(), fixture.id);
    let leader_session = format!("leader-display-{}", fixture.id);
    let team_session = "team-adaptive";
    let cleanup = RealTmuxCleanup { socket: socket.clone(), sessions: vec![leader_session.clone(), team_session.to_string()] };

    run_tmux(&socket, ["new-session", "-d", "-s", &leader_session, "-n", "leader"]).unwrap();
    run_tmux(&socket, ["new-session", "-d", "-s", team_session, "-n", "alpha"]).unwrap();
    run_tmux(&socket, ["new-window", "-t", team_session, "-n", "bravo", "sleep 60"]).unwrap();
    let socket_path = run_tmux(&socket, ["display-message", "-p", "-t", &leader_session, "-F", "#{socket_path}"])
        .unwrap()
        .trim()
        .to_string();
    let leader_pane = run_tmux(&socket, ["list-panes", "-t", &leader_session, "-F", "#{pane_id}"])
        .unwrap()
        .lines()
        .next()
        .unwrap_or("%0")
        .to_string();
    let tmux_env = format!("{socket_path},{},0", std::process::id());
    let _env = EnvGuard::set_many([
        ("TMUX", tmux_env.as_str()),
        ("TMUX_PANE", leader_pane.as_str()),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "quick-start",
            team_dir.to_str().unwrap(),
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--team-id",
            "adaptive",
            "--name",
            "adaptive",
            "--fresh",
            "--yes",
            "--json",
        ])
        .env("TMUX", &tmux_env)
        .env("TMUX_PANE", &leader_pane)
        .output()
        .expect("run team-agent quick-start adaptive display");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "quick-start should run before checking real tmux display; output={combined}");

    let probe = probe_display_capabilities(&fixture.workspace)
        .expect("real tmux fixture should produce a typed adaptive display probe");
    assert_eq!(
        probe.leader_session.as_ref().map(SessionName::as_str),
        Some(leader_session.as_str()),
        "fixture must pass the real leader tmux session to the display probe; probe={probe:?} output={combined}"
    );
    let opened = open_worker_displays(
        &fixture.workspace,
        &SessionName::new(team_session),
        DisplayBackend::Adaptive,
        &probe,
    )
    .expect("real adaptive display open should return a typed report");
    assert_real_display_records(&opened.displays);
    fixture.persist_display_records(&opened.displays);

    let windows = run_tmux(&socket, ["list-windows", "-t", &leader_session, "-F", "#{window_name}:#{window_panes}"])
        .unwrap();
    assert!(
        windows.contains("team-agent:team-adaptive:overview") && windows.contains(":2"),
        "adaptive display must create a tiled overview window in the leader tmux socket; windows={windows} output={combined}"
    );
    let closed = close_team_display_backends(&fixture.workspace, &SessionName::new(team_session))
        .expect("real adaptive display close should return cleanup evidence");
    assert!(
        closed.closed.iter().any(|target| target.contains("team-agent:team-adaptive:overview")),
        "adaptive close must clean the tagged overview window; closed={closed:?}"
    );
    let after_close = run_tmux(&socket, ["list-windows", "-t", &leader_session, "-F", "#{window_name}"])
        .unwrap();
    assert!(
        !after_close.contains("team-agent:team-adaptive:overview"),
        "adaptive close must remove tagged overview windows while leaving leader session alive; after_close={after_close}"
    );
    drop(cleanup);
}

struct DisplayFixture {
    id: u64,
    workspace: PathBuf,
}

impl DisplayFixture {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let workspace = std::env::temp_dir().join(format!(
            "ta-rs-display-adaptive-{tag}-{}-{id}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        Self { id, workspace: std::fs::canonicalize(workspace).unwrap() }
    }

    fn seed_running_workers<const N: usize>(&self, workers: [&str; N]) {
        let agents = workers
            .into_iter()
            .map(|agent| {
                (
                    agent.to_string(),
                    json!({
                        "status": "running",
                        "provider": "fake",
                        "window": agent,
                        "pane_id": format!("%{agent}"),
                        "owner_team_id": "adaptive",
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "adaptive",
                "session_name": "team-adaptive",
                "agents": agents,
            }),
        )
        .unwrap();
    }

    fn seed_state_with_open_adaptive_display(&self) {
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "adaptive",
                "session_name": "team-adaptive",
                "agents": {
                    "alpha": {
                        "status": "running",
                        "provider": "fake",
                        "window": "alpha",
                        "display": {
                            "backend": "adaptive",
                            "status": "opened",
                            "leader_session": "leader-live",
                            "display_session": "leader-live:team-agent:team-adaptive:overview",
                            "linked_session": "team-agent:team-adaptive:alpha:linked",
                            "pane_id": "%11",
                            "target": "team-adaptive:alpha",
                            "pane_title": "team-agent:alpha:Worker"
                        }
                    }
                }
            }),
        )
        .unwrap();
    }

    fn persist_display_records(&self, displays: &std::collections::BTreeMap<String, WorkerDisplay>) {
        let mut state = load_runtime_state(&self.workspace).unwrap();
        let state_snapshot = state.to_string();
        let agents = state
            .get_mut("agents")
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("quick-start fixture must persist agents before display open; state={state_snapshot}"));
        for (agent_id, display) in displays {
            let agent = agents
                .get_mut(agent_id)
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("display report referenced unknown agent {agent_id}; state={state_snapshot}"));
            agent.insert("display".to_string(), serde_json::to_value(display).unwrap());
        }
        save_runtime_state(&self.workspace, &state).unwrap();
    }

    fn write_fake_team<const N: usize>(&self, workers: [&str; N]) -> PathBuf {
        let team = self.workspace.join("adaptive-team");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: adaptive\nobjective: Adaptive display real tmux fixture.\nprovider: fake\ndisplay_backend: adaptive\n---\n\nTeam.\n",
        )
        .unwrap();
        for worker in workers {
            std::fs::write(
                team.join("agents").join(format!("{worker}.md")),
                format!(
                    "---\nname: {worker}\nrole: Adaptive fake worker {worker}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
                ),
            )
            .unwrap();
        }
        team
    }
}

impl Drop for DisplayFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct RealTmuxCleanup {
    socket: String,
    sessions: Vec<String>,
}

impl Drop for RealTmuxCleanup {
    fn drop(&mut self) {
        for session in &self.sessions {
            let _ = run_tmux(&self.socket, ["kill-session", "-t", session]);
        }
    }
}

struct EnvGuard {
    old: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set_many<const N: usize>(pairs: [(&'static str, &str); N]) -> Self {
        let old = pairs
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in pairs {
            std::env::set_var(key, value);
        }
        Self { old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, old) in self.old.drain(..) {
            match old {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn run_tmux<const N: usize>(socket: &str, args: [&str; N]) -> std::io::Result<String> {
    let output = Command::new("tmux").arg("-L").arg(socket).args(args).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn assert_real_display_records(displays: &std::collections::BTreeMap<String, WorkerDisplay>) {
    assert_eq!(displays.len(), 2, "real fixture has two fake workers; displays={displays:?}");
    for (agent_id, display) in displays {
        let value = serde_json::to_value(display).unwrap();
        assert_eq!(value.get("backend").and_then(Value::as_str), Some("adaptive"), "agent={agent_id} display={value}");
        assert_eq!(value.get("status").and_then(Value::as_str), Some("opened"), "agent={agent_id} display={value}");
        assert!(
            value.get("pane_id").and_then(Value::as_str).is_some_and(|pane| pane.starts_with('%')),
            "real adaptive display must record concrete pane_id; agent={agent_id} display={value}"
        );
        for field in ["leader_session", "display_session", "linked_session", "pane_title", "target_worker_session"] {
            assert!(
                value.get(field).and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
                "real adaptive display must persist `{field}` for cleanup/status; agent={agent_id} display={value}"
            );
        }
    }
}

#[allow(dead_code)]
fn assert_not_ghostty(display: &WorkerDisplay) {
    assert!(
        !matches!(display, WorkerDisplay::GhosttyWindow { .. } | WorkerDisplay::GhosttyWorkspace { .. }),
        "Ghostty backends are explicitly out of scope for this adaptive-only contract"
    );
    assert!(!matches!(display, WorkerDisplay::Blocked { reason: AdaptiveBlockReason::NotImplementedThisPlatform }));
}
