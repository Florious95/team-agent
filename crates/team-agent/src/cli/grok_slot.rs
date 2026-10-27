//! ---
//! purpose: 对账 .grok/config.toml 残余共享槽与升级前遗留的在役 grok 席，含未首 turn 危险窗
//! contract:
//!   provides:
//!     - name: grok_slot_report
//!       what: 盘上 ID/OWNER/AUTH、在役 grok、pre_first_turn；读不出为 unjudgeable
//! boundary:
//!   - 读不出不报一致
//!   - 不写盘、不改 overlay；legacy grok worker 已退役，本文件只做观测，
//!     帮助清理升级前留下的席位与共享槽（grok_build 叶子不写这份 toml）
//! maturity: wired
//! ---

use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokSlotReport {
    pub readable: bool,
    pub consistent: bool,
    pub disk_team_agent_id: Option<String>,
    pub disk_owner_team_id: Option<String>,
    pub disk_auth_mode: Option<String>,
    pub expected_owner_team_id: Option<String>,
    pub expected_auth_mode: Option<String>,
    pub live_seats: Vec<String>,
    pub pre_first_turn: Vec<String>,
    pub reason: String,
}

impl GrokSlotReport {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "readable": self.readable,
            "consistent": self.consistent,
            "disk_team_agent_id": self.disk_team_agent_id,
            "disk_owner_team_id": self.disk_owner_team_id,
            "disk_auth_mode": self.disk_auth_mode,
            "expected_owner_team_id": self.expected_owner_team_id,
            "expected_auth_mode": self.expected_auth_mode,
            "live_seats": self.live_seats,
            "pre_first_turn": self.pre_first_turn,
            "reason": self.reason,
        })
    }
}

pub(crate) fn grok_slot_report(workspace: &Path, state: &Value) -> GrokSlotReport {
    if let Some(reason) = unreadable_state_reason(workspace, state) {
        return unjudgeable(reason, Vec::new(), Vec::new());
    }
    let live = live_grok_from_state(state);
    let pre_first_turn = live
        .iter()
        .filter(|seat| seat.pre_first_turn)
        .map(|seat| seat.id.clone())
        .collect::<Vec<_>>();
    let live_ids = live.iter().map(|seat| seat.id.clone()).collect::<Vec<_>>();
    let expected_owner = expected_owner_team_id(state);
    let expected_auth = expected_auth_mode(&live);

    let toml_path = workspace.join(".grok").join("config.toml");
    if toml_path.exists() {
        match std::fs::read_to_string(&toml_path) {
            Err(error) => {
                return unjudgeable(
                    format!("unjudgeable: cannot read {} ({error})", toml_path.display()),
                    live_ids,
                    pre_first_turn,
                );
            }
            Ok(text) => {
                let disk = parse_disk_slot(&text);
                return classify(disk, live_ids, pre_first_turn, expected_owner, expected_auth);
            }
        }
    }
    classify(
        DiskSlot::default(),
        live_ids,
        pre_first_turn,
        expected_owner,
        expected_auth,
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DiskSlot {
    team_agent_id: Option<String>,
    owner_team_id: Option<String>,
    auth_mode: Option<String>,
    extras: Vec<(String, String)>,
}

/// Shared identity keys belong on pane env. The only TEAM_AGENT_* key
/// allowed in the directory-scoped grok toml is WORKSPACE (same cwd).
fn is_per_seat_env_key(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("TEAM_AGENT_") && key != "TEAM_AGENT_WORKSPACE"
}

/// Per-seat keys of every `*.env` table, first-seen order, values unquoted.
fn per_seat_keys_in_toml(text: &str) -> Vec<(String, String)> {
    let mut in_env_table = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            in_env_table = name.ends_with(".env");
            continue;
        }
        if !in_env_table {
            continue;
        }
        let Some((key, rest)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_per_seat_env_key(key) {
            continue;
        }
        let rest = rest.trim();
        let value = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(rest);
        if out.iter().any(|(existing, _)| existing == key) {
            continue;
        }
        out.push((key.to_string(), value.to_string()));
    }
    out
}

fn parse_disk_slot(text: &str) -> DiskSlot {
    let keys = per_seat_keys_in_toml(text);
    DiskSlot {
        team_agent_id: find_key(&keys, "TEAM_AGENT_ID"),
        owner_team_id: find_key(&keys, "TEAM_AGENT_OWNER_TEAM_ID"),
        auth_mode: find_key(&keys, "TEAM_AGENT_AUTH_MODE"),
        extras: keys
            .into_iter()
            .filter(|(key, _)| {
                key != "TEAM_AGENT_ID"
                    && key != "TEAM_AGENT_OWNER_TEAM_ID"
                    && key != "TEAM_AGENT_AUTH_MODE"
            })
            .collect(),
    }
}

fn find_key(keys: &[(String, String)], want: &str) -> Option<String> {
    keys.iter()
        .find(|(key, _)| key == want)
        .map(|(_, value)| value.clone())
}

fn unjudgeable(
    reason: String,
    live_seats: Vec<String>,
    pre_first_turn: Vec<String>,
) -> GrokSlotReport {
    GrokSlotReport {
        readable: false,
        consistent: false,
        disk_team_agent_id: None,
        disk_owner_team_id: None,
        disk_auth_mode: None,
        expected_owner_team_id: None,
        expected_auth_mode: None,
        live_seats,
        pre_first_turn,
        reason,
    }
}

fn unreadable_state_reason(workspace: &Path, state: &Value) -> Option<String> {
    let path = workspace.join(".team").join("runtime").join("state.json");
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Err(error) => Some(format!(
            "unjudgeable: cannot read {} ({error})",
            path.display()
        )),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Err(error) => Some(format!(
                "unjudgeable: cannot parse {} ({error})",
                path.display()
            )),
            Ok(_) if state.is_null() => {
                Some("unjudgeable: runtime state exists but could not be loaded".to_string())
            }
            Ok(_) => None,
        },
    }
}

fn classify(
    disk: DiskSlot,
    live_ids: Vec<String>,
    pre_first_turn: Vec<String>,
    expected_owner: Option<String>,
    expected_auth: Option<String>,
) -> GrokSlotReport {
    let mut mismatches = Vec::new();
    if let Some(disk_id) = disk.team_agent_id.as_deref() {
        mismatches.push(format!("per-seat key TEAM_AGENT_ID={disk_id}"));
    }
    if let Some(disk_owner) = disk.owner_team_id.as_deref() {
        mismatches.push(format!("per-seat key TEAM_AGENT_OWNER_TEAM_ID={disk_owner}"));
    }
    if let Some(disk_auth) = disk.auth_mode.as_deref() {
        mismatches.push(format!("per-seat key TEAM_AGENT_AUTH_MODE={disk_auth}"));
    }
    for (key, value) in &disk.extras {
        mismatches.push(format!("per-seat key {key}={value}"));
    }

    if mismatches.is_empty() {
        let reason =
            "toml carries no per-seat identity keys; identity inherits from pane env".to_string();
        return GrokSlotReport {
            readable: true,
            consistent: true,
            disk_team_agent_id: disk.team_agent_id,
            disk_owner_team_id: disk.owner_team_id,
            disk_auth_mode: disk.auth_mode,
            expected_owner_team_id: expected_owner,
            expected_auth_mode: expected_auth,
            live_seats: live_ids,
            pre_first_turn,
            reason,
        };
    }

    GrokSlotReport {
        readable: true,
        consistent: false,
        disk_team_agent_id: disk.team_agent_id,
        disk_owner_team_id: disk.owner_team_id,
        disk_auth_mode: disk.auth_mode,
        expected_owner_team_id: expected_owner,
        expected_auth_mode: expected_auth,
        live_seats: live_ids,
        pre_first_turn,
        reason: format!("grok slot mismatch: {}", mismatches.join("; ")),
    }
}

struct LiveGrok {
    id: String,
    pre_first_turn: bool,
    auth_mode: Option<String>,
}

fn live_grok_from_state(state: &Value) -> Vec<LiveGrok> {
    let mut out = Vec::new();
    push_live_from_map(state.get("agents").and_then(Value::as_object), &mut out);
    if let Some(teams) = state.get("teams").and_then(Value::as_object) {
        for team in teams.values() {
            push_live_from_map(team.get("agents").and_then(Value::as_object), &mut out);
        }
    }
    out
}

fn push_live_from_map(agents: Option<&serde_json::Map<String, Value>>, out: &mut Vec<LiveGrok>) {
    let Some(agents) = agents else {
        return;
    };
    for (id, agent) in agents {
        if out.iter().any(|row| row.id == *id) {
            continue;
        }
        let provider = agent.get("provider").and_then(Value::as_str).unwrap_or("");
        if provider != "grok" {
            continue;
        }
        let status = agent
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("running");
        if matches!(
            status,
            "stopped" | "stopping" | "removed" | "spawn_failed" | "failed"
        ) {
            continue;
        }
        let first = agent
            .get("first_send_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        out.push(LiveGrok {
            id: id.clone(),
            pre_first_turn: first.is_none(),
            auth_mode: agent_auth_mode(agent),
        });
    }
}

fn agent_auth_mode(agent: &Value) -> Option<String> {
    match agent.get("auth_mode") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(other) if !other.is_null() => Some(other.to_string().trim_matches('"').to_string()),
        _ => None,
    }
}

fn expected_owner_team_id(state: &Value) -> Option<String> {
    for key in ["active_team_key", "team_key", "TEAM_AGENT_OWNER_TEAM_ID"] {
        if let Some(value) = state
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(value.to_string());
        }
    }
    None
}

fn expected_auth_mode(live: &[LiveGrok]) -> Option<String> {
    let mut found = None;
    for seat in live {
        let Some(auth) = seat.auth_mode.as_deref() else {
            continue;
        };
        match found {
            None => found = Some(auth.to_string()),
            Some(ref existing) if existing != auth => return None,
            Some(_) => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_is_shared_other_team_agent_keys_are_per_seat() {
        assert!(!is_per_seat_env_key("TEAM_AGENT_WORKSPACE"));
        assert!(is_per_seat_env_key("TEAM_AGENT_ID"));
        assert!(is_per_seat_env_key("TEAM_AGENT_OWNER_TEAM_ID"));
        assert!(is_per_seat_env_key("TEAM_AGENT_AUTH_MODE"));
        assert!(is_per_seat_env_key("TEAM_AGENT_FUTURE_SEAT_KEY"));
        assert!(!is_per_seat_env_key("GROK_FOLDER_TRUST"));
    }

    #[test]
    fn scan_finds_per_seat_keys_in_env_table_only() {
        let text = r#"
[mcp_servers.other.env]
TEAM_AGENT_ID = "ignore-other-server-still-counts"

[mcp_servers.team_orchestrator]
command = "/bin/x"

[mcp_servers.team_orchestrator.env]
TEAM_AGENT_WORKSPACE = "/ws"
TEAM_AGENT_AUTH_MODE = "impostor"
"#;
        let keys = per_seat_keys_in_toml(text);
        assert!(
            keys.iter()
                .any(|(k, v)| k == "TEAM_AGENT_AUTH_MODE" && v == "impostor"),
            "must see AUTH; keys={keys:?}"
        );
        assert!(
            keys.iter().any(|(k, _)| k == "TEAM_AGENT_ID"),
            "any env table per-seat key counts; keys={keys:?}"
        );
        assert!(
            !keys.iter().any(|(k, _)| k == "TEAM_AGENT_WORKSPACE"),
            "WORKSPACE is shared; keys={keys:?}"
        );
    }
}
