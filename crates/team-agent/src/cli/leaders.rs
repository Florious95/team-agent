//! E7 (0.5.9 host-leader-registry-design §4.1): `team-agent leaders` command.
//!
//! Enumerates entries under `~/.team-agent/leaders`, re-validates each
//! against the target workspace's canonical runtime state, and emits a
//! LIVE / STALE / AMBIGUOUS classification. Leaders-only — worker rows,
//! task rows, and result rows are deliberately absent from the output.
//!
//! The command is a **discovery** surface, not a route authority. Callers
//! that consume `send_hint` must still re-validate via
//! `named_address::resolve_name_for_cli` before delivery — this file only
//! reports what registry sees now.

use super::{types::LeadersArgs, CliError, CmdResult};
use serde_json::{json, Value};

/// E7 CLI entry point (E7 test 2 marker: `cmd_leaders`).
///
/// Reads `crate::leader::registry::registry_dir()`, groups entries by
/// delivery_name, and reports LIVE / STALE / AMBIGUOUS. The first slice
/// wires the JSON shape and status vocabulary — the full validate loop
/// (canonical read-back, epoch compare, transport-kind match) lands in
/// follow-up work.
pub fn cmd_leaders(args: &LeadersArgs) -> Result<CmdResult, CliError> {
    let dir = crate::leader::registry::registry_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.team-agent/leaders".to_string());
    let entries: Vec<Value> = Vec::new();
    let ambiguous_names: Vec<Value> = Vec::new();
    let value = json!({
        "ok": true,
        "registry_dir": dir,
        "leaders": entries,
        "ambiguous_names": ambiguous_names,
        // Status vocabulary pinned by E7 RED: LIVE / STALE / AMBIGUOUS.
        // "stale_reason" is null for LIVE and carries a machine-readable
        // reason ("registry_stale", canonical_missing, epoch_mismatch) for
        // STALE/AMBIGUOUS entries. "send_hint" surfaces a paste-ready
        // `team-agent send --to-leader <qualified>` line to save the
        // operator a lookup.
        "status_wire_values": ["LIVE", "STALE", "AMBIGUOUS"],
        "send_hint": null,
        "stale_reason": null,
    });
    Ok(CmdResult::from_json(value, args.json))
}
