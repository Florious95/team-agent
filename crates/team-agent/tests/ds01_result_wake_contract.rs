#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use team_agent::coordinator::render_event_line;
use team_agent::message_store::MessageStore;
use team_agent::messaging::report_result;

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new(label: &str) -> Self {
        let serial = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ta-ds01-{label}-{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create DS-01 workspace");
        let _ = MessageStore::open(&path).expect("initialize team.db");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_db(workspace: &Path) -> Connection {
    let store = MessageStore::open(workspace).expect("open message store");
    team_agent::db::schema::open_db(store.db_path()).expect("open team.db")
}

fn envelope(task_id: &str, result_id: &str) -> Value {
    json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": task_id,
        "agent_id": "worker-ds01",
        "status": "success",
        "summary": "done",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
        "presentation": {
            "sink": "casefile",
            "class": "stage_result",
            "case_id": task_id,
        },
    })
}

fn report(workspace: &Path, task_id: &str, result_id: &str) -> Value {
    report_result(workspace, &envelope(task_id, result_id)).expect("report_result succeeds")
}

fn spawn_wait(workspace: &Path, task_id: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "wait",
            "--task",
            task_id,
            "--workspace",
            workspace.to_str().expect("utf8 workspace"),
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn team-agent wait")
}

fn finish_child(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().expect("poll child") {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("team-agent wait did not exit within {timeout:?}");
            }
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("child stdout")
        .read_to_end(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("child stderr")
        .read_to_end(&mut stderr)
        .expect("read stderr");
    Output {
        status,
        stdout,
        stderr,
    }
}

fn waiter_rows(workspace: &Path, task_id: &str) -> Vec<(String, PathBuf)> {
    let conn = open_db(workspace);
    let mut stmt = conn
        .prepare(
            "select watcher_id, recipient from result_watchers
             where task_id = ?1 order by created_at, watcher_id",
        )
        .expect("prepare waiter query");
    stmt.query_map(params![task_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PathBuf::from(row.get::<_, String>(1)?),
        ))
    })
    .expect("query waiters")
    .collect::<Result<Vec<_>, _>>()
    .expect("collect waiters")
}

fn wait_for_rows(workspace: &Path, task_id: &str, count: usize) -> Vec<(String, PathBuf)> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let rows = waiter_rows(workspace, task_id);
        if rows.len() == count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} waiter row(s); got {rows:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn insert_waiter(workspace: &Path, watcher_id: &str, task_id: &str, fifo: &Path) {
    let conn = open_db(workspace);
    conn.execute(
        "insert into result_watchers(
             watcher_id, owner_team_id, task_id, agent_id, message_id, leader_id,
             recipient, status, created_at
         ) values (?1, null, ?2, null, null, 'team-agent', ?3, 'pending', ?4)",
        params![
            watcher_id,
            task_id,
            fifo.to_string_lossy().as_ref(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .expect("insert FIFO waiter");
}

fn mkfifo(path: &Path) {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
}

fn event_text(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

#[test]
fn ds01_check_and_register_is_gapless_when_result_wins_the_write_lock() {
    let ws = TestWorkspace::new("gapless");
    let task_id = "task-gapless";
    let result_id = "res-gapless";
    let conn = open_db(ws.path());
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("hold write lock before wait starts");

    let child = spawn_wait(ws.path(), task_id);
    std::thread::sleep(Duration::from_millis(100));
    let stored = envelope(task_id, result_id);
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, 'current', ?2, 'worker-ds01', ?3, 'success', ?4)",
        params![
            result_id,
            task_id,
            stored.to_string(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .expect("insert result while owning immediate transaction");
    conn.execute_batch("COMMIT").expect("publish result");

    let output = finish_child(child, Duration::from_secs(3));
    assert!(
        output.status.success(),
        "wait must return the already committed result; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(result_id),
        "wait output must identify the result"
    );
    assert!(waiter_rows(ws.path(), task_id).is_empty());
}

#[test]
fn ds01_no_waiter_never_blocks_or_poison_report_result() {
    let ws = TestWorkspace::new("no-waiter");
    let started = Instant::now();
    let out = report(ws.path(), "task-no-waiter", "res-no-waiter");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "no-reader notification must not approach SQLite's 30s busy timeout"
    );
}

#[test]
fn ds01_wait_commits_registration_before_blocking_on_fifo() {
    let ws = TestWorkspace::new("short-tx");
    let task_id = "task-short-tx";
    let child = spawn_wait(ws.path(), task_id);
    let rows = wait_for_rows(ws.path(), task_id, 1);

    let conn = open_db(ws.path());
    let started = Instant::now();
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("wait must not retain its registration write lock");
    conn.execute_batch("COMMIT").expect("release probe lock");
    assert!(started.elapsed() < Duration::from_secs(1));

    let mut writer = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&rows[0].1)
        .expect("open registered FIFO");
    writer.write_all(b"res-short-tx\n").expect("wake wait");
    drop(writer);
    let output = finish_child(child, Duration::from_secs(3));
    assert!(output.status.success());
}

#[test]
fn ds01_enxio_and_enoent_are_observable_and_delete_stale_rows() {
    for (label, create_fifo, reason) in [("enxio", true, "ENXIO"), ("enoent", false, "ENOENT")] {
        let ws = TestWorkspace::new(label);
        let task_id = format!("task-{label}");
        let result_id = format!("res-{label}");
        let fifo = ws.path().join(format!("{label}.fifo"));
        if create_fifo {
            mkfifo(&fifo);
        }
        insert_waiter(ws.path(), &format!("wait-{label}"), &task_id, &fifo);

        let out = report(ws.path(), &task_id, &result_id);
        assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
        assert!(waiter_rows(ws.path(), &task_id).is_empty());
        let events = event_text(ws.path());
        assert!(events.contains("result_wake.notify_failed"), "{events}");
        assert!(events.contains(reason), "{events}");
    }
}

#[test]
fn ds01_wait_normal_exit_deletes_its_row_and_fifo() {
    let ws = TestWorkspace::new("normal-cleanup");
    let task_id = "task-normal-cleanup";
    let child = spawn_wait(ws.path(), task_id);
    let rows = wait_for_rows(ws.path(), task_id, 1);
    let fifo = rows[0].1.clone();
    let mut writer = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .expect("open registered FIFO");
    writer
        .write_all(b"res-normal-cleanup\n")
        .expect("write wake line");
    drop(writer);

    let output = finish_child(child, Duration::from_secs(3));
    assert!(output.status.success());
    assert!(waiter_rows(ws.path(), task_id).is_empty());
    assert!(!fifo.exists(), "normal exit must unlink its FIFO");
    assert!(event_text(ws.path()).contains("result_wake.registered"));
}

#[test]
fn ds01_wait_signal_exit_deletes_its_row_and_fifo() {
    let ws = TestWorkspace::new("signal-cleanup");
    let task_id = "task-signal-cleanup";
    let child = spawn_wait(ws.path(), task_id);
    let rows = wait_for_rows(ws.path(), task_id, 1);
    let fifo = rows[0].1.clone();

    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = finish_child(child, Duration::from_secs(3));
    assert!(
        !output.status.success(),
        "signal interruption must not report success"
    );
    assert!(waiter_rows(ws.path(), task_id).is_empty());
    assert!(!fifo.exists(), "signal exit must unlink its FIFO");
}

#[test]
fn ds01_orphan_path_is_never_reused_by_a_later_waiter() {
    let ws = TestWorkspace::new("orphan");
    let task_id = "task-orphan";
    let first = spawn_wait(ws.path(), task_id);
    let first_rows = wait_for_rows(ws.path(), task_id, 1);
    let first_id = first_rows[0].0.clone();
    let first_fifo = first_rows[0].1.clone();
    assert_eq!(unsafe { libc::kill(first.id() as i32, libc::SIGKILL) }, 0);
    let first_output = finish_child(first, Duration::from_secs(3));
    assert!(!first_output.status.success());
    assert!(
        first_fifo.exists(),
        "SIGKILL leaves the orphan FIFO as the test fixture"
    );

    let second = spawn_wait(ws.path(), task_id);
    let rows = wait_for_rows(ws.path(), task_id, 2);
    let second_fifo = rows
        .iter()
        .find(|(id, _)| id != &first_id)
        .map(|(_, path)| path.clone())
        .expect("second waiter row");
    assert_ne!(
        first_fifo, second_fifo,
        "later waits must never reuse an orphan path"
    );
    assert!(
        second_fifo
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(task_id)),
        "FIFO path must carry task identity in addition to its unique suffix"
    );

    let out = report(ws.path(), task_id, "res-orphan");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    let second_output = finish_child(second, Duration::from_secs(3));
    assert!(second_output.status.success());
    assert!(String::from_utf8_lossy(&second_output.stdout).contains("res-orphan"));
    assert!(waiter_rows(ws.path(), task_id).is_empty());
    let events = event_text(ws.path());
    assert!(events.contains("result_wake.notified"), "{events}");
    assert!(events.contains("result_wake.notify_failed"), "{events}");
}

#[test]
fn ds01_duplicate_result_id_does_not_wake_a_late_manual_waiter() {
    let ws = TestWorkspace::new("duplicate");
    let task_id = "task-duplicate";
    let result_id = "res-duplicate";
    let first = report(ws.path(), task_id, result_id);
    assert_eq!(first.get("ok").and_then(Value::as_bool), Some(true));

    let fifo = ws.path().join("duplicate.fifo");
    mkfifo(&fifo);
    let mut reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .expect("open FIFO reader");
    insert_waiter(ws.path(), "wait-duplicate", task_id, &fifo);

    let duplicate = report(ws.path(), task_id, result_id);
    assert_eq!(
        duplicate.get("status").and_then(Value::as_str),
        Some("duplicate_ignored")
    );
    let mut bytes = [0_u8; 128];
    assert_eq!(
        reader.read(&mut bytes).unwrap_or(0),
        0,
        "duplicate must not write FIFO"
    );
    assert_eq!(waiter_rows(ws.path(), task_id).len(), 1);
}

#[test]
fn ds01_three_wake_events_are_human_visible() {
    let registered = render_event_line(&json!({
        "event": "result_wake.registered",
        "task_id": "task-1",
        "watcher_id": "wait-1",
    }));
    let notified = render_event_line(&json!({
        "event": "result_wake.notified",
        "task_id": "task-1",
        "result_id": "res-1",
        "watcher_id": "wait-1",
    }));
    let failed = render_event_line(&json!({
        "event": "result_wake.notify_failed",
        "task_id": "task-1",
        "watcher_id": "wait-1",
        "reason": "ENXIO",
    }));

    assert!(registered.is_some(), "registration event must be rendered");
    assert!(notified.is_some(), "notification event must be rendered");
    assert!(failed.as_deref().is_some_and(|line| line.contains("ENXIO")));
}
