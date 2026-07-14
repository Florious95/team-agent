//! diagnose/preflight/wait-ready CLI helpers.
use super::*;
use crate::provider::wire::{command_name, parse_provider, provider_wire};
use crate::transport::Transport;

/// 0.5.39 Slice 1 (tmux-server-death-locate §11.1 B): classify a tmux
/// transport error string into a diagnose issue id. When the underlying
/// tmux subprocess stderr contains `server exited unexpectedly`, the
/// tmux server itself crashed (upstream tmux 3.6a control-mode +
/// broadcast attach/detach jitter is one known trigger) — the physical
/// layer that disappeared is the whole server, not just this team's
/// session. Otherwise, callers keep the legacy `tmux_session_missing`
/// classification.
pub(crate) fn classify_tmux_server_error(error_text: &str) -> &'static str {
    if error_text.contains("server exited unexpectedly") {
        "tmux_server_crashed"
    } else {
        "tmux_session_missing"
    }
}

pub(crate) fn diagnose_runtime(state: &Value, backend: &dyn Transport) -> (Value, Value) {
    let mut issues = Vec::new();
    let mut repairs = Vec::new();
    for issue in crate::topology::diagnose_topology_issues(state, backend) {
        if let Some(id) = crate::topology::issue_id(&issue) {
            repairs.push(topology_repair_hint(id));
        }
        issues.push(issue);
    }

    if let Some(session_name) = state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        match backend.has_session(&crate::transport::SessionName::new(
            session_name.to_string(),
        )) {
            Ok(true) => {}
            Ok(false) => {
                issues.push(json!("tmux_session_missing"));
                repairs.push(recovery_hint(
                    session_name,
                    "tmux_session_missing",
                    "team-agent restart",
                ));
            }
            Err(error) => {
                // 0.5.39 Slice 1 (tmux-server-death-locate §11.1 B):
                // when the transport error stderr matches "server exited
                // unexpectedly", the physical layer that disappeared is
                // the tmux server itself — not just this team's session.
                // Surface `tmux_server_crashed` so the user's next
                // action is a coordinator/host-level recovery, not a
                // per-agent `restart <agent>`.
                let error_str = error.to_string();
                let issue_id = classify_tmux_server_error(&error_str);
                issues.push(json!(issue_id));
                let mut hint = recovery_hint(session_name, issue_id, "team-agent diagnose");
                if let Some(obj) = hint.as_object_mut() {
                    obj.insert("reason".to_string(), Value::String(error_str));
                }
                repairs.push(hint);
            }
        }
    }

    if !leader_receiver_attached(state) {
        issues.push(json!("leader_not_attached"));
        repairs.push(recovery_hint(
            state
                .get("session_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "leader_not_attached",
            "team-agent attach-leader",
        ));
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
                        "action_required": true,
                        "advisory": true,
                        "broken_class": "leader_provider_exited",
                        "hint_action": "team-agent restart",
                        "dedupe_key": "leader_provider_exited",
                        "action": format!(
                            "leader pane fell back to shell — provider `{provider_label}` exited; \
                             relaunch with `team-agent {provider_label}` to restart the provider"
                        ),
                    }));
                }
                crate::leader::LeaderProviderHealth::Unreachable => {
                    issues.push(json!("leader_provider_unreachable"));
                    repairs.push(recovery_hint(
                        state
                            .get("session_name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        "leader_provider_unreachable",
                        "team-agent claim-leader",
                    ));
                }
                crate::leader::LeaderProviderHealth::Alive => {}
            }
        }
    }

    if let Some(session_name) = state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if let Ok(windows) = backend.list_windows(&crate::transport::SessionName::new(
            session_name.to_string(),
        )) {
            if let Some(agents) = state.get("agents").and_then(Value::as_object) {
                for (agent_id, agent_state) in agents {
                    let provider = agent_state
                        .get("provider")
                        .and_then(Value::as_str)
                        .and_then(parse_provider)
                        .unwrap_or(crate::provider::Provider::Codex);
                    let rollout_path = agent_state
                        .get("rollout_path")
                        .and_then(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(crate::provider::RolloutPath::new);
                    let identity_probe =
                        crate::lifecycle::restart::session_identity_probe_for_agent(
                            &crate::model::ids::AgentId::new(agent_id.clone()),
                            provider,
                            rollout_path.as_ref(),
                        );
                    if identity_probe.identity_ok == Some(false) {
                        let issue = format!("session_identity_mismatch:{agent_id}");
                        issues.push(json!(issue));
                        repairs.push(json!({
                            "issue": format!("session_identity_mismatch:{agent_id}"),
                            "action": format!(
                                "run `team-agent restart --allow-fresh` to discard the poisoned session tuple for `{agent_id}`"
                            ),
                            "expected_agent_id": agent_id,
                            "embedded_agent_id": identity_probe.embedded_agent_id,
                            "rollout_path": identity_probe
                                .rollout_path
                                .map(|path| path.to_string_lossy().to_string()),
                        }));
                    }
                    let window = ["window", "window_name"]
                        .iter()
                        .find_map(|key| {
                            agent_state
                                .get(*key)
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                        })
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

pub(crate) fn diagnose_runtime_for_workspace(
    workspace: &std::path::Path,
    state: &Value,
    backend: &dyn Transport,
) -> (Value, Value) {
    let (mut issues, mut repairs) = diagnose_runtime(state, backend);
    append_legacy_snapshot_issue(workspace, state, &mut issues);
    append_coordinator_health_issue(workspace, state, &mut issues, &mut repairs);
    append_runtime_bindings_stale_after_boot_issue(workspace, state, &mut issues, &mut repairs);
    (issues, repairs)
}

/// 0.5.41 Slice 1 (fault-invisibility-locate.md §5/§6.2): read the
/// coordinator heartbeat sidecar for the last-observed host boot
/// identity and compare it against the current host boot. If they
/// differ, the pane/pid/session bindings in state predate the current
/// boot — surface `runtime_bindings_stale_after_boot` with a reused
/// `team-agent restart` hint. Read-only: no mutation. When the
/// heartbeat is missing OR the current host boot cannot be probed,
/// stay silent (per §8 risk note — do not guess).
fn append_runtime_bindings_stale_after_boot_issue(
    workspace: &std::path::Path,
    state: &Value,
    issues: &mut Value,
    repairs: &mut Value,
) {
    let workspace_path = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let Some(heartbeat) = crate::coordinator::read_coordinator_heartbeat(&workspace_path) else {
        return;
    };
    let Some(recorded_host_boot) = heartbeat
        .get("host_boot_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "unknown")
    else {
        return;
    };
    let Some(current_host_boot) = crate::coordinator::probe_host_boot_id() else {
        return;
    };
    if current_host_boot == recorded_host_boot {
        return;
    }
    // Only surface when state actually has pane/session bindings that
    // matter — otherwise there's nothing to be stale about.
    let has_bindings = state
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                agent
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            })
        });
    if !has_bindings {
        return;
    }
    let session_name = state
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if let Some(items) = issues.as_array_mut() {
        items.push(json!({
            "id": "runtime_bindings_stale_after_boot",
            "session_name": session_name,
            "recorded_host_boot_id": recorded_host_boot,
            "current_host_boot_id": current_host_boot,
            "details": "cached pane/pid/session bindings predate the current host boot; runtime facts are stale",
        }));
    }
    if let Some(items) = repairs.as_array_mut() {
        items.push(json!({
            "issue": "runtime_bindings_stale_after_boot",
            "action_required": true,
            "advisory": false,
            "hint_action": "team-agent restart",
            "action": format!(
                "runtime bindings for `{session_name}` predate the current host boot \
                 (recorded {recorded_host_boot}, current {current_host_boot}); rerun \
                 `team-agent restart` to rebuild the pane/pid/session tuple"
            ),
            "dedupe_key": "runtime_bindings_stale_after_boot",
        }));
    }
}

fn append_legacy_snapshot_issue(workspace: &std::path::Path, state: &Value, issues: &mut Value) {
    let Ok(Some(details)) = crate::leader::detect_dual_state_divergence(workspace, state) else {
        return;
    };
    if let Some(items) = issues.as_array_mut() {
        items.push(json!({
            "id": "legacy_snapshot_stale",
            "details": details,
        }));
    }
}

fn append_coordinator_health_issue(
    workspace: &std::path::Path,
    state: &Value,
    issues: &mut Value,
    repairs: &mut Value,
) {
    let workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let health = crate::coordinator::coordinator_health(&workspace);
    let Some(id) = coordinator_issue_id(state, &health) else {
        return;
    };
    if let Some(items) = issues.as_array_mut() {
        items.push(coordinator_issue_value(id, &health, workspace.as_path()));
    }
    if let Some(items) = repairs.as_array_mut() {
        items.push(coordinator_repair_hint(id, &health));
    }
}

fn coordinator_issue_id(
    state: &Value,
    health: &crate::coordinator::HealthReport,
) -> Option<&'static str> {
    match health.status {
        crate::coordinator::CoordinatorHealthStatus::Stale
        | crate::coordinator::CoordinatorHealthStatus::InvalidPid => {
            Some("coordinator_unavailable")
        }
        crate::coordinator::CoordinatorHealthStatus::Running => {
            if !health.metadata_ok {
                if health.service_available
                    && matches!(
                        health.binary_identity_relation,
                        crate::coordinator::CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller
                    )
                {
                    return None;
                }
                return match health.metadata_mismatch_reason.as_deref() {
                    Some(
                        "binary_identity_missing"
                        | "binary_version_mismatch"
                        | "binary_path_mismatch",
                    ) => Some("coordinator_stale_identity"),
                    Some(_) | None => Some("coordinator_unavailable"),
                };
            }
            if !health.schema.ok {
                Some("coordinator_schema_incompatible")
            } else {
                None
            }
        }
        crate::coordinator::CoordinatorHealthStatus::Missing => {
            coordinator_expected(state).then_some("coordinator_unavailable")
        }
    }
}

fn coordinator_issue_value(
    id: &str,
    health: &crate::coordinator::HealthReport,
    workspace: &std::path::Path,
) -> Value {
    json!({
        "id": id,
        "status": coordinator_status_wire(health.status),
        "pid": health.pid.map(|pid| pid.get()),
        "metadata_ok": health.metadata_ok,
        "metadata_mismatch_reason": health.metadata_mismatch_reason.clone(),
        "process_running": health.process_running,
        "wire_metadata_ok": health.wire_metadata_ok,
        "binary_identity_ok": health.binary_identity_ok,
        "binary_identity_relation": health.binary_identity_relation.as_str(),
        "service_available": health.service_available,
        "binary_path": health.current_binary_identity.binary_path.clone(),
        "binary_version": health.current_binary_identity.binary_version.clone(),
        "schema_ok": health.schema.ok,
        "coordinator_log": crate::coordinator::coordinator_log_path(
            &crate::coordinator::WorkspacePath::new(workspace.to_path_buf())
        )
        .to_string_lossy()
        .to_string(),
    })
}

fn coordinator_repair_hint(id: &str, _health: &crate::coordinator::HealthReport) -> Value {
    let hint_action = match id {
        "coordinator_schema_incompatible" => "team-agent doctor --fix-schema --json",
        _ => "team-agent restart",
    };
    json!({
        "issue": id,
        "action_required": true,
        "advisory": true,
        "broken_class": id,
        "hint_action": hint_action,
        "dedupe_key": id,
        "action": format!("{hint_action} # diagnose only reports coordinator health; it does not start, stop, or rotate the coordinator"),
    })
}

fn coordinator_expected(state: &Value) -> bool {
    state
        .get("session_name")
        .and_then(Value::as_str)
        .is_some_and(|session| !session.is_empty())
        || state
            .get("agents")
            .and_then(Value::as_object)
            .is_some_and(|agents| !agents.is_empty())
}

fn coordinator_status_wire(status: crate::coordinator::CoordinatorHealthStatus) -> &'static str {
    match status {
        crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
        crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
        crate::coordinator::CoordinatorHealthStatus::Running => "running",
        crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
    }
}

fn topology_repair_hint(issue: &str) -> Value {
    json!({
        "issue": issue,
        "action_required": true,
        "advisory": true,
        "broken_class": issue,
        "hint_action": "team-agent diagnose --json",
        "dedupe_key": issue,
        "action": "repair the tmux topology mismatch, then rerun team-agent restart",
    })
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

fn recovery_hint(team: &str, broken_class: &str, hint_action: &str) -> Value {
    json!({
        "issue": broken_class,
        "action_required": true,
        "advisory": true,
        "broken_class": broken_class,
        "hint_action": hint_action,
        "dedupe_key": format!("{team}:{broken_class}"),
        "action": format!(
            "{hint_action} # alternatives: team-agent restart; team-agent claim-leader; team-agent takeover; team-agent quick-start; team-agent attach-leader"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_codex_identity_rollout(
        workspace: &std::path::Path,
        session_id: &str,
        embedded_agent_id: &str,
    ) -> std::path::PathBuf {
        let path = workspace.join("rollout-poison.jsonl");
        let text = format!(
            "{{\"session_meta\":{{\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\"}}}}}}\n\
             {{\"type\":\"turn_context\",\"payload\":{{}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"content\":[{{\"type\":\"input_text\",\"text\":\"You are Team Agent worker `{embedded_agent_id}` with role `fixture`.\"}}]}}}}\n",
            workspace.to_string_lossy()
        );
        std::fs::write(&path, text).expect("write rollout");
        path
    }

    fn coordinator_health_fixture(
        status: crate::coordinator::CoordinatorHealthStatus,
        metadata_mismatch_reason: Option<&str>,
        schema_ok: bool,
    ) -> crate::coordinator::HealthReport {
        crate::coordinator::HealthReport {
            ok: matches!(status, crate::coordinator::CoordinatorHealthStatus::Running)
                && metadata_mismatch_reason.is_none()
                && schema_ok,
            status,
            pid: Some(crate::coordinator::Pid::new(std::process::id())),
            metadata: None,
            metadata_ok: metadata_mismatch_reason.is_none(),
            process_running: matches!(status, crate::coordinator::CoordinatorHealthStatus::Running),
            wire_metadata_ok: metadata_mismatch_reason.is_none(),
            binary_identity_ok: metadata_mismatch_reason.is_none(),
            binary_identity_relation: crate::coordinator::CoordinatorBinaryIdentityRelation::Same,
            service_available: matches!(
                status,
                crate::coordinator::CoordinatorHealthStatus::Running
            ) && metadata_mismatch_reason.is_none()
                && schema_ok,
            metadata_mismatch_reason: metadata_mismatch_reason.map(ToString::to_string),
            current_binary_identity: crate::coordinator::CoordinatorBinaryIdentity {
                binary_path: "/current/team-agent".to_string(),
                binary_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            schema: crate::coordinator::SchemaHealth {
                ok: schema_ok,
                schema_version: crate::db::schema::SCHEMA_VERSION,
                error: None,
                action: None,
            },
        }
    }

    #[test]
    fn coordinator_issue_id_maps_stale_pid_to_unavailable() {
        let health = coordinator_health_fixture(
            crate::coordinator::CoordinatorHealthStatus::Stale,
            None,
            true,
        );
        assert_eq!(
            coordinator_issue_id(&json!({"session_name": "team-fixture"}), &health),
            Some("coordinator_unavailable")
        );
    }

    #[test]
    fn coordinator_issue_id_maps_binary_identity_to_stale_identity() {
        let health = coordinator_health_fixture(
            crate::coordinator::CoordinatorHealthStatus::Running,
            Some("binary_version_mismatch"),
            true,
        );
        assert_eq!(
            coordinator_issue_id(&json!({"session_name": "team-fixture"}), &health),
            Some("coordinator_stale_identity")
        );
    }

    #[test]
    fn coordinator_issue_id_maps_schema_failure_to_schema_incompatible() {
        let health = coordinator_health_fixture(
            crate::coordinator::CoordinatorHealthStatus::Running,
            None,
            false,
        );
        assert_eq!(
            coordinator_issue_id(&json!({"session_name": "team-fixture"}), &health),
            Some("coordinator_schema_incompatible")
        );
    }

    #[test]
    fn diagnose_surfaces_codex_session_identity_mismatch() {
        let workspace =
            std::env::temp_dir().join(format!("ta-diagnose-crossbind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        let rollout = write_codex_identity_rollout(
            &workspace,
            "019f3327-c35a-7023-b3cd-1bea93a7a157",
            "ios-dev",
        );
        let state = json!({
            "session_name": "team-fixture",
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "pane_id": "%leader",
                "provider": "codex"
            },
            "agents": {
                "frontend": {
                    "provider": "codex",
                    "status": "running",
                    "session_id": "019f3327-c35a-7023-b3cd-1bea93a7a157",
                    "rollout_path": rollout.to_string_lossy(),
                    "captured_at": "2026-07-05T17:04:04Z",
                    "captured_via": "fs_watch",
                    "spawn_cwd": workspace.to_string_lossy(),
                    "window": "frontend"
                }
            }
        });
        let backend = crate::transport::test_support::OfflineTransport::new()
            .with_session_present(true)
            .with_windows(vec![crate::transport::WindowName::new("frontend")]);
        let (issues, repairs) = diagnose_runtime(&state, &backend);
        assert!(
            issues
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item.as_str()
                    == Some("session_identity_mismatch:frontend"))),
            "diagnose must surface session_identity_mismatch for poisoned Codex tuples; issues={issues}"
        );
        assert!(
            repairs.as_array().is_some_and(|items| items.iter().any(|item| {
                item.get("issue").and_then(Value::as_str)
                    == Some("session_identity_mismatch:frontend")
            })),
            "diagnose should include an explicit repair hint for the mismatched tuple; repairs={repairs}"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }
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
            next_actions.push(json!(
                "fix TEAM.md and role front matter, then run preflight again"
            ));
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
    let profile_dir_exists = workspace
        .join(".team")
        .join("current")
        .join("profiles")
        .exists()
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
        format!(
            "preflight found blockers: {}",
            blocker_names(&blockers).join(", ")
        )
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

pub(crate) fn build_profile_smoke_check_for_team(
    team: &std::path::Path,
) -> Result<Value, CliError> {
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
    let timeout = if timeout.is_finite() && timeout > 0.0 {
        timeout
    } else {
        0.0
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    let mut readiness;
    let mut state_read_error: Option<String>;
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
            vec![json!(
                "inspect .team/runtime/state.json (corrupt or unreadable) and retry"
            )],
        )
    } else if awaiting_trust {
        (
            false,
            "pending",
            "awaiting_trust_prompt",
            "workers are awaiting trust prompt",
            vec![json!(
                "answer the provider trust prompt, then run wait-ready again"
            )],
        )
    } else if ready {
        (true, "ready", "ready", "workers ready", Vec::new())
    } else if readiness
        .get("session_capture_complete")
        .and_then(Value::as_bool)
        == Some(false)
    {
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
            vec![json!(
                "inspect team-agent diagnose output and worker terminals"
            )],
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
    let Some(session_name) = state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let session_name_owned = session_name.to_string();
    // 0.5.x Phase 1d Batch 3: factory-routed read path. conpty state
    // hits the shim; tmux state hits the persisted endpoint (byte-
    // equivalent). Diagnose no longer reports `tmux_session_missing`
    // for a conpty team just because it can't find a tmux session on
    // the workspace socket (design §Batch 3 Verification anchor).
    let resolved = crate::transport_factory::resolve_read_only_transport(
        workspace,
        Some(state),
        crate::transport_factory::TransportPurpose::Diagnose,
    );
    let present = match resolved {
        Ok(r) => r
            .backend
            .has_session(&crate::transport::SessionName::new(session_name_owned))
            .unwrap_or(false),
        Err(_) => false,
    };
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
            receiver.get("status").and_then(Value::as_str) == Some("attached")
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
        mcp_ready = !agents.is_empty() && agents.values().all(agent_mcp_ready);
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
        incomplete_sessions =
            crate::session_capture::incomplete_interacted_resumable_agent_ids(state);
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
    let mut rows = stmt
        .query([])
        .map_err(|e| CliError::Runtime(e.to_string()))?;
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
            agent
                .get("pane_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
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
            let name = check
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
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

fn write_details_log(
    workspace: &std::path::Path,
    prefix: &str,
    value: &Value,
) -> Result<std::path::PathBuf, CliError> {
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
