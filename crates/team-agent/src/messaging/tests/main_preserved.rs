use super::*;

// ── F1 [P1 byte-shape] — stuck_cancel suppression snapshot.delivered_message_ids must use golden's
// _DELIVERED_MESSAGE_STATUSES = {visible, submitted, delivered, acknowledged} (scheduler.py:27,376-383),
// sorted. The Rust delivered_message_ids (scheduler.rs:243) used {injected, visible, submitted,
// submitted_unverified, delivered} — the WRONG set (reviewer's explicit warning: don't copy the
// activity.rs deadlock query set): it EXCLUDES 'acknowledged' and INCLUDES 'injected'/'submitted_unverified'.
// (The snapshot also must carry BOTH assigned_task_ids AND delivered_message_ids — that structural half
// is already present; this pins the STATUS SET.)
#[test]
fn stuck_cancel_snapshot_delivered_message_ids_uses_golden_status_set() {
    let ws = tmp_ws("f1_delivered");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "active_team_key": "teamX",
            "agents": {"w1": {"status": "running", "provider": "codex"}}
        }),
    )
    .unwrap();
    let store = crate::message_store::MessageStore::open(&ws).unwrap();
    // golden-delivered (must be IN delivered_message_ids): acknowledged + visible.
    let m_ack = store.create_message(None, "leader", "w1", "ack-me", None, true, Some("teamX")).unwrap();
    store.mark(&m_ack, "acknowledged", None).unwrap();
    let m_vis = store.create_message(None, "leader", "w1", "vis", None, true, Some("teamX")).unwrap();
    store.mark(&m_vis, "visible", None).unwrap();
    // NOT golden-delivered (must be EXCLUDED): injected.
    let m_inj = store.create_message(None, "leader", "w1", "inj", None, true, Some("teamX")).unwrap();
    store.mark(&m_inj, "injected", None).unwrap();
    drop(store);
    let _ = stuck_cancel(&ws, "w1", None, "leader").unwrap();
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    let snapshot = &state["coordinator"]["suppressed_idle_alerts"]["teamX"]["w1"]["stuck"]["snapshot"];
    assert!(snapshot.get("assigned_task_ids").is_some(), "F1: snapshot must carry assigned_task_ids; got {snapshot}");
    let delivered = snapshot
        .get("delivered_message_ids")
        .and_then(serde_json::Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>())
        .unwrap_or_else(|| panic!("F1: snapshot must carry delivered_message_ids; got {snapshot}"));
    assert!(
        delivered.contains(&m_ack),
        "F1: golden _DELIVERED_MESSAGE_STATUSES includes 'acknowledged' — the acknowledged message must be \
         in delivered_message_ids; the Rust set omits it. got {delivered:?}"
    );
    assert!(delivered.contains(&m_vis), "F1: 'visible' is delivered (golden); got {delivered:?}");
    assert!(
        !delivered.contains(&m_inj),
        "F1: 'injected' is NOT in golden _DELIVERED_MESSAGE_STATUSES — it must be EXCLUDED; the Rust set \
         wrongly includes it. got {delivered:?}"
    );
    let mut sorted = delivered.clone();
    sorted.sort();
    assert_eq!(delivered, sorted, "F1: delivered_message_ids must be sorted (golden sorts); got {delivered:?}");
}
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
        "---\nname: w1\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nW1.\n",
    )
    .unwrap();
    let spec = crate::compiler::compile_team(&team).expect("compile collect team");
    std::fs::write(team.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
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
    assert_eq!(out["ok"], serde_json::json!(true), "valid --result-file collect must be ok:true; got {out}");
    let collected = out["collected_results"].as_array().expect("collected_results array");
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
    assert_eq!(out["results"]["collected"], serde_json::json!(1), "the ingested result must be marked collected; got {}", out["results"]);
    let _ = std::fs::remove_dir_all(&team);
}
