//! Repair restores addressing; failed physical sends are never queued for replay.
use crate::framework::*;

#[test]
fn send_009_worker_repair_requires_an_explicit_retry() {
    let ws = TestWorkspace::new("send009").with_fake_spec(&["a", "b"]);
    let path = ws.path().to_str().unwrap();
    assert!(quick_start_workers_available(&quick_start_fake(
        &ws, "send009"
    )));
    assert!(run_ta(&ws, &["stop-agent", "a", "--workspace", path, "--json"]).is_success());
    let failed = run_ta(
        &ws,
        &[
            "send",
            "a",
            "repair-retry-token",
            "--workspace",
            path,
            "--json",
        ],
    );
    assert!(!failed.is_success());
    assert!(failed.json().get("message_id").is_none());
    let start = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            path,
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    assert!(start.is_success(), "{} {}", start.stdout, start.stderr);
    let db = rusqlite::Connection::open(ws.path().join(".team/runtime/team.db")).unwrap();
    let queued: i64 = db
        .query_row(
            "select count(*) from messages where content = 'repair-retry-token'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        queued, 0,
        "failed direct send must not create a replay obligation"
    );
    assert!(!ws.path().join(".team/inbox/a.jsonl").exists());
    let retry = run_ta(
        &ws,
        &[
            "send",
            "a",
            "repair-retry-token",
            "--workspace",
            path,
            "--json",
        ],
    );
    assert!(retry.is_success(), "{} {}", retry.stdout, retry.stderr);
    assert_physical_input(&ws, &retry.json(), "repair-retry-token");
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}
