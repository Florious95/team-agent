use super::*;

// =============================================================================
// P0-1 LANE — `allow-peer-talk` byte-lock (was ZERO-test; a SHIPPED compat-noop).
//
// golden: cli/commands.py:422 -> messaging/leader.py:25 allow_peer_talk(ws,a,b)
//   -> message_store/core.py:326 allow_peer (INSERT OR IGNORE BOTH (a,b) & (b,a))
//   + events.py:27-35 EventLog.write (sort_keys=True -> .team/logs/events.jsonl).
//
// Golden bytes captured live (PYTHONPATH=.../src python3 /tmp/probe_app*.py):
//   return (insertion order):  {"ok":true,"a":..,"b":..,"status":"compat_noop",
//                               "reason":"team_scoped_peer_messages_enabled"}
//   return (--json sort_keys):  keys a,b,ok,reason,status
//   event line (sort_keys):     {"a":..,"b":..,"event":"communication.peer_allowed","ts":..}
//   idempotent:                 2nd call -> identical return + a SECOND event line
//                               (event fires per-call; DB rows are insert-or-ignore)
//   unknown/undeclared agents:  NO validation -> ok:true compat_noop, exit 0
//
// Rust impl: messaging/peers.rs:11 + message_store.rs:251 + adapters.rs:173.
// serde_json `preserve_order` is ON (Cargo.toml:24) so the default (human) output
// iterates dict keys in golden insertion order. These tests LOCK the contract so a
// porter/refactor cannot silently drift this live verb.
// =============================================================================

fn app_out(r: CmdResult) -> serde_json::Value {
    match r.output {
        CmdOutput::Json(v) => v,
        _ => panic!("allow-peer-talk must emit a Json CmdOutput"),
    }
}

fn app_events(ws: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(ws.join(".team").join("logs").join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn app_call(ws: &std::path::Path, a: &str, b: &str) -> CmdResult {
    cmd_allow_peer_talk(&AllowPeerTalkArgs {
        a: a.to_string(),
        b: b.to_string(),
        workspace: ws.to_path_buf(),
        json: false,
        team: None,
    })
    .expect("allow-peer-talk is a compat-noop and must not error")
}

// ── return dict: exact compat_noop shape + golden insertion order (drives human emit) ──
#[test]
fn allow_peer_talk_output_byte_locks_golden_compat_noop() {
    let ws = tmp_workspace();
    let v = app_out(app_call(&ws, "alpha", "bravo"));
    assert_eq!(
        v,
        json!({
            "ok": true,
            "a": "alpha",
            "b": "bravo",
            "status": "compat_noop",
            "reason": "team_scoped_peer_messages_enabled"
        }),
        "golden leader.py:28 return dict (key set + values); got {v:?}"
    );
    let order: Vec<&str> = v.as_object().expect("object").keys().map(String::as_str).collect();
    assert_eq!(
        order,
        vec!["ok", "a", "b", "status", "reason"],
        "golden dict insertion order ok,a,b,status,reason drives the default human emit key order \
         (helpers.py:16-19); got {order:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ── event: exactly one `communication.peer_allowed` line, sort_keys byte form, payload {a,b} ──
#[test]
fn allow_peer_talk_event_byte_locks_golden_line() {
    let ws = tmp_workspace();
    let _ = app_call(&ws, "alpha", "bravo");
    let lines = app_events(&ws);
    assert_eq!(lines.len(), 1, "exactly one event per call; got {lines:?}");
    let v: serde_json::Value = serde_json::from_str(&lines[0]).expect("event line must be json");
    let ts = v["ts"].as_str().expect("event has string ts").to_string();
    let expected = format!(
        "{{\"a\": \"alpha\", \"b\": \"bravo\", \"event\": \"communication.peer_allowed\", \"ts\": \"{ts}\"}}"
    );
    assert_eq!(
        lines[0], expected,
        "golden events.py:35 json.dumps(...,sort_keys=True) line: sorted keys a,b,event,ts with \
         Python `, `/`: ` spacing; got {}",
        lines[0]
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ── idempotency: identical return, event fires on EVERY call (rows are insert-or-ignore) ──
#[test]
fn allow_peer_talk_is_idempotent_and_fires_event_per_call() {
    let ws = tmp_workspace();
    let v1 = app_out(app_call(&ws, "alpha", "bravo"));
    let v2 = app_out(app_call(&ws, "alpha", "bravo")); // duplicate pair
    assert_eq!(v1, v2, "repeated allow returns identical compat_noop output");
    let lines = app_events(&ws);
    assert_eq!(
        lines.len(),
        2,
        "golden fires communication.peer_allowed on EVERY call (event NOT deduped; only DB rows \
         are insert-or-ignore); got {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ── permissive: undeclared/unknown agents still succeed (golden has NO validation path) ──
#[test]
fn allow_peer_talk_unknown_agents_succeed_no_validation() {
    let ws = tmp_workspace(); // no state.json / no spec: agents never declared
    let r = app_call(&ws, "ghost", "phantom-x");
    assert_eq!(
        r.exit,
        ExitCode::Ok,
        "golden leader.py:25-28 never validates agent ids -> no error, exit 0"
    );
    let v = app_out(r);
    assert_eq!(
        v,
        json!({
            "ok": true,
            "a": "ghost",
            "b": "phantom-x",
            "status": "compat_noop",
            "reason": "team_scoped_peer_messages_enabled"
        }),
        "unknown agents still get compat_noop (no unknown-agent error string/exit); got {v:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn allow_peer_talk_explicit_team_is_rejected_until_backend_is_scoped() {
    let ws = tmp_workspace();
    let err = cmd_allow_peer_talk(&AllowPeerTalkArgs {
        a: "alpha".to_string(),
        b: "bravo".to_string(),
        workspace: ws.clone(),
        json: true,
        team: Some("current".to_string()),
    })
    .expect_err("explicit --team must not silently write the global peer allowlist");
    assert!(
        err.to_string().contains("not supported yet"),
        "allow-peer-talk --team must be an explicit refusal until backend supports scoped writes; got {err}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
