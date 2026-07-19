use super::*;

pub(super) fn agent_window(agent_id: &str, agent_state: &Value) -> String {
    ["window", "window_name"]
        .iter()
        .find_map(|key| {
            agent_state
                .get(*key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(agent_id)
        .to_string()
}

pub(super) fn enrich_agents(
    workspace: &Path,
    state: &Value,
    tmux_session_present: bool,
    freshness: &RuntimeFreshness,
) -> Value {
    let agents = state.get("agents");
    let Some(Value::Object(input)) = agents else {
        return json!({});
    };
    let team_dir = state
        .get("team_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.to_path_buf());
    let mut out = Map::new();
    for (agent_id, value) in input {
        match value {
            Value::Object(obj) => {
                let mut enriched = obj.clone();
                apply_effective_role_projection(&team_dir, agent_id, &mut enriched);
                enriched.insert(
                    "interacted".to_string(),
                    Value::String(interacted_marker(obj.get("first_send_at"))),
                );
                // 0.5.41 Slice 3: order stale sources most-authoritative-first.
                // Host boot mismatch wins because it invalidates all cached
                // pane/pid/session facts. Provider-exit marker wins over
                // pane liveness because the wrapper leaves an interactive
                // shell live. Coordinator unavailability without stronger
                // live provider proof means DB agent_health rows are stale.
                // Tmux session missing keeps its existing legacy path so
                // pre-0.5.41 tests remain byte-identical when no new
                // signal fires.
                let has_pane_binding = agent_has_pane_fact(&Value::Object(obj.clone()));
                // 0.5.41 Slice 3 (fault-invisibility-locate.md §5 point 3
                // + §9 RED4 live-provider guard): the wrapper-era pane
                // liveness cannot prove provider liveness — but a state
                // `pane_current_command` that MATCHES the agent's provider
                // IS positive proof (the abnormal.rs classifier writes it
                // when the pane's foreground command is the provider CLI).
                // When that positive proof is present, no stale downgrade
                // fires here so the agent renders as working.
                let provider_command_positive_proof = provider_current_command_matches(obj);
                // 0.5.41 Slice 3 (0.5.35 R4 regression guard): when the
                // runtime classifier has already written canonical
                // `worker_state=UNKNOWN` / `activity.status=uncertain`,
                // that is the authoritative honest observation — do NOT
                // reclassify it as `coordinator_unavailable` stale (which
                // would land it in the Stopped bucket instead of Unknown).
                // Host-boot mismatch and provider-exited marker are
                // stronger, more specific signals and still win.
                let canonical_unknown = agent_canonical_worker_state_is_unknown(obj);
                let new_reason = if freshness.host_boot_stale && has_pane_binding {
                    freshness.host_boot_stale_reason()
                } else if freshness.provider_exited_agents.contains(agent_id) {
                    Some("worker_provider_exited")
                } else if !freshness.coordinator_service_available
                    && has_pane_binding
                    && !provider_command_positive_proof
                    && !canonical_unknown
                {
                    Some("coordinator_unavailable")
                } else {
                    None
                };
                let legacy_reason = if provider_command_positive_proof {
                    None
                } else {
                    stale_reason_for_agent(&Value::Object(obj.clone()), tmux_session_present)
                };
                let reason = new_reason.or(legacy_reason);
                if let Some(reason) = reason {
                    enriched.insert("stale".to_string(), Value::Bool(true));
                    enriched.insert(
                        "stale_reason".to_string(),
                        Value::String(reason.to_string()),
                    );
                    // Downgrade cached BUSY/working when the stale source is
                    // one of the new authoritative signals OR the pre-existing
                    // session-missing signal. Legacy code only downgraded on
                    // !tmux_session_present; that let host_boot / provider-
                    // exit / coord-unavailable stale rows keep raw=running.
                    let is_new_signal = matches!(
                        reason,
                        "host_boot_mismatch" | "worker_provider_exited" | "coordinator_unavailable"
                    );
                    if !tmux_session_present || is_new_signal {
                        downgrade_stale_agent(&mut enriched);
                    }
                }
                out.insert(agent_id.clone(), Value::Object(enriched));
            }
            _ => {
                out.insert(agent_id.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

pub(super) fn apply_effective_role_projection(
    team_dir: &Path,
    agent_id: &str,
    agent: &mut Map<String, Value>,
) {
    let role_file = agent
        .get("dynamic_role_file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| team_dir.join("agents").join(format!("{agent_id}.md")));
    let Ok((meta, _)) = crate::compiler::read_front_matter(&role_file) else {
        return;
    };
    let Some(meta) = yaml_map(&meta) else {
        return;
    };
    if let Some(provider) = yaml_str(meta, "provider").filter(|value| !value.is_empty()) {
        agent.insert("provider".to_string(), Value::String(provider.to_string()));
        agent.insert(
            "provider_source".to_string(),
            Value::String("role".to_string()),
        );
    }
    if let Some(model) = yaml_str(meta, "model").filter(|value| !value.is_empty()) {
        if agent.get("model").and_then(Value::as_str) != Some(model) {
            agent.insert("model_stale".to_string(), Value::Bool(true));
        }
        agent.insert("model".to_string(), Value::String(model.to_string()));
        agent.insert(
            "model_source".to_string(),
            Value::String("role".to_string()),
        );
    }
}

pub(super) fn yaml_map(
    value: &crate::model::yaml::Value,
) -> Option<&Vec<(String, crate::model::yaml::Value)>> {
    match value {
        crate::model::yaml::Value::Map(items) => Some(items),
        _ => None,
    }
}

pub(super) fn yaml_str<'a>(
    items: &'a [(String, crate::model::yaml::Value)],
    key: &str,
) -> Option<&'a str> {
    items.iter().find_map(|(name, value)| {
        if name == key {
            match value {
                crate::model::yaml::Value::Str(value) => Some(value.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

pub(super) fn downgrade_stale_agent(agent: &mut Map<String, Value>) {
    let raw = agent
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(raw.as_str(), "running" | "busy" | "working" | "idle") {
        agent.insert("status".to_string(), Value::String("stopped".to_string()));
    }
    let worker_state = agent
        .get("worker_state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(worker_state.as_str(), "RUNNING" | "BUSY" | "PROBABLY_IDLE") {
        agent.insert(
            "worker_state".to_string(),
            Value::String("DEAD".to_string()),
        );
    }
}

pub(super) fn stale_reason_for_agent(
    agent: &Value,
    tmux_session_present: bool,
) -> Option<&'static str> {
    let pane_dead = !tmux_session_present && agent_has_pane_fact(agent);
    let process_dead =
        agent_process_dead(agent) || (!tmux_session_present && agent_has_process_fact(agent));
    match (pane_dead, process_dead) {
        (true, true) => Some("both"),
        (false, true) => Some("process_dead"),
        (true, false) => Some("pane_dead"),
        (false, false) => None,
    }
}

/// 0.5.41 Slice 3 (0.5.35 R4 regression guard): true when the agent
/// row carries the canonical `worker_state=UNKNOWN` OR
/// `activity.status=uncertain` observation the runtime classifier
/// writes. Used to skip the coordinator-unavailable stale mark
/// (see `enrich_agents`) so the pre-existing R4 rendering (UNKNOWN
/// beats WORKING) is preserved.
pub(super) fn agent_canonical_worker_state_is_unknown(
    agent: &serde_json::Map<String, Value>,
) -> bool {
    let worker_state_unknown = agent
        .get("worker_state")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("UNKNOWN"));
    let activity_uncertain = agent
        .get("activity")
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("uncertain"));
    worker_state_unknown || activity_uncertain
}

/// 0.5.41 Slice 3 (fault-invisibility-locate.md §9 RED4 live-provider
/// guard): true when the agent row carries a `pane_current_command`
/// that matches the agent's provider CLI. This is positive proof
/// the provider is the pane's foreground process — the abnormal.rs
/// classifier writes this field after the marker/current-command
/// check clears. When true, stale-downgrade paths in
/// `enrich_agents` skip so the row keeps its BUSY/working state.
pub(super) fn provider_current_command_matches(agent: &serde_json::Map<String, Value>) -> bool {
    let Some(command) = agent
        .get("pane_current_command")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let Some(provider_wire) = agent.get("provider").and_then(Value::as_str) else {
        return false;
    };
    let Some(provider) = crate::provider::wire::parse_provider(provider_wire) else {
        return false;
    };
    crate::leader::command_matches_provider(provider, command)
}

pub(super) fn agent_has_pane_fact(agent: &Value) -> bool {
    ["pane_id", "window", "window_name"].iter().any(|key| {
        agent
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    })
}

pub(super) fn agent_has_process_fact(agent: &Value) -> bool {
    agent.get("pid").and_then(Value::as_i64).is_some()
        || agent.get("process_started").and_then(Value::as_bool) == Some(true)
        || agent
            .get("provider_process_dead")
            .and_then(Value::as_bool)
            .is_some()
        || agent
            .get("process_liveness")
            .and_then(Value::as_str)
            .is_some()
}

pub(super) fn agent_process_dead(agent: &Value) -> bool {
    if agent.get("provider_process_dead").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    ["process_liveness", "worker_state"].iter().any(|key| {
        agent
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(is_dead_process_state)
    })
}

pub(super) fn is_dead_process_state(value: &str) -> bool {
    matches!(
        value,
        "dead" | "missing" | "stopped" | "exited" | "terminated"
    )
}

pub(super) fn interacted_marker(value: Option<&Value>) -> String {
    let Some(raw) = value.and_then(Value::as_str) else {
        return "never".to_string();
    };
    if raw.is_empty() {
        return "never".to_string();
    }
    if chrono::DateTime::parse_from_rfc3339(raw).is_ok() {
        raw.to_string()
    } else {
        "never".to_string()
    }
}

pub(super) fn tmux_session_present(
    workspace: &Path,
    state: &Value,
    session_name: Option<&str>,
) -> bool {
    // Bug #7 (prerelease 0.4.0 gate review §6): probe the SAME endpoint
    // the runtime actually uses (state.tmux_endpoint / tmux_socket), not
    // the workspace-hash socket. When state has no persisted endpoint,
    // fall back to workspace — preserves legacy behavior. wait_readiness
    // formula unchanged per 不可改项; only the input signal is fixed.
    let Some(name) = session_name else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let run_ws = crate::model::paths::canonical_run_workspace(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf());
    // 0.5.x Phase 1d Batch 3: route through the factory so a
    // conpty team does NOT get its `has_session` probe served by a
    // tmux backend (which would always return false and drive the
    // reader into a false `tmux_session_missing` state — design
    // §Batch 3 Verification anchor). Tmux teams see byte-equivalent
    // behavior because factory Layer 3 (legacy tmux endpoint) uses
    // the same `tmux_backend_for_runtime_state_or_workspace` shape.
    let resolved = crate::transport_factory::resolve_read_only_transport(
        &run_ws,
        Some(state),
        crate::transport_factory::TransportPurpose::Status,
    );
    match resolved {
        Ok(r) => r
            .backend
            .has_session(&crate::transport::SessionName::new(name))
            .unwrap_or(false),
        Err(_) => {
            // Factory refused (e.g. explicit conpty without a
            // resolvable team_key). Honest: return false rather
            // than pretend a tmux session exists.
            false
        }
    }
}
