//! E2E-REC-003 Diagnose emits runtime/provider issue and repair payloads.

use crate::framework::*;

#[test]
fn rec_003_diagnose_output() {
    let team_id = "rec003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();
    // 262f6cdb: doctor answers with the six-link v1 report. The old inventory
    // (event_log, providers/*, issues, suggested_repairs) is retired with it
    // (M3; CHANGELOG 0.6.0).
    assert_json_field_eq_str(&j, "/schema", "team-agent.doctor.v1");
    let canonical = |path: &std::path::Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    assert_eq!(
        canonical(std::path::Path::new(j["workspace"].as_str().expect("doctor v1 workspace"))),
        canonical(ws.path()),
        "doctor v1 workspace: {j}"
    );
    assert_json_field_eq_str(&j, "/team", team_id);
    let ids: Vec<&str> = j["links"]
        .as_array()
        .expect("doctor v1 links")
        .iter()
        .filter_map(|link| link["id"].as_str())
        .collect();
    assert_eq!(ids, ["L1", "L2", "L3", "L4", "L5", "L6"], "doctor v1 link ids: {j}");
    // 262f6cdb (doctor_probe.rs:512-514): the repair (next_action) and the avoid
    // note are emitted exactly when the report is unhealthy.
    assert_eq!(j.get("next_action").is_some(), j["ok"] == false, "{j}");
    assert_eq!(j.get("avoid").is_some(), j["ok"] == false, "{j}");

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
