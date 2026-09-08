use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

fn brief_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ta-status-brief-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join(".team").join("runtime")).unwrap();
    dir
}

fn write_state(ws: &Path, state: &Value) {
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec_pretty(state).unwrap(),
    )
    .unwrap();
}

fn production_agent(id: &str, pane: &str) -> Value {
    json!({
        "status": "running",
        "provider": "pi",
        "agent_id": id,
        "window": id,
        "layout_window": id,
        "pane_id": pane,
        "display": {
            "backend": "adaptive",
            "status": "opened",
            "window": id,
            "pane_id": pane,
            "target_worker_session": "team-demo",
            "linked_session": null,
            "display_session": null
        }
    })
}

fn production_state(agents: Value) -> Value {
    json!({
        "session_name": "team-demo",
        "tmux_endpoint": "/tmp/ta-status-brief.sock",
        "tmux_socket": "/tmp/ta-status-brief.sock",
        "active_team_key": "demo",
        "agents": agents
    })
}

fn status_args(ws: &Path, json_out: bool, agent: Option<&str>, team: Option<&str>) -> StatusArgs {
    StatusArgs {
        agent: agent.map(str::to_string),
        workspace: ws.to_path_buf(),
        detail: false,
        summary: false,
        json: json_out,
        team: team.map(str::to_string),
    }
}

fn json_nodes(result: CmdResult) -> Vec<Value> {
    match result.output {
        CmdOutput::Json(value) => value
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        other => panic!("expected JSON nodes, got {other:?}"),
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                walk(&path, root, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

#[cfg(unix)]
fn producer_report(socket: &str, window: &str, pane: &str, method: &str) -> String {
    format!(
        r#"{{"schema_version":1,"socket":"{socket}","sampled_at":"2026-01-01T00:00:00Z","nodes":[{{"socket":"{socket}","workspace_path":"/ws","project_name":"demo","session":"team-demo","window_index":1,"window_name":"{window}","pane_id":"{pane}","name":"{window}","provider":"pi","state":"idle","activity":"idle","session_name":"pi-session","health":"normal","background_tasks":{{"running":0}},"evidence":{{"method":"{method}","detail":"channel"}}}}]}}"#
    )
}

#[cfg(unix)]
fn write_nodeprobe(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("nodeprobe");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn status_run(ws: &Path, extra: &[&str]) -> ExitCode {
    let mut argv = vec![
        "status".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().to_string(),
    ];
    argv.extend(extra.iter().map(|item| item.to_string()));
    run(&argv, Path::new("."))
}

#[cfg(unix)]
#[test]
fn cmd_status_from_production_registration_and_report() {
    let ws = brief_workspace("prod");
    write_state(
        &ws,
        &production_state(json!({ "worker": production_agent("worker", "%7") })),
    );
    let scratch = ws.join("probe");
    let report = producer_report("/tmp/ta-status-brief.sock", "worker", "%7", "pi_activity_channel");
    let log = scratch.join("calls.log");
    let bin = write_nodeprobe(
        &scratch,
        &format!(
            "printf '%s %s\\n' \"$1\" \"$2\" >> '{}'\ncat <<'EOF'\n{}\nEOF\n",
            log.display(),
            report
        ),
    );
    let result = status_port::with_test_nodeprobe(bin, || {
        cmd_status(&status_args(&ws, true, None, None)).expect("status")
    });
    let nodes = json_nodes(result);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["name"], "worker");
    assert_eq!(nodes[0]["provider"], "pi");
    assert_eq!(nodes[0]["runtime_status"], "running");
    assert_eq!(nodes[0]["activity"], "idle");
    assert_eq!(nodes[0]["health"], "normal");
    assert_eq!(nodes[0]["session_name"], "pi-session");
    assert_eq!(
        nodes[0]["tmux_command"],
        "tmux -S '/tmp/ta-status-brief.sock' attach -t 'team-demo:worker.%7'"
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(calls.contains("-S /tmp/ta-status-brief.sock"), "{calls}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
fn cmd_status_dedupes_same_endpoint_across_agents() {
    let ws = brief_workspace("dedupe");
    write_state(
        &ws,
        &production_state(json!({
            "alpha": production_agent("alpha", "%7"),
            "beta": production_agent("beta", "%8"),
        })),
    );
    let scratch = ws.join("probe");
    let log = scratch.join("calls.log");
    let report_alpha = producer_report("/tmp/ta-status-brief.sock", "alpha", "%7", "pi_activity_channel");
    let bin = write_nodeprobe(
        &scratch,
        &format!(
            "printf '%s %s\\n' \"$1\" \"$2\" >> '{}'\ncat <<'EOF'\n{}\nEOF\n",
            log.display(),
            report_alpha
        ),
    );
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    assert_eq!(nodes.len(), 2);
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn cmd_status_is_readonly_on_legacy_and_multi_team_state() {
    let ws = brief_workspace("readonly");
    let legacy = json!({
        "session_name": "sess",
        "team_dir": format!("{}/.team/tk", ws.display()),
        "agents": {
            "w1": {
                "agent_id": "w1",
                "status": "running",
                "provider": "pi"
            }
        },
        "team_owner": {"pane_id": "%1", "machine_fingerprint": "fp"}
    });
    write_state(&ws, &legacy);
    let before = snapshot_tree(&ws);
    let result = cmd_status(&status_args(&ws, true, None, None)).expect("status");
    let after = snapshot_tree(&ws);
    assert_eq!(before, after, "status must not migrate or write runtime state");
    assert_eq!(json_nodes(result).len(), 1);
    let pid_path = crate::coordinator::coordinator_pid_path(&crate::coordinator::WorkspacePath::new(
        ws.clone(),
    ));
    assert!(!pid_path.exists(), "status must not spawn a coordinator");

    let multi = brief_workspace("readonly-multi");
    write_state(
        &multi,
        &json!({
            "teams": {
                "alpha": {"status": "alive", "agents": {"a": {"status": "running", "provider": "pi"}}},
                "beta": {"status": "alive", "agents": {"b": {"status": "running", "provider": "pi"}}}
            }
        }),
    );
    let before_multi = snapshot_tree(&multi);
    let err = cmd_status(&status_args(&multi, true, None, None)).unwrap_err();
    assert!(
        err.to_string().contains("multiple alive teams"),
        "{err}"
    );
    assert_eq!(before_multi, snapshot_tree(&multi));
    let chosen = cmd_status(&status_args(&multi, true, None, Some("alpha"))).expect("team");
    assert_eq!(json_nodes(chosen).len(), 1);
    assert_eq!(before_multi, snapshot_tree(&multi));
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&multi);
}

#[test]
fn cmd_status_human_and_json_fail_on_selector_errors() {
    let multi = brief_workspace("ambiguous");
    write_state(
        &multi,
        &json!({
            "teams": {
                "alpha": {"status": "alive", "agents": {"a": {"status": "running", "provider": "pi"}}},
                "beta": {"status": "alive", "agents": {"b": {"status": "running", "provider": "pi"}}}
            }
        }),
    );
    for json_out in [false, true] {
        let err = cmd_status(&status_args(&multi, json_out, None, None)).unwrap_err();
        assert!(err.to_string().contains("multiple alive teams"), "{err}");
    }
    assert_ne!(status_run(&multi, &[]), ExitCode::Ok);
    assert_ne!(status_run(&multi, &["--json"]), ExitCode::Ok);

    let one = brief_workspace("one");
    write_state(
        &one,
        &production_state(json!({ "worker": production_agent("worker", "%7") })),
    );
    for json_out in [false, true] {
        let err = cmd_status(&status_args(&one, json_out, Some("ghost"), None)).unwrap_err();
        assert!(err.to_string().contains("unknown agent id: ghost"), "{err}");
        let err = cmd_status(&status_args(&one, json_out, None, Some("missing"))).unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("missing"),
            "{err}"
        );
    }
    assert_ne!(status_run(&one, &["ghost"]), ExitCode::Ok);
    assert_ne!(status_run(&one, &["ghost", "--json"]), ExitCode::Ok);

    let bad = brief_workspace("bad-state");
    std::fs::write(
        bad.join(".team").join("runtime").join("state.json"),
        b"{not-json",
    )
    .unwrap();
    for json_out in [false, true] {
        assert!(cmd_status(&status_args(&bad, json_out, None, None)).is_err());
    }
    let before_bad = snapshot_tree(&bad);
    assert_ne!(status_run(&bad, &[]), ExitCode::Ok);
    assert_eq!(before_bad, snapshot_tree(&bad));
    assert_ne!(status_run(&bad, &["--json"]), ExitCode::Ok);
    assert_eq!(before_bad, snapshot_tree(&bad));

    let missing = std::env::temp_dir()
        .join(format!(
            "ta-status-brief-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("nested");
    for json_out in [false, true] {
        let err = cmd_status(&status_args(&missing, json_out, None, None)).unwrap_err();
        assert!(
            err.to_string().contains("invalid workspace"),
            "{err}"
        );
    }
    assert_ne!(status_run(&missing, &[]), ExitCode::Ok);
    assert_ne!(status_run(&missing, &["--json"]), ExitCode::Ok);
    assert!(!missing.exists(), "status errors must not create workspace paths");

    let _ = std::fs::remove_dir_all(&multi);
    let _ = std::fs::remove_dir_all(&one);
    let _ = std::fs::remove_dir_all(&bad);
}

#[cfg(unix)]
#[test]
fn cmd_status_does_not_spawn_unbound_or_mismatched_nodeprobe() {
    let cases = [
        ("missing-receipt", None, None),
        ("wrong-hash", Some(("binary_sha256", json!("0".repeat(64)))), None),
        ("wrong-source", Some(("source_commit", json!("0".repeat(40)))), None),
        ("wrong-target", Some(("target", json!("unknown-target"))), None),
        (
            "wrong-capability",
            Some(("capabilities", json!(["tmux.capture-pane"]))),
            None,
        ),
        ("replaced-binary", None, Some("replace")),
    ];
    for (tag, mutation, replacement) in cases {
        let ws = brief_workspace(tag);
        write_state(
            &ws,
            &production_state(json!({ "worker": production_agent("worker", "%7") })),
        );
        let scratch = ws.join("probe");
        let log = scratch.join("spawned.log");
        let bin = write_nodeprobe(
            &scratch,
            &format!("printf spawned >> '{}'\n", log.display()),
        );
        let run = || {
            let nodes = json_nodes(
                cmd_status(&status_args(&ws, true, None, None)).expect("status"),
            );
            assert_eq!(nodes[0]["runtime_status"], "unknown", "{tag}");
            assert!(!log.exists(), "{tag} must not spawn an unbound probe");
        };
        if mutation.is_none() && replacement.is_none() {
            status_port::with_test_nodeprobe_unbound(bin, run);
        } else {
            status_port::with_test_nodeprobe(bin.clone(), || {
                if let Some((field, value)) = mutation {
                    let receipt = scratch.join("nodeprobe.capability.json");
                    let mut body: Value = serde_json::from_slice(
                        &std::fs::read(&receipt).unwrap(),
                    )
                    .unwrap();
                    body[field] = value;
                    std::fs::write(&receipt, serde_json::to_vec(&body).unwrap()).unwrap();
                }
                if replacement.is_some() {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&bin)
                        .unwrap()
                        .write_all(b"# replaced\n")
                        .unwrap();
                }
                run();
            });
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(unix)]
#[test]
fn cmd_status_rejects_producer_error_and_wrong_socket_as_unknown() {
    let cases = [
        (
            "exit0-error-nodes",
            0,
            r#"{"schema_version":1,"socket":"/tmp/ta-status-brief.sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[{"socket":"/tmp/ta-status-brief.sock","session":"team-demo","window_name":"worker","pane_id":"%7","provider":"pi","activity":"idle","health":"normal","evidence":{"method":"pi_activity_channel"}}],"error":{"kind":"corpus_error","message":"providers corpus failed"}}"#,
        ),
        (
            "exit1-error-empty",
            1,
            r#"{"schema_version":1,"socket":"/tmp/ta-status-brief.sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[],"error":{"kind":"tmux_inventory","message":"missing"}}"#,
        ),
        (
            "exit1-valid",
            1,
            r#"{"schema_version":1,"socket":"/tmp/ta-status-brief.sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[{"socket":"/tmp/ta-status-brief.sock","session":"team-demo","window_name":"worker","pane_id":"%7","provider":"pi","activity":"idle","health":"normal","evidence":{"method":"pi_activity_channel"}}]}"#,
        ),
        (
            "wrong-socket",
            0,
            r#"{"schema_version":1,"socket":"/tmp/other.sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[{"socket":"/tmp/other.sock","session":"team-demo","window_name":"worker","pane_id":"%7","provider":"pi","activity":"idle","health":"normal","evidence":{"method":"pi_activity_channel"}}]}"#,
        ),
    ];
    for (tag, code, payload) in cases {
        let ws = brief_workspace(tag);
        write_state(
            &ws,
            &production_state(json!({ "worker": production_agent("worker", "%7") })),
        );
        let scratch = ws.join("probe");
        let bin = write_nodeprobe(
            &scratch,
            &format!("cat <<'EOF'\n{payload}\nEOF\nexit {code}\n"),
        );
        let nodes = status_port::with_test_nodeprobe(bin, || {
            json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
        });
        assert_eq!(nodes[0]["runtime_status"], "unknown", "{tag}");
        assert_eq!(nodes[0]["activity"], "unknown", "{tag}");
        assert!(nodes[0]["tmux_command"].is_null(), "{tag}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(unix)]
#[test]
fn cmd_status_zero_match_and_stopped_conflict_stay_unknown() {
    let ws = brief_workspace("zero-match");
    write_state(
        &ws,
        &production_state(json!({ "worker": production_agent("worker", "%7") })),
    );
    let miss = producer_report("/tmp/ta-status-brief.sock", "worker", "%9", "pi_activity_channel");
    let bin = write_nodeprobe(&ws.join("probe"), &format!("cat <<'EOF'\n{miss}\nEOF\n"));
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "unknown");

    let conflict = brief_workspace("stopped-live");
    let mut agent = production_agent("worker", "%7");
    agent["status"] = json!("stopped");
    write_state(&conflict, &production_state(json!({ "worker": agent })));
    let live = producer_report(
        "/tmp/ta-status-brief.sock",
        "worker",
        "%7",
        "pi_activity_channel",
    );
    let bin = write_nodeprobe(&conflict.join("probe"), &format!("cat <<'EOF'\n{live}\nEOF\n"));
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&conflict, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "unknown");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&conflict);
}

#[cfg(unix)]
#[test]
fn cmd_status_shared_deadline_reaps_inherited_stdout_child() {
    let ws = brief_workspace("hang");
    write_state(
        &ws,
        &production_state(json!({ "worker": production_agent("worker", "%7") })),
    );
    let pid_file = ws.join("probe").join("child.pid");
    let bin = write_nodeprobe(
        &ws.join("probe"),
        &format!(
            "/bin/sleep 12 &\nprintf '%s\\n' \"$!\" > '{}'\nexit 0\n",
            pid_file.display()
        ),
    );
    let started = Instant::now();
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    let elapsed = started.elapsed();
    assert_eq!(nodes[0]["runtime_status"], "unknown");
    assert!(
        elapsed < Duration::from_secs(3),
        "shared deadline exceeded: {elapsed:?}"
    );
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline && pid_alive(pid) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!pid_alive(pid), "inherited stdout child {pid} was not reaped");
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
fn cmd_status_multi_endpoint_uses_total_budget() {
    let ws = brief_workspace("budget");
    let mut beta = production_agent("beta", "%8");
    beta["tmux_endpoint"] = json!("/tmp/ta-status-brief-b.sock");
    write_state(
        &ws,
        &json!({
            "session_name": "team-demo",
            "tmux_endpoint": "/tmp/ta-status-brief.sock",
            "agents": {
                "alpha": production_agent("alpha", "%7"),
                "beta": beta
            }
        }),
    );
    let bin = write_nodeprobe(&ws.join("probe"), "/bin/sleep 5\nexit 0\n");
    let started = Instant::now();
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    let elapsed = started.elapsed();
    assert_eq!(nodes.len(), 2);
    assert!(
        elapsed < Duration::from_secs(3),
        "total probe budget exceeded: {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
