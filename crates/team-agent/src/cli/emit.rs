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
/// `main(argv)`(`parser.py:84`):**CLI 唯一进程入口**。codex/claude/copilot/grok/cursor passthrough 早返回 →
/// 解析 argv 到 subcommand → 调对应 handler → 异常落盘 + 信封 + `ExitCode::Error` →
/// `consume_leader_inbox_summary` → `emit` → `result.ok is False ? Error : Ok`。
/// **行为入口**:契约可端到端跑 argv→(stdout, exit code)。
pub fn run(argv: &[String], cwd: &Path) -> ExitCode {
    let Some(command) = argv.first().map(String::as_str) else {
        println!("{}", default_help());
        return ExitCode::Ok;
    };
    if is_machine_command(command) && argv[1..].iter().any(|arg| matches!(arg.as_str(), "-h" | "--help")) {
        eprintln!("{HUMAN_NAVIGATION}");
        return ExitCode::Usage;
    }
    if command == "route" {
        return super::route::run(&argv[1..]);
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
        Err(error) => emit_cli_error_for_command(command, &argv[1..], cwd, &error),
    }
}

fn emit_cli_error_for_command(
    command: &str,
    args: &[String],
    cwd: &Path,
    error: &CliError,
) -> ExitCode {
    let missing_input = matches!(error, CliError::Usage(message) if message.starts_with("missing ") || message.starts_with("请填写") || matches!(message.as_str(), "add-agent requires --provider <name>" | "add-agent requires --bypass <true|false>" | "--source is required unless --uninstall"));
    if command_spec(command).is_some() && missing_input {
        let help = command_help(Some(command));
        let explanation = match error {
            CliError::Usage(message) => match message.as_str() {
                "missing agent" | "missing source_agent" => "请填写队友名。".to_string(),
                "missing profile command" => "请填写 profile 操作：init、doctor 或 show。".to_string(),
                "missing profile name" => "请填写登录/代理配置的名称。".to_string(),
                "add-agent requires --provider <name>" => "请用 --provider 明确选择工具，或用 --role-file 提供角色文件。".to_string(),
                "add-agent requires --bypass <true|false>" => "请用 --bypass false 明确保留权限询问，或在角色文件中明确此设置。".to_string(),
                other => other.strip_prefix("missing ").map(|field| format!("请补齐必需参数：{field}。")).unwrap_or_else(|| other.to_string()),
            },
            _ => error.to_string(),
        };
        if has_arg(args, "--json") {
            let payload = error.to_payload(Path::new(""), command);
            let mut value = serde_json::to_value(payload).unwrap_or_else(|_| serde_json::json!({"ok": false, "error": error.to_string()}));
            value["error"] = serde_json::json!(explanation);
            value["action"] = serde_json::json!(format!("team-agent {command} --help"));
            value["next_actions"] = serde_json::json!([format!("请按 team-agent {command} --help 的 Examples 补齐参数")]);
            println!("{}", python_compact_json(&value));
        } else {
            eprintln!("{explanation}\n\n{help}");
        }
        ExitCode::Usage
    } else if command == "status" {
        emit_status_cli_error(error)
    } else {
        emit_cli_error(command, args, cwd, error)
    }
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
                    "输出暂不可用；消息已保存，消息编号={message_id}。用 team-agent inbox leader -n 3 查看回复，不要重发。"
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
    // Keep the removed flag harmless for older scripts and persisted command
    // lines; the default tmux backend is already the only runtime path.
    let filtered_args: Vec<String> = args
        .iter()
        .filter(|arg| arg.as_str() != "--no-display")
        .cloned()
        .collect();
    let args = filtered_args.as_slice();
    if is_machine_command(command) {
        if args.iter().any(|arg| matches!(arg.as_str(), "-h" | "--help")) {
            eprintln!("{HUMAN_NAVIGATION}");
            return Ok(ExitCode::Usage);
        }
        return dispatch_machine(command, args, cwd);
    }
    let Some(spec) = command_spec(command) else {
        return Ok(emit_unknown_subcommand_usage(command));
    };
    match spec.kind {
        CommandKind::Dispatch(_) => {}
        CommandKind::LeaderPassthrough { .. } => {
            return Ok(emit_unknown_subcommand_usage(command));
        }
    }
    if args.iter().all(|arg| arg == "--json") && matches!(command, "send" | "inbox" | "add-agent" | "start-agent" | "stop-agent" | "reset-agent" | "clone-agent" | "fork-agent" | "remove-agent" | "allow-peer-talk" | "profile") {
        return Err(CliError::Usage("请填写队友名、任务内容或命令所需设置；见下方用法与 Examples".to_string()));
    }
    match command {
        "quick-start" => cmd_quick_start(&quick_start_args(args, cwd)?).map(emit_result),
        "send" => cmd_send(&send_args(args, cwd)?).map(emit_result),
        "allow-peer-talk" => {
            cmd_allow_peer_talk(&allow_peer_talk_args(args, cwd)?).map(emit_result)
        }
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

        "install-skill" => cmd_install_skill(&install_skill_args(args)?).map(emit_result),
        "profile" => cmd_profile(&profile_args(args, cwd)?).map(emit_result),

        _ => Ok(emit_unknown_subcommand_usage(command)),
    }
}

// Script compatibility is deliberately outside the human catalog and suggestions.
fn is_machine_command(command: &str) -> bool {
    matches!(command, "results" | "wait" | "wait-ready" | "preflight" | "validate" | "identity" | "sessions" | "watch" | "e2e" | "peek" | "coordinator" | "attach-app-server-leader")
}

fn dispatch_machine(command: &str, args: &[String], cwd: &Path) -> Result<ExitCode, CliError> {
    match command {
        "results" => cmd_results(&results_args(args, cwd)?).map(emit_result),
        "wait" => cmd_wait(&wait_args(args, cwd)?).map(emit_result),
        "wait-ready" => cmd_wait_ready(&wait_ready_args(args, cwd)).map(emit_result),
        "preflight" => cmd_preflight(&preflight_args(args, cwd)).map(emit_result),
        "validate" => cmd_validate(&validate_args(args, cwd)).map(emit_result),
        "identity" => cmd_identity(&identity_args(args, cwd)).map(emit_result),
        "sessions" => cmd_sessions(&sessions_args(args, cwd)).map(emit_result),
        "watch" => cmd_watch(&watch_args(args, cwd)).map(emit_result),
        "e2e" => cmd_e2e(&e2e_args(args, cwd)).map(emit_result),
        "peek" => cmd_peek(&peek_args(args, cwd)?).map(emit_result),
        "coordinator" => run_coordinator(args, cwd),
        "attach-app-server-leader" => cmd_attach_app_server_leader(&attach_app_server_leader_args(args, cwd)?).map(emit_result),
        _ => Ok(emit_unknown_subcommand_usage(command)),
    }
}

// Command grammar, not provider identity parsing: these are top-level CLI
// passthrough verbs for starting a leader under a provider executable.
const LEADER_PASSTHROUGH_COMMANDS: &[&str] =
    &["codex", "claude", "copilot", "grok", "cursor", "pi"];

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
    let mut out = String::from("Team Agent：起一支队伍，给队友发任务，看回复，安全关队。\n\n1. 准备队友\n   在项目目录运行 team-agent quick-start；空目录会给出 TEAM.md 和 agents/worker.md 两文件模板，请自行创建。\n2. 打开主控并起队\n   已安装且已登录 Pi？运行 team-agent pi。\n   在该主控的命令行/工具上下文运行 team-agent quick-start。\n   已有队伍要恢复？看 team-agent restart --help，不要接管或删除它。\n3. 发任务、看回复\n   team-agent send worker '计算 245 × 37，把答案回复给 leader。'\n   team-agent inbox leader -n 3\n   team-agent status\n   发出任务不等于完成；等队友真正回复。\n4. 体检、关队\n   team-agent doctor --workspace .\n   team-agent shutdown --workspace . --json\n   检查本队残留为空，再用 doctor 确认所选项目没有待处理问题。\n\n多个项目/队伍用 --workspace/--team 选择，不猜对象；模型名称先用 models 查询。\nWindows 使用 ConPTY 需要 Windows 主机和已安装的 shim；tmux 指令不是通用 Windows 路径。\n");
    append_help_section(&mut out, "开始协作", &["quick-start", "send", "status", "models", "inbox"]);
    append_help_section(&mut out, "队伍管理", &["restart", "shutdown"]);
    append_help_section(&mut out, "队友管理", &["add-agent", "start-agent", "stop-agent", "reset-agent", "clone-agent", "fork-agent", "remove-agent"]);
    append_help_section(&mut out, "观察与协作", &["leaders", "doctor", "approvals", "allow-peer-talk"]);
    append_help_section(&mut out, "设置", &["route", "profile", "install-skill"]);
    append_help_section(&mut out, "按体检提示恢复", &["claim-leader", "takeover", "attach-leader"]);
    append_help_section(&mut out, "启动主控", &["pi", "codex", "claude", "copilot", "grok", "cursor"]);
    out.push_str("\n每个操作的参数和例子：team-agent <command> --help");
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

pub(super) const TEAM_TEMPLATE: &str = "---\nname: help-demo\n---\nA small team for a command-line example.\n";
pub(super) const WORKER_TEMPLATE: &str = "---\nname: worker\nrole: assistant\nprovider: pi\nmodel: openai-codex/gpt-6-luna\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\n完成任务后，把简明答案回复给 leader。\n";
const HUMAN_NAVIGATION: &str = "请用 team-agent --help 查看可用操作。";

pub(super) fn command_help(command: Option<&str>) -> String {
    let Some(name) = command else { return default_help(); };
    let Some(spec) = command_spec(name) else { return HUMAN_NAVIGATION.to_string(); };
    if name == "route" { return super::route::HELP.to_string(); }
    let (details, examples, next) = match name {
        "quick-start" => (
            "TEAMDIR 是含 TEAM.md 和 agents/ 的角色目录；--workspace 选择运行项目。\n--name 设置队伍名称；--team/--team-id 选择队伍；--yes 确认已有提示；--detail 查看详细返回。\n工具须已安装并登录；工具/模型/思考强度写在角色文件中；跳过权限询问默认 false。\n--backend tmux 用于 POSIX；Windows 的 conpty 需要 Windows 主机和已安装的 ConPTY shim。",
            "team-agent quick-start\nteam-agent quick-start ./roles --workspace ./project\nteam-agent quick-start . --json",
            "确认 Pi 已安装并登录，用 team-agent models --provider pi 核对模型。\n运行 team-agent pi，在该主控的命令行/工具上下文再运行 quick-start；已有队伍看 restart --help。"),
        "send" => ("<agent> 是 status 中的队友名；MESSAGE 是任务内容。\n--mailbox 只留言，不发送到当前对话；默认发送到队友对话。",
            "team-agent send worker '计算 245 × 37，把答案回复给 leader。'\nteam-agent send worker '检查改动' --workspace . --team help-demo\nteam-agent send worker '下次上线时查看' --mailbox",
            "已收下任务不等于送达或完成。等真实回复，用 team-agent inbox leader -n 3；有疑问看 status/doctor，不反复重发。"),
        "inbox" => ("<agent> 是收信队友名；-n/--limit 限制条数。查看回复不触发重发。",
            "team-agent inbox leader -n 3\nteam-agent inbox worker --limit 5\nteam-agent inbox leader --workspace . --team help-demo --json", "没有回复时先等队友自然返回，再看 team-agent status；不要读取队友终端内容。"),
        "status" => ("可选 <agent> 只查看一位队友；--summary/--detail 保留兼容，不增加字段。\n九字段：name/provider/model/effort/runtime_status/activity/health/session_name/tmux_command。\n运行状态、忙闲、健康各有含义；unknown 表示证据不足，null 表示未设置，不猜默认模型。",
            "team-agent status\nteam-agent status worker\nteam-agent status --workspace . --team help-demo --json", "看回复用 team-agent inbox leader -n 3；查问题用 team-agent doctor --workspace .。"),
        "models" => ("--provider 选择工具，默认 pi；QUERY 或 --search 按关键词搜索，不能同时用。\nCursor 的工具名是 cursor_agent；Copilot 暂无此模型查询入口。返回真实目录，不猜模型。",
            "team-agent models --provider pi\nteam-agent models --provider pi --search luna\nteam-agent models --provider cursor_agent --json", "把返回的完整模型名称填入角色文件或 add-agent --model，再起队或加人。"),
        "restart" => ("WORKSPACE 默认当前项目；优先恢复已保存会话。\n只有用户明确允许丢弃旧对话时才加 --allow-fresh；--session-converge-deadline 设置等待秒数。",
            "team-agent restart .\nteam-agent restart . --team help-demo --json", "用 status 查看恢复结果；需要新会话先征得用户同意，不用 takeover 代替恢复。"),
        "shutdown" => ("只关闭所选项目/队伍；日志默认保留，--keep-logs 保留兼容。\n--json 查看关闭范围、降级情况及本队残留，返回成功不代表其他队伍已关闭。",
            "team-agent shutdown --workspace .\nteam-agent shutdown --workspace . --team help-demo --json", "核对本队残留为空，再用 team-agent doctor --workspace .；不要广域清理其他队伍。"),
        "add-agent" => ("<agent> 是新队友名；--provider 选择工具；--model 模型名称；--effort 思考强度。\n--bypass 是否跳过权限询问，示例 false；--prompt 任务职责；--profile 登录/代理设置。\n工具和 bypass 必须由参数或 --role-file 明确提供；冲突会拒绝，不猜设置。--force 替换已有队友，仅在用户明确授权后用。",
            "team-agent add-agent reviewer --provider pi --model openai-codex/gpt-6-luna --bypass false --prompt '检查任务'\nteam-agent add-agent reviewer --role-file ./agents/reviewer.md", "用 status 确认队友，再运行 team-agent send reviewer '检查改动'。"),
        "start-agent" => ("只启动已有队友；仍在运行时先 stop-agent。\n--provider 工具；--model 模型名称；--effort 思考强度；--bypass 是否跳过权限询问。\n--prompt/--profile 更换职责/登录设置；只有明确获准丢弃旧对话才用 --allow-fresh；--force 仅在获准替换已有队友后用。",
            "team-agent start-agent worker\nteam-agent start-agent worker --model openai-codex/gpt-6-luna", "新增队友用 add-agent；启动后看 status，再 send 分派任务。"),
        "stop-agent" => ("<agent> 指定要暂停的队友；暂停不是删除，配置与会话记录保留。",
            "team-agent stop-agent worker\nteam-agent stop-agent worker --workspace . --team help-demo --json", "用 status 确认停止；恢复用 team-agent start-agent worker。"),
        "reset-agent" => ("必须明确加 --discard-session，清除保存的会话关联；不承诺工具会删除全部历史记录。",
            "team-agent reset-agent worker --discard-session\nteam-agent reset-agent worker --discard-session --workspace . --team help-demo --json", "仅在用户同意丢弃会话关联后执行；再用 start-agent 启动并核对 status。"),
        "clone-agent" => ("<agent> 是源队友；--as 新队友名；--label 可读标签。\n复制配置创建新队友，不复制完整对话。",
            "team-agent clone-agent worker --as reviewer\nteam-agent clone-agent worker --as reviewer --label '审阅队友' --workspace .", "用 status 查看新队友，再 send reviewer 分派独立任务。"),
        "fork-agent" => ("<agent> 是源队友；--as 新队友名；--label 可读标签。\n需要源会话已保存且工具支持分支；不满足条件会拒绝，不保证所有工具都可分支。",
            "team-agent fork-agent worker --as experiment\nteam-agent fork-agent worker --as experiment --workspace . --team help-demo --json", "分支成功会占用新资源；看 status，再向 experiment 派发任务，用完停止或移除。"),
        "remove-agent" => ("必须 --confirm；配置定义的队友还需 --from-spec，动态队友可以不加。\n只删除托管角色副本，不删除外部用户角色文件；运行中的队友先 stop-agent。\n--force 可停止并移除运行中的队友，仅在理解风险且明确授权后使用。",
            "team-agent remove-agent reviewer --confirm\nteam-agent remove-agent reviewer --from-spec --confirm --workspace . --team help-demo --json", "先保存所需回复；移除后核对 status 和实际配置范围，不对其他队友使用强制清理。"),
        "leaders" => ("默认列出可用主控；--all 包含已失效登记，--stale 只看失效项。\nQUERY/--search 按项目/队伍/名称筛选；--prune 仅清理已确认退役的登记，--dry-run 先预览。",
            "team-agent leaders\nteam-agent leaders --all --json\nteam-agent leaders --prune --dry-run", "根据项目和队伍选择 --workspace/--team；清理登记不等于关闭进程。"),
        "doctor" => ("默认只检查，不修复；SPEC 可选配置路径。\n--comms/--gate comms 检查连接，不证明队友实际回复；--gate orphans 检查残留。\n--fix/--fix-schema/--cleanup-orphans 仅在获准的范围内使用；危险修复需要 --confirm。",
            "team-agent doctor --workspace .\nteam-agent doctor --workspace . --team help-demo --json", "按实际问题与所选队伍范围处理；不要把体检建议当成已执行的修复。"),
        "approvals" => ("可选 <agent> 查看一位队友；仅观察权限询问，不自动批准。",
            "team-agent approvals\nteam-agent approvals worker --json", "确认权限用途后，在工具的实际权限询问处处理，再看 status。"),
        "allow-peer-talk" => ("指定两位现有队友；--workspace 选择项目。此命令不支持 --team。",
            "team-agent allow-peer-talk worker reviewer\nteam-agent allow-peer-talk worker reviewer --workspace . --json", "再向队友发送交流任务；允许交流不等于已经互发消息。"),
        "profile" => ("init 创建配置，doctor 检查配置，show 显示设置；NAME 是配置名。\n--auth-mode 登录方式；--proxy-mode direct 不走代理、inherit 沿用环境代理。\n配置存放在所选工作区，不因 --team 自动变成队伍独立配置；不要输出凭证。",
            "team-agent profile init local --auth-mode subscription\nteam-agent profile doctor local\nteam-agent profile show local", "检查通过后用 add-agent/start-agent --profile local；需要登录时使用工具原生登录入口。"),
        "install-skill" => ("--source 必须是已核实的指南目录；--target 选择 codex/claude/copilot/all。\n--dest 覆盖安装位置；--dry-run 只预览；--uninstall 移除目标指南。此项不是起队前提。",
            "team-agent install-skill --source \"$SKILL_DIR\" --target codex --dry-run\nteam-agent install-skill --source \"$SKILL_DIR\" --target codex", "先把 SKILL_DIR 设为实际已核验目录；核对预览后再安装，不覆盖未知用户文件。"),
        "claim-leader" => ("仅在 doctor 明确提示登记主控、且当前终端确为主控时使用。\n--confirm 表示授权登记；不会自动打开新工具。",
            "team-agent claim-leader --workspace . --json\nteam-agent claim-leader --workspace . --team help-demo --confirm --json", "先核对 doctor 与当前终端归属；未确认前的拒绝不是一次成功登记。"),
        "takeover" => ("仅在 doctor 提示接管且用户已授权时使用 --confirm。\n没有 --confirm 的调用会拒绝，不是安全预演；正常恢复用 restart。",
            "team-agent takeover --workspace . --team help-demo\nteam-agent takeover --workspace . --team help-demo --confirm --json", "核实主控和队伍归属再确认；完成后查看 status/doctor，不接管其他人的队伍。"),
        "attach-leader" => ("PANE 必须来自实际核验的主控终端，不能猜终端编号。\n--provider 选择实际工具；--confirm 表示授权连接已有主控，仅按 doctor 提示使用。",
            "team-agent attach-leader --pane \"$PANE\" --provider pi --workspace .\nteam-agent attach-leader --pane \"$PANE\" --provider pi --workspace . --team help-demo --confirm --json", "先将 PANE 设为已核验的终端；连接后用 doctor 确认，再派发任务。"),
        _ => ("先确保工具已安装并完成原生登录；当前目录作为项目。\n--attach-existing 与 --confirm 只在已核验主控归属并明确授权后使用；--attach-session 选择已核验的会话。\n--external-leader 选择外部主控；--allow-nested-attach 仅在明确授权使用嵌套终端时添加。\n-- 后面的参数原样交给工具；纯 --help/-h 只显示这里的帮助，不启动工具。",
            "", "在打开的主控命令行/工具上下文运行 team-agent quick-start；不是在普通队友对话中起队。"),
    };
    let examples = if matches!(spec.kind, CommandKind::LeaderPassthrough { .. }) {
        let native = if name == "cursor" { "agent" } else { name };
        if name == "pi" {
            "team-agent pi\nteam-agent pi -- --model openai-codex/gpt-6-luna\npi --help".to_string()
        } else {
            format!("team-agent {name}\nteam-agent {name} --help\n{native} --help")
        }
    } else { examples.to_string() };
    let scope = if matches!(spec.kind, CommandKind::LeaderPassthrough { .. }) {
        "--json 给程序读取；不把 --workspace/--team 当作主控选项传入，它们会交给原生工具。"
    } else { "--json 给程序读取；--workspace 选项目，支持 --team 的操作用它选队伍，不猜对象。" };
    let mut out = format!("做什么：\n{}。\n用法：{}\n\n怎么用：\n{}\n{}\nExamples（可复制）：\n{}\n\n下一步（Next Action）：\n{}", spec.summary, spec.usage, details, scope, examples, next);
    if name == "quick-start" {
        out.push_str(&format!("\n\n最简两文件（请自行创建，不会自动写入）：\nTEAM.md：\n{TEAM_TEMPLATE}\nagents/worker.md：\n{WORKER_TEMPLATE}"));
    }
    if name == "cursor" { out.push_str("\n查询模型用 team-agent models --provider cursor_agent。"); }
    if name == "copilot" { out.push_str("\nCopilot 暂无 team-agent models 查询入口，请看工具原生帮助。"); }
    out
}

fn emit_unknown_subcommand_usage(command: &str) -> ExitCode {
    emit_usage_error(&format!(
        "没有这个操作：'{command}'。请用 team-agent --help 查看可用操作。"
    ));
    // E8 (N38): 错路引导 —— 拼写近似时建议最接近的真子命令(additive,不改既有 golden 行)。
    if let Some(suggestion) = nearest_subcommand(command) {
        eprintln!("你是否想使用 `{suggestion}`？运行 team-agent {suggestion} --help 查看例子。");
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
    eprintln!("用法：team-agent <command>；查看所有操作：team-agent --help");
    eprintln!("错误：{message}");
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
    let safe_error = crate::redaction::redact_external_text(&payload_error.to_string());
    let _ = std::fs::write(&log_path, format!("{safe_error}\n"));
    let mut payload = payload_error.to_payload(&log_path, command);
    payload.error = safe_error;
    if let CliError::AddressRefusal { error, action, .. } = payload_error {
        if !has_arg(args, "--json") {
            payload.error = error.clone();
            payload.action = action.clone();
        }
    } else if let Some(reason) = payload.error.strip_prefix("usage error: ").unwrap_or(&payload.error).strip_prefix("Error: ").and_then(|text| text.split_once(':').map(|(reason, _)| reason)) {
        if let Some(message) = super::named_address::human_address_reason(reason) {
            payload.error = message.to_string();
            payload.action = "先运行 team-agent status，再用列表中的队友名发送；用 --workspace/--team 选择项目和队伍，连接问题先 doctor。".to_string();
        }
    }
    if command_spec(command).is_some() && payload.action == "先运行 team-agent doctor --workspace . 检查所选队伍，或查看此处列出的错误日志。" {
        let mut doctor = format!("team-agent doctor --workspace {}", super::adapters::shell_quote(&workspace.to_string_lossy()));
        if let Some(team) = parsed.team.as_deref() { doctor.push_str(&format!(" --team {}", super::adapters::shell_quote(team))); }
        payload.action = format!("先运行 {doctor} 检查所选队伍，或查看此处列出的错误日志。");
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
    if has_arg(args, "--json") {
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
    providers: Option<String>,
    allow_raw_screen: bool,
    tail: Option<usize>,
    head: Option<usize>,
    search: Option<String>,
    file: Option<PathBuf>,
    result: Option<String>,
    real: bool,
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
            "--providers" => parsed.providers = next_arg(args, &mut i),
            "--allow-raw-screen" => parsed.allow_raw_screen = true,
            "--tail" => parsed.tail = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok()),
            "--head" => parsed.head = next_arg(args, &mut i).and_then(|v| v.parse::<usize>().ok()),
            "--search" => parsed.search = next_arg(args, &mut i),
            "--file" => parsed.file = next_arg(args, &mut i).map(PathBuf::from),
            "--result" => parsed.result = next_arg(args, &mut i),
            "--real" => parsed.real = true,
            "--assignee" => parsed.assignee = next_arg(args, &mut i),
            "--auth-mode" => parsed.auth_mode = next_arg(args, &mut i),
            "--proxy-mode" => parsed.proxy_mode = next_arg(args, &mut i),
            "--pane" => parsed.pane = next_arg(args, &mut i),
            "--to-name" => parsed.to_name = next_arg(args, &mut i),
            "--to-leader" => parsed.to_leader = next_arg(args, &mut i),
            "--provider" => parsed.provider = next_arg(args, &mut i),
            "--socket" => parsed.socket = next_arg(args, &mut i),
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
                .find(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some(".team"))
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
                "--backend 只能是 tmux 或 conpty，收到 {literal:?}。POSIX 使用 tmux；Windows ConPTY 需要 Windows 主机和已安装的 shim。"
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
    if parsed.targets.is_none() && parsed.pane.is_none() && parsed.to_name.is_none() && parsed.to_leader.is_none() && parsed.positionals.len() < 2 {
        return Err(CliError::Usage("请填写收信队友名和任务内容，例如 team-agent send worker '检查改动'".to_string()));
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
        eprintln!("提示：旧发送设置已弃用。直接用 team-agent send <agent> '任务内容'；只留言请加 --mailbox。");
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
        task: None,
        sender,
        no_ack: false,
        no_wait: true,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: parsed.json,
        message_id: None,
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
        "--task",
        "--watch-result",
        "--requires-ack",
        "--no-ack",
        "--no-wait",
        "--timeout",
        "--confirm-human",
        "--message-id",
    ];
    let spec = command_spec("send");
    let sunset = spec
        .and_then(|spec| spec.sunset)
        .unwrap_or("后续兼容版本");
    let action = spec
        .and_then(|spec| spec.action)
        .unwrap_or("使用 team-agent send <agent> '任务内容'，通过 --workspace/--team 选项目和队伍");
    for flag in FLAGS {
        if args
            .iter()
            .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
        {
            eprintln!("提示：{flag} 已弃用，将在{sunset}移除。下一步：{action}");
        }
    }
}

fn validate_send_flags(args: &[String]) -> Result<(), CliError> {
    const ALLOWED: &[&str] = &[
        "--workspace",
        "--team",
        "--targets",
        "--target",
        "--to",
        "--to-name",
        "--to-leader",
        "--pane",
        "--task",
        "--watch-result",
        "--requires-ack",
        "--no-ack",
        "--no-wait",
        "--timeout",
        "--confirm-human",
        "--message-id",
        "--mailbox",
        "--presentation-sink",
        "--message-class",
        "--case-id",
        "--json",
        "-h",
        "--help",
    ];
    const ALLOWED_PREFIXES: &[&str] = &[
        "--team=",
        "--pane=",
        "--to-name=",
        "--to-leader=",
        "--presentation-sink=",
        "--message-class=",
        "--case-id=",
    ];
    if let Some(flag) = args.iter().find(|arg| {
        arg.starts_with('-')
            && !ALLOWED.contains(&arg.as_str())
            && !ALLOWED_PREFIXES
                .iter()
                .any(|prefix| arg.starts_with(prefix))
    }) {
        return Err(CliError::Usage(format!(
            "unrecognized argument for `send`: {flag}"
        )));
    }
    Ok(())
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
        detail: parsed.detail,
    })
}

// Keep strict role CLI parsing separate from the legacy shared parser.
fn role_agent_args(args: &[String], add: bool) -> Result<(ParsedArgs, crate::lifecycle::role_config::RoleConfigPatch), CliError> {
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
        let (flag, inline) = argument.split_once('=').map_or((argument.as_str(), None), |(key, value)| (key, Some(value)));
        if !seen.insert(flag.to_string()) { return Err(CliError::Usage(format!("duplicate {flag}"))); }
        if matches!(flag, "--json" | "--allow-fresh" | "--force") {
            if inline.is_some() || (add && flag == "--allow-fresh") {
                return Err(CliError::Usage(format!("unsupported option: {argument}")));
            }
            forwarded.push(flag.to_string());
            index += 1;
            continue;
        }
        if !matches!(flag, "--provider" | "--model" | "--effort" | "--bypass" | "--prompt" | "--profile" | "--workspace" | "--team" | "--role-file")
            || (!add && flag == "--role-file") {
            return Err(CliError::Usage(format!("unknown option: {flag}")));
        }
        let value = if let Some(value) = inline { value.to_string() } else {
            index += 1;
            args.get(index).filter(|value| !value.starts_with("--"))
                .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))?.clone()
        };
        if value.trim().is_empty() { return Err(CliError::Usage(format!("empty value for {flag}"))); }
        match flag {
            "--provider" => {
                let provider = crate::provider::wire::parse_provider(&value)
                    .ok_or_else(|| CliError::Usage(format!("unknown provider: {value}")))?;
                role.provider = Some(crate::provider::wire::provider_wire(provider).to_string());
            }
            "--bypass" => role.bypass = Some(match value.as_str() {
                "true" => true, "false" => false,
                _ => return Err(CliError::Usage("--bypass requires true or false".into())),
            }),
            "--model" => role.model = Some(value),
            "--effort" => {
                if crate::model::enums::ProviderEffort::parse(&value).is_none() {
                    return Err(CliError::Usage(format!("unknown effort: {value}")));
                }
                role.effort = Some(value);
            }
            "--prompt" => role.prompt = Some(value),
            "--profile" => role.profile = Some(value),
            _ => { forwarded.push(flag.to_string()); forwarded.push(value); }
        }
        index += 1;
    }
    let parsed = parse_args(&forwarded);
    if add && parsed.role_file.is_none() {
        if role.provider.is_none() { return Err(CliError::Usage("add-agent requires --provider <name>".into())); }
        if role.bypass.is_none() { return Err(CliError::Usage("add-agent requires --bypass <true|false>".into())); }
    }
    if parsed.positionals.len() != 1 {
        return Err(CliError::Usage(if parsed.positionals.is_empty() { "missing agent" } else { "expected exactly one agent id" }.into()));
    }
    Ok((parsed, role))
}

#[cfg(test)]
mod role_cli_tests {
    use super::*;
    fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_string()).collect() }

    #[test]
    fn add_requires_decisions_without_a_template_but_accepts_file_input() {
        for args in [vec!["w"], vec!["w", "--provider", "pi"], vec!["w", "--bypass", "false"]] {
            assert!(role_agent_args(&strings(&args), true).is_err());
        }
        assert!(role_agent_args(&strings(&["w", "--role-file", "w.md"]), true).is_ok());
        let (parsed, _) = role_agent_args(&strings(&["w", "--role-file", "w.md", "--force"]), true).unwrap();
        assert!(parsed.force);
        assert!(role_agent_args(&strings(&["w", "--role-file", "w.md", "--force=true"]), true).is_err());
        assert!(role_agent_args(&strings(&["w", "--role-file", "w.md", "--allow-fresh"]), true).is_err());
        let (_, patch) = role_agent_args(&strings(&["w", "--provider=pi", "--bypass=false"]), true).unwrap();
        assert_eq!(patch.provider.as_deref(), Some("pi"));
        assert_eq!(patch.bypass, Some(false));
    }

    #[test]
    fn strict_role_options_reject_ambiguous_values() {
        for args in [
            vec!["w", "--bypass"], vec!["w", "--bypass", "yes"],
            vec!["w", "--no-bypass"], vec!["w", "--bypass", "true", "--bypass=false"],
            vec!["w", "--unknown", "x"], vec!["w", "extra"],
        ] { assert!(role_agent_args(&strings(&args), false).is_err()); }
        let (_, patch) = role_agent_args(&strings(&["w"]), false).unwrap();
        assert_eq!(patch, crate::lifecycle::role_config::RoleConfigPatch::default());
    }
}

fn start_agent_args(args: &[String], cwd: &Path) -> Result<StartAgentArgs, CliError> {
    let (parsed, role_config) = role_agent_args(args, false)?;
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
    if !parsed.discard_session { return Err(CliError::Usage("missing --discard-session；只有用户明确同意清除会话关联后才能添加此参数".to_string())); }
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

fn sessions_args(args: &[String], cwd: &Path) -> SessionsArgs {
    let parsed = parse_args(args);
    SessionsArgs {
        workspace: workspace(&parsed, cwd),
        json: parsed.json,
        team: parsed.team,
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
        proxy_mode: parsed.proxy_mode,
        json: parsed.json,
    })
}

fn results_args(args: &[String], cwd: &Path) -> Result<ResultsArgs, CliError> {
    if args
        .iter()
        .any(|arg| arg == "--to" || arg.starts_with("--to="))
    {
        return Err(CliError::Usage(
            "results does not accept --to; it is a read-only command".to_string(),
        ));
    }
    let parsed = parse_args(args);
    let case_id = option_value(args, "--case")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CliError::Usage("missing --case".to_string()))?;
    Ok(ResultsArgs {
        case_id,
        workspace: workspace(&parsed, cwd),
        team: parsed.team,
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
        assert_eq!(expected.len(), 30, "the public catalog is Human30");
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
            "default help must match the exact Human30 public spec set, not a slack threshold; got {visible:?}"
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
            "results", "wait", "attach-app-server-leader", "identity", "watch", "sessions",
            "validate", "preflight", "wait-ready", "e2e", "peek", "coordinator",
        ] {
            assert!(
                !visible.iter().any(|visible| visible == command),
                "`{command}` must stay hidden from default help"
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
            ("allow-peer-talk", CommandTier::Core),
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
                    "--model", "--effort", "--bypass", "--prompt", "--profile", "--provider",
                    "--workspace", "--team", "--allow-fresh", "--json",
                ][..],
            ),
            (
                "reset-agent",
                &[
                    "--workspace",
                    "--team",
                    "--discard-session",
                    "--json",
                ][..],
            ),
            (
                "add-agent",
                &[
                    "--role-file", "--provider", "--bypass", "--model", "--effort", "--prompt", "--profile",
                    "--force", "--workspace", "--team", "--json",
                ][..],
            ),
            (
                "fork-agent",
                &[
                    "--as",
                    "--label",
                    "--workspace",
                    "--team",
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
        for command in ["sessions", "wait-ready", "peek", "coordinator"] {
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
    fn worker_lifecycle_help_uses_authoritative_spec_usage_and_summary() {
        for name in ["start-agent", "add-agent"] {
            let spec = command_spec(name).expect("registered worker lifecycle command");
            let help = command_help(Some(name));
            assert!(help.contains(spec.usage));
            assert!(help.contains("做什么："));
            for flag in ["--model MODEL", "--effort LEVEL", "--bypass true|false", "--prompt TEXT", "--profile NAME", "--provider TOOL"] {
                assert!(help.contains(flag), "{name} help missing {flag}: {help}");
            }
            assert!(help.contains("下一步") && help.contains("team-agent send"));
        }
        assert!(!command_help(Some("start-agent")).contains("--force"));
        let add = command_help(Some("add-agent"));
        assert!(add.contains("[--role-file FILE]"));
        assert!(add.contains("provider") && add.contains("bypass") && add.contains("角色文件"));
        assert!(add.contains("冲突") && add.contains("拒绝"));
    }

    #[test]
    fn status_help_describes_brief_projection_and_unknown_boundary() {
        let help = command_help(Some("status"));
        for marker in [
            "name/provider/model/effort/runtime_status/activity/health/session_name/tmux_command",
            "--json 给程序读取",
            "unknown 表示证据不足",
            "tmux_command",
            "不增加字段",
        ] {
            assert!(help.contains(marker), "status help missing {marker}: {help}");
        }
        assert!(!help.contains("错误细分走 status --summary"));
    }

    #[test]
    fn models_defaults_provider_and_accepts_one_query_form() {
        let defaulted = models_args(&cli_argv(&["gpt", "5.6", "luna"])).unwrap();
        assert_eq!(defaulted.provider, "pi");
        assert_eq!(defaulted.search.as_deref(), Some("gpt 5.6 luna"));

        let cursor = models_args(&cli_argv(&[
            "--provider",
            "cursor_agent",
            "--search",
            "GPT Luna",
        ]))
        .unwrap();
        assert_eq!(cursor.provider, "cursor_agent");
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
        let args = send_args(
            &cli_argv(&["--pane", "%1596", "--to", "worker", "hello"]),
            &cwd,
        )
        .unwrap();
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message.contains("--pane") && message.contains("--to") && message.contains("不能") && message.contains("同时")),
            "expected --pane/--to mutual-exclusion usage error, got {err:?}"
        );

        let args = send_args(&cli_argv(&["--pane", "%1596"]), &cwd).unwrap();
        let err = cmd_send(&args).unwrap_err();
        assert!(
            matches!(err, CliError::Usage(ref message) if message == "--pane requires a non-empty message" || (message.contains("--pane") && message.contains("非空"))),
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
    }
}
