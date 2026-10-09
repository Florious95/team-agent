// Test-only compatibility oracle: exact pure helper excerpt from the frozen legacy wire.
use serde_json::Value;
#[derive(Clone, Copy)]
enum McpTool {
    SendMessage,
    ReportResult,
    GetTeamStatus,
}
impl McpTool {
    fn wire_name(self) -> &'static str {
        match self {
            Self::SendMessage => "send_message",
            Self::ReportResult => "report_result",
            Self::GetTeamStatus => "get_team_status",
        }
    }
}
pub fn expected() -> Vec<Value> {
    [
        McpTool::SendMessage,
        McpTool::ReportResult,
        McpTool::GetTeamStatus,
    ]
    .into_iter()
    .map(tool_contract)
    .collect()
}
// BEGIN FROZEN EXCERPT
fn tool_contract(tool: McpTool) -> Value {
    let (description, required) = match tool {
        McpTool::SendMessage => (
            "Send a message to a teammate, the leader, or '*' for all other team members. mailbox=true stores durably without live injection; the default is live delivery.",
            vec!["to", "content"],
        ),
        McpTool::ReportResult => (
            "Report task completion with a durable result envelope. Optional presentation routing controls live leader display, not persistence.",
            Vec::new(),
        ),
        McpTool::GetTeamStatus => ("Return machine-readable team status.", Vec::new()),
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
        McpTool::SendMessage => {
            insert_property(
                &mut properties,
                "to",
                string_property("Target agent id, 'leader', or '*' for broadcast."),
            );
            insert_property(&mut properties, "content", string_property("Message body."));
            insert_property(
                &mut properties,
                "mailbox",
                boolean_property(
                    "Set true to store durably without live injection; omit for default live delivery.",
                ),
            );
        }
        McpTool::ReportResult => {
            insert_property(
                &mut properties,
                "envelope",
                object_property("Optional full result envelope."),
            );
            insert_property(
                &mut properties,
                "summary",
                string_property("Short result summary."),
            );
            insert_property(&mut properties, "status", string_property("Result status."));
            insert_property(
                &mut properties,
                "changes",
                array_property("Changed files or artifacts."),
            );
            insert_property(
                &mut properties,
                "tests",
                array_property("Tests or checks performed."),
            );
            insert_property(
                &mut properties,
                "risks",
                array_property("Risks or blockers."),
            );
            insert_property(
                &mut properties,
                "artifacts",
                array_property("Artifact references."),
            );
            insert_property(
                &mut properties,
                "next_actions",
                array_property("Suggested next actions."),
            );
            insert_property(
                &mut properties,
                "task_id",
                string_property("Optional task id override."),
            );
            insert_property(
                &mut properties,
                "agent_id",
                string_property(
                    "Optional reporting agent id; must match framework-injected TEAM_AGENT_ID.",
                ),
            );
            insert_property(
                &mut properties,
                "presentation",
                presentation_property("Optional durable presentation routing."),
            );
        }
        McpTool::GetTeamStatus => {}
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

fn presentation_property(description: &str) -> Value {
    serde_json::json!({
        "type": "object",
        "description": description,
        "properties": {
            "sink": {"type": "string", "enum": ["leader", "casefile", "silent"]},
            "class": {"type": "string", "enum": [
                "message", "progress", "stage_result", "stage_pass", "bounce",
                "blocking", "final_review", "timeout"
            ]},
            "case_id": {"type": "string"}
        },
        "required": ["sink", "class"],
        "additionalProperties": false
    })
}

fn array_property(description: &str) -> Value {
    serde_json::json!({"type": "array", "description": description, "items": {"type": "object", "additionalProperties": true}})
}
// END FROZEN EXCERPT
