use super::*;
use serde_json::{json, Value};
use serial_test::serial;
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

struct BriefWorkspaceGuard(PathBuf);

impl std::ops::Deref for BriefWorkspaceGuard {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for BriefWorkspaceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn guarded_brief_workspace(tag: &str) -> BriefWorkspaceGuard {
    BriefWorkspaceGuard(brief_workspace(tag))
}

fn status_agent(
    name: &str,
    pane: &str,
    endpoint: &str,
    session: &str,
    model: &str,
    effort: &str,
) -> Value {
    let mut agent = production_agent(name, pane);
    agent["tmux_endpoint"] = json!(endpoint);
    agent["session_name"] = json!(session);
    agent["model"] = json!(model);
    agent["effort"] = json!(effort);
    agent
}

#[cfg(unix)]
fn producer_report_nodes(socket: &str, rows: &[(String, String, String, String)]) -> String {
    let nodes = rows
        .iter()
        .map(|(session, window, pane, native_session)| {
            json!({
                "socket": socket,
                "workspace_path": "/ws",
                "project_name": "demo",
                "session": session,
                "window_index": 1,
                "window_name": window,
                "pane_id": pane,
                "name": window,
                "provider": "pi",
                "state": "idle",
                "activity": "idle",
                "session_name": native_session,
                "health": "normal",
                "background_tasks": {"running": 0},
                "evidence": {"method": "pi_activity_channel", "detail": "channel"}
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&json!({
        "schema_version": 1,
        "socket": socket,
        "sampled_at": "2026-01-01T00:00:00Z",
        "nodes": nodes
    }))
    .unwrap()
}

#[cfg(unix)]
fn write_nodeprobe_reports(dir: &Path, reports: &[(String, String)]) -> (PathBuf, PathBuf) {
    let log = dir.join("calls.log");
    let mut body = format!(
        "printf '%s\\t%s\\n' \"$1\" \"$2\" >> {}\ncase \"$2\" in\n",
        shell_quote_for_status_test(&log.to_string_lossy())
    );
    for (index, (endpoint, report)) in reports.iter().enumerate() {
        let path = dir.join(format!("report-{index}.json"));
        std::fs::write(&path, report).unwrap();
        body.push_str(&format!(
            "  {} ) cat {} ;;\n",
            shell_quote_for_status_test(endpoint),
            shell_quote_for_status_test(&path.to_string_lossy())
        ));
    }
    body.push_str("  *) exit 2 ;;\nesac\n");
    (write_nodeprobe(dir, &body), log)
}

fn shell_quote_for_status_test(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn logged_probe_calls(log: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| {
            let (flag, endpoint) = line.split_once('\t').expect("probe call fields");
            (flag.to_string(), endpoint.to_string())
        })
        .collect()
}

fn human_text(result: CmdResult) -> String {
    match result.output {
        CmdOutput::Human(text) => text,
        other => panic!("expected human status output, got {other:?}"),
    }
}

fn legacy_human_node(node: &Value) -> String {
    let text = |key: &str| {
        node.get(key)
            .and_then(Value::as_str)
            .unwrap_or("null")
            .to_string()
    };
    format!(
        "name: {} provider: {} model: {} effort: {} runtime_status: {} activity: {} health: {} session_name: {} tmux_command: {}",
        text("name"),
        text("provider"),
        text("model"),
        text("effort"),
        text("runtime_status"),
        text("activity"),
        text("health"),
        text("session_name"),
        text("tmux_command")
    )
}

fn human_token_count(text: &str, expected: &str) -> usize {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| token.eq_ignore_ascii_case(expected))
        .count()
}

fn status_table_header(text: &str) -> Option<&str> {
    text.lines().find(|line| {
        !line.trim_start().starts_with("name:")
            && [
                "name",
                "provider",
                "model",
                "effort",
                "activity",
                "health",
                "session_name",
            ]
            .iter()
            .all(|field| human_token_count(line, field) == 1)
            && human_token_count(line, "runtime_status") + human_token_count(line, "runtime")
                == 1
            && human_token_count(line, "tmux_command") + human_token_count(line, "attach") == 1
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

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
    let result = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo {}: {}", path.display(), std::io::Error::last_os_error());
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
#[serial(status_brief)]
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
#[serial(status_brief)]
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

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn issue271_status_human_dedupes_six_nodes_and_preserves_json() {
    let ws = guarded_brief_workspace("issue271-six");
    let endpoint = "/tmp/271 shared.sock";
    let session = "team-271";
    let mut agents = serde_json::Map::new();
    let mut rows = Vec::new();
    for index in 1..=6 {
        let name = format!("worker-{index}");
        let pane = format!("%{}", index + 20);
        agents.insert(
            name.clone(),
            status_agent(
                &name,
                &pane,
                endpoint,
                session,
                &format!("gpt-5.6-luna-{index}"),
                if index % 2 == 0 { "high" } else { "max" },
            ),
        );
        rows.push((
            session.to_string(),
            name.clone(),
            pane,
            format!("native-{name}"),
        ));
    }
    write_state(
        &ws,
        &json!({
            "session_name": session,
            "tmux_endpoint": endpoint,
            "active_team_key": "demo",
            "agents": agents
        }),
    );
    let report = producer_report_nodes(endpoint, &rows);
    let (bin, log) = write_nodeprobe_reports(
        &ws.join("probe"),
        &[(endpoint.to_string(), report)],
    );
    let nodes = status_port::with_test_nodeprobe(bin.clone(), || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("JSON status"))
    });
    assert_eq!(nodes.len(), 6);
    for node in &nodes {
        assert_eq!(node.as_object().unwrap().len(), 9);
        for key in [
            "name",
            "provider",
            "model",
            "effort",
            "runtime_status",
            "activity",
            "health",
            "session_name",
            "tmux_command",
        ] {
            assert!(node.get(key).is_some(), "missing JSON field {key}");
        }
        let name = node["name"].as_str().unwrap();
        let index = name.strip_prefix("worker-").unwrap().parse::<u32>().unwrap();
        let pane = index + 20;
        assert_eq!(node["provider"], "pi");
        assert_eq!(node["model"], format!("gpt-5.6-luna-{index}"));
        assert_eq!(node["effort"], if index % 2 == 0 { "high" } else { "max" });
        assert_eq!(node["runtime_status"], "running");
        assert_eq!(node["activity"], "idle");
        assert_eq!(node["health"], "normal");
        assert_eq!(node["session_name"], format!("native-{name}"));
        let target = format!("{session}:{name}.%{pane}");
        assert_eq!(
            node["tmux_command"],
            json!(format!(
                "tmux -S {} attach -t {}",
                shell_quote_for_status_test(endpoint),
                shell_quote_for_status_test(&target)
            ))
        );
    }
    let before = nodes
        .iter()
        .map(legacy_human_node)
        .collect::<Vec<_>>()
        .join("\n");
    let human = status_port::with_test_nodeprobe(bin, || {
        human_text(cmd_status(&status_args(&ws, false, None, None)).expect("human status"))
    });
    let prefix = format!("tmux -S {}", shell_quote_for_status_test(endpoint));
    let prefix_count = human.matches(&prefix).count();
    let labels_once = status_table_header(&human).is_some();
    let targets_once = nodes.iter().all(|node| {
        let name = node["name"].as_str().unwrap();
        let index = name.strip_prefix("worker-").unwrap().parse::<u32>().unwrap();
        let pane = index + 20;
        let target = format!("{name}.%{pane}");
        let rows = human.lines().filter(|line| line.contains(&target)).collect::<Vec<_>>();
        rows.len() == 1 && rows[0].contains("1:")
    });
    let calls = logged_probe_calls(&log);
    assert_eq!(calls.len(), 2, "one native query per status invocation: {calls:?}");
    assert!(
        calls.iter().all(|(flag, called)| flag == "-S" && called == endpoint),
        "{calls:?}"
    );
    assert!(
        prefix_count == 1 && labels_once && targets_once && human.len() * 4 < before.len() * 3,
        "six-node human output must dedupe shared text and retain targets; prefix_count={prefix_count}, labels_once={labels_once}, targets_once={targets_once}, before_bytes={}, after_bytes={}\n{human}",
        before.len(),
        human.len()
    );
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn issue271_status_human_groups_socket_session_pairs_without_cross_routing() {
    let ws = guarded_brief_workspace("issue271-groups");
    let endpoint_a = "/tmp/271 socket 'α.sock";
    let endpoint_b = "shared named socket";
    let session_red = "red team α";
    let session_blue = "blue's team";
    let specs = [
        ("a-red", endpoint_a, session_red, "%31", 1),
        ("b-red", endpoint_a, session_red, "%32", 1),
        ("c-other", endpoint_a, session_blue, "%33", 2),
        ("d-named", endpoint_b, session_red, "%34", 3),
        ("e-named", endpoint_b, session_red, "%35", 3),
    ];
    let mut agents = serde_json::Map::new();
    let mut rows_a = Vec::new();
    let mut rows_b = Vec::new();
    for &(name, endpoint, session, pane, _) in &specs {
        agents.insert(
            name.to_string(),
            status_agent(name, pane, endpoint, session, "model", "high"),
        );
        let row = (
            session.to_string(),
            name.to_string(),
            pane.to_string(),
            format!("native-{name}"),
        );
        if endpoint == endpoint_a {
            rows_a.push(row);
        } else {
            rows_b.push(row);
        }
    }
    write_state(
        &ws,
        &json!({
            "session_name": session_red,
            "tmux_endpoint": endpoint_a,
            "active_team_key": "demo",
            "agents": agents
        }),
    );
    let reports = [
        (
            endpoint_a.to_string(),
            producer_report_nodes(endpoint_a, &rows_a),
        ),
        (
            endpoint_b.to_string(),
            producer_report_nodes(endpoint_b, &rows_b),
        ),
    ];
    let (bin, log) = write_nodeprobe_reports(&ws.join("probe"), &reports);
    let nodes = status_port::with_test_nodeprobe(bin.clone(), || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("JSON status"))
    });
    assert_eq!(nodes.len(), specs.len());
    for node in &nodes {
        let name = node["name"].as_str().unwrap();
        let (_, endpoint, session, pane, _) =
            *specs.iter().find(|spec| spec.0 == name).unwrap();
        let target = format!("{session}:{name}.{pane}");
        assert_eq!(
            node["tmux_command"],
            json!(format!(
                "tmux {} {} attach -t {}",
                if Path::new(endpoint).is_absolute() { "-S" } else { "-L" },
                shell_quote_for_status_test(endpoint),
                shell_quote_for_status_test(&target)
            ))
        );
    }
    let human = status_port::with_test_nodeprobe(bin, || {
        human_text(cmd_status(&status_args(&ws, false, None, None)).expect("human status"))
    });
    let abs_prefix = format!("tmux -S {}", shell_quote_for_status_test(endpoint_a));
    let named_prefix = format!("tmux -L {}", shell_quote_for_status_test(endpoint_b));
    let calls = logged_probe_calls(&log);
    assert_eq!(calls.len(), 4, "one probe per endpoint per invocation: {calls:?}");
    assert_eq!(calls.iter().filter(|(_, value)| value == endpoint_a).count(), 2);
    assert_eq!(calls.iter().filter(|(_, value)| value == endpoint_b).count(), 2);
    assert!(calls.iter().all(|(flag, value)| {
        (value == endpoint_a && flag == "-S") || (value == endpoint_b && flag == "-L")
    }));
    let expected_groups = [
        ("a-red", "%31", 1),
        ("b-red", "%32", 1),
        ("c-other", "%33", 2),
        ("d-named", "%34", 3),
        ("e-named", "%35", 3),
    ];
    let rows_routed = expected_groups.iter().all(|(name, pane, group)| {
        let target = format!("{name}.{pane}");
        let matching = human
            .lines()
            .filter(|line| line.contains(&target))
            .collect::<Vec<_>>();
        matching.len() == 1 && matching[0].contains(&format!("{group}:"))
    });
    assert!(
        human.matches(&abs_prefix).count() == 2
            && human.matches(&named_prefix).count() == 1
            && human.matches(session_red).count() == 2
            && human.matches("blue").count() == 1
            && rows_routed,
        "each (endpoint, session) needs one stable group and its own targets; abs={}, named={}, red={}, blue={}, rows_routed={rows_routed}\n{human}",
        human.matches(&abs_prefix).count(),
        human.matches(&named_prefix).count(),
        human.matches(session_red).count(),
        human.matches("blue").count()
    );
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn issue271_status_single_empty_unknown_and_stopped_compatibility() {
    let endpoint = "/tmp/271-single.sock";
    let single = guarded_brief_workspace("issue271-single");
    write_state(
        &single,
        &json!({
            "session_name": "team-single",
            "tmux_endpoint": endpoint,
            "agents": {
                "solo": status_agent(
                    "solo",
                    "%7",
                    endpoint,
                    "team-single",
                    "gpt-5.6-luna-xhigh",
                    "high"
                )
            }
        }),
    );
    let single_report = producer_report_nodes(
        endpoint,
        &[(
            "team-single".to_string(),
            "solo".to_string(),
            "%7".to_string(),
            "pi-native-solo".to_string(),
        )],
    );
    let (single_bin, single_log) =
        write_nodeprobe_reports(&single.join("probe"), &[(endpoint.to_string(), single_report)]);
    let single_nodes = status_port::with_test_nodeprobe(single_bin.clone(), || {
        json_nodes(cmd_status(&status_args(&single, true, None, None)).expect("single JSON"))
    });
    let single_human = status_port::with_test_nodeprobe(single_bin, || {
        human_text(cmd_status(&status_args(&single, false, None, None)).expect("single human"))
    });
    assert_eq!(single_nodes.len(), 1);
    assert_eq!(single_human, legacy_human_node(&single_nodes[0]));
    assert_eq!(logged_probe_calls(&single_log).len(), 2);
    let unknown_agent = cmd_status(&status_args(&single, false, Some("ghost"), None))
        .unwrap_err()
        .to_string();
    assert!(unknown_agent.contains("unknown agent id: ghost"), "{unknown_agent}");

    let empty = guarded_brief_workspace("issue271-empty");
    write_state(&empty, &production_state(json!({})));
    assert!(
        human_text(cmd_status(&status_args(&empty, false, None, None)).expect("empty")).is_empty()
    );

    let mixed = guarded_brief_workspace("issue271-mixed");
    let mut agents = serde_json::Map::new();
    agents.insert(
        "worker".to_string(),
        status_agent("worker", "%7", endpoint, "team-mixed", "gpt-worker", "max"),
    );
    agents.insert(
        "unknown".to_string(),
        status_agent("unknown", "%8", endpoint, "team-mixed", "gpt-unknown", "high"),
    );
    let mut stopped = status_agent(
        "stopped",
        "%9",
        endpoint,
        "team-mixed",
        "gpt-stopped",
        "low",
    );
    stopped["status"] = json!("stopped");
    agents.insert("stopped".to_string(), stopped);
    write_state(
        &mixed,
        &json!({
            "session_name": "team-mixed",
            "tmux_endpoint": endpoint,
            "agents": agents
        }),
    );
    let mixed_report = producer_report_nodes(
        endpoint,
        &[(
            "team-mixed".to_string(),
            "worker".to_string(),
            "%7".to_string(),
            "pi-native-worker".to_string(),
        )],
    );
    let (mixed_bin, mixed_log) =
        write_nodeprobe_reports(&mixed.join("probe"), &[(endpoint.to_string(), mixed_report)]);
    let mixed_nodes = status_port::with_test_nodeprobe(mixed_bin.clone(), || {
        json_nodes(cmd_status(&status_args(&mixed, true, None, None)).expect("mixed JSON"))
    });
    let node = |name: &str| mixed_nodes.iter().find(|node| node["name"] == name).unwrap();
    assert_eq!(node("worker")["runtime_status"], "running");
    assert_eq!(node("worker")["session_name"], "pi-native-worker");
    assert_eq!(node("unknown")["runtime_status"], "unknown");
    assert_eq!(node("unknown")["model"], "gpt-unknown");
    assert_eq!(node("unknown")["effort"], "high");
    assert!(node("unknown")["tmux_command"].is_null());
    assert!(node("unknown")["session_name"].is_null());
    assert_eq!(node("stopped")["runtime_status"], "stopped");
    assert_eq!(node("stopped")["model"], "gpt-stopped");
    assert_eq!(node("stopped")["effort"], "low");
    assert!(node("stopped")["tmux_command"].is_null());
    let mixed_human = status_port::with_test_nodeprobe(mixed_bin.clone(), || {
        human_text(cmd_status(&status_args(&mixed, false, None, None)).expect("mixed human"))
    });
    let agent_human = status_port::with_test_nodeprobe(mixed_bin, || {
        human_text(
            cmd_status(&status_args(&mixed, false, Some("worker"), None))
                .expect("status AGENT"),
        )
    });
    assert_eq!(agent_human, legacy_human_node(node("worker")));
    let calls = logged_probe_calls(&mixed_log);
    assert_eq!(calls.len(), 3, "one native query per status invocation");
    assert!(
        status_table_header(&mixed_human).is_some()
            && mixed_human
                .matches(&format!("tmux -S {}", shell_quote_for_status_test(endpoint)))
                .count()
                == 1,
        "mixed output must retain one shared header and only the observed worker target:\n{mixed_human}"
    );
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn issue271_status_human_escapes_fields_and_quotes_shared_template() {
    let ws = guarded_brief_workspace("issue271-escape");
    let endpoint = "/tmp/271 space '雪.sock";
    let session = "team '雪";
    let window_a = "win '甲";
    let window_b = "win β";
    let mut agent_a = status_agent(
        "node-a",
        "%51",
        endpoint,
        session,
        "gpt\tα\nmax",
        "xhigh\nhigh",
    );
    agent_a["window"] = json!(window_a);
    let mut agent_b = status_agent(
        "node-b",
        "%52",
        endpoint,
        session,
        "grok\tβ\nxhigh",
        "high\tmax\nlow",
    );
    agent_b["window"] = json!(window_b);
    write_state(
        &ws,
        &json!({
            "session_name": session,
            "tmux_endpoint": endpoint,
            "agents": {"node-a": agent_a, "node-b": agent_b}
        }),
    );
    let report = producer_report_nodes(
        endpoint,
        &[
            (
                session.to_string(),
                window_a.to_string(),
                "%51".to_string(),
                "pi\tα\nchannel".to_string(),
            ),
            (
                session.to_string(),
                window_b.to_string(),
                "%52".to_string(),
                "pi β native".to_string(),
            ),
        ],
    );
    let (bin, log) = write_nodeprobe_reports(
        &ws.join("probe"),
        &[(endpoint.to_string(), report)],
    );
    let nodes = status_port::with_test_nodeprobe(bin.clone(), || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("JSON status"))
    });
    let node_a = nodes.iter().find(|node| node["name"] == "node-a").unwrap();
    assert_eq!(node_a["model"], "gpt\tα\nmax");
    assert_eq!(node_a["effort"], "xhigh\nhigh");
    assert_eq!(node_a["session_name"], "pi\tα\nchannel");
    let target_a = format!("{session}:{window_a}.%51");
    assert_eq!(
        node_a["tmux_command"],
        json!(format!(
            "tmux -S {} attach -t {}",
            shell_quote_for_status_test(endpoint),
            shell_quote_for_status_test(&target_a)
        ))
    );
    let human = status_port::with_test_nodeprobe(bin, || {
        human_text(cmd_status(&status_args(&ws, false, None, None)).expect("human status"))
    });
    let row_a = human
        .lines()
        .find(|line| line.contains("node-a"))
        .unwrap_or("");
    let row_b = human
        .lines()
        .find(|line| line.contains("node-b"))
        .unwrap_or("");
    let calls = logged_probe_calls(&log);
    assert_eq!(calls.len(), 2, "one native query per status invocation");
    assert!(
        human
            .matches(&format!("tmux -S {}", shell_quote_for_status_test(endpoint)))
            .count()
            == 1
            && row_a.contains("gpt\\tα\\nmax")
            && row_a.contains("xhigh\\nhigh")
            && row_a.contains("pi\\tα\\nchannel")
            && row_a.contains("win '甲")
            && row_a.contains("%51")
            && row_b.contains("grok\\tβ\\nxhigh")
            && row_b.contains("high\\tmax\\nlow")
            && row_a.matches("node-a").count() == 1
            && row_b.matches("node-b").count() == 1,
        "human fields must be escaped without splitting node rows and the shared template must quote its context:\n{human}"
    );
}

#[test]
#[serial(status_brief)]
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
#[serial(status_brief)]
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
#[serial(status_brief)]
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
#[serial(status_brief)]
fn cmd_status_resolver_rejects_fifo_and_oversized_receipts_without_spawn() {
    let cases = ["fifo-receipt", "oversized-receipt"];
    for tag in cases {
        let ws = brief_workspace(tag);
        write_state(
            &ws,
            &production_state(json!({ "worker": production_agent("worker", "%7") })),
        );
        let scratch = ws.join("probe");
        let log = scratch.join("spawned.log");
        let bin = write_nodeprobe(
            &scratch.join("candidate"),
            &format!("printf spawned >> '{}'\n", log.display()),
        );
        let started = Instant::now();
        status_port::with_test_nodeprobe(bin.clone(), || {
            let receipt = scratch.join("candidate/nodeprobe.capability.json");
            if tag == "fifo-receipt" {
                std::fs::remove_file(&receipt).unwrap();
                make_fifo(&receipt);
            } else {
                std::fs::write(&receipt, vec![b'x'; 65 * 1024]).unwrap();
            }
            let nodes = json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"));
            assert_eq!(nodes[0]["runtime_status"], "unknown", "{tag}");
        });
        assert!(!log.exists(), "{tag} must not spawn the candidate");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{tag} resolver exceeded bounded wall clock: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    let ws = brief_workspace("lazy-candidate");
    write_state(
        &ws,
        &production_state(json!({ "worker": production_agent("worker", "%7") })),
    );
    let scratch = ws.join("probe");
    let first_log = scratch.join("first.log");
    let second_log = scratch.join("second.log");
    let first = write_nodeprobe(
        &scratch.join("first"),
        &format!("printf first >> '{}'\n", first_log.display()),
    );
    let second = write_nodeprobe(
        &scratch.join("second"),
        &format!("printf second >> '{}'\n", second_log.display()),
    );
    let started = Instant::now();
    status_port::with_test_nodeprobe_candidates(vec![first, second.clone()], || {
        let second_receipt = scratch.join("second/nodeprobe.capability.json");
        std::fs::remove_file(&second_receipt).unwrap();
        make_fifo(&second_receipt);
        let nodes = json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"));
        assert_eq!(nodes[0]["runtime_status"], "unknown");
    });
    assert!(first_log.exists(), "valid first candidate must be spawned");
    assert!(!second_log.exists(), "later blocking candidate must not be scanned");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "lazy resolver exceeded bounded wall clock: {:?}",
        started.elapsed()
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn cmd_status_resolver_late_result_cannot_spawn_after_deadline() {
    let ws = brief_workspace("resolver-late-result");
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
    status_port::with_test_nodeprobe(bin, || {
        let caller_started = Instant::now();
        let signal = status_port::with_test_nodeprobe_resolver_delay(
            Duration::from_millis(2500),
            |signal| {
                let result = cmd_status(&status_args(&ws, true, None, None)).expect("status");
                let caller_elapsed = caller_started.elapsed();
                assert!(
                    caller_elapsed < Duration::from_millis(2400),
                    "resolver delay blocked status caller: {:?}",
                    caller_elapsed
                );
                let nodes = json_nodes(result);
                assert_eq!(nodes[0]["runtime_status"], "unknown");
                signal.clone()
            },
        );
        let entered_deadline = Instant::now() + Duration::from_secs(1);
        while !signal.entered() && Instant::now() < entered_deadline {
            std::thread::yield_now();
        }
        assert!(signal.entered(), "resolver delay seam was not entered");
        let finished_deadline = Instant::now() + Duration::from_secs(3);
        while !signal.finished() && Instant::now() < finished_deadline {
            std::thread::yield_now();
        }
        assert!(signal.finished(), "resolver worker did not release the gate");
        assert!(!log.exists(), "late resolver result must not spawn probe");

        let nodes = json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"));
        assert_eq!(nodes[0]["runtime_status"], "unknown");
        assert!(
            log.exists(),
            "one normal selection must prove resolver gate release"
        );
    });
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
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
#[serial(status_brief)]
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
#[serial(status_brief)]
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
#[serial(status_brief)]
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
#[test]
#[serial(status_brief)]
fn cmd_status_uses_canonical_roster_and_filters_same_team_retired_tombstones() {
    let ws = brief_workspace("canonical-roster");
    let mut stale = serde_json::Map::new();
    for index in 0..220 {
        stale.insert(
            format!("stale-{index}"),
            production_agent(&format!("stale-{index}"), &format!("%{index}")),
        );
    }
    let mut stopped = production_agent("stopped", "%9");
    stopped["status"] = json!("stopped");
    let state = json!({
        "session_name": "team-demo",
        "tmux_endpoint": "/tmp/ta-status-brief.sock",
        "tmux_socket": "/tmp/ta-status-brief.sock",
        "active_team_key": "demo",
        "agents": stale,
        "teams": {
            "demo": {
                "status": "alive",
                "session_name": "team-demo",
                "tmux_endpoint": "/tmp/ta-status-brief.sock",
                "agents": {
                    "live": production_agent("live", "%7"),
                    "retired": production_agent("retired", "%8"),
                    "stopped": stopped
                },
                "agent_lifecycle": {
                    "retired": {"state": "retired"}
                }
            },
            "sibling": {
                "status": "alive",
                "agent_lifecycle": {"live": {"state": "retired"}}
            }
        }
    });
    write_state(&ws, &state);
    let bin = write_nodeprobe(
        &ws.join("probe"),
        "printf '%s\\n' '{\"schema_version\":1,\"socket\":\"/tmp/ta-status-brief.sock\",\"nodes\":[]}'\n",
    );
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, Some("demo"))).expect("status"))
    });
    let mut names = nodes
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, vec!["live", "stopped"]);
    assert_eq!(
        nodes
            .iter()
            .find(|node| node["name"] == "stopped")
            .and_then(|node| node["runtime_status"].as_str()),
        Some("stopped")
    );
    let err = cmd_status(&status_args(&ws, true, Some("retired"), Some("demo"))).unwrap_err();
    assert!(err.to_string().contains("unknown agent id: retired"), "{err}");

    let empty = brief_workspace("canonical-empty");
    write_state(
        &empty,
        &json!({
            "active_team_key": "demo",
            "agents": {"stale": production_agent("stale", "%1")},
            "teams": {
                "demo": {
                    "status": "alive",
                    "agents": {},
                    "agent_lifecycle": {}
                }
            }
        }),
    );
    assert!(json_nodes(cmd_status(&status_args(&empty, true, None, None)).expect("status")).is_empty());
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&empty);
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn cmd_status_allows_only_missing_window_compatibility_match() {
    let ws = brief_workspace("missing-window");
    let mut agent = production_agent("worker", "%7");
    agent.as_object_mut().unwrap().remove("window");
    agent.as_object_mut().unwrap().remove("layout_window");
    agent
        .get_mut("display")
        .and_then(Value::as_object_mut)
        .unwrap()
        .remove("window");
    write_state(
        &ws,
        &production_state(json!({"worker": agent})),
    );
    let report = producer_report(
        "/tmp/ta-status-brief.sock",
        "observed-window",
        "%7",
        "pi_activity_channel",
    );
    let bin = write_nodeprobe(&ws.join("probe"), &format!("cat <<'EOF'\n{report}\nEOF\n"));
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "running");
    assert_eq!(nodes[0]["activity"], "idle");
    assert_eq!(
        nodes[0]["tmux_command"],
        "tmux -S '/tmp/ta-status-brief.sock' attach -t 'team-demo:observed-window.%7'"
    );
    let _ = std::fs::remove_dir_all(&ws);

    let conflict = brief_workspace("present-window-conflict");
    write_state(
        &conflict,
        &production_state(json!({"worker": production_agent("worker", "%7")})),
    );
    let report = producer_report(
        "/tmp/ta-status-brief.sock",
        "different-window",
        "%7",
        "pi_activity_channel",
    );
    let bin = write_nodeprobe(&conflict.join("probe"), &format!("cat <<'EOF'\n{report}\nEOF\n"));
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&conflict, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "unknown");
    assert!(nodes[0]["tmux_command"].is_null());
    let _ = std::fs::remove_dir_all(&conflict);
}

#[cfg(unix)]
#[test]
#[serial(status_brief)]
fn cmd_status_accepts_envelope_bound_node_socket_but_rejects_explicit_mismatch() {
    let ws = brief_workspace("envelope-node-socket");
    write_state(
        &ws,
        &production_state(json!({"worker": production_agent("worker", "%7")})),
    );
    let mut report: Value =
        serde_json::from_str(&producer_report(
            "/tmp/ta-status-brief.sock",
            "worker",
            "%7",
            "pi_activity_channel",
        ))
        .unwrap();
    report["nodes"][0].as_object_mut().unwrap().remove("socket");
    let report = serde_json::to_string(&report).unwrap();
    let bin = write_nodeprobe(&ws.join("probe"), &format!("cat <<'EOF'\n{report}\nEOF\n"));
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&ws, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "running");
    assert_eq!(nodes[0]["activity"], "idle");
    assert_eq!(nodes[0]["health"], "normal");
    assert_eq!(nodes[0]["session_name"], "pi-session");
    assert_eq!(
        nodes[0]["tmux_command"],
        "tmux -S '/tmp/ta-status-brief.sock' attach -t 'team-demo:worker.%7'"
    );
    let _ = std::fs::remove_dir_all(&ws);

    let mismatch = brief_workspace("explicit-node-socket-mismatch");
    write_state(
        &mismatch,
        &production_state(json!({"worker": production_agent("worker", "%7")})),
    );
    let mut report: Value =
        serde_json::from_str(&producer_report(
            "/tmp/ta-status-brief.sock",
            "worker",
            "%7",
            "pi_activity_channel",
        ))
        .unwrap();
    report["nodes"][0]["socket"] = json!("/tmp/other.sock");
    let report = serde_json::to_string(&report).unwrap();
    let bin = write_nodeprobe(
        &mismatch.join("probe"),
        &format!("cat <<'EOF'\n{report}\nEOF\n"),
    );
    let nodes = status_port::with_test_nodeprobe(bin, || {
        json_nodes(cmd_status(&status_args(&mismatch, true, None, None)).expect("status"))
    });
    assert_eq!(nodes[0]["runtime_status"], "unknown");
    assert!(nodes[0]["tmux_command"].is_null());
    let _ = std::fs::remove_dir_all(&mismatch);
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
