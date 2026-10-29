//! Public send receipts describe physical submission, not provider acceptance.
use crate::framework::*;
use serde_json::Value;

#[test]
fn send_003_send_json_shape_locks_public_keys() {
    let ws = TestWorkspace::new("send003").with_fake_spec(&["a"]);
    let path = ws.path().to_str().unwrap();
    assert!(run_ta(&ws, &["quick-start", "--workspace", path, "--json"]).is_success());
    let result = run_ta(
        &ws,
        &["send", "a", "shape-token", "--workspace", path, "--json"],
    );
    assert!(result.is_success(), "{} {}", result.stdout, result.stderr);
    let receipt = result.json();
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["accepted"], Value::Null);
    assert_eq!(receipt["recipient"], "a");
    assert!(receipt["pane_id"].as_str().unwrap().starts_with('%'));
    for retired in [
        "message_id",
        "result_id",
        "delivery_pending",
        "poll_via",
        "message_status",
    ] {
        assert!(receipt.get(retired).is_none(), "{receipt}");
    }
    assert_physical_input(&ws, &receipt, "shape-token");
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}
