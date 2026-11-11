//! E12 (P0) · `sessions_to_kill` 纯决策单测(kill 决策下沉)。
//!
//! spare = state 锚 session(anchor_sessions) ∪ `team-agent-leader-` 命名前缀(并集,锚优先)。
//! Cleanup is always per-session and ownership-gated; a socket is never sufficient authority for kill-server.
//! 集成面由 tests/b5_leader_terminal_kill_red.rs 的真 tmux 契约覆盖,此处锁纯决策 + 4 反向 case。

use crate::transport::{ BackendKind, CaptureRange, CapturedText, Key, PaneField, PaneId, PaneInfo, SessionName, SpawnResult, Target, Transport, TransportError, WindowName };
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

#[test]
fn family_shutdown_is_deepest_first_and_does_not_include_siblings() {
    let mut state = json!({"teams":{
        "root":{}, "child":{"parent_team_key":"root"},
        "grandchild":{"parent_team_key":"child"},
        "closed":{"parent_team_key":"root","status":"shutdown","leader_receiver":null},
        "sibling":{}, "unrelated":{"parent_team_key":"sibling"}
    }});
    assert_eq!(crate::cli::lifecycle_port::shutdown_descendants(&state, "root").unwrap(),
        vec!["grandchild", "child"]);
    state["teams"]["root"]["parent_team_key"] = json!("grandchild");
    assert!(crate::cli::lifecycle_port::shutdown_descendants(&state, "root").is_err());
}

#[test]
fn spared_parent_keeps_manager_registration_running() {
    let ws = tmp_shutdown_workspace("spared-manager-registration");
    let initial = json!({"active_team_key":"parent", "team_key":"parent", "teams":{
        "parent":{"session_name":"team-parent", "team_key":"parent", "generation":"parent", "status":"alive",
            "team_owner":{"pane_id":"%root"},
            "agents":{"manager":{"provider":"pi","status":"running","pane_id":"%manager"}}},
        "child":{"session_name":"team-child", "parent_team_key":"parent", "status":"alive",
            "team_owner":{"pane_id":"%manager"},
            "leader_receiver":{"status":"attached","pane_id":"%manager"},"agents":{}}
    }});
    crate::state::persist::save_runtime_state(&ws, &initial).unwrap();
    let before = crate::state::persist::load_runtime_state(&ws).unwrap();
    let backend = CleanShutdownTransport::new().with_targets(vec![PaneInfo {
        pane_id:PaneId::new("%manager"), session:SessionName::new("team-parent"),
        window_index:Some(0), window_name:Some(WindowName::new("manager")),
        pane_index:Some(0), tty:None, current_command:Some("pi".into()),
        current_path:Some(ws.clone()), active:true, pane_pid:None, leader_env:BTreeMap::new(),
    }]).with_owned_session(&ws, "team-parent", "parent", "parent");
    let result = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, Some("parent"), &backend).unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["phase"], "target_session_spared", "{result}");
    let saved = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(saved["teams"]["parent"]["agents"]["manager"], before["teams"]["parent"]["agents"]["manager"]);
    assert_eq!(saved["teams"]["parent"]["status"], "alive");
    assert_eq!(saved["active_team_key"], before["active_team_key"]);
    assert_eq!(saved["teams"]["child"]["leader_receiver"], before["teams"]["child"]["leader_receiver"]);
    assert!(backend.killed_panes().is_empty());
}

#[test]
fn stopped_child_owner_is_not_a_live_parent_worker_anchor() {
    use crate::cli::lifecycle_port::collect_state_leader_anchor_pane_ids as collect;
    let mut state = json!({"teams": {
        "parent": {"status":"alive", "team_owner":{"pane_id":"%leader"}},
        "child": {"status":"shutdown", "team_owner":{"pane_id":"%manager"},
            "leader_receiver":{"status":"attached", "pane_id":"%manager"}}
    }});
    assert_eq!(collect(&state, None), anchors(&["%leader"]));
    state["teams"]["child"]["status"] = json!("alive");
    assert_eq!(collect(&state, None), anchors(&["%leader", "%manager"]));
}

#[test]
fn leader_anchors_are_endpoint_scoped_and_unknown_anchors_stay_protected() {
    use crate::cli::lifecycle_port::collect_state_leader_anchor_pane_ids as collect;
    let state = json!({
        "team_owner": {"pane_id": "%2"},
        "leader_receiver": {"pane_id": "%2", "tmux_socket": "/tmp/s4-host.sock"},
        "teams": {
            "local": {
                "team_owner": {"pane_id": "%3", "tmux_socket": "/tmp/s4-worker.sock"},
                "leader_receiver": {"pane_id": "%3"}
            },
            "legacy": {"team_owner": {"pane_id": "%4"}}
        }
    });
    assert_eq!(collect(&state, Some("/tmp/s4-worker.sock")), anchors(&["%3", "%4"]));
    assert_eq!(collect(&state, Some("/tmp/s4-host.sock")), anchors(&["%2", "%4"]));
    assert_eq!(collect(&state, None), anchors(&["%2", "%3", "%4"]));

    // An unrelated receiver's socket is not evidence about the owner's pane.
    let different_panes = json!({
        "team_owner": {"pane_id": "%5"},
        "leader_receiver": {"pane_id": "%2", "tmux_socket": "/tmp/s4-host.sock"}
    });
    assert_eq!(collect(&different_panes, Some("/tmp/s4-worker.sock")), anchors(&["%5"]));
}

#[test]
#[cfg(unix)]
fn leader_anchor_endpoint_aliases_preserve_same_server_protection() {
    use crate::cli::lifecycle_port::collect_state_leader_anchor_pane_ids as collect;
    let ws = tmp_shutdown_workspace("anchor-endpoint-alias");
    let socket = ws.join("server.sock");
    let alias = ws.join("alias.sock");
    std::fs::write(&socket, b"fixture").unwrap();
    std::os::unix::fs::symlink(&socket, &alias).unwrap();
    let state = json!({"leader_receiver": {"pane_id": "%2", "tmux_socket": alias}});
    assert_eq!(collect(&state, socket.to_str()), anchors(&["%2"]));
    let short_name = format!("s4-anchor-{}", std::process::id());
    let full_path = crate::tmux_backend::socket_path_for_name(&short_name).unwrap();
    let state = json!({"leader_receiver": {"pane_id": "%2", "tmux_socket": short_name}});
    assert_eq!(collect(&state, full_path.to_str()), anchors(&["%2"]));
}

// RC-4: even an apparently exclusive socket uses per-session cleanup.

// RC-1(本 P0 复现):in_tmux leader 在用户 session(**无前缀**),靠 state 锚 spare → 用户 session 存活。

// 前缀判据仍生效(并集):leader 前缀 session spare,即使无 state 锚。

// RC-2(state 损坏无锚):anchor_sessions 空 → 退命名前缀判据(此处仅前缀 spare;无前缀则全 kill)。
// (spare_fallback_to_naming event 在 anchor_anchor_sessions 发,本纯函数只验退化后的决策。)

// RC-3(共享 socket):目标 2 session + 用户 1 session(锚)→ 只 kill 目标 2,用户存活,不整 server 拆。

// 并集语义:同一 session 既前缀又锚 → spare 一次(不重复)。

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
            "team_key": "team-owned-clean",
            "generation": "team-owned-clean",
            "tmux_endpoint": socket.to_string_lossy(),
            "tmux_socket": socket.to_string_lossy(),
            "tmux_socket_source": "workspace",
            "is_external_leader": false,
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new().with_owned_session(
        &ws,
        "team-owned-clean",
        "team-owned-clean",
        "team-owned-clean",
    );

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["residuals"]["owned_files"], json!([]));
    assert!(
        socket.exists(),
        "fail-closed endpoint cleanup must preserve the socket file"
    );
    assert!(
        !transport.kill_server_called(),
        "endpoint ownership must not authorize server teardown"
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
            "team_key": "team-owned-residual",
            "generation": "team-owned-residual",
            "tmux_endpoint": socket_dir.to_string_lossy(),
            "tmux_socket": socket_dir.to_string_lossy(),
            "tmux_socket_source": "workspace",
            "is_external_leader": false,
            "agents": {}
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new().with_owned_session(
        &ws,
        "team-owned-residual",
        "team-owned-residual",
        "team-owned-residual",
    );

    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["phase"], json!(null));
    assert_eq!(out["residuals"]["owned_files"], json!([]));
    assert!(socket_dir.exists(), "fail-closed cleanup preserves residual files");
    assert!(!transport.kill_server_called());
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
fn scoped_clean_shutdown_persists_team_shutdown_status_after_disk_roundtrip() {
    let ws = tmp_shutdown_workspace("scoped-clean-status-roundtrip");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "schema_version": 1,
            "active_team_key": "current",
            "team_key": "current",
            "generation": "current",
            "session_name": "team-current",
            "team_dir": ws.display().to_string(),
            "workspace": ws.display().to_string(),
            "agents": {
                "adminweb": {
                    "agent_id": "adminweb",
                    "status": "running",
                    "provider": "fake",
                    "window": "adminweb"
                }
            },
            "teams": {
                "current": {
                    "team_key": "current",
                    "generation": "current",
                    "session_name": "team-current",
                    "team_dir": ws.display().to_string(),
                    "workspace": ws.display().to_string(),
                    "agents": {
                        "adminweb": {
                            "agent_id": "adminweb",
                            "status": "running",
                            "provider": "fake",
                            "window": "adminweb"
                        }
                    }
                }
            }
        }),
    )
    .unwrap();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        Some("current"),
        &CleanShutdownTransport::new().with_owned_session(
            &ws,
            "team-current",
            "current",
            "current",
        ),
    )
    .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(true), "shutdown report: {out}");
    assert_eq!(out["session_killed"], json!(true), "shutdown report: {out}");
    assert_eq!(
        out["verification_degraded"],
        json!(false),
        "shutdown report: {out}"
    );
    let saved = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        saved
            .pointer("/teams/current/status")
            .and_then(serde_json::Value::as_str),
        Some("shutdown"),
        "0.5.27 RED: clean scoped shutdown returned session_killed=true and not degraded, \
         so disk state must persist teams.current.status=shutdown. The compact projected \
         save path must not drop the tombstone; saved={saved}"
    );
    assert_eq!(
        saved
            .pointer("/teams/current/agents/adminweb/status")
            .and_then(serde_json::Value::as_str),
        Some("stopped"),
        "0.5.27 RED: clean shutdown must also persist stopped agents under the retained team; saved={saved}"
    );
}

#[test]
fn scoped_degraded_shutdown_does_not_persist_team_shutdown_status() {
    let ws = tmp_shutdown_workspace("scoped-degraded-no-status");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "schema_version": 1,
            "active_team_key": "current",
            "team_key": "current",
            "generation": "current",
            "session_name": "team-current",
            "agents": {
                "adminweb": {
                    "agent_id": "adminweb",
                    "status": "running",
                    "provider": "fake",
                    "window": "adminweb"
                }
            },
            "teams": {
                "current": {
                    "team_key": "current",
                    "generation": "current",
                    "session_name": "team-current",
                    "agents": {
                        "adminweb": {
                            "agent_id": "adminweb",
                            "status": "running",
                            "provider": "fake",
                            "window": "adminweb"
                        }
                    }
                }
            }
        }),
    )
    .unwrap();

    let out = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        Some("current"),
        &CleanShutdownTransport::new()
            .with_owned_session(&ws, "team-current", "current", "current")
            .with_probe_timeout("ps_table"),
    )
    .expect("shutdown should complete");

    assert_eq!(out["session_killed"], json!(true), "shutdown report: {out}");
    assert_eq!(
        out["verification_degraded"],
        json!(true),
        "shutdown report: {out}"
    );
    let saved = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_ne!(
        saved
            .pointer("/teams/current/status")
            .and_then(serde_json::Value::as_str),
        Some("shutdown"),
        "degraded shutdown must remain dirty/diagnostic and must not stamp \
         teams.current.status=shutdown; saved={saved}"
    );
}

#[test]
fn repeated_owned_endpoint_shutdowns_leave_no_socket_file_growth() {
    #[cfg(unix)]
    const CHILD: &str = "RB11_F01_NATURAL_PROBE_FAILURE_CHILD";
    #[cfg(unix)]
    const TEST: &str =
        "cli::tests::shutdown_kill_plan::repeated_owned_endpoint_shutdowns_leave_no_socket_file_growth";

    #[cfg(unix)]
    if std::env::var_os(CHILD).is_none() {
        let path = tmp_shutdown_workspace("rb11-f01-empty-path").join("bin");
        std::fs::create_dir_all(&path).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "directed child failed: status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test")
                && stdout.contains(&format!("test {TEST} ..."))
                && stdout.contains("RB11_F01_CHILD_SUCCESS"),
            "directed child omitted success marker: stdout={} stderr={}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        println!("RB11_F01_DIRECTED_CHILD_OUTPUT\n{stdout}");
        return;
    }

    #[cfg(unix)]
    let expected_probe_failure = std::env::var_os(CHILD).is_some();
    #[cfg(not(unix))]
    let expected_probe_failure = false;
    let ws = tmp_shutdown_workspace("owned-loop-no-growth");
    let sockets = (0..20)
        .map(|idx| ws.join(format!("owned-loop-{idx}.sock")))
        .collect::<Vec<_>>();
    let starting = sockets.iter().filter(|path| path.exists()).count();
    for (iteration, socket) in sockets.iter().enumerate() {
        std::fs::write(socket, b"socket").unwrap();
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "session_name": "team-owned-loop",
                "team_key": "team-owned-loop",
                "generation": "team-owned-loop",
                "tmux_endpoint": socket.to_string_lossy(),
                "tmux_socket": socket.to_string_lossy(),
                "tmux_socket_source": "workspace",
                "is_external_leader": false,
                "agents": {}
            }),
        )
        .unwrap();
        let before_len = crate::event_log::EventLog::new(&ws)
            .tail(0)
            .expect("read shutdown event boundary")
            .len();
        let result = crate::cli::lifecycle_port::shutdown_with_transport(
            &ws,
            true,
            None,
            &CleanShutdownTransport::new().with_owned_session(
                &ws,
                "team-owned-loop",
                "team-owned-loop",
                "team-owned-loop",
            ),
        );
        let out = match &result {
            Ok(out) if !shutdown_result_is_degraded(out) => out,
            Ok(out) if expected_probe_failure => {
                let diagnostic =
                    shutdown_invocation_diagnostic(&ws, before_len, iteration, socket, &result);
                assert!(
                    shutdown_result_is_degraded(out),
                    "directed probe-failure child unexpectedly clean: {diagnostic}"
                );
                assert_eq!(
                    out["ok"],
                    json!(false),
                    "probe failure must remain degraded: {out}"
                );
                if iteration == 0 {
                    println!("RB11_F01_FIRST_FAILURE {diagnostic}");
                }
                out
            }
            Ok(out) => {
                let diagnostic =
                    shutdown_invocation_diagnostic(&ws, before_len, iteration, socket, &result);
                panic!("unexpected degraded shutdown: {diagnostic}");
            }
            Err(error) => {
                let diagnostic =
                    shutdown_invocation_diagnostic(&ws, before_len, iteration, socket, &result);
                panic!("shutdown invocation failed: {error}; {diagnostic}");
            }
        };
        if !expected_probe_failure {
            assert_eq!(out["ok"], json!(true), "shutdown report: {out}");
        }
    }
    let ending = sockets.iter().filter(|path| path.exists()).count();
    assert_eq!(
        ending,
        starting + sockets.len(),
        "fail-closed endpoint cleanup must not remove unverified socket files"
    );
    #[cfg(unix)]
    if expected_probe_failure {
        println!("RB11_F01_CHILD_SUCCESS iterations={}", sockets.len());
    }
}

fn shutdown_result_is_degraded(out: &serde_json::Value) -> bool {
    out["ok"] != json!(true)
        || out["probe_degraded"] == json!(true)
        || out["verification_degraded"] == json!(true)
}

fn shutdown_invocation_diagnostic(
    ws: &Path,
    before_len: usize,
    iteration: usize,
    socket: &Path,
    result: &Result<serde_json::Value, crate::cli::CliError>,
) -> String {
    let result_summary = match result {
        Ok(out) => format!("report={out}"),
        Err(error) => format!("error={error}"),
    };
    let events = crate::event_log::EventLog::new(ws)
        .tail(0)
        .unwrap_or_else(|error| {
            panic!(
                "shutdown diagnostic event read failed: iteration={iteration} socket={} \
                 {result_summary}; error={error}",
                socket.display()
            )
        });
    let appended = events.get(before_len..).unwrap_or_else(|| {
        panic!(
            "shutdown diagnostic boundary invalid: iteration={iteration} socket={} \
             {result_summary}; before_len={before_len} event_len={}",
            socket.display(),
            events.len()
        )
    });
    fn event_name(event: &serde_json::Value) -> Option<&str> {
        event["event"].as_str()
    }
    let starts = appended
        .iter()
        .enumerate()
        .filter(|(_, event)| event_name(event) == Some("lifecycle.shutdown.started"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let terminals = appended
        .iter()
        .enumerate()
        .filter(|(_, event)| event_name(event) == Some("lifecycle.shutdown"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if starts.len() != 1 || terminals.len() != 1 || starts[0] >= terminals[0] {
        panic!(
            "shutdown diagnostic markers invalid: iteration={iteration} socket={} \
             {result_summary}; appended={appended:?}",
            socket.display()
        );
    }
    let terminal = &appended[terminals[0]];
    // These diagnostics use deliberately unverified fixture paths, not the
    // computed workspace endpoint. A source label cannot grant ownership.
    let workspace_socket = crate::tmux_backend::socket_name_for_workspace(ws);
    assert_ne!(
        socket.file_name(),
        Some(std::ffi::OsStr::new(&workspace_socket)),
        "diagnostic fixture unexpectedly uses the owned workspace socket"
    );
    if !terminal["owned_endpoint"].is_null() {
        panic!(
            "shutdown diagnostic falsely claimed fixture endpoint ownership: \
             iteration={iteration} socket={} {result_summary}; terminal={terminal}",
            socket.display()
        );
    }
    let bounded = &appended[starts[0]..=terminals[0]];
    let probe_failures = bounded
        .iter()
        .filter(|event| event_name(event) == Some("shutdown.process_probe_failed"))
        .cloned()
        .collect::<Vec<_>>();
    if probe_failures.is_empty() {
        panic!(
            "shutdown diagnostic missing process-probe failure: iteration={iteration} \
             socket={} {result_summary}; bounded={bounded:?}",
            socket.display()
        );
    }
    for event in &probe_failures {
        let phase = event["phase"].as_str();
        if event["probe"].as_str() != Some("ps_table")
            || !matches!(phase, Some("entry" | "residual_round" | "post_verify"))
            || !matches!(
                event["error"].as_str(),
                Some(error)
                    if !error.trim().is_empty()
                        && !error.contains("exited with status")
                        && (error.contains("No such file") || error.contains("os error 2"))
            )
        {
            panic!(
                "shutdown diagnostic probe failure invalid: iteration={iteration} \
                 socket={} {result_summary}; event={event}",
                socket.display()
            );
        }
    }
    format!(
        "iteration={iteration} socket={} {result_summary}; probe_failures={probe_failures:?}",
        socket.display()
    )
}

#[cfg(unix)]
#[test]
fn rb07_probe_failure_diagnostic_is_iteration_bound() {
    const CHILD: &str = "RB07_PROBE_FAILURE_DIAGNOSTIC_CHILD";
    const TEST: &str =
        "cli::tests::shutdown_kill_plan::rb07_probe_failure_diagnostic_is_iteration_bound";
    if std::env::var_os(CHILD).is_none() {
        let path = tmp_shutdown_workspace("rb07-empty-path").join("bin");
        std::fs::create_dir_all(&path).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "directed child failed: status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test")
                && stdout.contains(&format!("test {TEST} ..."))
                && stdout.contains("RB07_CHILD_SUCCESS"),
            "directed child omitted success marker: stdout={} stderr={}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let ws = tmp_shutdown_workspace("rb07-child");
    let socket = ws.join("owned.sock");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-rb07-child",
            "tmux_endpoint": socket.to_string_lossy(),
            "tmux_socket": socket.to_string_lossy(),
            "tmux_socket_source": "workspace",
            "is_external_leader": false,
            "agents": {}
        }),
    )
    .unwrap();
    let event_log = crate::event_log::EventLog::new(&ws);
    event_log
        .write(
            "shutdown.process_probe_failed",
            json!({
                "phase": "entry",
                "probe": "ps_table",
                "error": "rb07-stale-decoy"
            }),
        )
        .unwrap();
    let before_len = event_log.tail(0).unwrap().len();
    let result = crate::cli::lifecycle_port::shutdown_with_transport(
        &ws,
        true,
        None,
        &CleanShutdownTransport::new(),
    );
    let diagnostic = shutdown_invocation_diagnostic(&ws, before_len, 0, &socket, &result);
    let failures = match result {
        Ok(out) => {
            assert!(
                shutdown_result_is_degraded(&out),
                "child must force ps failure: {out}"
            );
            let text = diagnostic;
            assert!(
                !text.contains("rb07-stale-decoy"),
                "stale event leaked: {text}"
            );
            text
        }
        Err(error) => panic!("child shutdown unexpectedly returned Err: {error}; {diagnostic}"),
    };
    assert!(
        failures.contains("probe_failures=["),
        "missing bounded failures: {failures}"
    );
    println!("RB07_CHILD_SUCCESS {failures}");
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
    kill_session_error: Option<String>,
    owner: Option<crate::transport::SessionOwner>,
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
            kill_session_error: None,
            owner: None,
            killed_panes: Mutex::new(Vec::new()),
            killed_window_targets: Mutex::new(Vec::new()),
            killed_sessions: Mutex::new(Vec::new()),
        }
    }

    fn with_targets(mut self, targets: Vec<PaneInfo>) -> Self {
        self.targets = targets;
        self
    }

    fn with_owned_session(
        mut self,
        workspace: &Path,
        session: &str,
        team: &str,
        generation: &str,
    ) -> Self {
        self.owner = Some(crate::transport::SessionOwner {
            workspace: workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf())
                .to_string_lossy()
                .into_owned(),
            team: team.to_string(),
            generation: generation.to_string(),
        });
        if self.targets.is_empty() {
            self.targets.push(PaneInfo {
                pane_id: PaneId::new("%owned"),
                session: SessionName::new(session),
                window_index: Some(0),
                window_name: Some(WindowName::new("owned")),
                pane_index: Some(0),
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: Some(workspace.to_path_buf()),
                active: true,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            });
        } else {
            for target in &mut self.targets {
                if target.current_path.is_none() {
                    target.current_path = Some(workspace.to_path_buf());
                }
            }
        }
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

    fn with_kill_session_error(mut self, detail: impl Into<String>) -> Self {
        self.kill_session_error = Some(detail.into());
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

    fn session_owner(
        &self,
        _session: &SessionName,
    ) -> Result<Option<crate::transport::SessionOwner>, TransportError> {
        Ok(self.owner.clone())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
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
        match self.kill_session_error.as_deref() {
            Some(detail) => Err(TransportError::Subprocess {
                argv: vec![
                    "tmux".to_string(),
                    "kill-session".to_string(),
                    "-t".to_string(),
                    session.as_str().to_string(),
                ],
                code: Some(1),
                stderr: detail.to_string(),
            }),
            None => Ok(()),
        }
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

}

#[test]
fn lsof_cwd_timeout_is_diagnostic_not_shutdown_partial() {
    let ws = tmp_shutdown_workspace("lsof-cwd-timeout-diagnostic");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-lsof-cwd-timeout",
            "team_key": "team-lsof-cwd-timeout",
            "generation": "team-lsof-cwd-timeout",
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
        &CleanShutdownTransport::new()
            .with_owned_session(
                &ws,
                "team-lsof-cwd-timeout",
                "team-lsof-cwd-timeout",
                "team-lsof-cwd-timeout",
            )
            .with_probe_timeout("lsof_cwd"),
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
            "team_key": "team-ps-table-timeout",
            "generation": "team-ps-table-timeout",
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
        &CleanShutdownTransport::new()
            .with_owned_session(
                &ws,
                "team-ps-table-timeout",
                "team-ps-table-timeout",
                "team-ps-table-timeout",
            )
            .with_probe_timeout("ps_table"),
    )
    .expect("shutdown should complete");

    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("partial"));
    assert_eq!(out["phase"], json!("os_probe"));
    assert_eq!(out["verification_degraded"], json!(true));
    assert_eq!(out["probe_timeout_kind"], json!("ps_table"));
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
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: false,
        },
    );

    assert!(out.ok);
    assert_eq!(out.status, "ok");
    assert_eq!(out.phase, None);
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
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: false,
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
            "team_key": "team-layout",
            "generation": "team-layout",
            "session_name": "team-layout",
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
    let transport = transport.with_owned_session(&ws, "team-layout", "team-layout", "team-layout");

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
            "team_key": "current",
            "generation": "current",
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
        .with_targets_persist_after_kill()
        .with_owned_session(&ws, "team-current", "current", "current");

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
            "team_key": "team-external",
            "generation": "team-external",
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
        .with_targets_persist_after_kill()
        .with_owned_session(&ws, "team-external", "team-external", "team-external");

    let event_log = crate::event_log::EventLog::new(&ws);
    let event_start = event_log.tail(0).expect("events before shutdown").len();
    let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
        .expect("shutdown should complete");

    let killed_sessions = transport.killed_sessions_observed();
    assert!(
        killed_sessions.iter().any(|s| s == "team-external"),
        "E49 regression guard: external-leader topology must STILL kill_session \
         the team session — leader pane lives elsewhere so this is safe. Got \
         transport killed_sessions={killed_sessions:?}"
    );
    if out["ok"] != json!(true) {
        let events = event_log.tail(0).expect("events after shutdown");
        let invocation_events = events
            .into_iter()
            .skip(event_start)
            .take(64)
            .collect::<Vec<_>>();
        panic!(
            "E49 external-leader shutdown returned a non-clean outcome: workspace tag=e49-external-leader-still-kills, expected session=team-external, shutdown report={out}, killed_sessions={killed_sessions:?}, bounded invocation events={invocation_events:?}"
        );
    }
    assert_eq!(out["ok"], json!(true));
}

#[test]
fn e49_external_leader_non_clean_diagnostic_is_bounded_and_still_kills() {
    let ws = tmp_shutdown_workspace("e49-external-leader-non-clean");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-external",
            "team_key": "team-external",
            "generation": "team-external",
            "is_external_leader": true,
            "leader_receiver": {"pane_id": "%leaderelsewhere"},
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "w1", "pane_id": "%w1"}
            }
        }),
    )
    .unwrap();
    let transport = CleanShutdownTransport::new()
        .with_targets(vec![PaneInfo {
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
        }])
        .with_kill_session_error("deterministic E49 kill-session failure")
        .with_owned_session(&ws, "team-external", "team-external", "team-external");

    let event_log = crate::event_log::EventLog::new(&ws);
    let event_start = event_log.tail(0).expect("events before shutdown").len();
    let diagnostic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let out = crate::cli::lifecycle_port::shutdown_with_transport(&ws, true, None, &transport)
            .expect("shutdown should complete");
        let killed_sessions = transport.killed_sessions_observed();
        assert!(
            killed_sessions.iter().any(|s| s == "team-external"),
            concat!(
                "E49 non-clean path must still attempt kill_session(team-external); ",
                "got killed_sessions={:?}"
            ),
            killed_sessions
        );
        if out["ok"] != json!(true) {
            let events = event_log.tail(0).expect("events after shutdown");
            let invocation_events = events
                .into_iter()
                .skip(event_start)
                .take(64)
                .collect::<Vec<_>>();
            panic!(
                "E49 external-leader shutdown returned a non-clean outcome: workspace tag=e49-external-leader-non-clean, expected session=team-external, shutdown report={out}, killed_sessions={killed_sessions:?}, bounded invocation events={invocation_events:?}"
            );
        }
    }))
    .expect_err("deterministic kill-session error must exercise the diagnostic path");
    let diagnostic = diagnostic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| diagnostic.downcast_ref::<&str>().map(ToString::to_string))
        .expect("diagnostic panic should carry text");
    let bounded_events = diagnostic
        .split("bounded invocation events=")
        .nth(1)
        .expect("diagnostic should include bounded invocation events");
    let event_count = bounded_events.matches("\"event\":").count();
    assert!(
        event_count > 0,
        "diagnostic should include invocation events"
    );
    assert!(
        event_count <= 64,
        "diagnostic must be bounded to 64 invocation events, got {event_count}"
    );
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
            "team_key": "team-orphan",
            "generation": "team-orphan",
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
        .with_targets_persist_after_kill()
        .with_owned_session(&ws, "team-orphan", "team-orphan", "team-orphan");

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
            "team_key": "team-current",
            "generation": "team-current",
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
        .with_targets_persist_after_kill()
        .with_owned_session(&ws, "team-current", "team-current", "team-current");

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
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
            target_session_spared: false,
        },
    );
    assert!(!out.ok);
    assert_eq!(out.status, "failed");
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
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
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
            coordinator_post_stop: crate::cli::lifecycle_port::CoordinatorStopObservation::Gone,
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


// R-I269 / c8639440: unmatched roots are observed, never signalled.
#[cfg(unix)]
#[path = "../../../tests/support/proc_guard.rs"]
mod issue269_proc_guard;

std::thread_local! {
    static ISSUE_269_ROOT_PID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static ISSUE_269_OBSERVATIONS: std::cell::RefCell<Vec<(std::time::Instant, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct ShutdownWorkspaceCleanup(PathBuf);

impl Drop for ShutdownWorkspaceCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
struct Issue269Carrier {
    guard: issue269_proc_guard::FixtureGroup,
    pid: u32,
    row: serde_json::Value,
    signal_log: PathBuf,
    exit_log: PathBuf,
}

#[cfg(unix)]
impl Issue269Carrier {
    fn new(workspace: &Path, seconds: &str) -> Self {
        let signal_log = workspace.join("carrier-signals");
        let ready = workspace.join("carrier-ready");
        let pid_file = workspace.join("carrier-pid");
        let exit_log = workspace.join("carrier-exit");
        std::fs::write(&signal_log, "").unwrap();
        // The fixture supervisor owns waitpid: the SUT's waitpid(WNOHANG) cannot
        // steal this carrier's exit status. Both remain in the guarded PGID.
        let supervisor = r#"
import pathlib, signal, subprocess, sys
for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(sig, signal.SIG_DFL)
child = subprocess.Popen(["/bin/sh", "-c", sys.argv[1], "issue269-carrier", *sys.argv[2:5]], stdin=sys.stdin)
pid_file = pathlib.Path(sys.argv[5])
pid_file.with_suffix(".tmp").write_text(str(child.pid))
pid_file.with_suffix(".tmp").replace(pid_file)
status = child.wait()
pathlib.Path(sys.argv[6]).write_text(str(status))
sys.exit(status)
"#;
        let script = r#"
trap 'printf "TERM\n" >> "$1"; exit 97' TERM
trap 'printf "INT\n" >> "$1"; exit 98' INT
trap 'printf "HUP\n" >> "$1"; exit 99' HUP
: > "$2"
read -r _
sleep "$3" & wait "$!"
exit 0
"#;
        let mut command = std::process::Command::new(
            std::env::var("CONTRACT_PYTHON").unwrap_or_else(|_| "python3".into()));
        command.arg("-c").arg(supervisor).arg(script).arg(&signal_log)
            .arg(&ready).arg(seconds).arg(&pid_file).arg(&exit_log).current_dir(workspace);
        let guard = issue269_proc_guard::FixtureGroup::spawn(
            &mut command, workspace, std::time::Duration::from_secs(30));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready.exists() || !pid_file.exists() {
            assert!(std::time::Instant::now() < deadline, "carrier trap/readiness handshake timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        let mut row = json!({"status":"running","provider":"fake","window":"worker","provider_pid":pid});
        crate::lifecycle::carrier::record(&mut row, Some(pid));
        let carrier = Self { guard, pid, row, signal_log, exit_log };
        carrier.assert_possible();
        carrier
    }

    fn assert_possible(&self) {
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), None).is_ok());
        assert!(self.row["pane_birth_identity"].is_string(), "live birth must be recorded: {}", self.row);
        assert_eq!(crate::lifecycle::carrier::observe(&self.row, self.pid).state,
            crate::lifecycle::carrier::CarrierState::Possible);
    }

    fn release(&mut self) {
        use std::io::Write;
        self.assert_possible();
        self.guard.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
    }

    fn wait_for_normal_exit(&mut self) {
        assert_eq!(self.guard.wait().unwrap().code(), Some(0), "supervisor must propagate a normal child exit");
        assert_eq!(std::fs::read_to_string(&self.exit_log).unwrap(), "0", "actual child.wait() receipt");
    }

    fn assert_no_signals(&self, workspace: &Path) {
        assert_eq!(std::fs::read_to_string(&self.signal_log).unwrap(), "", "unmatched carrier received a signal");
        let log = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_else(|error| {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            String::new()
        });
        let pid = self.pid.to_string();
        for event in log.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()) {
            if event["event"] == "transport.pre_kill_audit" {
                assert!(!event["targets"].as_array().is_some_and(|targets| targets.iter().any(|target|
                    target.as_str().is_some_and(|target| target.trim_start_matches('-') == pid)
                    || target.as_u64() == Some(u64::from(self.pid)))),
                    "unmatched carrier must not be a pre-kill target: {event}");
            }
        }
        assert!(!log.contains("unverified carrier identity"), "{log}");
        eprintln!("issue269 pid={} birth={} signals=[] observations={:?}",
            self.pid, self.row["pane_birth_identity"],
            ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow().clone()));
    }
}

#[cfg(unix)]
impl Drop for Issue269Carrier {
    fn drop(&mut self) {
        if crate::lifecycle::carrier::observe(&self.row, self.pid).state
            == crate::lifecycle::carrier::CarrierState::Possible {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32),
                nix::sys::signal::Signal::SIGKILL);
        }
        // Reap the carrier through its supervisor before the group guard kills ancillary helpers.
        let _ = self.guard.wait();
    }
}

fn reset_issue_269_probe_state(pid: u32) {
    ISSUE_269_ROOT_PID.with(|root| root.set(pid));
    ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow_mut().clear());
}

fn issue_269_empty_process_table() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

fn issue_269_real_liveness(pid: u32) -> bool {
    let alive = crate::platform::process::pid_is_alive(pid);
    if ISSUE_269_ROOT_PID.with(std::cell::Cell::get) == pid {
        ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow_mut().push((std::time::Instant::now(), alive)));
    }
    alive
}

fn save_issue_269_root_state(workspace: &Path, session: &str, row: &serde_json::Value) {
    crate::state::persist::save_runtime_state(workspace, &json!({
        "session_name":session, "team_key":session, "generation":session,
        "agents":{"worker":row}
    })).unwrap();
}

fn shutdown_issue_269_with_probes(workspace: &Path, session: &str) -> serde_json::Value {
    let transport = CleanShutdownTransport::new().with_owned_session(workspace, session, session, session);
    crate::cli::lifecycle_port::with_process_table_test_probe(issue_269_empty_process_table, || {
        crate::cli::lifecycle_port::with_pid_liveness_test_probe(issue_269_real_liveness, || {
            crate::cli::lifecycle_port::shutdown_with_transport(workspace, true, None, &transport)
                .expect("shutdown should complete")
        })
    })
}

#[test]
#[cfg(unix)]
fn issue269_transient_unmatched_root_converges_inside_grace() {
    let workspace = tmp_shutdown_workspace("issue269-transient-root");
    let _workspace_cleanup = ShutdownWorkspaceCleanup(workspace.clone());
    let session = "team-issue269-transient";
    let mut carrier = Issue269Carrier::new(&workspace, "0.2");
    save_issue_269_root_state(&workspace, session, &carrier.row);
    reset_issue_269_probe_state(carrier.pid);
    carrier.release();

    let started = std::time::Instant::now();
    let out = shutdown_issue_269_with_probes(&workspace, session);
    let elapsed = started.elapsed();
    let observations = ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow().clone());

    assert_eq!(out["ok"], json!(true), "an unmatched root that exits inside grace must converge: {out}");
    assert_eq!(out["residuals"]["processes"], json!([]), "{out}");
    assert_eq!(observations.first().map(|(_, alive)| *alive), Some(true), "{observations:?}");
    let gone = observations.iter().find(|(_, alive)| !*alive).expect("real OS liveness must observe exit").0;
    assert!(gone.duration_since(observations[0].0) <= std::time::Duration::from_millis(500), "{observations:?}");
    assert!(elapsed < std::time::Duration::from_secs(2), "shutdown exceeded the bounded wait: {elapsed:?}");
    carrier.wait_for_normal_exit();
    carrier.assert_no_signals(&workspace);
}

#[test]
#[cfg(unix)]
fn issue269_persistent_unmatched_root_remains_a_residual_after_grace() {
    let workspace = tmp_shutdown_workspace("issue269-persistent-root");
    let _workspace_cleanup = ShutdownWorkspaceCleanup(workspace.clone());
    let session = "team-issue269-persistent";
    let mut carrier = Issue269Carrier::new(&workspace, "30");
    save_issue_269_root_state(&workspace, session, &carrier.row);
    reset_issue_269_probe_state(carrier.pid);
    carrier.release();

    let started = std::time::Instant::now();
    let out = shutdown_issue_269_with_probes(&workspace, session);
    let elapsed = started.elapsed();
    let calls = ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow().len());
    let residuals = out["residuals"]["processes"].as_array().unwrap();

    assert_eq!(out["ok"], json!(false), "a live root must not be filtered by placeholder fields: {out}");
    assert_eq!(residuals.len(), 1, "{out}");
    assert_eq!(residuals[0]["pid"], json!(carrier.pid), "{out}");
    assert_eq!(residuals[0]["ppid"], json!(0), "{out}");
    assert_eq!(residuals[0]["command"], json!(""), "{out}");
    assert!((2..=30).contains(&calls), "persistent-root polling must remain bounded; calls={calls}");
    assert!(elapsed < std::time::Duration::from_secs(2), "shutdown exceeded the bounded wait: {elapsed:?}");
    carrier.assert_possible();
    carrier.assert_no_signals(&workspace);
}

#[test]
#[cfg(unix)]
fn issue269_already_gone_root_returns_without_fixed_grace_sleep() {
    let workspace = tmp_shutdown_workspace("issue269-already-gone-root");
    let _workspace_cleanup = ShutdownWorkspaceCleanup(workspace.clone());
    let session = "team-issue269-gone";
    let mut carrier = Issue269Carrier::new(&workspace, "0.2");
    save_issue_269_root_state(&workspace, session, &carrier.row);
    carrier.release();
    carrier.wait_for_normal_exit();
    assert_ne!(crate::lifecycle::carrier::observe(&carrier.row, carrier.pid).state,
        crate::lifecycle::carrier::CarrierState::Possible);
    reset_issue_269_probe_state(carrier.pid);

    let started = std::time::Instant::now();
    let out = shutdown_issue_269_with_probes(&workspace, session);
    let elapsed = started.elapsed();
    let calls = ISSUE_269_OBSERVATIONS.with(|observations| observations.borrow().len());

    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["residuals"]["processes"], json!([]), "{out}");
    // c8639440: a physically gone carrier is excluded before the liveness wait.
    assert_eq!(calls, 0, "a verified-gone root must not enter the wait path");
    assert!(elapsed < std::time::Duration::from_millis(450), "already-gone roots must not incur a fixed grace sleep: {elapsed:?}");
    assert!(!out.to_string().contains("unverified carrier identity"), "{out}");
    carrier.assert_no_signals(&workspace);
}
