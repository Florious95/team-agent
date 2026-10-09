//!
//! cli · adapters — 每子命令的薄壳 `cmd_*`(commands.py)。委派 status/lifecycle/diagnose/
//! leader/messaging port,把委派结果包成 [`CmdResult`]。含逻辑的:`cmd_status`(三态互斥)、
//! `cmd_doctor`(gate/comms/fix-schema/cleanup-orphans 分派)。

use super::*;

/// `cmd_quick_start`(`commands.py:18`)。`--json` 或 `!ok` → 整 dict;否则 `result["summary"]`。
pub fn cmd_quick_start(args: &QuickStartArgs) -> Result<CmdResult, CliError> {
    #[cfg(unix)]
    crate::contract_runtime::config::read_team(args)
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut value = lifecycle_port::quick_start(
        &args.workspace,
        &args.agents_dir,
        args.name.as_deref(),
        args.team_id.as_deref(),
        args.yes,
        args.backend.as_deref(),
    )?;
    if let Some(result) = quick_start_config_guidance(&mut value, args) {
        return Ok(result);
    }
    append_send_guidance(&mut value, &args.workspace, args.team_id.as_deref());
    finish_quick_start(value, args)
}

fn finish_quick_start(mut value: Value, args: &QuickStartArgs) -> Result<CmdResult, CliError> {
    let readiness = value.get("readiness").and_then(Value::as_object);
    let all_resumable_have_session = readiness
        .and_then(|readiness| readiness.get("all_resumable_have_session"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let session_capture_incomplete = readiness
        .and_then(|readiness| readiness.get("session_capture_incomplete"))
        .and_then(Value::as_bool)
        .unwrap_or(!all_resumable_have_session);
    let readiness_ready = readiness
        .and_then(|readiness| readiness.get("ready"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    if !args.detail && value.get("ok").and_then(Value::as_bool) != Some(false) {
        lifecycle_port::compact_quick_start_value(&mut value);
    }
    if args.json
        || value.get("ok").and_then(Value::as_bool) == Some(false)
        || session_capture_incomplete
        || !readiness_ready
    {
        let mut result = CmdResult::from_json(value, args.json);
        if status.as_deref() == Some("pending_tool_load") {
            result.exit = ExitCode::Ok;
        }
        if !args.json {
            if let CmdOutput::Json(value) = &result.output {
                // Typed refusals often have no `error`. Preserve the complete
                // report instead of replacing its reason/actions with a guess.
                let safe = crate::redaction::redact_external_value(value);
                result.output = CmdOutput::Human(format!(
                    "quick-start report:\n{}",
                    serde_json::to_string_pretty(&safe)?
                ));
            }
        }
        Ok(result)
    } else {
        // E13:happy 人类路径必须带 attach_commands(json 路径 cli/mod.rs:1775 已有)。
        Ok(CmdResult::human(&quickstart_human(&value)))
    }
}

// Project only an actual compiler rejection; never reject a valid lifecycle fallback.
fn quick_start_config_guidance(value: &mut Value, args: &QuickStartArgs) -> Option<CmdResult> {
    if value.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = value.get("error")?.as_str()?;
    let compile = error.strip_prefix("spec compile failed: ")?;
    let detail = compile
        .strip_prefix("validation error: ")
        .unwrap_or(compile);
    let team_path = args.agents_dir.join("TEAM.md");
    let agents_path = args.agents_dir.join("agents");
    let missing_team = detail == format!("{}: missing TEAM.md", team_path.display())
        && team_path.try_exists().ok() == Some(false);
    let missing_roles = detail == format!("{}: missing agents directory", agents_path.display())
        || detail == format!("{}: no role docs found", agents_path.display());
    let missing_field = detail.split_once(": ").is_some_and(|(path, reason)| {
        Path::new(path).parent() == Some(agents_path.as_path())
            && (reason.contains("missing required field") || reason.contains("is required"))
    });
    if !missing_team && !(team_path.is_file() && (missing_roles || missing_field)) {
        return None;
    }
    let role_path = agents_path.join("worker.md");
    let mut retry = format!(
        "team-agent quick-start {} --workspace {}",
        shell_quote(&args.agents_dir.to_string_lossy()),
        shell_quote(&args.workspace.to_string_lossy())
    );
    if let Some(team) = args.team_id.as_deref() {
        retry.push_str(&format!(" --team {}", shell_quote(team)));
    }
    if let Some(name) = args.name.as_deref() {
        retry.push_str(&format!(" --name {}", shell_quote(name)));
    }
    if let Some(backend) = args.backend.as_deref() {
        retry.push_str(&format!(" --backend {}", shell_quote(backend)));
    }
    if args.yes {
        retry.push_str(" --yes");
    }
    if args.detail {
        retry.push_str(" --detail");
    }
    if args.json {
        retry.push_str(" --json");
    }
    let explanation = if missing_team {
        format!("Team configuration is missing: {}. No workers were started. Create the two files below; this command did not write them. Error: {error}", team_path.display())
    } else {
        format!("Worker configuration is incomplete: {detail}. Add the role directory or required fields. The examples below do not overwrite existing files. Error: {error}")
    };
    let next = format!("Check that Pi is installed and signed in; verify the model with team-agent models --provider pi.\nRun team-agent pi, then execute this command in that leader's tool context:\n{retry}");
    let mut templates =
        vec![serde_json::json!({"path": role_path, "content": super::emit::WORKER_TEMPLATE})];
    let mut human = explanation.clone();
    if missing_team {
        templates.insert(
            0,
            serde_json::json!({"path": team_path, "content": super::emit::TEAM_TEMPLATE}),
        );
        human.push_str(&format!(
            "\n\n{}:\n{}",
            team_path.display(),
            super::emit::TEAM_TEMPLATE
        ));
    }
    human.push_str(&format!(
        "\n\n{}:\n{}\nNext steps:\n{next}",
        role_path.display(),
        super::emit::WORKER_TEMPLATE
    ));
    value["action"] = serde_json::json!(explanation);
    value["next_actions"] =
        serde_json::json!(["team-agent models --provider pi", "team-agent pi", retry]);
    value["templates"] = serde_json::json!(templates);
    if args.json {
        Some(CmdResult::from_json(value.clone(), true))
    } else {
        let mut result = CmdResult::human(&human);
        result.exit = ExitCode::Error;
        Some(result)
    }
}

/// E13:quick-start "team 起了" 人类输出 = summary + attach 块。所有成功出口共用(别每分支手拷)。
/// attach_commands 缺/空 → 只 summary(向后兼容)。
fn quickstart_human(value: &Value) -> String {
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("Team started.");
    let attach: Vec<&str> = value
        .get("attach_commands")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let sends: Vec<&str> = value
        .get("send_commands")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = String::from(summary);
    if !attach.is_empty() {
        out.push_str("\n\nConnect to the team:");
        for cmd in attach {
            out.push_str("\n  ");
            out.push_str(cmd);
        }
    }
    if !sends.is_empty() {
        out.push_str("\n\nSend a task (replace the example message):");
        for cmd in sends {
            out.push_str("\n  ");
            out.push_str(cmd);
        }
    }
    append_reminder(out, crate::cli::QUICK_START_REMINDER)
}

pub(crate) fn append_send_guidance(value: &mut Value, workspace: &Path, team: Option<&str>) {
    let existing = value.get("summary").and_then(Value::as_str) == Some("existing runtime");
    if value.get("ok").and_then(Value::as_bool) != Some(true) && !existing {
        return;
    }
    let team = value
        .get("team")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(str::to_string)
        .or_else(|| team.map(str::to_string));
    let team = team.as_deref();
    let agents = value
        .get("agent_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let commands: Vec<Value> = agents
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|agent| send_command(agent, workspace, team).map(Value::String))
        .collect();
    if !commands.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.insert("send_commands".to_string(), Value::Array(commands));
        }
    }
}

#[cfg(test)]
pub(crate) fn split_shell_argv(command: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'') | (None, '"') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    argv.push(std::mem::take(&mut current));
                }
            }
            (_, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        argv.push(current);
    }
    argv
}

pub(crate) fn send_command(agent: &str, workspace: &Path, team: Option<&str>) -> Option<String> {
    let workspace = workspace.to_str()?;
    let mut command = format!(
        "team-agent send {} {} --workspace {}",
        shell_quote(agent),
        shell_quote("Complete the task and reply to the leader."),
        shell_quote(workspace)
    );
    if let Some(team) = team {
        command.push_str(" --team ");
        command.push_str(&shell_quote(team));
    }
    Some(command)
}

pub(crate) fn shell_quote(value: &str) -> String {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':')
    }) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn append_reminder(text: String, reminder: &str) -> String {
    if text.is_empty() {
        reminder.to_string()
    } else {
        format!("{text}\n{reminder}")
    }
}

/// `cmd_status`(`commands.py:90`)。CLI status 统一为只读九字段 brief；
/// `--json` 与人读路径共享 native tmux/process projection；`--summary`/`--detail` 保留参数兼容性但不暴露诊断。
#[cfg(test)]
pub(crate) fn cmd_status(args: &StatusArgs) -> Result<CmdResult, CliError> {
    cmd_status_for_team(args, args.team.as_deref())
}

pub fn cmd_status_for_team(args: &StatusArgs, team: Option<&str>) -> Result<CmdResult, CliError> {
    if args.summary && args.json {
        return Err(CliError::Runtime(
            "--summary and --json are mutually exclusive".to_string(),
        ));
    }
    if args.summary && args.agent.is_some() {
        return Err(CliError::Runtime(
            "status --summary does not accept an agent argument".to_string(),
        ));
    }
    // S4QR-001 (0.4.8): selected-team ambiguity gate. status is a selected-team
    // command: when the workspace has 2+ alive teams and no --team was passed,
    // refuse instead of silently defaulting to the active team. Uses the
    // read-only CommandScope helper so the gate cannot migrate/write state.
    if team.is_none() {
        let scope = crate::state::paths::CommandScope::resolve_readonly(&args.workspace, None);
        if scope.is_ambiguous() {
            let candidates: Vec<String> = scope.candidates().to_vec();
            let message = format!(
                "status: workspace has multiple alive teams ({}); pass `--team <key>` to choose one",
                candidates.join(", ")
            );
            return Err(CliError::Usage(message));
        }
    }
    let selected = crate::state::selector::resolve_active_team_readonly(
        &args.workspace,
        team,
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    if let Some(agent) = args.agent.as_deref() {
        if !status_port::registered_agent_exists(&selected.state, agent) {
            return Err(CliError::Runtime(format!("unknown agent id: {agent}")));
        }
    }
    // Status brief is deliberately independent of the legacy RuntimeSnapshot:
    // it performs one native tmux/process sample and exposes exactly nine
    // fields. `--summary` and `--detail` remain parser-compatible but do not
    // re-enable history, runtime diagnostics, or reminder text.
    if args.json {
        return Ok(CmdResult::from_json(
            status_port::status_brief_scoped(
                &selected.run_workspace,
                &selected.state,
                args.agent.as_deref(),
            ),
            true,
        ));
    }
    Ok(CmdResult::human(status_port::format_status_brief(
        &selected.run_workspace,
        &selected.state,
        args.agent.as_deref(),
    )))
}

pub fn cmd_wait(args: &WaitArgs) -> Result<CmdResult, CliError> {
    let result = messaging::wait_for_result(&args.workspace, &args.task_id)?;
    Ok(CmdResult::from_json(result.to_json(), args.json))
}

/// `cmd_approvals`(`commands.py:112`)。
pub fn cmd_approvals(args: &ApprovalsArgs) -> Result<CmdResult, CliError> {
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let value = status_port::approvals_scoped(
        &selected.run_workspace,
        &selected.state,
        args.agent.as_deref(),
        args.json,
    )?;
    if args.json {
        Ok(CmdResult::from_json(value, true))
    } else {
        Ok(CmdResult::human(status_port::format_approvals(&value)))
    }
}

/// `cmd_inbox`(`commands.py:137`)。
pub fn cmd_inbox(args: &InboxArgs) -> Result<CmdResult, CliError> {
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let value = status_port::inbox(
        &selected.run_workspace,
        &args.agent,
        args.limit,
        Some(&selected.team_key),
    )?;
    if args.json {
        Ok(CmdResult::from_json(value, true))
    } else {
        Ok(CmdResult::human(format_inbox_human(&args.agent, &value)))
    }
}

const INBOX_LINE_LIMIT_BYTES: usize = 160;

fn clean_inbox_field(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

fn truncate_inbox_line(line: &str) -> String {
    if line.len() <= INBOX_LINE_LIMIT_BYTES {
        return line.to_string();
    }
    let budget = INBOX_LINE_LIMIT_BYTES.saturating_sub("…".len());
    let mut end = 0;
    for (index, ch) in line.char_indices() {
        let next = index + ch.len_utf8();
        if next > budget {
            break;
        }
        end = next;
    }
    format!("{}…", &line[..end])
}

fn format_inbox_human(agent: &str, value: &Value) -> String {
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        return format!("{agent}: no messages");
    }
    messages
        .iter()
        .map(|message| {
            let id = clean_inbox_field(
                message
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
            );
            let sender =
                clean_inbox_field(message.get("sender").and_then(Value::as_str).unwrap_or("-"));
            let recipient = clean_inbox_field(
                message
                    .get("recipient")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
            );
            let status =
                clean_inbox_field(message.get("status").and_then(Value::as_str).unwrap_or("-"));
            let time = clean_inbox_field(
                message
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or("-"),
            );
            let summary =
                clean_inbox_field(message.get("summary").and_then(Value::as_str).unwrap_or(""));
            truncate_inbox_line(&format!(
                "[{id}] [{sender} -> {recipient}] [{status}] [{time}] [{summary}]"
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `cmd_takeover`(`commands.py:152`)。
pub fn cmd_takeover(args: &TakeoverArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        leader_port::takeover(&args.workspace, args.team.as_deref(), args.confirm)?,
        args.json,
    ))
}

/// `cmd_claim_leader`(`commands.py:156`)。
pub fn cmd_claim_leader(args: &ClaimLeaderArgs) -> Result<CmdResult, CliError> {
    let mut value = leader_port::claim_leader(&args.workspace, args.team.as_deref(), args.confirm)?;
    if !args.detail {
        leader_port::compact_lease_value(&mut value);
    }
    Ok(CmdResult::from_json(value, args.json))
}

/// `cmd_shutdown`(`commands.py:340`)。
pub fn cmd_shutdown(args: &ShutdownArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::shutdown(&args.workspace, args.keep_logs, args.team.as_deref())?,
        args.json,
    ))
}

/// `cmd_restart`(`commands.py:344`)。
pub fn cmd_restart(args: &RestartArgs) -> Result<CmdResult, CliError> {
    let mut value = lifecycle_port::restart(
        &args.workspace,
        args.allow_fresh,
        args.team.as_deref(),
        args.session_converge_deadline_ms,
    )?;
    if !args.detail {
        lifecycle_port::compact_restart_value(&mut value);
    }
    Ok(CmdResult::from_json(value, args.json))
}

/// `cmd_start_agent`(`commands.py:348`)。
pub fn cmd_start_agent(args: &StartAgentArgs) -> Result<CmdResult, CliError> {
    let mut value = lifecycle_port::start_agent(
        &args.workspace,
        &args.agent,
        args.force,
        args.allow_fresh,
        args.team.as_deref(),
        &args.role_config,
    )?;
    if value.get("agent_ids").is_none() {
        if let Some(object) = value.as_object_mut() {
            object.insert("agent_ids".to_string(), json!([args.agent.as_str()]));
        }
    }
    append_send_guidance(&mut value, &args.workspace, args.team.as_deref());
    Ok(CmdResult::from_json(value, args.json))
}

/// `cmd_stop_agent`(`commands.py:359`)。
pub fn cmd_stop_agent(args: &StopAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::stop_agent(&args.workspace, &args.agent, args.team.as_deref())?,
        args.json,
    ))
}

/// `cmd_reset_agent`(`commands.py:363`)。
pub fn cmd_reset_agent(args: &ResetAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::reset_agent(
            &args.workspace,
            &args.agent,
            args.discard_session,
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

/// `cmd_add_agent`(`commands.py:373`)。
pub fn cmd_add_agent(args: &AddAgentArgs) -> Result<CmdResult, CliError> {
    let mut value = lifecycle_port::add_agent(
        &args.workspace,
        &args.agent,
        &args.role_file,
        args.team.as_deref(),
        args.force,
        &args.role_config,
    )?;
    if value.get("agent_ids").is_none() {
        if let Some(object) = value.as_object_mut() {
            object.insert("agent_ids".to_string(), json!([args.agent.as_str()]));
        }
    }
    append_send_guidance(&mut value, &args.workspace, args.team.as_deref());
    Ok(CmdResult::from_json(value, args.json))
}

/// `cmd_fork_agent`(`commands.py:383`)。
pub fn cmd_fork_agent(args: &ForkAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::fork_agent(
            &args.workspace,
            &args.source_agent,
            &args.as_agent,
            args.label.as_deref(),
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

pub fn cmd_clone_agent(args: &CloneAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::clone_agent(
            &args.workspace,
            &args.source_agent,
            &args.as_agent,
            args.label.as_deref(),
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

/// `cmd_remove_agent`(`commands.py:394`)。
pub fn cmd_remove_agent(args: &RemoveAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::remove_agent(
            &args.workspace,
            &args.agent,
            args.from_spec,
            args.confirm,
            args.force,
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

/// `doctor` is the single health/diagnostic pipeline.
pub fn cmd_doctor(args: &DoctorArgs) -> Result<CmdResult, CliError> {
    if let Some(DoctorGate::Unknown(raw)) = &args.gate {
        let value = json!({
            "ok": false,
            "issues": ["unknown_gate"],
            "suggested_repairs": [{"action": "use --gate orphans or --gate comms", "issue": "unknown_gate"}],
            "status": "unknown_gate",
            "gate": raw,
        });
        return Ok(crate::cli::triage::report(value, args.json, "doctor"));
    }
    if args.fix && args.gate.is_none() {
        let value = json!({
            "ok": false,
            "error": "--fix requires --gate",
            "issues": ["fix_requires_gate"],
            "suggested_repairs": [{"issue": "fix_requires_gate", "action": "add --gate orphans|comms"}],
        });
        return Ok(crate::cli::triage::report(value, args.json, "doctor"));
    }

    let explicit_comms = args.comms || matches!(args.gate, Some(DoctorGate::Comms));
    let default_report =
        !explicit_comms && args.gate.is_none() && !args.cleanup_orphans && !args.fix_schema;
    let mut value = if explicit_comms {
        crate::diagnose::comms::doctor_comms_json(
            &args.workspace,
            args.team.as_deref(),
            Some("comms"),
        )?
    } else if matches!(args.gate, Some(DoctorGate::Orphans)) {
        crate::diagnose::orphans::orphan_gate_json(&args.workspace, args.fix, args.confirm)?
    } else if args.cleanup_orphans {
        crate::diagnose::orphans::cleanup_orphans_json(&args.workspace, args.confirm)?
    } else if args.fix_schema {
        diagnose_port::fix_schema(&args.workspace)?
    } else {
        let value = diagnose_port::doctor(&args.workspace, args.spec.as_deref())?;
        unified_default_doctor_report(args, value)
    };

    if explicit_comms {
        if let Some(object) = value.as_object_mut() {
            // The comms probe uses a random run id internally for its disposable
            // fixture; the public doctor/diagnose report must be alias-stable.
            object.insert("run_id".to_string(), Value::String("doctor".to_string()));
        }
    }
    if !default_report {
        finalize_doctor_report(&mut value, false);
    }
    let result = crate::cli::triage::report(value, args.json, "doctor");
    Ok(result)
}

fn unified_default_doctor_report(args: &DoctorArgs, mut value: Value) -> Value {
    let team_key = args
        .team
        .as_deref()
        .filter(|team| !team.is_empty())
        .unwrap_or("current");
    let requested_workspace = args.workspace.to_string_lossy().to_string();
    let mut runtime = json!({
        "status": "not_present",
        "workspace": requested_workspace,
        "run_workspace": Value::Null,
        "team_key": team_key,
        "session_name": Value::Null,
        "leader_receiver": Value::Null,
        "agent_count": 0,
        "message_count": 0,
        "result_count": 0,
    });
    // Resolve explicit selectors and nested team directories before deciding
    // that runtime inspection is inapplicable.
    let selected = crate::state::selector::resolve_active_team_readonly(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    );
    let run_workspace = selected
        .as_ref()
        .map(|selected| selected.run_workspace.clone())
        .unwrap_or_else(|_| {
            crate::model::paths::canonical_run_workspace(&args.workspace)
                .unwrap_or_else(|_| args.workspace.clone())
        });
    let runtime_present = selected.as_ref().is_ok_and(|selected| {
        crate::cli::diagnose::workspace_has_existing_team_runtime(
            &selected.run_workspace,
            &selected.team_key,
        )
    });
    let db_present = crate::model::paths::runtime_dir(&run_workspace)
        .join("team.db")
        .exists();
    // One read-only observation supplies both issue classification and details.
    // A physical database must be checked even before state.json is created.
    let health = (runtime_present || db_present).then(|| {
        crate::coordinator::coordinator_health_read_only(&crate::coordinator::WorkspacePath::new(
            run_workspace.clone(),
        ))
    });
    match selected {
        Ok(selected) => {
            if let Some(health) = health.as_ref().filter(|_| runtime_present) {
                let state = selected.state;
                let backend: Box<dyn crate::transport::Transport> =
                    match crate::transport_factory::resolve_read_only_transport(
                        &selected.run_workspace,
                        Some(&state),
                        crate::transport_factory::TransportPurpose::Diagnose,
                    ) {
                        Ok(resolved) => resolved.backend,
                        Err(_) => Box::new(crate::tmux_backend::TmuxBackend::for_workspace(
                            &selected.run_workspace,
                        )),
                    };
                let (issues, repairs) = crate::cli::diagnose::diagnose_runtime_for_workspace(
                    &selected.run_workspace,
                    &state,
                    backend.as_ref(),
                    Some(selected.team_key.as_str()),
                    health,
                );
                merge_report_array(&mut value, "issues", &issues);
                merge_report_array(&mut value, "suggested_repairs", &repairs);
                let run_workspace = selected.run_workspace.to_string_lossy().to_string();
                runtime = json!({
                    "status": "present",
                    "workspace": args.workspace.to_string_lossy().to_string(),
                    "run_workspace": run_workspace,
                    "team_key": selected.team_key,
                    "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
                    "leader_receiver": state.get("leader_receiver").cloned().unwrap_or(Value::Null),
                    "agent_count": state.get("agents").and_then(Value::as_object).map_or(0, serde_json::Map::len),
                    "message_count": count_dir_entries(&selected.run_workspace.join(".team").join("messages")),
                    "result_count": count_dir_entries(&selected.run_workspace.join(".team").join("results")),
                });
            }
        }
        Err(error) => {
            runtime["status"] = Value::String("unresolved".to_string());
            append_issue_repair(
                &mut value,
                "runtime_selection_failed",
                format!("unable to select runtime team: {error}"),
            );
        }
    }
    if let Some(health) = health {
        if db_present && !health.schema.ok {
            append_issue_repair(
                &mut value,
                "coordinator_schema_incompatible",
                "team-agent doctor --fix-schema --json".to_string(),
            );
        }
        value["coordinator"] = diagnose_port::coordinator_health_value(health);
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("runtime".to_string(), runtime);
        object.insert(
            "event_log".to_string(),
            Value::String(
                run_workspace
                    .join(".team")
                    .join("logs")
                    .join("events.jsonl")
                    .to_string_lossy()
                    .to_string(),
            ),
        );
    }
    finalize_doctor_report(&mut value, true);
    value
}

fn merge_report_array(report: &mut Value, key: &str, incoming: &Value) {
    let Some(items) = incoming.as_array() else {
        return;
    };
    let Some(object) = report.as_object_mut() else {
        return;
    };
    let target = object
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !target.is_array() {
        *target = Value::Array(Vec::new());
    }
    let target = target
        .as_array_mut()
        .expect("report array normalized before merge");
    for item in items {
        let identity = report_item_identity(item);
        if !target
            .iter()
            .any(|existing| report_item_identity(existing) == identity)
        {
            target.push(item.clone());
        }
    }
}

fn report_item_identity(item: &Value) -> String {
    if let Some(text) = item.as_str() {
        return text.to_string();
    }
    let primary = ["id", "code", "issue", "action", "reason", "message"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str));
    let scope = ["workspace", "team", "agent", "path", "rule"]
        .iter()
        .filter_map(|key| item.get(*key).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let line = item.get("line").and_then(Value::as_u64);
    if primary.is_some() || !scope.is_empty() || line.is_some() {
        if scope.is_empty() && line.is_none() {
            return primary.unwrap_or("object").to_string();
        }
        return format!(
            "{}|{}|{}",
            primary.unwrap_or("object"),
            scope.join("|"),
            line.map_or_else(String::new, |line| line.to_string())
        );
    }
    item.to_string()
}

fn append_issue_repair(report: &mut Value, issue: &str, repair: String) {
    merge_report_array(report, "issues", &json!([issue]));
    merge_report_array(
        report,
        "suggested_repairs",
        &json!([{"issue": issue, "action": repair}]),
    );
}

fn finalize_doctor_report(report: &mut Value, default_report: bool) {
    let Some(object) = report.as_object_mut() else {
        *report = json!({
            "ok": false,
            "issues": ["invalid_doctor_report"],
            "suggested_repairs": [{"issue": "invalid_doctor_report"}],
        });
        return;
    };
    let runtime_status = object
        .get("runtime")
        .and_then(|runtime| runtime.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("not_run")
        .to_string();
    let mut issues = object
        .remove("issues")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let mut repairs = object
        .remove("suggested_repairs")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let mut add = |issue: Value, repair: Option<Value>| {
        if !issues
            .iter()
            .any(|existing| report_item_identity(existing) == report_item_identity(&issue))
        {
            issues.push(issue);
        }
        if let Some(repair) = repair {
            if !repairs
                .iter()
                .any(|existing| report_item_identity(existing) == report_item_identity(&repair))
            {
                repairs.push(repair);
            }
        }
    };

    if !default_report && object.get("ok").and_then(Value::as_bool) == Some(false) {
        let status = object
            .get("status")
            .and_then(Value::as_str)
            .or_else(|| object.get("reason").and_then(Value::as_str))
            .unwrap_or("doctor_failed");
        let issue = if status == "failed" {
            object.get("gate").and_then(Value::as_str).map_or_else(
                || "doctor_failed".to_string(),
                |gate| format!("{gate}_failed"),
            )
        } else if status.ends_with("_failed") || status == "refused" {
            status.to_string()
        } else {
            format!("{status}_failed")
        };
        let repair = object
            .get("action")
            .or_else(|| object.get("next_action"))
            .or_else(|| object.get("reason"))
            .cloned();
        add(
            Value::String(issue),
            repair.map(|value| json!({"action": value})),
        );
    }
    if object
        .get("workspace")
        .and_then(Value::as_str)
        .is_some_and(|workspace| !std::path::Path::new(workspace).is_dir())
    {
        add(
            json!("invalid_workspace"),
            Some(json!({"issue": "invalid_workspace", "action": "use an existing workspace"})),
        );
    }
    if let Some(profile) = object.get("profile_smoke") {
        if profile.get("ok").and_then(Value::as_bool) == Some(false)
            && profile.get("status").and_then(Value::as_str) != Some("legacy_team_invalid")
        {
            add(
                json!("profile_smoke_failed"),
                profile
                    .get("next_action")
                    .cloned()
                    .map(|action| json!({"issue": "profile_smoke_failed", "action": action})),
            );
        }
    }
    if let Some(grok) = object.get("grok_slot") {
        if grok.get("readable").and_then(Value::as_bool) == Some(false)
            || grok.get("consistent").and_then(Value::as_bool) == Some(false)
        {
            add(
                json!("grok_slot_mismatch"),
                grok.get("reason")
                    .cloned()
                    .map(|reason| json!({"issue": "grok_slot_mismatch", "action": reason})),
            );
        }
    }
    if let Some(findings) = object
        .get("secret_scan")
        .and_then(|scan| scan.get("findings"))
        .and_then(Value::as_array)
    {
        for finding in findings {
            let Some(finding_object) = finding.as_object() else {
                continue;
            };
            let Some(rule) = finding_object.get("rule").and_then(Value::as_str) else {
                continue;
            };
            let path = finding_object
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let line = finding_object
                .get("line")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let issue = json!({
                "id": "secret_scan_finding",
                "rule": rule,
                "path": path,
                "line": line,
            });
            add(
                issue,
                Some(json!({
                    "issue": "secret_scan_finding",
                    "rule": rule,
                    "path": path,
                    "line": line,
                    "action": format!("remove secret finding at {path}:{line}"),
                })),
            );
        }
    }
    if runtime_status == "unresolved" {
        add(
            json!("runtime_selection_failed"),
            Some(
                json!({"issue": "runtime_selection_failed", "action": "select an existing Team runtime"}),
            ),
        );
    }
    if runtime_status == "present"
        && object
            .get("coordinator")
            .and_then(|coordinator| coordinator.get("ok"))
            .and_then(Value::as_bool)
            == Some(false)
    {
        let reason = object
            .get("coordinator")
            .and_then(|coordinator| coordinator.get("metadata_mismatch_reason"))
            .cloned()
            .unwrap_or_else(|| json!("coordinator unavailable"));
        add(
            json!("coordinator_unavailable"),
            Some(json!({"issue": "coordinator_unavailable", "action": reason})),
        );
    }
    object.insert("issues".to_string(), Value::Array(issues.clone()));
    object.insert("suggested_repairs".to_string(), Value::Array(repairs));
    object.insert("ok".to_string(), Value::Bool(issues.is_empty()));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{
        append_send_guidance, finish_quick_start, format_inbox_human, quickstart_human,
        send_command, split_shell_argv,
    };
    use crate::cli::{CmdOutput, ExitCode, QuickStartArgs};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    #[test]
    fn inbox_human_output_sanitizes_controls_and_caps_each_line() {
        let value = json!({
            "messages": [{
                "message_id": "msg-\u{001b}[31m".repeat(20),
                "sender": "sender\t\u{001b}[2J".repeat(20),
                "recipient": "worker".repeat(20),
                "status": "delivered",
                "created_at": "2026-09-19T00:00:00Z",
                "summary": "中".repeat(120)
            }]
        });
        let output = format_inbox_human("worker", &value);
        for line in output.lines() {
            assert!(line.len() <= 160, "line is {} bytes: {line:?}", line.len());
            assert!(
                !line.chars().any(char::is_control),
                "controls leaked: {line:?}"
            );
        }
    }

    // E13:happy 人类输出必须带 attach 块(此前 else 分支只打 summary 丢 attach_commands)。
    #[test]
    fn e13_quickstart_human_includes_attach_commands() {
        let value = json!({
            "summary": "team started",
            "attach_commands": [
                "tmux -S /tmp/ta-x attach -t team-y:w1",
                "tmux -S /tmp/ta-x attach -t team-y:w2",
            ],
        });
        let out = quickstart_human(&value);
        assert!(
            out.contains("team started"),
            "must show startup summary; got {out}"
        );
        assert!(
            out.contains("Connect to the team:"),
            "must render attach block; got {out}"
        );
        assert!(
            out.contains("team-y:w1") && out.contains("team-y:w2"),
            "must list each attach cmd; got {out}"
        );
        assert!(
            out.ends_with(crate::cli::QUICK_START_REMINDER),
            "must append harness reminder; got {out}"
        );
    }

    #[test]
    fn reminders_only_recommend_supported_result_commands() {
        let reminder = crate::cli::QUICK_START_REMINDER;
        assert!(reminder.contains("team-agent status"));
        assert!(reminder.contains("team-agent inbox"));
        for text in [
            reminder,
            crate::cli::STATUS_REMINDER,
            crate::cli::SEND_REMINDER,
            &quickstart_human(&json!({"summary": "team started"})),
        ] {
            for hidden in ["collect", "team-agent results", "team-agent wait"] {
                assert!(
                    !text.contains(hidden),
                    "private/removed CLI command leaked: {text}"
                );
            }
        }
    }

    #[test]
    fn e13_quickstart_human_summary_only_when_no_attach() {
        let value = json!({"summary": "quick-start complete"});
        assert_eq!(
            quickstart_human(&value),
            format!("quick-start complete\n{}", crate::cli::QUICK_START_REMINDER)
        );
        // 空数组也只显示人类摘要与下一步，不凭空编造连接方式。
        let value2 = json!({"summary": "s", "attach_commands": []});
        assert_eq!(
            quickstart_human(&value2),
            format!("s\n{}", crate::cli::QUICK_START_REMINDER)
        );
    }

    #[test]
    fn send_command_message_is_one_argv_token() {
        let command = send_command(
            "worker name; echo unsafe",
            Path::new("/tmp/my workspace"),
            Some("team-a"),
        )
        .unwrap();
        let argv = split_shell_argv(&command);
        assert_eq!(
            argv,
            [
                "team-agent",
                "send",
                "worker name; echo unsafe",
                "Complete the task and reply to the leader.",
                "--workspace",
                "/tmp/my workspace",
                "--team",
                "team-a"
            ]
        );
    }

    #[test]
    fn send_guidance_is_copyable_and_preserves_explicit_scope() {
        let command = send_command(
            "worker name; echo unsafe",
            Path::new("/tmp/my workspace"),
            Some("team-a"),
        )
        .unwrap();
        assert_eq!(
            command,
            "team-agent send 'worker name; echo unsafe' 'Complete the task and reply to the leader.' --workspace '/tmp/my workspace' --team team-a"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_workspace_fails_closed_without_send_guidance() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let workspace = PathBuf::from(OsString::from_vec(b"/tmp/workspace-\xff".to_vec()));
        assert!(send_command("worker", &workspace, None).is_none());
        let mut value = json!({"ok": true, "agent_ids": ["worker"]});
        append_send_guidance(&mut value, &workspace, None);
        assert!(value.get("send_commands").is_none());
    }

    #[test]
    fn quick_start_guidance_lists_every_known_agent_without_guessing() {
        let mut value = json!({"ok": true, "agent_ids": ["sol", "luna"]});
        append_send_guidance(&mut value, Path::new("/tmp/ws"), Some("team"));
        let commands = value
            .get("send_commands")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(commands.len(), 2);
        assert!(commands[0].as_str().unwrap().contains("send sol"));
        assert!(commands[1].as_str().unwrap().contains("send luna"));
    }

    #[test]
    fn error_results_do_not_get_send_guidance() {
        let mut value = json!({"ok": false, "agent_ids": ["worker"]});
        append_send_guidance(&mut value, Path::new("/tmp/ws"), None);
        assert!(value.get("send_commands").is_none());
    }

    #[test]
    fn existing_runtime_gets_canonical_send_guidance() {
        let mut value = json!({
            "ok": false,
            "summary": "existing runtime",
            "agent_ids": ["worker"]
        });
        append_send_guidance(&mut value, Path::new("/tmp/ws"), Some("team-a"));
        let command = value
            .pointer("/send_commands/0")
            .and_then(Value::as_str)
            .unwrap();
        assert!(command.contains("send worker"));
        assert!(command.contains("--team team-a"));
        assert_eq!(
            split_shell_argv(command),
            [
                "team-agent",
                "send",
                "worker",
                "Complete the task and reply to the leader.",
                "--workspace",
                "/tmp/ws",
                "--team",
                "team-a",
            ]
        );
    }

    fn quick_start_args(as_json: bool) -> QuickStartArgs {
        QuickStartArgs {
            workspace: PathBuf::from("/tmp/first-launch"),
            agents_dir: PathBuf::from("/tmp/first-launch/.team/current"),
            name: None,
            team_id: None,
            yes: true,
            json: as_json,
            detail: false,
            backend: None,
        }
    }

    #[test]
    fn quick_start_human_preserves_typed_refusals_without_error_field() {
        for status in [
            "existing_runtime",
            "preflight_blocked",
            "leader_binding_refused",
        ] {
            let value = json!({
                "ok": false,
                "status": status,
                "reason": "the exact underlying refusal",
                "summary": "creation refused",
                "blockers": [{"stage": "identity", "reason": "owner mismatch"}],
                "next_actions": ["team-agent restart --workspace /tmp/first-launch --team current"],
                "state_path": "/tmp/first-launch/.team/runtime/state.json",
                "readiness": {"reason": "underlying readiness evidence"}
            });
            let result = finish_quick_start(value.clone(), &quick_start_args(false)).unwrap();
            assert_eq!(result.exit, ExitCode::Error);
            let CmdOutput::Human(text) = result.output else {
                panic!("expected human report")
            };
            let rendered: Value =
                serde_json::from_str(text.strip_prefix("quick-start report:\n").unwrap()).unwrap();
            assert_eq!(rendered, value, "human projection lost fields for {status}");
            assert!(
                !text.contains("team-agent doctor"),
                "must not replace actual actions"
            );
            let json_result = finish_quick_start(value.clone(), &quick_start_args(true)).unwrap();
            assert_eq!(json_result.exit, ExitCode::Error);
            assert_eq!(json_result.output, CmdOutput::Json(value));
        }
    }

    #[test]
    fn quick_start_human_preserves_original_error_and_actions() {
        let value = json!({
            "ok": false,
            "error": "database initialization failed at /tmp/first-launch/.team/team.db: permission denied",
            "action": "repair directory permissions",
            "next_actions": ["inspect the exact database path"]
        });
        let result = finish_quick_start(value.clone(), &quick_start_args(false)).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Human(text) = result.output else {
            panic!("expected human report")
        };
        let rendered: Value =
            serde_json::from_str(text.strip_prefix("quick-start report:\n").unwrap()).unwrap();
        assert_eq!(rendered, value);
    }

    #[test]
    fn pending_tool_load_is_not_reported_as_failed_team_creation() {
        for as_json in [false, true] {
            let value = json!({
                "ok": true,
                "status": "pending_tool_load",
                "summary": "Team started; worker tool loading is not yet verified",
                "readiness": {"ready": false},
                "next_actions": ["send a task and wait for the actual reply"]
            });
            let result = finish_quick_start(value, &quick_start_args(as_json)).unwrap();
            assert_eq!(result.exit, ExitCode::Ok);
        }
    }
}
