//!
//! cli · send — `cmd_send` + target 解析(`_send_target`)+ `SendArgs`→`SendOptions` 翻译
//! (`send_options_from_args`,旗标取反语义)。

use super::*;
use crate::messaging::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus};

mod coordinator;
mod mailbox;
mod persist;
mod presentation;
mod resolve;

use coordinator::{
    append_loud_ensure_fields, dirty_topology_refusal_value, loud_ensure_coordinator,
    target_has_known_worker,
};
pub use persist::send_options_from_args;
use persist::{
    attach_mcp_handshake_wait, initial_delivery_allows_watch, observe_initial_delivery_for_watch,
    routing_ambiguous_value,
};
use presentation::{
    add_send_reminder_if_ok, attach_positional_typo_suggestions, cmd_send_result,
    delivery_outcome_json, watch_notice_json,
};
use resolve::{
    decorate_host_leader_alias, logical_to_from_args, resolve_host_leader_alias,
    send_to_logical_to, warn_send_alias,
};

/// `cmd_send`(`commands.py:164`)。解析 target(`--to` fanout / 单 target / `*`)→ [`MessageTarget`],
/// 拼 [`SendOptions`](no_ack→requires_ack 取反、no_wait→wait_visible 取反等)→ `messaging::send_message`。
pub fn cmd_send(args: &SendArgs) -> Result<CmdResult, CliError> {
    if args.pane.is_some() {
        return Err(CliError::Usage("raw pane delivery requires --socket and --dialect".into()));
    }
    let alias = args.to_leader.as_deref().map(resolve_host_leader_alias).transpose()
        .map_err(|value| CliError::Usage(value.to_string()))?;
    let logical_to = logical_to_from_args(args, alias.as_ref().map(|(to, _)| to.as_str()))?;
    let content = args.message.join(" ");
    if content.is_empty() { return Err(CliError::Usage("send requires a nonempty message".into())); }
    let requested = logical_to.split(',').map(str::trim).filter(|target| !target.is_empty());
    let mut results = Vec::new();
    let mut destinations = Vec::new();
    for target in requested {
        let (workspace, team, agent) = if let Some((team, agent)) = target.split_once('/') {
            (&args.workspace, Some(team), agent)
        } else { (&args.workspace, args.team.as_deref(), target) };
        let directory = crate::direct_delivery::directory(workspace, team).map_err(CliError::Usage)?;
        let agents: Vec<String> = if agent == "*" {
            directory.agents().into_iter().filter(|agent| agent != args.sender.as_str()).collect()
        } else { vec![agent.to_string()] };
        for agent in agents {
            let endpoint = directory.endpoint(&agent).map_err(CliError::Usage)?;
            if !destinations.iter().any(|(_, existing): &(crate::direct_delivery::Directory, crate::direct_delivery::Endpoint)| existing.socket == endpoint.socket && existing.pane_id == endpoint.pane_id) {
                destinations.push((crate::direct_delivery::Directory { workspace: directory.workspace.clone(), team: directory.team.clone(), state: serde_json::Value::Null }, endpoint));
            }
        }
    }
    if destinations.is_empty() { return Err(CliError::Usage("send has no recipients".into())); }
    for (directory, endpoint) in destinations {
        let mailbox = args.presentation.sink != crate::messaging::presentation::PresentationSink::Leader;
        if mailbox || (endpoint.agent == "leader" && !matches!(args.sender.as_str(), "human" | "leader")) {
            let record = crate::inbox_store::record(args.sender.as_str(), &endpoint.agent, &content, directory.team.as_deref(), "message");
            crate::inbox_store::append(&directory.workspace, &endpoint.agent, &record).map_err(CliError::Usage)?;
        }
        let value = if mailbox {
            serde_json::json!({"status": "stored", "recipient": endpoint.agent, "mailbox": true})
        } else {
            let text = format!("Team Agent message from {}:\n\n{content}", args.sender.as_str());
            crate::direct_delivery::send(&endpoint, &text).map_err(CliError::Usage)?
        };
        results.push(value);
    }
    let value = if results.len() == 1 { results.remove(0) } else { serde_json::json!({"status": "sent", "results": results}) };
    Ok(CmdResult::from_json(value, args.json))
}

/// Resolve a host-leader alias, send through the canonical persisted funnel,
/// then decorate the already-produced result for the host-leader surface.
pub fn send_to_canonical_leader_target(
    sender_workspace: &std::path::Path,
    name: &str,
    content: &str,
    sender: &TrustedSender,
    task_id: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    let (logical_to, entry) = match resolve_host_leader_alias(name) {
        Ok(resolved) => resolved,
        Err(value) => return Ok(value),
    };
    let args = SendArgs {
        target: Some(logical_to.clone()),
        message: vec![content.to_string()],
        targets: None,
        workspace: sender_workspace.to_path_buf(),
        team: None,
        task: task_id.map(str::to_string),
        sender: sender.clone(),
        no_ack: false,
        no_wait: true,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: true,
        message_id: None,
        presentation: crate::messaging::presentation::PresentationRequest::default(),
        pane: None,
        to_name: None,
        to_leader: None,
    };
    let mut value = send_to_logical_to(&args, &logical_to, content)?;
    decorate_host_leader_alias(&mut value, &entry);
    Ok(value)
}

/// `_send_target`(`commands.py:181-184`):`--to` comma-split fanout / `target` 单值 / None。
pub(super) fn send_target(targets: Option<&str>, target: Option<&str>) -> MessageTarget {
    if let Some(targets) = targets.filter(|s| !s.is_empty()) {
        let recipients: Vec<String> = targets
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();
        return MessageTarget::Fanout(recipients);
    }
    match target {
        Some("*") => MessageTarget::Broadcast,
        Some(target) => MessageTarget::Single(target.to_string()),
        None => MessageTarget::Single(String::new()),
    }
}
