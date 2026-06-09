//! step 14a · mcp_server::tools — `TeamOrchestratorTools`, the 12 typed handlers.

use std::path::{Path, PathBuf};

use serde_json::Value;

// ── REUSE: step 2 model (ids + normalized-envelope value enums) ─────────────
use crate::model::enums::ResultStatus;
use crate::model::ids::{AgentId, TaskId, TeamKey};

// ── REUSE: step 4 event_log / step 7 message_store ──────────────────────────
use crate::event_log::EventLog;

// ── REUSE: step 5 state persist / projection ────────────────────────────────
use crate::state::persist::{load_runtime_state, save_runtime_state};

// ── REUSE: step 11 messaging delegate surface ───────────────────────────────
use crate::messaging::{self, MessageTarget, SendOptions};

use super::helpers::{
    delivery_outcome_value, ensure_object, enum_value, insert_array, is_worker_recipient,
    json_dumps_default, latest_task_for_assignee, non_empty_string, normalized_envelope_value, object_fields,
    requires_ack_for_target, tool_runtime_error,
};
use super::normalize::{compact_tool_result, normalize_report_envelope};
use super::types::{Scope, SendOutcome, ToolError, ToolErrorReason, ToolOk, ToolResult, VisiblePeers};

// ═══════════════════════════════════════════════════════════════════════════
// TeamOrchestratorTools (tools.py:72) — the 12 typed tool handlers.
// Identity/scope anchored on spawn-time env (TEAM_AGENT_ID / TEAM_AGENT_OWNER_TEAM_ID);
// every handler delegates to runtime/MessageStore/EventLog. These ARE the
// contract-callable behavioral entry fns.
// ═══════════════════════════════════════════════════════════════════════════

/// `TeamOrchestratorTools` (`tools.py:72-82`). Scope anchored on spawn-time env: no
/// candidate scan of state/messages/runtime agents (C13-C17). Constructed with the
/// workspace; `agent_id`/`owner_team_id` are captured from env at construction.
pub struct TeamOrchestratorTools {
    /// Resolved (`workspace.resolve()`) workspace root.
    workspace: PathBuf,
    /// `TEAM_AGENT_ID` — the sender identity anchor (`None` when absent → `"unknown"`).
    agent_id: Option<AgentId>,
    /// `TEAM_AGENT_OWNER_TEAM_ID` — the scope anchor (`None` → legacy single-team).
    owner_team_id: Option<TeamKey>,
}

impl TeamOrchestratorTools {
    /// `__init__(workspace)` (`tools.py:79-82`): resolve workspace, read identity/scope
    /// from `TEAM_AGENT_ID` / `TEAM_AGENT_OWNER_TEAM_ID` env (`_text` empties → None).
    pub fn new(workspace: &Path) -> Self {
        let agent_id = std::env::var("TEAM_AGENT_ID")
            .ok()
            .and_then(|s| non_empty_string(&s).map(ToString::to_string))
            .map(AgentId::new);
        let owner_team_id = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
            .ok()
            .and_then(|s| non_empty_string(&s).map(ToString::to_string))
            .map(TeamKey::new);
        Self::with_identity(workspace, agent_id, owner_team_id)
    }

    /// Test/explicit-injection constructor: bind identity/scope directly instead of
    /// reading env (so contracts can exercise scoped behavior deterministically).
    pub fn with_identity(workspace: &Path, agent_id: Option<AgentId>, owner_team_id: Option<TeamKey>) -> Self {
        Self {
            workspace: std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf()),
            agent_id,
            owner_team_id,
        }
    }

    /// `assign_task` (`tools.py:84-133`): C8 Family-B task-view reconcile then deliver.
    /// Resolves team key from owner-team env (or `active_team_key`), appends or
    /// field-updates the task in state, then delegates delivery to
    /// [`Self::send_message`] and compacts the result.
    pub fn assign_task(&self, task: &Value, message: Option<&str>) -> ToolResult {
        self.validate_rpc_scope_args("assign_task", task)?;
        let Some(task_obj) = task.as_object() else {
            return Err(ToolError::new(
                ToolErrorReason::InvalidToolArguments,
                "assign_task task must be an object",
                "ValueError",
            ));
        };
        let Some(task_id) = task.get("id").and_then(Value::as_str).and_then(non_empty_string) else {
            return Err(ToolError::new(
                ToolErrorReason::InvalidToolArguments,
                "assign_task task.id is required",
                "ValueError",
            ));
        };
        let Some(assignee) = task
            .get("assignee")
            .and_then(Value::as_str)
            .and_then(non_empty_string)
        else {
            return Err(ToolError::new(
                ToolErrorReason::InvalidToolArguments,
                "assign_task task.assignee is required",
                "ValueError",
            ));
        };

        let task_value = Value::Object(task_obj.clone());
        let mut state = load_runtime_state(&self.workspace).map_err(tool_runtime_error)?;
        ensure_object(&mut state);
        let team_key = self
            .canonical_owner_team_key()?
            .map(|team| team.as_str().to_string())
            .or_else(|| assignment_team_key(&state));
        reconcile_assigned_task(&mut state, team_key.as_deref(), &task_value);
        save_runtime_state(&self.workspace, &state).map_err(tool_runtime_error)?;

        let content = assignment_message(task, message);
        let out = self.send_message(
            &MessageTarget::Single(assignee.to_string()),
            &content,
            Some(task_id),
            None,
            None,
            None,
        )?;
        compact_tool_result(&out.to_value())
    }

    /// `send_message` (`tools.py:135-183`): C14/C15/C17 scope resolution.
    ///   - sender = explicit / `TEAM_AGENT_ID` env / `"unknown"` (no candidate scan).
    ///   - `requires_ack` defaults from target (`_requires_ack_for_target`).
    ///   - C23 cross-team pre-refusal ([`Self::refuse_cross_team_peer`]) before any
    ///     runtime call.
    ///   - delegates to [`messaging::send_message`], writes `mcp.scope_resolved`.
    ///   - worker recipient + message_id → [`SendOutcome::WorkerAccepted`]; else
    ///     [`SendOutcome::Direct`].
    /// Returns `Err(ToolError{PeerNotInScope})` on a refused cross-team peer.
    pub fn send_message(
        &self,
        to: &MessageTarget,
        content: &str,
        task_id: Option<&str>,
        sender: Option<&str>,
        requires_ack: Option<bool>,
        scope_override: Option<Scope>,
    ) -> Result<SendOutcome, ToolError> {
        let canonical_owner_team = self.canonical_owner_team_key()?;
        if matches!(scope_override, Some(Scope::Workspace)) {
            return Err(self.rpc_scope_refused(
                "send_message",
                None,
                scope_override.and_then(scope_override_name),
            ));
        }
        if let Some(err) = self.refuse_cross_team_peer(to, None) {
            return Err(err);
        }
        let sender = sender
            .and_then(non_empty_string)
            .or_else(|| self.agent_id.as_ref().map(AgentId::as_str))
            .unwrap_or("unknown");
        let ack = requires_ack.unwrap_or_else(|| requires_ack_for_target(to));
        // C14/C15/C17 scope audit (#230 I-2/I-6 contract): emit mcp.scope_resolved
        // for every worker-origin send before any routing/delivery — the funnel
        // assertions grep this event to verify the worker call was scoped under the
        // spawn-time owner-team env, not a back-inferred default.
        EventLog::new(&self.workspace)
            .write(
                "mcp.scope_resolved",
                serde_json::json!({
                    "tool": "send_message",
                    "sender": sender,
                    "owner_team_id": canonical_owner_team.as_ref().map(TeamKey::as_str),
                    "to": match to {
                        MessageTarget::Single(t) => serde_json::Value::String(t.clone()),
                        MessageTarget::Broadcast => serde_json::Value::String("*".to_string()),
                        MessageTarget::Fanout(list) => serde_json::Value::Array(
                            list.iter().map(|s| serde_json::Value::String(s.clone())).collect(),
                        ),
                    },
                    "requires_ack": ack,
                }),
            )
            .map_err(tool_runtime_error)?;
        let opts = SendOptions {
            task_id: task_id.map(TaskId::new),
            route_task_id: true,
            sender: sender.to_string(),
            requires_ack: ack,
            team: canonical_owner_team,
            ..SendOptions::default()
        };
        if is_worker_recipient(to) {
            let out = messaging::send_message(&self.workspace, to, content, &opts).map_err(tool_runtime_error)?;
            let message_id = match out.message_id {
                Some(message_id) if out.ok => message_id,
                None if self.owner_team_id.is_none() => {
                    format!("mcp_{}", chrono::Utc::now().timestamp_micros())
                }
                _ => {
                    let value = delivery_outcome_value(&out);
                    let ok = compact_tool_result(&value)?;
                    return Ok(SendOutcome::Direct(ok));
                }
            };
            return Ok(SendOutcome::WorkerAccepted {
                poll_via: format!("team-agent inbox {message_id}"),
                message_id,
            });
        }
        let out = messaging::send_message(&self.workspace, to, content, &opts).map_err(tool_runtime_error)?;
        let value = delivery_outcome_value(&out);
        let ok = compact_tool_result(&value)?;
        Ok(SendOutcome::Direct(ok))
    }

    pub(crate) fn refuse_scope_override(&self) -> ToolError {
        self.rpc_scope_refused("unknown", None, None)
    }

    pub(crate) fn validate_rpc_scope_args(&self, tool: &str, args: &Value) -> Result<(), ToolError> {
        if let Some(nested) = args.get("task").or_else(|| args.get("envelope")) {
            self.validate_rpc_scope_args(tool, nested)?;
        }
        let owner_team = self.canonical_owner_team_key()?;
        let requested_team = requested_team_arg(args);
        let requested_scope = requested_scope_arg(args);
        let workspace_override = args.get("workspace").is_some();
        let scope_widens = requested_scope
            .as_deref()
            .is_some_and(|scope| !scope.eq_ignore_ascii_case("team"));
        let team_widens = match (owner_team.as_ref(), requested_team.as_deref()) {
            (_, None) => false,
            (Some(owner), Some(requested)) => {
                let state = load_runtime_state(&self.workspace).unwrap_or(serde_json::json!({}));
                let requested_canonical = crate::state::projection::resolve_owner_team_id(&state, requested)
                    .canonical_key()
                    .unwrap_or(requested)
                    .to_string();
                requested_canonical != owner.as_str()
            }
            (None, Some(_)) => true,
        };
        if workspace_override || scope_widens || team_widens {
            return Err(self.rpc_scope_refused(
                tool,
                requested_team.as_deref(),
                requested_scope
                    .as_deref()
                    .or_else(|| workspace_override.then_some("workspace")),
            ));
        }
        Ok(())
    }

    /// `report_result` (`tools.py:249-279`): build & normalize the result envelope
    /// (inferring `task_id`/`agent_id` with byte-stable `"manual"`/`"unknown"`
    /// fallbacks), then delegate to [`messaging::report_result`] and compact.
    #[allow(clippy::too_many_arguments)]
    pub fn report_result(
        &self,
        envelope: Option<&Value>,
        summary: Option<&str>,
        status: ResultStatus,
        changes: Option<&[Value]>,
        tests: Option<&[Value]>,
        risks: Option<&[Value]>,
        artifacts: Option<&[Value]>,
        next_actions: Option<&[Value]>,
        task_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> ToolResult {
        if let Some(envelope) = envelope {
            self.validate_rpc_scope_args("report_result", envelope)?;
        }
        let mut base = envelope.cloned().unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        ensure_object(&mut base);
        if let Some(obj) = base.as_object_mut() {
            if !obj.contains_key("summary") {
                obj.insert(
                    "summary".to_string(),
                    Value::String(summary.map_or_else(|| "completed".to_string(), ToString::to_string)),
                );
            }
            if !obj.contains_key("status") {
                obj.insert("status".to_string(), enum_value(status));
            }
            if !obj.contains_key("task_id") {
                let resolved = task_id
                    .map(ToString::to_string)
                    .or_else(|| self.agent_id
                        .as_ref()
                        .and_then(|agent| latest_task_for_assignee(&self.workspace, agent.as_str())))
                    .unwrap_or_else(|| "manual".to_string());
                obj.insert("task_id".to_string(), Value::String(resolved));
            }
            if !obj.contains_key("agent_id") {
                let resolved = agent_id
                    .map(ToString::to_string)
                    .or_else(|| self.agent_id.as_ref().map(|env_agent| env_agent.as_str().to_string()))
                    .unwrap_or_else(|| "unknown".to_string());
                obj.insert("agent_id".to_string(), Value::String(resolved));
            }
            if !obj.contains_key("changes") {
                insert_array(obj, "changes", changes);
            }
            if !obj.contains_key("tests") {
                insert_array(obj, "tests", tests);
            }
            if !obj.contains_key("risks") {
                insert_array(obj, "risks", risks);
            }
            if !obj.contains_key("artifacts") {
                insert_array(obj, "artifacts", artifacts);
            }
            if !obj.contains_key("next_actions") {
                insert_array(obj, "next_actions", next_actions);
            }
        }
        let normalized = normalize_report_envelope(&base);
        let env_value = normalized_envelope_value(&normalized);
        let owner_team = self.canonical_owner_team_key()?;
        messaging::report_result_for_owner_team(
            &self.workspace,
            &env_value,
            owner_team.as_ref().map(TeamKey::as_str),
        )
            .map_err(tool_runtime_error)
            .and_then(|value| compact_tool_result(&value))
    }

    /// `update_state` (`tools.py:316-325`): delegated through the lifecycle tools
    /// facade. S0 preserves the old placeholder behavior.
    pub fn update_state(&self, note: &str) -> ToolResult {
        let owner_team = self.canonical_owner_team_key()?;
        super::lifecycle_tools::update_state(&self.workspace, owner_team.as_ref(), note)
    }

    /// `get_team_status` (`tools.py:327-328`): delegated through the lifecycle tools
    /// facade. S0 preserves the old placeholder behavior.
    pub fn get_team_status(&self) -> ToolResult {
        let owner_team = self.canonical_owner_team_key()?;
        super::lifecycle_tools::get_team_status(&self.workspace, owner_team.as_ref())
    }

    /// `stop_agent` (`tools.py:330-331`): delegated through the lifecycle tools facade.
    pub fn stop_agent(&self, agent_id: &str) -> ToolResult {
        let owner_team = self.canonical_owner_team_key()?;
        super::lifecycle_tools::stop_agent(&self.workspace, owner_team.as_ref(), agent_id)
    }

    /// `reset_agent` (`tools.py:333-334`): delegated through the lifecycle tools facade.
    pub fn reset_agent(&self, agent_id: &str, discard_session: bool) -> ToolResult {
        let owner_team = self.canonical_owner_team_key()?;
        super::lifecycle_tools::reset_agent(&self.workspace, owner_team.as_ref(), agent_id, discard_session)
    }

    /// `add_agent` (`tools.py:336-337`): delegate to real lifecycle add-agent
    /// under the spawn-time owner team.
    pub fn add_agent(&self, new_agent_id: &str, role_file_path: &str) -> ToolResult {
        let owner_team = self
            .canonical_owner_team_key()?
            .ok_or_else(|| self.scope_refused("add_agent requires TEAM_AGENT_OWNER_TEAM_ID"))?;
        let role_file = Path::new(role_file_path);
        let role_file = if role_file.is_absolute() {
            role_file.to_path_buf()
        } else {
            self.workspace.join(role_file)
        };
        crate::lifecycle::launch::add_agent(
            &self.workspace,
            &AgentId::new(new_agent_id.to_string()),
            &role_file,
            false,
            Some(owner_team.as_str()),
        )
        .map_err(tool_runtime_error)
        .and_then(|report| {
            compact_tool_result(&serde_json::json!({
                "ok": true,
                "status": "added",
                "agent_id": new_agent_id,
                "state_file": report.env.state_file.to_string_lossy().to_string(),
                "coordinator_started": report.env.coordinator_started,
                "start_mode": format!("{:?}", report.start_mode),
                "role_file": report.role_file.to_string_lossy().to_string(),
            }))
        })
    }

    /// `fork_agent` (`tools.py:339-340`): delegated through the lifecycle tools facade.
    pub fn fork_agent(&self, source_agent_id: &str, as_agent_id: &str, label: Option<&str>) -> ToolResult {
        let owner_team = self.canonical_owner_team_key()?;
        super::lifecycle_tools::fork_agent(
            &self.workspace,
            owner_team.as_ref(),
            source_agent_id,
            as_agent_id,
            label,
        )
    }

    /// `request_human` (`tools.py:342-346`): create a `requires_ack` leader message via
    /// the shared leader-delivery funnel; sender = env / inferred / `"unknown"`.
    /// Returns `{ok:true, message_id, status:"needs_human"}`.
    pub fn request_human(&self, question: &str, task_id: Option<&str>, agent_id: Option<&str>) -> ToolResult {
        let _owner_team = self.canonical_owner_team_key()?;
        let explicit_sender = agent_id.and_then(non_empty_string);
        let sender = explicit_sender
            .or_else(|| self.agent_id.as_ref().map(AgentId::as_str))
            .unwrap_or("unknown");
        let event_log = EventLog::new(&self.workspace);
        if explicit_sender.is_none() && self.agent_id.is_none() {
            event_log
                .write(
                    "mcp.identity_inference_failed",
                    serde_json::json!({"tool": "request_human"}),
                )
                .map_err(tool_runtime_error)?;
        }
        // #230 N31/N32 funnel: request_human is a leader-bound caller and must go through
        // the same primitive as send_message(to=leader) / report_result / idle reminder.
        // The legacy path was a raw store insert for recipient="leader" that
        // bypassed the leader-delivery audit (no deliver_to_leader.submit emit, no rebind
        // guard, no leader_notification_log dedup). funnel it now.
        let state = crate::state::persist::load_runtime_state(&self.workspace)
            .unwrap_or(serde_json::json!({}));
        let task = task_id.map(|t| TaskId::new(t.to_string()));
        let outcome = crate::messaging::send_to_leader_receiver(
            &self.workspace,
            &state,
            "leader",
            question,
            task.as_ref(),
            sender,
            true,
            None,
            &event_log,
        )
        .map_err(tool_runtime_error)?;
        let mut fields = serde_json::Map::new();
        fields.insert("ok".to_string(), Value::Bool(outcome.ok));
        fields.insert(
            "message_id".to_string(),
            outcome.message_id.clone().map_or(Value::Null, Value::String),
        );
        fields.insert("status".to_string(), Value::String("needs_human".to_string()));
        Ok(ToolOk { fields })
    }

    /// `stuck_list` (`tools.py:348-349`): delegate to [`messaging::stuck_list`] (the
    /// team-scoped suppressed-alert projection).
    pub fn stuck_list(&self) -> ToolResult {
        let _owner_team = self.canonical_owner_team_key()?;
        messaging::stuck_list(&self.workspace)
            .map_err(tool_runtime_error)
            .map(|v| ToolOk { fields: object_fields(v) })
    }

    /// `stuck_cancel` (`tools.py:351-352`): delegate to [`messaging::stuck_cancel`];
    /// `suppressed_by` = env agent_id / `"leader"`.
    pub fn stuck_cancel(&self, agent_id: &str, alert_type: &str) -> ToolResult {
        let _owner_team = self.canonical_owner_team_key()?;
        let alert = match alert_type {
            "stuck" => Some(messaging::AlertType::Stuck),
            "idle_fallback" => Some(messaging::AlertType::IdleFallback),
            "cross_worker_deadlock" => Some(messaging::AlertType::CrossWorkerDeadlock),
            "all" => None,
            _ => None,
        };
        let suppressed_by = self.agent_id.as_ref().map(AgentId::as_str).unwrap_or("leader");
        messaging::stuck_cancel(&self.workspace, agent_id, alert, suppressed_by)
            .map_err(tool_runtime_error)
            .map(|v| ToolOk { fields: object_fields(v) })
    }

    /// `get_visible_peers` (`tools.py:226-247`): C16 scope-filtered peer list — live
    /// agents within the spawn-time owner-team scope only; other teams and dead/stopped
    /// agents are filtered server-side and never named.
    pub fn get_visible_peers(&self) -> Result<VisiblePeers, ToolError> {
        let mut peers = Vec::new();
        if let Some(team) = self.canonical_owner_team_key_for_mcp()? {
            let state = load_runtime_state(&self.workspace).map_err(tool_runtime_error)?;
            if let Some(agents) = state
                .get("teams")
                .and_then(|v| v.get(team.as_str()))
                .and_then(|v| v.get("agents"))
                .and_then(Value::as_object)
            {
                for (agent_id, info) in agents {
                    let status = info
                        .as_object()
                        .and_then(|obj| obj.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if status == "dead" || status == "stopped" {
                        continue;
                    }
                    peers.push(AgentId::new(agent_id.clone()));
                }
            }
        }
        peers.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(VisiblePeers {
            peers,
            sender_team_id: self.canonical_owner_team_key_for_mcp()?,
            scope: Scope::Team,
        })
    }

    /// `_refuse_cross_team_peer` (`tools.py:185-213`): server-side C23 pre-refusal. A
    /// non-`*`/non-leader string target NOT in the visible-peer scope and not the
    /// sender itself, with `scope != workspace`, → `Some(ToolError{PeerNotInScope})`
    /// (also writes `mcp.send_message_refused`). `None` = allowed to proceed.
    pub fn refuse_cross_team_peer(&self, to: &MessageTarget, scope_override: Option<Scope>) -> Option<ToolError> {
        let owner_team = match self.canonical_owner_team_key() {
            Ok(team) => team,
            Err(error) => return Some(error),
        };
        if matches!(scope_override, Some(Scope::Workspace)) {
            return Some(self.rpc_scope_refused(
                "send_message",
                None,
                scope_override.and_then(scope_override_name),
            ));
        }
        if owner_team.is_none() {
            return None;
        }
        let MessageTarget::Single(target) = to else {
            return None;
        };
        if target.is_empty()
            || target == "*"
            || target == "leader"
            || target == "Leader"
            || self.agent_id.as_ref().is_some_and(|id| id.as_str() == target)
        {
            return None;
        }
        if let Ok(visible) = self.get_visible_peers() {
            if visible.peers.iter().any(|peer| peer.as_str() == target) {
                return None;
            }
        }
        let hint = "the requested peer is not part of your team; worker-origin MCP cannot widen team scope.";
        let _ = EventLog::new(&self.workspace).write(
            "mcp.send_message_refused",
            serde_json::json!({
                "reason": "peer_not_in_scope",
                "sender_team_id": owner_team.as_ref().map(TeamKey::as_str).unwrap_or(""),
                "scope": "team",
                "hint": hint
            }),
        );
        let mut extra = serde_json::Map::new();
        extra.insert("status".to_string(), Value::String("refused".to_string()));
        extra.insert(
            "hint".to_string(),
            Value::String(hint.to_string()),
        );
        Some(ToolError {
            reason: ToolErrorReason::PeerNotInScope,
            exc_type: "PeerNotInScope".to_string(),
            message: format!("peer '{target}' is not in scope"),
            extra,
        })
    }

    fn canonical_owner_team_key(&self) -> Result<Option<TeamKey>, ToolError> {
        // Single worker MCP owner resolver: TEAM_AGENT_OWNER_TEAM_ID must resolve
        // through state::projection::resolve_owner_team_id to the runtime team key.
        // Unresolved/ambiguous owner scope emits mcp.scope_refused; never fallback
        // to active/top-level/sibling teams in a multi-team state.
        let Some(owner_team_id) = &self.owner_team_id else {
            let state = load_runtime_state(&self.workspace).unwrap_or(serde_json::json!({}));
            if state
                .get("teams")
                .and_then(Value::as_object)
                .is_some_and(|teams| !teams.is_empty())
            {
                return Err(self.scope_refused("TEAM_AGENT_OWNER_TEAM_ID is required for multi-team MCP"));
            }
            return Ok(None);
        };
        let state = load_runtime_state(&self.workspace)
            .map_err(|_| self.scope_refused("owner team could not be resolved"))?;
        match canonicalize_owner_team_id(&state, owner_team_id.as_str()) {
            Some(team) => Ok(Some(TeamKey::new(team))),
            None => Err(self.scope_refused("owner team could not be resolved")),
        }
    }

    fn canonical_owner_team_key_for_mcp(&self) -> Result<Option<TeamKey>, ToolError> {
        let Some(owner_team_id) = &self.owner_team_id else {
            let state = load_runtime_state(&self.workspace).unwrap_or(serde_json::json!({}));
            if state
                .get("teams")
                .and_then(Value::as_object)
                .is_some_and(|teams| !teams.is_empty())
            {
                return Err(self.scope_refused("TEAM_AGENT_OWNER_TEAM_ID is required for multi-team MCP"));
            }
            return Ok(None);
        };
        let state = load_runtime_state(&self.workspace)
            .map_err(|_| self.scope_refused("owner team could not be resolved"))?;
        match canonicalize_owner_team_id(&state, owner_team_id.as_str()) {
            Some(team) => Ok(Some(TeamKey::new(team))),
            None => Err(self.scope_refused("owner team could not be resolved")),
        }
    }

    fn scope_refused(&self, message: &str) -> ToolError {
        let canonical_owner_team_id = self.canonical_owner_team_key_for_event();
        let _ = EventLog::new(&self.workspace).write(
            "mcp.scope_refused",
            serde_json::json!({
                "reason": "scope_refused",
                "requested_owner_team_id": self.owner_team_id.as_ref().map(TeamKey::as_str),
                "owner_team_id": canonical_owner_team_id,
                "canonical_owner_team_id": canonical_owner_team_id,
                "message": message,
            }),
        );
        let mut extra = serde_json::Map::new();
        extra.insert("status".to_string(), Value::String("refused".to_string()));
        extra.insert("hint".to_string(), Value::String(message.to_string()));
        ToolError {
            reason: ToolErrorReason::McpScopeRefused,
            exc_type: "McpScopeRefused".to_string(),
            message: "mcp.scope_refused".to_string(),
            extra,
        }
    }

    fn rpc_scope_refused(
        &self,
        tool: &str,
        requested_team: Option<&str>,
        requested_scope: Option<&str>,
    ) -> ToolError {
        let owner_team_id = self.canonical_owner_team_key_for_event();
        let agent_id = self.agent_id.as_ref().map(AgentId::as_str).unwrap_or("unknown");
        let _ = EventLog::new(&self.workspace).write(
            "mcp.scope_refused",
            serde_json::json!({
                "reason": "rpc_scope_override",
                "tool": tool,
                "agent_id": agent_id,
                "owner_team_id": owner_team_id,
                "requested_team": requested_team,
                "requested_scope": requested_scope,
            }),
        );
        let mut extra = serde_json::Map::new();
        extra.insert("status".to_string(), Value::String("refused".to_string()));
        extra.insert("tool".to_string(), Value::String(tool.to_string()));
        extra.insert("agent_id".to_string(), Value::String(agent_id.to_string()));
        extra.insert(
            "owner_team_id".to_string(),
            owner_team_id.map_or(Value::Null, Value::String),
        );
        extra.insert(
            "requested_team".to_string(),
            requested_team.map_or(Value::Null, |team| Value::String(team.to_string())),
        );
        extra.insert(
            "requested_scope".to_string(),
            requested_scope.map_or(Value::Null, |scope| Value::String(scope.to_string())),
        );
        ToolError {
            reason: ToolErrorReason::McpScopeRefused,
            exc_type: "McpScopeRefused".to_string(),
            message: "mcp.scope_refused".to_string(),
            extra,
        }
    }

    fn canonical_owner_team_key_for_event(&self) -> Option<String> {
        let owner_team_id = self.owner_team_id.as_ref()?;
        let state = load_runtime_state(&self.workspace).ok()?;
        canonicalize_owner_team_id(&state, owner_team_id.as_str())
    }
}

fn canonicalize_owner_team_id(state: &Value, owner_team_id: &str) -> Option<String> {
    crate::state::projection::resolve_owner_team_id(state, owner_team_id)
        .canonical_key()
        .map(ToString::to_string)
}

fn requested_team_arg(args: &Value) -> Option<String> {
    ["team", "team_id", "owner_team_id", "owner_team", "target_team"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(ToString::to_string)
}

fn requested_scope_arg(args: &Value) -> Option<String> {
    args.get("scope")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| args.get("workspace").map(|_| "workspace".to_string()))
}

fn assignment_team_key(state: &Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .and_then(non_empty_string)
        .map(ToString::to_string)
}

fn reconcile_assigned_task(state: &mut Value, team_key: Option<&str>, task: &Value) {
    let mut top = state
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    upsert_task_in_place(&mut top, task);
    if let Some(root) = state.as_object_mut() {
        root.insert("tasks".to_string(), Value::Array(top.clone()));
    }
    if let Some(key) = team_key {
        let mut team_tasks = state
            .get("teams")
            .and_then(|v| v.get(key))
            .and_then(|team| team.get("tasks"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        upsert_task_in_place(&mut team_tasks, task);
        write_team_tasks(state, key, team_tasks);
    }
}

fn upsert_task_in_place(tasks: &mut Vec<Value>, task: &Value) {
    let Some(task_id) = task.get("id").and_then(Value::as_str) else {
        return;
    };
    for existing in tasks.iter_mut() {
        if existing.get("id").and_then(Value::as_str) == Some(task_id) {
            merge_object_fields(existing, task);
            return;
        }
    }
    tasks.push(task.clone());
}

fn merge_object_fields(existing: &mut Value, incoming: &Value) {
    let Some(existing_obj) = existing.as_object_mut() else {
        *existing = incoming.clone();
        return;
    };
    let Some(incoming_obj) = incoming.as_object() else {
        return;
    };
    for (key, value) in incoming_obj {
        existing_obj.insert(key.clone(), value.clone());
    }
}

fn write_team_tasks(state: &mut Value, team_key: &str, tasks: Vec<Value>) {
    let Some(root) = state.as_object_mut() else {
        return;
    };
    let teams = root
        .entry("teams".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(teams_obj) = teams.as_object_mut() else {
        return;
    };
    let team = teams_obj
        .entry(team_key.to_string())
        .or_insert_with(|| {
            let mut team = serde_json::Map::new();
            team.insert("tasks".to_string(), Value::Array(Vec::new()));
            team.insert("status".to_string(), Value::String("alive".to_string()));
            Value::Object(team)
        });
    let Some(team_obj) = team.as_object_mut() else {
        return;
    };
    team_obj.insert("tasks".to_string(), Value::Array(tasks));
}

fn assignment_message(task: &Value, explicit: Option<&str>) -> String {
    if let Some(message) = explicit.and_then(non_empty_string) {
        return message.to_string();
    }
    for key in ["description", "title"] {
        if let Some(text) = task.get(key).and_then(Value::as_str).and_then(non_empty_string) {
            return text.to_string();
        }
    }
    json_dumps_default(task)
}

fn scope_override_name(scope: Scope) -> Option<&'static str> {
    match scope {
        Scope::Team => Some("team"),
        Scope::Workspace => Some("workspace"),
    }
}
