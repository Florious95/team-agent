use super::*;
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use crate::state::repository::{StateRepository, StateWriteIntent};

const WORKER: &str = "coordinator::tests::s3_bounded::s3_process_worker";

fn isolated(name: &str, check: impl FnOnce(&Path)) {
    if std::env::var("TA_S3_CASE").as_deref() == Ok(name) {
        check(&std::env::current_dir().unwrap());
        return;
    }
    let dir = std::env::temp_dir().join(format!("ta-s3-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("coordinator::tests::s3_bounded::{name}"), "--nocapture"])
        .env_clear().env("HOME", dir.join("home")).env("TA_S3_CASE", name)
        .current_dir(&dir).output().unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

fn disk(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(crate::state::persist::runtime_state_path(dir)).unwrap()).unwrap()
}

fn seed(dir: &Path, legacy: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let team = |key: &str| {
        let spec = dir.join(".team/runtime").join(key).join("team.spec.yaml");
        std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
        std::fs::write(&spec, format!("name: {key}\nagents: []\ntasks: []\n")).unwrap();
        json!({
            "session_name": key, "team_dir": dir.join(".team").join(key), "spec_path": spec,
            "agents": {"w1": {"agent_id": "w1", "provider": "fake", "status": "running",
                "window": "w1", "pane_id": "%11", "spawn_epoch": 1,
                "session_id": "fixture-session", "startup_prompts": "handled"}},
            "tasks": [{"id": "original", "title": "Original task", "assignee": "w1", "status": "pending"}],
            "notes": ["original note"]
        })
    };
    let mut state = team("T1");
    state["active_team_key"] = json!("T1");
    if !legacy { state["teams"] = json!({"T1": state.clone(), "T2": team("T2")}); }
    crate::state::persist::save_runtime_state(dir, &state).unwrap();
    MessageStore::open(dir).unwrap();
    std::fs::write(dir.join("result.json"), json!({
        "schema_version": "result_envelope_v1", "result_id": "result-original",
        "task_id": "original", "agent_id": "w1", "status": "success", "summary": "done",
        "artifacts": [], "changes": [], "tests": [], "risks": [], "next_actions": []
    }).to_string()).unwrap();
}

fn task(id: &str) -> Value { json!({"id": id, "title": id, "assignee": "w1"}) }

fn start_writer(dir: &Path, action: &str, team: Option<&str>, payload: Value, pause: bool) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", WORKER, "--ignored", "--nocapture"])
        .env_clear().env("HOME", dir.join("home"))
        .env("TA_S3_ACTION", action).env("TA_S3_TEAM", team.unwrap_or(""))
        .env("TA_S3_PAYLOAD", payload.to_string()).env("TA_S3_PAUSE", if pause { "1" } else { "0" })
        .current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    if pause {
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0, "writer exited before barrier");
            print!("{line}");
            if line.contains("S3_READY") { break; }
        }
        child.stdout = Some(reader.into_inner());
    }
    child
}

fn finish(mut child: Child, paused: bool) {
    if paused { child.stdin.take().unwrap().write_all(b"commit\n").unwrap(); }
    let output = child.wait_with_output().unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn write(dir: &Path, action: &str, team: Option<&str>, payload: Value) {
    finish(start_writer(dir, action, team, payload, false), false);
}

#[test]
#[ignore = "hermetic subprocess entry; parent contracts invoke this exact test"]
fn s3_process_worker() {
    let dir = std::env::current_dir().unwrap();
    let action = std::env::var("TA_S3_ACTION").unwrap();
    let team = std::env::var("TA_S3_TEAM").unwrap();
    let team = (!team.is_empty()).then_some(team.as_str());
    let payload: Value = serde_json::from_str(&std::env::var("TA_S3_PAYLOAD").unwrap()).unwrap();
    if std::env::var("TA_S3_PAUSE").as_deref() == Ok("1") {
        crate::state::repository::test_support::set_before_commit(|| {
            println!("S3_READY");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "commit");
        });
    }
    let tools = crate::mcp_server::TeamOrchestratorTools::with_identity(
        &dir, Some(crate::model::ids::AgentId::new("leader")), team.map(crate::model::ids::TeamKey::new),
    );
    match action.as_str() {
        "assign" => { tools.assign_task(&payload, Some("bounded contract")).unwrap(); }
        "note" => { println!("S3 note={:?}", tools.update_state(payload.as_str().unwrap()).unwrap()); }
        "collect" => {
            let output = crate::messaging::results::collect_for_team(&dir, Some(&dir.join("result.json")), false, team).unwrap();
            assert_eq!(output["ok"], true);
            println!("S3 collect={output}");
        }
        "sibling" => {
            let mut state = StateRepository::new(&dir).load_workspace().unwrap();
            state["teams"]["T3"] = json!({"session_name": "T3", "tasks": [task("third")], "notes": ["third"]});
            StateRepository::new(&dir).save(StateWriteIntent::LaunchTeam { team_key: "T3" }, &state).unwrap();
        }
        "topology" => {
            let mut state = StateRepository::new(&dir).load_team(Some("T1")).unwrap();
            state["agents"]["w1"]["pane_id"] = json!("%22");
            state["agents"]["w1"]["spawn_epoch"] = json!(2);
            StateRepository::new(&dir).save(StateWriteIntent::StartAgent { team_key: "T1", agent_id: "w1" }, &state).unwrap();
        }
        _ => panic!("unexpected test action"),
    }
    println!("S3_COMMITTED {action}");
}

fn coordinator(dir: &Path, hook: SaveHook) -> Coordinator {
    let transport = OfflineTransport::new().with_session_present(true)
        .with_windows(vec![WindowName::new("live-peer")]);
    Coordinator::for_test(WorkspacePath::new(dir.to_path_buf()),
        Box::new(MockRegistry::new(&[], &[])), Box::new(transport), None, None)
        .with_before_save_hook(hook)
}

#[test]
fn s3_tick_preserves_tasks_notes_and_siblings() {
    isolated("s3_tick_preserves_tasks_notes_and_siblings", |dir| {
        seed(dir, false);
        let hook: SaveHook = Box::new(|ws, observed| {
            assert_eq!(observed["agents"]["w1"]["worker_state"], "DEAD", "tick must produce a real observation delta");
            write(ws.as_path(), "assign", Some("T1"), task("new-task"));
            write(ws.as_path(), "note", Some("T1"), json!("concurrent note"));
            write(ws.as_path(), "collect", Some("T1"), Value::Null);
            write(ws.as_path(), "note", Some("T2"), json!("sibling note"));
            write(ws.as_path(), "sibling", None, Value::Null);
            let latest = disk(ws.as_path());
            assert_eq!(latest["teams"]["T1"]["agents"]["w1"]["pane_id"], observed["agents"]["w1"]["pane_id"]);
            println!("S3 before tick save topology unchanged; committed business={}", latest["teams"]);
            Ok(())
        });
        let report = coordinator(dir, hook).tick().unwrap();
        assert!(report.ok, "tick must save its observation: {report:?}");
        let state = disk(dir);
        let team = &state["teams"]["T1"];
        assert_eq!(team["tasks"][0]["accepted_result_id"], "result-original");
        assert!(team["tasks"].as_array().unwrap().iter().any(|row| row["id"] == "new-task"));
        assert_eq!(team["notes"], json!(["original note", "concurrent note"]));
        assert_eq!(team["agents"]["w1"]["worker_state"], "DEAD");
        assert_eq!(state["teams"]["T2"]["notes"], json!(["original note", "sibling note"]));
        assert_eq!(state["teams"]["T3"]["notes"], json!(["third"]));
        assert_eq!(state["tasks"], team["tasks"]);
        assert_eq!(StateRepository::new(dir).load_team(Some("T1")).unwrap()["tasks"], team["tasks"]);
        println!("S3 after tick save={state}");
    });
}

#[test]
fn s3_business_writers_commit_once_on_latest() {
    isolated("s3_business_writers_commit_once_on_latest", |root| {
        for (i, (left, right)) in [
            (("assign", task("left")), ("assign", task("right"))),
            (("note", json!("same note")), ("note", json!("same note"))),
            (("note", json!("same note")), ("assign", task("right"))),
            (("collect", Value::Null), ("assign", task("right"))),
            (("collect", Value::Null), ("note", json!("same note"))),
        ].into_iter().enumerate() {
            let dir = root.join(i.to_string());
            seed(&dir, false);
            let waiting = start_writer(&dir, left.0, Some("T1"), left.1, true);
            write(&dir, right.0, Some("T1"), right.1);
            finish(waiting, true);
            let state = disk(&dir);
            let team = &state["teams"]["T1"];
            if left.0 == "assign" { assert!(team["tasks"].as_array().unwrap().iter().any(|task| task["id"] == "left")); }
            if right.0 == "assign" { assert!(team["tasks"].as_array().unwrap().iter().any(|task| task["id"] == "right")); }
            if left.0 == "collect" { assert_eq!(team["tasks"][0]["accepted_result_id"], "result-original"); }
            let notes = usize::from(left.0 == "note") + usize::from(right.0 == "note");
            assert_eq!(team["notes"].as_array().unwrap().len(), 1 + notes);
            if left.0 == "note" && right.0 == "assign" {
                let markdown = std::fs::read_to_string(dir.join(".team/runtime/T1/team_state.md")).unwrap();
                assert!(markdown.contains("right"), "note must render the committed task view: {markdown}");
            }
            if left.0 == "collect" {
                write(&dir, "assign", Some("T1"), json!({"id": "original", "assignee": "w1", "title": "edited"}));
                assert_eq!(disk(&dir)["teams"]["T1"]["tasks"][0]["accepted_result_id"], "result-original");
            }
            println!("S3 pair {i} canonical={team}");
        }
    });
}

#[test]
fn s3_legacy_note_and_collection_keep_independent_updates() {
    isolated("s3_legacy_note_and_collection_keep_independent_updates", |dir| {
        seed(dir, true);
        let waiting = start_writer(dir, "collect", None, Value::Null, true);
        write(dir, "note", None, json!("legacy note"));
        finish(waiting, true);
        let state = disk(dir);
        assert_eq!(state["tasks"][0]["accepted_result_id"], "result-original");
        assert_eq!(state["notes"], json!(["original note", "legacy note"]));
        assert!(state.get("teams").is_none(), "legacy single Team shape remains valid");
    });
}

#[test]
fn s3_observation_keeps_live_topology_conflict() {
    isolated("s3_observation_keeps_live_topology_conflict", |dir| {
        seed(dir, false);
        let hook: SaveHook = Box::new(|ws, _| { write(ws.as_path(), "topology", Some("T1"), Value::Null); Ok(()) });
        let report = coordinator(dir, hook).tick().unwrap();
        assert_eq!(report.reason, Some(TickStopReason::PersistenceDegraded));
        assert_eq!(disk(dir)["teams"]["T1"]["agents"]["w1"]["pane_id"], "%22");
    });
}
