#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};

type BoundaryHook = Box<dyn FnMut(&str) -> Result<(), MessagingError>>;
thread_local! {
    static HOOK: std::cell::RefCell<Option<BoundaryHook>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn boundary(point: &str) -> Result<(), MessagingError> {
    HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(point),
        None => Ok(()),
    })
}

const WORKER: &str = "messaging::results::collection_tests::s2_process_collector";

fn isolated(name: &str, check: impl FnOnce(&Path)) {
    if std::env::var("TA_S2_CASE").as_deref() == Ok(name) {
        check(&std::env::current_dir().unwrap());
        return;
    }
    let dir = std::env::temp_dir().join(format!("ta-s2-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("messaging::results::collection_tests::{name}"),
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", dir.join("home"))
        .env("TA_S2_CASE", name)
        .current_dir(&dir)
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn envelope(result: &str, task: &str) -> Value {
    json!({"schema_version": "result_envelope_v1", "result_id": result,
        "task_id": task, "agent_id": "w1", "status": "success", "summary": "done",
        "artifacts": [], "changes": [], "tests": [], "risks": [], "next_actions": []})
}

fn write_fixture_state(dir: &Path, state: &Value) {
    let path = crate::state::persist::runtime_state_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, serde_json::to_vec_pretty(state).unwrap()).unwrap();
}

fn seed(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let mut teams = serde_json::Map::new();
    for key in ["T1", "T2"] {
        let spec = dir.join(".team/runtime").join(key).join("team.spec.yaml");
        std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
        std::fs::write(&spec, format!("name: {key}\ntasks: []\nagents: []\n")).unwrap();
        teams.insert(
            key.to_string(),
            json!({"session_name": key, "spec_path": spec,
            "team_dir": dir.join(".team").join(key), "agents": {},
            "tasks": [{"id": "task", "title": "Task", "status": "pending"}], "notes": []}),
        );
    }
    let mut state = teams["T1"].clone();
    state["active_team_key"] = json!("T1");
    state["teams"] = Value::Object(teams);
    write_fixture_state(dir, &state);
    MessageStore::open(dir).unwrap();
    std::fs::write(
        dir.join("result.json"),
        envelope("result", "task").to_string(),
    )
    .unwrap();
}

fn seed_batch(dir: &Path) {
    seed(dir);
    let mut state = disk(dir);
    state["teams"]["T1"]["tasks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "task-b", "title": "Task B", "status": "pending"}));
    write_fixture_state(dir, &state);
    let conn = db(dir);
    for (id, task_id, created_at) in [
        ("a1", "task", "1"),
        ("a2", "task", "2"),
        ("b1", "task-b", "3"),
    ] {
        insert_result_if_absent(
            &conn,
            id,
            task_id,
            "w1",
            &envelope(id, task_id).to_string(),
            "success",
            Some("T1"),
        )
        .unwrap();
        conn.execute(
            "update results set created_at = ?1 where result_id = ?2",
            rusqlite::params![created_at, id],
        )
        .unwrap();
    }
}

fn disk(dir: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(crate::state::persist::runtime_state_path(dir)).unwrap(),
    )
    .unwrap()
}

fn db(dir: &Path) -> rusqlite::Connection {
    crate::db::schema::open_db(MessageStore::open(dir).unwrap().db_path()).unwrap()
}

fn db_status(dir: &Path) -> Option<String> {
    db_status_for(dir, "result")
}

fn db_status_for(dir: &Path, result_id: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    db(dir)
        .query_row(
            "select status from results where result_id = ?1",
            [result_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn collected_ids(response: &Value) -> Vec<String> {
    response["response"]["collected_results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["result_id"].as_str().unwrap().to_string())
        .collect()
}

fn remove_task_from_state(dir: &Path, task_id: &str) {
    let mut state = disk(dir);
    let tasks = state
        .pointer_mut("/teams/T1/tasks")
        .and_then(Value::as_array_mut)
        .unwrap();
    tasks.retain(|task| task.get("id").and_then(Value::as_str) != Some(task_id));
    write_fixture_state(dir, &state);
}

fn collect_event_count(dir: &Path) -> usize {
    let path = dir.join(".team/logs/events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("collect.result"))
        .count()
}

fn install_finalize_execute_error(dir: &Path) {
    db(dir)
        .execute(
            "create trigger s2_test_finalize_execute_error
         before update of status on results
         when new.status = 'collected'
         begin
           select raise(abort, 's2 injected finalize execute error');
         end",
            [],
        )
        .unwrap();
}

fn remove_finalize_execute_error(dir: &Path) {
    db(dir)
        .execute("drop trigger s2_test_finalize_execute_error", [])
        .unwrap();
}

fn start(dir: &Path, label: &str, point: &str, pause: bool, file: bool) -> Child {
    start_with_options(dir, label, point, pause, file, false)
}

fn start_with_task_removal(dir: &Path, label: &str, point: &str, pause: bool, file: bool) -> Child {
    start_with_options(dir, label, point, pause, file, true)
}

fn start_with_options(
    dir: &Path,
    label: &str,
    point: &str,
    pause: bool,
    file: bool,
    remove_task: bool,
) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", WORKER, "--ignored", "--nocapture"])
        .env_clear()
        .env("HOME", dir.join("home"))
        .env("TA_S2_LABEL", label)
        .env("TA_S2_POINT", point)
        .env("TA_S2_PAUSE", if pause { "1" } else { "0" })
        .env("TA_S2_FILE", if file { "1" } else { "0" });
    if remove_task {
        command.env("TA_S2_REMOVE_TASK", "task");
    }
    let mut child = command
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if !point.is_empty() {
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "collector exited before {point}"
            );
            print!("{line}");
            if line.contains("S2_READY") {
                break;
            }
        }
        child.stdout = Some(reader.into_inner());
    }
    child
}

fn finish(mut child: Child, paused: bool) {
    if paused {
        write!(child.stdin.as_mut().unwrap(), "continue\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn output(dir: &Path, label: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{label}.json"))).unwrap())
        .unwrap()
}

fn collect_fresh(dir: &Path, label: &str, file: bool) -> Value {
    finish(start(dir, label, "", false, file), false);
    output(dir, label)
}

fn recovered(dir: &Path, file: bool, expected_output: usize) {
    let result = collect_fresh(dir, "recovered", file);
    assert_eq!(result["response"]["ok"], true, "{result}");
    assert_eq!(
        result["response"]["collected_results"]
            .as_array()
            .unwrap()
            .len(),
        expected_output,
        "{result}"
    );
    let state = disk(dir);
    assert_eq!(
        state["teams"]["T1"]["tasks"][0]["status"], "done",
        "{state}"
    );
    assert_eq!(
        state["teams"]["T1"]["tasks"][0]["accepted_result_id"],
        "result"
    );
    assert_eq!(state["teams"]["T2"]["tasks"][0]["status"], "pending");
    assert_eq!(db_status(dir).as_deref(), Some("collected"));
    assert_eq!(result["response"]["results"]["collected"], 1);
    let repeated = collect_fresh(dir, "repeated", false);
    assert_eq!(repeated["response"]["collected"], json!([]));
    assert_eq!(repeated["response"]["results"]["collected"], 1);
    println!(
        "S2 recovered state={} db={:?} output={result}",
        state["teams"],
        db_status(dir)
    );
}

#[test]
#[ignore = "hermetic subprocess entry; parent contracts invoke this exact test"]
fn s2_process_collector() {
    let dir = std::env::current_dir().unwrap();
    let label = std::env::var("TA_S2_LABEL").unwrap();
    let expected = std::env::var("TA_S2_POINT").unwrap();
    let pause = std::env::var("TA_S2_PAUSE").as_deref() == Ok("1");
    let file = std::env::var("TA_S2_FILE").as_deref() == Ok("1");
    let remove_task = std::env::var("TA_S2_REMOVE_TASK").ok();
    let hook_dir = dir.clone();
    let mut fired = false;
    HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |point| {
            if !fired && point == expected {
                fired = true;
                if let Some(task_id) = remove_task.as_deref() {
                    remove_task_from_state(&hook_dir, task_id);
                }
                println!("S2_READY {point}");
                std::io::stdout().flush().unwrap();
                if pause {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).unwrap();
                    assert_eq!(line.trim(), "continue");
                }
            }
            Ok(())
        }))
    });
    let path = dir.join("result.json");
    let result = match collect_for_team(&dir, file.then_some(path.as_path()), Some("T1")) {
        Ok(value) => json!({"response": value}),
        Err(error) => json!({"error": error.to_string()}),
    };
    std::fs::write(dir.join(format!("{label}.json")), result.to_string()).unwrap();
    println!("S2_RESULT {label} {result}");
}









