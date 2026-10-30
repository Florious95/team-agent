//! Leader reminders are per-job files activated by an ephemeral, supervised agenda.
use std::path::Path;

use serde_json::json;

use super::{CliError, CmdOutput, CmdResult};
use crate::station::agenda;

pub(super) fn run(args: &[String], cwd: &Path) -> Result<CmdResult, CliError> {
    let mut workspace = cwd.to_path_buf();
    let (mut team, mut after, mut every, mut text) = (None, None, None, None);
    let mut positional = Vec::new();
    let mut tokens = args.iter();
    while let Some(token) = tokens.next() {
        // arguments::parse normalizes valued flags to --flag=value, including payloads.
        match token.split_once('=') {
            Some(("--workspace", value)) => workspace = Path::new(value).to_path_buf(),
            Some(("--team", value)) => team = Some(value),
            Some(("--in", value)) => after = Some(value),
            Some(("--every", value)) => every = Some(value),
            Some(("--text", value)) => text = Some(value),
            _ if token == "--json" => {}
            _ if token == "--" => positional.extend(tokens.by_ref().map(String::as_str)),
            _ => positional.push(token.as_str()),
        }
    }
    // Validate the full request before touching the workspace or starting a daemon.
    let duration = if positional.as_slice() == ["add"] {
        let duration = match (after, every) {
            (Some(value), None) | (None, Some(value)) => value,
            _ => {
                return Err(CliError::Usage(
                    "schedule add requires exactly one of --in or --every".into(),
                ))
            }
        };
        if text.is_none_or(|text| text.trim().is_empty()) {
            return Err(CliError::Usage(
                "schedule add requires non-empty --text".into(),
            ));
        }
        agenda::parse_duration(duration).map_err(CliError::Usage)?;
        Some(duration)
    } else {
        if after.is_some() || every.is_some() || text.is_some() {
            return Err(CliError::Usage(
                "--in, --every and --text are only valid with schedule add".into(),
            ));
        }
        match positional.as_slice() {
            ["list"] => {}
            ["cancel", id]
                if id.strip_prefix("sched_").is_some_and(|hex| {
                    !hex.is_empty() && hex.bytes().all(|c| c.is_ascii_hexdigit())
                }) => {}
            _ => {
                return Err(CliError::Usage(
                    "expected schedule add, schedule list or schedule cancel sched_<hex>".into(),
                ))
            }
        }
        None
    };
    let selected = crate::state::selector::resolve_active_team_readonly(
        &workspace,
        team,
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let workspace = &selected.run_workspace;
    let owner = selected.team_key.as_str();
    let store = agenda::Store::new(workspace, owner).map_err(CliError::Runtime)?;
    let value = if let Some(duration) = duration {
        store
            .add(text.unwrap_or_default(), duration, every.is_some())
            .map_err(CliError::Runtime)?
    } else if positional[0] == "list" {
        json!({"ok": true, "schedules": store.list().map_err(CliError::Runtime)?})
    } else {
        if !store.cancel(positional[1]).map_err(CliError::Runtime)? {
            return Err(CliError::Usage(format!(
                "active reminder {} not found in team {owner}",
                positional[1]
            )));
        }
        json!({"ok": true, "cancelled": positional[1]})
    };
    let as_json = args.iter().any(|arg| arg == "--json");
    let mut result = CmdResult::from_json(value.clone(), as_json);
    if !as_json {
        let human = if value["ok"] == false {
            format!(
                "Reminder {} stored but not activated: {}",
                value["id"], value["error"]
            )
        } else if let Some(id) = value["cancelled"].as_str() {
            format!("Cancelled scheduled reminder {id}.")
        } else if let Some(schedules) = value["schedules"].as_array() {
            let mut rows = vec!["ID\tTRIGGER\tTEXT\tSTATUS".to_string()];
            rows.extend(schedules.iter().map(|row| {
                format!(
                    "{}\t{}\t{}\t{}",
                    row["id"].as_str().unwrap_or_default(),
                    row["trigger"].as_str().unwrap_or_default(),
                    row["text"].as_str().unwrap_or_default(),
                    row["status"].as_str().unwrap_or_default()
                )
            }));
            rows.join("\n")
        } else {
            format!(
                "Scheduled reminder {} active ({}): {}",
                value["id"].as_str().unwrap_or_default(),
                value["trigger"].as_str().unwrap_or_default(),
                value["text"].as_str().unwrap_or_default()
            )
        };
        result.output = CmdOutput::Human(human);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(raw: &[&str]) -> Result<CmdResult, CliError> {
        let args = raw.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let checked = crate::cli::arguments::parse("schedule", &args)?;
        run(&checked.tokens, Path::new("/unused-schedule-test"))
    }

    #[test]
    fn rejects_invalid_schedule_requests_before_workspace_access() {
        for args in [
            vec![],
            vec!["add", "--text", "hi"],
            vec!["add", "--in", "1s", "--every", "1s", "--text", "hi"],
            vec!["add", "--in", "0s", "--text", "hi"],
            vec!["add", "--in", "1s"],
            vec!["add", "--in", "1s", "--text", " "],
            vec!["list", "--every", "1s"],
            vec!["cancel"],
            vec!["cancel", "sched_0"],
            vec!["add", "--in", "1s", "--in", "2s", "--text", "hi"],
            vec!["list", "--unknown"],
        ] {
            assert!(
                matches!(checked(&args), Err(CliError::Usage(_))),
                "{args:?}"
            );
        }
    }

    #[test]
    fn normalized_values_and_flag_shaped_text_reach_workspace_selection() {
        for args in [
            vec!["add", "--in", "1s", "--text", "message = value"],
            vec!["add", "--every=2m", "--text=--json"],
            vec!["list", "--json"],
            vec!["cancel", "sched_a"],
        ] {
            assert!(
                matches!(checked(&args), Err(CliError::State(_))),
                "{args:?}"
            );
        }
    }
}
