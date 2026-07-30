#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeSet;
use std::io::Write as _;
use std::process::{Command, Stdio};

use hermetic_guard::{HermeticTestEnv, CALLER_IDENTITY_ENVS};
use serde_json::{json, Value};
use serial_test::serial;

#[test]
#[serial(env)]
fn report_result_tools_list_exposes_closed_typed_presentation_schema() {
    let case = HermeticTestEnv::enter("report-result-public-schema");
    case.scrub_tmux();
    case.assert_no_real_tmux();
    let workspace = case.workspace("mcp-stdio");

    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    command
        .args(["mcp-server", "--workspace"])
        .arg(&workspace)
        .current_dir(&workspace)
        .env("HOME", case.home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in CALLER_IDENTITY_ENVS {
        command.env_remove(key);
    }

    let mut child = command.spawn().expect("spawn real team-agent MCP server");
    let mut stdin = child.stdin.take().expect("open MCP stdin");
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
    )
    .unwrap();
    drop(stdin);

    let output = child.wait_with_output().expect("wait for MCP EOF");
    assert!(
        output.status.success(),
        "real MCP server must exit cleanly after initialize + tools/list; status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("MCP stdout is UTF-8");
    let frames = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("MCP stdout line is not JSON: {error}; line={line}"))
        })
        .collect::<Vec<_>>();

    let initialize = frames
        .iter()
        .find(|frame| frame["id"] == json!(1))
        .unwrap_or_else(|| panic!("initialize response missing; stdout={stdout}"));
    assert!(
        initialize.get("error").is_none(),
        "initialize must succeed before tools/list is accepted; frame={initialize}"
    );

    let listed = frames
        .iter()
        .find(|frame| frame["id"] == json!(2))
        .unwrap_or_else(|| panic!("tools/list response missing; stdout={stdout}"));
    assert!(
        listed.get("error").is_none(),
        "tools/list must succeed on the real MCP line protocol; frame={listed}"
    );
    let report_result = listed["result"]["tools"]
        .as_array()
        .expect("tools/list result contains tools")
        .iter()
        .find(|tool| tool["name"] == json!("report_result"))
        .unwrap_or_else(|| panic!("tools/list omitted report_result; frame={listed}"));

    let input_schema = report_result["inputSchema"]
        .as_object()
        .expect("report_result inputSchema is an object");
    assert_eq!(
        input_schema.get("type"),
        Some(&json!("object")),
        "report_result inputSchema must remain an object; schema={input_schema:?}"
    );
    assert_eq!(
        input_schema.get("additionalProperties"),
        Some(&json!(false)),
        "adding presentation must not reopen report_result to undeclared top-level fields; schema={input_schema:?}"
    );
    let properties = input_schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("report_result inputSchema declares properties");
    let presentation = properties
        .get("presentation")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!(
                "public tools/list report_result schema must declare presentation; properties={properties:?}"
            )
        });

    assert_eq!(
        presentation.get("type"),
        Some(&json!("object")),
        "presentation must be a typed object; schema={presentation:?}"
    );
    assert_eq!(
        presentation.get("additionalProperties"),
        Some(&json!(false)),
        "presentation must be closed, not an arbitrary object; schema={presentation:?}"
    );
    let presentation_properties = presentation
        .get("properties")
        .and_then(Value::as_object)
        .expect("presentation declares typed child properties");
    for field in ["sink", "class", "case_id"] {
        assert_eq!(
            presentation_properties
                .get(field)
                .and_then(|schema| schema.get("type")),
            Some(&json!("string")),
            "presentation.{field} must be declared as a string; schema={presentation:?}"
        );
    }
    for field in ["sink", "class"] {
        let values = presentation_properties[field]["enum"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("presentation.{field} must publish its typed enum; schema={presentation:?}")
            });
        assert!(
            !values.is_empty() && values.iter().all(Value::is_string),
            "presentation.{field} enum must be a non-empty string catalog; values={values:?}"
        );
    }
    let sink_values = presentation_properties["sink"]["enum"]
        .as_array()
        .expect("presentation.sink publishes its typed enum")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let expected_sink_values = ["leader", "casefile", "silent"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        sink_values, expected_sink_values,
        "presentation.sink enum must exactly match the public catalog"
    );
    let class_values = presentation_properties["class"]["enum"]
        .as_array()
        .expect("presentation.class publishes its typed enum")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let expected_class_values = [
        "message",
        "progress",
        "stage_result",
        "stage_pass",
        "bounce",
        "blocking",
        "final_review",
        "timeout",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        class_values, expected_class_values,
        "presentation.class enum must exactly match the public catalog"
    );
    let required = presentation["required"]
        .as_array()
        .expect("presentation publishes required fields");
    for field in ["sink", "class"] {
        assert!(
            required.iter().any(|value| value == field),
            "presentation must require {field}; required={required:?}"
        );
    }
    let required_fields = required
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let expected_required_fields = ["sink", "class"].into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        required_fields, expected_required_fields,
        "presentation.required must contain exactly sink and class"
    );
}
