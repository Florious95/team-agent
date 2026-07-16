//! cli · emit — `emit`(--json vs 人读 dict 逐键)+ 顶层 `run` 调度(parser.py `main`)+
//! 人读标量/集合渲染(`human_value` / `json_dumps_like`)。

use super::spec::{command_spec, CommandKind, CommandTier, ALL_DISPATCH_KINDS, COMMAND_SPECS};
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

/// `main(argv)`(`parser.py:84`):**CLI 唯一进程入口**。codex/claude/copilot passthrough 早返回 →
/// 解析 argv 到 subcommand → 调对应 handler → 异常落盘 + 信封 + `ExitCode::Error` →
/// `consume_leader_inbox_summary` → `emit` → `result.ok is False ? Error : Ok`。
/// **行为入口**:契约可端到端跑 argv→(stdout, exit code)。
pub fn run(argv: &[String], cwd: &Path) -> ExitCode {
    let Some(command) = argv.first().map(String::as_str) else {
        return emit_missing_subcommand_usage();
    };
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
    let Some(spec) = command_spec(command) else {
        return Ok(emit_unknown_subcommand_usage(command));
    };
    match spec.kind {
        CommandKind::Dispatch(_) => {}
        CommandKind::SpecOnlyAlias { .. } => {
            eprintln!("{}", command_help(Some(command)));
            return Ok(ExitCode::Usage);
        }
        CommandKind::LeaderPassthrough { .. } => {
            return Ok(emit_unknown_subcommand_usage(command));
        }
    }
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
        "stop" => cmd_shutdown(&shutdown_args(args, cwd)?).map(emit_result),
        "shutdown" => cmd_shutdown(&shutdown_args(args, cwd)?).map(emit_result),
        "restart" => cmd_restart(&restart_args(args, cwd)?).map(emit_result),
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
        "attach-app-server-leader" => {
            cmd_attach_app_server_leader(&attach_app_server_leader_args(args, cwd)?)
                .map(emit_result)
        }
        "identity" => cmd_identity(&identity_args(args, cwd)).map(emit_result),
        "approvals" => cmd_approvals(&approvals_args(args, cwd)).map(emit_result),
        "inbox" => cmd_inbox(&inbox_args(args, cwd)?).map(emit_result),
        "doctor" => cmd_doctor(&doctor_args(args, cwd)).map(emit_result),
        "watch" => cmd_watch(&watch_args(args, cwd)).map(emit_result),
        "sessions" => cmd_sessions(&sessions_args(args, cwd)).map(emit_result),
        // 0.5.9 E7 host-leader-registry: `leaders` is the host-level derived
        // discovery command. It reads ~/.team-agent/leaders, validates each
        // entry against canonical state, and reports LIVE/STALE/AMBIGUOUS
        // status. `--to-leader NAME` on `send` uses the same registry to
        // resolve short/qualified/hash-qualified names to a canonical
        // (workspace, team_key) tuple and delegates to the E6 named-leader
        // delivery path — no separate route authority.
        "leaders" => cmd_leaders(&leaders_args(args, cwd)).map(emit_result),
        "validate" => cmd_validate(&validate_args(args, cwd)).map(emit_result),
        "install-skill" => cmd_install_skill(&install_skill_args(args)?).map(emit_result),
        "profile" => cmd_profile(&profile_args(args, cwd)?).map(emit_result),
        "collect" => {
            cmd_collect_for_team(&collect_args(args, cwd)?, parse_args(args).team.as_deref())
                .map(emit_result)
        }
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
    "attach-app-server-leader",
    "identity",
    "approvals",
    "inbox",
    "doctor",
    "watch",
    "sessions",
    // 0.5.9 E7: host leader discovery command surface.
    "leaders",
    "validate",
    "install-skill",
    "profile",
    "collect",
    "diagnose",
    "preflight",
    "wait-ready",
    "e2e",
    "peek",
    "coordinator",
];

// 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.6):
// `purge-agent` was previously listed in help but had no dispatch arm,
// so it read as a supported recovery command while actually failing with
// "invalid choice". The dispatch registration remains out of scope for
// 0.5.26 (destructive semantics deserve their own CR); keep the help
// consistent with the dispatch table so it is no longer advertised.
const SPEC_ONLY_HELP_COMMANDS: &[&str] = &["start"];
// Command grammar, not provider identity parsing: these are top-level CLI
// passthrough verbs for starting a leader under a provider executable.
const LEADER_PASSTHROUGH_COMMANDS: &[&str] = &["codex", "claude", "copilot"];

fn is_leader_passthrough_command(command: &str) -> bool {
    LEADER_PASSTHROUGH_COMMANDS.contains(&command)
        && matches!(
            command_spec(command).map(|spec| spec.kind),
            Some(CommandKind::LeaderPassthrough { .. })
        )
}

fn emit_missing_subcommand_usage() -> ExitCode {
    emit_usage_error("the following arguments are required: {codex,claude,...,doctor}");
    ExitCode::Usage
}

/// Registered subcommands (the dispatch table) plus spec-only verbs that have no
/// dispatch arm yet but must still respond to `--help` per CR-063/G4.
/// Used by the `--help` short-circuit gate so unknown commands keep falling through
/// to the argparse invalid-choice path.
fn is_known_subcommand(command: &str) -> bool {
    command_spec(command).is_some_and(|spec| spec.command_help)
}

fn default_help() -> String {
    let mut out = String::from("usage: team-agent <command> [options]\n");
    append_help_section(
        &mut out,
        "Core",
        &["quick-start", "send", "status", "collect"],
    );
    append_help_section(
        &mut out,
        "Lifecycle",
        &[
            "restart",
            "shutdown",
            "add-agent",
            "start-agent",
            "stop-agent",
            "reset-agent",
        ],
    );
    append_help_section(&mut out, "Diagnose", &["diagnose"]);
    append_help_section(
        &mut out,
        "Guided recovery",
        &["claim-leader", "takeover", "attach-leader"],
    );
    out.push_str("\nProvider launchers:\n  team-agent codex|claude|copilot ...\n");
    out.push_str("\nRun `team-agent <command> --help` for command flags.");
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

fn compat_hidden_help(command: &str, usage: &str) -> String {
    let Some(spec) = command_spec(command) else {
        return usage.to_string();
    };
    if spec.tier != CommandTier::CompatHidden {
        return usage.to_string();
    }
    let sunset = spec.sunset.unwrap_or("C2");
    let action = spec.action.unwrap_or("use a supported command");
    format!("{usage}\n\nstatus: hidden compatibility command\nsunset: {sunset}\naction: {action}")
}

/// Test-only public accessor for `command_help` — allows integration
/// tests to grep the help copy without depending on internal parser
/// machinery.
pub fn __test_command_help(command: Option<&str>) -> String {
    command_help(command)
}

/// Test-only public accessor for `quick_start_args` — allows
/// integration tests to exercise the parser without going through
/// stdio + the full `main` entrypoint.
pub fn __test_quick_start_args(
    args: &[String],
    cwd: &std::path::Path,
) -> Result<crate::cli::types::QuickStartArgs, crate::cli::CliError> {
    quick_start_args(args, cwd)
}

fn command_help(command: Option<&str>) -> String {
    match command {
        None => default_help(),
        Some("init") => compat_hidden_help("init", "usage: team-agent init [--workspace WORKSPACE] [--force] [--json]"),
        Some("quick-start") => "usage: team-agent quick-start [TEAMDIR] [--workspace WORKSPACE] [--name NAME] [--team-id TEAM|--team TEAM] [--yes] [--no-display] [--backend tmux|conpty] [--json]\n\ndefaults: display_backend=adaptive; set display_backend: none in TEAM.md or pass --no-display to use one worker window per agent.\n\n--backend selects the worker transport (Phase 1d Batch 2): tmux (default on POSIX; unchanged behavior), conpty (Windows-native ConPTY worker transport; requires the shim binary and Windows host).".to_string(),
        Some("start") => compat_hidden_help("start", "usage: team-agent start [TEAMDIR] [--yes] [--fresh] [--json]"),
        Some("compile") => "usage: team-agent compile --team TEAM [--out FILE] [--json]".to_string(),
        Some("send") => concat!(
            "usage: team-agent send TARGET MESSAGE... ",
            "[--workspace WORKSPACE] [--team TEAM] [--targets AGENTS] ",
            "[--to-name NAME] [--pane PANE] [--task TASK] [--sender SENDER] ",
            "[--watch-result] [--requires-ack|--no-ack] [--no-wait] ",
            "[--timeout SECONDS] [--confirm-human] [--message-id ID] [--json]\n\n",
            "TARGET is a short id scoped by --team; MCP `to` is a short id scoped ",
            "by the worker's owner team.\n",
            "--to-name accepts: --to-name AGENT, --to-name TEAM/AGENT, or ",
            "--to-name WORKSPACE::TEAM/AGENT.\n",
            "--team scopes only a bare --to-name AGENT; qualified forms keep the ",
            "scope in the address.\n",
            "TEAM/AGENT and WORKSPACE::TEAM/AGENT are not valid positional TARGET ",
            "or MCP `to` values.\n\n",
            "MVP: name-based cross-workspace addressing assumes trusted local ",
            "caller; no auth gate."
        )
        .to_string(),
        Some("allow-peer-talk") => "usage: team-agent allow-peer-talk A B [--workspace WORKSPACE] [--json]".to_string(),
        Some("status") => "usage: team-agent status [AGENT] [--workspace WORKSPACE] [--team TEAM] [--summary|--json] [--detail]\n\n默认输出: worker,空闲|工作|错误；错误细分走 status --summary".to_string(),
        Some("stop") => compat_hidden_help("stop", "usage: team-agent stop [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]"),
        Some("shutdown") => "usage: team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]".to_string(),
        Some("restart") => "usage: team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json]".to_string(),
        Some("restart-agent") => compat_hidden_help("restart-agent", "usage: team-agent restart-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]"),
        Some("reset-agent") => "usage: team-agent reset-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]".to_string(),
        Some("start-agent") => "usage: team-agent start-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--force] [--allow-fresh] [--no-display] [--json]".to_string(),
        Some("stop-agent") => "usage: team-agent stop-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("add-agent") => "usage: team-agent add-agent AGENT --role-file FILE [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]".to_string(),
        Some("fork-agent") => "usage: team-agent fork-agent SOURCE_AGENT --as AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]".to_string(),
        Some("remove-agent") => "usage: team-agent remove-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--from-spec] [--confirm] [--force] [--json]".to_string(),
        // 0.5.26 (§7.6): removed from help; dispatch was never wired.
        Some("stuck-list") => "usage: team-agent stuck-list [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("stuck-cancel") => "usage: team-agent stuck-cancel AGENT [--workspace WORKSPACE] [--alert-type stuck|idle_fallback|cross_worker_deadlock|all] [--json]".to_string(),
        Some("acknowledge-idle") => "usage: team-agent acknowledge-idle [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("takeover") => "usage: team-agent takeover [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]".to_string(),
        Some("claim-leader") => "usage: team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]".to_string(),
        Some("attach-leader") => "usage: team-agent attach-leader [--workspace WORKSPACE] [--team TEAM] [--pane PANE] [--provider PROVIDER] [--confirm] [--json]".to_string(),
        Some("attach-app-server-leader") => "usage: team-agent attach-app-server-leader [--workspace WORKSPACE] [--team TEAM] --socket unix:///path.sock --thread-id THREAD_ID [--json]".to_string(),
        Some("identity") => "usage: team-agent identity [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("approvals") => "usage: team-agent approvals [AGENT] [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("inbox") => "usage: team-agent inbox AGENT [--workspace WORKSPACE] [--team TEAM] [--limit N] [--since CURSOR] [--json]".to_string(),
        Some("doctor") => "usage: team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--gate orphans|comms] [--comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]".to_string(),
        Some("watch") => "usage: team-agent watch [--workspace WORKSPACE] [--team TEAM]".to_string(),
        Some("sessions") => "usage: team-agent sessions [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("validate") => "usage: team-agent validate [SPEC] [--json]".to_string(),
        Some("install-skill") => "usage: team-agent install-skill (--source DIR | --uninstall) [--target codex|claude|copilot|all] [--dest DIR] [--dry-run] [--json]".to_string(),
        Some("profile") => "usage: team-agent profile COMMAND NAME [--workspace WORKSPACE] [--team TEAM] [--auth-mode MODE] [--json]".to_string(),
        Some("collect") => "usage: team-agent collect [--workspace WORKSPACE] [--team TEAM] [--result-file FILE] [--json]".to_string(),
        Some("diagnose") => "usage: team-agent diagnose [--workspace WORKSPACE] [--team TEAM] [--json]".to_string(),
        Some("preflight") => "usage: team-agent preflight [TEAMDIR] [--json]".to_string(),
        Some("wait-ready") => "usage: team-agent wait-ready [--workspace WORKSPACE] [--team TEAM] [--timeout SECONDS] [--json]".to_string(),
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
    // E8 (N38): 错路引导 —— 拼写近似时建议最接近的真子命令(additive,不改既有 golden 行)。
    if let Some(suggestion) = nearest_subcommand(command) {
        eprintln!("team-agent: did you mean `{suggestion}`?");
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
    eprintln!("usage: team-agent [-h] {{codex,claude,...,doctor}} ...");
    eprintln!("team-agent: error: {message}");
}

/// `cmd_validate` delegates to runtime validate_file.
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
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
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
    to_name: Option<String>,
    /// E7 (0.5.9 host-leader-registry-design §4.2): `send --to-leader NAME`
    /// resolves NAME through `~/.team-agent/leaders` to a canonical target
    /// (workspace, team_key) then delegates to the E6 leader delivery path.
    /// Mutually exclusive with `--to-name`, TARGET, `--pane`, and `--to`.
    to_leader: Option<String>,
    provider: Option<String>,
    socket: Option<String>,
    thread_id: Option<String>,
    message_id: Option<String>,
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
            "--name" => parsed.name = next_arg(args, &mut i),
            "--team-id" => parsed.team_id = next_arg(args, &mut i),
            "--targets" | "--target" | "--to" => parsed.targets = next_arg(args, &mut i),
            "--task" => parsed.task = next_arg(args, &mut i),
            "--task-id" => parsed.task_id = next_arg(args, &mut i),
            "--sender" => parsed.sender = next_arg(args, &mut i),
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
            "--no-display" => parsed.no_display = true,
            "--backend" => parsed.backend = next_arg(args, &mut i),
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
            "--to-name" => parsed.to_name = next_arg(args, &mut i),
            "--to-leader" => parsed.to_leader = next_arg(args, &mut i),
            "--provider" => parsed.provider = next_arg(args, &mut i),
            "--socket" => parsed.socket = next_arg(args, &mut i),
            "--thread-id" => parsed.thread_id = next_arg(args, &mut i),
            "--message-id" => parsed.message_id = next_arg(args, &mut i),
            "--content" => parsed.content = next_arg(args, &mut i),
            "--primary-error" => parsed.primary_error = next_arg(args, &mut i),
            "--result-json" => parsed.result_json = next_arg(args, &mut i),
            "-h" | "--help" => {}
            other if other.starts_with("--team=") => {
                parsed.team = Some(other.trim_start_matches("--team=").to_string());
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
            other if other.starts_with("--provider=") => {
                parsed.provider = Some(other.trim_start_matches("--provider=").to_string());
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
    if has_arg(args, "--fresh") {
        return Err(CliError::Usage(
            "quick-start no longer accepts --fresh. Reset semantics moved to \
             `team-agent restart --allow-fresh`, which requires explicit user \
             confirmation."
                .to_string(),
        ));
    }
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
    // 0.5.x Phase 1d Batch 2: validate the `--backend` literal up-front
    // so users get a fast, actionable error instead of a downstream
    // factory refusal. Accept the same literals as the factory
    // `RequestedTransportBackend::parse_literal`.
    if let Some(literal) = parsed.backend.as_deref() {
        let normalized = literal.trim().to_ascii_lowercase();
        if normalized != "tmux" && normalized != "conpty" {
            return Err(CliError::Usage(format!(
                "--backend must be `tmux` or `conpty`, got {literal:?}. \
                 `pty` is not a supported literal in Phase 1d (design \
                 §Non-Goals + CR C-1 ②)."
            )));
        }
    }
    Ok(QuickStartArgs {
        workspace,
        agents_dir,
        name: parsed.name,
        team_id: parsed.team_id.or(parsed.team),
        yes: parsed.yes,
        no_display: parsed.no_display,
        json: parsed.json,
        backend: parsed.backend,
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
        pane: parsed.pane.clone(),
        to_name: parsed.to_name.clone(),
        to_leader: parsed.to_leader.clone(),
    })
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

fn allow_peer_talk_args(args: &[String], cwd: &Path) -> Result<AllowPeerTalkArgs, CliError> {
    let parsed = parse_args(args);
    Ok(AllowPeerTalkArgs {
        a: required_pos(&parsed, 0, "a")?,
        b: required_pos(&parsed, 1, "b")?,
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
        team: parsed.team,
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
        team: parsed.team,
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
        team: parsed.team,
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

fn identity_args(args: &[String], cwd: &Path) -> IdentityArgs {
    let parsed = parse_args(args);
    IdentityArgs {
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
        json: parsed.json,
    }
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
    let workspace = parsed
        .positionals
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace(&parsed, cwd));
    refuse_if_multi_alive_team_missing_scope("restart", &workspace, parsed.team.as_deref())?;
    Ok(RestartArgs {
        workspace,
        team: parsed.team,
        allow_fresh: parsed.allow_fresh,
        session_converge_deadline_ms: parsed.session_converge_deadline_ms,
        json: parsed.json,
    })
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
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("reset-agent", &workspace, parsed.team.as_deref())?;
    Ok(ResetAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace,
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
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("remove-agent", &workspace, parsed.team.as_deref())?;
    Ok(RemoveAgentArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace,
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
        team: parsed.team,
    }
}

fn stuck_cancel_args(args: &[String], cwd: &Path) -> Result<StuckCancelArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("stuck-cancel", &workspace, parsed.team.as_deref())?;
    Ok(StuckCancelArgs {
        agent: required_pos(&parsed, 0, "agent")?,
        workspace,
        alert_type: alert_type(parsed.alert_type.as_deref())?,
        json: parsed.json,
        team: parsed.team,
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
        Some(other) => Some(DoctorGate::Unknown(other.to_string())),
        None => None,
    }
}

fn sessions_args(args: &[String], cwd: &Path) -> SessionsArgs {
    let parsed = parse_args(args);
    SessionsArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
        team: parsed.team,
    }
}

fn leaders_args(args: &[String], _cwd: &Path) -> LeadersArgs {
    let parsed = parse_args(args);
    LeadersArgs { json: parsed.json }
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

fn collect_args(args: &[String], cwd: &Path) -> Result<CollectArgs, CliError> {
    let parsed = parse_args(args);
    let workspace = workspace(&parsed, cwd);
    refuse_if_multi_alive_team_missing_scope("collect", &workspace, parsed.team.as_deref())?;
    Ok(CollectArgs {
        workspace,
        result_file: parsed.result_file,
        json: parsed.json,
        team: parsed.team,
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
        team: parsed.team,
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
        team: parsed.team,
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
                if trimmed.starts_with("team-agent ") {
                    return None;
                }
                let command = trimmed.split_whitespace().next()?;
                command
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
                    .then(|| command.to_string())
            })
            .collect()
    }

    #[test]
    fn command_specs_have_unique_names_and_valid_aliases() {
        let mut names = std::collections::BTreeSet::new();
        for spec in COMMAND_SPECS {
            assert!(
                names.insert(spec.name),
                "duplicate command spec `{}`",
                spec.name
            );
        }
        for spec in COMMAND_SPECS {
            if let Some(alias_of) = spec.alias_of {
                assert!(
                    names.contains(alias_of),
                    "`{}` aliases missing command `{alias_of}`",
                    spec.name
                );
            }
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
        for spec in COMMAND_SPECS.iter().filter(|spec| spec.default_help) {
            assert!(
                visible.iter().any(|command| command == spec.name),
                "`{}` has default_help=true but is missing from top-level help",
                spec.name
            );
        }
        assert!(
            visible.len() <= 15,
            "default visible command count must stay <= 15, got {visible:?}"
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
        for command in ["leaders", "doctor", "e2e", "peek", "coordinator"] {
            assert!(
                !visible.iter().any(|visible| visible == command),
                "`{command}` must stay hidden from default help"
            );
        }
    }

    #[test]
    fn compat_hidden_help_has_sunset_action() {
        for command in ["stop", "restart-agent", "start", "init"] {
            let help = command_help(Some(command)).to_lowercase();
            assert!(help.contains("status: hidden compatibility command"));
            assert!(help.contains("sunset: c2"));
            assert!(help.contains("action:"));
        }
    }

    #[test]
    fn observation_a_commands_have_terminal_tiers() {
        for (command, tier) in [
            ("allow-peer-talk", CommandTier::Secondary),
            ("approvals", CommandTier::Secondary),
            ("profile", CommandTier::Secondary),
            ("install-skill", CommandTier::Secondary),
            ("init", CommandTier::CompatHidden),
        ] {
            assert_eq!(command_spec(command).map(|spec| spec.tier), Some(tier));
        }
    }

    #[test]
    fn suggestion_index_excludes_hidden_commands() {
        assert_eq!(nearest_subcommand("statu"), Some("status"));
        assert_eq!(nearest_subcommand("leader"), None);
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
            (
                "send",
                &[
                    "--workspace",
                    "--team",
                    "--targets",
                    "--to-name",
                    "--pane",
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
            ("stuck-list", &["--workspace", "--team", "--json"][..]),
            ("approvals", &["--workspace", "--team", "--json"][..]),
            (
                "inbox",
                &["--workspace", "--team", "--limit", "--since", "--json"][..],
            ),
            ("sessions", &["--workspace", "--team", "--json"][..]),
            ("diagnose", &["--workspace", "--team", "--json"][..]),
            (
                "wait-ready",
                &["--workspace", "--team", "--timeout", "--json"][..],
            ),
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
        assert!(
            !command_help(Some("quick-start")).contains("--fresh"),
            "quick-start help must not advertise removed reset semantics"
        );
    }

    #[test]
    fn status_help_mentions_summary_for_error_details() {
        assert!(
            command_help(Some("status")).contains("错误细分走 status --summary"),
            "status help must tell users where detailed error classes live"
        );
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
        let args = send_args(
            &cli_argv(&["--pane", "%1596", "--to", "worker", "hello"]),
            &cwd,
        )
        .unwrap();
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message.contains("--pane and TARGET/--to are mutually exclusive")),
            "expected --pane/--to mutual-exclusion usage error, got {err:?}"
        );

        let args = send_args(&cli_argv(&["--pane", "%1596"]), &cwd).unwrap();
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message == "--pane requires a non-empty message"),
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

    // ── E8 (N38): 未知子命令 → 最近似建议(additive,不破坏 golden invalid-choice 行) ──
    #[test]
    fn e8_unknown_subcommand_suggests_nearest_known_command() {
        // 'statu' typo → status; 'add-agen' → add-agent.
        assert_eq!(nearest_subcommand("statu"), Some("status"));
        assert_eq!(nearest_subcommand("add-agen"), Some("add-agent"));
        assert_eq!(nearest_subcommand("start-agnet"), Some("start-agent"));
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
    fn refuse_helper_passes_when_explicit_team_provided_even_in_multi_alive() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        assert!(
            refuse_if_multi_alive_team_missing_scope("collect", &ws, Some("alpha")).is_ok(),
            "explicit --team alpha must bypass the ambiguity gate"
        );
    }

    #[test]
    fn refuse_helper_refuses_when_multi_alive_team_and_no_explicit_team() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let err = refuse_if_multi_alive_team_missing_scope("collect", &ws, None)
            .expect_err("multi-alive-team must refuse without --team");
        let message = err.to_string();
        assert!(
            message.contains("multiple alive teams"),
            "refusal must name the ambiguity; got: {message}"
        );
        assert!(
            message.contains("alpha") && message.contains("beta"),
            "refusal must list candidate teams; got: {message}"
        );
        assert!(
            message.contains("collect"),
            "refusal must name the command for diagnostic clarity; got: {message}"
        );
    }

    #[test]
    fn stuck_cancel_args_builder_refuses_on_multi_alive_team() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["worker_a", "--workspace", &ws.to_string_lossy()]);
        let err = stuck_cancel_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "stuck-cancel args builder must surface the refusal; got: {err}"
        );
    }

    #[test]
    fn collect_args_builder_refuses_on_multi_alive_team() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy()]);
        let err = collect_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "collect args builder must surface the refusal; got: {err}"
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
    fn reset_agent_args_builder_refuses_on_multi_alive_team_before_agent_validation() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy()]);
        let err = reset_agent_args(&argv, &ws).expect_err("must refuse");
        assert!(
            err.to_string().contains("multiple alive teams"),
            "reset-agent args builder must surface the refusal; got: {err}"
        );
    }

    #[test]
    fn remove_agent_args_builder_refuses_on_multi_alive_team_before_agent_validation() {
        let ws = tmp_workspace();
        seed_two_alive_teams_in(&ws);
        let argv = cli_argv(&["--workspace", &ws.to_string_lossy()]);
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
        let _ = args.no_display;
    }
}
