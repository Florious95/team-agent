//! ---
//! purpose: 对账 .grok/config.toml 槽位与在役 grok 席，含未首 turn 危险窗
//! contract:
//!   provides:
//!     - name: grok_slot_report
//!       what: 盘上 TEAM_AGENT_ID、在役 grok、pre_first_turn；读不出为 unjudgeable
//! boundary:
//!   - 读不出不报一致
//!   - 不写盘、不改 overlay
//! maturity: wired
//! ---

use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokSlotReport {
    pub readable: bool,
    pub consistent: bool,
    pub disk_team_agent_id: Option<String>,
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
                let disk = parse_toml_team_agent_id(&text);
                return classify(disk, live_ids, pre_first_turn);
            }
        }
    }
    classify(None, live_ids, pre_first_turn)
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
    disk: Option<String>,
    live_ids: Vec<String>,
    pre_first_turn: Vec<String>,
) -> GrokSlotReport {
    if live_ids.is_empty() && disk.is_none() {
        return GrokSlotReport {
            readable: true,
            consistent: true,
            disk_team_agent_id: None,
            live_seats: live_ids,
            pre_first_turn,
            reason: "no grok seats and no slot file".to_string(),
        };
    }
    if live_ids.len() == 1 && disk.as_deref() == Some(live_ids[0].as_str()) {
        return GrokSlotReport {
            readable: true,
            consistent: true,
            disk_team_agent_id: disk,
            live_seats: live_ids,
            pre_first_turn,
            reason: "slot matches the single live grok seat".to_string(),
        };
    }
    let disk_label = disk.clone().unwrap_or_else(|| "(absent)".to_string());
    let live_label = if live_ids.is_empty() {
        "(none)".to_string()
    } else {
        live_ids.join(",")
    };
    GrokSlotReport {
        readable: true,
        consistent: false,
        disk_team_agent_id: disk,
        live_seats: live_ids,
        pre_first_turn,
        reason: format!("grok slot mismatch: disk={disk_label} live={live_label}"),
    }
}

struct LiveGrok {
    id: String,
    pre_first_turn: bool,
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
        });
    }
}

fn parse_toml_team_agent_id(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("TEAM_AGENT_ID") else {
            continue;
        };
        let rest = rest.trim().strip_prefix('=')?;
        let rest = rest.trim();
        let unquoted = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(rest);
        if !unquoted.is_empty() {
            return Some(unquoted.to_string());
        }
    }
    None
}
