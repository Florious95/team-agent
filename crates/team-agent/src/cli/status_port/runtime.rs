use super::*;

/// 0.5.41 Slice 3 (fault-invisibility-locate.md §5): single-source
/// runtime freshness projection consumed by status/diagnose. Read-
/// only over existing sources (`coordinator_health`, heartbeat
/// sidecar, watch state); computes NO transport calls of its own.
#[derive(Default, Clone)]
pub(crate) struct RuntimeFreshness {
    pub coordinator_service_available: bool,
    pub host_boot_stale: bool,
    pub host_boot_recorded: Option<String>,
    pub host_boot_current: Option<String>,
    pub provider_exited_agents: std::collections::BTreeSet<String>,
}

impl RuntimeFreshness {
    pub(super) fn host_boot_stale_reason(&self) -> Option<&'static str> {
        self.host_boot_stale.then_some("host_boot_mismatch")
    }
}

pub(crate) fn compute_runtime_freshness(
    workspace: &Path,
    state: &Value,
    health: &crate::coordinator::HealthReport,
) -> RuntimeFreshness {
    let workspace_path = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let heartbeat = crate::coordinator::read_coordinator_heartbeat(&workspace_path);
    let host_boot_recorded = heartbeat
        .as_ref()
        .and_then(|hb| hb.get("host_boot_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "unknown")
        .map(str::to_string);
    let host_boot_current = crate::coordinator::probe_host_boot_id();
    let host_boot_stale = match (host_boot_recorded.as_deref(), host_boot_current.as_deref()) {
        (Some(recorded), Some(current)) => recorded != current,
        _ => false,
    };
    // Collect worker-provider-exited agents from the coordinator
    // abnormal_exit_watch payload. Only the typed positive provider-exit
    // marker participates here: generic process/pane death diagnostics are
    // not proof that the provider exited. Read from top-level
    // `coordinator.abnormal_exit_watch`
    // OR the team-scoped mirror `teams.<key>.coordinator...`.
    let mut provider_exited_agents = std::collections::BTreeSet::new();
    for path in [
        "/coordinator/abnormal_exit_watch",
        &format!(
            "/teams/{}/coordinator/abnormal_exit_watch",
            state
                .get("active_team_key")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
    ] {
        if let Some(watch) = state.pointer(path).and_then(Value::as_object) {
            for (agent_id, entry) in watch {
                let exited =
                    entry.get("worker_provider_exited").and_then(Value::as_bool) == Some(true);
                if exited {
                    provider_exited_agents.insert(agent_id.clone());
                }
            }
        }
    }
    RuntimeFreshness {
        coordinator_service_available: health.service_available,
        host_boot_stale,
        host_boot_recorded,
        host_boot_current,
        provider_exited_agents,
    }
}

pub(super) fn build_runtime_status_block(
    coordinator_running: bool,
    undelivered: i64,
    tmux_session_missing: bool,
    freshness: &RuntimeFreshness,
) -> Value {
    let mut runtime = serde_json::Map::new();
    runtime.insert(
        "coordinator".to_string(),
        json!({"ok": coordinator_running}),
    );
    runtime.insert("undelivered".to_string(), json!(undelivered));
    // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): expose
    // typed issues on the runtime block so `status --json --detail`
    // can be scanned for `runtime_bindings_stale_after_boot` the
    // same way `diagnose` surfaces it. Hint precedence: session-
    // missing > host-boot-stale > coordinator-not-running-with-
    // backlog — most-actionable-first.
    let mut issues: Vec<Value> = Vec::new();
    if freshness.host_boot_stale {
        issues.push(json!({
            "id": "runtime_bindings_stale_after_boot",
            "recorded_host_boot_id": freshness.host_boot_recorded,
            "current_host_boot_id": freshness.host_boot_current,
        }));
    }
    if !issues.is_empty() {
        runtime.insert("issues".to_string(), Value::Array(issues));
    }
    if tmux_session_missing {
        runtime.insert(
            "hint".to_string(),
            json!("tmux session missing — run team-agent restart"),
        );
    } else if freshness.host_boot_stale {
        runtime.insert(
            "hint".to_string(),
            json!("runtime_bindings_stale_after_boot — run team-agent restart"),
        );
    } else if !coordinator_running && undelivered > 0 {
        runtime.insert(
            "hint".to_string(),
            json!(format!(
                "coordinator not running with {undelivered} undelivered — run team-agent restart"
            )),
        );
    }
    Value::Object(runtime)
}

/// Whether the coordinator HealthReport reflects a running tick loop. Used by the
/// runtime block + the hint gate.
pub(super) fn coordinator_status_running(health: &crate::coordinator::HealthReport) -> bool {
    // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): runtime
    // service predicate is `service_available`, not any-`Running` pid.
    // A stale-pid daemon is CoordinatorHealthStatus::Stale (or Running
    // with metadata_ok=false); a service-compatible newer daemon is
    // Running + service_available=true — send/diagnose already use
    // this same truth source, and status must agree.
    health.service_available
}

/// Count of messages currently sitting in delivery-able backlog
/// (accepted/pending/queued forms — not delivered / not failed / not refused).
/// owner_team_id scope honored when present.
pub(super) fn coordinator_health_value(health: crate::coordinator::HealthReport) -> Value {
    let expose_binary_drift = health.service_available && !health.metadata_ok;
    let binary_identity_relation = health.binary_identity_relation.as_str();
    let mut value = json!({
        "ok": health.ok,
        "status": coordinator_status_wire(health.status),
        "pid": health.pid.map(|p| p.get()),
        "metadata": health.metadata.map(|m| json!({
            "pid": m.pid.get(),
            "protocol_version": m.protocol_version,
            "message_store_schema_version": m.message_store_schema_version,
            "binary_path": m.binary_path,
            "binary_version": m.binary_version,
            "source": m.source,
            "updated_at": m.updated_at,
        })),
        "metadata_ok": health.metadata_ok,
        "metadata_mismatch_reason": health.metadata_mismatch_reason,
        "binary_path": health.current_binary_identity.binary_path,
        "binary_version": health.current_binary_identity.binary_version,
        "schema_ok": health.schema.ok,
        "schema_error": health.schema.error.map(|e| format!("{e:?}")),
        "schema": {
            "message_store_schema_version": health.schema.schema_version,
        },
        // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3):
        // `service_available` is now always exposed alongside `ok`
        // and `status` so status/diagnose consumers can share one
        // service-availability truth without keying on the legacy
        // `expose_binary_drift` conditional (that path only fired
        // for the newer-daemon-preserved shape). `binary_identity_
        // relation` moves to always-on for the same reason — RED3
        // scans the summary for `daemon_newer_than_caller`.
        "service_available": health.service_available,
        "binary_identity_relation": binary_identity_relation,
    });
    if expose_binary_drift {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("wire_metadata_ok".to_string(), Value::Bool(true));
            obj.insert("binary_identity_ok".to_string(), Value::Bool(false));
        }
    }
    value
}

pub(super) fn coordinator_status_wire(
    status: crate::coordinator::CoordinatorHealthStatus,
) -> &'static str {
    match status {
        crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
        crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
        crate::coordinator::CoordinatorHealthStatus::Running => "running",
        crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
    }
}
