//! Pure control-argument grammar. Native/route payloads keep their own parsers.
use super::CliError;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
enum ValueKind {
    Switch,
    Token,
    Path,
    Payload,
    Query,
    Count,
    Seconds,
    PositiveSeconds,
    Choice(&'static [&'static str]),
    FoldedChoice(&'static [&'static str]),
}
use ValueKind::*;

struct Schema {
    flags: &'static [(&'static str, ValueKind)],
    max_positionals: usize,
    workspace: bool,
    team: bool,
}

fn schema(command: &str) -> Option<Schema> {
    let (flags, max_positionals): (&'static [(&'static str, ValueKind)], usize) = match command {
        "intent-start" | "start" => (
            &[
                ("--from", Path),
                ("--session", Choice(&["auto", "preserve"])),
                ("--dry-run", Switch),
                ("--allow-fresh", Switch),
                ("--provider", Token),
                ("--model", Token),
                ("--effort", Token),
                ("--bypass", Choice(&["true", "false"])),
                ("--prompt", Payload),
                ("--profile", Token),
                ("--role-file", Path),
            ],
            1,
        ),
        "intent-stop" | "stop" => (&[], 1),
        "intent-restart" => (
            &[
                ("--session", Choice(&["auto", "preserve"])),
                ("--dry-run", Switch),
                ("--allow-fresh", Switch),
                ("--session-converge-deadline", Seconds),
                ("--detail", Switch),
            ],
            1,
        ),
        "quick-start" => (
            &[
                ("--name", Token),
                ("--team-id", Token),
                ("--yes", Switch),
                ("--backend", FoldedChoice(&["tmux", "conpty"])),
                ("--detail", Switch),
                ("--no-display", Switch),
            ],
            1,
        ),
        "send" => (
            &[
                ("--socket", Token),
                ("--dialect", Token),
                ("--targets", Token),
                ("--pane", Token),
                ("--to-name", Token),
                ("--to-leader", Token),
                ("--mailbox", Switch),
                (
                    "--presentation-sink",
                    Choice(&["leader", "casefile", "silent"]),
                ),
                ("--message-class", Token),
                ("--case-id", Token),
                ("--task", Token),
                ("--watch-result", Switch),
                ("--requires-ack", Switch),
                ("--no-ack", Switch),
                ("--no-wait", Switch),
                ("--timeout", Seconds),
                ("--confirm-human", Switch),
                ("--message-id", Token),
            ],
            usize::MAX,
        ),
        "schedule" => (&[("--in", Token), ("--every", Token), ("--text", Payload)], 2),
        "status" => (&[("--detail", Switch), ("--summary", Switch)], 1),
        "models" => (&[("--provider", Token), ("--search", Query)], usize::MAX),
        "inbox" => (&[("--limit", Count)], 1),
        "locate" | "migrate" => (&[], 0),
        "shutdown" => (&[("--keep-logs", Switch)], 0),
        "restart" => (
            &[
                ("--allow-fresh", Switch),
                ("--session-converge-deadline", Seconds),
                ("--detail", Switch),
                ("--no-display", Switch),
            ],
            1,
        ),
        "start-agent" | "add-agent" => (
            &[
                ("--provider", Token),
                ("--model", Token),
                ("--effort", Token),
                ("--bypass", Choice(&["true", "false"])),
                ("--prompt", Payload),
                ("--profile", Token),
                ("--role-file", Path),
                ("--force", Switch),
                ("--allow-fresh", Switch),
            ],
            1,
        ),
        "stop-agent" | "approvals" => (&[], 1),
        "reset-agent" => (&[("--discard-session", Switch)], 1),
        "clone-agent" | "fork-agent" => (&[("--as", Token), ("--label", Token)], 1),
        "remove-agent" => (
            &[
                ("--from-spec", Switch),
                ("--confirm", Switch),
                ("--force", Switch),
            ],
            1,
        ),
        "doctor" => (
            &[
                ("--gate", Token), // Preserve the doctor's typed unknown_gate refusal.
                ("--comms", Switch),
                ("--fix", Switch),
                ("--fix-schema", Switch),
                ("--cleanup-orphans", Switch),
                ("--confirm", Switch),
            ],
            1,
        ),
        "leaders" => (
            &[
                ("--all", Switch),
                ("--stale", Switch),
                ("--prune", Switch),
                ("--dry-run", Switch),
                ("--search", Token),
            ],
            1,
        ),
        "profile" => (
            &[
                ("--auth-mode", Token),
                ("--proxy-mode", Choice(&["direct", "inherit"])),
            ],
            2,
        ),
        "install-skill" => (
            &[
                ("--target", Choice(&["all", "codex", "claude", "copilot"])),
                ("--source", Path),
                ("--dest", Path),
                ("--uninstall", Switch),
                ("--dry-run", Switch),
            ],
            0,
        ),
        "claim-leader" => (&[("--confirm", Switch), ("--detail", Switch)], 0),
        "takeover" => (&[("--confirm", Switch)], 0),
        "attach-leader" => (
            &[
                ("--pane", Token),
                ("--provider", Token),
                ("--confirm", Switch),
            ],
            0,
        ),
        "attach-app-server-leader" => (&[("--socket", Path), ("--thread-id", Token)], 0),
        "wait" => (&[("--task", Token)], 0),
        "coordinator" => (
            &[("--once", Switch), ("--tick-interval", PositiveSeconds)],
            0,
        ),
        "kiro" => (&[], 0),
        _ => return None,
    };
    Some(Schema {
        flags,
        max_positionals,
        workspace: !matches!(command, "models" | "leaders" | "install-skill" | "kiro"),
        team: !matches!(
            command,
            "models" | "leaders" | "install-skill" | "kiro" | "wait" | "quick-start"
        ),
    })
}

fn canonical_flag<'a>(command: &str, flag: &'a str) -> &'a str {
    match (command, flag) {
        (_, "-h") => "--help",
        ("inbox", "-n") => "--limit",
        ("quick-start", "--team") => "--team-id",
        ("send", "--target" | "--to") => "--targets",
        _ => flag,
    }
}

fn option(schema: &Schema, command: &str, flag: &str) -> Option<ValueKind> {
    match flag {
        "--help" | "--json" | "--no-display" => Some(Switch),
        "--workspace" if schema.workspace => Some(Path),
        "--team" if schema.team => Some(Token),
        "--role-file" if command == "start-agent" => None,
        "--allow-fresh" if command == "add-agent" => None,
        _ => schema
            .flags
            .iter()
            .find(|(name, _)| *name == flag)
            .map(|(_, kind)| *kind),
    }
}

fn valid_value(kind: ValueKind, value: &str) -> bool {
    if value.trim().is_empty() {
        return matches!(kind, Query);
    }
    match kind {
        Count => value.parse::<usize>().is_ok(),
        Seconds | PositiveSeconds => value.parse::<f64>().is_ok_and(|number| {
            number.is_finite()
                && number >= 0.0
                && number <= u64::MAX as f64 / 1000.0
                && (!matches!(kind, PositiveSeconds) || number > 0.0)
        }),
        Choice(values) => values.contains(&value),
        FoldedChoice(values) => values.contains(&value.trim().to_ascii_lowercase().as_str()),
        _ => true,
    }
}

pub(super) struct CheckedArgs {
    pub(super) tokens: Vec<String>,
    pub(super) help: bool,
}

/// Validate only control tokens. Values retain everything after the first '='.
/// A free --help wins before filesystem/owner checks; a consumed/literal one never does.
pub(super) fn parse(command: &str, args: &[String]) -> Result<CheckedArgs, CliError> {
    let schema = schema(command)
        .ok_or_else(|| CliError::Usage(format!("no argument schema for {command}")))?;
    let mut tokens = Vec::new();
    let mut seen = BTreeSet::new();
    let mut positionals = 0usize;
    let mut error = None;
    let mut help = false;
    let mut i = 0;
    while i < args.len() {
        let raw = &args[i];
        if raw == "--" {
            positionals += args.len() - i - 1;
            tokens.extend_from_slice(&args[i..]);
            break;
        }
        if !raw.starts_with('-') {
            positionals += 1;
            tokens.push(raw.clone());
            i += 1;
            continue;
        }
        let (key, inline) = raw
            .split_once('=')
            .map_or((raw.as_str(), None), |(key, value)| (key, Some(value)));
        let flag = canonical_flag(command, key);
        let Some(kind) = option(&schema, command, flag) else {
            error.get_or_insert_with(|| format!("unknown option for {command}: {key}"));
            i += 1;
            continue;
        };
        if flag == "--help" && inline.is_none() {
            help = true;
        }
        if !seen.insert(flag.to_string()) {
            error.get_or_insert_with(|| format!("duplicate option: {flag}"));
        }
        if matches!(kind, Switch) {
            if inline.is_some() {
                error.get_or_insert_with(|| format!("{flag} does not take a value"));
            }
            // Explicit historical compatibility flag, never inferred from an unknown
            // option and never removed from a consumed or post-delimiter payload.
            if flag != "--no-display" {
                tokens.push(flag.to_string());
            }
        } else {
            let value = if let Some(value) = inline {
                Some(value)
            } else {
                args.get(i + 1)
                    .filter(|value| matches!(kind, Payload | Query) || !value.starts_with('-'))
                    .map(|value| {
                        i += 1;
                        value.as_str()
                    })
            };
            match value {
                Some(value) if valid_value(kind, value) => tokens.push(format!("{flag}={value}")),
                Some(_) => {
                    error.get_or_insert_with(|| format!("invalid value for {flag}"));
                }
                None => {
                    error.get_or_insert_with(|| format!("missing value for {flag}"));
                }
            }
        }
        i += 1;
    }
    if help {
        return Ok(CheckedArgs { tokens, help });
    }
    if positionals > schema.max_positionals {
        error.get_or_insert_with(|| format!("too many positional arguments for {command}"));
    }
    for pair in [
        ["--detail", "--summary"],
        ["--all", "--stale"],
        ["--requires-ack", "--no-ack"],
    ] {
        if pair.iter().all(|flag| seen.contains(*flag)) {
            error.get_or_insert_with(|| format!("{} conflicts with {}", pair[0], pair[1]));
        }
    }
    if command == "send"
        && ["--targets", "--pane", "--to-name", "--to-leader"]
            .iter()
            .filter(|flag| seen.contains(**flag))
            .count()
            > 1
    {
        error.get_or_insert_with(|| "choose one recipient addressing mode".to_string());
    }
    if command == "start-agent" && seen.contains("--force") {
        error = Some("start-agent does not support --force; use stop-agent AGENT followed by start-agent AGENT for a safe restart".to_string());
    }
    if let Some(error) = error {
        return Err(CliError::Usage(error));
    }
    Ok(CheckedArgs { tokens, help })
}

/// Rendering switches must not be inferred from a consumed value or literal payload.
pub(super) fn has_flag(command: &str, args: &[String], needle: &str) -> bool {
    if matches!(command, "team" | "agent") {
        if let Some((operation, args)) = args.split_first() {
            let command = match operation.as_str() {
                "start" => "intent-start",
                "stop" => "intent-stop",
                "restart" => "intent-restart",
                "status" if command == "team" => "status",
                "add" => "add-agent",
                "remove" => "remove-agent",
                "reset" => "reset-agent",
                "clone" => "clone-agent",
                "fork" => "fork-agent",
                _ => return false,
            };
            return has_flag(command, args, needle);
        }
    }
    let Some(schema) = schema(command) else {
        // Dedicated native parsers retain their pre-delimiter rendering switch.
        return args
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == needle);
    };
    let mut i = 0;
    while i < args.len() {
        let raw = &args[i];
        if raw == "--" {
            break;
        }
        let (key, inline) = raw
            .split_once('=')
            .map_or((raw.as_str(), None), |(key, value)| (key, Some(value)));
        let flag = canonical_flag(command, key);
        if flag == needle && inline.is_none() {
            return true;
        }
        if let Some(kind) = option(&schema, command, flag) {
            if !matches!(kind, Switch) && inline.is_none() {
                if args
                    .get(i + 1)
                    .is_some_and(|value| matches!(kind, Payload | Query) || !value.starts_with('-'))
                {
                    i += 1;
                }
            }
        }
        i += 1;
    }
    false
}
