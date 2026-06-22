//! E12 (P0) · `sessions_to_kill` 纯决策单测(kill 决策下沉)。
//!
//! spare = state 锚 session(anchor_sessions) ∪ `team-agent-leader-` 命名前缀(并集,锚优先)。
//! 独享 socket(无 spare)才允许整 server 拆;共享/leader 在 → 逐 session kill。
//! 集成面由 tests/b5_leader_terminal_kill_red.rs 的真 tmux 契约覆盖,此处锁纯决策 + 4 反向 case。

use crate::cli::lifecycle_port::{sessions_to_kill, KillDecision};
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
    PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome, SpawnResult, Target, Transport,
    TransportError, WindowName,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn names(raw: &[&str]) -> Vec<SessionName> {
    raw.iter().map(|name| SessionName::new(*name)).collect()
}

fn anchors(raw: &[&str]) -> BTreeSet<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

// RC-4(独享 socket):仅目标 session(无 spare)→ 整 server 拆。
#[test]
fn rc4_exclusive_socket_kills_server() {
    assert_eq!(
        sessions_to_kill(&names(&["team-x", "team-y"]), &BTreeSet::new()),
        KillDecision::KillServerExclusive
    );
    // 空 session 集 → 逐 kill(no-op),不整 server 拆(没东西可拆)。
    assert_eq!(
        sessions_to_kill(&[], &BTreeSet::new()),
        KillDecision::KillIndividually {
            to_kill: vec![],
            spared: vec![]
        }
    );
}

// RC-1(本 P0 复现):in_tmux leader 在用户 session(**无前缀**),靠 state 锚 spare → 用户 session 存活。
#[test]
fn rc1_in_tmux_no_prefix_anchor_spares_user_session() {
    let sessions = names(&["team-coder-team", "team-x"]); // 用户 session 无 leader 前缀
    let anchor = anchors(&["team-coder-team"]); // state 锚 pane 所在 session
    let decision = sessions_to_kill(&sessions, &anchor);
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(
                to_kill,
                names(&["team-x"]),
                "only non-anchor session killed"
            );
            assert_eq!(
                spared,
                names(&["team-coder-team"]),
                "user/leader session spared by anchor"
            );
        }
        other => panic!("anchor session must force per-session kill, not {other:?}"),
    }
}

// 前缀判据仍生效(并集):leader 前缀 session spare,即使无 state 锚。
#[test]
fn prefix_session_spared_without_anchor() {
    let sessions = names(&["team-agent-leader-claude-ws-deadbeef", "team-x"]);
    let decision = sessions_to_kill(&sessions, &BTreeSet::new());
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(to_kill, names(&["team-x"]));
            assert_eq!(spared, names(&["team-agent-leader-claude-ws-deadbeef"]));
        }
        other => panic!("prefix session must spare, not {other:?}"),
    }
}

// RC-2(state 损坏无锚):anchor_sessions 空 → 退命名前缀判据(此处仅前缀 spare;无前缀则全 kill)。
// (spare_fallback_to_naming event 在 anchor_anchor_sessions 发,本纯函数只验退化后的决策。)
#[test]
fn rc2_no_anchor_falls_back_to_naming() {
    // 无锚 + 有前缀 leader → 前缀 spare。
    let with_leader = sessions_to_kill(
        &names(&["team-agent-leader-codex-ws-cafe", "team-x"]),
        &BTreeSet::new(),
    );
    assert!(matches!(with_leader, KillDecision::KillIndividually { .. }));
    // 无锚 + 无前缀(真损坏且 in_tmux 无前缀)→ 无 spare → 独享拆(退化兜底,与历史一致)。
    assert_eq!(
        sessions_to_kill(&names(&["team-x"]), &BTreeSet::new()),
        KillDecision::KillServerExclusive
    );
}

// RC-3(共享 socket):目标 2 session + 用户 1 session(锚)→ 只 kill 目标 2,用户存活,不整 server 拆。
#[test]
fn rc3_shared_socket_kills_only_target_sessions() {
    let sessions = names(&["team-a", "team-b", "user-shell"]);
    let anchor = anchors(&["user-shell"]);
    let decision = sessions_to_kill(&sessions, &anchor);
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(to_kill, names(&["team-a", "team-b"]));
            assert_eq!(spared, names(&["user-shell"]));
        }
        other => panic!("shared socket must not whole-server kill, got {other:?}"),
    }
}

// 并集语义:同一 session 既前缀又锚 → spare 一次(不重复)。
#[test]
fn union_prefix_and_anchor_no_double_count() {
    let sessions = names(&["team-agent-leader-claude-ws-beef"]);
    let anchor = anchors(&["team-agent-leader-claude-ws-beef"]);
    let decision = sessions_to_kill(&sessions, &anchor);
    assert_eq!(
        decision,
        KillDecision::KillIndividually {
            to_kill: vec![],
            spared: names(&["team-agent-leader-claude-ws-beef"])
        }
    );
}

#[test]
fn missing_coordinator_is_ok_when_shutdown_cleaned_session() {
    let ws = tmp_shutdown_workspace("missing-coordinator-clean");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-lane-l1-clean-shutdown",
            "agents": {
                "fake_impl": {
                    "status": "running",
                    "provider": "fake",
                    "window": "fake_impl"
                }
            }
        }),
    )
    .unwrap();
    let out = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        None,
        &CleanShutdownTransport::new(),
    )
    .expect("shutdown should complete");
    assert_eq!(out["coordinator"]["status"], json!("missing"));
    assert_eq!(
        out["ok"],
        json!(true),
        "coordinator.status=missing alone must not make a fully cleaned shutdown partial: {out}"
    );
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["residuals"]["sessions"], json!([]));
    assert_eq!(out["residuals"]["processes"], json!([]));
    assert_eq!(out["residuals"]["owned_files"], json!([]));
}

#[test]
fn owned_empty_endpoint_cleanup_removes_socket_file_before_reporting_ok() {
    let ws = tmp_shutdown_workspace("owned-empty-endpoint-clean");
    let socket = ws.join("owned.sock");
    std::fs::write(&socket, b"socket").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-owned-clean",
            "tmux_endpoint": socket.to_string_lossy(),
            "tmux_socket": socket.to_string_lossy(),
            "tmux_socket_source": "workspace",
            "is_external_leader": false,
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["residuals"]["owned_files"], json!([]));
    assert!(
        !socket.exists(),
        "owned empty endpoint socket file must be removed"
    );
    assert!(
        transport.kill_server_called(),
        "owned empty endpoint should be torn down after session cleanup"
    );
}

#[test]
fn leader_env_endpoint_is_not_owned_or_removed() {
    let ws = tmp_shutdown_workspace("leader-env-endpoint-spared");
    let socket = ws.join("leader-env.sock");
    std::fs::write(&socket, b"socket").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-external",
            "tmux_endpoint": socket.to_string_lossy(),
            "tmux_socket": socket.to_string_lossy(),
            "tmux_socket_source": "leader_env",
            "is_external_leader": true,
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["residuals"]["owned_files"], json!([]));
    assert!(socket.exists(), "leader_env socket is not team-owned");
    assert!(
        !transport.kill_server_called(),
        "leader_env/shared socket must never be torn down by owned cleanup"
    );
}

#[test]
fn owned_file_residual_makes_shutdown_failed_not_partial() {
    let ws = tmp_shutdown_workspace("owned-file-residual-failed");
    let socket_dir = ws.join("owned-dir.sock");
    std::fs::create_dir_all(&socket_dir).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-owned-residual",
            "tmux_endpoint": socket_dir.to_string_lossy(),
            "tmux_socket": socket_dir.to_string_lossy(),
            "tmux_socket_source": "workspace",
            "is_external_leader": false,
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("failed"));
    assert_eq!(out["phase"], json!(null));
    assert_eq!(
        out["residuals"]["owned_files"],
        json!([{ "path": socket_dir.display().to_string() }])
    );
}

#[test]
fn scoped_shutdown_keeps_owned_endpoint_when_sibling_session_remains() {
    let ws = tmp_shutdown_workspace("scoped-owned-endpoint-sibling");
    let socket = ws.join("owned-shared.sock");
    std::fs::write(&socket, b"socket").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "team-a",
            "teams": {
                "team-a": {
                    "team_key": "team-a",
                    "status": "alive",
                    "session_name": "team-a",
                    "tmux_endpoint": socket.to_string_lossy(),
                    "tmux_socket": socket.to_string_lossy(),
                    "tmux_socket_source": "workspace",
                    "is_external_leader": false,
                    "agents": {}
                },
                "team-b": {
                    "team_key": "team-b",
                    "status": "alive",
                    "session_name": "team-b",
                    "tmux_endpoint": socket.to_string_lossy(),
                    "tmux_socket": socket.to_string_lossy(),
                    "tmux_socket_source": "workspace",
                    "is_external_leader": false,
                    "agents": {}
                }
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![PaneInfo {
            pane_id: PaneId::new("%2"),
            session: SessionName::new("team-b"),
            window_index: Some(0),
            window_name: Some(WindowName::new("worker")),
            pane_index: Some(0),
            tty: None,
            current_command: Some("fake".to_string()),
            current_path: None,
            active: true,
            pane_pid: None,
            leader_env: BTreeMap::new(),
        }])
        .with_targets_persist_after_kill();

    let out =
        crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, Some("team-a"), &transport)
            .expect("shutdown should complete");

    // 0.4.0 refactor: shutdown now reports `ok=false` when sessions appear as
    // residuals (`status="failed"`). This test uses
    // `with_targets_persist_after_kill()` which leaves the killed session
    // visible to the residual probe, so the new refactor surfaces it as a
    // residual. The actual contract of this test is the SOCKET keep behavior
    // (owned endpoint preserved when sibling sessions live on it), which the
    // assertions below cover. We assert no owned_files residual + socket on
    // disk + no kill_server, which are the invariants this test actually
    // exists to guard.
    assert_eq!(out["residuals"]["owned_files"], json!([]), "full out={out}");
    assert!(
        socket.exists(),
        "owned endpoint stays while sibling team session remains"
    );
    assert!(
        !transport.kill_server_called(),
        "scoped shutdown must not tear down a non-empty shared owned endpoint"
    );
}

#[test]
fn repeated_owned_endpoint_shutdowns_leave_no_socket_file_growth() {
    let ws = tmp_shutdown_workspace("owned-loop-no-growth");
    let sockets = (0..20)
        .map(|idx| ws.join(format!("owned-loop-{idx}.sock")))
        .collect::<Vec<_>>();
    let starting = sockets.iter().filter(|path| path.exists()).count();
    for socket in &sockets {
        std::fs::write(socket, b"socket").unwrap();
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "session_name": "team-owned-loop",
                "tmux_endpoint": socket.to_string_lossy(),
                "tmux_socket": socket.to_string_lossy(),
                "tmux_socket_source": "workspace",
                "is_external_leader": false,
                "agents": {}
            }),
        )
        .unwrap();
        let out = crate::cli::lifecycle_port::shutdown_with_transport(
            &ws,
            true,
            None,
            &CleanShutdownTransport::new(),
        )
        .expect("shutdown should complete");
        assert_eq!(out["ok"], json!(true), "shutdown report: {out}");
    }
    let ending = sockets.iter().filter(|path| path.exists()).count();
    assert_eq!(
        ending, starting,
        "owned socket files must not grow across loops"
    );
}

fn tmp_shutdown_workspace(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-shutdown-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join(".team").join("runtime")).unwrap();
    dir
}

struct CleanShutdownTransport {
    session_present: Mutex<bool>,
    targets: Vec<PaneInfo>,
    kill_server_called: Mutex<bool>,
    probe_timeout_kind: Option<&'static str>,
    targets_persist_after_kill: bool,
    // E49 (0.3.24 P0, shutdown kills leader CLI): record per-pane / per-window /
    // per-session kill targets so RED contracts can assert (a) the leader pane is
    // never killed and (b) worker panes ARE killed via the new per-pane path.
    killed_panes: Mutex<Vec<String>>,
    killed_window_targets: Mutex<Vec<String>>,
    killed_sessions: Mutex<Vec<String>>,
}

impl CleanShutdownTransport {
    fn new() -> Self {
        Self {
            session_present: Mutex::new(true),
            targets: Vec::new(),
            kill_server_called: Mutex::new(false),
            probe_timeout_kind: None,
            targets_persist_after_kill: false,
            killed_panes: Mutex::new(Vec::new()),
            killed_window_targets: Mutex::new(Vec::new()),
            killed_sessions: Mutex::new(Vec::new()),
        }
    }

    fn with_targets(mut self, targets: Vec<PaneInfo>) -> Self {
        self.targets = targets;
        self
    }

    fn kill_server_called(&self) -> bool {
        *self.kill_server_called.lock().unwrap()
    }

    fn with_probe_timeout(mut self, probe: &'static str) -> Self {
        self.probe_timeout_kind = Some(probe);
        self
    }

    fn with_targets_persist_after_kill(mut self) -> Self {
        self.targets_persist_after_kill = true;
        self
    }

    /// E49 (0.3.24 P0): observed per-pane kill targets, in call order.
    fn killed_panes(&self) -> Vec<String> {
        self.killed_panes.lock().unwrap().clone()
    }

    /// E49 (0.3.24 P0): observed per-window kill targets (target stringified).
    fn killed_window_targets(&self) -> Vec<String> {
        self.killed_window_targets.lock().unwrap().clone()
    }

    /// E49 (0.3.24 P0): observed per-session kill targets, in call order.
    fn killed_sessions_observed(&self) -> Vec<String> {
        self.killed_sessions.lock().unwrap().clone()
    }
}

impl Transport for CleanShutdownTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("shutdown test does not spawn")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("shutdown test does not spawn")
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        unimplemented!("shutdown test does not inject")
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<crate::model::enums::PaneLiveness, TransportError> {
        Ok(crate::model::enums::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        if *self.session_present.lock().unwrap() || self.targets_persist_after_kill {
            Ok(self.targets.clone())
        } else {
            Ok(Vec::new())
        }
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session_present.lock().unwrap())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        if let Some(probe) = self.probe_timeout_kind {
            crate::os_probe::set_probe_timeout_for_test(probe, None, 900);
        }
        self.killed_sessions
            .lock()
            .unwrap()
            .push(session.as_str().to_string());
        *self.session_present.lock().unwrap() = false;
        Ok(())
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        *self.kill_server_called.lock().unwrap() = true;
        Ok(())
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        self.killed_window_targets
            .lock()
            .unwrap()
            .push(format!("{target:?}"));
        Ok(())
    }

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        self.killed_panes
            .lock()
            .unwrap()
            .push(pane.as_str().to_string());
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

#[test]
fn lsof_cwd_timeout_is_diagnostic_not_shutdown_partial() {
    let ws = tmp_shutdown_workspace("lsof-cwd-timeout-diagnostic");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-lsof-cwd-timeout",
            "is_external_leader": true,
            "agents": {
                "fake_impl": {
                    "status": "running",
                    "provider": "fake",
                    "window": "fake_impl"
                }
            }
        }),
    )
    .unwrap();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        None,
        &CleanShutdownTransport::new().with_probe_timeout("lsof_cwd"),
    )
    .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["phase"], json!(null));
    assert_eq!(out["verification_degraded"], json!(true));
    assert_eq!(out["probe_timeout_kind"], json!("lsof_cwd"));
    assert_eq!(out["residuals"]["sessions"], json!([]));
    assert_eq!(out["residuals"]["processes"], json!([]));

    let events = crate::event_log::EventLog::new(&ws)
        .tail(0)
        .expect("events");
    let shutdown = events
        .iter()
        .find(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("lifecycle.shutdown")
        })
        .unwrap_or_else(|| panic!("missing lifecycle.shutdown event: {events:?}"));
    assert_eq!(shutdown["status"], json!("ok"));
    assert_eq!(shutdown["phase"], json!(null));
    assert_eq!(shutdown["verification_degraded"], json!(true));
    assert_eq!(shutdown["probe_timeout_kind"], json!("lsof_cwd"));
}

#[test]
fn ps_table_timeout_still_degrades_shutdown_truth() {
    let ws = tmp_shutdown_workspace("ps-table-timeout-partial");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-ps-table-timeout",
            "agents": {
                "fake_impl": {
                    "status": "running",
                    "provider": "fake",
                    "window": "fake_impl"
                }
            }
        }),
    )
    .unwrap();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        None,
        &CleanShutdownTransport::new().with_probe_timeout("ps_table"),
    )
    .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("partial"));
    assert_eq!(out["phase"], json!("os_probe"));
    assert_eq!(out["verification_degraded"], json!(true));
    assert_eq!(out["probe_timeout_kind"], json!("ps_table"));
}

#[test]
fn bounded_coordinator_stop_returns_grace_window_late_success() {
    let ws = tmp_shutdown_workspace("late-coordinator-stop-success");
    let report = crate::cli::lifecycle_port::stop_coordinator_bounded_with(
        crate::coordinator::WorkspacePath::new(ws),
        std::time::Duration::from_millis(5),
        |_workspace| {
            std::thread::sleep(std::time::Duration::from_millis(25));
            Ok(crate::coordinator::StopReport {
                ok: true,
                status: crate::coordinator::StopOutcome::Stopped,
                pid: Some(crate::coordinator::Pid::new(12345)),
            })
        },
    )
    .expect("late result inside grace window must be returned")
    .expect("stop result should be ok");

    assert!(report.ok, "late success must not be discarded as timeout");
    assert_eq!(report.status, crate::coordinator::StopOutcome::Stopped);
}

#[test]
fn shutdown_outcome_late_or_postcheck_gone_is_ok_with_lsof_diagnostic() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: true,
            coordinator_stop_ok: None,
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone, target_session_spared: false,
        },
    );

    assert!(out.ok);
    assert_eq!(out.status, "ok");
    assert_eq!(out.phase, None);
}

#[test]
fn shutdown_outcome_coordinator_timeout_still_running_is_timeout() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: true,
            coordinator_stop_ok: None,
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Running, target_session_spared: false,
        },
    );

    assert!(!out.ok);
    assert_eq!(out.status, "timeout");
    assert_eq!(out.phase, Some("stop_coordinator"));
}

#[test]
fn shutdown_outcome_ps_table_degraded_still_partial_after_coordinator_gone() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: true,
            coordinator_timeout: true,
            coordinator_stop_ok: None,
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone, target_session_spared: false,
        },
    );

    assert!(!out.ok);
    assert_eq!(out.status, "partial");
    assert_eq!(out.phase, Some("os_probe"));
}

#[test]
fn leader_env_tmux_socket_never_kills_server_even_when_sessions_look_exclusive() {
    let ws = tmp_shutdown_workspace("leader-env-socket-no-kill-server");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "is_external_leader": true,
            "tmux_socket_source": "leader_env",
            "agents": {
                "fake_impl": {
                    "status": "running",
                    "provider": "fake",
                    "window": "fake_impl"
                }
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new().with_targets(vec![PaneInfo {
        pane_id: PaneId::new("%1"),
        session: SessionName::new("team-layout"),
        window_index: Some(0),
        window_name: Some(WindowName::new("team-w1")),
        pane_index: Some(0),
        tty: None,
        current_command: None,
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }]);

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["killed_sessions"], json!(["team-layout"]));
    assert_eq!(out["spared_sessions"], json!([]));
    assert!(
        !transport.kill_server_called(),
        "leader-env/shared socket shutdown must kill sessions individually, never kill-server"
    );
}

/// E49 (0.3.24 P0, shutdown kills leader CLI): managed-leader topology with
/// leader anchor pane + worker panes in the SAME tmux session.
///
/// Pre-fix (cli/mod.rs:629-638): `transport.kill_session(state.session_name)`
/// killed the session unconditionally — including the leader's own pane.
/// User truth: "执行命令绝不能关掉启动自己的那个 leader CLI". macmini repro:
/// run `team-agent quick-start ...`, then `team-agent shutdown` from the
/// same terminal → leader CLI dies.
///
/// Architect-approved fix:
///   * pre :629 guard: when state is managed (NOT external leader) and the
///     leader_receiver / team_owner anchor pane is in `state.session_name`,
///     do NOT call kill_session(state.session_name). Instead kill workers
///     per-pane via `kill_pane`, deriving pane_ids from state.agents +
///     teams[*].agents and EXCLUDING the leader anchor pane ids
///     (`collect_state_leader_anchor_pane_ids`).
///   * cli/mod.rs:384-435 managed_leader_socket_cleanup: spare any session
///     carrying a leader anchor; never kill_session it.
///
/// Pre-fix test asserted `killed_sessions=["team-current"]` — that was
/// LOCKING THE BUG (the architect explicitly named it). Post-fix:
///   * spared_sessions contains "team-current"
///   * killed_sessions does NOT contain "team-current"
///   * killed_panes contains worker panes %w1, %w2
///   * killed_panes does NOT contain %leader
///   * no kill_server call
#[test]
fn e49_managed_leader_shutdown_spares_leader_session_and_kills_workers_per_pane() {
    let ws = tmp_shutdown_workspace("e49-managed-leader-spares-session");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-current",
            "is_external_leader": false,
            "leader_receiver": {"pane_id": "%leader"},
            "team_owner": {"pane_id": "%leader"},
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "w1", "pane_id": "%w1"},
                "w2": {"status": "running", "provider": "codex", "window": "w2", "pane_id": "%w2"}
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![
            // Leader anchor pane
            PaneInfo {
                pane_id: PaneId::new("%leader"),
                session: SessionName::new("team-current"),
                window_index: Some(0),
                window_name: Some(WindowName::new("leader")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: true,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
            // Worker pane 1
            PaneInfo {
                pane_id: PaneId::new("%w1"),
                session: SessionName::new("team-current"),
                window_index: Some(1),
                window_name: Some(WindowName::new("w1")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
            // Worker pane 2
            PaneInfo {
                pane_id: PaneId::new("%w2"),
                session: SessionName::new("team-current"),
                window_index: Some(2),
                window_name: Some(WindowName::new("w2")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
        ])
        .with_targets_persist_after_kill();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    let killed_sessions = transport.killed_sessions_observed();
    assert!(
        !killed_sessions.iter().any(|s| s == "team-current"),
        "E49 (RED): managed-leader shutdown must NEVER kill_session(team-current) — \
         that ends the leader's own pane (the user's CLI). Got transport \
         killed_sessions={killed_sessions:?}"
    );
    let killed_panes = transport.killed_panes();
    assert!(
        !killed_panes.iter().any(|p| p == "%leader"),
        "E49 (RED): leader anchor pane %leader must NEVER be killed. Got \
         killed_panes={killed_panes:?}"
    );
    assert!(
        killed_panes.iter().any(|p| p == "%w1"),
        "E49: worker pane %w1 must be killed per-pane (workers cleared, leader spared). \
         Got killed_panes={killed_panes:?}"
    );
    assert!(
        killed_panes.iter().any(|p| p == "%w2"),
        "E49: worker pane %w2 must be killed per-pane. Got killed_panes={killed_panes:?}"
    );
    assert!(
        !transport.kill_server_called(),
        "E49: managed topology must never kill the tmux server (would end leader pane)."
    );
    // Report-level assertions: the leader session must appear in spared_sessions,
    // not killed_sessions.
    let out_killed = out["killed_sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let out_spared = out["spared_sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !out_killed
            .iter()
            .any(|v| v.as_str() == Some("team-current")),
        "E49: report's killed_sessions must NOT contain the leader session. Got {out_killed:?}"
    );
    assert!(
        out_spared
            .iter()
            .any(|v| v.as_str() == Some("team-current")),
        "E49: report's spared_sessions MUST contain the leader session. Got {out_spared:?}"
    );
    // 0.4.0 refactor: when the target session is deliberately spared (E49
    // managed-leader topology), shutdown reports
    // `status="dirty_state", phase="target_session_spared", ok=false`. This
    // is semantically the SUCCESS path for E49 — leader pane preserved — but
    // the new refactor surfaces it as a non-clean exit so callers can detect
    // residual sessions. The E49 invariant (leader spared, workers killed per
    // pane) is already verified by the preceding `out_killed` / `out_spared`
    // assertions; here we pin the new top-level shape.
    assert_eq!(
        out["status"],
        json!("dirty_state"),
        "0.4.0 refactor: managed-leader spare path surfaces dirty_state \
         (target_session_spared) — leader was spared, not killed. Got out={out}"
    );
    assert_eq!(
        out["phase"],
        json!("target_session_spared"),
        "0.4.0 refactor: phase must name the spare reason. Got out={out}"
    );
    assert_eq!(out["session_killed"], json!(false));
}

/// E49 regression guard: external-leader topology (is_external_leader=true)
/// must STILL kill_session unconditionally. In that topology the team session
/// is a disposable worker session and the leader pane lives elsewhere
/// (a separate terminal / different socket). Sparing the session here would
/// leak worker processes after shutdown.
#[test]
fn e49_external_leader_shutdown_still_kills_team_session_unconditionally() {
    let ws = tmp_shutdown_workspace("e49-external-leader-still-kills");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-external",
            "is_external_leader": true,
            "leader_receiver": {"pane_id": "%leaderelsewhere"},
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "w1", "pane_id": "%w1"}
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![
            // Worker pane in team session
            PaneInfo {
                pane_id: PaneId::new("%w1"),
                session: SessionName::new("team-external"),
                window_index: Some(0),
                window_name: Some(WindowName::new("w1")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: true,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
            // Leader pane is in a DIFFERENT session (external topology)
            PaneInfo {
                pane_id: PaneId::new("%leaderelsewhere"),
                session: SessionName::new("user-shell"),
                window_index: Some(0),
                window_name: Some(WindowName::new("shell")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("zsh".to_string()),
                current_path: None,
                active: true,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            },
        ])
        .with_targets_persist_after_kill();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    let killed_sessions = transport.killed_sessions_observed();
    assert!(
        killed_sessions.iter().any(|s| s == "team-external"),
        "E49 regression guard: external-leader topology must STILL kill_session \
         the team session — leader pane lives elsewhere so this is safe. Got \
         transport killed_sessions={killed_sessions:?}"
    );
    assert_eq!(out["ok"], json!(true));
}

/// E49 regression guard: managed topology where the leader anchor pane is NOT
/// in `state.session_name` (e.g. a stale state from an aborted launch where
/// only worker panes exist in the session). The pre-fix behaviour (kill_session)
/// is preserved here — no leader pane is at risk.
#[test]
fn e49_managed_leader_without_anchor_in_session_still_kills_session() {
    let ws = tmp_shutdown_workspace("e49-managed-no-anchor-in-session");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-orphan",
            "is_external_leader": false,
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "w1", "pane_id": "%w1"}
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![PaneInfo {
            pane_id: PaneId::new("%w1"),
            session: SessionName::new("team-orphan"),
            window_index: Some(0),
            window_name: Some(WindowName::new("w1")),
            pane_index: Some(0),
            tty: None,
            current_command: Some("codex".to_string()),
            current_path: None,
            active: true,
            pane_pid: None,
            leader_env: BTreeMap::new(),
        }])
        .with_targets_persist_after_kill();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    let killed_sessions = transport.killed_sessions_observed();
    assert!(
        killed_sessions.iter().any(|s| s == "team-orphan"),
        "E49 regression guard: when no leader anchor pane is in the session, \
         the pre-fix kill_session path is preserved (no leader is at risk). \
         Got transport killed_sessions={killed_sessions:?}"
    );
    assert_eq!(out["ok"], json!(true));
}

#[test]
fn shutdown_missing_topology_marker_defaults_to_managed_cleanup() {
    let ws = tmp_shutdown_workspace("missing-topology-marker-managed-cleanup");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-current",
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![PaneInfo {
            pane_id: PaneId::new("%1"),
            session: SessionName::new("team-current"),
            window_index: Some(0),
            window_name: Some(WindowName::new("leader")),
            pane_index: Some(0),
            tty: None,
            current_command: Some("codex".to_string()),
            current_path: None,
            active: true,
            pane_pid: None,
            leader_env: BTreeMap::new(),
        }])
        .with_targets_persist_after_kill();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["killed_sessions"], json!(["team-current"]));
    assert_eq!(out["spared_sessions"], json!([]));
    assert!(
        !transport.kill_server_called(),
        "missing topology marker must default to managed cleanup, not external kill-server"
    );
}

// ════════════════════════════════════════════════════════════════════════
// unit-0 (Stage 0) characterization tests
//
// These pin the CURRENT classify_shutdown_outcome behavior so that the
// Stage 1 refactor (unit-2: move shutdown into lifecycle and feed real
// session-kill facts into the classifier) is detectable as a behavior
// change rather than an accidental regression.
//
// The classifier today does NOT consume `session_killed` / `spared_sessions`
// facts — it only looks at residuals. That is the 0.3.39 false-green bug
// surface. The tests below LOCK the current OK/failed/partial/timeout
// branches against opaque residual booleans so that:
//   - unit-2 can wire in the missing topology facts and we observe the
//     intended new behavior on the false-green scenario,
//   - any unintended status change in OTHER branches breaks the suite.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn unit0_classify_outcome_no_residuals_returns_ok() {
    // Baseline: nothing residual, coordinator clean -> ok/status:"ok".
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: false,
            coordinator_stop_ok: Some(true),
            coordinator_post_stop:
                crate::cli::lifecycle_port::CoordinatorStopObservation::NotNeeded,
            target_session_spared: false,
        },
    );
    assert!(out.ok);
    assert_eq!(out.status, "ok");
    assert_eq!(out.phase, None);
}

#[test]
fn unit0_classify_outcome_session_residual_returns_failed() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: true,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: false,
            coordinator_stop_ok: Some(true),
            coordinator_post_stop:
                crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: false,
        },
    );
    assert!(!out.ok);
    assert_eq!(out.status, "failed");
}

#[test]
fn unit0_classify_outcome_coordinator_timeout_returns_timeout_phase() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: true,
            coordinator_stop_ok: Some(false),
            coordinator_post_stop:
                crate::cli::lifecycle_port::CoordinatorStopObservation::Running,
            target_session_spared: false,
        },
    );
    assert!(!out.ok);
    assert_eq!(out.status, "timeout");
    assert_eq!(out.phase, Some("stop_coordinator"));
}

#[test]
fn unit0_classify_outcome_cleanup_truth_degraded_returns_partial_os_probe() {
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: true,
            coordinator_timeout: false,
            coordinator_stop_ok: Some(true),
            coordinator_post_stop:
                crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: false,
        },
    );
    assert!(!out.ok);
    assert_eq!(out.status, "partial");
    assert_eq!(out.phase, Some("os_probe"));
}

#[test]
fn unit2_classify_outcome_target_session_spared_is_not_green() {
    // unit-2 false-green guard: when the target worker session was spared
    // (still alive) the classifier MUST NOT return ok/status:"ok". This is
    // the 0.3.39 shutdown false-green shape unit-2 fixes.
    let out = crate::cli::lifecycle_port::classify_shutdown_outcome(
        crate::cli::lifecycle_port::ShutdownOutcomeInput {
            kill_error: false,
            session_residuals: false,
            process_residuals: false,
            owned_file_residuals: false,
            cleanup_truth_degraded: false,
            coordinator_timeout: false,
            coordinator_stop_ok: Some(true),
            coordinator_post_stop:
                crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: true,
        },
    );
    assert!(
        !out.ok,
        "false-green regression: target_session_spared=true must force ok=false"
    );
    assert_eq!(out.status, "dirty_state");
    assert_eq!(out.phase, Some("target_session_spared"));
}

#[test]
fn unit0_sessions_to_kill_leader_prefixed_session_is_always_spared() {
    // Pinned invariant: a session named `team-agent-leader-*` is NEVER in
    // the kill list, even without an anchor. unit-1/3 wraps this in typed
    // identity; the behavior must remain identical.
    use crate::transport::SessionName;
    let session_names = vec![
        SessionName::new("team-real-worker"),
        SessionName::new("team-agent-leader-claude-x"),
    ];
    let decision = sessions_to_kill(&session_names, &BTreeSet::new());
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert!(to_kill.iter().any(|s| s.as_str() == "team-real-worker"));
            assert!(!to_kill
                .iter()
                .any(|s| s.as_str().starts_with("team-agent-leader-")));
            assert!(spared
                .iter()
                .any(|s| s.as_str() == "team-agent-leader-claude-x"));
        }
        other => panic!(
            "expected KillIndividually with leader spared; got {:?}",
            other
        ),
    }
}
