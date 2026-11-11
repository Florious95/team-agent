//! ---
//! purpose: Exercise unified doctor runtime health and the explicit comms attachment gate
//! contract:
//!   provides:
//!     - name: rec_002_doctor_checks
//!       what: Verifies unattached and host-failure JSON/exit contracts for doctor
//!   depends:
//!     - crate::cli::diagnose_port::doctor
//!     - crate::cli::diagnose::append_registry_channel_unbound_to_report
//! boundary:
//!   - Does not run full workspace verification or complete GitHub workflows
//! maturity: wired
//! ---
//! E2E-REC-002 Doctor emits provider/tmux health checks; the retired coordinator reports not_required.

use crate::framework::*;

#[test]
fn rec_002_doctor_checks() {
    let team_id = "rec002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);
    seed_unattached_leader(&ws);

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
        !out.is_success(),
        "running but unattached team must fail unified doctor; stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", false);
    // 262f6cdb: doctor answers with the six-link v1 report (a report, not an
    // error). The coordinator/*, profile_smoke and grok_slot inventory is retired
    // with it (R-M3; CHANGELOG 0.6.0).
    assert_json_field_eq_str(&j, "/schema", "team-agent.doctor.v1");
    assert_eq!(doctor_link(&j, "L2")["status"], "PASS", "the tmux host must be reachable: {j}");
    assert!(
        j["links"]
            .as_array()
            .is_some_and(|links| links.iter().any(|link| link["status"] != "PASS")),
        "unattached host doctor must report a failing link: {j}"
    );
    assert_eq!(
        doctor_link(&j, "L6")["status"],
        "FAIL",
        "unattached host doctor must retain leader_not_attached issue: {j}"
    );
    let repair = &j["next_action"];
    let argv: Vec<&str> = repair["argv"]
        .as_array()
        .map(|argv| argv.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        argv.get(1) == Some(&"claim-leader")
            && argv.contains(&"--workspace")
            && argv.windows(2).any(|pair| pair == ["--team", team_id])
            && repair["context"] == "from the current leader terminal",
        "unattached host doctor must retain the attachment repair: {j}"
    );

    // R-M3: the coordinator-metadata destruction case is gone with the coordinator (nothing writes
    // coordinator.json any more); the comms gate below keeps the attachment contract.
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
    assert!(
        !comms.is_success(),
        "unattached comms gate must fail: {comms:?}"
    );
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
