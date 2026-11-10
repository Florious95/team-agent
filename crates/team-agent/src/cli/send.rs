//! Current CLI send admission and station dispatch.

use super::*;

mod resolve;

use resolve::logical_to_from_args;

/// Validate CLI addressing and dispatch through the current durable inbox path.
pub fn cmd_send(args: &SendArgs) -> Result<CmdResult, CliError> {
    cmd_send_framed(args, crate::direct_delivery::Framing::Framed)
}

pub fn cmd_send_bare(args: &SendArgs) -> Result<CmdResult, CliError> {
    crate::station::send::validate_bare_payload(&args.message.join(" ")).map_err(CliError::Usage)?;
    if args.sender.as_str() != "leader" && args.sender.display_name() != "operator" {
        return Err(CliError::Usage("--bare requires a trusted leader/operator".into()));
    }
    cmd_send_framed(args, crate::direct_delivery::Framing::Bare)
}

fn cmd_send_framed(args: &SendArgs, framing: crate::direct_delivery::Framing) -> Result<CmdResult, CliError> {
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
    let (workspace, team) = alias.as_ref().map_or((&args.workspace, args.team.as_deref()),
        |entry| (&entry.workspace, Some(entry.team_key.as_str())));
    let outcome = crate::station::send::execute_with_framing(crate::station::send::Request {
        workspace, team, to: &logical_to, content: &content,
        task_id: args.task.as_deref(), message_id: args.message_id.as_deref(),
        presentation: &args.presentation,
        mailbox: args.presentation.sink != crate::messaging::presentation::PresentationSink::Leader,
        envelope: None,
    }, crate::station::send::Authority::Operator { sender: args.sender.display_name() }, framing);
    let value = match outcome {
        Ok(value) if value["status"] == "refused" => return refused_outcome(args, workspace, team, value),
        Ok(value) => value,
        // Addressing refusals already carry a typed envelope above. Storage/I/O
        // errors must not be replaced by a second, unrelated name lookup.
        Err(error) if framing == crate::direct_delivery::Framing::Bare && error.starts_with("--bare") =>
            return Err(CliError::Usage(error)),
        Err(error) => return Err(CliError::Runtime(error)),
    };
    Ok(CmdResult::from_json(value, args.json))
}

fn refused_outcome(
    args: &SendArgs,
    workspace: &std::path::Path,
    team: Option<&str>,
    mut value: serde_json::Value,
) -> Result<CmdResult, CliError> {
    let mut first_error = None;
    for (index, detail) in value["invalid_details"].as_array_mut().into_iter().flatten().enumerate() {
        let raw = detail["target"].as_str().unwrap_or_default().to_string();
        if let Err(mut error) = crate::cli::named_address::resolve_name_for_cli(workspace, &raw, team) {
            if error.kind.as_str() != detail["reason"].as_str().unwrap_or_default() { continue; }
            resolve::adapt_positional_bare_error(args, &raw, &mut error);
            for (key, field) in error.to_json().as_object().unwrap() {
                detail[key] = field.clone();
            }
            if index == 0 { first_error = Some((raw, error)); }
        }
    }
    if let Some((raw, error)) = first_error {
        if !args.json {
            let (message, action) = error.human_guidance(&raw);
            return Err(CliError::AddressRefusal { audit: error.n38_message(), error: message, action });
        }
        for (key, field) in error.to_json().as_object().unwrap() {
            value[key] = field.clone();
        }
    } else if !args.json {
        let message = value["error"].as_str().unwrap_or("recipient refused").to_string();
        let action = value["action"].as_str().unwrap_or_default().to_string();
        return Err(CliError::AddressRefusal {
            audit: format!("Error: {}: {message}\nAction: {action}\nLog: no targets attempted", value["reason"].as_str().unwrap_or("refused")),
            error: message,
            action,
        });
    }
    Ok(CmdResult::from_json(value, args.json))
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
