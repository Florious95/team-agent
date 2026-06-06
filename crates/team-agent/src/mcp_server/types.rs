//! step 14a · mcp_server::types — wire enums / envelopes / normalized-result carriers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

// ── REUSE: step 2 model (ids + normalized-envelope value enums) ─────────────
use crate::model::enums::{ChangeKind, ResultStatus, RiskSeverity, TestStatus};
use crate::model::ids::{AgentId, TaskId, TeamKey};

use super::helpers::tool_error_reason_wire;

// ═══════════════════════════════════════════════════════════════════════════
// ERRORS — McpError is the server-level surface; ToolError is the wire envelope.
// ═══════════════════════════════════════════════════════════════════════════

/// Server/handler-level error (lib boundary, `thiserror`). Distinct from the
/// per-tool wire envelope [`ToolError`]: this is for I/O / state / delegate
/// failures that propagate up through `Result`. `handle_mcp`'s `tools/call` path
/// converts a caught argument/runtime failure into a [`ToolError`] envelope rather
/// than an `McpError`, mirroring `server.py:69-72`.
#[derive(Debug, Error)]
pub enum McpError {
    /// JSON parse of an stdin line failed (`json.loads` raise; surfaced on stdout
    /// as a `-32000`/`-32700` frame per `server.py:135-149`).
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// State read/write failed (delegated to step 5 persist).
    #[error("state: {0}")]
    State(#[from] crate::state::StateError),
    /// Message store failed (delegated to step 7).
    #[error("message_store: {0}")]
    MessageStore(#[from] crate::message_store::MessageStoreError),
    /// Event log append failed (delegated to step 4).
    #[error("event_log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    /// A delegated messaging op failed (step 11).
    #[error("messaging: {0}")]
    Messaging(#[from] crate::messaging::MessagingError),
    /// stdio transport I/O (read line / write frame / flush).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

// ═══════════════════════════════════════════════════════════════════════════
// WIRE ENUMS (§19) —散字符串 → 穷尽 enum;契约字节级真相由这些派生。
// ═══════════════════════════════════════════════════════════════════════════

/// The 12 MCP tool names (`server.py:19-43` if-chain = 字符串打地鼠). Exhaustive
/// match; unknown → [`ToolErrorReason::UnknownTool`]. [`tools_contract`] (the
/// `TOOLS` wire list returned by `tools/list`) MUST be derived from this enum so
/// name/inputSchema stay byte-identical to `contracts.py`.
///
/// [`tools_contract`]: super::tools_contract
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTool {
    AssignTask,
    SendMessage,
    ReportResult,
    UpdateState,
    GetTeamStatus,
    StopAgent,
    ResetAgent,
    AddAgent,
    ForkAgent,
    RequestHuman,
    StuckList,
    StuckCancel,
}

impl McpTool {
    /// Wire name (`assign_task` … `stuck_cancel`) — the byte-stable string used in
    /// `tools/list` and `tools/call`. Parse failure ⇒ [`ToolErrorReason::UnknownTool`].
    pub fn wire_name(self) -> &'static str {
        match self {
            McpTool::AssignTask => "assign_task",
            McpTool::SendMessage => "send_message",
            McpTool::ReportResult => "report_result",
            McpTool::UpdateState => "update_state",
            McpTool::GetTeamStatus => "get_team_status",
            McpTool::StopAgent => "stop_agent",
            McpTool::ResetAgent => "reset_agent",
            McpTool::AddAgent => "add_agent",
            McpTool::ForkAgent => "fork_agent",
            McpTool::RequestHuman => "request_human",
            McpTool::StuckList => "stuck_list",
            McpTool::StuckCancel => "stuck_cancel",
        }
    }

    /// Parse a wire tool name into the enum; `None` for an unknown tool (caller maps
    /// to [`ToolErrorReason::UnknownTool`], matching `server.py:43`).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "assign_task" => Some(McpTool::AssignTask),
            "send_message" => Some(McpTool::SendMessage),
            "report_result" => Some(McpTool::ReportResult),
            "update_state" => Some(McpTool::UpdateState),
            "get_team_status" => Some(McpTool::GetTeamStatus),
            "stop_agent" => Some(McpTool::StopAgent),
            "reset_agent" => Some(McpTool::ResetAgent),
            "add_agent" => Some(McpTool::AddAgent),
            "fork_agent" => Some(McpTool::ForkAgent),
            "request_human" => Some(McpTool::RequestHuman),
            "stuck_list" => Some(McpTool::StuckList),
            "stuck_cancel" => Some(McpTool::StuckCancel),
            _ => None,
        }
    }
}

/// JSON-RPC method (`server.py:46-91`). `notifications/*` → no reply; unknown →
/// `-32601`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcMethod {
    Initialize,
    ToolsList,
    ToolsCall,
    /// `notifications/<rest>` — [`handle_mcp`] returns `None`, loop continues.
    ///
    /// [`handle_mcp`]: super::handle_mcp
    Notification(String),
    /// Anything else → `-32601 unknown method`.
    Unknown(String),
}

impl RpcMethod {
    /// Classify a raw `method` string (`server.py:47-49,87`). `notifications/foo` →
    /// `Notification("foo")` (or full suffix); empty/absent handled by the caller.
    pub fn classify(method: &str) -> Self {
        match method {
            "initialize" => RpcMethod::Initialize,
            "tools/list" => RpcMethod::ToolsList,
            "tools/call" => RpcMethod::ToolsCall,
            m if m.starts_with("notifications/") => {
                RpcMethod::Notification(m.trim_start_matches("notifications/").to_string())
            }
            other => RpcMethod::Unknown(other.to_string()),
        }
    }
}

/// Tool error reason (`server.py:43,69-72`, `tools.py:211`). `reason == error_code`
/// in the wire envelope; this also maps to the JSON-RPC code on the non-tool path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorReason {
    /// Unknown tool name in `dispatch` (`server.py:43`).
    UnknownTool,
    /// `TypeError/ValueError/KeyError/AttributeError` caught (`server.py:69-70`).
    InvalidToolArguments,
    /// Any other exception caught (`server.py:71-72`).
    InternalRuntimeError,
    /// Cross-team peer addressed without `scope="workspace"` (`tools.py:211`).
    PeerNotInScope,
}

/// Send scope (`tools.py:165-173`). `Workspace` is the only cross-team opt-in;
/// absent/`team` resolves to the spawn-time owner team. The `scope` field on
/// `mcp.scope_resolved` is one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Team,
    Workspace,
}

// ═══════════════════════════════════════════════════════════════════════════
// WIRE ENVELOPES (byte-stable) — request/response + tool result/error.
// ═══════════════════════════════════════════════════════════════════════════

/// A JSON-RPC request id: int, string, or null/absent (`request.get("id")`).
/// Serialized verbatim into the response `id` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Int(i64),
    Str(String),
    Number(serde_json::Number),
    /// `null` / absent — echoed back as `null`.
    Null,
}

/// A JSON-RPC 2.0 response frame (`server.py:52-91`). Exactly one of `result` /
/// `error` is set. `handle_mcp` returns `Option<RpcResponse>`; `None` ⇒
/// notifications path (no frame written).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// JSON-RPC error object (`server.py:90,142`). `-32601` unknown method, `-32000`
/// stdin-loop catch-all, `-32700` parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// The `tools/call` result wrapper (`server.py:74-86`): `{content:[{type:"text",
/// text:<json>}], isError:<bool>}`. `text` is the JSON-encoded [`ToolResult`]; the
/// `is_error` flag mirrors `result.get("ok") is False`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolCallContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

/// One `content[]` block — always `{type:"text", text:<json>}` here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

/// A successful tool result body — the compacted delegate result
/// (`normalize._compact_tool_result`, `normalize.py:6-64`). The whitelist-key
/// compaction (ok/error key sets + `fanout_*` extras + `acknowledged_count`) lives
/// in [`compact_tool_result`]; this struct carries the post-compaction object.
/// `ok` defaults to `true` (an empty compaction yields `{"ok": true}`).
///
/// [`compact_tool_result`]: super::compact_tool_result
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOk {
    /// The compacted, byte-stable result object (whitelist keys only).
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}

/// The tool error envelope (`server.py:98-106`). **Redundant keys are load-bearing**:
/// `reason == error_code` and `message == error` are byte-stable downstream
/// contracts and MUST be serialized verbatim (not deduped). `ok` is always `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// The reason; serialized into BOTH `reason` and `error_code`.
    pub reason: ToolErrorReason,
    /// Exception class name (`exc_type`): `"UnknownTool"`, `"ValueError"`, …
    /// Public-message scrub (`_public_exception_message`, ≤200 chars, newlines → space).
    pub exc_type: String,
    /// The public message; serialized into BOTH `message` and `error`.
    pub message: String,
    /// Extra envelope keys (e.g. the `_refuse_cross_team_peer` `status:"refused"` +
    /// `hint`, `tools.py:208-213`) preserved alongside the canonical keys.
    pub extra: serde_json::Map<String, Value>,
}

impl ToolError {
    /// Build the `{ok:false, reason, error_code, exc_type, message, error, **extra}`
    /// JSON object with the redundant keys filled byte-for-byte (`server.py:98-106`).
    pub fn to_envelope(&self) -> Value {
        let reason = tool_error_reason_wire(self.reason);
        let mut obj = serde_json::Map::new();
        obj.insert("ok".to_string(), Value::Bool(false));
        obj.insert("reason".to_string(), Value::String(reason.to_string()));
        obj.insert("error_code".to_string(), Value::String(reason.to_string()));
        obj.insert("exc_type".to_string(), Value::String(self.exc_type.clone()));
        obj.insert("message".to_string(), Value::String(self.message.clone()));
        obj.insert("error".to_string(), Value::String(self.message.clone()));
        for (k, v) in &self.extra {
            obj.insert(k.clone(), v.clone());
        }
        Value::Object(obj)
    }

    /// `_tool_error_result(reason, message, exc_type)` (`server.py:98-106`) for the
    /// `unknown_tool` / `peer_not_in_scope` non-exception paths.
    pub fn new(reason: ToolErrorReason, message: impl Into<String>, exc_type: impl Into<String>) -> Self {
        Self {
            reason,
            exc_type: exc_type.into(),
            message: message.into(),
            extra: serde_json::Map::new(),
        }
    }

    /// `_public_exception_message(exc)` (`server.py:109-111`): strip newlines, trim,
    /// truncate to 200 chars; empty → the exception type name.
    pub fn public_exception_message(raw: &str, exc_type: &str) -> String {
        let cleaned = raw.replace(['\n', '\r'], " ");
        let trimmed = cleaned.trim();
        let base = if trimmed.is_empty() { exc_type } else { trimmed };
        base.chars().take(200).collect()
    }
}

/// A tool handler outcome (`dispatch` return). Maps to `result.get("ok") is False`:
/// `Ok` ⇒ `isError:false`, `Err(ToolError)` ⇒ `isError:true`. This is the
/// contract-callable shape — handlers below return it (or a richer typed wrapper).
pub type ToolResult = Result<ToolOk, ToolError>;

/// `send_message` outcome (`tools.py:176-183`). A worker recipient with a
/// message_id → async `accepted` carrying the byte-stable poll hint; leader / `*` /
/// broadcast → the compacted delegate result.
#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    /// `{status:"accepted", delivery_pending:true, poll_via:"team-agent inbox <id>",
    /// message_id:<id>}` — worker recipient, `tools.py:177-182`.
    WorkerAccepted {
        message_id: String,
        /// Byte-stable `"team-agent inbox <message_id>"`.
        poll_via: String,
    },
    /// Compacted delegate result (leader / `*` / broadcast / fanout).
    Direct(ToolOk),
}

impl SendOutcome {
    /// Render the outcome into its wire JSON object (`tools.py:177-183`). Folds into
    /// the [`ToolResult`] `Ok` body.
    pub fn to_value(&self) -> Value {
        match self {
            SendOutcome::WorkerAccepted { message_id, poll_via } => {
                let mut obj = serde_json::Map::new();
                obj.insert("status".to_string(), Value::String("accepted".to_string()));
                obj.insert("delivery_pending".to_string(), Value::Bool(true));
                obj.insert("poll_via".to_string(), Value::String(poll_via.clone()));
                obj.insert("message_id".to_string(), Value::String(message_id.clone()));
                Value::Object(obj)
            }
            SendOutcome::Direct(ok) => Value::Object(ok.fields.clone()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// NORMALIZED RESULT ENVELOPE (normalize.py:67-258) — typed carriers of the
// asserted values. Contracts call the normalize fns and assert these enums/structs.
// ═══════════════════════════════════════════════════════════════════════════

/// A normalized result envelope (`_normalize_report_envelope`, `normalize.py:67-80`).
/// `schema_version` is fixed `"result_envelope_v1"`; `task_id`/`agent_id` fall back
/// to `"manual"`/`"unknown"`. `status` is the regularized [`ResultStatus`] (step 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedReportEnvelope {
    pub schema_version: String,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub status: ResultStatus,
    pub summary: String,
    pub changes: Vec<NormalizedChange>,
    pub tests: Vec<NormalizedTest>,
    pub risks: Vec<NormalizedRisk>,
    pub artifacts: Vec<NormalizedArtifact>,
    pub next_actions: Vec<NormalizedNextAction>,
}

/// `changes[]` (`normalize.py:126-142`): path + regularized [`ChangeKind`] +
/// description (falls back to the envelope summary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedChange {
    pub path: String,
    pub kind: ChangeKind,
    pub description: String,
}

/// `tests[]` (`normalize.py:180-196`): command + regularized [`TestStatus`] +
/// optional detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedTest {
    pub command: String,
    pub status: TestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `risks[]` (`normalize.py:215-230`): regularized [`RiskSeverity`] (out-of-set →
/// `Low`) + description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRisk {
    pub severity: RiskSeverity,
    pub description: String,
}

/// `artifacts[]` (`normalize.py:233-245`): path + description (defaults to path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedArtifact {
    pub path: String,
    pub description: String,
}

/// `next_actions[]` (`normalize.py:248-257`): description only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedNextAction {
    pub description: String,
}

/// `get_visible_peers` result (`tools.py:226-247`): the C16 scope-filtered peer list.
/// Other teams and dead/stopped agents are filtered server-side and never named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisiblePeers {
    /// Sorted live peer ids within the sender's spawn-time owner-team scope.
    pub peers: Vec<AgentId>,
    /// The resolving team scope (`None` when no owner team env is set).
    pub sender_team_id: Option<TeamKey>,
    /// Always [`Scope::Team`] for this query.
    pub scope: Scope,
}

/// Summary of one stdio server run (`main`'s loop accounting) — frames in/out, the
/// notification skips, and whether the loop exited on EOF cleanly. Rich return so a
/// harness can assert the session shape without scraping stdout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerRunReport {
    pub requests_read: u64,
    pub responses_written: u64,
    /// `notifications/*` lines that produced no frame.
    pub notifications_skipped: u64,
    /// Lines that surfaced an error frame (parse / `-32000` / `-32601`).
    pub error_frames: u64,
    /// Clean EOF on stdin (`for line in sys.stdin` exhausted).
    pub clean_eof: bool,
}
