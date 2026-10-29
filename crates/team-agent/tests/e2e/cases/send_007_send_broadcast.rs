//! Broadcast returns one physical receipt per published worker.
use crate::framework::*;

#[test]
fn send_007_broadcast_fanout_status() {
    let ws = TestWorkspace::new("send007").with_fake_spec(&["a", "b"]);
    let path = ws.path().to_str().unwrap();
    assert!(run_ta(&ws, &["quick-start", "--workspace", path, "--json"]).is_success());
    let result = run_ta(
        &ws,
        &[
            "send",
            "--to",
            "a,b",
            "fanout-probe",
            "--workspace",
            path,
            "--json",
        ],
    );
    assert!(result.is_success(), "{} {}", result.stdout, result.stderr);
    let value = result.json();
    assert_eq!(value["ok"], true);
    let receipts = value["results"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    let mut recipients = receipts
        .iter()
        .map(|value| value["recipient"].as_str().unwrap())
        .collect::<Vec<_>>();
    recipients.sort_unstable();
    assert_eq!(recipients, ["a", "b"]);
    for receipt in receipts {
        assert_physical_input(&ws, receipt, "fanout-probe");
    }
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", path, "--keep-logs", "--json"],
    );
}
