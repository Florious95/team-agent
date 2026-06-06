//! cli · adapters — 每子命令的薄壳 `cmd_*`(commands.py)。委派 status/lifecycle/diagnose/
//! leader/messaging port,把委派结果包成 [`CmdResult`]。含逻辑的:`cmd_status`(三态互斥)、
//! `cmd_doctor`(gate/comms/fix-schema/cleanup-orphans 分派)。

use super::*;
use crate::transport::Transport;

const INIT_SPEC_TEMPLATE: &str = include_str!("../model/testdata/team.spec.yaml");
const INIT_STATE_TEMPLATE: &str = r#"# Team State

Updated: not launched

## Objective

Pending.

## Team

- Name: pending
- Runtime session: pending

## Agents

- Pending launch.

## Task Graph

- Pending task graph.

## Latest Results

- None.

## Blockers

- None.

## Next Step

- Run `team-agent validate team.spec.yaml`, review permissions, then run `team-agent launch team.spec.yaml --yes`.
"#;

pub fn cmd_init(args: &InitArgs) -> Result<CmdResult, CliError> {
    let team_root = args.workspace.join(".team");
    let spec_path = team_root.join("current").join("team.spec.yaml");
    let state_path = args.workspace.join("team_state.md");
    if spec_path.exists() && !args.force {
        return Err(CliError::Runtime(format!(
            "{} already exists; pass --force to overwrite",
            spec_path.display()
        )));
    }
    for dir in [
        team_root.clone(),
        team_root.join("current"),
        team_root.join("runtime"),
        team_root.join("logs"),
        team_root.join("messages"),
        team_root.join("artifacts"),
    ] {
        std::fs::create_dir_all(&dir)?;
    }
    std::fs::write(&spec_path, INIT_SPEC_TEMPLATE)?;
    if args.force || !state_path.exists() {
        std::fs::write(&state_path, INIT_STATE_TEMPLATE)?;
    }
    crate::event_log::EventLog::new(&args.workspace).write(
        "init",
        json!({
            "spec_path": spec_path.to_string_lossy().to_string(),
            "state_path": state_path.to_string_lossy().to_string(),
        }),
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "spec": spec_path.to_string_lossy().to_string(),
            "state": state_path.to_string_lossy().to_string(),
        }),
        args.json,
    ))
}

/// `cmd_quick_start`(`commands.py:18`)。`--json` 或 `!ok` → 整 dict;否则 `result["summary"]`。
pub fn cmd_quick_start(args: &QuickStartArgs) -> Result<CmdResult, CliError> {
    let value = lifecycle_port::quick_start(
        &args.agents_dir,
        &args.agents_dir,
        args.name.as_deref(),
        args.team_id.as_deref(),
        args.yes,
        args.fresh,
    )?;
    if args.json || value.get("ok").and_then(Value::as_bool) == Some(false) {
        Ok(CmdResult::from_json(value, args.json))
    } else {
        Ok(CmdResult::human(
            value
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("quick-start complete"),
        ))
    }
}

/// `cmd_compile`(`commands.py:42`)。
pub fn cmd_compile(args: &CompileArgs) -> Result<CmdResult, CliError> {
    let spec = crate::compiler::compile_team(&args.team)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    std::fs::write(&args.out, crate::model::yaml::dumps(&spec))?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "team_dir": args.team.to_string_lossy().to_string(),
            "out": args.out.to_string_lossy().to_string(),
            "agents": compiled_agent_ids_for_cli(&spec),
        }),
        args.json,
    ))
}

/// `cmd_status`(`commands.py:90`)。三态:`--summary`(xor json,xor agent)→五行文本;
/// `--json`→`status_port::status(compact=!detail)`;else→`status_port::format_status(agent)`。
pub fn cmd_status(args: &StatusArgs) -> Result<CmdResult, CliError> {
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
    let selected = match crate::state::selector::resolve_active_team(
        &args.workspace,
        None,
        crate::state::selector::SelectorMode::RuntimeOnly,
    ) {
        Ok(selected) => selected,
        Err(error) => {
            return Ok(CmdResult::from_json(
                status_selector_error_payload(&error.to_string(), &args.workspace),
                args.json,
            ));
        }
    };
    if args.summary {
        let value = status_port::status(&selected.run_workspace, true, false)?;
        return Ok(CmdResult::human(format_status_summary(&value)));
    }
    if args.json {
        let value = status_port::status(&selected.run_workspace, status_compact_flag(args.detail), args.detail)?;
        return Ok(CmdResult::from_json(value, true));
    }
    Ok(CmdResult::human(status_port::format_status(
        &selected.run_workspace,
        args.agent.as_deref(),
    )?))
}

fn status_selector_error_payload(error: &str, workspace: &Path) -> Value {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.6f");
    let log_path = std::env::temp_dir()
        .join("team-agent")
        .join("cli-errors")
        .join(format!("status-{stamp}.log"));
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&log_path, format!("{error}\n"));
    json!({
        "ok": false,
        "error": error,
        "action": "run `team-agent doctor` or inspect the log path shown here",
        "log": log_path.to_string_lossy().to_string(),
        "workspace": workspace.to_string_lossy().to_string(),
    })
}

/// `cmd_watch`(`commands.py:103`)。委派 `coordinator::run_watch`;KeyboardInterrupt/正常 → `SystemExit(0)`。
/// 返回 [`CmdResult::none`](不经 emit)。
pub fn cmd_watch(args: &WatchArgs) -> Result<CmdResult, CliError> {
    let workspace = crate::coordinator::WorkspacePath::new(args.workspace.clone());
    #[cfg(not(test))]
    {
        let mut output = |line: &str| {
            println!("{line}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        };
        crate::coordinator::run_watch(&workspace, args.team.as_deref(), 1.0, &mut output)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        Ok(CmdResult::none())
    }
    #[cfg(test)]
    {
    let store = crate::message_store::MessageStore::open(&args.workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut cursor = crate::coordinator::WatchCursor::default();
    let lines = crate::coordinator::collect_watch_lines(
        &workspace,
        &mut cursor,
        &store,
        args.team.as_deref(),
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(CmdResult::human(lines.join("\n")))
    }
}

/// `cmd_sessions`(`parser.py:230`)。
pub fn cmd_sessions(args: &SessionsArgs) -> Result<CmdResult, CliError> {
    let state = crate::state::persist::load_runtime_state(&args.workspace)?;
    let spec = load_team_spec_optional(&args.workspace, &state)?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "sessions": sessions_overview(&state, spec.as_ref()),
            "workspace": args.workspace.to_string_lossy().to_string(),
        }),
        args.json,
    ))
}

/// `cmd_validate_result`(`commands.py:206`)。
pub fn cmd_validate_result(args: &ValidateResultArgs) -> Result<CmdResult, CliError> {
    let raw = if let Some(path) = &args.file {
        std::fs::read_to_string(path)?
    } else if let Some(envelope) = &args.envelope {
        envelope.clone()
    } else if let Some(result) = &args.result {
        result.clone()
    } else {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
        input
    };
    let envelope: Value = serde_json::from_str(&raw)?;
    crate::model::spec::validate_result_envelope(&envelope)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "task_id": envelope.get("task_id").cloned().unwrap_or(Value::Null),
            "agent_id": envelope.get("agent_id").cloned().unwrap_or(Value::Null),
            "status": envelope.get("status").cloned().unwrap_or(Value::Null),
        }),
        args.json,
    ))
}

/// `cmd_collect`(`parser.py:292`)。
pub fn cmd_collect(args: &CollectArgs) -> Result<CmdResult, CliError> {
    let selected = match crate::state::selector::resolve_active_team(
        &args.workspace,
        None,
        crate::state::selector::SelectorMode::RuntimeOnly,
    ) {
        Ok(selected) => selected,
        Err(error) => {
            return Ok(CmdResult::from_json(
                json!({
                    "ok": false,
                    "error": error.to_string(),
                    "workspace": args.workspace.to_string_lossy().to_string(),
                }),
                args.json,
            ));
        }
    };
    let value = match messaging::collect(&selected.run_workspace, args.result_file.as_deref(), false) {
        Ok(value) => value,
        Err(error) => {
            return Ok(CmdResult::from_json(
                json!({
                    "ok": false,
                    "error": error.to_string(),
                    "workspace": selected.run_workspace.to_string_lossy().to_string(),
                }),
                args.json,
            ));
        }
    };
    let results = value.get("results").cloned().unwrap_or_else(|| json!({}));
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
    Ok(CmdResult::from_json(
        json!({
            "collected": [],
            "collected_results": value.get("collected_results").cloned().unwrap_or_else(|| json!([])),
            "coordinator": {
                "ok": false,
                "status": "not_required",
            },
            "delivered_messages": value.get("delivered_messages").cloned().unwrap_or_else(|| json!([])),
            "invalid_results": value.get("invalid_results").cloned().unwrap_or_else(|| json!([])),
            "ok": ok,
            "results": results,
            "state_file": value
                .get("state_file")
                .cloned()
                .unwrap_or_else(|| {
                    json!(selected
                        .spec_workspace
                        .as_deref()
                        .unwrap_or(&selected.run_workspace)
                        .join("team_state.md")
                        .to_string_lossy()
                        .to_string())
                }),
        }),
        args.json,
    ))
}

/// `cmd_settle`(`commands.py:86`)。
pub fn cmd_settle(args: &SettleArgs) -> Result<CmdResult, CliError> {
    match settle_value(&args.workspace) {
        Ok(value) => Ok(CmdResult::from_json(value, args.json)),
        Err(error) => Ok(CmdResult::from_json(
            json!({
                "ok": false,
                "error": error.to_string(),
                "workspace": args.workspace.to_string_lossy().to_string(),
            }),
            args.json,
        )),
    }
}

fn settle_value(workspace: &Path) -> Result<Value, CliError> {
    let mut collect = messaging::collect(workspace, None, false)?;
    if collect.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = collect
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("collect failed");
        return Err(CliError::Runtime(message.to_string()));
    }
    let coordinator_log = crate::coordinator::coordinator_log_path(
        &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
    );
    let collect_object = collect
        .as_object_mut()
        .ok_or_else(|| CliError::Runtime("collect returned non-object output".to_string()))?;
    collect_object.insert(
        "coordinator".to_string(),
        json!({
            "ok": true,
            "status": "started",
            "log": coordinator_log.to_string_lossy().to_string(),
        }),
    );
    let status = status_port::status(workspace, true, false)?;
    let details_log = write_settle_details_log(workspace, &collect, &status)?;
    let collected_count = collect
        .get("collected")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(json!({
        "ok": true,
        "summary": format!("collected {collected_count} result(s)"),
        "next_actions": ["Review team_state.md and decide whether to continue or shutdown."],
        "details_log": details_log.to_string_lossy().to_string(),
        "collect": collect,
    }))
}

fn write_settle_details_log(workspace: &Path, collect: &Value, status: &Value) -> Result<PathBuf, CliError> {
    let logs = workspace.join(".team").join("logs");
    std::fs::create_dir_all(&logs)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let path = logs.join(format!("settle-{timestamp}.json"));
    let details = json!({
        "collect": collect,
        "status": status,
    });
    let text = serde_json::to_string_pretty(&crate::cli::sort_json(&details))?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// `cmd_allow_peer_talk`(`parser.py allow-peer-talk`).
pub fn cmd_allow_peer_talk(args: &AllowPeerTalkArgs) -> Result<CmdResult, CliError> {
    let value = messaging::allow_peer_talk(&args.workspace, &args.a, &args.b)?;
    Ok(CmdResult::from_json(value, args.json))
}

/// `cmd_repair_state`(`parser.py:303`)。
pub fn cmd_repair_state(args: &RepairStateArgs) -> Result<CmdResult, CliError> {
    if !is_repair_task_status(&args.status) {
        return Err(CliError::Runtime(format!(
            "unknown task status for repair: {}",
            args.status
        )));
    }
    let mut state = crate::state::persist::load_runtime_state(&args.workspace)?;
    let before = find_task_projection(&state, &args.task_id).unwrap_or_else(repair_task_projection_null);
    update_task(
        &mut state,
        &args.task_id,
        args.assignee.as_deref(),
        &args.status,
        args.summary.as_deref(),
    );
    let after = find_task_projection(&state, &args.task_id).unwrap_or_else(repair_task_projection_null);
    crate::state::persist::save_runtime_state(&args.workspace, &state)?;
    let spec = load_team_spec_optional(&args.workspace, &state)?
        .ok_or_else(|| CliError::Runtime("team.spec.yaml not found".to_string()))?;
    let state_file = crate::lifecycle::restart::write_team_state(&args.workspace, &spec, &state)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    crate::event_log::EventLog::new(&args.workspace)
        .write(
            "repair_state.task",
            json!({
                "task_id": args.task_id,
                "before": before,
                "after": after,
            }),
        )
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "task_id": args.task_id,
            "before": before,
            "after": after,
            "state_file": state_file.to_string_lossy().to_string(),
        }),
        args.json,
    ))
}

/// `cmd_diagnose`(`parser.py:298`)。
pub fn cmd_diagnose(args: &DiagnoseArgs) -> Result<CmdResult, CliError> {
    let state = crate::state::persist::load_runtime_state(&args.workspace)?;
    let event_log = args.workspace.join(".team").join("logs").join("events.jsonl");
    let backend = crate::tmux_backend::TmuxBackend::for_workspace(&args.workspace);
    let (issues, suggested_repairs) = diagnose_runtime(&state, &backend);
    let ok = issues.as_array().is_some_and(Vec::is_empty);
    Ok(CmdResult::from_json(
        json!({
            "event_log": event_log.to_string_lossy().to_string(),
            "issues": issues,
            "ok": ok,
            "providers": provider_doctor_checks(),
            "runtime": {
                "workspace": args.workspace.to_string_lossy().to_string(),
                "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
                "leader_receiver": state.get("leader_receiver").cloned().unwrap_or(Value::Null),
                "agent_count": state.get("agents").and_then(Value::as_object).map_or(0, serde_json::Map::len),
                "message_count": count_dir_entries(&args.workspace.join(".team").join("messages")),
                "result_count": count_dir_entries(&args.workspace.join(".team").join("results")),
            },
            "suggested_repairs": suggested_repairs,
        }),
        args.json,
    ))
}

/// `cmd_preflight`(`parser.py:160`)。
pub fn cmd_preflight(args: &PreflightArgs) -> Result<CmdResult, CliError> {
    let report = build_preflight_report(&args.team)?;
    Ok(CmdResult::from_json(
        report,
        args.json,
    ))
}

/// `cmd_wait_ready`(`parser.py:171`)。
pub fn cmd_wait_ready(args: &WaitReadyArgs) -> Result<CmdResult, CliError> {
    let report = build_wait_ready_report(&args.workspace, args.timeout)?;
    Ok(CmdResult::from_json(
        report,
        args.json,
    ))
}

/// `cmd_e2e`(`parser.py:449`)。
pub fn cmd_e2e(args: &E2eArgs) -> Result<CmdResult, CliError> {
    let mut providers = serde_json::Map::new();
    for provider in &args.providers {
        let result = if provider == "fake" {
            run_fake_e2e(&args.workspace)?
        } else {
            skipped_provider_e2e(provider, args.real)
        };
        providers.insert(provider.clone(), result);
    }
    let ok = providers
        .values()
        .all(|value| value.get("ok").and_then(Value::as_bool) == Some(true));
    Ok(CmdResult::from_json(
        json!({
            "workspace": args.workspace.to_string_lossy().to_string(),
            "providers": Value::Object(providers),
            "ok": ok,
        }),
        args.json,
    ))
}

/// `cmd_peek`(`commands.py:118`)。
pub fn cmd_peek(args: &PeekArgs) -> Result<CmdResult, CliError> {
    if !args.allow_raw_screen {
        return Err(CliError::Usage("peek requires --allow-raw-screen".to_string()));
    }
    let state = crate::state::persist::load_runtime_state(&args.workspace)?;
    let Some(agent_state) = state.get("agents").and_then(|agents| agents.get(&args.agent)) else {
        return Ok(CmdResult::from_json(
            json!({
                "ok": false,
                "agent_id": args.agent,
                "error": format!("unknown agent id: {}", args.agent),
            }),
            args.json,
        ));
    };
    let Some((session, window, target)) = agent_pane_id(&state, &args.agent, agent_state) else {
        return Ok(peek_unavailable(&args.agent, args.json));
    };
    let backend = crate::tmux_backend::TmuxBackend::for_workspace(&args.workspace);
    let windows = backend
        .list_windows(&crate::transport::SessionName::new(session.clone()))
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    if !windows.iter().any(|w| w.as_str() == window) {
        return Ok(peek_unavailable(&args.agent, args.json));
    }
    let capture = backend
        .capture(
            &crate::transport::Target::Pane(crate::transport::PaneId::new(target.clone())),
            crate::transport::CaptureRange::Tail(args.tail as u32),
        )
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(CmdResult::from_json(
        json!({
            "ok": true,
            "agent_id": args.agent,
            "workspace": args.workspace.to_string_lossy().to_string(),
            "tail": args.tail,
            "pane_id": target,
            "text": capture.text,
        }),
        args.json,
    ))
}

fn peek_unavailable(agent: &str, json: bool) -> CmdResult {
    CmdResult::from_json(
        json!({
            "ok": false,
            "agent_id": agent,
            "error": format!("agent terminal is not available: {agent}"),
        }),
        json,
    )
}

fn agent_pane_id(state: &Value, agent: &str, agent_state: &Value) -> Option<(String, String, String)> {
    let session = state.get("session_name").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let window = ["window", "window_name"]
        .iter()
        .find_map(|key| agent_state.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .unwrap_or(agent);
    Some((session.to_string(), window.to_string(), format!("{session}:{window}")))
}

fn run_fake_e2e(workspace: &Path) -> Result<Value, CliError> {
    std::fs::create_dir_all(workspace)?;
    let spec_path = workspace.join("team.spec.yaml");
    std::fs::write(&spec_path, fake_spec_yaml(workspace))?;
    let launch = match crate::lifecycle::launch(&spec_path, true, true, true) {
        Ok(report) => json!({
            "ok": true,
            "dry_run": report.dry_run,
            "session_name": report.session_name.as_str(),
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    };
    seed_fake_e2e_state(workspace)?;
    let send = messaging::send_message(
        workspace,
        &MessageTarget::Single("fake_impl".to_string()),
        "implement fake task",
        &SendOptions {
            task_id: Some(TaskId::new("task_impl")),
            route_task_id: false,
            sender: "leader".to_string(),
            requires_ack: true,
            ..SendOptions::default()
        },
    )?;
    let send_value = json!({
        "ok": send.ok,
        "status": send.status,
        "message_status": send.message_status.0,
        "message_id": send.message_id,
        "reason": send.reason,
    });
    if send.ok {
        let _ = messaging::report_result(
            workspace,
            &json!({
                "schema_version": "result_envelope_v1",
                "task_id": "task_impl",
                "agent_id": "fake_impl",
                "status": "success",
                "summary": "fake result collected",
                "changes": [],
                "tests": [],
                "risks": [],
                "artifacts": [],
                "next_actions": [],
            }),
        )?;
    }
    let mut collect = messaging::collect(workspace, None, false)?;
    let collected = collect
        .get("collected_results")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if let Some(obj) = collect.as_object_mut() {
        obj.insert("collected".to_string(), Value::Bool(collected));
    }
    let shutdown = fake_shutdown(workspace)?;
    let ok = launch.get("ok").and_then(Value::as_bool) == Some(true)
        && send.ok
        && collected
        && shutdown.get("ok").and_then(Value::as_bool) == Some(true);
    Ok(json!({
        "ok": ok,
        "launch": launch,
        "send": send_value,
        "collect": collect,
        "shutdown": shutdown,
    }))
}

fn skipped_provider_e2e(provider: &str, real: bool) -> Value {
    let command = provider_command(provider);
    if !command_on_path(command) {
        return json!({
            "ok": false,
            "skipped": true,
            "reason": format!("{command} not installed"),
            "version": null,
        });
    }
    let version = command_version(command);
    if !real {
        return json!({
            "ok": false,
            "skipped": true,
            "reason": "real provider launch disabled; rerun with --real on an authenticated machine",
            "version": version,
        });
    }
    json!({
        "ok": false,
        "skipped": true,
        "reason": "real provider e2e is not available in this build",
        "version": version,
    })
}

fn command_on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(command);
            candidate.is_file()
        })
    })
}

fn command_version(command: &str) -> Value {
    match std::process::Command::new(command).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            }
        }
        _ => Value::Null,
    }
}

fn provider_command(provider: &str) -> &str {
    match provider {
        "claude" | "claude_code" => "claude",
        "gemini" | "gemini_cli" => "gemini",
        other => other,
    }
}

fn seed_fake_e2e_state(workspace: &Path) -> Result<(), CliError> {
    crate::state::persist::save_runtime_state(
        workspace,
        &json!({
            "leader": {"id": "leader"},
            "session_name": "team-agent-fake-e2e",
            "agents": {
                "fake_impl": {
                    "provider": "fake",
                    "status": "running",
                    "window": "fake_impl",
                    "pane_id": "%fake_impl",
                    "spawn_cwd": workspace.to_string_lossy().to_string(),
                }
            },
            "tasks": [{
                "id": "task_impl",
                "title": "Fake implementation",
                "type": "implementation",
                "assignee": "fake_impl",
                "status": "pending",
            }]
        }),
    )?;
    Ok(())
}

fn fake_shutdown(workspace: &Path) -> Result<Value, CliError> {
    let mut state = crate::state::persist::load_runtime_state(workspace)?;
    if let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) {
        for agent in agents.values_mut() {
            if let Some(obj) = agent.as_object_mut() {
                obj.insert("status".to_string(), Value::String("stopped".to_string()));
            }
        }
    }
    crate::state::persist::save_runtime_state(workspace, &state)?;
    Ok(json!({
        "ok": true,
        "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
        "session_killed": false,
        "coordinator": {"status": "missing", "pid": null},
    }))
}

fn fake_spec_yaml(workspace: &Path) -> String {
    let ws = workspace.to_string_lossy();
    format!(
        r#"version: 1
team:
  name: "fake-e2e"
  mode: "supervisor_worker"
  objective: "Exercise fake provider orchestration."
  workspace: "{ws}"
leader:
  id: "leader"
  role: "leader"
  provider: "fake"
  model: null
  tools:
    - "fs_read"
    - "fs_list"
    - "mcp_team"
  context_policy:
    keep_user_thread: true
    receive_worker_outputs: "structured_only"
    max_worker_result_tokens: 2000
agents:
  - id: "fake_impl"
    role: "implementation_engineer"
    provider: "fake"
    model: null
    working_directory: "{ws}"
    system_prompt:
      inline: "Handle fake implementation tasks."
      file: null
    tools:
      - "fs_read"
      - "fs_write"
      - "fs_list"
      - "execute_bash"
      - "git_diff"
      - "mcp_team"
      - "provider_builtin"
    permission_mode: "restricted"
    preferred_for:
      - "implementation"
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields:
        - "task_id"
        - "status"
        - "summary"
        - "artifacts"
routing:
  default_assignee: "leader"
  rules:
    - id: "implementation-to-fake"
      match:
        type:
          - "implementation"
      assign_to: "fake_impl"
      priority: 10
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 2
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-agent-fake-e2e"
  auto_launch: true
  require_user_approval_before_launch: false
  max_active_agents: 1
  startup_order:
    - "fake_impl"
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks:
  - id: "task_impl"
    title: "Fake implementation"
    type: "implementation"
    assignee: "fake_impl"
    deps: []
    acceptance:
      - "fake result collected"
    status: "pending"
    requires_tools:
      - "fs_write"
      - "execute_bash"
    files:
      - "src/example.py"
    risk: "low"
"#
    )
}

fn sessions_overview(state: &Value, spec: Option<&crate::model::yaml::Value>) -> Value {
    let mut rows = Vec::new();
    if let Some(agents) = spec.and_then(|v| v.get("agents")).and_then(crate::model::yaml::Value::as_list) {
        for agent in agents {
            let Some(agent_id) = agent.get("id").and_then(crate::model::yaml::Value::as_str) else {
                continue;
            };
            let agent_state = state
                .get("agents")
                .and_then(|agents| agents.get(agent_id))
                .unwrap_or(&Value::Null);
            rows.push(session_row(state, agent, agent_id, agent_state));
        }
    }
    Value::Array(rows)
}

fn compiled_agent_ids_for_cli(spec: &crate::model::yaml::Value) -> Vec<String> {
    spec.get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|agent| agent.get("id").and_then(crate::model::yaml::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn session_row(
    state: &Value,
    spec_agent: &crate::model::yaml::Value,
    agent_id: &str,
    agent_state: &Value,
) -> Value {
    json!({
        "agent_id": agent_id,
        "provider": yaml_str(spec_agent, "provider").map(Value::String).unwrap_or(Value::Null),
        "model": yaml_str(spec_agent, "model").map(Value::String).unwrap_or(Value::Null),
        "profile": yaml_str(spec_agent, "profile").map(Value::String).unwrap_or(Value::Null),
        "session_id": field_or_null(agent_state, &["session_id"]),
        "resume_id": field_or_null(agent_state, &["resume_id"]),
        "rollout_path": field_or_null(agent_state, &["rollout_path"]),
        "captured_at": field_or_null(agent_state, &["captured_at"]),
        "captured_via": field_or_null(agent_state, &["captured_via"]),
        "attribution_confidence": field_or_null(agent_state, &["attribution_confidence"]),
        "spawn_cwd": field_or_null(agent_state, &["spawn_cwd", "working_directory"]),
        "context_usage": field_or_null(agent_state, &["context_usage"]),
        "status": field_or_default(agent_state, "status", "unknown"),
        "last_task": last_task_for_agent(state, agent_id),
        "handoff_path": field_or_null(agent_state, &["handoff_path"]),
        "display_target": field_or_null(agent_state, &["display"]),
        "terminal_target": terminal_target(state, agent_id, agent_state),
    })
}

fn yaml_str(value: &crate::model::yaml::Value, key: &str) -> Option<String> {
    value.get(key).and_then(crate::model::yaml::Value::as_str).map(ToString::to_string)
}

fn field_or_null(value: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .find_map(|key| value.get(*key).cloned())
        .unwrap_or(Value::Null)
}

fn field_or_default(value: &Value, key: &str, default: &str) -> Value {
    value
        .get(key)
        .cloned()
        .unwrap_or_else(|| Value::String(default.to_string()))
}

fn last_task_for_agent(state: &Value, agent_id: &str) -> Value {
    state
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| {
            tasks
                .iter()
                .rev()
                .find(|task| task.get("assignee").and_then(Value::as_str) == Some(agent_id))
        })
        .and_then(|task| task.get("id").and_then(Value::as_str).map(ToString::to_string))
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn terminal_target(state: &Value, agent_id: &str, agent_state: &Value) -> Value {
    json!({
        "session": agent_state
            .get("session_name")
            .cloned()
            .or_else(|| state.get("session_name").cloned())
            .unwrap_or(Value::Null),
        "window": window_target(agent_state, agent_id),
        "pane": field_or_null(agent_state, &["pane_id"]),
    })
}

fn window_target(agent_state: &Value, agent_id: &str) -> Value {
    let window = field_or_null(agent_state, &["window", "window_name"]);
    if window.is_null() {
        Value::String(agent_id.to_string())
    } else {
        window
    }
}


fn find_task_projection(state: &Value, task_id: &str) -> Option<Value> {
    state
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.iter().find(|task| task.get("id").and_then(Value::as_str) == Some(task_id)))
        .map(repair_task_projection)
}

fn update_task(
    state: &mut Value,
    task_id: &str,
    assignee: Option<&str>,
    status: &str,
    summary: Option<&str>,
) -> Value {
    if let Some(tasks) = state.get_mut("tasks").and_then(Value::as_array_mut) {
        for task in tasks {
            if task.get("id").and_then(Value::as_str) == Some(task_id) {
                if let Some(obj) = task.as_object_mut() {
                    if let Some(assignee) = assignee {
                        obj.insert("assignee".to_string(), Value::String(assignee.to_string()));
                    }
                    obj.insert("status".to_string(), Value::String(status.to_string()));
                    if let Some(summary) = summary {
                        obj.insert("last_result_summary".to_string(), Value::String(summary.to_string()));
                    }
                    return Value::Object(obj.clone());
                }
            }
        }
    }
    json!({
        "id": task_id,
        "assignee": assignee.unwrap_or(""),
        "status": status,
        "summary": summary.unwrap_or(""),
    })
}

fn is_repair_task_status(status: &str) -> bool {
    matches!(
        status,
        "blocked" | "cancelled" | "done" | "failed" | "needs_retry" | "pending" | "ready" | "running"
    )
}

fn repair_task_projection(task: &Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "assignee".to_string(),
        task.get("assignee").cloned().unwrap_or(Value::Null),
    );
    map.insert(
        "status".to_string(),
        task.get("status").cloned().unwrap_or(Value::Null),
    );
    map.insert(
        "last_result_summary".to_string(),
        task.get("last_result_summary").cloned().unwrap_or(Value::Null),
    );
    Value::Object(map)
}

fn repair_task_projection_null() -> Value {
    let mut map = serde_json::Map::new();
    map.insert("assignee".to_string(), Value::Null);
    map.insert("status".to_string(), Value::Null);
    map.insert("last_result_summary".to_string(), Value::Null);
    Value::Object(map)
}

fn load_team_spec_optional(workspace: &Path, state: &Value) -> Result<Option<crate::model::yaml::Value>, CliError> {
    let spec_path = state
        .get("spec_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            state
                .get("team_dir")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(path).join("team.spec.yaml"))
        })
        .unwrap_or_else(|| workspace.join("team.spec.yaml"));
    if !spec_path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&spec_path)?;
    crate::model::yaml::loads(&text)
        .map(Some)
        .map_err(|e| CliError::Runtime(e.to_string()))
}

/// `cmd_approvals`(`commands.py:112`)。
pub fn cmd_approvals(args: &ApprovalsArgs) -> Result<CmdResult, CliError> {
    let value = status_port::approvals(&args.workspace, args.agent.as_deref(), args.json)?;
    if args.json {
        Ok(CmdResult::from_json(value, true))
    } else {
        Ok(CmdResult::human(status_port::format_approvals(&value)))
    }
}

/// `cmd_inbox`(`commands.py:137`)。
pub fn cmd_inbox(args: &InboxArgs) -> Result<CmdResult, CliError> {
    let value = status_port::inbox(
        &args.workspace,
        &args.agent,
        args.limit,
        args.since.as_deref(),
        args.json,
    )?;
    if args.json {
        Ok(CmdResult::from_json(value, true))
    } else {
        Ok(CmdResult::human(format_inbox_human(
            &args.workspace,
            &args.agent,
            args.since.as_deref(),
            &value,
        )?))
    }
}

fn format_inbox_human(
    workspace: &Path,
    agent: &str,
    since: Option<&str>,
    value: &Value,
) -> Result<String, CliError> {
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    if messages.is_empty() {
        let mut line = format!("{agent}: no messages");
        if let Some(since) = since {
            line.push_str(" since ");
            line.push_str(since);
        }
        lines.push(line);
    } else {
        lines.push(format!("{agent}: {} message(s)", messages.len()));
        for message in messages {
            let sender = message.get("sender").and_then(Value::as_str).unwrap_or("-");
            let content = message.get("content").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("- {sender}: {content}"));
        }
    }
    let pending = uncollected_result_count(workspace)?;
    let mut note = "final results are not in inbox; use team-agent collect".to_string();
    if pending > 0 {
        note.push_str(&format!(" ({pending} uncollected result(s) pending)"));
    }
    lines.push(note);
    Ok(lines.join("\n"))
}

fn uncollected_result_count(workspace: &Path) -> Result<i64, CliError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    conn.query_row(
        "select count(*) from results where status not in ('collected', 'invalid')",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map_err(|e| CliError::Runtime(e.to_string()))
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
    Ok(CmdResult::from_json(
        leader_port::claim_leader(&args.workspace, args.team.as_deref(), args.confirm)?,
        args.json,
    ))
}

/// `cmd_identity`(`commands.py:160`)。
pub fn cmd_identity(args: &IdentityArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        leader_port::leader_identity(&args.workspace, args.team.as_deref())?,
        args.json,
    ))
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
    Ok(CmdResult::from_json(
        lifecycle_port::restart(&args.workspace, args.allow_fresh, args.team.as_deref())?,
        args.json,
    ))
}

/// `cmd_start_agent`(`commands.py:348`)。
pub fn cmd_start_agent(args: &StartAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::start_agent(
            &args.workspace,
            &args.agent,
            args.force,
            !args.no_display,
            args.allow_fresh,
            args.team.as_deref(),
        )?,
        args.json,
    ))
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
            !args.no_display,
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

/// `cmd_add_agent`(`commands.py:373`)。
pub fn cmd_add_agent(args: &AddAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::add_agent(
            &args.workspace,
            &args.agent,
            &args.role_file,
            !args.no_display,
            args.team.as_deref(),
        )?,
        args.json,
    ))
}

/// `cmd_fork_agent`(`commands.py:383`)。
pub fn cmd_fork_agent(args: &ForkAgentArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::fork_agent(
            &args.workspace,
            &args.source_agent,
            &args.as_agent,
            args.label.as_deref(),
            !args.no_display,
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

/// `cmd_stuck_list`(`commands.py:405`)。REUSE `messaging::stuck_list`。
pub fn cmd_stuck_list(args: &StuckListArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(messaging::stuck_list(&args.workspace)?, args.json))
}

/// `cmd_stuck_cancel`(`commands.py:409`)。REUSE `messaging::stuck_cancel`(suppressed_by="leader")。
pub fn cmd_stuck_cancel(args: &StuckCancelArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        messaging::stuck_cancel(&args.workspace, &args.agent, args.alert_type, "leader")?,
        args.json,
    ))
}

/// `cmd_acknowledge_idle`(`commands.py:418`)。
pub fn cmd_acknowledge_idle(args: &AcknowledgeIdleArgs) -> Result<CmdResult, CliError> {
    Ok(CmdResult::from_json(
        lifecycle_port::acknowledge_idle(&args.workspace, args.team.as_deref())?,
        args.json,
    ))
}

/// `cmd_doctor`(`commands.py:218`)。分派:`--fix` 缺 gate→Usage err;`--comms`/`gate==comms`→
/// `diagnose_port::comms_selftest`(+ COMMS_BOUNDARY_TEXT 人读前缀);`gate==orphans`→`orphan_gate`;
/// 非 gate:`--fix-schema`→`fix_schema`;schema drift→注入;`--cleanup-orphans`→`cleanup_orphans`;
/// else→`diagnose_port::doctor(spec)` + schema 注入。返回 `dict | str`(comms 人读 = boundary+json)。
pub fn cmd_doctor(args: &DoctorArgs) -> Result<CmdResult, CliError> {
    if args.fix && args.gate.is_none() {
        return Err(CliError::Runtime("--fix requires --gate".to_string()));
    }
    if args.comms || matches!(args.gate, Some(DoctorGate::Comms)) {
        let value = diagnose_port::comms_selftest(&args.workspace, args.team.as_deref(), Some("comms"))?;
        if !args.json {
            let json_tail = serde_json::to_string_pretty(&sort_json(&value))?;
            return Ok(CmdResult::human(format!("{COMMS_BOUNDARY_TEXT}\n{json_tail}")));
        }
        return Ok(CmdResult::from_json(value, true));
    }
    let value = if matches!(args.gate, Some(DoctorGate::Orphans)) {
        diagnose_port::orphan_gate(args.fix, args.confirm)?
    } else if args.cleanup_orphans {
        diagnose_port::cleanup_orphans(args.confirm)?
    } else if args.fix_schema {
        diagnose_port::fix_schema(&args.workspace)?
    } else {
        diagnose_port::doctor(&args.workspace, args.spec.as_deref())?
    };
    Ok(CmdResult::from_json(value, args.json))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::agent_pane_id;
    use serde_json::json;

    #[test]
    fn agent_pane_id_resolves_session_window_even_with_recorded_pane_id() {
        let state = json!({
            "session_name": "team-x",
            "agents": {
                "w1": {"pane_id": "%7", "window": "w1"}
            }
        });
        let agent = state.get("agents").and_then(|agents| agents.get("w1")).unwrap();

        assert_eq!(
            agent_pane_id(&state, "w1", agent).unwrap(),
            ("team-x".to_string(), "w1".to_string(), "team-x:w1".to_string())
        );
    }

    #[test]
    fn agent_pane_id_falls_back_to_session_window_target() {
        let state = json!({
            "session_name": "team-x",
            "agents": {
                "w1": {"window": "worker-one"}
            }
        });
        let agent = state.get("agents").and_then(|agents| agents.get("w1")).unwrap();

        assert_eq!(
            agent_pane_id(&state, "w1", agent).unwrap(),
            ("team-x".to_string(), "worker-one".to_string(), "team-x:worker-one".to_string())
        );
    }
}
