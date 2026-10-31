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
    // R-M1b (M1 store-first): the offline seat is not typed into; the message is stored with an explained error.
    let undelivered = assert_stored_not_delivered(&ws, &physical);
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
    // R-M1c: the segment layout is the store's choice; reads must leave every inbox file byte-identical.
    let before = inbox_files(ws.path());
    for _ in 0..2 {
        let inbox = run_ta(&ws, &["inbox", "a", "--workspace", path, "--json"]);
        assert!(inbox.is_success(), "{}", inbox.stdout);
        let value = inbox.json();
        let records = value["messages"].as_array().unwrap();
        // R-M1a: both sends stored first, in order; records carry no queued delivery state.
        assert_eq!(records.len(), 2, "{value}");
        assert_eq!(records[0]["content"], "not-delivered");
        assert_eq!(records[0]["message_id"], undelivered["message_id"]);
        assert_eq!(records[1]["content"], "stored-for-later");
        for record in records {
            assert_eq!(record["recipient"], "a");
            assert!(record["message_id"].is_string());
            assert!(record.get("status").is_none(), "an inbox record is not a delivery state: {record}");
        }
    }
    assert_eq!(inbox_files(ws.path()), before);
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}

/// Every inbox file under `.team/inbox`, keyed by path (layout-agnostic).
fn inbox_files(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(dir: &std::path::Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() { walk(&path, out); } else { out.insert(path.clone(), std::fs::read(&path).unwrap()); }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(&root.join(".team/inbox"), &mut out);
    assert!(!out.is_empty(), "store-first sends must leave durable inbox files");
    out
}
