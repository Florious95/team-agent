//! Public CLI invariants for reminder text and stopping a team's only worker.

use crate::framework::*;
use serde_json::Value;
use std::fs;

fn collect_reminders(value: &Value, command: &str, reminders: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if key == "reminder" {
                    reminders.push((
                        command.to_string(),
                        child
                            .as_str()
                            .unwrap_or("<non-string reminder>")
                            .to_string(),
                    ));
                }
                collect_reminders(child, command, reminders);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_reminders(child, command, reminders);
            }
        }
        _ => {}
    }
}

fn session_missing_events(ws: &TestWorkspace) -> Vec<Value> {
    fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event.get("event").and_then(Value::as_str) == Some("coordinator.session_missing")
        })
        .collect()
}

#[test]
fn every_command_reminder_is_free_of_collect() {
    let team_id = "reminderred";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().expect("workspace path UTF-8");

    let quick_start = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&quick_start), "quick-start: {}", quick_start.stdout);
    let quick_start_json = quick_start.json();

    let status = run_ta(&ws, &["status", "--workspace", ws_path, "--json"]);
    assert!(status.is_success(), "status: {}", status.stderr);
    let status_json = status.json();

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
    ws.seed_crashed_workers_for_restart(team_id, &["a"]);
    let restart = run_ta(&ws, &["restart", ws_path, "--json"]);
    let restart_json = restart.json();
    assert!(
        restart_rebuild_completed(&restart_json),
        "restart did not complete: stdout={} stderr={}",
        restart.stdout,
        restart.stderr
    );

    let restarted_status = run_ta(&ws, &["status", "--workspace", ws_path, "--json"]);
    assert!(restarted_status.is_success(), "status after restart: {}", restarted_status.stderr);
    let restarted_status_json = restarted_status.json();

    let mut reminders = Vec::new();
    for (command, value) in [
        ("quick-start", quick_start_json),
        ("status", status_json),
        ("restart", restart_json),
        ("status after restart", restarted_status_json),
    ] {
        collect_reminders(&value, command, &mut reminders);
    }

    for required_command in ["quick-start", "restart"] {
        assert!(
            reminders.iter().any(|(command, _)| command == required_command),
            "{required_command} response must expose its reminder; observed={reminders:?}"
        );
    }
    assert!(
        !reminders.iter().any(|(_, reminder)| reminder.contains("collect")),
        "no command reminder may contain the literal `collect`; observed={reminders:?}"
    );
}

#[test]
fn stopping_the_only_worker_keeps_session_and_restartability() {
    let team_id = "stoponlyred";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().expect("workspace path UTF-8");
    let quick_start = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&quick_start), "quick-start: {}", quick_start.stdout);

    let state = ws.read_state();
    let socket = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .expect("quick-start records the team's tmux socket")
        .to_string();
    let session = state
        .get("session_name")
        .and_then(Value::as_str)
        .expect("quick-start records the team's tmux session")
        .to_string();

    let stop = run_ta(&ws, &["stop-agent", "a", "--workspace", ws_path, "--json"]);
    assert!(stop.is_success(), "stop-agent: {}", stop.stderr);
    assert_json_field_eq_bool(&stop.json(), "/stopped", true);

    // R-M3: there is no resident coordinator to keep ticking; the contract is the session and restartability.
    let session_preserved = tmux_session_exists_on_socket(&socket, &session);
    let missing_events = session_missing_events(&ws);

    let doctor = run_ta(&ws, &["doctor", "--workspace", ws_path, "--json"]);
    let doctor_json = serde_json::from_str::<Value>(&doctor.stdout).unwrap_or(Value::Null);
    // R-M3: doctor reports the retired coordinator as not required.
    let coordinator_not_required =
        doctor_json.pointer("/coordinator/status").and_then(Value::as_str) == Some("not_required");

    let start = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws_path,
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    let start_json = serde_json::from_str::<Value>(&start.stdout).unwrap_or(Value::Null);
    let state_after_start = ws.read_state();
    let woke_worker = start.is_success()
        && start_json.pointer("/ok").and_then(Value::as_bool) == Some(true)
        && tmux_session_exists_on_socket(&socket, &session)
        && state_agent(&state_after_start, "a")
            .get("status")
            .and_then(Value::as_str)
            == Some("running");

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );

    assert!(
        session_preserved
            && coordinator_not_required
            && missing_events.is_empty()
            && woke_worker,
        "single-worker stop contract failed: \
         session_preserved={session_preserved} ({session}@{socket}), coordinator_not_required={coordinator_not_required}, \
         coordinator.session_missing={missing_events:?}, woke_worker={woke_worker}; \
         doctor={doctor_json}, start_stdout={}, start_stderr={}, events={}",
        start.stdout,
        start.stderr,
        ws.events_jsonl_path().display()
    );
}
