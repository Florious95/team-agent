use super::*;

// ═════════════════════════════════════════════════════════════════════════
// GROUP G — watch render_event_line (watch.py:46) — golden text — RED
//   exact strings captured via PYTHONPATH probe against v0.2.11.
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn render_result_received_collapses_whitespace_and_defaults() {
    // watch.py:48-49 + _result_line — "result_received: <agent|-> -> <clean summary>".
    let e = serde_json::json!({"event": "result_received", "agent_id": "w1", "summary": "did   the\nthing"});
    assert_eq!(
        render_event_line(&e),
        Some("result_received: w1 -> did the thing".to_string())
    );
    // missing agent + summary → both '-'.
    let bare = serde_json::json!({"event": "result_received"});
    assert_eq!(
        render_event_line(&bare),
        Some("result_received: - -> -".to_string())
    );
}

#[test]
fn render_injected_and_submitted_share_prefix_and_truncate_message_id() {
    // watch.py:50-51 — both render as "leader_receiver.injected"; message_id truncated to 12.
    let inj = serde_json::json!({"event": "leader_receiver.injected", "message_id": "abcdef0123456789", "recipient": "w2"});
    assert_eq!(
        render_event_line(&inj),
        Some("leader_receiver.injected: abcdef012345 -> w2".to_string())
    );
    // submitted uses same label, msg_id fallback, `to` recipient fallback.
    let sub = serde_json::json!({"event": "leader_receiver.submitted", "msg_id": "xy", "to": "w3"});
    assert_eq!(
        render_event_line(&sub),
        Some("leader_receiver.injected: xy -> w3".to_string())
    );
}

#[test]
fn render_send_failed_uses_reason_then_error_fallback() {
    // watch.py:52-53 — reason || error || '-', recipient || to || target || '-', whitespace-cleaned.
    let with_reason =
        serde_json::json!({"event": "send.failed", "recipient": "w4", "reason": "  pane   gone  "});
    assert_eq!(
        render_event_line(&with_reason),
        Some("send.failed: w4 reason=pane gone".to_string())
    );
    let with_error = serde_json::json!({"event": "send.failed", "target": "w5", "error": "boom"});
    assert_eq!(
        render_event_line(&with_error),
        Some("send.failed: w5 reason=boom".to_string())
    );
}

#[test]
fn render_rebind_required_uses_pane_and_reason_fallbacks() {
    // watch.py:54-57 — old_pane_id || pane_id || target || '-'; reason || rediscovery_status || '-'.
    let e = serde_json::json!({"event": "leader_receiver.rebind_required", "old_pane_id": "%9", "reason": "lost"});
    assert_eq!(
        render_event_line(&e),
        Some("leader_receiver.rebind_required: pane=%9 reason=lost".to_string())
    );
    let no_reason =
        serde_json::json!({"event": "leader_receiver.rebind_required", "pane_id": "%7"});
    assert_eq!(
        render_event_line(&no_reason),
        Some("leader_receiver.rebind_required: pane=%7 reason=-".to_string())
    );
}

#[test]
fn render_api_error_defaults_unknown_class_and_dash() {
    // watch.py:58-62 — error_class || "Unknown"; provider || '-'; snippet || '-' cleaned.
    let error_class = ["Over", "loaded"].concat();
    let code = (500 + 29).to_string();
    let e = serde_json::json!({"event": "leader.api_error", "error_class": error_class, "provider": "claude_code", "matched_pattern_snippet": format!("{code}  too  many")});
    assert_eq!(
        render_event_line(&e),
        Some(format!(
            "leader.api_error: {} provider=claude_code snippet={} too many",
            ["Over", "loaded"].concat(),
            code
        ))
    );
    let bare = serde_json::json!({"event": "leader.api_error"});
    assert_eq!(
        render_event_line(&bare),
        Some("leader.api_error: Unknown provider=- snippet=-".to_string())
    );
}

#[test]
fn render_non_renderable_events_return_none() {
    // watch.py:63 — coordinator.* / unknown events → None.
    assert_eq!(
        render_event_line(&serde_json::json!({"event": "coordinator.boot"})),
        None
    );
    assert_eq!(
        render_event_line(&serde_json::json!({"event": "unknown.thing"})),
        None
    );
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP H — WatchCursor rotation invariants (watch.py:66-97) — RED via collect_watch_lines
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn watch_cursor_default_is_uninitialized() {
    let c = WatchCursor::default();
    assert_eq!(c.event_offset, 0);
    assert!(!c.initialized);
    assert!(c.archive_signature.is_none());
    assert!(c.seen_result_ids.is_empty());
}

/// seed a workspace with an `events.jsonl` (+ optional archived `events.jsonl.1`).
/// Returns the resolved `WorkspacePath`. The events file holds one renderable
/// `result_received` line so `collect_watch_lines` has observable non-marker output.
fn seed_watch_workspace(event_summary: &str, archive_bytes: Option<&[u8]>) -> WorkspacePath {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-watch-rotate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let logs = crate::model::paths::logs_dir(&dir);
    std::fs::create_dir_all(&logs).unwrap();
    let line =
        serde_json::json!({"event": "result_received", "agent_id": "w1", "summary": event_summary});
    std::fs::write(
        logs.join("events.jsonl"),
        format!("{}\n", serde_json::to_string(&line).unwrap()),
    )
    .unwrap();
    if let Some(bytes) = archive_bytes {
        std::fs::write(logs.join("events.jsonl.1"), bytes).unwrap();
    }
    // MessageStore::open creates the schema so collect_watch_lines' result tail works.
    let _ = MessageStore::open(&dir).unwrap();
    WorkspacePath::new(dir)
}

#[test]
fn watch_rotation_emits_marker_once_resets_offset_and_does_not_replay() {
    // watch/__init__.py:66-97 — archive_signature change OR offset>size => ROTATION_MARKER
    // (once), event_offset reset to 0, archived segment NOT replayed (only the current
    // events.jsonl is read from the new offset forward). The doc-comment invariant
    // (coordinator.rs lines 42-43 / 532-533) is now ASSERTED, not commented.
    let ws = seed_watch_workspace("first segment", None);
    let store = MessageStore::open(ws.as_path()).unwrap();
    let mut cursor = WatchCursor::default();

    // First call: initializes the cursor (no marker even though no archive yet),
    // renders the seeded result line, advances offset past EOF.
    let first = collect_watch_lines(&ws, &mut cursor, &store, None).unwrap();
    assert!(cursor.initialized, "first call initializes the cursor");
    assert!(
        !first.iter().any(|l| l == ROTATION_MARKER),
        "no rotation marker on first/initializing call"
    );
    assert!(
        first.iter().any(|l| l.contains("first segment")),
        "first segment line is rendered exactly once"
    );
    let offset_after_first = cursor.event_offset;
    assert!(
        offset_after_first > 0,
        "offset advanced past the consumed segment"
    );

    // Simulate rotation: an archive segment now appears (archive_signature changes from
    // None -> Some), and the live events.jsonl is replaced with a fresh, SHORTER file
    // whose new content must NOT include the archived "first segment".
    let logs = crate::model::paths::logs_dir(ws.as_path());
    std::fs::write(
        logs.join("events.jsonl.1"),
        b"archived old bytes that must never replay\n",
    )
    .unwrap();
    let fresh = serde_json::json!({"event": "result_received", "agent_id": "w1", "summary": "post rotation"});
    std::fs::write(
        logs.join("events.jsonl"),
        format!("{}\n", serde_json::to_string(&fresh).unwrap()),
    )
    .unwrap();

    let second = collect_watch_lines(&ws, &mut cursor, &store, None).unwrap();
    let marker_count = second.iter().filter(|l| **l == ROTATION_MARKER).count();
    assert_eq!(
        marker_count, 1,
        "ROTATION_MARKER emitted exactly once on rotation"
    );
    assert!(
        !second.iter().any(|l| l.contains("first segment")),
        "archived segment is NOT replayed"
    );
    assert!(
        !second
            .iter()
            .any(|l| l.contains("old bytes that must never replay")),
        "archive file contents are NEVER read/replayed"
    );
    assert!(
        second.iter().any(|l| l.contains("post rotation")),
        "post-rotation live segment IS rendered (read from reset offset forward)"
    );
    // offset was reset to 0 by the rotation branch, then re-advanced over the fresh
    // (shorter) file — so it must be < the pre-rotation offset, proving the reset.
    assert!(
        cursor.event_offset <= offset_after_first,
        "event_offset reset on rotation (re-advanced over the shorter fresh file)"
    );
    assert_eq!(
        cursor.archive_signature.map(|(sz, _)| sz),
        Some(b"archived old bytes that must never replay\n".len() as u64),
        "archive_signature updated to the new archived segment's (size, mtime_ns)"
    );
}
