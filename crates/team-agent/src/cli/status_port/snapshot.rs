use super::agents::{enrich_agents, tmux_session_present};
use super::compact::compact_status;
use super::runtime::{
    build_runtime_status_block, compute_runtime_freshness, coordinator_health_value,
    coordinator_status_running,
};
use super::store::{
    agent_health, count_undelivered_backlog, latest_result_summaries, message_counts,
    pending_leader_notifications, queued_messages, result_counts,
};
use crate::cli::CliError;
use crate::state::projection::OwnerTeamResolution;
use serde_json::{json, Value};
use std::path::Path;

pub(crate) struct RuntimeSnapshot {
    full: Value,
}

impl RuntimeSnapshot {
    pub(crate) fn assemble(
        workspace: &Path,
        state: &Value,
        owner_team_id: Option<&str>,
    ) -> Result<Self, CliError> {
        let resolved_owner_team_id = resolve_status_owner_team(workspace, owner_team_id)?;
        let owner_team_id = resolved_owner_team_id.as_deref().or(owner_team_id);
        let health = crate::coordinator::coordinator_health(
            &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
        );
        let store = crate::message_store::MessageStore::open(workspace)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let conn = crate::db::schema::open_db(store.db_path())
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        // B-5 / 036b N38 explicable — status 出口 runtime 块:把 coordinator_health
        // (现状)+ undelivered backlog count 一起暴露;coordinator not running ∧
        // backlog>0 才挂 down-hint(anti-nag)。auto-recovery 不做(user 已裁)。
        let coordinator_running = coordinator_status_running(&health);
        let undelivered_backlog = count_undelivered_backlog(&conn, owner_team_id)?;
        let session_name = state.get("session_name").cloned().unwrap_or(Value::Null);
        let tmux_present = tmux_session_present(workspace, state, session_name.as_str());
        // 0.5.41 Slice 3 (fault-invisibility-locate.md §5/§6.3): resolve
        // RuntimeFreshness once and thread it through the runtime block
        // and per-agent enrichment. This is the single-source read that
        // makes host-boot / coordinator / provider-exit staleness win
        // over cached state.agents and DB agent_health.
        let freshness = compute_runtime_freshness(workspace, state, &health);
        let runtime_block = build_runtime_status_block(
            coordinator_running,
            undelivered_backlog,
            !tmux_present,
            &freshness,
        );
        let agents = enrich_agents(workspace, state, tmux_present, &freshness);
        let tasks = state.get("tasks").cloned().unwrap_or_else(|| json!([]));
        let leader_receiver = state
            .get("leader_receiver")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let is_external_leader = crate::state::projection::state_is_external_leader(state);
        let leader_topology = if is_external_leader {
            "external"
        } else {
            "managed"
        };
        let leader_attach_command = if is_external_leader {
            None
        } else {
            let window_name = state
                .pointer("/leader_receiver/window_name")
                .and_then(Value::as_str)
                .unwrap_or("leader");
            session_name.as_str().and_then(|session| {
                // Bug #7 (gate review §6): build the attach command from the
                // SAME endpoint the readiness probe uses (state's persisted
                // tmux_endpoint/tmux_socket), so the printed command matches
                // where the session actually lives.
                crate::tmux_backend::attach_command_for_runtime_state_or_workspace(
                    workspace,
                    Some(state),
                    &crate::transport::SessionName::new(session.to_string()),
                    window_name,
                )
            })
        };
        let mut readiness_state = state.clone();
        if let Some(obj) = readiness_state.as_object_mut() {
            obj.insert(
                "tmux_session_present".to_string(),
                serde_json::json!(tmux_present),
            );
        }
        let readiness = crate::cli::diagnose::wait_readiness(&readiness_state);
        let full = crate::redaction::redact_external_value(&json!({
            "ok": true,
            "team": state.pointer("/leader/id").cloned().unwrap_or_else(|| json!("leader")),
            "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
            "leader_topology": leader_topology,
            "is_external_leader": is_external_leader,
            "leader_attach_command": leader_attach_command,
            "leader_client": state.get("leader_client").cloned().unwrap_or(Value::Null),
            "tmux_session_present": tmux_present,
            "all_spawned": readiness.get("all_spawned").cloned().unwrap_or(Value::Bool(false)),
            "all_attached_receiver": readiness.get("all_attached_receiver").cloned().unwrap_or(Value::Bool(true)),
            "all_resumable_have_session": readiness.get("all_resumable_have_session").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_complete": readiness.get("session_capture_complete").cloned().unwrap_or(Value::Bool(true)),
            "session_capture_incomplete": readiness.get("session_capture_incomplete").cloned().unwrap_or(Value::Bool(false)),
            "incomplete_session_capture_agents": readiness.get("incomplete_session_capture_agents").cloned().unwrap_or_else(|| json!([])),
            "pending_session_agent_ids": readiness.get("pending_session_agent_ids").cloned().unwrap_or_else(|| json!([])),
            "leader_receiver": leader_receiver,
            "teams": state.get("teams").cloned().unwrap_or_else(|| json!({})),
            "agents": agents,
            "agent_health": agent_health(&conn, owner_team_id)?,
            "tasks": tasks,
            "messages": message_counts(&conn, owner_team_id)?,
            "queued_messages": queued_messages(&conn, owner_team_id, 8)?,
            "pending_leader_notifications": pending_leader_notifications(&conn, owner_team_id, 8)?,
            "results": result_counts(&conn, owner_team_id)?,
            "latest_results": latest_result_summaries(&store, owner_team_id)?,
            "readiness": readiness,
            "coordinator": coordinator_health_value(health),
            "runtime": runtime_block,
            "reminder": crate::cli::STATUS_REMINDER,
            "last_events": Value::Array(
                crate::event_log::EventLog::new(workspace)
                    .tail(10)
                    .map_err(|e| CliError::Runtime(e.to_string()))?,
            ),
        }));
        Ok(Self { full })
    }

    pub(crate) fn full(&self) -> &Value {
        &self.full
    }

    pub(crate) fn into_full(self) -> Value {
        self.full
    }

    pub(crate) fn compact(&self) -> Value {
        compact_status(self.full.clone())
    }
}

pub(super) fn read_runtime_state(workspace: &Path) -> Value {
    crate::state::repository::StateRepository::new(workspace)
        .load_workspace_if_exists_without_migrations()
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({}))
}

pub(super) fn resolve_status_owner_team(
    workspace: &Path,
    owner_team_id: Option<&str>,
) -> Result<Option<String>, CliError> {
    let Some(requested) = owner_team_id.filter(|team| !team.is_empty()) else {
        return Ok(None);
    };
    let state = read_runtime_state(workspace);
    match crate::state::projection::resolve_owner_team_id(&state, requested) {
        OwnerTeamResolution::Canonical(canonical) => Ok(Some(canonical)),
        OwnerTeamResolution::LegacyAlias {
            requested,
            canonical,
        } => {
            let log = crate::event_log::EventLog::new(workspace);
            crate::messaging::delivery::normalize_owner_team_id_rows(
                workspace,
                &requested,
                &canonical,
                None,
                Some(&log),
            )
            .map_err(CliError::from)?;
            Ok(Some(canonical))
        }
        OwnerTeamResolution::Unresolved { .. } | OwnerTeamResolution::Ambiguous { .. } => Ok(None),
    }
}
