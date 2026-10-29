//!
//! cli · emit — `emit`(--json vs 人读 dict 逐键)+ 顶层 `run` 调度(parser.py `main`)+
//! 人读标量/集合渲染(`human_value` / `json_dumps_like`)。

use super::models::cmd_models;
use super::spec::{command_spec, CommandKind, COMMAND_SPECS};
#[cfg(test)]
use super::spec::{CommandTier, ALL_DISPATCH_KINDS};
use super::*;
use std::io::{ErrorKind, Write as _};

///
/// `emit`(`helpers.py:12-23`):`--json`→`json.dumps(indent=2, ensure_ascii=False, sort_keys=True)`;
/// 否则 dict 逐键 `key: value`(嵌套 dict/list 内联 compact json,`ensure_ascii=False`)、非 dict 直接 print。
/// 返回应打印到 stdout 的字符串(bin main 负责实际 println)。
pub fn emit(output: &CmdOutput, as_json: bool) -> Option<String> {
    emit_with_json_order(output, as_json, false)
}

fn emit_with_json_order(
    output: &CmdOutput,
    as_json: bool,
    preserve_json_order: bool,
) -> Option<String> {
    match output {
        CmdOutput::None => None,
        CmdOutput::Human(text) => Some(crate::redaction::redact_external_text(text)),
        CmdOutput::Json(value) => {
            let value = crate::redaction::redact_external_value(value);
            if as_json {
                return if preserve_json_order {
                    serde_json::to_string_pretty(&value).ok()
                } else {
                    serde_json::to_string_pretty(&sort_json(&value)).ok()
                };
            }
            if let Value::Object(obj) = value {
                let lines: Vec<String> = obj
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", human_value(value)))
                    .collect();
                Some(lines.join("\n"))
            } else {
                Some(human_value(&value))
            }
        }
    }
}

///
/// `main(argv)`(`parser.py:84`):**CLI 唯一进程入口**。codex/claude/copilot/grok passthrough 早返回 →
/// 解析 argv 到 subcommand → 调对应 handler → 异常落盘 + 信封 + `ExitCode::Error` →
/// `consume_leader_inbox_summary` → `emit` → `result.ok is False ? Error : Ok`。
/// **行为入口**:契约可端到端跑 argv→(stdout, exit code)。
pub fn run(argv: &[String], cwd: &Path) -> ExitCode {
    let Some(command) = argv.first().map(String::as_str) else {
        println!("{}", default_help());
        return ExitCode::Ok;
    };
    if command == "route" {
        return super::route::run(&argv[1..]);
    }
    if command == "leader-prompt" {
        return super::leader_prompt::run(&argv[1..]);
    }
    if is_leader_passthrough_command(command) {
        return match cmd_leader_passthrough(command, &argv[1..], cwd) {
            Ok(result) => emit_result(result),
            Err(error) => emit_cli_error(command, &argv[1..], cwd, &error),
        };
    }
    if matches!(command, "-h" | "--help" | "help") {
        println!("{}", command_help(None));
        return ExitCode::Ok;
    }
    if matches!(command, "-V" | "--version") {
        println!("team-agent {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::Ok;
    }
    if let Some(result) = super::intent::run(command, &argv[1..], cwd) {
        return match result {
            Ok(result) => emit_result(result),
            Err(error) => emit_cli_error_for_command(command, &argv[1..], cwd, &error),
        };
    }
    // Classify syntax/scope errors before dispatch; contextual status refusals
    // retain their existing stderr-only contract. Resolve the scope only once.
    if is_machine_command(command)
        || matches!(command_spec(command).map(|spec| spec.kind), Some(CommandKind::Dispatch(_)))
    {
        let checked = match prepare_control_args(command, &argv[1..], cwd) {
            Ok(checked) => checked,
            Err(error) => return emit_parser_usage(command, &argv[1..], &error),
        };
        let error_args = checked.tokens.clone();
        return match dispatch_prepared(command, checked, cwd) {
            Ok(exit) => exit,
            Err(error) => emit_cli_error_for_command(command, &error_args, cwd, &error),
        };
    }
    match dispatch(command, &argv[1..], cwd) {
        Ok(exit) => exit,
        Err(error) => emit_cli_error_for_command(command, &argv[1..], cwd, &error),
    }
}

fn emit_cli_error_for_command(
    command: &str,
    args: &[String],
    cwd: &Path,
    error: &CliError,
) -> ExitCode {
    let missing_input = matches!(error, CliError::Usage(message) if message.starts_with("missing ") || message.starts_with("Provide ") || matches!(message.as_str(), "add-agent requires --provider <name>" | "--source is required unless --uninstall"));
    if command_spec(command).is_some() && missing_input {
        let help = command_help(Some(command));
        let explanation = match error {
            CliError::Usage(message) => match message.as_str() {
                "missing agent" | "missing source_agent" => "Provide an agent name.".to_string(),
                "missing profile command" => {
                    "Provide a profile operation: init, doctor or show.".to_string()
                }
                "missing profile name" => "Provide an authentication/proxy profile name.".to_string(),
                "add-agent requires --provider <name>" => {
                    "Select a provider with --provider, or supply a role file with --role-file.".to_string()
                }
                other => other
                    .strip_prefix("missing ")
                    .map(|field| format!("Provide the required argument: {field}."))
                    .unwrap_or_else(|| other.to_string()),
            },
            _ => error.to_string(),
        };
        if super::arguments::has_flag(command, args, "--json") {
            let payload = error.to_payload(Path::new(""), command);
            let mut value = serde_json::to_value(payload)
                .unwrap_or_else(|_| serde_json::json!({"ok": false, "error": error.to_string()}));
            value["error"] = serde_json::json!(explanation);
            value["action"] = serde_json::json!(format!("team-agent {command} --help"));
            value["next_actions"] = serde_json::json!([format!(
                "Complete the required arguments using the Examples in team-agent {command} --help"
            )]);
            println!("{}", python_compact_json(&value));
        } else {
            eprintln!("{explanation}\n\n{help}");
        }
        ExitCode::Usage
    } else if command == "status" {
        emit_status_cli_error(error)
    } else if matches!(error, CliError::Usage(_)) {
        emit_parser_usage(command, args, error)
    } else {
        emit_cli_error(command, args, cwd, error)
    }
}

#[cfg(test)]
pub(crate) fn __test_runtime_error_emitter(command: &str, cwd: &Path, error: &CliError) -> ExitCode {
    emit_cli_error(command, &[], cwd, error)
}

fn emit_status_cli_error(error: &CliError) -> ExitCode {
    let normalized = normalize_cli_error(error);
    let payload_error = normalized.as_ref().unwrap_or(error);
    let safe_error = crate::redaction::redact_external_text(&payload_error.to_string());
    eprintln!("error: {safe_error}");
    ExitCode::Error
}

/// Print a handler's CmdResult to stdout (emit formats json/human), then surface its exit code.
/// (parser.py: `print(emit(result, as_json))` then the ok→exit mapping.)
pub(super) fn emit_result(r: CmdResult) -> ExitCode {
    let persisted_message_id = match &r.output {
        CmdOutput::Json(value) => value
            .get("message_id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        _ => None,
    };
    if let Some(text) = emit_with_json_order(&r.output, r.as_json, r.preserve_json_order) {
        if let Err(error) = write_stdout_line(&text) {
            if let Some(message_id) = persisted_message_id {
                let stderr = std::io::stderr();
                let mut stderr = stderr.lock();
                let _ = writeln!(
                    stderr,
                    "Output unavailable; the message was persisted with message_id={message_id}. Use team-agent inbox leader -n 3 for replies; do not resend."
                );
            }
            return if error.kind() == ErrorKind::BrokenPipe {
                r.exit
            } else {
                ExitCode::Error
            };
        }
    }
    r.exit
}

fn write_stdout_line(text: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(text.as_bytes())?;
    stdout.write_all(b"\n")
}

#[cfg(test)]
pub(crate) fn __test_dispatch(
    command: &str,
    args: &[String],
    cwd: &Path,
) -> Result<ExitCode, CliError> {
    dispatch(command, args, cwd)
}

fn dispatch(command: &str, args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    if command == "route" {
        return Ok(super::route::run(args));
    }
    if command == "leader-prompt" {
        return Ok(super::leader_prompt::run(args));
    }
    if let Some(result) = super::intent::run(command, args, cwd) {
        return result.map(emit_result);
    }
    let machine = is_machine_command(command);
    if !machine && !matches!(command_spec(command).map(|spec| spec.kind), Some(CommandKind::Dispatch(_))) {
        return Ok(emit_unknown_subcommand_usage(command));
    }
    dispatch_prepared(command, prepare_control_args(command, args, cwd)?, cwd)
}

fn prepare_control_args(command: &str, args: &[String], cwd: &Path) -> Result<super::arguments::CheckedArgs, CliError> {
    let mut checked = super::arguments::parse(command, args)?;
    let raw_send = command == "send" && checked.tokens.iter().any(|token| token == "--socket");
    if !checked.help && !raw_send { checked.tokens = resolve_command_scope(command, checked.tokens, cwd)?; }
    Ok(checked)
}

fn dispatch_prepared(command: &str, checked: super::arguments::CheckedArgs, cwd: &Path) -> Result<ExitCode, CliError> {
    let machine = is_machine_command(command);
    if checked.help {
        if machine {
            eprintln!("{HUMAN_NAVIGATION}");
            return Ok(ExitCode::Usage);
        }
        println!("{}", command_help(Some(command)));
        return Ok(ExitCode::Ok);
    }
    let args = checked.tokens.as_slice();
    if machine { return dispatch_machine(command, args, cwd); }
    if args.iter().all(|arg| arg == "--json")
        && matches!(
            command,
            "send"
                | "inbox"
                | "add-agent"
                | "start-agent"
                | "stop-agent"
                | "reset-agent"
                | "clone-agent"
                | "fork-agent"
                | "remove-agent"
                | "profile"
        )
    {
        return Err(CliError::Usage(
            "Provide the agent name, task message or required command settings; see Usage and Examples below".to_string(),
        ));
    }
    match command {
        "quick-start" => cmd_quick_start(&quick_start_args(args, cwd)?).map(emit_result),
        "send" => {
            let parsed = parse_args(args);
            if parsed.socket.is_some() || parsed.dialect.is_some() {
                let socket = parsed.socket.as_deref().ok_or_else(|| CliError::Usage("raw send requires --socket".into()))?;
                let pane = parsed.pane.as_deref().ok_or_else(|| CliError::Usage("raw send requires --pane".into()))?;
                let dialect = parsed.dialect.as_deref().ok_or_else(|| CliError::Usage("raw send requires --dialect".into()))?;
                if parsed.targets.is_some() || parsed.to_name.is_some() || parsed.to_leader.is_some() || parsed.mailbox {
                    return Err(CliError::Usage("raw send cannot be combined with semantic addressing or --mailbox".into()));
                }
                crate::direct_delivery::direct_send(socket, pane, dialect, &parsed.positionals.join(" ")).map_err(CliError::Runtime)?;
                Ok(emit_result(CmdResult::from_json(serde_json::json!({"ok": true, "status": "sent", "delivery": "physical", "pane_id": pane, "accepted": null}), parsed.json)))
            } else { cmd_send(&send_args(args, cwd)?).map(emit_result) }
        },
        "locate" => {
            let parsed = parse_args(args);
            let value = crate::direct_delivery::locate(&workspace(&parsed, cwd), parsed.team.as_deref()).map_err(CliError::Usage)?;
            Ok(emit_result(CmdResult::from_json(value, parsed.json)))
        },
        "schedule" => super::schedule::run(args, cwd).map(emit_result),
        "status" => cmd_status_for_team(&status_args(args, cwd), parse_args(args).team.as_deref())
            .map(emit_result),
        "shutdown" => cmd_shutdown(&shutdown_args(args, cwd)?).map(emit_result),
        "restart" => cmd_restart(&restart_args(args, cwd)?).map(emit_result),
        "start-agent" => cmd_start_agent(&start_agent_args(args, cwd)?).map(emit_result),
        "stop-agent" => cmd_stop_agent(&stop_agent_args(args, cwd)?).map(emit_result),
        "reset-agent" => cmd_reset_agent(&reset_agent_args(args, cwd)?).map(emit_result),
        "add-agent" => cmd_add_agent(&add_agent_args(args, cwd)?).map(emit_result),
        "clone-agent" => cmd_clone_agent(&clone_agent_args(args, cwd)?).map(emit_result),
        "fork-agent" => cmd_fork_agent(&fork_agent_args(args, cwd)?).map(emit_result),
        "remove-agent" => cmd_remove_agent(&remove_agent_args(args, cwd)?).map(emit_result),
        "takeover" => cmd_takeover(&takeover_args(args, cwd)).map(emit_result),
        "claim-leader" => cmd_claim_leader(&claim_leader_args(args, cwd)).map(emit_result),
        // Real dispatch: `cmd_attach_leader` writes the `leader_receiver` binding.
        "attach-leader" => cmd_attach_leader(&attach_leader_args(args, cwd)?).map(emit_result),

        "approvals" => cmd_approvals(&approvals_args(args, cwd)).map(emit_result),
        "inbox" => cmd_inbox(&inbox_args(args, cwd)?).map(emit_result),
        "doctor" => cmd_doctor(&doctor_args(args, cwd)).map(emit_result),

        // 0.5.9 E7 host-leader-registry: `leaders` is the host-level derived
        // discovery command. It reads ~/.team-agent/leaders, validates each
        // entry against canonical state, and reports LIVE/STALE/AMBIGUOUS
        // status. `--to-leader NAME` on `send` uses the same registry to
        // resolve short/qualified/hash-qualified names to a canonical
        // (workspace, team_key) tuple and delegates to the E6 named-leader
        // delivery path — no separate route authority.
        "leaders" => cmd_leaders(&leaders_args(args, cwd)?).map(emit_result),
        "models" => cmd_models(&models_args(args)?).map(emit_result),
        "kiro" => Ok(emit_result(cmd_kiro_leader(args))),

        "install-skill" => cmd_install_skill(&install_skill_args(args)?).map(emit_result),
        "profile" => cmd_profile(&profile_args(args, cwd)?).map(emit_result),

        _ => Ok(emit_unknown_subcommand_usage(command)),
    }
}

// Script compatibility is deliberately outside the human catalog and suggestions.
fn is_machine_command(command: &str) -> bool {
    matches!(command, "wait" | "coordinator" | "attach-app-server-leader")
}

fn dispatch_machine(command: &str, args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    match command {
        "wait" => cmd_wait(&wait_args(args, cwd)?).map(emit_result),
        "coordinator" => run_coordinator(args, cwd),
        "attach-app-server-leader" => {
            cmd_attach_app_server_leader(&attach_app_server_leader_args(args, cwd)?)
                .map(emit_result)
        }
        _ => Ok(emit_unknown_subcommand_usage(command)),
    }
}

// Command grammar, not provider identity parsing: these are top-level CLI
// passthrough verbs for starting a leader under a provider executable.
const LEADER_PASSTHROUGH_COMMANDS: &[&str] =
    &["codex", "claude", "copilot", "grok", "pi"];

pub(crate) fn is_leader_passthrough_command(command: &str) -> bool {
    LEADER_PASSTHROUGH_COMMANDS.contains(&command)
        && matches!(
            command_spec(command).map(|spec| spec.kind),
            Some(CommandKind::LeaderPassthrough { .. })
        )
}

/// Registered subcommands (the dispatch table) plus spec-only verbs that have no
/// dispatch arm yet but must still respond to `--help` per CR-063/G4.
/// Used by the `--help` short-circuit gate so unknown commands keep falling through
/// to the argparse invalid-choice path.
fn is_known_subcommand(command: &str) -> bool {
    command_spec(command).is_some_and(|spec| spec.command_help)
}

pub(crate) fn default_help() -> String {
    let mut out = String::from("Team Agent: create a team, send tasks, read replies and shut down safely.\n\n1. Prepare agents\n   Prepare TEAM.md and agents/*.md with explicit provider and bypass decisions; use --from for an external source.\n2. Open a leader and create the team\n   With Pi installed and signed in, run team-agent pi.\n   Run team-agent start in that leader's tool context. Verified live members stay unchanged; stopped members are started.\n   Use team restart for a deliberate restart (session auto); legacy bare restart retains preserve. Do not take over or delete an existing team.\n3. Send tasks and read replies\n   team-agent send worker 'Calculate 245 * 37 and reply to the leader.'\n   team-agent inbox leader -n 3\n   team-agent status\n   Sending is not completion; wait for an actual reply.\n4. Diagnose and stop\n   team-agent doctor --workspace .\n   team-agent stop --workspace . --json\n   Verify this team's residual resources are empty, then diagnose the selected workspace.\n\nSelect workspace/team scope with --workspace/--team. Discover model names with models.\nWindows ConPTY requires a Windows host and an installed shim; tmux commands are not a generic Windows path.\n");
    append_help_section(
        &mut out,
        "Getting started",
        &["start", "quick-start", "send", "schedule", "status", "models", "inbox"],
    );
    append_help_section(&mut out, "Team lifecycle", &["stop", "team", "restart", "shutdown"]);
    append_help_section(
        &mut out,
        "Agent lifecycle",
        &[
            "agent",
            "add-agent",
            "start-agent",
            "stop-agent",
            "reset-agent",
            "clone-agent",
            "fork-agent",
            "remove-agent",
        ],
    );
    append_help_section(
        &mut out,
        "Observation and collaboration",
        &["locate", "leaders", "doctor", "approvals"],
    );
    append_help_section(
        &mut out,
        "Configuration",
        &["route", "leader-prompt", "profile", "install-skill"],
    );
    append_help_section(
        &mut out,
        "Guided recovery",
        &["claim-leader", "takeover", "attach-leader"],
    );
    append_help_section(
        &mut out,
        "Leader launch",
        &["pi", "codex", "claude", "copilot", "grok", "kiro"],
    );
    out.push_str("\nCommand arguments and examples: team-agent <command> --help");
    out
}

fn append_help_section(out: &mut String, title: &str, names: &[&str]) {
    out.push('\n');
    out.push_str(title);
    out.push_str(":\n");
    for name in names {
        if let Some(spec) = command_spec(name).filter(|spec| spec.default_help) {
            out.push_str(&format!("  {:<13} {}\n", spec.name, spec.summary));
        }
    }
}

///
/// Test-only public accessor for `command_help` — allows integration
/// tests to grep the help copy without depending on internal parser
/// machinery.
pub fn __test_command_help(command: Option<&str>) -> String {
    command_help(command)
}

///
/// Test-only public accessor for `quick_start_args` — allows
/// integration tests to exercise the parser without going through
/// stdio + the full `main` entrypoint.
pub fn __test_quick_start_args(
    args: &[String],
    cwd: &std::path::Path,
) -> Result<crate::cli::types::QuickStartArgs, crate::cli::CliError> {
    quick_start_args(args, cwd)
}

pub(super) const TEAM_TEMPLATE: &str =
    "---\nname: current\n---\nA small team for a command-line example.\n";
pub(super) const WORKER_TEMPLATE: &str = "---\nname: worker\nrole: assistant\nprovider: pi\nmodel: openai-codex/gpt-6-luna\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\nComplete the task and send a concise reply to the leader.\n";
const HUMAN_NAVIGATION: &str = "Use team-agent --help to list available commands.";

pub(super) fn command_help(command: Option<&str>) -> String {
    if let Some(name) = command.filter(|name| matches!(*name, "start" | "stop" | "team" | "agent")) {
        return super::intent::help(name);
    }
    let Some(name) = command else {
        return default_help();
    };
    let Some(spec) = command_spec(name) else {
        return HUMAN_NAVIGATION.to_string();
    };
    if name == "route" {
        return super::route::HELP.to_string();
    }
    if name == "leader-prompt" {
        return super::leader_prompt::HELP.to_string();
    }
    let (details, examples, next) = match name {
        "quick-start" => (
            "TEAMDIR contains TEAM.md and agents/; --workspace selects the runtime project.\n--name sets the team name; --team/--team-id selects the team; --yes retains compatibility; --detail includes full diagnostics.\nProviders must be installed and signed in. Declare provider/model/effort in role files; permission bypass defaults to false.\n--backend tmux is for POSIX; conpty requires a Windows host and an installed ConPTY shim.",
            "team-agent quick-start\nteam-agent quick-start ./roles --workspace ./project\nteam-agent quick-start . --json",
            "Compatibility bridge only. In the authorized Leader context, prefer team-agent start --from TEAMDIR for complete new configuration and team-agent start for later activation. Check models and native sign-in first; inspect status and actual replies after starting."),
        "send" => ("<agent> is an agent name from the read-only endpoint snapshot; MESSAGE is the task text.\nPaste + Enter completes synchronously without coordinator queues or SQLite writes. Physical success is not provider acceptance.\n--socket SOCKET --pane %N --dialect PROVIDER sends directly without a workspace snapshot. Automatic snapshot provider aliases select the native sequence: Claude/Cursor 100ms, Grok 150ms, Pi/Codex no floor, then one Enter.\nExplicit double-enter submits twice; cursor-double-enter retains Cursor\'s 100ms floor. No automatic retry or third Enter.\n--mailbox appends to the independent inbox without injecting into the conversation.",
            "team-agent send worker 'Calculate 245 * 37 and reply to the leader.'\nteam-agent send worker 'Review the changes' --workspace . --team help-demo\nteam-agent send worker 'Read this when next available' --mailbox",
            "Acceptance is not delivery or completion. Wait for an actual reply; use team-agent inbox leader -n 3. Diagnose with status/doctor rather than repeated sends."),
        "locate" => ("Read the endpoint snapshot and list live tmux panes without starting or contacting the coordinator.",
            "team-agent locate --workspace . --json\nteam-agent locate --team help-demo", "Use the socket, pane_id and dialect for raw send, or the agent name for workspace-scoped send."),
        "schedule" => ("add requires exactly one of --in or --every and non-empty --text. Durations are positive integers followed by ms, s, m, h or d.\nlist shows active reminders; cancel takes the sched_<hex> id returned by add.",
            "team-agent schedule add --in 10m --text 'Check progress'\nteam-agent schedule add --every 1h --text 'Review status'\nteam-agent schedule list --json\nteam-agent schedule cancel sched_1",
            "Uses the existing coordinator timer; schedules survive daemon restarts. Delivery has tick granularity. Cancellation stops future occurrences, not reminders already firing or queued. A stopped coordinator resumes timers when restarted, without replaying missed intervals."),
        "inbox" => ("<agent> is the recipient name; -n/--limit bounds the message count. Reading replies does not resend messages.",
            "team-agent inbox leader -n 3\nteam-agent inbox worker --limit 5\nteam-agent inbox leader --workspace . --team help-demo --json", "Wait for a natural reply, then check team-agent status if needed. Do not inspect worker terminal content."),
        "status" => ("Optional <agent> selects one agent. --summary/--detail retain compatibility and do not add fields.\nNine fields: name/provider/model/effort/runtime_status/activity/health/session_name/tmux_command.\nRuntime, activity and health are distinct. unknown means insufficient evidence; null means unset. No default model is guessed.",
            "team-agent status\nteam-agent status worker\nteam-agent status --workspace . --team help-demo --json", "Read replies with team-agent inbox leader -n 3; diagnose problems with team-agent doctor --workspace ."),
        "models" => ("--provider selects the provider, default pi. QUERY or --search performs keyword search; they are mutually exclusive.\nUse cursor (or cursor-agent, agent, cursor_agent) for the contract-native Cursor catalog. Copilot has no model discovery entrypoint here. Results come from the native catalog, not guesses.",
            "team-agent models --provider pi\nteam-agent models --provider pi --search luna\nteam-agent models --provider cursor --json", "Use the exact returned model in a role file or team-agent start AGENT --model MODEL; stop a running seat before changing its settings."),
        "restart" => ("restart AGENT and team restart default to --session auto; bare/explicit-path restart keeps preserve.\n--session preserve requires continuation evidence; auto permits only the original legal fresh fallback, not guaranteed saved memory.\n--dry-run plans without lifecycle effects. --allow-fresh is the legacy authorized opt-in; --session-converge-deadline is in seconds.",
            "team-agent restart worker --dry-run --json\nteam-agent restart worker --session preserve --workspace . --dry-run --json", "Use team-agent team restart for deliberate team recovery; choose --session preserve when context must be retained. Team restart leaves explicitly stopped/paused seats alone; team-agent start intentionally activates the non-paused roster. Inspect partial/unknown outcomes with status/doctor, never blind reset or retry."),
        "shutdown" => ("Compatibility bridge: stop only the selected workspace/team. Logs are retained by default; --keep-logs remains compatible.\n--json reports scope, degradation and owned residuals. Success does not mean other teams were shut down.",
            "team-agent shutdown --workspace .\nteam-agent shutdown --workspace . --team help-demo --json", "Prefer team-agent stop --workspace . --team KEY --json. Verify this team's owned residuals are empty; use team-agent start for authorized later activation. Do not clean up other teams or shared servers."),
        "add-agent" => ("Compatibility add-only entry. <agent> names the new seat; --provider/--model/--effort select intentional launch settings.\n--bypass requires true|false; --prompt sets responsibilities; --profile selects a local profile.\nProvider and bypass must be explicit in arguments or --role-file. Conflicting settings are refused before writes. --force requires explicit authorization, a positively dead old pane and no live cohort;\nfor a removed seat, --force re-adopts the role file it left behind and CLI values override it.",
            "team-agent add-agent reviewer --provider pi --model openai-codex/gpt-6-luna --bypass false --prompt 'Review tasks'\nteam-agent add-agent reviewer --role-file ./agents/reviewer.md", "Prefer team-agent start reviewer --role-file FILE for add-or-wake, or team-agent agent add reviewer for strict new-seat behavior. Keep provider/bypass explicit; check status and send a task after actual startup."),
        "start-agent" => ("Compatibility entry starts an existing stopped seat only; a running seat is refused.\n--provider/--model/--effort/--bypass select intentional settings; --prompt replaces permanent instructions and --profile selects local authentication.\nA different provider is refused. Use --allow-fresh only after explicit authorization to lose saved context.",
            "team-agent start-agent worker\nteam-agent start-agent worker --model openai-codex/gpt-6-luna", "Prefer team-agent start worker for daily add-or-wake or verified live no-op. Use team-agent stop worker before applying changed settings; inspect status and send a task afterward. A no-op is not role synchronization or business readiness."),
        "stop-agent" => ("Compatibility single-agent stop; configuration and saved session associations are retained, not removed.",
            "team-agent stop-agent worker\nteam-agent stop-agent worker --workspace . --team help-demo --json", "Prefer team-agent stop worker. Verify the stopped result; intentionally wake with team-agent start worker, then check status before sending new work."),
        "reset-agent" => ("Compatibility entry requires explicit --discard-session. Stops the old process, clears the saved association and attempts fresh startup. Paused agents stay paused; not all native history is deleted.",
            "team-agent reset-agent worker --discard-session\nteam-agent reset-agent worker --discard-session --workspace . --team help-demo --json", "Prefer team-agent agent reset worker --discard-session only after explicit context-loss approval. Never use reset as a resume-refusal fallback; check the actual result/status before sending new work."),
        "clone-agent" => ("Compatibility entry: <agent> is the source; --as names the new agent; --label sets a readable label.\nCopies configuration, not a complete conversation.",
            "team-agent clone-agent worker --as reviewer\nteam-agent clone-agent worker --as reviewer --label 'Review agent' --workspace .", "Prefer team-agent agent clone worker --as reviewer. Check the new agent with status, then send it an independent task; original capability gates still apply."),
        "fork-agent" => ("Compatibility entry: <agent> is the source; --as names the new agent; --label sets a readable label.\nRequires captured context and admitted new-seat fork capability. Unsupported providers refuse without fallback.",
            "team-agent fork-agent worker --as experiment\nteam-agent fork-agent worker --as experiment --workspace . --team help-demo --json", "Prefer team-agent agent fork worker --as experiment where admitted. Inspect status before sending work; finish with team-agent stop experiment or the explicit agent remove operation."),
        "remove-agent" => ("Compatibility entry requires --confirm. Spec-defined agents also require --from-spec; dynamic agents do not.\nDeletes only the managed role copy, not external user files. Stop a running agent first.\n--force stops/removes a running agent only with explicit authorization and the original safety checks.",
            "team-agent remove-agent reviewer --confirm\nteam-agent remove-agent reviewer --from-spec --confirm --workspace . --team help-demo --json", "Prefer team-agent agent remove reviewer --confirm with any required --from-spec. Save replies first and verify the resulting scope/status; do not force-clean other agents."),
        "leaders" => ("Lists live leaders by default; --all includes stale registrations and --stale selects stale entries.\nQUERY/--search filters workspace/team/name. --prune removes verified retired registrations; preview with --dry-run.",
            "team-agent leaders\nteam-agent leaders --all --json\nteam-agent leaders --prune --dry-run", "Select --workspace/--team from the reported scope. Registry cleanup is not process shutdown."),
        "doctor" => ("Read-only diagnosis by default; SPEC is an optional configuration path.\n--comms/--gate comms checks channels, not actual replies; --gate orphans checks residual resources.\nUse --fix/--fix-schema/--cleanup-orphans only within authorized scope. Dangerous repairs require --confirm.",
            "team-agent doctor --workspace .\nteam-agent doctor --workspace . --team help-demo --json", "Follow the actual issue and selected-team scope. A suggested repair is not an executed repair."),
        "approvals" => ("Optional <agent> selects one agent. Observes permission prompts without granting approval.",
            "team-agent approvals\nteam-agent approvals worker --json", "Verify the requested permission, handle it in the actual provider prompt, then check status."),
        "profile" => ("init creates a profile; doctor checks it; show displays settings. NAME is the profile name.\n--auth-mode selects authentication; --proxy-mode direct bypasses proxies and inherit uses the environment proxy.\nProfiles belong to the selected workspace, not automatically to an isolated --team. Do not expose credentials.",
            "team-agent profile init local --auth-mode subscription\nteam-agent profile doctor local\nteam-agent profile show local", "After validation, use team-agent start AGENT --profile local; stop a running seat before changing settings. Use native sign-in when needed, never copy external credentials."),
        "install-skill" => ("--source must be a verified skill directory; --target selects codex/claude/copilot/all.\n--dest overrides the destination; --dry-run previews; --uninstall removes the target skill. Installation is not required to create a team.",
            "team-agent install-skill --source \"$SKILL_DIR\" --target codex --dry-run\nteam-agent install-skill --source \"$SKILL_DIR\" --target codex", "Set SKILL_DIR to a verified directory and inspect the preview before installation. Do not overwrite unknown user files."),
        "claim-leader" => ("Use only when doctor recommends leader registration and this terminal is the actual leader.\n--confirm authorizes registration; it does not open a new provider.",
            "team-agent claim-leader --workspace . --json\nteam-agent claim-leader --workspace . --team help-demo --confirm --json", "Verify doctor findings and terminal ownership first. A refusal without confirmation is not successful registration."),
        "takeover" => ("Use only when doctor recommends takeover and the user has authorized it. This changes leader ownership.\nWith a usable team terminal, takeover may execute even without --confirm; it is not a read-only preview. Use team restart --session preserve for normal context-preserving recovery.",
            "team-agent takeover --workspace . --team help-demo --confirm\nteam-agent takeover --workspace . --team help-demo --confirm --json", "Verify leader/team ownership before confirmation. Check status/doctor afterwards; do not take over someone else's team."),
        "attach-leader" => ("PANE must be a verified leader terminal, not a guessed pane id.\n--provider selects the actual provider; --confirm authorizes attachment to an existing leader. Use only when doctor recommends it.",
            "team-agent attach-leader --pane \"$PANE\" --provider pi --workspace .\nteam-agent attach-leader --pane \"$PANE\" --provider pi --workspace . --team help-demo --confirm --json", "Set PANE to the verified terminal. Confirm attachment with doctor before sending a task."),
        "kiro" => ("Kiro leader launching is not admitted by this candidate. This entry returns the stable capability refusal kiro_leader_not_admitted (exit 1).\nNo native process, leader binding, attach or provider fallback is attempted. --help/-h is read-only and exits 0.",
            "team-agent kiro\nteam-agent kiro --json", "Use provider: kiro in a complete worker role source with team-agent start --from TEAMDIR; worker support does not grant Leader capability."),
        _ => ("Install the provider and complete native sign-in first. The current directory is the workspace.\nUse --attach-existing/--confirm only after verifying leader ownership and receiving authorization; --attach-session selects a verified session.\n--external-leader selects an external leader; --allow-nested-attach requires explicit authorization for nested terminals.\nArguments after -- pass through unchanged. Bare --help/-h displays this help without starting the provider.",
            "", "Prepare complete TEAM.md/roles with explicit user choices, then run team-agent start in the opened Leader's tool context, never an ordinary worker conversation."),
    };
    let examples = if matches!(spec.kind, CommandKind::LeaderPassthrough { .. }) {
        let native = name;
        if name == "pi" {
            "team-agent pi\nteam-agent pi -- --model openai-codex/gpt-6-luna\npi --help".to_string()
        } else {
            format!("team-agent {name}\nteam-agent {name} --help\n{native} --help")
        }
    } else {
        examples.to_string()
    };
    let scope = if name == "kiro" {
        "Use --json for the structured capability refusal. Native arguments and attach options cannot enable this capability."
    } else if matches!(spec.kind, CommandKind::LeaderPassthrough { .. }) {
        "Use --json for structured output. --workspace/--team are not launcher options here; they pass to the native provider."
    } else {
        "Use --json for structured output. --workspace selects the project; supported commands use --team to select the team."
    };
    let mut out = format!(
        "Purpose:\n{}.\nUsage: {}\n\nOptions:\n{}\n{}\nExamples:\n{}\n\nNext Action:\n{}",
        spec.summary, spec.usage, details, scope, examples, next
    );
    if name == "quick-start" {
        out.push_str(&format!("\n\nMinimal two-file configuration (create manually; not written automatically):\nTEAM.md:\n{TEAM_TEMPLATE}\nagents/worker.md:\n{WORKER_TEMPLATE}"));
    }
    if name == "restart" {
        out.push_str("\nRestart forms: restart AGENT selects one registered agent (default session auto); use agent restart ID to disambiguate a same-named directory. Legacy bare or explicit-path restart retains preserve and original receipts. New team restart defaults to auto. auto never means reset; native capability refusals remain authoritative.");
    }
    if name == "copilot" {
        out.push_str(
            "\nCopilot has no team-agent models entrypoint; consult native provider help.",
        );
    }
    out
}

fn emit_unknown_subcommand_usage(command: &str) -> ExitCode {
    emit_usage_error(&format!(
        "Unknown command: '{command}'. Use team-agent --help to list available commands."
    ));
    // E8 (N38): 错路引导 —— 拼写近似时建议最接近的真子命令(additive,不改既有 golden 行)。
    if let Some(suggestion) = nearest_subcommand(command) {
        eprintln!("Did you mean `{suggestion}`? See team-agent {suggestion} --help for examples.");
    }
    // 0.5.45 naming-addressing (RED-6): unknown subcommand exits 1
    // (Error), aligned with the family of typo refusals throughout
    // send/named. Pre-0.5.45 mapped to Usage (2) argparse-style; the
    // shared refusal shape ("typo diagnostic + advisory suggestion +
    // exit 1") is the invariant callers of `team-agent send` /
    // `--to-name` also see, so keeping unknown-subcommand at 2 was
    // internal drift.
    ExitCode::Error
}

/// 在已知子命令里找与 `input` 最接近的一个。0.5.45 naming-addressing
/// (design §3.2/§4.1) 抽公 shared `model::name_similarity` — 距离
/// 阈值与排序规则跟 CLI `--to-name` typo suggestion 走同一份纯函数,
/// 避免两套 fuzzy 逻辑漂移。既有 `statu -> status` 行为保留(RED-6
/// grep guard + prefix-hit priority)。
fn nearest_subcommand(input: &str) -> Option<&'static str> {
    use crate::model::name_similarity::{rank, Candidate};
    let candidates: Vec<Candidate<&'static str>> = COMMAND_SPECS
        .iter()
        .filter(|spec| spec.suggestion_index)
        .map(|spec| Candidate {
            match_key: spec.name.to_string(),
            stable_key: spec.name.to_string(),
            payload: spec.name,
        })
        .collect();
    rank(input, &candidates).into_iter().next()
}

fn emit_usage_error(message: &str) {
    eprintln!("Usage: team-agent <command>; list commands with team-agent --help");
    eprintln!("Error: {message}");
}

/// `install-skill` 参数(RED-1 根治:把 skill 安装单源收敛到二进制,install.mjs 调它)。
struct InstallSkillArgs {
    target: crate::packaging::SkillTarget,
    dest: Option<PathBuf>,
    dry_run: bool,
    /// `--uninstall`:删 target 的 skill 目标目录(单源,走同一 SkillTarget 表),不需 --source。
    uninstall: bool,
    source: Option<PathBuf>,
    json: bool,
}

fn install_skill_args(args: &[String]) -> Result<InstallSkillArgs, CliError> {
    let parsed = parse_args(args);
    // `--target` 复用 parse_args.targets(codex|claude|copilot|all,默认 all)。
    let target = match parsed.targets.as_deref() {
        None | Some("all") => crate::packaging::SkillTarget::All,
        Some("codex") => crate::packaging::SkillTarget::Codex,
        Some("claude") => crate::packaging::SkillTarget::Claude,
        Some("copilot") => crate::packaging::SkillTarget::Copilot,
        Some(other) => {
            return Err(CliError::Usage(format!(
                "invalid --target: {other} (choose from codex, claude, copilot, all)"
            )))
        }
    };
    let uninstall = args.iter().any(|a| a == "--uninstall");
    // `--source <dir>` 安装时必需(npm 包的 skills/team-agent;运行期无 CARGO_MANIFEST_DIR);
    // 卸载不需要。
    let source = flag_value(args, "--source").map(PathBuf::from);
    if !uninstall && source.is_none() {
        return Err(CliError::Usage("missing --source <skill dir>".to_string()));
    }
    let dest = flag_value(args, "--dest").map(PathBuf::from);
    let dry_run = args.iter().any(|a| a == "--dry-run");
    Ok(InstallSkillArgs {
        target,
        dest,
        dry_run,
        uninstall,
        source,
        json: parsed.json,
    })
}

/// 取 `--flag <value>` 的值(用于 install-skill 的 --source/--dest,parse_args 不覆盖的旗标)。
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    for (index, argument) in args.iter().enumerate() {
        if argument == "--" { break; }
        if argument == flag { return args.get(index + 1).cloned(); }
        if let Some((key, value)) = argument.split_once('=') {
            if key == flag { return Some(value.to_string()); }
        }
    }
    None
}

/// `team-agent install-skill`(RED-1 单源):repo `skills/team-agent` → `~/.codex|.claude|.copilot`。
/// install.mjs 删 JS 拷贝逻辑、改调本命令(`--target all --source <pkg>/skills/team-agent`)。
fn cmd_install_skill(args: &InstallSkillArgs) -> Result<CmdResult, CliError> {
    // 卸载分支(单源:走同一 SkillTarget 表的 dest_dir;all → SINGLE_TARGETS 全集)。
    if args.uninstall {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let targets: Vec<crate::packaging::SkillTarget> = match args.target {
            crate::packaging::SkillTarget::All => {
                crate::packaging::SkillTarget::SINGLE_TARGETS.to_vec()
            }
            t => vec![t],
        };
        let mut removed: Vec<serde_json::Value> = Vec::new();
        for t in targets {
            if let Some(dest) = t.dest_dir(&home) {
                let existed = dest.0.exists();
                if existed && !args.dry_run {
                    std::fs::remove_dir_all(&dest.0)
                        .map_err(|e| CliError::Runtime(e.to_string()))?;
                }
                removed.push(serde_json::json!({
                    "target": t,
                    "dest": dest.0.to_string_lossy(),
                    "removed": existed,
                    "dry_run": args.dry_run,
                }));
            }
        }
        return Ok(CmdResult::from_json(
            serde_json::json!({"ok": true, "uninstalled": removed}),
            args.json,
        ));
    }
    let source = args
        .source
        .clone()
        .ok_or_else(|| CliError::Usage("missing --source <skill dir>".to_string()))?;
    let outcomes =
        crate::packaging::install::install_skill(&crate::packaging::SkillInstallOptions {
            target: args.target,
            dest: args.dest.clone(),
            dry_run: args.dry_run,
            source,
        })
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let installed: Vec<serde_json::Value> = outcomes
        .iter()
        .map(|o| {
            serde_json::json!({
                "target": o.target,
                "dest": o.dest.0.to_string_lossy(),
                "dry_run": o.dry_run,
                "removed_stale": o.removed_stale.len(),
            })
        })
        .collect();
    Ok(CmdResult::from_json(
        serde_json::json!({"ok": true, "installed": installed}),
        args.json,
    ))
}

fn resolve_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn emit_parser_usage(command: &str, args: &[String], error: &CliError) -> ExitCode {
    if super::arguments::has_flag(command, args, "--json") {
        let mut payload = error.to_payload(Path::new(""), command);
        payload.action = format!("Use team-agent {command} --help to correct the command arguments.");
        if let Ok(value) = serde_json::to_value(payload) {
            println!("{}", python_compact_json(&crate::redaction::redact_external_value(&value)));
        }
    } else {
        eprintln!("error: {}\nUse team-agent {command} --help.", crate::redaction::redact_external_text(&error.to_string()));
    }
    ExitCode::Usage
}

fn emit_cli_error(command: &str, args: &[String], cwd: &Path, error: &CliError) -> ExitCode {
    if matches!(error, CliError::Usage(_)) { return emit_parser_usage(command, args, error); }
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    let log_path = cli_error_log_path(&workspace);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let normalized = normalize_cli_error(error);
    let payload_error = normalized.as_ref().unwrap_or(error);
    let safe_error = crate::redaction::redact_external_text(&payload_error.to_string());
    let _ = std::fs::write(&log_path, format!("{safe_error}\n"));
    let mut payload = payload_error.to_payload(&log_path, command);
    payload.error = safe_error;
    if let CliError::AddressRefusal { error, action, .. } = payload_error {
        if !super::arguments::has_flag(command, args, "--json") {
            payload.error = crate::redaction::redact_external_text(error);
            payload.action = action.clone();
        }
    } else if let Some(reason) = payload
        .error
        .strip_prefix("usage error: ")
        .unwrap_or(&payload.error)
        .strip_prefix("Error: ")
        .and_then(|text| text.split_once(':').map(|(reason, _)| reason))
    {
        if let Some(message) = super::named_address::human_address_reason(reason) {
            payload.error = message.to_string();
            payload.action = "Run team-agent status and use an agent name from the list. Select scope with --workspace/--team; diagnose channel issues with doctor.".to_string();
        }
    }
    if command_spec(command).is_some()
        && payload.action
            == "Run team-agent doctor --workspace . for the selected team, or inspect the error log shown here."
    {
        let mut doctor = format!(
            "team-agent doctor --workspace {}",
            super::adapters::shell_quote(&workspace.to_string_lossy())
        );
        if let Some(team) = parsed.team.as_deref() {
            doctor.push_str(&format!(" --team {}", super::adapters::shell_quote(team)));
        }
        payload.action = format!("Run {doctor} for the selected team, or inspect the error log shown here.");
    }
    payload.action = crate::redaction::redact_external_text(&payload.action);
    payload.log = crate::redaction::redact_external_text(&payload.log);
    payload.reason = payload
        .reason
        .map(|value| crate::redaction::redact_external_text(&value));
    payload.session_name = payload
        .session_name
        .map(|value| crate::redaction::redact_external_text(&value));
    payload.next_actions = payload.next_actions.map(|values| {
        values
            .into_iter()
            .map(|value| crate::redaction::redact_external_text(&value))
            .collect()
    });
    if super::arguments::has_flag(command, args, "--json") {
        if let Ok(value) = serde_json::to_value(payload) {
            println!("{}", python_compact_json(&value));
        }
    } else {
        eprintln!("error: {}", payload.error);
        eprintln!("action: {}", payload.action);
        if matches!(payload_error, CliError::AddressRefusal { .. }) {
            eprintln!("log: {:?}", payload.log);
        } else {
            eprintln!("log: {}", payload.log);
        }
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
    args.iter().take_while(|arg| arg.as_str() != "--").any(|arg| arg == needle)
}

#[derive(Debug, Default)]
struct ParsedArgs {
    positionals: Vec<String>,
    workspace: Option<PathBuf>,
    team: Option<String>,
    json: bool,
    yes: bool,
    name: Option<String>,
    team_id: Option<String>,
    targets: Option<String>,
    task: Option<String>,
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
    discard_session: bool,
    role_file: Option<String>,
    as_agent: Option<String>,
    label: Option<String>,
    from_spec: bool,
    confirm: bool,
    limit: Option<usize>,
    gate: Option<String>,
    comms: bool,
    fix: bool,
    fix_schema: bool,
    cleanup_orphans: bool,
    once: bool,
    tick_interval: Option<f64>,
    status_value: Option<String>,
    search: Option<String>,
    file: Option<PathBuf>,
    result: Option<String>,
    assignee: Option<String>,
    auth_mode: Option<String>,
    proxy_mode: Option<String>,
    pane: Option<String>,
    to_name: Option<String>,
    /// E7 (0.5.9 host-leader-registry-design §4.2): `send --to-leader NAME`
    /// resolves NAME through `~/.team-agent/leaders` to a canonical target
    /// (workspace, team_key) then delegates to the E6 leader delivery path.
    /// Mutually exclusive with `--to-name`, TARGET, `--pane`, and `--to`.
    to_leader: Option<String>,
    provider: Option<String>,
    socket: Option<String>,
    dialect: Option<String>,
    thread_id: Option<String>,
    message_id: Option<String>,
    mailbox: bool,
    presentation_sink: Option<String>,
    message_class: Option<String>,
    case_id: Option<String>,
    content: Option<String>,
    primary_error: Option<String>,
    agent_id: Option<String>,
    task_id: Option<String>,
    result_json: Option<String>,
    /// 0.5.x Phase 1d Batch 2: quick-start `--backend <tmux|conpty>`.
    /// Raw string (validated at the quick-start builder); the factory
    /// enforces literal semantics.
    backend: Option<String>,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut expanded = Vec::new();
    let mut literal = false;
    for argument in args {
        if argument == "--" { literal = true; }
        if !literal && argument.starts_with("--") {
            if let Some((flag, value)) = argument.split_once('=') {
                expanded.extend([flag.to_string(), value.to_string()]);
                continue;
            }
        }
        expanded.push(argument.clone());
    }
    let args = expanded.as_slice();
    let mut parsed = ParsedArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        let Some(arg) = args.get(i) else {
            break;
        };
        match arg.as_str() {
            "--" => {
                parsed.positionals.extend_from_slice(&args[i + 1..]);
                break;
            }
            "--workspace" => {
                parsed.workspace = next_arg(args, &mut i).map(PathBuf::from);
            }
            "--team" => parsed.team = next_arg(args, &mut i),
            "--json" => parsed.json = true,
            "--yes" => parsed.yes = true,
            "--name" => parsed.name = next_arg(args, &mut i),
            "--team-id" => parsed.team_id = next_arg(args, &mut i),
            "--targets" | "--target" | "--to" => parsed.targets = next_arg(args, &mut i),
            "--task" => parsed.task = next_arg(args, &mut i),
            "--task-id" => parsed.task_id = next_arg(args, &mut i),
            "--agent-id" => parsed.agent_id = next_arg(args, &mut i),
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
            "--backend" => parsed.backend = next_arg(args, &mut i),
            "--discard-session" => parsed.discard_session = true,
            "--role-file" => parsed.role_file = next_arg(args, &mut i),
            "--as" => parsed.as_agent = next_arg(args, &mut i),
            "--label" => parsed.label = next_arg(args, &mut i),
            "--from-spec" => parsed.from_spec = true,
            "--confirm" => parsed.confirm = true,
            "-n" | "--limit" => {
                parsed.limit = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok())
            }
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
            "--search" => parsed.search = next_arg(args, &mut i),
            "--file" => parsed.file = next_arg(args, &mut i).map(PathBuf::from),
            "--result" => parsed.result = next_arg(args, &mut i),
            "--assignee" => parsed.assignee = next_arg(args, &mut i),
            "--auth-mode" => parsed.auth_mode = next_arg(args, &mut i),
            "--proxy-mode" => parsed.proxy_mode = next_arg(args, &mut i),
            "--pane" => parsed.pane = next_arg(args, &mut i),
            "--to-name" => parsed.to_name = next_arg(args, &mut i),
            "--to-leader" => parsed.to_leader = next_arg(args, &mut i),
            "--provider" => parsed.provider = next_arg(args, &mut i),
            "--socket" => parsed.socket = next_arg(args, &mut i),
            "--dialect" => parsed.dialect = next_arg(args, &mut i),
            "--thread-id" => parsed.thread_id = next_arg(args, &mut i),
            "--message-id" => parsed.message_id = next_arg(args, &mut i),
            "--mailbox" => parsed.mailbox = true,
            "--presentation-sink" => parsed.presentation_sink = next_arg(args, &mut i),
            "--message-class" => parsed.message_class = next_arg(args, &mut i),
            "--case-id" => parsed.case_id = next_arg(args, &mut i),
            "--content" => parsed.content = next_arg(args, &mut i),
            "--primary-error" => parsed.primary_error = next_arg(args, &mut i),
            "--result-json" => parsed.result_json = next_arg(args, &mut i),
            "-h" | "--help" => {}
            other if other.starts_with("--team=") => {
                parsed.team = Some(other.trim_start_matches("--team=").to_string());
            }
            other if other.starts_with("--proxy-mode=") => {
                parsed.proxy_mode = Some(other.trim_start_matches("--proxy-mode=").to_string());
            }
            other if other.starts_with("--pane=") => {
                parsed.pane = Some(other.trim_start_matches("--pane=").to_string());
            }
            other if other.starts_with("--to-name=") => {
                parsed.to_name = Some(other.trim_start_matches("--to-name=").to_string());
            }
            other if other.starts_with("--to-leader=") => {
                parsed.to_leader = Some(other.trim_start_matches("--to-leader=").to_string());
            }
            other if other.starts_with("--presentation-sink=") => {
                parsed.presentation_sink =
                    Some(other.trim_start_matches("--presentation-sink=").to_string());
            }
            other if other.starts_with("--message-class=") => {
                parsed.message_class =
                    Some(other.trim_start_matches("--message-class=").to_string());
            }
            other if other.starts_with("--case-id=") => {
                parsed.case_id = Some(other.trim_start_matches("--case-id=").to_string());
            }
            other if other.starts_with("--provider=") => {
                parsed.provider = Some(other.trim_start_matches("--provider=").to_string());
            }
            other if other.starts_with("--search=") => {
                parsed.search = Some(other.trim_start_matches("--search=").to_string());
            }
            other if other.starts_with("--socket=") => {
                parsed.socket = Some(other.trim_start_matches("--socket=").to_string());
            }
            other if other.starts_with("--thread-id=") => {
                parsed.thread_id = Some(other.trim_start_matches("--thread-id=").to_string());
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

pub(super) fn parse_seconds_ms(raw: &str) -> Option<u64> {
    let seconds = raw.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some((seconds * 1000.0).round() as u64)
    } else {
        None
    }
}

fn parse_cli_provider(raw: &str) -> Result<crate::provider::Provider, CliError> {
    serde_json::from_value::<crate::provider::Provider>(serde_json::json!(raw))
        .map_err(|_| CliError::Usage(format!("unknown provider: {raw}")))
}

pub(super) fn explicit_path(value: &str) -> bool {
    value == "." || value == ".." || value.contains('/') || value.contains('\\')
        || Path::new(value).is_absolute()
        || (value.as_bytes().get(1) == Some(&b':') && value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
}

fn checked_workspace(path: &Path, cwd: &Path) -> Result<PathBuf, CliError> {
    let path = resolve_cli_path(cwd, path);
    let canonical = std::fs::canonicalize(&path).map_err(|_| CliError::Usage(format!(
        "workspace does not exist or is unavailable: {}", path.display()
    )))?;
    if !canonical.is_dir() {
        return Err(CliError::Usage(format!("workspace is not a directory: {}", path.display())));
    }
    Ok(canonical)
}

fn resolve_command_scope(command: &str, mut args: Vec<String>, cwd: &Path) -> Result<Vec<String>, CliError> {
    let mut selected = None;
    for argument in &mut args {
        if argument == "--" { break; }
        if let Some(path) = argument.strip_prefix("--workspace=") {
            let canonical = checked_workspace(Path::new(path), cwd)?;
            *argument = format!("--workspace={}", canonical.display());
            selected = Some(canonical);
        }
    }
    if command == "restart" {
        let index = args.iter().position(|arg| arg == "--")
            .and_then(|index| (index + 1 < args.len()).then_some(index + 1))
            .or_else(|| args.iter().position(|arg| !arg.starts_with('-')));
        if let Some(index) = index {
            if !explicit_path(&args[index]) {
                return Err(CliError::Usage("bare restart targets are ambiguous; use restart --workspace PATH or restart ./PATH for a team".to_string()));
            }
            let canonical = checked_workspace(Path::new(&args[index]), cwd)?;
            if selected.as_ref().is_some_and(|selected| selected != &canonical) {
                return Err(CliError::Usage("restart WORKSPACE conflicts with --workspace".to_string()));
            }
            args[index] = canonical.to_string_lossy().into_owned();
        }
    }
    Ok(args)
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
    if has_arg(args, "--fresh") {
        return Err(CliError::Usage(
            "quick-start no longer accepts --fresh. Reset semantics moved to \
             `team-agent restart --allow-fresh`, which requires explicit user \
             confirmation."
                .to_string(),
        ));
    }
    let parsed = parse_args(args);
    let explicit_workspace = parsed
        .workspace
        .as_deref()
        .map(|path| resolve_cli_path(cwd, path));
    let positional_agents_dir = parsed.positionals.first().map(PathBuf::from).map(|path| {
        if path.is_absolute() {
            path
        } else if let Some(workspace) = explicit_workspace.as_ref() {
            workspace.join(path)
        } else {
            cwd.join(path)
        }
    });
    let workspace = explicit_workspace
        .or_else(|| {
            let path = positional_agents_dir.as_ref()?;
            if let Some(run_workspace) = path
                .ancestors()
                .find(|ancestor| {
                    ancestor.file_name().and_then(|name| name.to_str()) == Some(".team")
                })
                .and_then(Path::parent)
            {
                return Some(run_workspace.to_path_buf());
            }
            path.join("TEAM.md").is_file().then(|| path.to_path_buf())
        })
        .unwrap_or_else(|| cwd.to_path_buf());
    // A positional team directory containing TEAM.md is a complete standalone
    // workspace. Use it as the runtime workspace unless --workspace explicitly
    // selects another root; this prevents host owner context/state leaking into
    // an independent quick-start target.
    let agents_dir = positional_agents_dir.unwrap_or_else(|| workspace.clone());
    // 0.5.x Phase 1d Batch 2: validate the `--backend` literal up-front
    // so users get a fast, actionable error instead of a downstream
    // factory refusal. Accept the same literals as the factory
    // `RequestedTransportBackend::parse_literal`.
    if let Some(literal) = parsed.backend.as_deref() {
        let normalized = literal.trim().to_ascii_lowercase();
        if normalized != "tmux" && normalized != "conpty" {
            return Err(CliError::Usage(format!(
                "--backend must be tmux or conpty; received {literal:?}. Use tmux on POSIX. ConPTY requires a Windows host and an installed shim."
            )));
        }
    }
    Ok(QuickStartArgs {
        workspace,
        agents_dir,
        name: parsed.name,
        team_id: parsed.team_id.or(parsed.team),
        yes: parsed.yes,
        json: parsed.json,
        detail: parsed.detail,
        backend: parsed.backend,
    })
}

fn models_args(args: &[String]) -> Result<ModelsArgs, CliError> {
    let parsed = parse_args(args);
    let provider = parsed.provider.unwrap_or_else(|| "pi".to_string());
    if parsed.search.is_some() && !parsed.positionals.is_empty() {
        return Err(CliError::Usage(
            "models accepts either QUERY or --search TEXT, not both".to_string(),
        ));
    }
    let search = parsed
        .search
        .or_else(|| (!parsed.positionals.is_empty()).then(|| parsed.positionals.join(" ")))
        .filter(|query| !query.trim().is_empty());
    Ok(ModelsArgs {
        provider,
        search,
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
    validate_send_flags(args)?;
    warn_send_legacy_delivery_flags(args);
    let parsed = parse_args(args);
    if parsed.targets.is_none()
        && parsed.pane.is_none()
        && parsed.to_name.is_none()
        && parsed.to_leader.is_none()
        && parsed.positionals.len() < 2
    {
        return Err(CliError::Usage(
            "Provide a recipient agent name and task message, such as team-agent send worker 'Review the changes'".to_string(),
        ));
    }
    let target = if parsed.targets.is_some()
        || parsed.pane.is_some()
        || parsed.to_name.is_some()
        || parsed.to_leader.is_some()
    {
        None
    } else {
        parsed.positionals.first().cloned()
    };
    let message_start = usize::from(target.is_some());
    let workspace = workspace(&parsed, cwd);
    let sender = trusted_cli_sender(&workspace, parsed.team.as_deref())?;
    let presentation_value = if parsed.presentation_sink.is_some()
        || parsed.message_class.is_some()
        || parsed.case_id.is_some()
    {
        Some(serde_json::json!({
            "sink": parsed.presentation_sink.clone(),
            "class": parsed.message_class.clone(),
            "case_id": parsed.case_id.clone(),
        }))
    } else {
        None
    };
    let mailbox_value = parsed.mailbox.then(|| serde_json::json!(true));
    let normalized = crate::messaging::presentation::normalize_send_presentation(
        mailbox_value.as_ref(),
        presentation_value.as_ref(),
    )
    .map_err(|error| CliError::Usage(format!("invalid send routing: {error}")))?;
    if normalized.deprecation.is_some() {
        eprintln!("Warning: legacy send settings are deprecated. Use team-agent send <agent> 'task message'; add --mailbox for storage without injection.");
    }
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
        sender,
        no_ack: false,
        no_wait: true,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: parsed.json,
        message_id: parsed.message_id,
        presentation: normalized.request,
        pane: parsed.pane.clone(),
        to_name: parsed.to_name.clone(),
        to_leader: parsed.to_leader.clone(),
    })
}

fn trusted_cli_sender(workspace: &Path, team: Option<&str>) -> Result<TrustedSender, CliError> {
    let agent_id = std::env::var("TEAM_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let worker_context = ["TEAM_AGENT_OWNER_TEAM_ID", "TEAM_AGENT_AGENT_ID"]
        .into_iter()
        .any(|key| {
            std::env::var(key)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        });
    let Some(agent_id) = agent_id else {
        if worker_context {
            return Err(CliError::Usage(
                "send requires framework-injected TEAM_AGENT_ID; refusing leader identity fallback"
                    .to_string(),
            ));
        }
        return Ok(TrustedSender::leader());
    };
    let source_workspace = std::env::var("TEAM_AGENT_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| workspace.display().to_string());
    let source_team = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
        .ok()
        .or_else(|| std::env::var("TEAM_AGENT_TEAM_ID").ok())
        .or_else(|| team.map(ToOwned::to_owned))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    Ok(TrustedSender::from_runtime_identity_with_source(
        crate::model::ids::AgentId::new(agent_id),
        source_workspace,
        &source_team,
    ))
}

fn warn_send_legacy_delivery_flags(args: &[String]) {
    const FLAGS: &[&str] = &[
        "--watch-result",
        "--requires-ack",
        "--no-ack",
        "--no-wait",
        "--timeout",
        "--confirm-human",
    ];
    let spec = command_spec("send");
    let sunset = spec
        .and_then(|spec| spec.sunset)
        .unwrap_or("a future compatibility release");
    let action = spec.and_then(|spec| spec.action).unwrap_or(
        "Use team-agent send <agent> 'task message'; select scope with --workspace/--team",
    );
    for flag in FLAGS {
        if args
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
        {
            eprintln!(
                "Warning: {flag} is deprecated and will be removed in {sunset}. Next: {action}"
            );
        }
    }
}

fn validate_send_flags(args: &[String]) -> Result<(), CliError> {
    super::arguments::parse("send", args).map(|_| ())
}

/// Stage 4 of identity-boundary unified plan (architect direction
/// 2026-06-24, .team/artifacts/identity-boundary-unified-plan.md §2 Stage
/// 4): destructive command ambiguity gate. When the workspace has 2+
/// alive teams and the caller did not pass `--team`, refuse with a
/// usage error listing the candidates so the operator picks explicitly.
/// Single-team workspaces (the 0.4.x baseline) are unaffected — the
/// `CommandScope::resolve` helper returns `Resolved` and this function
/// is a no-op.
fn refuse_if_multi_alive_team_missing_scope(
    command: &str,
    workspace: &Path,
    requested_team: Option<&str>,
) -> Result<(), CliError> {
    let scope = crate::state::paths::CommandScope::resolve(workspace, requested_team);
    if scope.is_ambiguous() {
        let candidates = scope.candidates().join(", ");
        return Err(CliError::Usage(format!(
            "{command}: workspace has multiple alive teams ({candidates}); \
             pass `--team <key>` to choose one (refusing to default to any \
             single team — Stage 4 identity-boundary contract)"
        )));
    }
    Ok(())
}

fn status_args(args: &[String], cwd: &Path) -> StatusArgs {
    let parsed = parse_args(args);
    StatusArgs {
        agent: parsed.positionals.first().cloned(),
        workspace: workspace(&parsed, cwd),
        detail: parsed.detail,
        summary: parsed.summary,
        json: parsed.json,
        team: parsed.team,
    }
}

fn wait_args(args: &[String], cwd: &Path) -> Result<WaitArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    Ok(WaitArgs {
        task_id: parsed
            .task
            .ok_or_else(|| CliError::Usage("wait requires --task <id>".to_string()))?,
        workspace,
        json: parsed.json,
    })
}

fn approvals_args(args: &[String], cwd: &Path) -> ApprovalsArgs {
    let parsed = parse_args(args);
    ApprovalsArgs {
        agent: parsed.positionals.first().cloned(),
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
        team: parsed.team,
    }
}

fn inbox_args(args: &[String], cwd: &Path) -> Result<InboxArgs, CliError> {
    if args
        .iter()
        .any(|arg| arg == "--since" || arg.starts_with("--since="))
    {
        return Err(CliError::Usage(
            "inbox only supports -n/--limit; --since is not supported".to_string(),
        ));
    }
    let parsed = parse_args(args);
    Ok(InboxArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        limit: parsed.limit.unwrap_or(3),
        json: parsed.json,
        team: parsed.team,
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
        detail: parsed.detail,
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
        provider: parse_cli_provider(parsed.provider.as_deref()
            .filter(|provider| !provider.trim().is_empty())
            .ok_or_else(|| CliError::Usage("attach-leader requires --provider <name>".to_string()))?)?,
        confirm: parsed.confirm,
        json: parsed.json,
    })
}

fn attach_app_server_leader_args(
    args: &[String],
    cwd: &Path,
) -> Result<AttachAppServerLeaderArgs, CliError> {
    let parsed = parse_args(args);
    Ok(AttachAppServerLeaderArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        socket: parsed
            .socket
            .filter(|socket| !socket.is_empty())
            .ok_or_else(|| CliError::Usage("missing --socket".to_string()))?,
        thread_id: parsed
            .thread_id
            .filter(|thread_id| !thread_id.is_empty())
            .ok_or_else(|| CliError::Usage("missing --thread-id".to_string()))?,
        json: parsed.json,
    })
}

fn shutdown_args(args: &[String], cwd: &Path) -> Result<ShutdownArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("shutdown", &workspace, parsed.team.as_deref())?;
    Ok(ShutdownArgs {
        workspace,
        team: parsed.team,
        keep_logs: parsed.keep_logs,
        json: parsed.json,
    })
}

fn restart_args(args: &[String], cwd: &Path) -> Result<RestartArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = if let Some(path) = parsed.positionals.first() {
        if !explicit_path(path) {
            return Err(CliError::Usage("bare restart targets are ambiguous; use restart --workspace PATH or restart ./PATH for a team".to_string()));
        }
        resolve_cli_path(cwd, Path::new(path))
    } else { workspace(&parsed, cwd) };
    refuse_if_multi_alive_team_missing_scope("restart", &workspace, parsed.team.as_deref())?;
    Ok(RestartArgs {
        workspace,
        team: parsed.team,
        allow_fresh: parsed.allow_fresh,
        session_converge_deadline_ms: parsed.session_converge_deadline_ms,
        json: parsed.json,
        detail: parsed.detail,
    })
}

// Keep strict role CLI parsing separate from the legacy shared parser.
pub(super) fn intent_source_workspace(source: &Path, cwd: &Path) -> Result<PathBuf, CliError> {
    quick_start_args(&[source.to_string_lossy().into_owned()], cwd).map(|args| args.workspace)
}

pub(super) fn intent_role_patch(args: &[String]) -> Result<crate::lifecycle::role_config::RoleConfigPatch, CliError> {
    role_agent_args(args, false).map(|(_, patch)| patch)
}

/// Explicit object aliases reuse the same syntax checks, typed builders and handlers.
pub(super) fn intent_alias(command: &str, args: &[String], cwd: &Path) -> Result<CmdResult, CliError> {
    let mut checked = prepare_control_args(command, args, cwd)?;
    if checked.help { return Ok(CmdResult::human(command_help(Some(command)))); }
    for token in &mut checked.tokens {
        if let Some(path) = token.strip_prefix("--role-file=") {
            let path = Path::new(path);
            let path = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
            let path = path.canonicalize().map_err(|e| CliError::Usage(format!("invalid explicit role file: {e}")))?;
            *token = format!("--role-file={}", path.display());
        }
    }
    let args = checked.tokens.as_slice();
    match command {
        "status" => {
            let parsed = status_args(args, cwd);
            cmd_status_for_team(&parsed, parsed.team.as_deref())
        }
        "add-agent" => cmd_add_agent(&add_agent_args(args, cwd)?),
        "remove-agent" => cmd_remove_agent(&remove_agent_args(args, cwd)?),
        "reset-agent" => cmd_reset_agent(&reset_agent_args(args, cwd)?),
        "clone-agent" => cmd_clone_agent(&clone_agent_args(args, cwd)?),
        "fork-agent" => cmd_fork_agent(&fork_agent_args(args, cwd)?),
        _ => Err(CliError::Usage("unsupported lifecycle alias".into())),
    }
}

fn role_agent_args(
    args: &[String],
    add: bool,
) -> Result<(ParsedArgs, crate::lifecycle::role_config::RoleConfigPatch), CliError> {
    let mut role = crate::lifecycle::role_config::RoleConfigPatch::default();
    let mut forwarded = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if !argument.starts_with('-') {
            forwarded.push(argument.clone());
            index += 1;
            continue;
        }
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(key, value)| (key, Some(value)));
        if !seen.insert(flag.to_string()) {
            return Err(CliError::Usage(format!("duplicate {flag}")));
        }
        if matches!(flag, "--json" | "--allow-fresh" | "--force") {
            if inline.is_some() || (add && flag == "--allow-fresh") {
                return Err(CliError::Usage(format!("unsupported option: {argument}")));
            }
            forwarded.push(flag.to_string());
            index += 1;
            continue;
        }
        if !matches!(
            flag,
            "--provider"
                | "--model"
                | "--effort"
                | "--bypass"
                | "--prompt"
                | "--profile"
                | "--workspace"
                | "--team"
                | "--role-file"
        ) || (!add && flag == "--role-file")
        {
            return Err(CliError::Usage(format!("unknown option: {flag}")));
        }
        let value = if let Some(value) = inline {
            value.to_string()
        } else {
            index += 1;
            args.get(index)
                .filter(|value| flag == "--prompt" || !value.starts_with("--"))
                .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))?
                .clone()
        };
        if value.trim().is_empty() {
            return Err(CliError::Usage(format!("empty value for {flag}")));
        }
        match flag {
            "--provider" => {
                // A contract name is written as the leaf's public name (`agent` →
                // `cursor`, `codex_cli` → `codex`); it declares the same leaf.
                #[cfg(unix)]
                let contract = crate::contract_runtime::registry::declared(&value)
                    .map(crate::contract_runtime::registry::public_name);
                #[cfg(not(unix))]
                let contract: Option<&str> = None;
                let provider = match contract {
                    Some(id) => id,
                    None => crate::provider::wire::provider_wire(
                        crate::provider::wire::parse_provider(&value)
                            .ok_or_else(|| CliError::Usage(format!("unknown provider: {value}")))?,
                    ),
                };
                role.provider = Some(provider.to_string());
            }
            "--bypass" => {
                role.bypass = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(CliError::Usage("--bypass requires true or false".into())),
                })
            }
            "--model" => role.model = Some(value),
            "--effort" => {
                if crate::model::enums::ProviderEffort::parse(&value).is_none() {
                    return Err(CliError::Usage(format!("unknown effort: {value}")));
                }
                role.effort = Some(value);
            }
            "--prompt" => role.prompt = Some(value),
            "--profile" => role.profile = Some(value),
            _ => {
                forwarded.push(flag.to_string());
                forwarded.push(value);
            }
        }
        index += 1;
    }
    let parsed = parse_args(&forwarded);
    if add && parsed.role_file.is_none() {
        if role.provider.is_none() {
            return Err(CliError::Usage(
                "add-agent requires --provider <name>".into(),
            ));
        }
    }
    if parsed.positionals.len() != 1 {
        return Err(CliError::Usage(
            if parsed.positionals.is_empty() {
                "missing agent"
            } else {
                "expected exactly one agent id"
            }
            .into(),
        ));
    }
    Ok((parsed, role))
}

#[cfg(test)]
mod role_cli_tests {
    use super::*;
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn add_requires_provider_without_a_template_but_accepts_file_input() {
        for args in [
            vec!["w"],
            vec!["w", "--bypass", "false"],
        ] {
            assert!(role_agent_args(&strings(&args), true).is_err());
        }
        assert!(role_agent_args(&strings(&["w", "--role-file", "w.md"]), true).is_ok());
        let (parsed, _) =
            role_agent_args(&strings(&["w", "--role-file", "w.md", "--force"]), true).unwrap();
        assert!(parsed.force);
        assert!(role_agent_args(
            &strings(&["w", "--role-file", "w.md", "--force=true"]),
            true
        )
        .is_err());
        assert!(role_agent_args(
            &strings(&["w", "--role-file", "w.md", "--allow-fresh"]),
            true
        )
        .is_err());
        let (_, patch) =
            role_agent_args(&strings(&["w", "--provider=pi", "--bypass=false"]), true).unwrap();
        assert_eq!(patch.provider.as_deref(), Some("pi"));
        assert_eq!(patch.bypass, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn provider_accepts_contract_names_as_their_leaf_id() {
        for (name, leaf) in [
            ("cursor", "cursor"), ("cursor-agent", "cursor"), ("agent", "cursor"),
            ("cursor_agent", "cursor"), ("kiro", "kiro"),
            // Codex and Grok Build keep their public command names, which
            // declare the leaf.
            ("codex", "codex"), ("codex_cli", "codex"), ("codex-cli", "codex"),
            ("codex_tui", "codex"), ("grok", "grok"), ("grok_build", "grok"),
        ] {
            let (_, patch) = role_agent_args(
                &strings(&["w", "--provider", name, "--bypass", "true"]), true,
            ).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(patch.provider.as_deref(), Some(leaf), "{name}");
        }
        assert!(role_agent_args(&strings(&["w", "--provider", "Cursor", "--bypass", "true"]), true).is_err());
    }

    #[test]
    fn add_accepts_bypass_true_false_or_omission_for_every_provider() {
        for &provider in crate::provider::wire::all_providers() {
            let provider = crate::provider::wire::provider_wire(provider);
            let (_, patch) = role_agent_args(&strings(&["w", "--provider", provider]), true).unwrap();
            assert_eq!(patch.bypass, None);
            for bypass in ["true", "false"] {
                let (_, patch) = role_agent_args(
                    &strings(&["w", "--provider", provider, "--bypass", bypass]),
                    true,
                )
                .unwrap();
                assert_eq!(patch.bypass, Some(bypass == "true"));
            }
        }
    }

    #[test]
    fn strict_role_options_reject_ambiguous_values() {
        for args in [
            vec!["w", "--bypass"],
            vec!["w", "--bypass", "yes"],
            vec!["w", "--no-bypass"],
            vec!["w", "--bypass", "true", "--bypass=false"],
            vec!["w", "--unknown", "x"],
            vec!["w", "extra"],
        ] {
            assert!(role_agent_args(&strings(&args), false).is_err());
        }
        let (_, patch) = role_agent_args(&strings(&["w"]), false).unwrap();
        assert_eq!(
            patch,
            crate::lifecycle::role_config::RoleConfigPatch::default()
        );
    }
}

fn start_agent_args(args: &[String], cwd: &Path) -> Result<StartAgentArgs, CliError> {
    let (parsed, role_config) = role_agent_args(args, false)?;
    if parsed.force {
        return Err(CliError::Usage("start-agent does not support --force; use stop-agent AGENT followed by start-agent AGENT for a safe restart".to_string()));
    }
    Ok(StartAgentArgs {
        role_config,
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        force: parsed.force,
        allow_fresh: parsed.allow_fresh,
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
    let agent = required_pos(&parsed, 0, "agent")?;
    if !parsed.discard_session {
        return Err(CliError::Usage(
            "missing --discard-session; add this option only after explicit authorization to discard the saved session association".to_string(),
        ));
    }
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("reset-agent", &workspace, parsed.team.as_deref())?;
    Ok(ResetAgentArgs {
        agent,
        workspace,
        team: parsed.team,
        discard_session: parsed.discard_session,
        json: parsed.json,
    })
}

fn add_agent_args(args: &[String], cwd: &Path) -> Result<AddAgentArgs, CliError> {
    let (parsed, role_config) = role_agent_args(args, true)?;
    Ok(AddAgentArgs {
        role_config,
        agent: required_pos(&parsed, 0, "agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        role_file: parsed.role_file.unwrap_or_default(),
        force: parsed.force,
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
        json: parsed.json,
    })
}

fn clone_agent_args(args: &[String], cwd: &Path) -> Result<CloneAgentArgs, CliError> {
    let parsed = parse_args(args);
    Ok(CloneAgentArgs {
        source_agent: required_pos(&parsed, 0, "source_agent")?,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        as_agent: parsed
            .as_agent
            .ok_or_else(|| CliError::Usage("missing --as".to_string()))?,
        label: parsed.label,
        json: parsed.json,
    })
}

fn remove_agent_args(args: &[String], cwd: &Path) -> Result<RemoveAgentArgs, CliError> {
    let parsed = parse_args(args);
    let agent = required_pos(&parsed, 0, "agent")?;
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("remove-agent", &workspace, parsed.team.as_deref())?;
    Ok(RemoveAgentArgs {
        agent,
        workspace,
        team: parsed.team,
        from_spec: parsed.from_spec,
        confirm: parsed.confirm,
        force: parsed.force,
        json: parsed.json,
    })
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
        Some(other) => Some(DoctorGate::Unknown(other.to_string())),
        None => None,
    }
}

fn leaders_args(args: &[String], _cwd: &Path) -> Result<LeadersArgs, CliError> {
    let mut view = None;
    let mut query = None;
    let mut prune = false;
    let mut dry_run = false;
    let mut json = false;
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--json" => json = true,
            "--all" => {
                if view.replace(LeadersView::All).is_some() {
                    return Err(CliError::Usage(
                        "leaders accepts only one of --all or --stale".to_string(),
                    ));
                }
            }
            "--stale" => {
                if view.replace(LeadersView::Stale).is_some() {
                    return Err(CliError::Usage(
                        "leaders accepts only one of --all or --stale".to_string(),
                    ));
                }
            }
            "--prune" => {
                if prune {
                    return Err(CliError::Usage(
                        "leaders accepts --prune at most once".to_string(),
                    ));
                }
                prune = true;
            }
            "--dry-run" => dry_run = true,
            "--search" => {
                let Some(value) = args.get(i.saturating_add(1)) else {
                    return Err(CliError::Usage(
                        "leaders --search requires TEXT".to_string(),
                    ));
                };
                if value.starts_with('-') {
                    return Err(CliError::Usage(
                        "leaders --search requires TEXT".to_string(),
                    ));
                }
                i = i.saturating_add(1);
                if query.replace(value.clone()).is_some() {
                    return Err(CliError::Usage(
                        "leaders accepts only one QUERY or --search TEXT".to_string(),
                    ));
                }
            }
            value if value.starts_with("--search=") => {
                let value = value.trim_start_matches("--search=").to_string();
                if query.replace(value).is_some() {
                    return Err(CliError::Usage(
                        "leaders accepts only one QUERY or --search TEXT".to_string(),
                    ));
                }
            }
            value if value.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown leaders argument: {value}"
                )))
            }
            value => {
                if query.replace(value.to_string()).is_some() {
                    return Err(CliError::Usage(
                        "leaders accepts only one QUERY or --search TEXT".to_string(),
                    ));
                }
            }
        }
        i = i.saturating_add(1);
    }
    if query
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(CliError::Usage("leaders QUERY cannot be blank".to_string()));
    }
    if prune && view.is_some() {
        return Err(CliError::Usage(
            "leaders --prune cannot be combined with --all or --stale".to_string(),
        ));
    }
    if prune && query.is_some() {
        return Err(CliError::Usage(
            "leaders --prune cannot be combined with QUERY or --search".to_string(),
        ));
    }
    if dry_run && !prune {
        return Err(CliError::Usage(
            "leaders --dry-run requires --prune".to_string(),
        ));
    }
    Ok(LeadersArgs {
        view: view.unwrap_or(LeadersView::Live),
        query,
        prune,
        dry_run,
        json,
    })
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
        proxy_mode: parsed.proxy_mode,
        json: parsed.json,
    })
}

fn run_coordinator(args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    let parsed = parse_args(args);
    let workspace = crate::coordinator::WorkspacePath::new(workspace(&parsed, cwd));
    // 0.5.x Windows portability Batch 9 F8: pass `--team` through
    // to the daemon so it doesn't have to derive from state at
    // boot time. `parse_args` already recognizes `--team`; we just
    // thread the value into `DaemonArgs::team_key`.
    crate::coordinator::run_daemon(crate::coordinator::DaemonArgs {
        workspace,
        once: parsed.once,
        tick_interval_sec: parsed.tick_interval,
        team_key: parsed.team.clone(),
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

    fn visible_help_commands(help: &str) -> Vec<String> {
        help.lines()
            .filter_map(|line| {
                let trimmed = line.strip_prefix("  ")?;
                if trimmed.starts_with("team-agent ") || trimmed.starts_with(char::is_whitespace) {
                    return None;
                }
                let command = trimmed.split_whitespace().next()?;
                (command.chars().next()?.is_ascii_lowercase()
                    && command
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'))
                .then(|| command.to_string())
            })
            .collect()
    }

    #[test]
    fn leaders_parser_supports_views_and_search() {
        let parsed = leaders_args(
            &cli_argv(&["--all", "--search", "Wiki", "--json"]),
            Path::new("."),
        )
        .unwrap();
        assert_eq!(parsed.view, LeadersView::All);
        assert_eq!(parsed.query.as_deref(), Some("Wiki"));
        assert!(!parsed.prune);
        assert!(!parsed.dry_run);
        assert!(parsed.json);
    }

    #[test]
    fn leaders_parser_rejects_invalid_search_and_prune_combinations() {
        for args in [
            cli_argv(&["one", "two"]),
            cli_argv(&["one", "--search", "two"]),
            cli_argv(&["--search", "   "]),
            cli_argv(&["--dry-run"]),
            cli_argv(&["--prune", "--stale"]),
            cli_argv(&["--prune", "--search", "x"]),
        ] {
            assert!(
                leaders_args(&args, Path::new(".")).is_err(),
                "args={args:?}"
            );
        }
    }

    #[test]
    fn command_specs_have_unique_names() {
        let mut names = std::collections::BTreeSet::new();
        for spec in COMMAND_SPECS {
            assert!(
                names.insert(spec.name),
                "duplicate command spec `{}`",
                spec.name
            );
        }
    }

    #[test]
    fn all_dispatch_kinds_have_exactly_one_spec() {
        for kind in ALL_DISPATCH_KINDS {
            let count = COMMAND_SPECS
                .iter()
                .filter(|spec| spec.kind == CommandKind::Dispatch(*kind))
                .count();
            assert_eq!(
                count, 1,
                "dispatch kind `{kind:?}` must appear in exactly one CommandSpec"
            );
        }
    }

    #[test]
    fn known_command_gate_uses_specs() {
        for spec in COMMAND_SPECS.iter().filter(|spec| spec.command_help) {
            assert!(
                is_known_subcommand(spec.name),
                "`{}` must be accepted by the known-command help gate",
                spec.name
            );
        }
        assert!(!is_known_subcommand("missing-c1-command"));
    }

    #[test]
    fn default_help_lists_only_default_help_specs() {
        let top_help = command_help(None);
        let visible = visible_help_commands(&top_help);
        for command in &visible {
            let spec = command_spec(command).expect("visible command must have a spec");
            assert!(
                spec.default_help,
                "`{command}` appears in default help without default_help=true"
            );
        }
        let expected: Vec<&str> = COMMAND_SPECS
            .iter()
            .filter(|spec| spec.default_help)
            .map(|spec| spec.name)
            .collect();
        assert_eq!(
            expected.len(),
            36,
            "the public catalog includes endpoint discovery and scheduled leader reminders"
        );
        for spec_name in &expected {
            assert!(
                visible.iter().any(|command| command == spec_name),
                "`{spec_name}` has default_help=true but is missing from top-level help"
            );
        }
        let mut actual = visible.clone();
        actual.sort();
        let mut expected_sorted: Vec<String> =
            expected.iter().map(|name| (*name).to_string()).collect();
        expected_sorted.sort();
        assert_eq!(
            actual, expected_sorted,
            "default help must match the exact public spec set, not a slack threshold; got {visible:?}"
        );
        assert!(
            top_help.contains("copilot"),
            "top-level leader passthrough help must list copilot"
        );
    }

    #[test]
    fn hidden_commands_not_in_default_help() {
        let top_help = command_help(None);
        let visible = visible_help_commands(&top_help);
        assert!(visible.iter().any(|command| command == "doctor"));
        assert!(visible.iter().any(|command| command == "leaders"));
        for command in [
            "results",
            "wait",
            "attach-app-server-leader",
            "identity",
            "watch",
            "sessions",
            "validate",
            "preflight",
            "wait-ready",
            "e2e",
            "peek",
            "coordinator",
        ] {
            assert!(
                !visible.iter().any(|visible| visible == command),
                "`{command}` must stay hidden from default help"
            );
        }
    }

    #[test]
    fn retired_commands_have_no_catalog_machine_or_help_gate() {
        for command in [
            "e2e",
            "allow-peer-talk",
            "results",
            "validate",
            "identity",
            "sessions",
            "watch",
            "peek",
            "wait-ready",
            "preflight",
        ] {
            assert!(
                command_spec(command).is_none(),
                "retired catalog entry: {command}"
            );
            assert!(
                !is_machine_command(command),
                "retired Machine route: {command}"
            );
            assert!(
                !is_known_subcommand(command),
                "retired help gate: {command}"
            );
        }
    }

    #[test]
    fn leaders_help_publishes_selectors_and_prune() {
        let help = command_help(Some("leaders"));
        assert!(help.contains("--all"));
        assert!(help.contains("--stale"));
        assert!(help.contains("--search TEXT"));
        assert!(help.contains("--prune"));
        assert!(help.contains("--dry-run"));
    }

    #[test]
    fn observation_a_commands_have_terminal_tiers() {
        for (command, tier) in [
            ("approvals", CommandTier::Core),
            ("profile", CommandTier::Core),
            ("install-skill", CommandTier::Core),
        ] {
            assert_eq!(command_spec(command).map(|spec| spec.tier), Some(tier));
        }
    }

    #[test]
    fn suggestion_index_excludes_hidden_commands() {
        assert_eq!(nearest_subcommand("statu"), Some("status"));
        assert_eq!(nearest_subcommand("leader"), Some("leaders"));
        assert_eq!(nearest_subcommand("fallback-send-leade"), None);
        assert_eq!(nearest_subcommand("coordinato"), None);
    }

    #[test]
    fn copilot_is_listed_as_leader_passthrough_candidate() {
        assert!(command_help(None).contains("copilot"));
        assert_eq!(nearest_subcommand("copliot"), Some("copilot"));
    }

    #[test]
    fn copilot_help_dispatches_as_leader_passthrough() {
        let cwd = tmp_workspace();
        assert_eq!(run(&cli_argv(&["copilot", "--help"]), &cwd), ExitCode::Ok);
    }

    #[test]
    fn t0_help_catalog_lists_command_flags() {
        for (command, flags) in [
            (
                "quick-start",
                &["--workspace", "--team-id", "--yes", "--json"][..],
            ),
            ("send", &["--workspace", "--team", "--json"][..]),
            ("schedule", &["--in", "--every", "--text", "--workspace", "--team", "--json"][..]),
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
                    "--model",
                    "--effort",
                    "--bypass",
                    "--prompt",
                    "--profile",
                    "--provider",
                    "--workspace",
                    "--team",
                    "--allow-fresh",
                    "--json",
                ][..],
            ),
            (
                "reset-agent",
                &["--workspace", "--team", "--discard-session", "--json"][..],
            ),
            (
                "add-agent",
                &[
                    "--role-file",
                    "--provider",
                    "--bypass",
                    "--model",
                    "--effort",
                    "--prompt",
                    "--profile",
                    "--force",
                    "--workspace",
                    "--team",
                    "--json",
                ][..],
            ),
            (
                "fork-agent",
                &["--as", "--label", "--workspace", "--team", "--json"][..],
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
            ("stop-agent", &["--workspace", "--team", "--json"][..]),
            ("approvals", &["--workspace", "--team", "--json"][..]),
            (
                "inbox",
                &["--workspace", "--team", "-n", "--limit", "--json"][..],
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
        let cwd = tmp_workspace();
        for command in ["wait", "attach-app-server-leader", "coordinator"] {
            assert_eq!(
                run(&cli_argv(&[command, "--help"]), &cwd),
                ExitCode::Usage,
                "Machine help must be generic Usage2, not a private flag tutorial"
            );
        }
        std::fs::remove_dir_all(cwd).unwrap();
        assert!(
            !command_help(Some("quick-start")).contains("--fresh"),
            "quick-start help must not advertise removed reset semantics"
        );
    }

    #[test]
    fn public_help_templates_and_cli_guidance_are_english() {
        fn assert_english(text: &str) {
            assert!(
                !text.chars().any(|ch| matches!(ch, '\u{3400}'..='\u{9fff}')),
                "unauthorized CJK CLI guidance: {text}"
            );
        }
        assert_english(&default_help());
        assert_eq!(TEAM_TEMPLATE.lines().nth(1), Some("name: current"));
        for spec in COMMAND_SPECS {
            assert_english(spec.summary);
            assert_english(&command_help(Some(spec.name)));
        }
        for text in [
            TEAM_TEMPLATE,
            WORKER_TEMPLATE,
            HUMAN_NAVIGATION,
            crate::cli::route::HELP,
            crate::cli::COMMS_BOUNDARY_TEXT,
            crate::cli::QUICK_START_REMINDER,
            crate::cli::SEND_REMINDER,
            crate::cli::STATUS_REMINDER,
        ] {
            assert_english(text);
        }
        for command in ["quick-start", "restart", "send"] {
            let payload = CliError::Runtime("fixture failure".to_string())
                .to_payload(Path::new("/tmp/fixture.log"), command);
            assert_english(&payload.action);
        }
    }

    #[test]
    fn worker_lifecycle_help_uses_authoritative_spec_usage_and_summary() {
        for name in ["start-agent", "add-agent"] {
            let spec = command_spec(name).expect("registered worker lifecycle command");
            let help = command_help(Some(name));
            assert!(help.contains(spec.usage));
            assert!(help.contains("Purpose:"));
            for flag in [
                "--model MODEL",
                "--effort LEVEL",
                "--bypass true|false",
                "--prompt TEXT",
                "--profile NAME",
                "--provider TOOL",
            ] {
                assert!(help.contains(flag), "{name} help missing {flag}: {help}");
            }
            assert!(help.contains("Next Action") && help.contains("send"));
        }
        assert!(!command_help(Some("start-agent")).contains("--force"));
        let add = command_help(Some("add-agent"));
        assert!(add.contains("[--role-file FILE]"));
        assert!(add.contains("provider") && add.contains("bypass") && add.contains("--role-file"));
        assert!(add.contains("Conflicting") && add.contains("refused"));
    }

    #[test]
    fn status_help_describes_brief_projection_and_unknown_boundary() {
        let help = command_help(Some("status"));
        for marker in [
            "name/provider/model/effort/runtime_status/activity/health/session_name/tmux_command",
            "Use --json for structured output",
            "unknown means insufficient evidence",
            "tmux_command",
            "do not add fields",
        ] {
            assert!(
                help.contains(marker),
                "status help missing {marker}: {help}"
            );
        }
        assert!(!help.contains("error breakdown via status --summary"));
    }

    #[test]
    fn models_defaults_provider_and_accepts_one_query_form() {
        let defaulted = models_args(&cli_argv(&["gpt", "5.6", "luna"])).unwrap();
        assert_eq!(defaulted.provider, "pi");
        assert_eq!(defaulted.search.as_deref(), Some("gpt 5.6 luna"));

        let cursor = models_args(&cli_argv(&[
            "--provider",
            "cursor",
            "--search",
            "GPT Luna",
        ]))
        .unwrap();
        assert_eq!(cursor.provider, "cursor");
        assert_eq!(cursor.search.as_deref(), Some("GPT Luna"));

        let both = models_args(&cli_argv(&["luna", "--search", "gpt"])).unwrap_err();
        assert!(
            matches!(both, CliError::Usage(message) if message.contains("either QUERY or --search TEXT"))
        );
        let blank = models_args(&cli_argv(&["--search", "   "])).unwrap();
        assert_eq!(blank.search, None);
    }

    #[test]
    fn send_pane_positionals_are_message_not_target() {
        let cwd = tmp_workspace();
        let args = send_args(&cli_argv(&["--pane", "%1596", "hello"]), &cwd).unwrap();
        assert_eq!(args.pane.as_deref(), Some("%1596"));
        assert_eq!(args.target, None);
        assert_eq!(args.targets, None);
        assert_eq!(args.message, vec!["hello".to_string()]);

        let args = send_args(
            &cli_argv(&["--pane", "%1596", "multi", "word", "message"]),
            &cwd,
        )
        .unwrap();
        assert_eq!(args.target, None);
        assert_eq!(
            args.message,
            vec![
                "multi".to_string(),
                "word".to_string(),
                "message".to_string()
            ]
        );

        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn send_mailbox_and_legacy_progress_map_to_the_one_bit_request() {
        let cwd = tmp_workspace();
        let mailbox = send_args(&cli_argv(&["leader", "progress", "--mailbox"]), &cwd).unwrap();
        assert_eq!(
            mailbox.presentation.sink,
            crate::messaging::presentation::PresentationSink::Casefile
        );

        let args = send_args(
            &cli_argv(&[
                "leader",
                "progress",
                "--presentation-sink",
                "casefile",
                "--message-class",
                "progress",
                "--case-id",
                "case-9",
            ]),
            &cwd,
        )
        .unwrap();
        assert_eq!(
            args.presentation.sink,
            crate::messaging::presentation::PresentationSink::Casefile
        );
        assert_eq!(
            args.presentation.class,
            crate::messaging::presentation::PresentationClass::Message
        );
        assert_eq!(args.presentation.case_id, None);
    }

    #[test]
    fn send_presentation_flags_fail_closed_when_incomplete() {
        let cwd = tmp_workspace();
        let error = send_args(
            &cli_argv(&["leader", "progress", "--presentation-sink", "casefile"]),
            &cwd,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CliError::Usage(message) if message == "invalid send routing: missing_class"
        ));
    }

    #[test]
    fn send_to_name_positionals_are_message_not_target() {
        let cwd = tmp_workspace();
        let args = send_args(&cli_argv(&["--to-name", "team-a/qa", "hello"]), &cwd).unwrap();
        assert_eq!(args.to_name.as_deref(), Some("team-a/qa"));
        assert_eq!(args.target, None);
        assert_eq!(args.targets, None);
        assert_eq!(args.message, vec!["hello".to_string()]);

        let args = send_args(
            &cli_argv(&["--to-name=team-a/qa", "multi", "word", "message"]),
            &cwd,
        )
        .unwrap();
        assert_eq!(args.target, None);
        assert_eq!(
            args.message,
            vec![
                "multi".to_string(),
                "word".to_string(),
                "message".to_string()
            ]
        );

        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn send_pane_still_rejects_to_and_empty_message() {
        let cwd = tmp_workspace();
        let err = send_args(
            &cli_argv(&["--pane", "%1596", "--to", "worker", "hello"]),
            &cwd,
        ).unwrap_err();
        assert!(matches!(err, CliError::Usage(ref message) if message.contains("one recipient addressing mode")));
        // The public parser now refuses first; the typed handler must still
        // reject the same conflict if a non-CLI caller constructs it directly.
        let mut args = send_args(&cli_argv(&["--pane", "%1596", "hello"]), &cwd).unwrap();
        args.targets = Some("worker".to_string());
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message.contains("--pane") && message.contains("--to") && message.contains("cannot") && message.contains("combined")),
            "expected --pane/--to mutual-exclusion usage error, got {err:?}"
        );

        let args = send_args(&cli_argv(&["--pane", "%1596"]), &cwd).unwrap();
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message.contains("--pane") && message.contains("cannot send an empty message")),
            "expected empty-message usage error, got {err:?}"
        );

        let _ = std::fs::remove_dir_all(&cwd);
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

    #[test]
    fn ux_quick_start_positional_team_dir_is_standalone_workspace() {
        let cwd = tmp_workspace();
        let ws = tmp_workspace();
        std::fs::write(ws.join("TEAM.md"), "# team\n").unwrap();
        let args = quick_start_args(&cli_argv(&[&ws.to_string_lossy()]), &cwd).unwrap();
        assert_eq!(args.workspace, ws);
        assert_eq!(args.agents_dir, ws);
        let _ = std::fs::remove_dir_all(&cwd);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn ux_quick_start_team_dir_under_dot_team_uses_project_workspace() {
        let cwd = tmp_workspace();
        let team_dir = cwd.join(".team/current");
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::write(team_dir.join("TEAM.md"), "# team\n").unwrap();
        let args = quick_start_args(&cli_argv(&[".team/current"]), &cwd).unwrap();
        assert_eq!(args.workspace, cwd);
        assert_eq!(args.agents_dir, team_dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    // ── E8 (N38): 未知子命令 → 最近似建议(additive,不破坏 golden invalid-choice 行) ──
    #[test]
    fn e8_unknown_subcommand_suggests_nearest_known_command() {
        // 'statu' typo → status; 'add-agen' → add-agent.
        assert_eq!(nearest_subcommand("statu"), Some("status"));
        assert_eq!(nearest_subcommand("add-agen"), Some("add-agent"));
        // Presentation migration: the new canonical short verb is now a
        // prefix candidate in the unchanged shared similarity ranking.
        assert_eq!(nearest_subcommand("start-agnet"), Some("start"));
    }

    #[test]
    fn e8_unknown_subcommand_no_suggestion_when_far() {
        // 完全无关的串不应误配出任何建议。
        assert_eq!(nearest_subcommand("zzzzzzzzzz"), None);
        assert_eq!(nearest_subcommand("x"), None);
    }

    #[test]
    fn e8_levenshtein_basic() {
        // 0.5.45 naming-addressing: distance function moved to
        // `crate::model::name_similarity` (shared with --to-name typo
        // suggestions). Same math, single source.
        use crate::model::name_similarity::levenshtein;
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("status", "status"), 0);
        assert_eq!(levenshtein("statu", "status"), 1);
    }

    // ─── Stage 4: multi-team ambiguity refusal for destructive commands ───

    fn seed_two_alive_teams_in(ws: &std::path::Path) {
        crate::state::persist::save_runtime_state(
            ws,
            &serde_json::json!({
                "teams": {
                    "alpha": {"status": "alive"},
                    "beta": {"status": "alive"},
                },
            }),
        )
        .unwrap();
    }

    #[test]
    fn refuse_helper_passes_on_single_alive_team() {
        let ws = tmp_workspace();
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({"teams": {"alpha": {"status": "alive"}}}),
        )
        .unwrap();
        assert!(
            refuse_if_multi_alive_team_missing_scope("stuck-cancel", &ws, None).is_ok(),
            "single-alive-team workspace must not trigger the ambiguity refusal"
        );
    }

    #[test]
    fn shutdown_args_builder_refuses_on_multi_alive_team() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy()]);
        let err = shutdown_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "shutdown args builder must surface the refusal; got: {err}"
        );
    }

    #[test]
    fn restart_args_builder_refuses_on_multi_alive_team() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&[&ws.to_string_lossy()]);
        let err = restart_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "restart args builder must surface the refusal; got: {err}"
        );
    }

    #[test]
    fn reset_agent_complete_input_refuses_multi_alive_team_before_agent_validation() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&[
            "nonexistent-agent",
            "--discard-session",
            "--workspace",
            &ws.to_string_lossy(),
        ]);
        let err = reset_agent_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "reset-agent args builder must surface the refusal; got: {err}"
        );
    }

    #[test]
    fn remove_agent_complete_input_refuses_multi_alive_team_before_agent_validation() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["nonexistent-agent", "--workspace", &ws.to_string_lossy()]);
        let err = remove_agent_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "remove-agent args builder must surface the refusal; got: {err}"
        );
    }

    // ──────────── Stage QR: quick-start/restart separation ────────────
    // Design doc: .team/artifacts/quickstart-restart-separation-design.md

    #[test]
    fn quick_start_refuses_fresh_flag_with_restart_guidance() {
        // QR contract: `--fresh` is gone from quick-start. The flag is
        // not advertised or carried in QuickStartArgs, but scripts that
        // still pass it get a clear redirect to restart --allow-fresh.
        let ws = tmp_workspace();
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy(), "--fresh"]);
        let err = quick_start_args(&argv, &ws).expect_err("must refuse --fresh");
        let message = err.to_string();
        assert!(
            message.contains("no longer accepts --fresh"),
            "QR: refusal must say --fresh is gone; got: {message}"
        );
        assert!(
            message.contains("restart --allow-fresh"),
            "QR: refusal must redirect to `restart --allow-fresh`; got: {message}"
        );
    }

    #[test]
    fn quick_start_without_fresh_flag_still_builds_args() {
        // Without --fresh, args build normally (the initial-creation path).
        let ws = tmp_workspace();
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy()]);
        let args = quick_start_args(&argv, &ws).expect("must build");
        assert_eq!(args.workspace, ws);
        // No `fresh` field anymore — the struct must compile and round-trip
        // without it.
    }
}

#[cfg(test)]
#[path = "tests/parser_safety.rs"]
mod parser_safety_tests;
