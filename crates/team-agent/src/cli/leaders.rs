//! ---
//! purpose: `team-agent leaders` 发现面。人读默认面按节点列出 workspace 全路径、
//!   team/leader、可复制 `team-agent send '<ws>::<team>/leader' '<消息>'`。
//! contract:
//!   provides:
//!     - name: cmd_leaders
//!       what: --json 保持 E7 字段(含 hash/send_hint); 默认 stdout 只有三要素、不含 hash/channel/pane id
//! boundary:
//!   - 不改 send 解析; 不把 --json 字段从默认面漏出去
//!   - Leaders-only; worker 行仍缺席
//! ---
//!
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

///
/// E7 CLI entry point (E7 test 2 marker: `cmd_leaders`).
///
/// Reads registry entries, canonical-validates each, and groups by
/// delivery_name to classify each as LIVE / STALE / AMBIGUOUS. Terminal
/// stale entries (canonical workspace/team gone) are pruned as GC side
/// effect. Leader-detached-but-team-alive stays visible as STALE so the
/// operator can decide.
pub fn cmd_leaders(args: &LeadersArgs) -> Result<CmdResult, CliError> {
    let dir = crate::leader::registry::registry_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.team-agent/leaders".to_string());
    let classified = crate::leader::registry::list_validated_with_gc();
    let mut name_counts: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for (entry, _status, _reason) in &classified {
        name_counts
            .entry(entry.delivery_name.clone())
            .or_default()
            .push(json!({
                "name": entry.qualified_name,
                "workspace": entry.workspace.display().to_string(),
                "team_key": entry.team_key,
                "workspace_hash": entry.workspace_hash,
                "stable_qualified_name": entry.stable_qualified_name,
            }));
    }
    let ambiguous_map: std::collections::HashMap<String, Vec<Value>> = name_counts
        .iter()
        .filter(|(_, list)| list.len() > 1)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut leaders: Vec<Value> = Vec::new();
    for (entry, status, reason) in &classified {
        let final_status = if ambiguous_map.contains_key(&entry.delivery_name) {
            "AMBIGUOUS"
        } else {
            *status
        };
        let send_hint = format!(
            "team-agent send --to-leader {} MESSAGE",
            entry.qualified_name
        );
        leaders.push(json!({
            "name": entry.delivery_name,
            "delivery_name": entry.delivery_name,
            "qualified_name": entry.qualified_name,
            "stable_qualified_name": entry.stable_qualified_name,
            "workspace": entry.workspace.display().to_string(),
            "workspace_hash": entry.workspace_hash,
            "workspace_short": entry.workspace_short,
            "team_key": entry.team_key,
            "transport_kind": entry.transport_kind,
            "owner_epoch": entry.owner_epoch,
            "status": final_status,
            "stale_reason": reason.clone(),
            "send_hint": send_hint,
        }));
    }
    let ambiguous_names: Vec<Value> = ambiguous_map
        .into_iter()
        .map(|(name, candidates)| {
            json!({
                "name": name,
                "candidates": candidates,
            })
        })
        .collect();
    let value = json!({
        "ok": true,
        "registry_dir": dir,
        "leaders": leaders,
        "ambiguous_names": ambiguous_names,
        // Status vocabulary pinned by E7 RED: LIVE / STALE / AMBIGUOUS.
        // "stale_reason" is null for LIVE and carries a machine-readable
        // reason for STALE/AMBIGUOUS entries.
        "status_wire_values": ["LIVE", "STALE", "AMBIGUOUS"],
    });
    if args.json {
        return Ok(CmdResult::from_json(value, true));
    }
    let rows: Vec<(String, String)> = classified
        .iter()
        .map(|(entry, _, _)| {
            (
                entry.workspace.display().to_string(),
                entry.team_key.clone(),
            )
        })
        .collect();
    Ok(CmdResult::human(format_leaders_human(&rows)))
}

fn quote_cli_arg(raw: &str) -> String {
    if !raw.contains('\'') {
        format!("'{raw}'")
    } else {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Default human face: workspace path, `<team>/leader`, copyable send line.
fn format_leaders_human(rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return "no registered leaders\n".to_string();
    }
    let mut out = String::new();
    for (workspace, team_key) in rows {
        let role = format!("{team_key}/leader");
        let to = format!("{workspace}::{role}");
        out.push_str(workspace);
        out.push('\n');
        out.push_str(&role);
        out.push('\n');
        out.push_str("team-agent send ");
        out.push_str(&quote_cli_arg(&to));
        out.push(' ');
        out.push_str(&quote_cli_arg("消息"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_human() -> String {
        format_leaders_human(&[
            (
                "/Volumes/nvme/Projects/讨论team-agent".to_string(),
                "wiki-team".to_string(),
            ),
            (
                "/Users/alauda/Documents/code/agent前沿探索/多agent协作".to_string(),
                "refactor-maintainability".to_string(),
            ),
        ])
    }

    #[test]
    fn leaders_human_lists_workspace_team_role_and_copyable_send() {
        let out = sample_human();
        assert!(
            out.contains("team-agent send '/"),
            "A: copyable send TO must start with absolute workspace path; got {out:?}"
        );
        assert!(
            out.contains("::wiki-team/leader'"),
            "B: send TO must contain ::<team>/<role>; got {out:?}"
        );
        assert!(
            out.contains(
                "team-agent send '/Volumes/nvme/Projects/讨论team-agent::wiki-team/leader' '消息'"
            ),
            "C: complete copyable send line; got {out:?}"
        );
        assert!(out.contains("/Volumes/nvme/Projects/讨论team-agent\nwiki-team/leader\n"));
        assert!(out.contains(
            "/Users/alauda/Documents/code/agent前沿探索/多agent协作\nrefactor-maintainability/leader\n"
        ));
        assert!(out.contains(
            "team-agent send '/Users/alauda/Documents/code/agent前沿探索/多agent协作::refactor-maintainability/leader' '消息'"
        ));
    }

    #[test]
    fn leaders_human_omits_hash_channel_and_pane_id_fields() {
        let out = sample_human();
        for noise in [
            "workspace_hash",
            "stable_qualified_name",
            "channel_id",
            "\"channel\":",
            "pane_id",
        ] {
            assert!(
                !out.contains(noise),
                "D: default surface must not print {noise}; got {out:?}"
            );
        }
        let empty = format_leaders_human(&[]);
        for noise in [
            "workspace_hash",
            "stable_qualified_name",
            "channel_id",
            "\"channel\":",
            "pane_id",
        ] {
            assert!(!empty.contains(noise), "empty surface leaked {noise}");
        }
    }
}
