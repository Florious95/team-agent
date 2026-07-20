//! RED contract: status agent-detail error PRIORITY must be stable across the
//! smell-34 status_port split.
//!
//! Attack input (case: r12 BLOCKING on the dependency-cycle-break 0280c319):
//! `team-agent status --agent <unknown>` against a workspace whose messages
//! table holds a row with a corrupt `requires_ack` (TEXT where INTEGER is
//! expected). The unknown-agent decision MUST fire before any inbox row is
//! decoded, so the user sees a clean "unknown agent id" error — a malformed
//! inbox row must never turn a status query into a SQLite decode panic/error.
//!
//! This pins the ordering that the mechanical split silently reversed:
//! before, `format_agent_status` checked `known` then opened the inbox;
//! after, the caller read the inbox first. Same product behavior => same
//! error precedence.

use serde_json::json;
use std::path::Path;

/// Seed a workspace whose runtime state knows NO agents, and whose messages
/// table has one row addressed to `ghost` with a corrupt `requires_ack`
/// (stored as TEXT). `status` is a valid string so the aggregate counts that
/// `assemble` computes stay clean — the ONLY landmine is the inbox row decode.
fn seed(ws: &Path) {
    let store = team_agent::message_store::MessageStore::open(ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into messages \
         (message_id, owner_team_id, sender, recipient, content, status, created_at, requires_ack, delivery_attempts) \
         values ('m1', '', 'leaderX', 'ghost', 'hi', 'accepted', '2026-01-01T00:00:00Z', 'not-an-int', 0)",
        [],
    )
    .unwrap();
}

#[test]
fn unknown_agent_is_reported_before_a_corrupt_inbox_row_is_decoded() {
    let dir =
        std::env::temp_dir().join(format!("status-errprio-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);

    // No agents known -> "ghost" is unknown.
    let state = json!({ "agents": {}, "agent_health": {} });

    let result =
        team_agent::cli::status_port::format_status_scoped(&dir, &state, None, Some("ghost"));
    let _ = std::fs::remove_dir_all(&dir);

    let err = match result {
        Ok(rendered) => {
            panic!("expected an unknown-agent error, got a rendered status block:\n{rendered}")
        }
        Err(e) => e.to_string(),
    };

    // Priority contract: the unknown-agent branch must win. If the split let
    // the inbox read run first, this is a SQLite decode error (mentioning a
    // column / type), not the unknown-agent message.
    assert!(
        err.contains("unknown agent"),
        "status --agent <unknown> must report the unknown agent BEFORE decoding \
         any inbox row; a corrupt requires_ack must not surface as a SQLite error. \
         got: {err}"
    );
    let lowered = err.to_lowercase();
    assert!(
        !(lowered.contains("column")
            || lowered.contains("sqlite")
            || lowered.contains("invalid column type")
            || lowered.contains("decode")),
        "a malformed inbox row leaked a storage-layer decode error into `status`: {err}"
    );
}
