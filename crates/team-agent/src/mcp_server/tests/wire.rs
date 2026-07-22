#[test]
fn mcp_tool_wire_names_and_parse_roundtrip() {
    let names = [
        (McpTool::AssignTask, "assign_task"),
        (McpTool::SendMessage, "send_message"),
        (McpTool::ReportResult, "report_result"),
        (McpTool::UpdateState, "update_state"),
        (McpTool::GetTeamStatus, "get_team_status"),
        (McpTool::StopAgent, "stop_agent"),
        (McpTool::ResetAgent, "reset_agent"),
        (McpTool::AddAgent, "add_agent"),
        (McpTool::CloneAgent, "clone_agent"),
        (McpTool::ForkAgent, "fork_agent"),
        (McpTool::RequestHuman, "request_human"),
        (McpTool::StuckList, "stuck_list"),
        (McpTool::StuckCancel, "stuck_cancel"),
    ];
    for (tool, name) in names {
        assert_eq!(tool.wire_name(), name);
        assert_eq!(McpTool::parse(name), Some(tool));
    }
    // unknown → None (server.py:43 maps to UnknownTool)
    assert_eq!(McpTool::parse("nope"), None);
    assert_eq!(McpTool::parse("AssignTask"), None); // case-sensitive snake_case
}

#[test]
fn rpc_method_classify() {
    assert_eq!(RpcMethod::classify("initialize"), RpcMethod::Initialize);
    assert_eq!(RpcMethod::classify("tools/list"), RpcMethod::ToolsList);
    assert_eq!(RpcMethod::classify("tools/call"), RpcMethod::ToolsCall);
    // notifications/* → Notification (no reply path)
    assert!(matches!(
        RpcMethod::classify("notifications/initialized"),
        RpcMethod::Notification(_)
    ));
    // unknown → Unknown
    assert_eq!(
        RpcMethod::classify("foo/bar"),
        RpcMethod::Unknown("foo/bar".to_string())
    );
}

// ════════════════════════════════════════════════════════════════════════
// tools_contract — TOOLS wire list, exact names+order
// ════════════════════════════════════════════════════════════════════════
#[test]
fn tools_contract_has_thirteen_tools_in_order() {
    let tools = tools_contract();
    assert_eq!(tools.len(), 13);
    let got: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        got,
        vec![
            "assign_task",
            "send_message",
            "report_result",
            "update_state",
            "get_team_status",
            "stop_agent",
            "reset_agent",
            "add_agent",
            "clone_agent",
            "fork_agent",
            "request_human",
            "stuck_list",
            "stuck_cancel",
        ]
    );
    // each carries description + inputSchema
    for t in &tools {
        assert!(t.get("description").and_then(Value::as_str).is_some());
        assert!(t.get("inputSchema").is_some());
    }
    // spot-check byte-stable description + schema for send_message
    let send = tools
        .iter()
        .find(|t| t["name"] == json!("send_message"))
        .unwrap();
    assert_eq!(
            send["description"],
            json!("Send a message to a teammate, the leader, or '*' for all other team members. Team Agent fills identity and delivery metadata; optional presentation routing is durable and never drops the message.")
        );
    assert_eq!(send["inputSchema"]["additionalProperties"], json!(false));
    assert_eq!(send["inputSchema"]["required"], json!(["to", "content"]));
    for internal in ["sender", "task_id", "requires_ack"] {
        assert!(
            send["inputSchema"]["properties"].get(internal).is_none(),
            "{internal} is framework-owned, not caller-supplied"
        );
    }
    let clone = tools
        .iter()
        .find(|tool| tool["name"] == json!("clone_agent"))
        .unwrap();
    assert_eq!(
        clone["description"],
        json!("Clone a worker role into a fresh provider session.")
    );
    assert_eq!(clone["inputSchema"]["additionalProperties"], json!(false));
    assert_eq!(
        clone["inputSchema"]["required"],
        json!(["source_agent_id", "as_agent_id"])
    );
    assert_eq!(
        clone["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "as_agent_id".to_string(),
            "label".to_string(),
            "source_agent_id".to_string(),
        ])
    );
}

#[test]
fn tools_contract_input_schemas_are_openai_strict_top_level_objects() {
    let forbidden = ["oneOf", "anyOf", "allOf", "enum", "not"];
    for tool in tools_contract() {
        let schema = tool["inputSchema"].as_object().unwrap();
        assert_eq!(
            schema.get("type"),
            Some(&json!("object")),
            "schema must be a top-level object: {tool}"
        );
        for key in forbidden {
            assert!(
                !schema.contains_key(key),
                "OpenAI rejects top-level `{key}` in MCP tool schema: {tool}"
            );
        }
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("schema properties must be an object: {tool}"));
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = required.as_str() else {
                panic!("required entries must be strings: {tool}");
            };
            assert!(
                properties.contains_key(name),
                "required property `{name}` must be declared in properties: {tool}"
            );
        }
    }
}

// ════════════════════════════════════════════════════════════════════════
// handle_mcp — JSON-RPC routing (server.py:46-91)
// ════════════════════════════════════════════════════════════════════════
#[test]
fn handle_mcp_initialize_echoes_protocol_and_serverinfo() {
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let resp = handle_mcp(
        &tools,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "X"}
        }),
    )
    .unwrap()
    .expect("initialize yields a frame");
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.id, RpcId::Int(1));
    let result = resp.result.unwrap();
    assert_eq!(result["protocolVersion"], json!("X"));
    assert_eq!(result["serverInfo"]["name"], json!("team_orchestrator"));
    assert_eq!(result["serverInfo"]["version"], json!("0.1.4"));
    assert_eq!(result["capabilities"], json!({"tools": {}}));
}

#[test]
fn handle_mcp_initialize_defaults_protocol_version() {
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let resp = handle_mcp(
        &tools,
        &json!({
            "jsonrpc": "2.0", "id": "abc", "method": "initialize"
        }),
    )
    .unwrap()
    .unwrap();
    assert_eq!(resp.id, RpcId::Str("abc".to_string()));
    assert_eq!(resp.result.unwrap()["protocolVersion"], json!("2024-11-05"));
}

#[test]
fn handle_mcp_notifications_return_none_no_frame() {
    // 铁律: notifications/* MUST NOT emit a frame (would corrupt stdout stream).
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let resp = handle_mcp(
        &tools,
        &json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }),
    )
    .unwrap();
    assert!(resp.is_none(), "notifications/* → None (loop continues)");
}

#[test]
fn handle_mcp_unknown_method_is_minus_32601() {
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let resp = handle_mcp(
        &tools,
        &json!({
            "jsonrpc": "2.0", "id": 7, "method": "foo/bar"
        }),
    )
    .unwrap()
    .unwrap();
    assert!(resp.result.is_none());
    let err = resp.error.unwrap();
    assert_eq!(err.code, -32601);
    assert_eq!(err.message, "unknown method 'foo/bar'"); // exact Python repr w/ quotes
}

#[test]
fn handle_mcp_unknown_tool_call_is_error_with_envelope_text() {
    // tools/call with unknown tool → isError:true, content[0].text == json.dumps(envelope)
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let resp = handle_mcp(
        &tools,
        &json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "nope", "arguments": {}}
        }),
    )
    .unwrap()
    .unwrap();
    let result = resp.result.unwrap();
    assert_eq!(result["isError"], json!(true));
    let text = result["content"][0]["text"].as_str().unwrap();
    // the text is a JSON-encoded error envelope with redundant keys
    let env: Value = serde_json::from_str(text).unwrap();
    assert_eq!(env["ok"], json!(false));
    assert_eq!(env["reason"], json!("unknown_tool"));
    assert_eq!(env["error_code"], json!("unknown_tool"));
    assert_eq!(env["exc_type"], json!("UnknownTool"));
    assert_eq!(env["message"], json!("unknown tool 'nope'"));
    assert_eq!(env["error"], json!("unknown tool 'nope'"));
    assert_eq!(result["content"][0]["type"], json!("text"));
}

// ════════════════════════════════════════════════════════════════════════
// dispatch — unknown tool → Err(UnknownTool) (server.py:43)
// ════════════════════════════════════════════════════════════════════════
#[test]
fn dispatch_unknown_tool_returns_unknown_tool_error() {
    let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
    let r = dispatch(&tools, &json!({"tool": "nope"}));
    let err = r.expect_err("unknown tool ⇒ Err");
    assert_eq!(err.reason, ToolErrorReason::UnknownTool);
    assert_eq!(err.exc_type, "UnknownTool");
    assert_eq!(err.message, "unknown tool 'nope'");
}

// ════════════════════════════════════════════════════════════════════════
// requires_ack_for_target — leader-only → false (tools.py:16)
// ════════════════════════════════════════════════════════════════════════
