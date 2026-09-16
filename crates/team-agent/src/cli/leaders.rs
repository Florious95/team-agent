//! `team-agent leaders` host-level discovery and registry maintenance.
//!
//! Listing is read-only: it validates the derived registry index without
//! deleting entries. Explicit `--prune` is the only path that removes entries.
//! The command is a discovery surface, not a route authority; send callers
//! continue to re-validate through the named-address resolver.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{
    types::{LeadersArgs, LeadersView},
    CliError, CmdResult, ExitCode,
};
use crate::leader::registry::{LeaderRegistryEntry, RegistryPruneItem, RegistryPruneReport};

#[derive(Debug, Clone)]
struct LeaderRow {
    entry: LeaderRegistryEntry,
    status: &'static str,
    stale_reason: Option<String>,
    send_hint: String,
}

/// E7 CLI entry point.
///
/// The default view contains only canonical-live entries. `--all` preserves
/// live and retained stale entries, while `--stale` selects only stale rows.
/// None of the listing views mutates the host registry.
pub fn cmd_leaders(args: &LeadersArgs) -> Result<CmdResult, CliError> {
    let dir = crate::leader::registry::registry_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.team-agent/leaders".to_string());
    if args.prune {
        return cmd_prune(args, &dir);
    }

    let mut classified = crate::leader::registry::list_validated_no_gc();
    classified.sort_by(|left, right| {
        left.0
            .workspace
            .cmp(&right.0.workspace)
            .then_with(|| left.0.team_key.cmp(&right.0.team_key))
    });
    let mut active_candidates: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (entry, status, _) in &classified {
        if *status == "LIVE" {
            active_candidates
                .entry(entry.delivery_name.clone())
                .or_default()
                .push(candidate_json(entry));
        }
    }
    let ambiguous_names = active_candidates
        .iter()
        .filter(|(_, candidates)| candidates.len() > 1)
        .map(|(name, candidates)| {
            json!({
                "name": name,
                "candidates": candidates,
            })
        })
        .collect::<Vec<_>>();

    let rows = classified
        .into_iter()
        .map(|(entry, status, reason)| {
            let ambiguous = status == "LIVE"
                && active_candidates
                    .get(&entry.delivery_name)
                    .is_some_and(|candidates| candidates.len() > 1);
            let status = if ambiguous { "AMBIGUOUS" } else { status };
            let stale_reason = if ambiguous {
                Some(crate::leader::registry::REASON_AMBIGUOUS.to_string())
            } else {
                reason
            };
            let send_hint = if status == "STALE" {
                "-".to_string()
            } else {
                send_hint(&entry)
            };
            LeaderRow {
                entry,
                status,
                stale_reason,
                send_hint,
            }
        })
        .collect::<Vec<_>>();
    let rows = filter_rows(rows, args.view, args.query.as_deref());

    let leaders = rows.iter().map(row_json).collect::<Vec<_>>();
    let value = json!({
        "ok": true,
        "registry_dir": dir,
        "leaders": leaders,
        "ambiguous_names": if matches!(args.view, LeadersView::Stale) {
            Vec::<Value>::new()
        } else {
            ambiguous_names
        },
        "status_wire_values": ["LIVE", "STALE", "AMBIGUOUS"],
    });
    if args.json {
        return Ok(CmdResult::from_json(value, true));
    }
    Ok(CmdResult::human(format_leaders_human(&rows, args.view)))
}

fn candidate_json(entry: &LeaderRegistryEntry) -> Value {
    json!({
        "name": entry.qualified_name,
        "workspace": entry.workspace.display().to_string(),
        "team_key": entry.team_key,
        "workspace_hash": entry.workspace_hash,
        "stable_qualified_name": entry.stable_qualified_name,
    })
}

fn send_hint(entry: &LeaderRegistryEntry) -> String {
    let to = format!("{}::{}/leader", entry.workspace.display(), entry.team_key);
    format!(
        "team-agent send {} {}",
        quote_cli_arg(&to),
        quote_cli_arg("消息")
    )
}

fn row_json(row: &LeaderRow) -> Value {
    json!({
        "name": row.entry.delivery_name,
        "delivery_name": row.entry.delivery_name,
        "qualified_name": row.entry.qualified_name,
        "stable_qualified_name": row.entry.stable_qualified_name,
        "workspace": row.entry.workspace.display().to_string(),
        "workspace_hash": row.entry.workspace_hash,
        "workspace_short": row.entry.workspace_short,
        "team_key": row.entry.team_key,
        "transport_kind": row.entry.transport_kind,
        "owner_epoch": row.entry.owner_epoch,
        "status": row.status,
        "stale_reason": row.stale_reason,
        "send_hint": row.send_hint,
    })
}

fn filter_rows(rows: Vec<LeaderRow>, view: LeadersView, query: Option<&str>) -> Vec<LeaderRow> {
    let query = query.map(str::to_lowercase);
    rows.into_iter()
        .filter(|row| match view {
            LeadersView::Live => row.status == "LIVE" || row.status == "AMBIGUOUS",
            LeadersView::All => true,
            LeadersView::Stale => row.status == "STALE",
        })
        .filter(|row| {
            let Some(query) = query.as_deref() else {
                return true;
            };
            [
                row.entry.workspace.display().to_string(),
                row.entry.team_key.clone(),
                row.entry.qualified_name.clone(),
                row.entry.delivery_name.clone(),
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(query))
        })
        .collect()
}

fn quote_cli_arg(raw: &str) -> String {
    // POSIX single-quoted strings are literal except for the quote itself;
    // split and escape quotes rather than falling back to unsafe double quotes.
    format!("'{}'", raw.replace('\'', "'\\''"))
}

/// Human face: status, complete workspace path, canonical team key, and a
/// copyable SEND command. Tabs keep paths and commands intact without a table
/// dependency or truncation.
fn format_leaders_human(rows: &[LeaderRow], view: LeadersView) -> String {
    if rows.is_empty() {
        return match view {
            LeadersView::Live => "no live leaders\n".to_string(),
            LeadersView::All => "no registered leaders\n".to_string(),
            LeadersView::Stale => "no stale leaders\n".to_string(),
        };
    }
    let mut out = String::from("STATUS\tWORKSPACE\tTEAM\tSEND\n");
    for row in rows {
        out.push_str(row.status);
        out.push('\t');
        out.push_str(&row.entry.workspace.display().to_string());
        out.push('\t');
        out.push_str(&row.entry.team_key);
        out.push('\t');
        out.push_str(&row.send_hint);
        out.push('\n');
    }
    out
}

fn prune_item_json(item: &RegistryPruneItem) -> Value {
    json!({
        "workspace": item.workspace.display().to_string(),
        "team_key": item.team_key,
        "reason": item.reason,
    })
}

fn prune_json(report: &RegistryPruneReport) -> Value {
    json!({
        "dry_run": report.dry_run,
        "candidates": report.candidates.iter().map(prune_item_json).collect::<Vec<_>>(),
        "removed": report.removed.iter().map(prune_item_json).collect::<Vec<_>>(),
        "kept": report.kept.iter().map(prune_item_json).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(prune_item_json).collect::<Vec<_>>(),
        "errors": report.errors.iter().map(prune_item_json).collect::<Vec<_>>(),
    })
}

fn format_prune_human(report: &RegistryPruneReport) -> String {
    let mut out = format!("dry-run: {}\n", report.dry_run);
    for (label, items) in [
        ("candidates", &report.candidates),
        ("removed", &report.removed),
        ("kept", &report.kept),
        ("skipped", &report.skipped),
        ("errors", &report.errors),
    ] {
        out.push_str(&format!("{label}: {}\n", items.len()));
        for item in items {
            out.push_str("  ");
            out.push_str(&item.workspace.display().to_string());
            out.push_str("::");
            out.push_str(&item.team_key);
            out.push_str(" [");
            out.push_str(&item.reason);
            out.push_str("]\n");
        }
    }
    out
}

fn cmd_prune(args: &LeadersArgs, dir: &str) -> Result<CmdResult, CliError> {
    let report = crate::leader::registry::prune_registry(args.dry_run);
    let value = json!({
        "ok": report.errors.is_empty(),
        "registry_dir": dir,
        "prune": prune_json(&report),
    });
    if args.json {
        return Ok(CmdResult::from_json(value, true));
    }
    let mut result = CmdResult::human(format_prune_human(&report));
    if !report.errors.is_empty() {
        result.exit = ExitCode::Error;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(workspace: &str, team: &str, status: &'static str, send_hint: &str) -> LeaderRow {
        LeaderRow {
            entry: LeaderRegistryEntry {
                schema_version: crate::leader::registry::REGISTRY_SCHEMA_VERSION,
                delivery_name: team.to_string(),
                qualified_name: format!("workspace/{team}"),
                stable_qualified_name: format!("hash/{team}"),
                aliases: Vec::new(),
                workspace: workspace.into(),
                workspace_hash: "hash".to_string(),
                workspace_short: "workspace".to_string(),
                team_key: team.to_string(),
                transport_kind: "direct_tmux".to_string(),
                channel: Value::Null,
                owner_epoch: 1,
                attached_at: String::new(),
                updated_at: String::new(),
                source: "test".to_string(),
                status: "attached".to_string(),
            },
            status,
            stale_reason: (status == "STALE").then(|| "leader_pane_dead".to_string()),
            send_hint: send_hint.to_string(),
        }
    }

    #[test]
    fn leaders_human_is_a_status_table_and_stale_has_no_send() {
        let rows = vec![
            row(
                "/Volumes/nvme/Projects/讨论team-agent",
                "wiki-team",
                "LIVE",
                "send-live",
            ),
            row("/Users/alauda/stale", "old-team", "STALE", "-"),
        ];
        let out = format_leaders_human(&rows, LeadersView::All);
        assert!(out.starts_with("STATUS\tWORKSPACE\tTEAM\tSEND\n"));
        assert!(out.contains("LIVE\t/Volumes/nvme/Projects/讨论team-agent\twiki-team\tsend-live\n"));
        assert!(out.contains("STALE\t/Users/alauda/stale\told-team\t-\n"));
    }

    #[test]
    fn stale_rows_are_filtered_from_live_view() {
        let rows = vec![
            row("/live", "live", "LIVE", "send"),
            row("/stale", "stale", "STALE", "-"),
        ];
        let rows = filter_rows(rows, LeadersView::Live, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.team_key, "live");
    }

    #[test]
    fn quote_cli_arg_escapes_single_quotes_without_shell_expansion() {
        assert_eq!(quote_cli_arg("a'b $HOME `date`"), "'a'\\''b $HOME `date`'");
    }

    #[test]
    fn search_matches_selected_fields_case_insensitively() {
        let rows = vec![
            row(
                "/Volumes/nvme/Projects/讨论team-agent",
                "Wiki-Team",
                "LIVE",
                "send",
            ),
            row("/Users/alauda/stale", "old-team", "STALE", "-"),
        ];
        let rows = filter_rows(rows, LeadersView::All, Some("WIKI-"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.team_key, "Wiki-Team");
    }

    #[test]
    fn empty_view_names_are_explicit() {
        assert_eq!(
            format_leaders_human(&[], LeadersView::Live),
            "no live leaders\n"
        );
        assert_eq!(
            format_leaders_human(&[], LeadersView::Stale),
            "no stale leaders\n"
        );
        assert_eq!(
            format_leaders_human(&[], LeadersView::All),
            "no registered leaders\n"
        );
    }
}
