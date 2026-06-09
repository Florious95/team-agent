//! cli · emit — `emit`(--json vs 人读 dict 逐键)+ 顶层 `run` 调度(parser.py `main`)+
//! 人读标量/集合渲染(`human_value` / `json_dumps_like`)。

use super::*;
use std::io::Write as _;

/// `emit`(`helpers.py:12-23`):`--json`→`json.dumps(indent=2, ensure_ascii=False, sort_keys=True)`;
/// 否则 dict 逐键 `key: value`(嵌套 dict/list 内联 compact json,`ensure_ascii=False`)、非 dict 直接 print。
/// 返回应打印到 stdout 的字符串(bin main 负责实际 println)。
pub fn emit(output: &CmdOutput, as_json: bool) -> Option<String> {
    match output {
        CmdOutput::None => None,
        CmdOutput::Human(text) => Some(text.clone()),
        CmdOutput::Json(value) if as_json => serde_json::to_string_pretty(&sort_json(value)).ok(),
        CmdOutput::Json(Value::Object(obj)) => {
            let lines: Vec<String> = obj
                .iter()
                .map(|(key, value)| format!("{key}: {}", human_value(value)))
                .collect();
            Some(lines.join("\n"))
        }
        CmdOutput::Json(value) => Some(human_value(value)),
    }
}

/// `main(argv)`(`parser.py:84`):**CLI 唯一进程入口**。codex/claude passthrough 早返回 →
/// 解析 argv 到 subcommand → 调对应 handler → 异常落盘 + 信封 + `ExitCode::Error` →
/// `consume_leader_inbox_summary` → `emit` → `result.ok is False ? Error : Ok`。
/// **行为入口**:契约可端到端跑 argv→(stdout, exit code)。
pub fn run(argv: &[String], cwd: &Path) -> ExitCode {
    let Some(command) = argv.first().map(String::as_str) else {
        return emit_missing_subcommand_usage();
    };
    if command == "codex" || command == "claude" {
        return cmd_leader_passthrough(command, &argv[1..], cwd)
            .map(emit_result)
            .unwrap_or(ExitCode::Error);
    }
    if matches!(command, "-h" | "--help" | "help") {
        println!("{}", command_help(None));
        return ExitCode::Ok;
    }
    // CR-063/G4: every registered subcommand's `--help` must short-circuit before dispatch,
    // before argument validation, leader-pane checks, or runtime-state writes.
    //
    // The gate stays on KNOWN subcommands so an unknown command still falls through to
    // the argparse-style invalid-choice path (golden parser.py:84; covered by
    // `cli_unknown_command_red` and the `claude_code` divergence guard which would
    // otherwise be silently passthrough-shaped).
    if is_known_subcommand(command)
        && argv
            .iter()
            .skip(1)
            .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!("{}", command_help(Some(command)));
        return ExitCode::Ok;
    }
    match dispatch(command, &argv[1..], cwd) {
        Ok(exit) => exit,
        Err(error) => emit_cli_error(command, &argv[1..], cwd, &error),
    }
}

/// Print a handler's CmdResult to stdout (emit formats json/human), then surface its exit code.
/// (parser.py: `print(emit(result, as_json))` then the ok→exit mapping.)
fn emit_result(r: CmdResult) -> ExitCode {
    if let Some(text) = emit(&r.output, r.as_json) {
        println!("{text}");
    }
    r.exit
}

fn dispatch(command: &str, args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    match command {
        "init" => cmd_init(&init_args(args, cwd)).map(emit_result),
        "quick-start" => cmd_quick_start(&quick_start_args(args, cwd)?).map(emit_result),
        "compile" => cmd_compile(&compile_args(args, cwd)?).map(emit_result),
        "send" => cmd_send(&send_args(args, cwd)?).map(emit_result),
        "allow-peer-talk" => {
            cmd_allow_peer_talk(&allow_peer_talk_args(args, cwd)?).map(emit_result)
        }
        "status" => cmd_status_for_team(&status_args(args, cwd), parse_args(args).team.as_deref())
            .map(emit_result),
        "stop" => cmd_shutdown(&shutdown_args(args, cwd)).map(emit_result),
        "shutdown" => cmd_shutdown(&shutdown_args(args, cwd)).map(emit_result),
        "restart" => cmd_restart(&restart_args(args, cwd)).map(emit_result),
        "restart-agent" => cmd_reset_agent(&reset_agent_args(args, cwd)?).map(emit_result),
        "start-agent" => cmd_start_agent(&start_agent_args(args, cwd)?).map(emit_result),
        "stop-agent" => cmd_stop_agent(&stop_agent_args(args, cwd)?).map(emit_result),
        "reset-agent" => cmd_reset_agent(&reset_agent_args(args, cwd)?).map(emit_result),
        "add-agent" => cmd_add_agent(&add_agent_args(args, cwd)?).map(emit_result),
        "fork-agent" => cmd_fork_agent(&fork_agent_args(args, cwd)?).map(emit_result),
        "remove-agent" => cmd_remove_agent(&remove_agent_args(args, cwd)?).map(emit_result),
        "stuck-list" => cmd_stuck_list(&stuck_list_args(args, cwd)).map(emit_result),
        "stuck-cancel" => cmd_stuck_cancel(&stuck_cancel_args(args, cwd)?).map(emit_result),
        "acknowledge-idle" => {
            cmd_acknowledge_idle(&acknowledge_idle_args(args, cwd)).map(emit_result)
        }
        "takeover" => cmd_takeover(&takeover_args(args, cwd)).map(emit_result),
        "claim-leader" => cmd_claim_leader(&claim_leader_args(args, cwd)).map(emit_result),
        // Real dispatch: `cmd_attach_leader` writes the `leader_receiver` binding.
        "attach-leader" => cmd_attach_leader(&attach_leader_args(args, cwd)?).map(emit_result),
        "identity" => cmd_identity(&identity_args(args, cwd)).map(emit_result),
        "approvals" => cmd_approvals(&approvals_args(args, cwd)).map(emit_result),
        "inbox" => cmd_inbox(&inbox_args(args, cwd)?).map(emit_result),
        "doctor" => cmd_doctor(&doctor_args(args, cwd)).map(emit_result),
        "watch" => cmd_watch(&watch_args(args, cwd)).map(emit_result),
        "sessions" => cmd_sessions(&sessions_args(args, cwd)).map(emit_result),
        "validate" => cmd_validate(&validate_args(args, cwd)).map(emit_result),
        "profile" => cmd_profile(&profile_args(args, cwd)?).map(emit_result),
        "validate-result" if has_arg(args, "--result") => {
            eprintln!("team-agent: error: unrecognized arguments: --result");
            Ok(ExitCode::Usage)
        }
        "validate-result" => cmd_validate_result(&validate_result_args(args)?).map(emit_result),
        "collect" => {
            cmd_collect_for_team(&collect_args(args, cwd), parse_args(args).team.as_deref())
                .map(emit_result)
        }
        "settle" => cmd_settle(&settle_args(args, cwd)).map(emit_result),
        "repair-state" => cmd_repair_state(&repair_state_args(args, cwd)?).map(emit_result),
        "diagnose" => cmd_diagnose(&diagnose_args(args, cwd)).map(emit_result),
        "preflight" => cmd_preflight(&preflight_args(args, cwd)).map(emit_result),
        "wait-ready" => cmd_wait_ready(&wait_ready_args(args, cwd)).map(emit_result),
        "e2e" => cmd_e2e(&e2e_args(args, cwd)).map(emit_result),
        "peek" => cmd_peek(&peek_args(args, cwd)?).map(emit_result),
        "coordinator" => run_coordinator(args, cwd),
        _ => Ok(emit_unknown_subcommand_usage(command)),
    }
}

const DISPATCH_COMMANDS: &[&str] = &[
    "init",
    "quick-start",
    "compile",
    "send",
    "allow-peer-talk",
    "status",
    "stop",
    "shutdown",
    "restart",
    "restart-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "add-agent",
    "fork-agent",
    "remove-agent",
    "stuck-list",
    "stuck-cancel",
    "acknowledge-idle",
    "takeover",
    "claim-leader",
    "attach-leader",
    "identity",
    "approvals",
    "inbox",
    "doctor",
    "watch",
    "sessions",
    "validate",
    "profile",
    "validate-result",
    "collect",
    "settle",
    "repair-state",
    "diagnose",
    "preflight",
    "wait-ready",
    "e2e",
    "peek",
    "coordinator",
];

const SPEC_ONLY_HELP_COMMANDS: &[&str] = &["start", "purge-agent"];

fn emit_missing_subcommand_usage() -> ExitCode {
    emit_usage_error("the following arguments are required: {codex,claude,...,doctor}");
    ExitCode::Usage
}

/// Registered subcommands (the dispatch table) plus spec-only verbs that have no
/// dispatch arm yet but must still respond to `--help` per CR-063/G4.
/// Used by the `--help` short-circuit gate so unknown commands keep falling through
/// to the argparse invalid-choice path.
fn is_known_subcommand(command: &str) -> bool {
    DISPATCH_COMMANDS.contains(&command) || SPEC_ONLY_HELP_COMMANDS.contains(&command)
}

fn command_help(command: Option<&str>) -> String {
    match command {
        None => {
            let mut commands = vec!["codex", "claude"];
            commands.extend_from_slice(DISPATCH_COMMANDS);
            commands.extend_from_slice(SPEC_ONLY_HELP_COMMANDS);
            format!(
                "usage: team-agent <command> [options]\n\nCommands: {}\n\nRun `team-agent <command> --help` for command flags.",
                commands.join(", ")
            )
        }
        Some("init") => "usage: team-agent init [--workspace WORKSPACE] [--force] [--json]".to_string(),
        Some("quick-start") => "usage: team-agent quick-start [TEAMDIR] [--workspace WORKSPACE] [--name NAME] [--team-id TEAM|--team TEAM] [--yes] [--fresh] [--json]\n\ndefaults: display_backend=none; set display_backend: adaptive in TEAM.md to opt in to adaptive display windows.".to_string(),
        Some("start") => "usage: team-agent start [TEAMDIR] [--yes] [--fresh] [--json]".to_string(),
        Some("compile") => "usage: team-agent compile --team TEAM [--out FILE] [--json]".to_string(),
        Some("send") => "usage: team-agent send TARGET MESSAGE... [--workspace WORKSPACE] [--team TEAM] [--targets AGENTS] [--task TASK] [--sender SENDER] [--watch-result] [--requires-ack|--no-ack] [--no-wait] [--timeout SECONDS] [--confirm-human] [--message-id ID] [--json]".to_string(),
        Some("allow-peer-talk") => "usage: team-agent allow-peer-talk A B [--workspace WORKSPACE] [--json]".to_string(),
        Some("status") => "usage: team-agent status [AGENT] [--workspace WORKSPACE] [--team TEAM] [--summary|--json] [--detail]".to_string(),
        Some("stop") => "usage: team-agent stop [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]".to_string(),
        Some("shutdown") => "usage: team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]".to_string(),
        Some("restart") => "usage: team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json]".to_string(),
        Some("restart-agent") => "usage: team-agent restart-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]".to_string(),
        Some("reset-agent") => "usage: team-agent reset-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]".to_string(),
        Some("start-agent") => "usage: team-agent start-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--force] [--allow-fresh] [--no-display] [--json]".to_string(),
        Some("stop-agent") => "usage: team-agent stop-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("add-agent") => "usage: team-agent add-agent AGENT --role-file FILE [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]".to_string(),
        Some("fork-agent") => "usage: team-agent fork-agent SOURCE_AGENT --as AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]".to_string(),
        Some("remove-agent") => "usage: team-agent remove-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--from-spec] [--confirm] [--force] [--json]".to_string(),
        Some("purge-agent") => "usage: team-agent purge-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--force] [--json]".to_string(),
        Some("stuck-list") => "usage: team-agent stuck-list [--workspace WORKSPACE] [--json]".to_string(),
        Some("stuck-cancel") => "usage: team-agent stuck-cancel AGENT [--workspace WORKSPACE] [--alert-type stuck|idle_fallback|cross_worker_deadlock|all] [--json]".to_string(),
        Some("acknowledge-idle") => "usage: team-agent acknowledge-idle [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("takeover") => "usage: team-agent takeover [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]".to_string(),
        Some("claim-leader") => "usage: team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]".to_string(),
        Some("attach-leader") => "usage: team-agent attach-leader [--workspace WORKSPACE] [--team TEAM] [--pane PANE] [--provider PROVIDER] [--confirm] [--json]".to_string(),
        Some("identity") => "usage: team-agent identity [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("approvals") => "usage: team-agent approvals [AGENT] [--workspace WORKSPACE] [--json]".to_string(),
        Some("inbox") => "usage: team-agent inbox AGENT [--workspace WORKSPACE] [--limit N] [--since CURSOR] [--json]".to_string(),
        Some("doctor") => "usage: team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--gate orphans|comms] [--comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]".to_string(),
        Some("watch") => "usage: team-agent watch [--workspace WORKSPACE] [--team TEAM]".to_string(),
        Some("sessions") => "usage: team-agent sessions [--workspace WORKSPACE] [--json]".to_string(),
        Some("validate") => "usage: team-agent validate [SPEC] [--json]".to_string(),
        Some("profile") => "usage: team-agent profile COMMAND NAME [--workspace WORKSPACE] [--team TEAM] [--auth-mode MODE] [--json]".to_string(),
        Some("validate-result") => "usage: team-agent validate-result [ENVELOPE] [--file FILE|--result JSON] [--json]".to_string(),
        Some("collect") => "usage: team-agent collect [--workspace WORKSPACE] [--team TEAM] [--result-file FILE] [--json]".to_string(),
        Some("settle") => "usage: team-agent settle [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("repair-state") => "usage: team-agent repair-state --task TASK --status STATUS [SUMMARY] [--assignee AGENT] [--workspace WORKSPACE] [--json]".to_string(),
        Some("diagnose") => "usage: team-agent diagnose [--workspace WORKSPACE] [--json]".to_string(),
        Some("preflight") => "usage: team-agent preflight [TEAMDIR] [--json]".to_string(),
        Some("wait-ready") => "usage: team-agent wait-ready [--workspace WORKSPACE] [--timeout SECONDS] [--json]".to_string(),
        Some("e2e") => "usage: team-agent e2e [--workspace WORKSPACE] [--providers LIST] [--real] [--json]".to_string(),
        Some("peek") => "usage: team-agent peek AGENT [--workspace WORKSPACE] [--tail N|--head N] [--search TEXT] [--allow-raw-screen] [--json]".to_string(),
        Some("coordinator") => "usage: team-agent coordinator [--workspace WORKSPACE] [--once] [--tick-interval SECONDS]".to_string(),
        Some(other) => format!("usage: team-agent {other} [options]"),
    }
}

fn emit_unknown_subcommand_usage(command: &str) -> ExitCode {
    emit_usage_error(&format!(
        "argument {{codex,claude,...,doctor}}: invalid choice: '{command}' (choose from codex, claude, ..., doctor)"
    ));
    ExitCode::Usage
}

fn emit_usage_error(message: &str) {
    eprintln!("usage: team-agent [-h] {{codex,claude,...,doctor}} ...");
    eprintln!("team-agent: error: {message}");
}

/// `cmd_validate` delegates to runtime validate_file.
pub fn cmd_validate(args: &ValidateArgs) -> Result<CmdResult, CliError> {
    let spec = resolve_path(&args.spec);
    let value = if spec.is_dir() {
        validate_team_dir(&spec)?
    } else {
        validate_spec_file(&spec)?
    };
    Ok(CmdResult::from_json(value, args.json))
}

fn validate_spec_file(spec_path: &Path) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(spec_path)?;
    let base_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let spec =
        crate::model::spec::load_and_validate_spec(&text, base_dir).map_err(model_error_to_cli)?;
    let team = spec
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(crate::model::yaml::Value::as_str)
        .unwrap_or("");
    let workspace = spec
        .get("team")
        .and_then(|team| team.get("workspace"))
        .and_then(crate::model::yaml::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.to_path_buf());
    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(true));
    obj.insert("team".to_string(), Value::String(team.to_string()));
    obj.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string_lossy().to_string()),
    );
    Ok(Value::Object(obj))
}

fn validate_team_dir(team_dir: &Path) -> Result<Value, CliError> {
    let spec = crate::compiler::compile_team(team_dir).map_err(model_error_to_cli)?;
    let team = spec
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(crate::model::yaml::Value::as_str)
        .unwrap_or("");
    let workspace = spec
        .get("team")
        .and_then(|team| team.get("workspace"))
        .and_then(crate::model::yaml::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| team_dir.to_path_buf());
    let agents = spec
        .get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|agent| {
                    agent
                        .get("id")
                        .and_then(crate::model::yaml::Value::as_str)
                        .map(|id| Value::String(id.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(true));
    obj.insert("type".to_string(), Value::String("team_dir".to_string()));
    obj.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string_lossy().to_string()),
    );
    obj.insert("team".to_string(), Value::String(team.to_string()));
    obj.insert("agents".to_string(), Value::Array(agents));
    Ok(Value::Object(obj))
}

fn resolve_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn model_error_to_cli(error: crate::model::errors::ModelError) -> CliError {
    match error {
        crate::model::errors::ModelError::Validation(message) => CliError::Runtime(message),
        other => CliError::Runtime(other.to_string()),
    }
}

fn emit_cli_error(command: &str, args: &[String], cwd: &Path, error: &CliError) -> ExitCode {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    let log_path = cli_error_log_path(&workspace);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let normalized = normalize_cli_error(error);
    let payload_error = normalized.as_ref().unwrap_or(error);
    let _ = std::fs::write(&log_path, format!("{payload_error}\n"));
    let payload = payload_error.to_payload(&log_path, command);
    if has_arg(args, "--json") {
        if let Ok(value) = serde_json::to_value(payload) {
            println!("{}", python_compact_json(&value));
        }
    } else {
        eprintln!("error: {}", payload.error);
        eprintln!("action: {}", payload.action);
        eprintln!("log: {}", payload.log);
    }
    ExitCode::Error
}

fn normalize_cli_error(error: &CliError) -> Option<CliError> {
    match error {
        CliError::Runtime(message) => Some(CliError::Runtime(
            message
                .strip_prefix("validation error: ")
                .unwrap_or(message)
                .to_string(),
        )),
        _ => None,
    }
}

fn cli_error_log_path(workspace: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.6f");
    workspace
        .join(".team")
        .join("logs")
        .join(format!("cli-error-{stamp}.log"))
}

struct PythonCompactFormatter;

impl serde_json::ser::Formatter for PythonCompactFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

fn python_compact_json(value: &Value) -> String {
    let mut bytes = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut bytes, PythonCompactFormatter);
    if value.serialize(&mut ser).is_err() {
        return "{}".to_string();
    }
    String::from_utf8(bytes).unwrap_or_else(|_| "{}".to_string())
}

fn has_arg(args: &[String], needle: &str) -> bool {
    args.iter().any(|arg| arg == needle)
}

#[derive(Debug, Default)]
struct ParsedArgs {
    positionals: Vec<String>,
    workspace: Option<PathBuf>,
    team: Option<String>,
    json: bool,
    yes: bool,
    fresh: bool,
    name: Option<String>,
    team_id: Option<String>,
    targets: Option<String>,
    task: Option<String>,
    sender: Option<String>,
    watch_result: bool,
    requires_ack: bool,
    no_ack: bool,
    no_wait: bool,
    timeout: Option<f64>,
    confirm_human: bool,
    detail: bool,
    summary: bool,
    keep_logs: bool,
    allow_fresh: bool,
    session_converge_deadline_ms: Option<u64>,
    force: bool,
    no_display: bool,
    discard_session: bool,
    role_file: Option<String>,
    as_agent: Option<String>,
    label: Option<String>,
    from_spec: bool,
    confirm: bool,
    alert_type: Option<String>,
    limit: Option<usize>,
    since: Option<String>,
    gate: Option<String>,
    comms: bool,
    fix: bool,
    fix_schema: bool,
    cleanup_orphans: bool,
    once: bool,
    tick_interval: Option<f64>,
    status_value: Option<String>,
    providers: Option<String>,
    allow_raw_screen: bool,
    tail: Option<usize>,
    head: Option<usize>,
    search: Option<String>,
    result_file: Option<PathBuf>,
    file: Option<PathBuf>,
    result: Option<String>,
    real: bool,
    assignee: Option<String>,
    out: Option<PathBuf>,
    auth_mode: Option<String>,
    pane: Option<String>,
    provider: Option<String>,
    message_id: Option<String>,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut parsed = ParsedArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        let Some(arg) = args.get(i) else {
            break;
        };
        match arg.as_str() {
            "--workspace" => {
                parsed.workspace = next_arg(args, &mut i).map(PathBuf::from);
            }
            "--team" => parsed.team = next_arg(args, &mut i),
            "--json" => parsed.json = true,
            "--yes" => parsed.yes = true,
            "--fresh" => parsed.fresh = true,
            "--name" => parsed.name = next_arg(args, &mut i),
            "--team-id" => parsed.team_id = next_arg(args, &mut i),
            "--targets" | "--target" | "--to" => parsed.targets = next_arg(args, &mut i),
            "--task" => parsed.task = next_arg(args, &mut i),
            "--sender" => parsed.sender = next_arg(args, &mut i),
            "--watch-result" => parsed.watch_result = true,
            "--requires-ack" => parsed.requires_ack = true,
            "--no-ack" => parsed.no_ack = true,
            "--no-wait" => parsed.no_wait = true,
            "--timeout" => {
                parsed.timeout = next_arg(args, &mut i).and_then(|v| v.parse::<f64>().ok())
            }
            "--confirm-human" => parsed.confirm_human = true,
            "--detail" => parsed.detail = true,
            "--summary" => parsed.summary = true,
            "--keep-logs" => parsed.keep_logs = true,
            "--allow-fresh" => parsed.allow_fresh = true,
            "--session-converge-deadline" => {
                parsed.session_converge_deadline_ms =
                    next_arg(args, &mut i).and_then(|v| parse_seconds_ms(&v));
            }
            "--force" => parsed.force = true,
            "--no-display" => parsed.no_display = true,
            "--discard-session" => parsed.discard_session = true,
            "--role-file" => parsed.role_file = next_arg(args, &mut i),
            "--as" => parsed.as_agent = next_arg(args, &mut i),
            "--label" => parsed.label = next_arg(args, &mut i),
            "--from-spec" => parsed.from_spec = true,
            "--confirm" => parsed.confirm = true,
            "--alert-type" => parsed.alert_type = next_arg(args, &mut i),
            "--limit" => {
                parsed.limit = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok())
            }
            "--since" => parsed.since = next_arg(args, &mut i),
            "--gate" => parsed.gate = next_arg(args, &mut i),
            "--comms" => parsed.comms = true,
            "--fix" => parsed.fix = true,
            "--fix-schema" => parsed.fix_schema = true,
            "--cleanup-orphans" => parsed.cleanup_orphans = true,
            "--once" => parsed.once = true,
            "--tick-interval" => {
                parsed.tick_interval = next_arg(args, &mut i).and_then(|v| v.parse::<f64>().ok())
            }
            "--status" => parsed.status_value = next_arg(args, &mut i),
            "--providers" => parsed.providers = next_arg(args, &mut i),
            "--allow-raw-screen" => parsed.allow_raw_screen = true,
            "--tail" => parsed.tail = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok()),
            "--head" => parsed.head = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok()),
            "--search" => parsed.search = next_arg(args, &mut i),
            "--result-file" => parsed.result_file = next_arg(args, &mut i).map(PathBuf::from),
            "--file" => parsed.file = next_arg(args, &mut i).map(PathBuf::from),
            "--result" => parsed.result = next_arg(args, &mut i),
            "--real" => parsed.real = true,
            "--assignee" => parsed.assignee = next_arg(args, &mut i),
            "--out" => parsed.out = next_arg(args, &mut i).map(PathBuf::from),
            "--auth-mode" => parsed.auth_mode = next_arg(args, &mut i),
            "--pane" => parsed.pane = next_arg(args, &mut i),
            "--provider" => parsed.provider = next_arg(args, &mut i),
            "--message-id" => parsed.message_id = next_arg(args, &mut i),
            "-h" | "--help" => {}
            other if other.starts_with("--team=") => {
                parsed.team = Some(other.trim_start_matches("--team=").to_string());
            }
            other if other.starts_with("--pane=") => {
                parsed.pane = Some(other.trim_start_matches("--pane=").to_string());
            }
            other if other.starts_with("--provider=") => {
                parsed.provider = Some(other.trim_start_matches("--provider=").to_string());
            }
            other if other.starts_with('-') => {}
            other => parsed.positionals.push(other.to_string()),
        }
        i = i.saturating_add(1);
    }
    parsed
}

fn next_arg(args: &[String], index: &mut usize) -> Option<String> {
    *index = index.saturating_add(1);
    args.get(*index).cloned()
}

fn parse_seconds_ms(raw: &str) -> Option<u64> {
    let seconds = raw.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some((seconds * 1000.0).round() as u64)
    } else {
        None
    }
}

fn parse_cli_provider(raw: Option<&str>) -> Result<crate::provider::Provider, CliError> {
    let raw = raw.unwrap_or("codex");
    serde_json::from_value::<crate::provider::Provider>(serde_json::json!(raw))
        .map_err(|_| CliError::Runtime(format!("unknown provider: {raw}")))
}

fn workspace(parsed: &ParsedArgs, cwd: &Path) -> PathBuf {
    parsed
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf())
}

fn required_pos(parsed: &ParsedArgs, index: usize, name: &str) -> Result<String, CliError> {
    parsed
        .positionals
        .get(index)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("missing {name}")))
}

fn quick_start_args(args: &[String], cwd: &Path) -> Result<QuickStartArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    let agents_dir = parsed
        .positionals
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.clone());
    let agents_dir = if agents_dir.is_absolute() {
        agents_dir
    } else {
        workspace.join(agents_dir)
    };
    Ok(QuickStartArgs {
        workspace,
        agents_dir,
        name: parsed.name,
        team_id: parsed.team_id.or(parsed.team),
        yes: parsed.yes,
        fresh: parsed.fresh,
        json: parsed.json,
    })
}

fn init_args(args: &[String], cwd: &Path) -> InitArgs {
    let parsed = parse_args(args);
    InitArgs {
        workspace: workspace(&parsed, cwd),
        force: parsed.force,
        json: parsed.json,
    }
}

fn compile_args(args: &[String], cwd: &Path) -> Result<CompileArgs, CliError> {
    let parsed = parse_args(args);
    let team = parsed
        .team
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Usage("missing --team".to_string()))?;
    let out = parsed
        .out
        .unwrap_or_else(|| PathBuf::from("team.spec.yaml"));
    Ok(CompileArgs {
        team: resolve_cli_path(cwd, &team),
        out: resolve_cli_path(cwd, &out),
        json: parsed.json,
    })
}

fn resolve_cli_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn send_args(args: &[String], cwd: &Path) -> Result<SendArgs, CliError> {
    let parsed = parse_args(args);
    let target = if parsed.targets.is_some() {
        None
    } else {
        parsed.positionals.first().cloned()
    };
    let message_start = usize::from(target.is_some());
    let workspace = workspace(&parsed, cwd);
    Ok(SendArgs {
        target,
        message: parsed
            .positionals
            .iter()
            .skip(message_start)
            .cloned()
            .collect(),
        targets: parsed.targets,
        workspace,
        team: parsed.team,
        task: parsed.task,
        sender: parsed.sender.unwrap_or_else(|| "leader".to_string()),
        no_ack: parsed.no_ack && !parsed.requires_ack,
        no_wait: parsed.no_wait,
        watch_result: parsed.watch_result,
        timeout: parsed.timeout.unwrap_or(30.0),
        confirm_human: parsed.confirm_human,
        json: parsed.json,
        message_id: parsed.message_id,
    })
}

fn allow_peer_talk_args(args: &[String], cwd: &Path) -> Result<AllowPeerTalkArgs, CliError> {
    let parsed = parse_args(args);
    Ok(AllowPeerTalkArgs {
        a: required_pos(&parsed, 0, "a")?,
        b: required_pos(&parsed, 1, "b")?,
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    })
}

fn status_args(args: &[String], cwd: &Path) -> StatusArgs {
    let parsed = parse_args(args);
    StatusArgs {
        agent: parsed.positionals.first().cloned(),
        workspace: workspace(&parsed, cwd),
        detail: parsed.detail,
        summary: parsed.summary,
        json: parsed.json,
    }
}

fn watch_args(args: &[String], cwd: &Path) -> WatchArgs {
    let parsed = parse_args(args);
    WatchArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
    }
}

fn approvals_args(args: &[String], cwd: &Path) -> ApprovalsArgs {
    let parsed = parse_args(args);
    ApprovalsArgs {
        agent: parsed.positionals.first().cloned(),
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    }
}

fn inbox_args(args: &[String], cwd: &Path) -> Result<InboxArgs, CliError> {
    let parsed = parse_args(args);
    Ok(InboxArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        limit: parsed.limit.unwrap_or(20),
        since: parsed.since,
        json: parsed.json,
    })
}

fn takeover_args(args: &[String], cwd: &Path) -> TakeoverArgs {
    let parsed = parse_args(args);
    TakeoverArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        confirm: parsed.confirm,
        json: parsed.json,
    }
}

fn claim_leader_args(args: &[String], cwd: &Path) -> ClaimLeaderArgs {
    let parsed = parse_args(args);
    ClaimLeaderArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        confirm: parsed.confirm,
        json: parsed.json,
    }
}

fn attach_leader_args(args: &[String], cwd: &Path) -> Result<AttachLeaderArgs, CliError> {
    let parsed = parse_args(args);
    Ok(AttachLeaderArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        pane: parsed
            .pane
            .filter(|pane| !pane.is_empty())
            .map(crate::transport::PaneId::new),
        provider: parse_cli_provider(parsed.provider.as_deref())?,
        confirm: parsed.confirm,
        json: parsed.json,
    })
}

fn identity_args(args: &[String], cwd: &Path) -> IdentityArgs {
    let parsed = parse_args(args);
    IdentityArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        json: parsed.json,
    }
}

fn shutdown_args(args: &[String], cwd: &Path) -> ShutdownArgs {
    let parsed = parse_args(args);
    ShutdownArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        keep_logs: parsed.keep_logs,
        json: parsed.json,
    }
}

fn restart_args(args: &[String], cwd: &Path) -> RestartArgs {
    let parsed = parse_args(args);
    RestartArgs {
        workspace: parsed
            .positionals
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace(&parsed, cwd)),
        team: parsed.team,
        allow_fresh: parsed.allow_fresh,
        session_converge_deadline_ms: parsed.session_converge_deadline_ms,
        json: parsed.json,
    }
}

fn start_agent_args(args: &[String], cwd: &Path) -> Result<StartAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(StartAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        force: parsed.force,
        allow_fresh: parsed.allow_fresh,
        no_display: parsed.no_display,
        json: parsed.json,
    })
}

fn stop_agent_args(args: &[String], cwd: &Path) -> Result<StopAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(StopAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        json: parsed.json,
    })
}

fn reset_agent_args(args: &[String], cwd: &Path) -> Result<ResetAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(ResetAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        discard_session: parsed.discard_session,
        no_display: parsed.no_display,
        json: parsed.json,
    })
}

fn add_agent_args(args: &[String], cwd: &Path) -> Result<AddAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(AddAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        role_file: parsed
            .role_file
            .ok_or_else(|| CliError::Usage("missing --role-file".to_string()))?,
        no_display: parsed.no_display,
        json: parsed.json,
    })
}

fn fork_agent_args(args: &[String], cwd: &Path) -> Result<ForkAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(ForkAgentArgs {
        source_agent: required_pos(&parsed, 0, "source_agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        as_agent: parsed
            .as_agent
            .ok_or_else(|| CliError::Usage("missing --as".to_string()))?,
        label: parsed.label,
        no_display: parsed.no_display,
        json: parsed.json,
    })
}

fn remove_agent_args(args: &[String], cwd: &Path) -> Result<RemoveAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(RemoveAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        from_spec: parsed.from_spec,
        confirm: parsed.confirm,
        force: parsed.force,
        json: parsed.json,
    })
}

fn stuck_list_args(args: &[String], cwd: &Path) -> StuckListArgs {
    let parsed = parse_args(args);
    StuckListArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    }
}

fn stuck_cancel_args(args: &[String], cwd: &Path) -> Result<StuckCancelArgs, CliError> {
    let parsed = parse_args(args);
    Ok(StuckCancelArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        alert_type: alert_type(parsed.alert_type.as_deref())?,
        json: parsed.json,
    })
}

fn alert_type(raw: Option<&str>) -> Result<Option<AlertType>, CliError> {
    match raw {
        Some("stuck") => Ok(Some(AlertType::Stuck)),
        Some("idle_fallback") => Ok(Some(AlertType::IdleFallback)),
        Some("cross_worker_deadlock") => Ok(Some(AlertType::CrossWorkerDeadlock)),
        Some("all") | None => Ok(None),
        Some(other) => Err(CliError::Usage(format!("invalid --alert-type: {other}"))),
    }
}

fn acknowledge_idle_args(args: &[String], cwd: &Path) -> AcknowledgeIdleArgs {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    AcknowledgeIdleArgs {
        team: parsed.team,
        workspace,
        json: parsed.json,
    }
}

fn doctor_args(args: &[String], cwd: &Path) -> DoctorArgs {
    let parsed = parse_args(args);
    DoctorArgs {
        spec: parsed.positionals.first().map(PathBuf::from),
        workspace: workspace(&parsed, cwd),
        gate: doctor_gate(parsed.gate.as_deref()),
        comms: parsed.comms,
        team: parsed.team,
        fix: parsed.fix,
        fix_schema: parsed.fix_schema,
        cleanup_orphans: parsed.cleanup_orphans,
        confirm: parsed.confirm,
        json: parsed.json,
    }
}

fn doctor_gate(raw: Option<&str>) -> Option<DoctorGate> {
    match raw {
        Some("orphans") => Some(DoctorGate::Orphans),
        Some("comms") => Some(DoctorGate::Comms),
        _ => None,
    }
}

fn sessions_args(args: &[String], cwd: &Path) -> SessionsArgs {
    let parsed = parse_args(args);
    SessionsArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    }
}

fn validate_args(args: &[String], cwd: &Path) -> ValidateArgs {
    let parsed = parse_args(args);
    ValidateArgs {
        spec: parsed
            .positionals
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join("team.spec.yaml")),
        json: parsed.json,
    }
}

fn profile_args(args: &[String], cwd: &Path) -> Result<ProfileArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = resolve_cli_path(cwd, &workspace(&parsed, cwd));
    Ok(ProfileArgs {
        command: required_pos(&parsed, 0, "profile command")?,
        name: required_pos(&parsed, 1, "profile name")?,
        workspace: resolve_path(&workspace),
        team: parsed.team,
        auth_mode: parsed.auth_mode,
        json: parsed.json,
    })
}

fn validate_result_args(args: &[String]) -> Result<ValidateResultArgs, CliError> {
    let parsed = parse_args(args);
    Ok(ValidateResultArgs {
        envelope: parsed.positionals.first().cloned(),
        file: parsed.file,
        result: parsed.result,
        json: parsed.json,
    })
}

fn collect_args(args: &[String], cwd: &Path) -> CollectArgs {
    let parsed = parse_args(args);
    CollectArgs {
        workspace: workspace(&parsed, cwd),
        result_file: parsed.result_file,
        json: parsed.json,
    }
}

fn settle_args(args: &[String], cwd: &Path) -> SettleArgs {
    let parsed = parse_args(args);
    SettleArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    }
}

fn repair_state_args(args: &[String], cwd: &Path) -> Result<RepairStateArgs, CliError> {
    let parsed = parse_args(args);
    Ok(RepairStateArgs {
        workspace: workspace(&parsed, cwd),
        task_id: parsed
            .task
            .ok_or_else(|| CliError::Usage("missing --task".to_string()))?,
        assignee: parsed.assignee,
        status: parsed
            .status_value
            .ok_or_else(|| CliError::Usage("missing --status".to_string()))?,
        summary: option_value(args, "--summary").or_else(|| parsed.positionals.first().cloned()),
        json: parsed.json,
    })
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let mut i = 0usize;
    while i < args.len() {
        let arg = args.get(i)?;
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        if arg == flag {
            return args
                .get(i.saturating_add(1))
                .filter(|value| !value.starts_with('-'))
                .cloned();
        }
        i = i.saturating_add(1);
    }
    None
}

fn diagnose_args(args: &[String], cwd: &Path) -> DiagnoseArgs {
    let parsed = parse_args(args);
    DiagnoseArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
    }
}

fn preflight_args(args: &[String], cwd: &Path) -> PreflightArgs {
    let parsed = parse_args(args);
    let team = parsed
        .team
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| parsed.positionals.first().map(PathBuf::from))
        .unwrap_or_else(|| cwd.to_path_buf());
    PreflightArgs {
        team,
        json: parsed.json,
    }
}

fn wait_ready_args(args: &[String], cwd: &Path) -> WaitReadyArgs {
    let parsed = parse_args(args);
    WaitReadyArgs {
        workspace: workspace(&parsed, cwd),
        timeout: parsed.timeout.unwrap_or(60.0),
        json: parsed.json,
    }
}

fn e2e_args(args: &[String], cwd: &Path) -> E2eArgs {
    let parsed = parse_args(args);
    let providers = parsed
        .providers
        .as_deref()
        .unwrap_or("fake")
        .split(',')
        .filter_map(|p| {
            let trimmed = p.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect();
    E2eArgs {
        workspace: workspace(&parsed, cwd),
        providers,
        real: parsed.real,
        json: parsed.json,
    }
}

fn peek_args(args: &[String], cwd: &Path) -> Result<PeekArgs, CliError> {
    let parsed = parse_args(args);
    Ok(PeekArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        tail: parsed.tail.unwrap_or(80),
        head: parsed.head,
        search: parsed.search,
        allow_raw_screen: parsed.allow_raw_screen,
        json: parsed.json,
    })
}
fn run_coordinator(args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    let parsed = parse_args(args);
    let workspace = crate::coordinator::WorkspacePath::new(workspace(&parsed, cwd));
    crate::coordinator::run_daemon(crate::coordinator::DaemonArgs {
        workspace,
        once: parsed.once,
        tick_interval_sec: parsed.tick_interval,
    })
    .map(|()| ExitCode::Ok)
    .map_err(|e| CliError::Runtime(e.to_string()))
}

fn human_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => json_dumps_like(value),
    }
}

fn json_dumps_like(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => match serde_json::to_string(s) {
            Ok(text) => text,
            Err(_) => "\"\"".to_string(),
        },
        Value::Array(arr) => {
            let inner = arr
                .iter()
                .map(json_dumps_like)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Object(obj) => {
            let inner = obj
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        json_dumps_like(&Value::String(k.clone())),
                        json_dumps_like(v)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn tmp_workspace() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-cli-emit-test-{}-{}",
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cli_argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn source_dispatch_commands() -> Vec<&'static str> {
        let source = include_str!("emit.rs");
        let after_start = source.split_once("fn dispatch(").unwrap().1;
        let dispatch_source = after_start.split_once("const DISPATCH_COMMANDS").unwrap().0;
        let mut commands = Vec::new();
        for line in dispatch_source.lines() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix('"') else {
                continue;
            };
            let Some((command, after_command)) = rest.split_once('"') else {
                continue;
            };
            let after_command = after_command.trim_start();
            if (after_command.starts_with("=>") || after_command.starts_with("if "))
                && !commands.contains(&command)
            {
                commands.push(command);
            }
        }
        commands
    }

    #[test]
    fn t0_help_catalog_tracks_dispatch_commands() {
        let source_commands = source_dispatch_commands();
        for command in &source_commands {
            assert!(
                DISPATCH_COMMANDS.contains(command),
                "dispatch command `{command}` is missing from DISPATCH_COMMANDS"
            );
        }
        for command in DISPATCH_COMMANDS {
            assert!(
                source_commands.contains(command),
                "DISPATCH_COMMANDS contains `{command}` but dispatch has no matching arm"
            );
        }

        let top_help = command_help(None);
        for command in DISPATCH_COMMANDS {
            assert!(
                top_help.contains(command),
                "top-level --help is missing dispatch command `{command}`"
            );
            let command_help = command_help(Some(command));
            assert!(
                command_help.contains("usage: team-agent") && command_help.contains(command),
                "`team-agent {command} --help` must show command-specific usage, got {command_help:?}"
            );
        }
        for command in SPEC_ONLY_HELP_COMMANDS {
            assert!(
                top_help.contains(command),
                "top-level --help is missing spec-only help command `{command}`"
            );
        }
    }

    #[test]
    fn t0_help_catalog_lists_command_flags() {
        for (command, flags) in [
            (
                "quick-start",
                &["--workspace", "--team-id", "--yes", "--fresh", "--json"][..],
            ),
            (
                "send",
                &[
                    "--workspace",
                    "--team",
                    "--targets",
                    "--watch-result",
                    "--timeout",
                    "--json",
                ][..],
            ),
            (
                "status",
                &["--workspace", "--team", "--summary", "--json", "--detail"][..],
            ),
            (
                "shutdown",
                &["--workspace", "--team", "--keep-logs", "--json"][..],
            ),
            (
                "restart",
                &[
                    "--team",
                    "--allow-fresh",
                    "--session-converge-deadline",
                    "--json",
                ][..],
            ),
            (
                "start-agent",
                &[
                    "--workspace",
                    "--team",
                    "--force",
                    "--allow-fresh",
                    "--no-display",
                    "--json",
                ][..],
            ),
            (
                "reset-agent",
                &[
                    "--workspace",
                    "--team",
                    "--discard-session",
                    "--no-display",
                    "--json",
                ][..],
            ),
            (
                "add-agent",
                &[
                    "--role-file",
                    "--workspace",
                    "--team",
                    "--no-display",
                    "--json",
                ][..],
            ),
            (
                "fork-agent",
                &[
                    "--as",
                    "--label",
                    "--workspace",
                    "--team",
                    "--no-display",
                    "--json",
                ][..],
            ),
            (
                "remove-agent",
                &[
                    "--workspace",
                    "--team",
                    "--from-spec",
                    "--confirm",
                    "--force",
                    "--json",
                ][..],
            ),
            (
                "doctor",
                &[
                    "--workspace",
                    "--team",
                    "--gate",
                    "--fix-schema",
                    "--cleanup-orphans",
                    "--json",
                ][..],
            ),
            (
                "attach-leader",
                &[
                    "--workspace",
                    "--team",
                    "--pane",
                    "--provider",
                    "--confirm",
                    "--json",
                ][..],
            ),
            (
                "collect",
                &["--workspace", "--team", "--result-file", "--json"][..],
            ),
            (
                "repair-state",
                &["--task", "--status", "--assignee", "--workspace", "--json"][..],
            ),
            ("wait-ready", &["--workspace", "--timeout", "--json"][..]),
            (
                "peek",
                &[
                    "--workspace",
                    "--tail",
                    "--head",
                    "--search",
                    "--allow-raw-screen",
                    "--json",
                ][..],
            ),
            (
                "coordinator",
                &["--workspace", "--once", "--tick-interval"][..],
            ),
        ] {
            let help = command_help(Some(command));
            for flag in flags {
                assert!(
                    help.contains(flag),
                    "`team-agent {command} --help` is missing {flag}"
                );
            }
        }
    }

    #[test]
    fn ux_quick_start_workspace_resolves_relative_agents_dir_inside_workspace() {
        let cwd = tmp_workspace();
        let ws = tmp_workspace();
        let args = quick_start_args(
            &cli_argv(&[
                "--workspace",
                &ws.to_string_lossy(),
                "agents",
                "--yes",
                "--json",
            ]),
            &cwd,
        )
        .unwrap();
        assert_eq!(
            args.agents_dir,
            ws.join("agents"),
            "quick-start --workspace <ws> agents must resolve the role-doc dir under <ws>, so team-in-team \
             setup works from any caller cwd"
        );
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&ws);
    }
}
