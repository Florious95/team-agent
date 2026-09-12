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
        .args(["--exact", &format!("messaging::results::collection_tests::{name}"), "--nocapture"])
        .env_clear().env("HOME", dir.join("home")).env("TA_S2_CASE", name)
        .current_dir(&dir).output().unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

fn envelope(result: &str, task: &str) -> Value {
    json!({"schema_version": "result_envelope_v1", "result_id": result,
        "task_id": task, "agent_id": "w1", "status": "success", "summary": "done",
        "artifacts": [], "changes": [], "tests": [], "risks": [], "next_actions": []})
}

fn seed(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let mut teams = serde_json::Map::new();
    for key in ["T1", "T2"] {
        let spec = dir.join(".team/runtime").join(key).join("team.spec.yaml");
        std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
        std::fs::write(&spec, format!("name: {key}\ntasks: []\nagents: []\n")).unwrap();
        teams.insert(key.to_string(), json!({"session_name": key, "spec_path": spec,
            "team_dir": dir.join(".team").join(key), "agents": {},
            "tasks": [{"id": "task", "title": "Task", "status": "pending"}], "notes": []}));
    }
    let mut state = teams["T1"].clone();
    state["active_team_key"] = json!("T1");
    state["teams"] = Value::Object(teams);
    crate::state::persist::save_runtime_state(dir, &state).unwrap();
    MessageStore::open(dir).unwrap();
    std::fs::write(dir.join("result.json"), envelope("result", "task").to_string()).unwrap();
}

fn disk(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(crate::state::persist::runtime_state_path(dir)).unwrap()).unwrap()
}

fn db(dir: &Path) -> rusqlite::Connection {
    crate::db::schema::open_db(MessageStore::open(dir).unwrap().db_path()).unwrap()
}

fn db_status(dir: &Path) -> Option<String> {
    use rusqlite::OptionalExtension;
    db(dir).query_row("select status from results where result_id = 'result'", [], |row| row.get(0)).optional().unwrap()
}

fn start(dir: &Path, label: &str, point: &str, pause: bool, file: bool) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", WORKER, "--ignored", "--nocapture"])
        .env_clear().env("HOME", dir.join("home"))
        .env("TA_S2_LABEL", label).env("TA_S2_POINT", point)
        .env("TA_S2_PAUSE", if pause { "1" } else { "0" })
        .env("TA_S2_FILE", if file { "1" } else { "0" })
        .current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    if !point.is_empty() {
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0, "collector exited before {point}");
            print!("{line}");
            if line.contains("S2_READY") { break; }
        }
        child.stdout = Some(reader.into_inner());
    }
    child
}

fn finish(mut child: Child, paused: bool) {
    if paused { child.stdin.take().unwrap().write_all(b"continue\n").unwrap(); }
    let output = child.wait_with_output().unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn output(dir: &Path, label: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{label}.json"))).unwrap()).unwrap()
}

fn collect_fresh(dir: &Path, label: &str, file: bool) -> Value {
    finish(start(dir, label, "", false, file), false);
    output(dir, label)
}

fn recovered(dir: &Path) {
    let result = collect_fresh(dir, "recovered", true);
    assert_eq!(result["response"]["ok"], true, "{result}");
    let state = disk(dir);
    assert_eq!(state["teams"]["T1"]["tasks"][0]["status"], "done", "{state}");
    assert_eq!(state["teams"]["T1"]["tasks"][0]["accepted_result_id"], "result");
    assert_eq!(state["teams"]["T2"]["tasks"][0]["status"], "pending");
    assert_eq!(db_status(dir).as_deref(), Some("collected"));
    let repeated = collect_fresh(dir, "repeated", true);
    assert_eq!(repeated["response"]["collected"], json!([]));
    println!("S2 recovered state={} db={:?} output={result}", state["teams"], db_status(dir));
}

#[test]
#[ignore = "hermetic subprocess entry; parent contracts invoke this exact test"]
fn s2_process_collector() {
    let dir = std::env::current_dir().unwrap();
    let label = std::env::var("TA_S2_LABEL").unwrap();
    let expected = std::env::var("TA_S2_POINT").unwrap();
    let pause = std::env::var("TA_S2_PAUSE").as_deref() == Ok("1");
    let file = std::env::var("TA_S2_FILE").as_deref() == Ok("1");
    let mut fired = false;
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(move |point| {
        if !fired && point == expected {
            fired = true;
            println!("S2_READY {point}");
            std::io::stdout().flush().unwrap();
            if pause {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line).unwrap();
                assert_eq!(line.trim(), "continue");
            }
        }
        Ok(())
    })));
    let path = dir.join("result.json");
    let result = match collect_for_team(&dir, file.then_some(path.as_path()), false, Some("T1")) {
        Ok(value) => json!({"response": value}),
        Err(error) => json!({"error": error.to_string()}),
    };
    std::fs::write(dir.join(format!("{label}.json")), result.to_string()).unwrap();
    println!("S2_RESULT {label} {result}");
}

#[test]
fn s2_original_finalize_window_recovers() {
    isolated("s2_original_finalize_window_recovers", |dir| {
        seed(dir);
        let mut child = start(dir, "interrupted", "after_finalize", true, true);
        assert_eq!(db_status(dir).as_deref(), Some("collected"));
        println!("S2 after durable DB finalize: task={} db={:?}", disk(dir)["teams"]["T1"]["tasks"], db_status(dir));
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        recovered(dir);
    });
}

#[test]
fn s2_all_persistence_windows_recover_in_fresh_processes() {
    isolated("s2_all_persistence_windows_recover_in_fresh_processes", |root| {
        for point in ["before_ingest", "after_ingest", "before_state", "after_state",
            "before_event", "after_event", "before_finalize", "after_finalize"] {
            let dir = root.join(point);
            seed(&dir);
            let mut child = start(&dir, "interrupted", point, true, true);
            println!("S2 crash at={point} task={} db={:?}", disk(&dir)["teams"]["T1"]["tasks"], db_status(&dir));
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            recovered(&dir);
        }
    });
}

#[test]
fn s2_event_and_state_io_failures_remain_recoverable() {
    isolated("s2_event_and_state_io_failures_remain_recoverable", |root| {
        for (point, relative) in [("before_event", ".team/logs/events.jsonl"), ("before_state", ".team/runtime/state-save.lock")] {
            let dir = root.join(point);
            seed(&dir);
            let child = start(&dir, "failed", point, true, true);
            let obstruction = dir.join(relative);
            std::fs::create_dir_all(obstruction.parent().unwrap()).unwrap();
            let backup = obstruction.with_extension("fault-original");
            if obstruction.exists() { std::fs::rename(&obstruction, &backup).unwrap(); }
            std::fs::create_dir(&obstruction).unwrap();
            finish(child, true);
            let failure = output(&dir, "failed");
            assert!(failure["error"].is_string(), "real filesystem failure must surface: {failure}");
            println!("S2 real IO failure at={point} error={failure} task={} db={:?}", disk(&dir)["teams"]["T1"]["tasks"], db_status(&dir));
            std::fs::remove_dir(&obstruction).unwrap();
            if backup.exists() { std::fs::rename(&backup, &obstruction).unwrap(); }
            recovered(&dir);
        }
    });
}

#[test]
fn s2_concurrent_collectors_emit_each_result_once() {
    isolated("s2_concurrent_collectors_emit_each_result_once", |dir| {
        seed(dir);
        let first = start(dir, "first", "after_state", true, true);
        let second = start(dir, "second", "before_lock", false, true);
        finish(first, true);
        finish(second, false);
        let first = output(dir, "first");
        let second = output(dir, "second");
        assert!(first.get("error").is_none() && second.get("error").is_none(), "{first} {second}");
        assert_eq!(first["response"]["collected"].as_array().unwrap().len()
            + second["response"]["collected"].as_array().unwrap().len(), 1);
        recovered(dir);
        let notifications: i64 = db(dir).query_row("select count(*) from leader_notification_log", [], |row| row.get(0)).unwrap();
        assert_eq!(notifications, 0, "collect must not create notification claims");
    });
}

#[test]
fn s2_scope_order_and_legacy_collected_rows_stay_compatible() {
    isolated("s2_scope_order_and_legacy_collected_rows_stay_compatible", |dir| {
        seed(dir);
        let conn = db(dir);
        conn.execute("insert into messages(message_id,owner_team_id,sender,recipient,content,status,created_at) values ('msg_scope','T1','leader','w1','fixture','accepted','0')", []).unwrap();
        for (id, task, team) in [("a", "task", "T1"), ("b", "task", "T1"),
            ("message", "msg_scope", "T1"), ("foreign", "task", "T2")] {
            insert_result_if_absent(&conn, id, task, "w1", &envelope(id, task).to_string(), "success", Some(team)).unwrap();
        }
        insert_result_if_absent(&conn, "invalid", "task", "w1", "{}", "success", Some("T1")).unwrap();
        insert_result_if_absent(&conn, "legacy-collected", "task", "w1", &envelope("legacy-collected", "task").to_string(), "collected", Some("T1")).unwrap();
        drop(conn);
        let result = collect_fresh(dir, "scoped", false);
        let response = &result["response"];
        assert_eq!(response["ok"], false, "invalid envelope is reported, not hidden");
        assert_eq!(response["collected_results"].as_array().unwrap().iter().map(|row| row["result_id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["a", "b", "message"]);
        assert_eq!(response["collected_results"][2]["scope"], "message");
        assert_eq!(response["results"], json!({"total": 5, "collected": 4, "invalid": 1, "uncollected": 0, "by_status": {}}));
        let state = disk(dir);
        assert_eq!(state["teams"]["T1"]["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(state["teams"]["T1"]["tasks"][0]["accepted_result_id"], "b");
        assert_eq!(state["teams"]["T2"]["tasks"][0]["status"], "pending");
        let repeated = collect_fresh(dir, "scope-repeated", false);
        assert_eq!(repeated["response"]["collected"], json!([]));
    });
}
