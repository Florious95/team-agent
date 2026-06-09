//! step 14a · mcp_server::wire — stdio loop + JSON-RPC route + tool dispatch + contracts.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::Value;

use crate::messaging::MessageTarget;

use super::helpers::{json_dumps_default, object_fields};
use super::normalize::normalize_result_status;
use super::tools::TeamOrchestratorTools;
use super::types::{
    McpError, McpTool, RpcError, RpcId, RpcMethod, RpcResponse, Scope, SendOutcome, ServerRunReport,
    ToolError, ToolErrorReason, ToolOk, ToolResult,
};

// ═══════════════════════════════════════════════════════════════════════════
// CONTRACTS — TOOLS wire list (contracts.py), derived from McpTool.
// ═══════════════════════════════════════════════════════════════════════════

/// `TOOLS` (`contracts.py:4`): the `tools/list` payload — name/description/inputSchema
/// per tool. **Byte-stable wire single-truth**, derived from [`McpTool`] so name and
/// schema cannot drift from the enum. Returned verbatim by `tools/list`.
pub fn tools_contract() -> Vec<Value> {
    let tools = [
        McpTool::AssignTask,
        McpTool::SendMessage,
        McpTool::ReportResult,
        McpTool::UpdateState,
        McpTool::GetTeamStatus,
        McpTool::StopAgent,
        McpTool::ResetAgent,
        McpTool::AddAgent,
        McpTool::ForkAgent,
        McpTool::RequestHuman,
        McpTool::StuckList,
        McpTool::StuckCancel,
    ];
    tools
        .into_iter()
        .map(tool_contract)
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// SERVER — stdio loop + JSON-RPC route. THE single external MCP entry surface
// (boundary tests lock handle_mcp / dispatch / TOOLS).
// ═══════════════════════════════════════════════════════════════════════════

/// `dispatch(tools, request)` (`server.py:16-43`): route a `{tool, arguments}` (or
/// `{method, params}`) request to the matching [`TeamOrchestratorTools`] handler.
/// Unknown tool → `Err(ToolError{reason: UnknownTool})`. Argument/runtime errors
/// from the handler propagate as the handler's own [`ToolResult`]; the
/// argument-vs-internal exception split happens in [`handle_mcp`].
pub fn dispatch(tools: &TeamOrchestratorTools, request: &Value) -> ToolResult {
    let tool_value = request
        .get("tool")
        .filter(|v| !v.as_str().is_some_and(str::is_empty))
        .or_else(|| request.get("name").filter(|v| !v.as_str().is_some_and(str::is_empty)))
        .or_else(|| request.get("method"));
    let name = tool_value.and_then(Value::as_str);
    let args = request
        .get("arguments")
        .or_else(|| request.get("params"))
        .unwrap_or(&Value::Null);
    let Some(name) = name else {
        return Err(ToolError::new(
            ToolErrorReason::UnknownTool,
            "unknown tool None",
            "UnknownTool",
        ));
    };
    let Some(tool) = McpTool::parse(name) else {
        return Err(ToolError::new(
            ToolErrorReason::UnknownTool,
            format!("unknown tool {}", python_repr(name)),
            "UnknownTool",
        ));
    };
    dispatch_tool(tools, tool, args)
}

/// `handle_mcp(tools, request)` (`server.py:46-91`): the JSON-RPC router.
///   - `initialize` → serverInfo `team_orchestrator` v0.1.4 + echoed protocolVersion.
///   - `tools/list` → `{tools: TOOLS}`.
///   - `tools/call` → run [`dispatch`], wrap into [`ToolCallResult`] (`isError` =
///     dispatch returned `Err`); the arg-vs-runtime exception split (`server.py:
///     69-72`) classifies a caught failure into `InvalidToolArguments` vs
///     `InternalRuntimeError`.
///   - `notifications/*` → `Ok(None)` (no frame; **must not** emit to stdout).
///   - unknown method → `-32601` error frame.
/// Returns `Ok(None)` only for the notifications path; every other branch yields a
/// frame.
///
/// [`ToolCallResult`]: super::ToolCallResult
pub fn handle_mcp(tools: &TeamOrchestratorTools, request: &Value) -> Result<Option<RpcResponse>, McpError> {
    let id = rpc_id_from_request(request);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match RpcMethod::classify(method) {
        RpcMethod::Initialize => {
            let protocol = request
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            let mut result = serde_json::Map::new();
            result.insert("protocolVersion".to_string(), Value::String(protocol.to_string()));
            result.insert("capabilities".to_string(), serde_json::json!({"tools": {}}));
            result.insert(
                "serverInfo".to_string(),
                serde_json::json!({"name": "team_orchestrator", "version": "0.1.4"}),
            );
            Ok(Some(RpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(Value::Object(result)),
                error: None,
            }))
        }
        RpcMethod::ToolsList => Ok(Some(RpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::json!({ "tools": tools_contract() })),
            error: None,
        })),
        RpcMethod::ToolsCall => {
            let params = request.get("params").unwrap_or(&Value::Null);
            let body = match dispatch(tools, params) {
                Ok(ok) => {
                    let value = Value::Object(ok.fields);
                    tool_call_result_value(value.get("ok").and_then(Value::as_bool) == Some(false), &value)
                }
                Err(err) => tool_call_result_value(true, &err.to_envelope()),
            };
            Ok(Some(RpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(body),
                error: None,
            }))
        }
        RpcMethod::Notification(_) => Ok(None),
        RpcMethod::Unknown(method) => Ok(Some(RpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code: -32601,
                message: format!("unknown method '{method}'"),
            }),
        })),
    }
}

/// `main(argv)` (`server.py:114-151`): the stdio process entry. Reads stdin line by
/// line; for `jsonrpc:"2.0"` lines routes via [`handle_mcp`] and writes the response
/// frame (skipping `None`); legacy `{tool,...}` lines go straight to [`dispatch`].
/// **All errors surface as JSON-RPC frames on stdout**; the loop never crashes the
/// process. Returns a [`ServerRunReport`] summarizing the session for tests/daemon.
pub fn main(workspace: &Path, argv: &[String]) -> Result<ServerRunReport, McpError> {
    let _ = argv;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_stdio_loop(workspace, stdin.lock(), stdout.lock())
}

fn run_stdio_loop<R: BufRead, W: Write>(
    workspace: &Path,
    reader: R,
    mut writer: W,
) -> Result<ServerRunReport, McpError> {
    let tools = TeamOrchestratorTools::new(workspace);
    let mut report = ServerRunReport::default();
    for line in reader.lines() {
        let line = line?;
        report.requests_read = report.requests_read.saturating_add(1);
        let frame = handle_stdin_line(&tools, &line, &mut report)?;
        if let Some(value) = frame {
            serde_json::to_writer(&mut writer, &value)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            report.responses_written = report.responses_written.saturating_add(1);
        }
    }
    report.clean_eof = true;
    Ok(report)
}

fn handle_stdin_line(
    tools: &TeamOrchestratorTools,
    line: &str,
    report: &mut ServerRunReport,
) -> Result<Option<Value>, McpError> {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            report.error_frames = report.error_frames.saturating_add(1);
            return Ok(Some(error_response_value(
                RpcId::Null,
                -32700,
                format!("parse error: {err}"),
            )));
        }
    };
    if request.get("jsonrpc").and_then(Value::as_str) == Some("2.0") {
        match handle_mcp(tools, &request)? {
            Some(response) => {
                if response.error.is_some() {
                    report.error_frames = report.error_frames.saturating_add(1);
                }
                Ok(Some(serde_json::to_value(response)?))
            }
            None => {
                report.notifications_skipped = report.notifications_skipped.saturating_add(1);
                Ok(None)
            }
        }
    } else {
        let value = match dispatch(tools, &request) {
            Ok(ok) => Value::Object(ok.fields),
            Err(err) => {
                report.error_frames = report.error_frames.saturating_add(1);
                err.to_envelope()
            }
        };
        Ok(Some(value))
    }
}

fn rpc_id_from_request(request: &Value) -> RpcId {
    match request.get("id") {
        Some(Value::Number(n)) => n.as_i64().map_or_else(|| RpcId::Number(n.clone()), RpcId::Int),
        Some(Value::String(s)) => RpcId::Str(s.clone()),
        _ => RpcId::Null,
    }
}

fn tool_call_result_value(is_error: bool, body: &Value) -> Value {
    let text = json_dumps_default(body);
    let mut content = serde_json::Map::new();
    content.insert("type".to_string(), Value::String("text".to_string()));
    content.insert("text".to_string(), Value::String(text));
    serde_json::json!({
        "content": [Value::Object(content)],
        "isError": is_error
    })
}

fn error_response_value(id: RpcId, code: i64, message: String) -> Value {
    let response = RpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(RpcError { code, message }),
    };
    match serde_json::to_value(response) {
        Ok(value) => value,
        Err(_) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": -32000,
                "message": "internal runtime error"
            }
        }),
    }
}

fn python_repr(value: &str) -> String {
    if value.contains('\'') && !value.contains('"') {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "\\'"))
    }
}

fn tool_contract(tool: McpTool) -> Value {
    let (description, required) = match tool {
        McpTool::SendMessage => (
            "Send a message to a teammate, the leader, or '*' for all other team members. Provide only target and content; Team Agent fills sender, task id, ack policy, and delivery metadata.",
            vec!["to", "content"],
        ),
        McpTool::AssignTask => ("Assign or update a task in the team graph and deliver it to its assignee.", vec!["task"]),
        McpTool::ReportResult => ("Report task completion with a result envelope.", Vec::new()),
        McpTool::UpdateState => ("Append a note to team state and rewrite team_state.md.", vec!["note"]),
        McpTool::GetTeamStatus => ("Return machine-readable team status.", Vec::new()),
        McpTool::StopAgent => ("Stop a running worker.", vec!["agent_id"]),
        McpTool::ResetAgent => ("Reset one worker to a fresh session.", vec!["agent_id", "discard_session"]),
        McpTool::AddAgent => ("Add a first-class worker from a role file.", vec!["new_agent_id", "role_file_path"]),
        McpTool::ForkAgent => ("Fork a running worker.", vec!["source_agent_id", "as_agent_id"]),
        McpTool::RequestHuman => ("Ask the leader or user for human input.", vec!["question"]),
        McpTool::StuckList => ("List manually suppressed idle alerts.", Vec::new()),
        McpTool::StuckCancel => ("Suppress repeated stuck or idle alerts.", vec!["agent_id"]),
    };
    serde_json::json!({
        "name": tool.wire_name(),
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": tool_properties(tool),
            "required": required,
            "additionalProperties": false
        }
    })
}

fn tool_properties(tool: McpTool) -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    match tool {
        McpTool::AssignTask => {
            insert_property(&mut properties, "task", object_property("Task object to add or update."));
            insert_property(&mut properties, "message", string_property("Optional message to deliver with the task."));
        }
        McpTool::SendMessage => {
            insert_property(&mut properties, "to", string_property("Target agent id, 'leader', or '*' for broadcast."));
            insert_property(&mut properties, "content", string_property("Message body."));
            insert_property(&mut properties, "task_id", string_property("Optional task id to associate with the message."));
            insert_property(&mut properties, "sender", string_property("Optional sender override."));
            insert_property(&mut properties, "requires_ack", boolean_property("Whether the recipient should acknowledge delivery."));
        }
        McpTool::ReportResult => {
            insert_property(&mut properties, "envelope", object_property("Optional full result envelope."));
            insert_property(&mut properties, "summary", string_property("Short result summary."));
            insert_property(&mut properties, "status", string_property("Result status."));
            insert_property(&mut properties, "changes", array_property("Changed files or artifacts."));
            insert_property(&mut properties, "tests", array_property("Tests or checks performed."));
            insert_property(&mut properties, "risks", array_property("Risks or blockers."));
            insert_property(&mut properties, "artifacts", array_property("Artifact references."));
            insert_property(&mut properties, "next_actions", array_property("Suggested next actions."));
            insert_property(&mut properties, "task_id", string_property("Optional task id override."));
            insert_property(&mut properties, "agent_id", string_property("Optional reporting agent id override."));
        }
        McpTool::UpdateState => {
            insert_property(&mut properties, "note", string_property("Note to append to team state."));
        }
        McpTool::GetTeamStatus | McpTool::StuckList => {}
        McpTool::StopAgent => {
            insert_property(&mut properties, "agent_id", string_property("Agent id to stop."));
        }
        McpTool::ResetAgent => {
            insert_property(&mut properties, "agent_id", string_property("Agent id to reset."));
            insert_property(&mut properties, "discard_session", boolean_property("Whether to discard the existing provider session."));
        }
        McpTool::AddAgent => {
            insert_property(&mut properties, "new_agent_id", string_property("New agent id."));
            insert_property(&mut properties, "role_file_path", string_property("Workspace-relative role file path."));
        }
        McpTool::ForkAgent => {
            insert_property(&mut properties, "source_agent_id", string_property("Agent id to fork from."));
            insert_property(&mut properties, "as_agent_id", string_property("Agent id for the forked worker."));
            insert_property(&mut properties, "label", string_property("Optional display label."));
        }
        McpTool::RequestHuman => {
            insert_property(&mut properties, "question", string_property("Question to ask the human."));
            insert_property(&mut properties, "task_id", string_property("Optional related task id."));
            insert_property(&mut properties, "agent_id", string_property("Optional requesting agent id."));
        }
        McpTool::StuckCancel => {
            insert_property(&mut properties, "agent_id", string_property("Agent id whose stuck alerts should be suppressed."));
            insert_property(&mut properties, "alert_type", string_property("Alert type to suppress, or all."));
        }
    }
    properties
}

fn insert_property(properties: &mut serde_json::Map<String, Value>, name: &str, schema: Value) {
    properties.insert(name.to_string(), schema);
}

fn string_property(description: &str) -> Value {
    serde_json::json!({"type": "string", "description": description})
}

fn boolean_property(description: &str) -> Value {
    serde_json::json!({"type": "boolean", "description": description})
}

fn object_property(description: &str) -> Value {
    serde_json::json!({"type": "object", "description": description, "additionalProperties": true})
}

fn array_property(description: &str) -> Value {
    serde_json::json!({"type": "array", "description": description, "items": {"type": "object", "additionalProperties": true}})
}

pub(crate) fn dispatch_tool(tools: &TeamOrchestratorTools, tool: McpTool, args: &Value) -> ToolResult {
    if scope_ceiling_tool(tool) {
        tools.validate_rpc_scope_args(tool.wire_name(), args)?;
    }
    match tool {
        McpTool::AssignTask => tools.assign_task(args.get("task").unwrap_or(args), args.get("message").and_then(Value::as_str)),
        McpTool::SendMessage => {
            let target = message_target_from_value(args.get("to"));
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            let outcome = tools.send_message(
                &target,
                content,
                args.get("task_id").and_then(Value::as_str),
                args.get("sender").and_then(Value::as_str),
                args.get("requires_ack").and_then(Value::as_bool),
                None,
            )?;
            match outcome {
                SendOutcome::WorkerAccepted { .. } => Ok(ToolOk {
                    fields: object_fields(outcome.to_value()),
                }),
                SendOutcome::Direct(ok) => Ok(ok),
            }
        }
        McpTool::ReportResult => tools.report_result(
            args.get("envelope"),
            args.get("summary").and_then(Value::as_str),
            normalize_result_status(args.get("status").and_then(Value::as_str)),
            args.get("changes").and_then(Value::as_array).map(Vec::as_slice),
            args.get("tests").and_then(Value::as_array).map(Vec::as_slice),
            args.get("risks").and_then(Value::as_array).map(Vec::as_slice),
            args.get("artifacts").and_then(Value::as_array).map(Vec::as_slice),
            args.get("next_actions").and_then(Value::as_array).map(Vec::as_slice),
            args.get("task_id").and_then(Value::as_str),
            args.get("agent_id").and_then(Value::as_str),
        ),
        McpTool::UpdateState => tools.update_state(args.get("note").and_then(Value::as_str).unwrap_or("")),
        McpTool::GetTeamStatus => tools.get_team_status(),
        McpTool::StopAgent => tools.stop_agent(args.get("agent_id").and_then(Value::as_str).unwrap_or("")),
        McpTool::ResetAgent => tools.reset_agent(
            args.get("agent_id").and_then(Value::as_str).unwrap_or(""),
            args.get("discard_session").and_then(Value::as_bool).unwrap_or(false),
        ),
        McpTool::AddAgent => tools.add_agent(
            args.get("new_agent_id").and_then(Value::as_str).unwrap_or(""),
            args.get("role_file_path").and_then(Value::as_str).unwrap_or(""),
        ),
        McpTool::ForkAgent => tools.fork_agent(
            args.get("source_agent_id").and_then(Value::as_str).unwrap_or(""),
            args.get("as_agent_id").and_then(Value::as_str).unwrap_or(""),
            args.get("label").and_then(Value::as_str),
        ),
        McpTool::RequestHuman => tools.request_human(
            args.get("question").and_then(Value::as_str).unwrap_or(""),
            args.get("task_id").and_then(Value::as_str),
            args.get("agent_id").and_then(Value::as_str),
        ),
        McpTool::StuckList => tools.stuck_list(),
        McpTool::StuckCancel => tools.stuck_cancel(
            args.get("agent_id").and_then(Value::as_str).unwrap_or(""),
            args.get("alert_type").and_then(Value::as_str).unwrap_or("all"),
        ),
    }
}

fn scope_ceiling_tool(tool: McpTool) -> bool {
    matches!(
        tool,
        McpTool::SendMessage
            | McpTool::ReportResult
            | McpTool::RequestHuman
            | McpTool::AssignTask
            | McpTool::UpdateState
            | McpTool::GetTeamStatus
            | McpTool::StopAgent
            | McpTool::ResetAgent
            | McpTool::ForkAgent
    )
}

fn message_target_from_value(value: Option<&Value>) -> MessageTarget {
    match value {
        Some(Value::String(s)) if s == "*" => MessageTarget::Broadcast,
        Some(Value::String(s)) => MessageTarget::Single(s.clone()),
        Some(Value::Array(items)) => MessageTarget::Fanout(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect(),
        ),
        _ => MessageTarget::Single(String::new()),
    }
}
