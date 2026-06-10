use super::*;

// ═════════════════════════════════════════════════════════════════════════
// A0 GREEN regression lock, integration tier (save_hook window injection).
// Locate doc: .team/artifacts/a0-rs-lostupdate-locate.md §5.4.
//
// Python 0.2.11 A0: the coordinator tick loads state (tick window opens), mutates in
// memory for seconds, then whole-file-saves with no merge — an add-agent registration
// landing inside that window is permanently overwritten (state.py:493). RS blocks this
// via the in-lock reload+merge at the save chokepoint (persist.rs:210-221, 272-313).
//
// This test pins the END-TO-END guard through the REAL tick: the `save_hook` seam
// (tick.rs save point) lets us deterministically land a concurrent registration on disk
// AFTER tick's load and BEFORE tick's save, then delegate to the real
// `save_team_scoped_state`. Zero sleeps, zero real races — ordering is fixed by the
// hook call order.
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn a0_green_lock_tick_save_preserves_registration_landed_after_tick_load() {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-coord-a0-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({
            "session_name": "team-a0",
            "active_team_key": "team-a0",
            "agents": { "w1": { "provider": "codex", "status": "running", "agent_id": "w1", "window": "w1" } },
        }),
    )
    .unwrap();

    // The hook runs at tick's atomic-save point: first simulate the concurrent
    // add-agent registration landing on disk (raw write, as another process would),
    // then run the REAL save path with tick's stale in-memory state.
    let hook: SaveHook = Box::new(|ws, tick_state| {
        let path = crate::state::persist::runtime_state_path(ws.as_path());
        let mut latest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        latest["agents"]["joiner"] = serde_json::json!({
            "provider": "codex", "status": "running", "agent_id": "joiner", "window": "joiner",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&latest).unwrap()).unwrap();
        crate::state::projection::save_team_scoped_state(ws.as_path(), tick_state)
    });

    let ws = WorkspacePath::new(dir.clone());
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let transport = MockTransport::new(true);
    let coord = Coordinator::for_test(ws, reg, Box::new(transport), Some(hook), None);
    let report = coord.tick().expect("tick should complete");
    assert!(report.ok, "tick must not degrade; report={report:?}");

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(crate::state::persist::runtime_state_path(&dir)).unwrap(),
    )
    .unwrap();
    assert!(
        saved
            .pointer("/agents/joiner")
            .is_some_and(serde_json::Value::is_object),
        "A0 GREEN lock: a registration landing between tick load and tick save must \
survive the tick's save (in-lock reload+merge, persist.rs:272-313); saved={saved}"
    );
    // 0.3.5 integration re-anchor (P3 / perf C-P3-1): the tick iteration counter moved
    // OUT of persistent state into .team/runtime/coordinator_tick.json — state.json is
    // counter-free BY DESIGN (p3_steady_tick_no_state_write). The original proxy
    // ("tick's save really happened") is preserved via the counter metadata file.
    let tick_meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".team/runtime/coordinator_tick.json")).unwrap(),
    )
    .unwrap();
    assert!(
        tick_meta
            .get("coordinator_tick_iteration_count")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count >= 1),
        "the tick really ran (its iteration counter metadata landed); tick_meta={tick_meta}"
    );
}
