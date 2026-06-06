//! leader::inject — `push_idle_reminder` no-op shim.
//!
//! #236 nag_removal (N35): the framework no longer auto-pings the leader pane based on
//! idle-classification inferences. `push_idle_reminder` used to emit the
//! `idle_takeover.reminder` event AND route a "leader takeover may be appropriate"
//! message through the N31/N32 funnel; both were nag outputs synthesized from
//! time/state, not from a real delivery obligation. They are deleted by design —
//! ownership / handover changes only via explicit `claim-leader` / `takeover` commands.
//!
//! The function signature is preserved (callers in coordinator/tick.rs and lifecycle
//! tests still resolve) but the body is a strict no-op: no event, no primitive call,
//! no state mutation. The `_` discards keep clippy quiet and document that the
//! arguments are intentionally ignored under N35.

use std::path::Path;

use serde_json::Value;

use super::{LeaderError, TakeoverReminderResult};

/// #236 nag_removal: was the take-over reminder injector; now a true no-op. Delivery
/// primitives (`report_result` / `send_to_leader` / N31/N32 funnel / `request_human` /
/// broadcast-leader) continue to flow through their own callsites — only the
/// time/state-inferred nag output that lived in this helper is gone.
pub fn push_idle_reminder(
    workspace: &Path,
    state: &Value,
    event_log: &crate::event_log::EventLog,
    result: &TakeoverReminderResult,
) -> Result<(), LeaderError> {
    let _ = (workspace, state, event_log, result);
    Ok(())
}
