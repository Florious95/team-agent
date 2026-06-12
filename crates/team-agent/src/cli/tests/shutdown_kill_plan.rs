//! E12 (P0) · `sessions_to_kill` 纯决策单测(kill 决策下沉)。
//!
//! spare = state 锚 session(anchor_sessions) ∪ `team-agent-leader-` 命名前缀(并集,锚优先)。
//! 独享 socket(无 spare)才允许整 server 拆;共享/leader 在 → 逐 session kill。
//! 集成面由 tests/b5_leader_terminal_kill_red.rs 的真 tmux 契约覆盖,此处锁纯决策 + 4 反向 case。

use crate::cli::lifecycle_port::{sessions_to_kill, KillDecision};
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome, SpawnResult, Target, Transport,
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
        KillDecision::KillIndividually { to_kill: vec![], spared: vec![] }
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
            assert_eq!(to_kill, names(&["team-x"]), "only non-anchor session killed");
            assert_eq!(spared, names(&["team-coder-team"]), "user/leader session spared by anchor");
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
        KillDecision::KillIndividually { to_kill: vec![], spared: names(&["team-agent-leader-claude-ws-beef"]) }
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
        out["ok"], json!(true),
        "coordinator.status=missing alone must not make a fully cleaned shutdown partial: {out}"
    );
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["residuals"]["sessions"], json!([]));
    assert_eq!(out["residuals"]["processes"], json!([]));
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
}

impl CleanShutdownTransport {
    fn new() -> Self {
        Self {
            session_present: Mutex::new(true),
            targets: Vec::new(),
            kill_server_called: Mutex::new(false),
            probe_timeout_kind: None,
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
        Ok(CapturedText { text: String::new(), range })
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
        if *self.session_present.lock().unwrap() {
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

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        if let Some(probe) = self.probe_timeout_kind {
            crate::os_probe::set_probe_timeout_for_test(probe, None, 900);
        }
        *self.session_present.lock().unwrap() = false;
        Ok(())
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        *self.kill_server_called.lock().unwrap() = true;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
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

    let events = crate::event_log::EventLog::new(&ws).tail(0).expect("events");
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
fn leader_env_tmux_socket_never_kills_server_even_when_sessions_look_exclusive() {
    let ws = tmp_shutdown_workspace("leader-env-socket-no-kill-server");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
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
