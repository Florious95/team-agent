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
    // R-M1b (M1 store-first): the stopped seat is not typed into; the message is stored, not queued.
    let failed = assert_stored_not_delivered(&ws, &failed);
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
    // M4 retired team.db, so "no replay obligation" is observed physically: the restarted seat's
    // pane must not receive the stored message on its own before the explicit retry.
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(
        !pane_of(&ws, "a").contains("repair-retry-token"),
        "a stored, undelivered send must never be replayed after repair"
    );
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
    // R-M1a: the retry is a new durable record; the failed one stays a distinct, unreplayed record.
    assert_ne!(retry.json()["message_id"], failed["message_id"]);
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}

/// The live pane text of `agent`, through the public locate endpoint.
fn pane_of(ws: &TestWorkspace, agent: &str) -> String {
    let located = run_ta(ws, &["locate", "--workspace", ws.path().to_str().unwrap(), "--json"]).json();
    let endpoint = located["endpoints"].as_array().unwrap().iter()
        .find(|endpoint| endpoint["agent"] == agent)
        .unwrap_or_else(|| panic!("{agent} must have a live endpoint after start-agent: {located}"))
        .clone();
    let socket = endpoint["socket"].as_str().unwrap();
    let output = std::process::Command::new("tmux")
        .args([if socket.contains('/') { "-S" } else { "-L" }, socket,
            "capture-pane", "-p", "-S", "-200", "-t", endpoint["pane_id"].as_str().unwrap()])
        .output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}
