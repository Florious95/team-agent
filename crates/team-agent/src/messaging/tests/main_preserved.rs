use super::*;

// ── collect --result-file INGEST SEMANTIC (golden results.py:58-73) ──────────────────────────────
// Golden `collect(result_file=…)` INDEPENDENTLY INGESTS a standalone result: a valid result_envelope_v1
// is `store.add_result(envelope)`'d (results.py:73) regardless of any in-flight delivery, then the
// collection loop collects it when its task_id is a known task (state.tasks) OR message-scoped (msg_+
// matching message). NO live in-flight task is required at ingest time. rt-host-b @ c262bf7 saw a
// VALID envelope collect to exit-1 / empty output / NOT in the
// results table — i.e. the --result-file ingest path was a no-op (results.rs once did
// `let _ = (result_file, …)`). This pins the happy path: a valid envelope for a KNOWN task must be
// ingested into the results table AND collected, with ok=true. (Completes the previously-deferred
// collected_results golden — the fixture is built from the real compiler + state persistence.)
#[test]
fn collect_result_file_ingests_valid_known_task_envelope_into_results() {
    let team = tmp_ws("collect_rf");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: ct\nobjective: collect --result-file probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join("w1.md"),
        "---\nname: w1\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nW1.\n",
    )
    .unwrap();
    let spec = crate::compiler::compile_team(&team).expect("compile collect team");
    std::fs::write(
        team.join("team.spec.yaml"),
        crate::model::yaml::dumps(&spec),
    )
    .unwrap();
    // seed runtime state with a KNOWN task "task-1" so the collection loop accepts the ingested result.
    crate::state::persist::save_runtime_state(
        &team,
        &serde_json::json!({
            "spec_path": team.join("team.spec.yaml").to_string_lossy(),
            "agents": {"w1": {"status": "running", "provider": "codex"}},
            "tasks": [{"id": "task-1", "title": "t", "type": "impl", "assignee": "w1",
                       "deps": [], "acceptance": "x", "status": "pending"}]
        }),
    )
    .unwrap();
    // a schema-valid result_envelope_v1 (validate_result_envelope accepts it) for task-1.
    let envelope = serde_json::json!({
        "schema_version": "result_envelope_v1", "task_id": "task-1", "agent_id": "w1",
        "status": "success", "summary": "done",
        "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
    });
    let rf = team.join("result.json");
    std::fs::write(&rf, serde_json::to_string(&envelope).unwrap()).unwrap();
    let out = collect(&team, Some(rf.as_path()), false)
        .expect("collect with a valid --result-file must not error");
    // golden: a valid standalone envelope is ingested + collected, ok=true (NOT a silent exit-1).
    assert_eq!(
        out["ok"],
        serde_json::json!(true),
        "valid --result-file collect must be ok:true; got {out}"
    );
    let collected = out["collected_results"]
        .as_array()
        .expect("collected_results array");
    assert_eq!(
        collected.len(),
        1,
        "golden: the --result-file envelope must be INGESTED (store.add_result) then collected; a no-op \
         ingest leaves collected_results=[]. got {out}"
    );
    assert_eq!(collected[0]["task_id"], serde_json::json!("task-1"));
    assert_eq!(collected[0]["agent_id"], serde_json::json!("w1"));
    assert_eq!(collected[0]["status"], serde_json::json!("success"));
    // the result is actually IN the results table (counts reflect it) — proves real ingestion.
    assert_eq!(
        out["results"]["total"], serde_json::json!(1),
        "the ingested result must persist in the results table (counts.total=1); a no-op ingest yields 0. got {}",
        out["results"]
    );
    assert_eq!(
        out["results"]["collected"],
        serde_json::json!(1),
        "the ingested result must be marked collected; got {}",
        out["results"]
    );
    let _ = std::fs::remove_dir_all(&team);
}
