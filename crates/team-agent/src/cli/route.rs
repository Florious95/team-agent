//! Host-global route management: raw argv, no workspace selection or error log.

use super::{CmdOutput, CmdResult, ExitCode};
use crate::provider::argv_route::{self, Config, Mutation, Override, RouteError};
use crate::provider::Provider;
use serde_json::{json, Value};

pub(crate) const HELP: &str = "Purpose:\nConfigure additional provider launch arguments. Disabled by default.\nUsage: team-agent route [status|enable|disable|show [TOOL]|set TOOL -- ARG...|add TOOL -- ARG...|clear TOOL] [--json]\n\nOptions:\nstatus reports the switch; enable/disable change it; show lists all or one provider's arguments.\nset replaces arguments; add appends; clear removes a provider's arguments. Use --json for structured output.\nTOOL is a supported provider name, such as pi/codex/claude/copilot/grok/cursor_agent.\nEverything after the first -- is a literal provider argument, including --help and --json.\nConfiguration: ~/.team-agent/argv-routing.json. This is host-global, not isolated by workspace or team.\nTEAM_AGENT_CLI_ARGV_ROUTING overrides the persisted switch.\nExamples:\nteam-agent route status\nteam-agent route set pi -- --mode text\nteam-agent route enable\n\nNext Action:\nVerify arguments with route show pi. Changes apply to the next launch, not an existing session.\nWithout an enabled configuration, providers retain their original arguments. Leave routing disabled unless the arguments are understood.";

struct Request {
    operation: String,
    provider: Option<Provider>,
    tokens: Vec<String>,
    json: bool,
}

fn usage() -> RouteError {
    RouteError {
        error: "Invalid or incomplete arguments; set/add require a provider name, -- and at least one provider argument".to_string(),
        reason: "argv_route_usage",
        action: "Follow the Examples in team-agent route --help, such as team-agent route set pi -- --mode text".to_string(),
        config_path: None,
    }
}

fn parse(args: &[String], delimiter: usize) -> Result<Request, RouteError> {
    let mut json = false;
    let mut positionals = Vec::new();
    for token in &args[..delimiter] {
        if token == "--json" {
            if json {
                return Err(usage());
            }
            json = true;
        } else if token.starts_with('-') {
            return Err(usage());
        } else {
            positionals.push(token.as_str());
        }
    }
    let operation = positionals.first().copied().unwrap_or("status");
    let provider_required = matches!(operation, "set" | "add" | "clear");
    let provider_allowed = provider_required || operation == "show";
    if !matches!(
        operation,
        "status" | "enable" | "disable" | "show" | "set" | "add" | "clear"
    ) || positionals.len() > if provider_allowed { 2 } else { 1 }
        || (provider_required && positionals.len() != 2)
    {
        return Err(usage());
    }
    let provider = positionals
        .get(1)
        .map(|raw| argv_route::parse_route_provider(raw).ok_or_else(usage))
        .transpose()?;
    let tokens = if matches!(operation, "set" | "add") {
        if delimiter == args.len() || delimiter + 1 == args.len() {
            return Err(usage());
        }
        let tokens = args[delimiter + 1..].to_vec();
        if !argv_route::valid_tokens(&tokens) {
            return Err(usage());
        }
        tokens
    } else {
        if delimiter != args.len() {
            return Err(usage());
        }
        Vec::new()
    };
    Ok(Request {
        operation: operation.to_string(),
        provider,
        tokens,
        json,
    })
}

fn switch_fields(
    path: Option<&std::path::Path>,
    persisted: Option<bool>,
    present: bool,
    override_state: Override,
) -> Value {
    json!({
        "config_path": path,
        "persisted_enabled": persisted,
        "effective_enabled": override_state.effective(persisted.unwrap_or(false)),
        "enabled_source": override_state.source(present),
        "override_status": override_state.status(),
    })
}

fn execute(request: &Request) -> Result<Value, RouteError> {
    let path = argv_route::config_path()?;
    let (config, present) = match request.operation.as_str() {
        "enable" => argv_route::mutate(&path, Mutation::Enable(true))?,
        "disable" => argv_route::mutate(&path, Mutation::Enable(false))?,
        "set" | "add" | "clear" => {
            let provider = request.provider.ok_or_else(usage)?;
            let mutation = match request.operation.as_str() {
                "set" => Mutation::Set(provider, request.tokens.clone()),
                "add" => Mutation::Add(provider, request.tokens.clone()),
                _ => Mutation::Clear(provider),
            };
            argv_route::mutate(&path, mutation)?
        }
        _ => argv_route::read_config(&path)?,
    };
    let override_state = Override::current();
    let mut value = switch_fields(Some(&path), Some(config.enabled), present, override_state);
    value["ok"] = json!(true);
    value["operation"] = json!(request.operation);
    if let Some(provider) = request.provider {
        add_provider_fields(&mut value, &config, provider);
    } else if request.operation == "show" {
        value["providers"] = json!(config.providers);
    }
    if override_state != Override::Unset {
        value["notice"] = json!(format!(
            "Switch overridden by environment variable {} ({})",
            argv_route::ENV_NAME,
            override_state.status()
        ));
    }
    Ok(value)
}

fn add_provider_fields(value: &mut Value, config: &Config, provider: Provider) {
    if let Some(key) = argv_route::route_key(provider) {
        value["provider"] = json!(key);
        value["configured"] = json!(config.providers.contains_key(key));
        value["argv"] = json!(config.providers.get(key).cloned().unwrap_or_default());
    }
}

/// Route-only error renderer: reuse redaction/formatting, never cwd log policy.
fn emit_error(error: RouteError, args: &[String], delimiter: usize, as_json: bool) -> ExitCode {
    let is_usage = error.reason == "argv_route_usage";
    let mut value = json!({
        "ok": false, "error": error.error, "reason": error.reason,
        "action": error.action, "config_path": error.config_path,
    });
    if !is_usage {
        // Best-effort switch observation even when strict management read fails.
        // No config content or invalid argv is included in the error channel.
        let path = error.config_path.as_deref();
        let loose = path
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let persisted = loose
            .as_ref()
            .and_then(|v| v.get("enabled"))
            .and_then(Value::as_bool);
        let fields = switch_fields(
            path,
            persisted,
            path.is_some_and(std::path::Path::exists),
            Override::current(),
        );
        if let (Some(target), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
            target.extend(fields.clone());
        }
        value["operation"] = json!(args[..delimiter]
            .iter()
            .find(|arg| !arg.starts_with('-'))
            .map(String::as_str)
            .unwrap_or("status"));
    }
    if is_usage {
        value["next_actions"] = json!(["team-agent route --help"]);
    }
    if let Some(text) = super::emit(&CmdOutput::Json(value), as_json) {
        if as_json {
            println!("{text}");
        } else {
            eprintln!("{text}");
        }
    }
    if is_usage {
        if !as_json {
            eprintln!("\n{HELP}");
        }
        ExitCode::Usage
    } else {
        ExitCode::Error
    }
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    let delimiter = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    // Help and output-format controls stop at the first delimiter. Neither
    // generic help scanning nor the old --no-display compatibility filter runs.
    if args[..delimiter]
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{HELP}");
        return ExitCode::Ok;
    }
    let as_json = args[..delimiter].iter().any(|arg| arg == "--json");
    match parse(args, delimiter) {
        Ok(request) => match execute(&request) {
            Ok(value) => super::emit::emit_result(CmdResult::from_json(value, request.json)),
            Err(error) => emit_error(error, args, delimiter, as_json),
        },
        Err(error) => emit_error(error, args, delimiter, as_json),
    }
}
