//! Offline mailbox records are readable, not queued delivery state.
use crate::framework::*;

#[test]
fn inbox_001_mailbox_is_append_only_and_non_consuming() {
    let ws = TestWorkspace::new("inbox001").with_fake_spec(&["a"]);
    let path = ws.path().to_str().unwrap();
    assert!(run_ta(&ws, &["quick-start", "--workspace", path, "--json"]).is_success());
    assert!(run_ta(&ws, &["stop-agent", "a", "--workspace", path, "--json"]).is_success());
    let physical = run_ta(
        &ws,
        &["send", "a", "not-delivered", "--workspace", path, "--json"],
    );
    assert!(!physical.is_success(), "offline pane must not report sent");
    let stored = run_ta(
        &ws,
        &[
            "send",
            "a",
            "stored-for-later",
            "--mailbox",
            "--workspace",
            path,
            "--json",
        ],
    );
    assert!(stored.is_success(), "{}", stored.stdout);
    assert_eq!(stored.json()["status"], "stored");
    let inbox_path = ws.path().join(".team/inbox/a.jsonl");
    let before = std::fs::read(&inbox_path).unwrap();
    for _ in 0..2 {
        let inbox = run_ta(&ws, &["inbox", "a", "--workspace", path, "--json"]);
        assert!(inbox.is_success(), "{}", inbox.stdout);
        let value = inbox.json();
        let records = value["messages"].as_array().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["content"], "stored-for-later");
        assert_eq!(records[0]["recipient"], "a");
        assert!(records[0]["message_id"].is_string());
        assert!(records[0].get("status").is_none());
    }
    assert_eq!(std::fs::read(inbox_path).unwrap(), before);
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}
