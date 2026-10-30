//!
//! cli · send — `cmd_send` + target 解析(`_send_target`)+ `SendArgs`→`SendOptions` 翻译
//! (`send_options_from_args`,旗标取反语义)。

use super::*;
use crate::messaging::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus};

mod mailbox;
mod persist;
mod presentation;
mod resolve;

pub use persist::send_options_from_args;
use persist::{
    initial_delivery_allows_watch, observe_initial_delivery_for_watch,
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
        if args.target.is_some() || args.targets.is_some() || args.to_name.is_some() || args.to_leader.is_some() {
            return Err(CliError::Usage("An agent name cannot be combined with --to-name/--pane/--to/--to-leader/--targets; choose one addressing mode".into()));
        }
        if args.message.join(" ").trim().is_empty() {
            return Err(CliError::Usage("Provide a task message; --pane cannot send an empty message".into()));
        }
        return Err(CliError::Usage("raw pane delivery requires --socket and --dialect".into()));
    }
    logical_to_from_args(args, args.to_leader.as_deref())?;
    let alias = args.to_leader.as_deref().map(read_host_leader_alias).transpose()?;
    let logical_to = logical_to_from_args(args, alias.as_ref().map(|_| "leader"))?;
    let content = args.message.join(" ");
    if content.is_empty() { return Err(CliError::Usage("empty_message: send requires a nonempty message".into())); }
    if logical_to.split(',').any(|target| matches!(
        crate::messaging::parse_logical_address(target),
        Ok(crate::messaging::LogicalAddress { target: crate::messaging::LogicalAddressTarget::SessionWindow { .. }, .. })
    )) {
        return Err(CliError::Usage("Terminal-window addressing is no longer supported; use an agent name or leader, with --workspace/--team to select scope".into()));
    }
    let (workspace, team) = alias.as_ref().map_or((&args.workspace, args.team.as_deref()),
        |entry| (&entry.workspace, Some(entry.team_key.as_str())));
    crate::coordinator::retire_legacy_coordinator(workspace).map_err(CliError::Runtime)?;
    let outcome = crate::station::send::execute(crate::station::send::Request {
        workspace, team, to: &logical_to, content: &content,
        task_id: args.task.as_deref(), message_id: args.message_id.as_deref(),
        presentation: &args.presentation,
        mailbox: args.presentation.sink != crate::messaging::presentation::PresentationSink::Leader,
        envelope: None,
    }, crate::station::send::Authority::Operator { sender: args.sender.display_name() });
    let value = match outcome {
        Ok(value) => value,
        Err(error) if alias.is_some() => return Err(CliError::Runtime(error)),
        Err(error) => return refuse(args, &logical_to, error),
    };
    Ok(CmdResult::from_json(value, args.json))
}

/// Refuse an unroutable recipient. The read-only named-address diagnosis only
/// explains the refusal (reason, scoped spelling suggestions); it never routes.
fn refuse(args: &SendArgs, raw: &str, cause: String) -> Result<CmdResult, CliError> {
    let Err(mut error) = crate::cli::named_address::resolve_name_for_cli(&args.workspace, raw, args.team.as_deref()) else {
        return Err(CliError::Runtime(cause));
    };
    resolve::adapt_positional_bare_error(args, raw, &mut error);
    if args.json {
        return Ok(CmdResult::from_json(error.to_json(), true));
    }
    let (message, action) = error.human_guidance(raw);
    Err(CliError::AddressRefusal { audit: error.n38_message(), error: message, action })
}

fn read_host_leader_alias(name: &str) -> Result<crate::leader::registry::LeaderRegistryEntry, CliError> {
    let candidates: Vec<_> = crate::leader::registry::snapshots().into_iter()
        .filter(|entry| entry.delivery_name == name || entry.qualified_name == name
            || entry.stable_qualified_name == name || entry.aliases.iter().any(|alias| alias == name)).collect();
    if candidates.len() != 1 {
        return Err(CliError::Runtime(format!(
            "leader name is unknown or ambiguous: {name}; refused, nothing was sent. List attached leader names with team-agent leaders; to leave the message for later, resend with --mailbox and read it with team-agent inbox leader"
        )));
    }
    Ok(candidates.into_iter().next().unwrap())
}

/// Resolve a host-leader alias, send through the canonical persisted funnel,
/// then decorate the already-produced result for the host-leader surface.
pub fn send_to_canonical_leader_target(
    _sender_workspace: &std::path::Path,
    name: &str,
    content: &str,
    sender: &TrustedSender,
    task_id: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    let entry = read_host_leader_alias(name)?;
    let args = SendArgs {
        target: Some("leader".to_string()),
        message: vec![content.to_string()],
        targets: None,
        workspace: entry.workspace.clone(),
        team: Some(entry.team_key.clone()),
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
    let mut value = match cmd_send(&args)?.output {
        CmdOutput::Json(value) => value,
        _ => unreachable!("JSON send requested"),
    };
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
