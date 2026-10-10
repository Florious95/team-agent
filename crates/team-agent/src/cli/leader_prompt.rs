//! Early, host-global command dispatch: never select or initialize a workspace.
use super::ExitCode;
use crate::leader::prompt::{self, Mutation, PromptError};
use serde_json::json;
use std::io::{self, Write};
use std::path::PathBuf;

pub(crate) const HELP: &str = "Purpose:\nPersist supplemental system instructions for future Team Agent Leader launches only.\nUsage: team-agent leader-prompt [show|set|append|clear] [--json] [--file PATH | -- TEXT]\n\nOptions:\nshow (default) prints the exact text; set replaces it; append joins old and new text with two newlines; clear removes it.\nset/append require exactly one TEXT token after --, or --file PATH containing UTF-8 text. No implicit stdin.\nEverything after -- is literal text, including --json and --help. Empty text and NUL are rejected.\nConfiguration: ~/.team-agent/leader-prompt.txt, independent of the workspace. Mutations do not echo the text.\nOnly new Pi, Claude, Codex and Grok Leaders apply it; Worker launches never read it. Cursor/Copilot are refused when configured.\nCodex merges explicit argv developer_instructions, but overrides that user/profile configuration slot; migrate its existing text here.\nNative core prompts and unrelated launch arguments remain unchanged. Attach does not update an existing session.\nExamples:\nteam-agent leader-prompt show\nteam-agent leader-prompt set -- 'Keep replies concise.'\nteam-agent leader-prompt clear\n\nNext Action:\nUse show to inspect the text, then start a new Leader with team-agent pi (or claude/codex/grok).\nset/append/clear affect the next new process, not running Leaders or their conversation history.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Show,
    Set,
    Append,
    Clear,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Set => "set",
            Self::Append => "append",
            Self::Clear => "clear",
        }
    }
}
#[derive(Debug, Eq, PartialEq)]
enum Input {
    Text(String),
    File(PathBuf),
}
#[derive(Debug)]
struct Request {
    operation: Operation,
    input: Option<Input>,
    json: bool,
    help: bool,
}
fn usage() -> PromptError {
    PromptError::new(
        "leader_prompt_invalid_input",
        "Invalid or incomplete leader-prompt arguments; use one input for set/append",
        None,
    )
}
fn parse(args: &[String]) -> Result<Request, PromptError> {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut json = false;
    let mut help = false;
    let mut file = None;
    let mut positional = None;
    let mut i = 0;
    while i < end {
        match args[i].as_str() {
            "--json" if !json => json = true,
            "--help" | "-h" if !help => help = true,
            "--stdin" => {
                return Err(PromptError::new(
                    "leader_prompt_invalid_input",
                    "--stdin is not supported for leader-prompt; use -- TEXT or --file PATH instead",
                    None,
                ));
            }
            "--file" if file.is_none() && i + 1 < end => {
                i += 1;
                if matches!(args[i].as_str(), "--json" | "--help" | "-h" | "--file") {
                    return Err(usage());
                }
                file = Some(PathBuf::from(&args[i]));
            }
            token if !token.starts_with('-') && positional.is_none() => positional = Some(token),
            _ => return Err(usage()),
        }
        i += 1;
    }
    let operation = match positional.unwrap_or("show") {
        "show" => Operation::Show,
        "set" => Operation::Set,
        "append" => Operation::Append,
        "clear" => Operation::Clear,
        _ => return Err(usage()),
    };
    let text = if end < args.len() {
        if args.len() != end + 2 {
            return Err(usage());
        }
        Some(args[end + 1].clone())
    } else {
        None
    };
    if (file.is_some() && text.is_some()) || (help && (file.is_some() || text.is_some())) {
        return Err(usage());
    }
    let input = file.map(Input::File).or_else(|| text.map(Input::Text));
    if !help && (matches!(operation, Operation::Set | Operation::Append) != input.is_some()) {
        return Err(usage());
    }
    Ok(Request {
        operation,
        input,
        json,
        help,
    })
}
fn execute(request: Request) -> Result<(), PromptError> {
    let path = prompt::config_path()?;
    if request.operation == Operation::Show {
        let text = prompt::read(&path)?.unwrap_or_default();
        if request.json {
            println!(
                "{}",
                json!({"configured": !text.is_empty(), "config_path": path, "prompt": text})
            );
        } else {
            // Unlike mutation diagnostics, explicit show must preserve every byte.
            io::stdout()
                .write_all(text.as_bytes())
                .and_then(|_| io::stdout().flush())
                .map_err(|error| PromptError::io("leader_prompt_write_failed", &path, error))?;
        }
        return Ok(());
    }
    let mutation = match request.operation {
        Operation::Clear => Mutation::Clear,
        _ => {
            let text = match request.input {
                Some(Input::Text(text)) => text,
                Some(Input::File(input)) => std::fs::read_to_string(&input).map_err(|error| {
                    PromptError::io("leader_prompt_unreadable", &input, error).at_config(&path)
                })?,
                None => return Err(usage()),
            };
            if request.operation == Operation::Set {
                Mutation::Set(text)
            } else {
                Mutation::Append(text)
            }
        }
    };
    let configured = prompt::mutate(&path, mutation)?;
    if request.json {
        println!(
            "{}",
            json!({"operation": request.operation.name(), "configured": configured, "config_path": path})
        );
    } else {
        println!(
            "Leader prompt {}: configured={configured}; config={}",
            request.operation.name(),
            path.display()
        );
    }
    Ok(())
}
pub(crate) fn run(args: &[String]) -> ExitCode {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let as_json = args[..end].iter().any(|arg| arg == "--json");
    let result = match parse(args) {
        Ok(request) if request.help => {
            println!("{HELP}");
            return ExitCode::Ok;
        }
        Ok(request) => execute(request),
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            if as_json {
                println!("{}", error.report());
            } else {
                eprintln!("{error}");
            }
            if error.reason == "leader_prompt_invalid_input" {
                ExitCode::Usage
            } else {
                ExitCode::Error
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|text| text.to_string()).collect()
    }
    #[test]
    fn leader_prompt_controls_stop_at_delimiter() {
        for text in ["--json", "--help", "--file", "--stdin", "multi\nline🙂", ""] {
            let request = parse(&args(&["set", "--", text])).unwrap();
            assert!(!request.json && !request.help);
            assert_eq!(request.input, Some(Input::Text(text.into())));
        }
        assert!(parse(&args(&[])).unwrap().operation == Operation::Show);
        let request = parse(&args(&["--json", "append", "--file", "path with 中文"])).unwrap();
        assert_eq!(
            request.input,
            Some(Input::File(PathBuf::from("path with 中文")))
        );
        assert!(request.json);
    }
    #[test]
    fn leader_prompt_stdin_control_reports_unsupported_input_with_usage_exit() {
        for tokens in [
            vec!["set", "--stdin"],
            vec!["set", "--json", "--stdin"],
            vec!["append", "--json", "--stdin"],
        ] {
            let tokens = args(&tokens);
            let error = parse(&tokens).unwrap_err();
            assert_eq!(error.reason, "leader_prompt_invalid_input");
            assert_eq!(
                error.error,
                "--stdin is not supported for leader-prompt; use -- TEXT or --file PATH instead"
            );
            assert!(error.config_path.is_none());
            assert_eq!(error.report()["reason"], "leader_prompt_invalid_input");
            assert_eq!(error.report()["error"], error.error);
            assert!(error.to_string().contains("--stdin is not supported"));
            assert_eq!(run(&tokens).code(), 2);
        }
    }
    #[test]
    fn leader_prompt_stdin_file_value_is_not_reclassified_as_a_control() {
        for operation in ["set", "append"] {
            let request = parse(&args(&[operation, "--file", "--stdin"])).unwrap();
            assert_eq!(request.input, Some(Input::File(PathBuf::from("--stdin"))));
        }
    }
    #[test]
    fn leader_prompt_rejects_duplicate_missing_unknown_or_extra_controls() {
        for tokens in [
            vec!["set"],
            vec!["append", "--"],
            vec!["set", "--", "one", "two"],
            vec!["set", "--file"],
            vec!["set", "--file", "input", "--", "text"],
            vec!["show", "--", "text"],
            vec!["clear", "--file", "input"],
            vec!["show", "--json", "--json"],
            vec!["set", "--file", "one", "--file", "two"],
            vec!["show", "--unknown"],
            vec!["show", "clear"],
            vec!["unknown", "--help"],
        ] {
            assert_eq!(
                parse(&args(&tokens)).unwrap_err().reason,
                "leader_prompt_invalid_input"
            );
        }
    }
}
