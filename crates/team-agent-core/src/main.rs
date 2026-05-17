use std::env;
use std::io::{self, Read};

use team_agent_core::{
    contains_inline_secret, json_escape, list_tmux_targets, redact_secrets, render_message,
    validate_profile, Profile,
};

fn main() {
    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("");
    let result = match command {
        "list-targets" => list_targets(),
        "render-message" => render_message_cmd(),
        "redact" => redact_cmd(),
        "validate-profile" => validate_profile_cmd(),
        _ => Err(format!(
            "unknown command {command:?}; expected list-targets, render-message, redact, validate-profile"
        )),
    };
    match result {
        Ok(json) => println!("{json}"),
        Err(error) => {
            println!(
                "{{\"ok\":false,\"error\":\"{}\"}}",
                json_escape(error.trim())
            );
            std::process::exit(1);
        }
    }
}

fn read_stdin() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|err| format!("stdin read failed: {err}"))?;
    Ok(input)
}

fn list_targets() -> Result<String, String> {
    let targets = list_tmux_targets()?;
    let mut items = Vec::new();
    for target in targets {
        items.push(format!(
            "{{\"pane_id\":\"{}\",\"session_name\":\"{}\",\"window_index\":\"{}\",\"window_name\":\"{}\",\"pane_index\":\"{}\",\"pane_tty\":\"{}\",\"pane_current_command\":\"{}\",\"pane_active\":{},\"fingerprint\":\"{}\"}}",
            json_escape(&target.pane_id),
            json_escape(&target.session_name),
            json_escape(&target.window_index),
            json_escape(&target.window_name),
            json_escape(&target.pane_index),
            json_escape(&target.pane_tty),
            json_escape(&target.pane_current_command),
            target.pane_active,
            json_escape(&target.fingerprint())
        ));
    }
    Ok(format!("{{\"ok\":true,\"targets\":[{}]}}", items.join(",")))
}

fn render_message_cmd() -> Result<String, String> {
    let input = read_stdin()?;
    let sender = extract_string(&input, "from")
        .or_else(|| extract_string(&input, "sender"))
        .unwrap_or_else(|| "unknown".to_string());
    let task_id = extract_string(&input, "task_id");
    let content = extract_string(&input, "content").unwrap_or_default();
    let token = extract_string(&input, "message_id").unwrap_or_else(|| "missing".to_string());
    let rendered = render_message(&sender, task_id.as_deref(), &content, &token);
    Ok(format!(
        "{{\"ok\":true,\"text\":\"{}\",\"token\":\"{}\"}}",
        json_escape(&rendered),
        json_escape(&token)
    ))
}

fn redact_cmd() -> Result<String, String> {
    let input = read_stdin()?;
    let text = extract_string(&input, "text").unwrap_or(input);
    Ok(format!(
        "{{\"ok\":true,\"text\":\"{}\"}}",
        json_escape(&redact_secrets(&text))
    ))
}

fn validate_profile_cmd() -> Result<String, String> {
    let input = read_stdin()?;
    let profile = Profile {
        provider: extract_string(&input, "provider").unwrap_or_default(),
        model: extract_string(&input, "model").unwrap_or_default(),
        auth_mode: extract_string(&input, "auth_mode").unwrap_or_default(),
        profile: extract_string(&input, "profile").unwrap_or_default(),
        credential_ref: extract_string(&input, "credential_ref"),
    };
    let mut errors = match validate_profile(&profile) {
        Ok(()) => Vec::new(),
        Err(errors) => errors,
    };
    if contains_inline_secret(&input) {
        errors.push("input contains a probable inline secret".to_string());
    }
    if errors.is_empty() {
        Ok("{\"ok\":true,\"errors\":[]}".to_string())
    } else {
        let encoded: Vec<String> = errors
            .into_iter()
            .map(|err| format!("\"{}\"", json_escape(&err)))
            .collect();
        Ok(format!("{{\"ok\":false,\"errors\":[{}]}}", encoded.join(",")))
    }
}

fn extract_string(input: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_pos = input.find(&needle)?;
    let rest = &input[key_pos + needle.len()..];
    let colon = rest.find(':')?;
    let value = rest[colon + 1..].trim_start();
    if !value.starts_with('"') {
        return None;
    }
    parse_json_string(value)
}

fn parse_json_string(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = input.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut escaped = false;
    for ch in chars {
        if escaped {
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => out.push(other),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}
