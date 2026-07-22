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
    initial_delivery_allows_watch, observe_initial_delivery_for_watch, routing_ambiguous_value,
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
    // F1 (0.3.26, cross-team send): --pane <pane_id> direct targeting.
    // Mutually exclusive with target / --to (agent-name routing).
    if let Some(ref pane_id) = args.pane {
        warn_send_alias("--pane");
        if args.target.is_some()
            || args.targets.is_some()
            || args.to_name.is_some()
            || args.to_leader.is_some()
        {
            let message = if args.to_name.is_some() {
                "--to-name and --pane/TARGET/--to are mutually exclusive"
            } else {
                "--pane and TARGET/--to are mutually exclusive; --pane also conflicts with --to-leader"
            };
            return Err(CliError::Usage(message.to_string()));
        }
        let content = args.message.join(" ");
        if content.is_empty() {
            return Err(CliError::Usage(
                "--pane requires a non-empty message".to_string(),
            ));
        }
        return Err(CliError::Usage(format!(
            "--pane {pane_id} is deprecated and sunset; use a logical TARGET so the message is persisted before delivery"
        )));
    }
    if args.targets.is_some() {
        warn_send_alias("--targets");
    }
    if args.to_name.is_some() {
        warn_send_alias("--to-name");
    }
    if args.to_leader.is_some() {
        warn_send_alias("--to-leader");
    }
    let host_leader_alias = if let Some(name) = args.to_leader.as_deref() {
        match resolve_host_leader_alias(name) {
            Ok(resolved) => Some(resolved),
            Err(value) => return Ok(cmd_send_result(value, args.json)),
        }
    } else {
        None
    };
    let logical_to = logical_to_from_args(
        args,
        host_leader_alias
            .as_ref()
            .map(|(logical_to, _)| logical_to.as_str()),
    )?;
    let content = args.message.join(" ");
    if !logical_to.is_empty() && logical_to != "*" && !content.is_empty() {
        let mut value = send_to_logical_to(args, &logical_to, &content)?;
        if let Some((_, entry)) = host_leader_alias.as_ref() {
            decorate_host_leader_alias(&mut value, entry);
        }
        add_send_reminder_if_ok(&mut value);
        return Ok(cmd_send_result(value, args.json));
    }
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let target = send_target(None, Some(logical_to.as_str()));
    let mut opts = send_options_from_args(args);
    // `args.team` is a selector and may be a legacy session/team-dir alias.
    // All downstream membership and DB scope must use the canonical key that
    // resolve_active_team returned, never the original selector spelling.
    opts.team = Some(TeamKey::new(selected.team_key.clone()));
    // CR-061/N27 routing-ambiguous: a single positional with no `--to`/`--targets` and an
    // empty message body is a prompt-only invocation (`team-agent send "fix the build"`).
    // The lone positional is CONTENT, not a target — reject with `routing_ambiguous`
    // (NOT `target_not_in_team`, which would lie that the user did pick a target).
    if let Some(amb) =
        routing_ambiguous_value(&selected.run_workspace, args, &target, &content, &opts)
    {
        return Ok(cmd_send_result(amb, args.json));
    }
    if let Some(value) = dirty_topology_refusal_value(&selected, args.team.as_deref()) {
        return Ok(cmd_send_result(value, args.json));
    }
    let coordinator_ensure =
        if target_has_known_worker(&selected.state, &target, opts.sender.as_str()) {
            loud_ensure_coordinator(&selected)?
        } else {
            None
        };
    let mut outcome = messaging::send_message(&selected.run_workspace, &target, &content, &opts)?;
    if opts.watch_result {
        outcome = observe_initial_delivery_for_watch(&selected, &target, &outcome, &opts)?;
    }
    let mut value = delivery_outcome_json(&outcome, &target, &content, &opts);
    // 0.5.45 naming-addressing (design §3.5, RED-2/RED-3 positional):
    // when the refusal reason is `target_not_in_team` AND the target
    // was a Single non-special short id, attach scope-safe
    // suggestions ranked from `selected.state.agents` — never from
    // raw workspace `teams` (design §3.5 & risk table). Zero DB
    // write / zero inject: the request stays refused, only the JSON
    // envelope gains `requested_name`/`suggested_name`/`candidates`
    // and the human `Action` gains a "Did you mean" line.
    attach_positional_typo_suggestions(&mut value, &target, &selected.state);
    append_loud_ensure_fields(&mut value, coordinator_ensure.as_ref());
    if opts.watch_result && initial_delivery_allows_watch(outcome.status) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("watch".to_string(), watch_notice_json(&target, &opts));
        }
    }
    add_send_reminder_if_ok(&mut value);
    Ok(cmd_send_result(value, args.json))
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
pub fn send_target(targets: Option<&str>, target: Option<&str>) -> MessageTarget {
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
