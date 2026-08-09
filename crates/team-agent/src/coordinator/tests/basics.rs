use super::*;

// ═════════════════════════════════════════════════════════════════════════
// GROUP A — typed-enum / event byte-lock (committed model derive; GREEN baseline)
// 这些钉死 serde 契约,确保 events.jsonl 字节不漂移(§3/§22)。
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn protocol_version_is_two() {
    // metadata.py:13 COORDINATOR_PROTOCOL_VERSION = 2
    assert_eq!(PROTOCOL_VERSION, 2);
}

#[test]
fn default_tick_interval_is_five_seconds() {
    // __main__.py:101 DEFAULT_TICK_INTERVAL_SEC = 5.0 (Gap 36c)
    assert_eq!(DEFAULT_TICK_INTERVAL_SEC, 5.0);
    assert_eq!(BACKOFF_MAX_SEC, 60.0);
}

#[test]
fn rotation_marker_is_byte_exact() {
    // watch.py:22 — byte-for-byte (note the U+2014 em dash).
    assert_eq!(
        ROTATION_MARKER,
        "[watch] log rotated; archived segment events.jsonl.1 not replayed — historical replay deferred to a future --replay flag"
    );
}

#[test]
fn status_enums_serialize_to_exact_python_strings() {
    let cases: &[(String, &str)] = &[
        (
            serde_json::to_string(&CoordinatorHealthStatus::Missing).unwrap(),
            "\"missing\"",
        ),
        (
            serde_json::to_string(&CoordinatorHealthStatus::InvalidPid).unwrap(),
            "\"invalid_pid\"",
        ),
        (
            serde_json::to_string(&CoordinatorHealthStatus::Running).unwrap(),
            "\"running\"",
        ),
        (
            serde_json::to_string(&CoordinatorHealthStatus::Stale).unwrap(),
            "\"stale\"",
        ),
        (
            serde_json::to_string(&StartOutcome::AlreadyRunning).unwrap(),
            "\"already_running\"",
        ),
        (
            serde_json::to_string(&StartOutcome::RestartIncompatibleStopFailed).unwrap(),
            "\"restart_incompatible_stop_failed\"",
        ),
        (
            serde_json::to_string(&StartOutcome::SchemaIncompatible).unwrap(),
            "\"schema_incompatible\"",
        ),
        (
            serde_json::to_string(&StartOutcome::Started).unwrap(),
            "\"started\"",
        ),
        (
            serde_json::to_string(&StartOutcome::StartedAfterRotation).unwrap(),
            "\"started_after_rotation\"",
        ),
        (
            serde_json::to_string(&StopOutcome::Missing).unwrap(),
            "\"missing\"",
        ),
        (
            serde_json::to_string(&StopOutcome::InvalidPidRemoved).unwrap(),
            "\"invalid_pid_removed\"",
        ),
        (
            serde_json::to_string(&StopOutcome::KillFailed).unwrap(),
            "\"kill_failed\"",
        ),
        (
            serde_json::to_string(&StopOutcome::Stopped).unwrap(),
            "\"stopped\"",
        ),
        (
            serde_json::to_string(&TickStopReason::TmuxSessionMissing).unwrap(),
            "\"tmux_session_missing\"",
        ),
        (
            serde_json::to_string(&TickStopReason::PersistenceDegraded).unwrap(),
            "\"persistence_degraded\"",
        ),
        (
            serde_json::to_string(&MetadataSource::Boot).unwrap(),
            "\"boot\"",
        ),
        (
            serde_json::to_string(&MetadataSource::Start).unwrap(),
            "\"start\"",
        ),
        (
            serde_json::to_string(&WholeTeamGoneClass::Alive).unwrap(),
            "\"alive\"",
        ),
        (
            serde_json::to_string(&WholeTeamGoneClass::CleanShutdown).unwrap(),
            "\"clean_shutdown\"",
        ),
        (
            serde_json::to_string(&WholeTeamGoneClass::RestartInProgress).unwrap(),
            "\"restart_in_progress\"",
        ),
        (
            serde_json::to_string(&WholeTeamGoneClass::UnexpectedExit).unwrap(),
            "\"unexpected_exit\"",
        ),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
}

#[test]
fn coordinator_metadata_json_field_names_are_stable() {
    // metadata.py:50-57 — 稳定 coordinator.json 契约。
    let m = meta(123, PROTOCOL_VERSION, GOLDEN_SCHEMA_VERSION);
    let json = serde_json::to_value(&m).unwrap();
    assert_eq!(json["pid"], 123);
    assert_eq!(json["protocol_version"], 2);
    assert_eq!(json["message_store_schema_version"], 3);
    let identity = current_coordinator_binary_identity();
    assert_eq!(json["binary_path"], identity.binary_path);
    assert_eq!(json["binary_version"], identity.binary_version);
    assert_eq!(json["source"], "boot");
    assert_eq!(json["updated_at"], "2026-06-02T00:00:00+00:00");
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP B — pure functions (backoff / orphan / metadata_ok) — RED via unimplemented!()
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn backoff_sequence_is_5_10_20_40_60_60() {
    // __main__.py:65 min(interval * 2^min(failures-1, 5), 60.0). interval=5.0.
    // Golden (probe): [5, 10, 20, 40, 60, 60, 60, ...].
    assert_eq!(backoff_sleep_sec(5.0, 1), 5.0);
    assert_eq!(backoff_sleep_sec(5.0, 2), 10.0);
    assert_eq!(backoff_sleep_sec(5.0, 3), 20.0);
    assert_eq!(backoff_sleep_sec(5.0, 4), 40.0);
    assert_eq!(backoff_sleep_sec(5.0, 5), 60.0);
    assert_eq!(backoff_sleep_sec(5.0, 6), 60.0);
    assert_eq!(backoff_sleep_sec(5.0, 9), 60.0);
}

#[test]
fn backoff_caps_at_60_even_with_large_interval() {
    // min(.., 60.0) — interval 30 → 30,60,(cap)60...
    assert_eq!(backoff_sleep_sec(30.0, 1), 30.0);
    assert_eq!(backoff_sleep_sec(30.0, 2), 60.0);
    assert_eq!(backoff_sleep_sec(30.0, 7), 60.0);
}

#[test]
fn orphan_self_terminate_requires_all_three_conditions() {
    // __main__.py:52 — current_ppid != initial_ppid ∧ current_ppid == 1 ∧ !workspace.exists().
    let missing = WorkspacePath::new("/tmp/team-agent-NONEXISTENT-coord-orphan-xyz");
    // 全三成立 → true.
    assert!(should_orphan_self_terminate(9000, 1, &missing));
    // ppid 未变(== initial)→ false,即便 ppid==1 且 workspace 不存在。
    assert!(!should_orphan_self_terminate(1, 1, &missing));
    // ppid != 1(被正常 supervisor 收养)→ false。
    assert!(!should_orphan_self_terminate(9000, 4242, &missing));
}

#[test]
fn orphan_self_terminate_false_when_workspace_exists() {
    // workspace 仍在磁盘 → 绝不自杀,即便 ppid 变成 1(card §91)。
    let alive = WorkspacePath::new("/tmp"); // /tmp 存在
    assert!(!should_orphan_self_terminate(9000, 1, &alive));
}

#[test]
fn metadata_ok_requires_all_three_to_match() {
    // metadata.py:37-43 — pid, protocol version, and current message schema all match.
    let good = meta(555, PROTOCOL_VERSION, crate::db::schema::SCHEMA_VERSION);
    assert!(coordinator_metadata_ok(Some(&good), Pid(555)));
    // pid 不符 → false。
    assert!(!coordinator_metadata_ok(Some(&good), Pid(999)));
    // protocol_version 不符(bump 触发 restart_incompatible)→ false。
    let bad_proto = meta(555, 1, crate::db::schema::SCHEMA_VERSION);
    assert!(!coordinator_metadata_ok(Some(&bad_proto), Pid(555)));
    // schema_version 不符 → false(不可静默继续旧 schema 写库,card §89)。
    let bad_schema = meta(555, PROTOCOL_VERSION, crate::db::schema::SCHEMA_VERSION - 1);
    assert!(!coordinator_metadata_ok(Some(&bad_schema), Pid(555)));
}

#[test]
fn metadata_ok_none_is_false() {
    // metadata.py:38 — bool(metadata and ...) — None → False.
    assert!(!coordinator_metadata_ok(None, Pid(1)));
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP C — paths (paths.py) — RED
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn coordinator_paths_end_with_expected_filenames() {
    // paths.py:8-17 — runtime_dir(ws)/coordinator.{pid,json,log}.
    let w = ws();
    assert!(coordinator_pid_path(&w).ends_with("coordinator.pid"));
    assert!(coordinator_meta_path(&w).ends_with("coordinator.json"));
    assert!(coordinator_log_path(&w).ends_with("coordinator.log"));
}

// NOTE: schema_health / health / start / stop / tick are `&self` methods on
// `Coordinator`. They are NO LONGER deferred: `Coordinator::for_test` (a #[cfg(test)]
// constructor) + the local `MockTransport` (all ~15 Transport methods stubbed, recording
// calls) + `MockRegistry` give a fully constructible Coordinator. Their behavior contracts
// are ASSERTED in GROUP I/J below (RED via the unimplemented production bodies).
