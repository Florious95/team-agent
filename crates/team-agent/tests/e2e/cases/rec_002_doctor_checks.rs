//! E2E-REC-002 Doctor emits provider/coordinator/tmux health checks.

use crate::framework::*;

#[test]
fn rec_002_doctor_checks() {
    let team_id = "rec002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "unattached host doctor must remain healthy; stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field(&j, "/error", &serde_json::Value::Null);
    assert_json_field_eq_str(&j, "/coordinator/status", "running");
    assert_json_field_eq_bool(&j, "/coordinator/schema_ok", true);
    assert_json_field_eq_bool(&j, "/tmux/installed", true);
    assert_json_field_eq_bool(&j, "/profile_smoke/ok", true);
    assert_json_field_eq_bool(&j, "/grok_slot/readable", true);
    assert_json_field_eq_bool(&j, "/grok_slot/consistent", true);
    assert!(
        j["issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue == "leader_not_attached")),
        "unattached host doctor must retain leader_not_attached issue: {j}"
    );
    assert!(
        j["suggested_repairs"].as_array().is_some_and(|repairs| repairs
            .iter()
            .any(|repair| repair["issue"] == "leader_not_attached")),
        "unattached host doctor must retain the attachment repair: {j}"
    );

    std::fs::remove_file(ws.path().join(".team/runtime/coordinator.json"))
        .expect("remove coordinator metadata for destruction test");
    let missing_metadata = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        !missing_metadata.is_success(),
        "missing coordinator metadata must fail host doctor: {missing_metadata:?}"
    );
    let missing_metadata_json = missing_metadata.json();
    assert_json_field_eq_bool(&missing_metadata_json, "/ok", false);
    assert_json_field_eq_str(&missing_metadata_json, "/error", "metadata_missing");

    let comms = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--gate",
            "comms",
            "--json",
        ],
    );
    assert!(!comms.is_success(), "unattached comms gate must fail: {comms:?}");
    let comms_json = comms.json();
    assert_json_field_eq_bool(&comms_json, "/ok", false);
    assert_json_field_eq_str(&comms_json, "/checks/receiver_binding/status", "fail");

    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
}
