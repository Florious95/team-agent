//! diagnose/preflight/wait-ready CLI helpers.
use super::*;
use crate::provider::wire::{command_name, provider_wire};
use crate::transport::Transport;

pub(crate) fn diagnose_runtime(state: &Value, backend: &dyn Transport) -> (Value, Value) {
    let mut issues = Vec::new();
    let mut repairs = Vec::new();

    if let Some(session_name) = state.get("session_name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        match backend.has_session(&crate::transport::SessionName::new(session_name.to_string())) {
            Ok(true) => {}
            Ok(false) => {
                issues.push(json!("tmux_session_missing"));
                repairs.push(json!({
                    "issue": "tmux_session_missing",
                    "action": format!("restart or relaunch tmux session `{session_name}`"),
                }));
            }
            Err(error) => {
                issues.push(json!("tmux_session_missing"));
                repairs.push(json!({
                    "issue": "tmux_session_missing",
                    "action": format!("restart or relaunch tmux session `{session_name}`"),
                    "reason": error.to_string(),
                }));
            }
        }
    }

    if !leader_receiver_attached(state) {
        issues.push(json!("leader_not_attached"));
        repairs.push(json!({
            "issue": "leader_not_attached",
            "action": "attach or claim a leader receiver before sending work",
        }));
    } else {
        // 0.4.x (CR R2 P0): leader provider health reconciliation. The
        // leader_receiver may be marked `attached` (pane addressable) but the
        // provider process has exited — pane fell back to shell with the exit
        // marker. Distinguish `leader_provider_exited` from `attached` so
        // status/diagnose surfaces the real state.
        if let Some((pane_id, provider_label)) = leader_pane_and_provider(state) {
            let health = crate::leader::leader_provider_health(
                backend,
                &crate::transport::PaneId::new(pane_id),
                &provider_label,
            );
            match health {
                crate::leader::LeaderProviderHealth::ProviderExited => {
                    issues.push(json!("leader_provider_exited"));
                    repairs.push(json!({
                        "issue": "leader_provider_exited",
                        "action": format!(
                            "leader pane fell back to shell — provider `{provider_label}` exited; \
                             relaunch with `team-agent {provider_label}` to restart the provider"
                        ),
                    }));
                }
                crate::leader::LeaderProviderHealth::Unreachable => {
                    issues.push(json!("leader_provider_unreachable"));
                    repairs.push(json!({
                        "issue": "leader_provider_unreachable",
                        "action": "leader pane is dead — relaunch the leader",
                    }));
                }
                crate::leader::LeaderProviderHealth::Alive => {}
            }
        }
    }

    if let Some(session_name) = state.get("session_name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        if let Ok(windows) = backend.list_windows(&crate::transport::SessionName::new(session_name.to_string())) {
            if let Some(agents) = state.get("agents").and_then(Value::as_object) {
                for (agent_id, agent_state) in agents {
                    let window = ["window", "window_name"]
                        .iter()
                        .find_map(|key| agent_state.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
                        .unwrap_or(agent_id);
                    if !windows.iter().any(|w| w.as_str() == window) {
                        issues.push(json!(format!("worker_window_missing:{agent_id}")));
                    }
                    if agent_state
                        .get("approval")
                        .or_else(|| agent_state.get("approval_status"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| s == "pending" || s == "awaiting_trust_prompt")
                    {
                        issues.push(json!(format!("worker_approval_pending:{agent_id}")));
                    }
                }
            }
        }
    }

    (Value::Array(issues), Value::Array(repairs))
}

/// 0.4.x (CR R2): pull (pane_id, provider_label) from leader_receiver. Used
/// by `leader_provider_health` reconcile in diagnose. Returns None when the
/// leader is not attached or any field is missing.
fn leader_pane_and_provider(state: &Value) -> Option<(String, String)> {
    let receiver = state.get("leader_receiver")?;
    let pane_id = receiver
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let provider = receiver
        .get("provider")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("claude")
        .to_string();
    Some((pane_id, provider))
}

fn leader_receiver_attached(state: &Value) -> bool {
    let Some(receiver) = state.get("leader_receiver") else {
        return false;
    };
    let mode_direct = receiver
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode == "direct_tmux" || mode == "direct");
    let status_attached = receiver.get("status").and_then(Value::as_str) == Some("attached");
    let pane_present = receiver
        .get("pane_id")
        .and_then(Value::as_str)
        .is_some_and(|pane| !pane.is_empty());
    mode_direct && status_attached && pane_present
}

pub(crate) fn build_preflight_report(team: &std::path::Path) -> Result<Value, CliError> {
    let mut checks = Vec::new();
    let mut next_actions = Vec::new();

    let compiled = match crate::compiler::compile_team(team) {
        Ok(spec) => {
            checks.push(json!({
                "name": "compile",
                "ok": true,
                "agents": compiled_agent_ids(&spec),
            }));
            Some(spec)
        }
        Err(error) => {
            checks.push(json!({
                "name": "compile",
                "ok": false,
                "error": error.to_string(),
            }));
            next_actions.push(json!("fix TEAM.md and role front matter, then run preflight again"));
            None
        }
    };

    let tmux_path = command_path("tmux");
    checks.push(json!({
        "name": "tmux",
        "ok": tmux_path.is_some(),
        "path": tmux_path,
    }));
    if tmux_path.is_none() {
        next_actions.push(json!("install tmux or add it to PATH"));
    }

    let display_backend = compiled
        .as_ref()
        .and_then(|spec| yaml_path_str(spec, &["runtime", "display_backend"]))
        .unwrap_or("none");
    let ghostty_required = display_backend == "ghostty_window" || display_backend == "ghostty";
    let ghostty_path = command_path("ghostty");
    checks.push(json!({
        "name": "ghostty",
        "ok": !ghostty_required || ghostty_path.is_some(),
        "path": ghostty_path,
        "required": ghostty_required,
    }));
    if ghostty_required && ghostty_path.is_none() {
        next_actions.push(json!("install Ghostty or choose another display_backend"));
    }

    let workspace = crate::model::paths::team_workspace(team)
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    let profile_dir_exists = workspace.join(".team").join("current").join("profiles").exists()
        || team.join("profiles").exists();
    let profile_dir_check = json!({
        "name": "profile_dir",
        "ok": true,
        "status": if profile_dir_exists { "present" } else { "not_required" },
    });
    let profile_smoke_check = build_profile_smoke_check_for_team(team)?;
    if profile_smoke_check.get("ok").and_then(Value::as_bool) == Some(false) {
        let reason = profile_smoke_check
            .get("reason")
            .or_else(|| profile_smoke_check.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("profile smoke failed");
        next_actions.push(json!(format!("fix compatible_api profile smoke: {reason}")));
    }
    checks.push(json!({
        "name": "profiles",
        "ok": true,
        "checks": compact_profile_checks(team),
    }));
    checks.push(profile_smoke_check);
    checks.push(if compiled.is_some() {
        json!({
            "name": "models",
            "ok": true,
        })
    } else {
        json!({
            "name": "models",
            "ok": true,
            "status": "skipped",
        })
    });
    checks.push(json!({
        "name": "core_runtime",
        "ok": true,
        "status": core_runtime_status(),
    }));
    checks.push(profile_dir_check);

    let blockers = preflight_blockers(&checks);
    let ok = blockers.is_empty();
    let summary = if ok {
        "preflight passed".to_string()
    } else {
        format!("preflight found blockers: {}", blocker_names(&blockers).join(", "))
    };
    let checks_value = Value::Array(checks);
    let details_log = write_details_log(
        team,
        "preflight",
        &json!({
            "ok": ok,
            "summary": summary,
            "checks": checks_value,
            "blockers": blockers,
            "next_actions": next_actions,
        }),
    )?;
    let report = json!({
        "blockers": blockers,
        "checks": checks_value,
        "details_log": details_log.to_string_lossy().to_string(),
        "next_actions": next_actions,
        "ok": ok,
        "summary": summary,
    });
    let _ = crate::event_log::EventLog::new(team).write("preflight.complete", report.clone());
    Ok(report)
}

pub(crate) fn build_profile_smoke_check_for_team(team: &std::path::Path) -> Result<Value, CliError> {
    let workspace = crate::model::paths::team_workspace(team)
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    let spec = match crate::compiler::compile_team(team) {
        Ok(spec) => spec,
        Err(error) => {
            // SMOKE-1 (locate.md §"Smallest likely code touch" item 2):compile
            // 失败时把 team_dir + next_action 带上,operator 才有可下手的诊断
            // (不是只贴一行 reason)。
            return Ok(json!({
                "name": "profile_smoke",
                "ok": false,
                "status": "profile_invalid",
                "team_dir": team.to_string_lossy().to_string(),
                "reason": error.to_string(),
                "next_action": format!(
                    "fix the team spec at `{}` (see reason above) or re-run \
                     doctor with a different `<team-dir>`",
                    team.display()
                ),
                "secret_values_printed": false,
                "checks": [],
            }));
        }
    };
    let agents = spec
        .get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .unwrap_or(&[]);
    let checks = crate::lifecycle::profile_smoke::profile_smoke_checks_for_agents_with_profile_dir(
        &workspace,
        agents,
        Some(&team.join("profiles")),
        crate::lifecycle::profile_smoke::DEFAULT_PROFILE_SMOKE_TIMEOUT,
    );
    Ok(aggregate_profile_smoke_checks(checks))
}

fn aggregate_profile_smoke_checks(checks: Vec<Value>) -> Value {
    let failed = checks
        .iter()
        .filter(|check| check.get("ok").and_then(Value::as_bool) == Some(false))
        .cloned()
        .collect::<Vec<_>>();
    let ok = failed.is_empty();
    let status = if !ok {
        failed
            .first()
            .and_then(|check| check.get("status").and_then(Value::as_str))
            .unwrap_or("smoke_failed")
    } else if checks
        .iter()
        .any(|check| check.get("status").and_then(Value::as_str) == Some("smoke_passed"))
    {
        "smoke_passed"
    } else if checks
        .iter()
        .any(|check| check.get("status").and_then(Value::as_str) == Some("skipped_by_profile"))
    {
        "skipped_by_profile"
    } else {
        "not_required"
    };
    let mut out = json!({
        "name": "profile_smoke",
        "ok": ok,
        "status": status,
        "checks": checks,
        "secret_values_printed": false,
    });
    if let Some(first) = failed.first() {
        copy_optional_field(first, &mut out, "reason");
        copy_optional_field(first, &mut out, "http_status");
        copy_optional_field(first, &mut out, "endpoint");
        copy_optional_field(first, &mut out, "error");
    } else if let Some(first_passed) = checks
        .iter()
        .find(|check| check.get("status").and_then(Value::as_str) == Some("smoke_passed"))
    {
        copy_optional_field(first_passed, &mut out, "http_status");
        copy_optional_field(first_passed, &mut out, "endpoint");
    }
    out
}

fn copy_optional_field(from: &Value, to: &mut Value, key: &str) {
    let Some(value) = from.get(key).cloned() else {
        return;
    };
    let Some(obj) = to.as_object_mut() else {
        return;
    };
    obj.insert(key.to_string(), value);
}

pub(crate) fn build_wait_ready_report(
    workspace: &std::path::Path,
    timeout: f64,
    team: Option<&str>,
) -> Result<Value, CliError> {
    // swallow batch 3 ③: an unreadable runtime state must never read as "ready" — the
    // read error is surfaced verbatim (state_read_error) with ready=false instead of
    // silently degrading to an empty/stale state.
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RuntimeOnly,
    ) {
        Ok(selected) => selected,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "status": "error",
                "reason": "state_read_error",
                "state_read_error": error.to_string(),
                "readiness": {"ready": false},
                "summary": "runtime state could not be read",
                "next_actions": [json!("inspect .team/runtime/state.json (corrupt or unreadable) and retry")],
            }));
        }
    };
    let timeout = if timeout.is_finite() && timeout > 0.0 { timeout } else { 0.0 };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    let mut readiness;
    let mut state_read_error: Option<String> = None;
    loop {
        let mut state = match crate::state::projection::select_runtime_state(
            &selected.run_workspace,
            Some(&selected.team_key),
        ) {
            Ok(state) => {
                state_read_error = None;
                state
            }
            Err(error) => {
                state_read_error = Some(error.to_string());
                readiness = json!({"ready": false, "state_read_error": error.to_string()});
                break;
            }
        };
        inject_tmux_session_present(&selected.run_workspace, &mut state);
        inject_message_counts(&selected.run_workspace, &mut state)?;
        readiness = wait_readiness(&state);
        let awaiting_trust = readiness
            .get("awaiting_trust_prompt")
            .and_then(Value::as_bool)
            == Some(true);
        let ready = readiness.get("ready").and_then(Value::as_bool) == Some(true);
        if awaiting_trust || ready || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let awaiting_trust = readiness
        .get("awaiting_trust_prompt")
        .and_then(Value::as_bool)
        == Some(true);
    let ready = readiness.get("ready").and_then(Value::as_bool) == Some(true);
    let (ok, status, reason, summary, next_actions) = if state_read_error.is_some() {
        (
            false,
            "error",
            "state_read_error",
            "runtime state could not be read",
            vec![json!("inspect .team/runtime/state.json (corrupt or unreadable) and retry")],
        )
    } else if awaiting_trust {
        (
            false,
            "pending",
            "awaiting_trust_prompt",
            "workers are awaiting trust prompt",
            vec![json!("answer the provider trust prompt, then run wait-ready again")],
        )
    } else if ready {
        (true, "ready", "ready", "workers ready", Vec::new())
    } else if readiness.get("session_capture_complete").and_then(Value::as_bool) == Some(false) {
        (
            false,
            "pending",
            "session_capture_incomplete",
            "provider session capture is incomplete",
            vec![json!("wait for provider session capture before restart")],
        )
    } else {
        (
            false,
            "timeout",
            "workers_not_ready",
            "workers not ready before timeout",
            vec![json!("inspect team-agent diagnose output and worker terminals")],
        )
    };
    let details_log = write_details_log(
        &selected.run_workspace,
        "wait-ready",
        &json!({
            "ok": ok,
            "status": status,
            "reason": reason,
            "timeout": timeout,
            "readiness": readiness,
        }),
    )?;
    let mut report = json!({
        "details_log": details_log.to_string_lossy().to_string(),
        "next_actions": next_actions,
        "ok": ok,
        "reason": reason,
        "readiness": readiness,
        "status": status,
        "summary": summary,
    });
    if let Some(error) = state_read_error {
        report["state_read_error"] = json!(error);
    }
    Ok(report)
}

fn inject_tmux_session_present(workspace: &std::path::Path, state: &mut Value) {
    // Bug #7 (prerelease 0.4.0 gate review §6): probe the SAME endpoint the
    // runtime actually uses (state.tmux_endpoint / tmux_socket), not the
    // workspace-hash socket. Otherwise readiness reports `process_started=false`
    // even though the session is alive on the persisted socket.
    let Some(session_name) = state.get("session_name").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return;
    };
    let session_name_owned = session_name.to_string();
    let selection = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
        workspace,
        Some(state),
    );
    let present = selection
        .backend
        .has_session(&crate::transport::SessionName::new(session_name_owned))
        .unwrap_or(false);
    if let Value::Object(map) = state {
        map.insert("tmux_session_present".to_string(), Value::Bool(present));
    }
}

pub(crate) fn wait_readiness(state: &Value) -> Value {
    let agents = state.get("agents").and_then(Value::as_object);
    let mut process_started = false;
    let mut cli_prompt_ready = false;
    let mut mcp_ready = false;
    let mut task_prompt_delivered = false;
    let mut awaiting_trust_prompt = false;
    let mut incomplete_sessions = Vec::new();
    // A-5: a missing/unreadable leader_receiver must NOT count as attached —
    // "unreadable is never ready" (doctor/wait-ready truthfulness rule).
    let all_attached_receiver = state
        .get("leader_receiver")
        .and_then(Value::as_object)
        .is_some_and(|receiver| {
            receiver
                .get("status")
                .and_then(Value::as_str)
                == Some("attached")
                || receiver
                .get("pane_id")
                .and_then(Value::as_str)
                .is_some_and(|pane| !pane.is_empty() && pane != "__team_agent_unbound__")
        });

    if let Some(agents) = agents {
        process_started = state
            .get("tmux_session_present")
            .map(crate::cli::helpers::python_truthy)
            .map(|present| present || fake_process_started(agents))
            .unwrap_or_else(|| legacy_process_started(agents));
        cli_prompt_ready = !agents.is_empty()
            && agents.values().all(|agent| {
                agent.get("cli_prompt_ready").and_then(Value::as_bool) == Some(true)
                    || agent.get("startup_prompts").and_then(Value::as_str) == Some("complete")
                    || matches!(
                        agent.get("status").and_then(Value::as_str),
                        Some("running" | "busy" | "ready")
                    )
            });
        mcp_ready = !agents.is_empty()
            && agents.values().all(agent_mcp_ready);
        task_prompt_delivered = !agents.is_empty()
            && (message_counts_positive(state.get("messages"))
                || agents.values().all(|agent| {
                    agent.get("task_prompt_delivered").and_then(Value::as_bool) == Some(true)
                        || agent.get("first_send_at").is_some_and(|v| !v.is_null())
                }));
        awaiting_trust_prompt = agents.values().any(|agent| {
            agent
                .get("startup_prompt_status")
                .or_else(|| agent.get("startup_prompts"))
                .or_else(|| agent.get("status"))
                .and_then(Value::as_str)
                == Some("awaiting_trust_prompt")
        });
        incomplete_sessions = crate::session_capture::incomplete_interacted_resumable_agent_ids(state);
    }
    let all_resumable_have_session = incomplete_sessions.is_empty();
    let session_capture_incomplete = !all_resumable_have_session;
    let all_spawned = process_started && cli_prompt_ready && mcp_ready;
    let ready = all_spawned && all_attached_receiver && all_resumable_have_session;
    json!({
        "all_attached_receiver": all_attached_receiver,
        "all_resumable_have_session": all_resumable_have_session,
        "all_spawned": all_spawned,
        "awaiting_trust_prompt": awaiting_trust_prompt,
        "cli_prompt_ready": cli_prompt_ready,
        "incomplete_session_capture_agents": incomplete_sessions.clone(),
        "mcp_ready": mcp_ready,
        "process_started": process_started,
        "ready": ready,
        "session_capture_complete": all_resumable_have_session,
        "session_capture_incomplete": session_capture_incomplete,
        "pending_session_agent_ids": incomplete_sessions,
        "task_prompt_delivered": task_prompt_delivered,
    })
}

fn inject_message_counts(workspace: &std::path::Path, state: &mut Value) -> Result<(), CliError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut stmt = conn
        .prepare("select status, count(*) from messages group by status order by status")
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut rows = stmt.query([]).map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut messages = Map::new();
    while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
        let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
        let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
        messages.insert(status, json!(count));
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("messages".to_string(), Value::Object(messages));
    }
    Ok(())
}

fn message_counts_positive(value: Option<&Value>) -> bool {
    let Some(Value::Object(counts)) = value else {
        return false;
    };
    counts.values().any(|count| {
        count
            .as_i64()
            .or_else(|| count.as_u64().and_then(|n| i64::try_from(n).ok()))
            .is_some_and(|n| n > 0)
    })
}

fn legacy_process_started(agents: &serde_json::Map<String, Value>) -> bool {
    !agents.is_empty()
        && agents.values().all(|agent| {
            agent.get("pane_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                || agent.get("pid").and_then(Value::as_i64).is_some()
                || agent.get("process_started").and_then(Value::as_bool) == Some(true)
                || fake_agent_started(agent)
        })
}

fn fake_process_started(agents: &serde_json::Map<String, Value>) -> bool {
    !agents.is_empty() && agents.values().all(fake_agent_started)
}

fn fake_agent_started(agent: &Value) -> bool {
    agent.get("provider").and_then(Value::as_str) == Some("fake")
        && matches!(
            agent.get("status").and_then(Value::as_str),
            Some("running" | "busy" | "ready")
        )
}

fn agent_mcp_ready(agent: &Value) -> bool {
    agent
        .get("mcp_config")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| std::path::Path::new(path).exists())
        .unwrap_or_else(|| {
            agent.get("mcp_ready").and_then(Value::as_bool) == Some(true)
                || agent.get("mcp").and_then(Value::as_str) == Some("ready")
        })
}

fn command_path(command: &str) -> Option<String> {
    let output = std::process::Command::new("which")
        .arg(command)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn compact_profile_checks(team: &std::path::Path) -> Vec<Value> {
    let profiles = team.join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles) else {
        return Vec::new();
    };
    let mut checks = Vec::new();
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("profile")
            .to_string();
        checks.push(json!({
            "name": name,
            "ok": true,
            "path": path.to_string_lossy().to_string(),
        }));
    }
    checks
}

fn core_runtime_status() -> &'static str {
    if std::env::current_exe().is_ok() {
        "available"
    } else {
        "python_fallback"
    }
}

fn compiled_agent_ids(spec: &crate::model::yaml::Value) -> Vec<String> {
    spec.get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|agent| agent.get("id").and_then(crate::model::yaml::Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn preflight_blockers(checks: &[Value]) -> Vec<Value> {
    checks
        .iter()
        .filter(|check| check.get("ok").and_then(Value::as_bool) == Some(false))
        .map(|check| {
            let name = check.get("name").and_then(Value::as_str).unwrap_or("unknown");
            let reason = check
                .get("error")
                .or_else(|| check.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("failed");
            json!({
                "name": name,
                "reason": reason,
            })
        })
        .collect()
}

fn blocker_names(blockers: &[Value]) -> Vec<String> {
    blockers
        .iter()
        .filter_map(|blocker| blocker.get("name").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

fn yaml_path_str<'a>(value: &'a crate::model::yaml::Value, keys: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in keys {
        current = current.get(key)?;
    }
    current.as_str()
}

fn write_details_log(workspace: &std::path::Path, prefix: &str, value: &Value) -> Result<std::path::PathBuf, CliError> {
    let logs = workspace.join(".team").join("logs");
    std::fs::create_dir_all(&logs)?;
    let path = logs.join(format!("{prefix}-{}.json", timestamp_slug()));
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn timestamp_slug() -> String {
    chrono::Utc::now().timestamp().to_string()
}

pub(crate) fn count_dir_entries(path: &std::path::Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| entries.filter_map(std::result::Result::ok).count())
        .unwrap_or(0)
}

pub(crate) fn provider_doctor_checks() -> Value {
    let mut providers = serde_json::Map::new();
    for provider in [
        crate::provider::Provider::Claude,
        crate::provider::Provider::ClaudeCode,
        crate::provider::Provider::Codex,
        crate::provider::Provider::GeminiCli,
        crate::provider::Provider::Fake,
    ] {
        let adapter = crate::provider::get_adapter(provider);
        let name = provider_wire(provider);
        let version = adapter.version().unwrap_or_else(|error| error.to_string());
        providers.insert(
            name.to_string(),
            json!({
                "auth": adapter.auth_hint(crate::provider::AuthMode::Subscription),
                "command": provider_command(provider),
                "installed": adapter.is_installed(),
                "version": version,
            }),
        );
    }
    Value::Object(providers)
}

fn provider_command(provider: crate::provider::Provider) -> &'static str {
    match provider {
        crate::provider::Provider::Fake => "team-agent fake-worker",
        other => command_name(other),
    }
}
