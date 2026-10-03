#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;
#[path = "../../../tests/support/mcp_sim_harness.rs"]
mod sim;

use hermetic::HermeticTestEnv;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crate::event_log::EventLog;
use crate::lifecycle::launch::pi_mcp::{
    materialize_pi_plan, pi_model_candidates, pi_seat_paths, PiMaterializeRequest, PiSessionScope,
};
use crate::lifecycle::launch::run_pi_catalog_preflight;
use crate::lifecycle::tests::test_binary_path;
use crate::message_store::MessageStore;
use crate::provider::McpConfig;

const CHILD_MARKER: &str = "TEAM_AGENT_TEST_PI_NATIVE_MCP_CHILD";
const TEST_PATH: &str = concat!(
    "lifecycle::tests::pi_native_mcp_capability_red::",
    "native_pi_without_adapter_registers_and_serves_all_team_tools"
);
const REQUIRED_TOOLS: [&str; 3] = ["send_message", "report_result", "get_team_status"];

#[test]
fn native_pi_without_adapter_registers_and_serves_all_team_tools() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        native_pi_without_adapter_registers_and_serves_all_team_tools_body();
        return;
    }

    let output = Command::new(std::env::current_exe().expect("lib-test executable"))
        .args(["--exact", TEST_PATH, "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .output()
        .expect("run N1 native Pi fixture in an isolated test process");
    assert!(
        output.status.success(),
        "isolated N1 child failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn native_pi_without_adapter_registers_and_serves_all_team_tools_body() {
    let hermetic = HermeticTestEnv::enter("pi-native-mcp-n1");
    let system_path = std::env::var_os("PATH").expect("test PATH");
    let bun = executable_on_path("bun", &system_path)
        .expect("N1 fixture requires the preflight-verified Bun TypeScript runtime");
    let root = hermetic.workspace("native-pi");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("create fake Pi bin");
    let call_log = root.join("pi-calls.txt");
    let pi_real = root.join("pi-real");
    let script = r##"#!/bin/sh
printf '%s\n' "${1-}" >> "$TEAM_AGENT_TEST_PI_CALL_LOG"
case "${1-}" in
  --version) printf '1.0.0\n' ;;
  --list-models) printf 'provider model\nteam-agent qwen3.8-27b\n' ;;
  list) exit 0 ;;
  *) exit 64 ;;
esac
"##;
    std::fs::write(&pi_real, script).expect("write standard Pi fixture");
    std::fs::set_permissions(&pi_real, std::fs::Permissions::from_mode(0o755))
        .expect("make fake Pi executable");
    std::os::unix::fs::symlink(&pi_real, bin.join("pi")).expect("create npm-style Pi symlink");
    let mut fixture_path_entries = vec![bin.clone()];
    fixture_path_entries.extend(std::env::split_paths(&system_path));
    let fixture_path = std::env::join_paths(fixture_path_entries).expect("join fake Pi PATH");
    let _path = hermetic.with_env(
        "PATH",
        fixture_path.to_str().expect("fixture PATH must be UTF-8"),
    );
    let _call_log = hermetic.with_env(
        "TEAM_AGENT_TEST_PI_CALL_LOG",
        call_log.to_str().expect("call log path must be UTF-8"),
    );

    let roles = hermetic.workspace("roles");
    let agents = roles.join("agents");
    std::fs::create_dir_all(&agents).expect("create Pi role directory");
    std::fs::write(
        agents.join("n1.md"),
        "---\nprovider: pi\nmodel: team-agent/qwen3.8-27b\n---\nN1 native Pi fixture\n",
    )
    .expect("write exact-model Pi role");
    assert_eq!(
        pi_model_candidates("qwen3.8-27b").expect("fake Pi model catalog"),
        ["team-agent/qwen3.8-27b"],
        "catalog discovery must succeed without consulting the package list"
    );
    let mut discover = |requested: &str| pi_model_candidates(requested).map_err(|_| ());
    run_pi_catalog_preflight(&roles, &mut discover)
        .expect("exact-model catalog preflight must pass on native-only Pi");

    let harness = sim::McpSimHarness::new();
    let workspace = harness.workspace_path();
    let candidate = PathBuf::from(test_binary_path());
    assert!(
        candidate.is_absolute() && candidate.is_file(),
        "MCP candidate must be an absolute built binary: {}",
        candidate.display()
    );
    let args = vec![
        "mcp-server".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
    ];
    let env = json!({
        "TEAM_AGENT_WORKSPACE": workspace,
        "TEAM_AGENT_ID": "worker_a",
        "TEAM_AGENT_AGENT_ID": "worker_a",
        "TEAM_AGENT_OWNER_TEAM_ID": "teamA",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": "leader-session-team-a"
    });
    let config = McpConfig {
        raw: json!({
            "team_orchestrator": {
                "command": candidate,
                "args": args,
                "cwd": workspace,
                "env": env
            }
        }),
    };
    let plan = materialize_pi_plan(PiMaterializeRequest {
        workspace,
        team_id: "teamA",
        agent_id: "worker_a",
        model: Some("team-agent/qwen3.8-27b"),
        effort: None,
        system_prompt: "N1 native Pi MCP capability fixture",
        team_mcp_tools: &REQUIRED_TOOLS,
        mcp_config: &config,
        session_scope: PiSessionScope::Isolated,
    });
    let pi_calls = std::fs::read_to_string(&call_log).unwrap_or_default();
    assert!(
        plan.is_ok(),
        "N1 must materialize on native-capable Pi when `pi list` is empty; got={plan:?}; observed_pi_commands={pi_calls:?}"
    );
    let plan = plan.expect("N1 materialization after the assertion above");

    let commands = pi_calls.lines().collect::<Vec<_>>();
    assert!(
        !commands.iter().any(|command| *command == "list"),
        "capability resolution must not call `pi list` or inspect package presence: {commands:?}"
    );
    let seat = pi_seat_paths(workspace, "teamA", "worker_a");
    assert!(
        seat.wrapper.is_file(),
        "successful materialization must publish the seat wrapper"
    );
    assert!(
        plan.argv
            .iter()
            .any(|arg| arg == seat.wrapper.to_string_lossy().as_ref()),
        "Pi command plan must load the generated native extension: {:?}",
        plan.argv
    );

    let receipt_path = workspace.join(".team/test-evidence/native-registration.json");
    std::fs::create_dir_all(receipt_path.parent().expect("registration receipt parent"))
        .expect("create registration receipt directory");
    let host_fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/lifecycle/tests/fixtures/pi_native_mcp_host.mjs");
    let host = Command::new(&bun)
        .arg(host_fixture)
        .arg(&seat.wrapper)
        .arg(&receipt_path)
        .arg(workspace)
        .current_dir(workspace)
        .output()
        .expect("execute generated extension in the Pi native API fixture");
    assert!(
        host.status.success(),
        "Pi native host fixture failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        host.status.code(),
        String::from_utf8_lossy(&host.stdout),
        String::from_utf8_lossy(&host.stderr),
    );
    let native_receipt: Value = serde_json::from_slice(
        &std::fs::read(&receipt_path).expect("native registerMcpServer receipt"),
    )
    .expect("parse native registration receipt");
    let registrations = native_receipt["registrations"]
        .as_array()
        .expect("host recorded registerMcpServer calls");
    assert_eq!(
        registrations.len(),
        1,
        "one owned native server registration is expected"
    );
    let expected_runtime_tools = BTreeSet::from(REQUIRED_TOOLS.map(|tool| {
        format!(
            "mcp__{}__{tool}",
            registrations[0]["name"].as_str().unwrap()
        )
    }));
    let runtime_tools = native_receipt["activeTools"]
        .as_array()
        .expect("native Pi consumer exposes active tools")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(runtime_tools, expected_runtime_tools);
    assert_eq!(
        native_receipt["expectedTools"]
            .as_array()
            .expect("host confirms native binding tools")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        expected_runtime_tools,
    );
    let wire_prompt = native_receipt["wirePrompt"]
        .as_str()
        .expect("Pi wrapper installs an actual tool wire binding");
    assert!(REQUIRED_TOOLS.iter().all(|tool| wire_prompt.contains(tool)));
    let registration = &registrations[0];
    let alias = registration["name"]
        .as_str()
        .expect("native registration name");
    let alias_id = alias
        .strip_prefix("team_")
        .expect("native server alias must be Team-owned");
    assert_eq!(
        alias_id.len(),
        32,
        "Team alias retains UUID entropy: {alias:?}"
    );
    assert!(alias_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!alias.contains('-') && !alias.contains('/'));
    let native_config = &registration["config"];
    assert_eq!(native_config["command"], json!(candidate.to_string_lossy()));
    assert_eq!(native_config["args"], json!(args));
    assert_eq!(native_config["cwd"], json!(workspace.to_string_lossy()));
    assert_eq!(native_config["exposure"], json!("hidden"));
    let exposure = native_config["toolExposure"]
        .as_object()
        .expect("native per-tool exposure map");
    assert_eq!(
        exposure.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(REQUIRED_TOOLS.map(str::to_string)),
        "only the three Team operations are directly exposed"
    );
    assert!(exposure.values().all(|value| value == "direct"));
    for (key, expected) in [
        ("TEAM_AGENT_WORKSPACE", workspace.to_string_lossy().as_ref()),
        ("TEAM_AGENT_ID", "worker_a"),
        ("TEAM_AGENT_AGENT_ID", "worker_a"),
        ("TEAM_AGENT_OWNER_TEAM_ID", "teamA"),
    ] {
        assert_eq!(
            native_config["env"][key].as_str(),
            Some(expected),
            "native server must retain captured seat identity for {key}"
        );
    }

    let store = MessageStore::open(workspace).expect("open isolated MCP store");
    store
        .create_message(
            Some("task_mcp"),
            "leader",
            "worker_a",
            "N1_NATIVE_MCP_INPUT",
            None,
            false,
            Some("teamA"),
        )
        .expect("seed attributable task input");
    let mut client = RawMcpClient::spawn(native_config, &system_path, hermetic.home());
    let initialize = client.rpc("initialize", json!({"protocolVersion":"2024-11-05"}));
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        json!("team_orchestrator")
    );
    client.notification("notifications/initialized", json!({}));
    let listed = client.rpc("tools/list", json!({}));
    let tool_names = listed["result"]["tools"]
        .as_array()
        .expect("raw tools/list response")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tool_names,
        BTreeSet::from(REQUIRED_TOOLS.map(str::to_string)),
        "native config must reach the actual stdio MCP server with exactly the three Team tools"
    );

    let status = client.call_tool("get_team_status", json!({}));
    assert!(
        !status.is_error && status.body["ok"] == json!(true),
        "get_team_status stdio call: {}",
        status.body
    );
    assert!(status.body["teams"].get("teamA").is_some());
    let marker = format!("N1_NATIVE_MCP_SEND_{}", std::process::id());
    let send = client.call_tool("send_message", json!({"to":"leader","content":marker}));
    assert!(
        !send.is_error && send.body["ok"] == json!(true),
        "send_message stdio call: {}",
        send.body
    );
    let result_marker = format!("N1_NATIVE_MCP_RESULT_{}", std::process::id());
    let report = client.call_tool(
        "report_result",
        json!({"task_id":"task_mcp","summary":result_marker}),
    );
    assert!(
        !report.is_error,
        "report_result stdio call: {}",
        report.body
    );
    let result_id = report.body["result_id"]
        .as_str()
        .expect("durable result id");
    let result_row = harness
        .result_row(result_id)
        .expect("report_result persists result");
    assert_eq!(result_row.owner_team_id.as_deref(), Some("teamA"));
    assert_eq!(result_row.agent_id, "worker_a");
    assert_eq!(result_row.task_id, "task_mcp");
    harness.drive_delivery_twice();
    assert!(harness.pane_text("leader").contains(&marker));
    assert!(harness.pane_text("leader").contains(&result_marker));

    let trace_path = workspace.join(".team/test-evidence/native-mcp-stdio.jsonl");
    client.write_trace(&trace_path);
    let server_pid = client.pid();
    let exit = client.close_stdin_and_wait();
    assert!(
        exit.success(),
        "native candidate stdio server exit must be successful: {exit:?}"
    );
    let events = EventLog::new(workspace)
        .tail(0)
        .expect("MCP server lifecycle receipt");
    assert!(
        events.iter().any(|event| {
            event["event"] == "mcp.server_started" && event["pid"] == json!(server_pid)
        }),
        "actual stdio server start receipt must match child pid {server_pid}"
    );
    assert!(
        events.iter().any(|event| {
            event["event"] == "mcp.server_exit"
                && event["pid"] == json!(server_pid)
                && event["reason"] == json!("stdin_eof")
        }),
        "actual stdio server exit receipt must match child pid and clean EOF"
    );
    println!(
        "N1_NATIVE_MCP_RECEIPT={}",
        json!({"alias":alias,"candidate":candidate,"server_pid":server_pid,"exit_code":exit.code(),"tool_names":tool_names,"stdio_trace":trace_path})
    );
}

fn executable_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|entry| entry.join(name))
        .find(|candidate| candidate.is_file())
}

struct RawMcpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<String>,
    next_id: u64,
    trace: Vec<Value>,
    waited: bool,
}

struct ToolCall {
    body: Value,
    is_error: bool,
}

impl RawMcpClient {
    fn spawn(config: &Value, path: &std::ffi::OsStr, home: &Path) -> Self {
        let program = config["command"].as_str().expect("registered MCP command");
        let args = config["args"]
            .as_array()
            .expect("registered MCP args")
            .iter()
            .map(|arg| arg.as_str().expect("string MCP arg").to_string())
            .collect::<Vec<_>>();
        let cwd = config["cwd"].as_str().expect("registered MCP cwd");
        let env = config["env"].as_object().expect("registered MCP env");
        let mut command = Command::new(program);
        command
            .args(&args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("LANG", "C.UTF-8")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value.as_str().expect("string MCP env value"));
        }
        let mut child = command
            .spawn()
            .expect("spawn registered native stdio MCP command");
        let stdin = child.stdin.take().expect("MCP child stdin");
        let stdout = child.stdout.take().expect("MCP child stdout");
        let (tx, responses) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            responses,
            next_id: 1,
            trace: Vec::new(),
            waited: false,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let line = serde_json::to_string(&request).expect("serialize MCP request");
        let stdin = self.stdin.as_mut().expect("MCP stdin is open");
        writeln!(stdin, "{line}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
        let response_line = self
            .responses
            .recv_timeout(Duration::from_secs(75))
            .unwrap_or_else(|_| panic!("timed out waiting for stdio MCP {method} response"));
        let response: Value = serde_json::from_str(&response_line).unwrap_or_else(|error| {
            panic!("invalid MCP response for {method}: {error}; {response_line}")
        });
        assert_eq!(response["id"], json!(id), "MCP response id for {method}");
        assert!(
            response.get("error").is_none(),
            "MCP protocol error for {method}: {response}"
        );
        self.trace
            .push(json!({"request":request,"response":response}));
        response
    }

    fn notification(&mut self, method: &str, params: Value) {
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        let line = serde_json::to_string(&request).expect("serialize MCP notification");
        let stdin = self.stdin.as_mut().expect("MCP stdin is open");
        writeln!(stdin, "{line}").expect("write MCP notification");
        stdin.flush().expect("flush MCP notification");
        self.trace.push(json!({"request":request,"response":null}));
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> ToolCall {
        let response = self.rpc("tools/call", json!({"name":name,"arguments":arguments}));
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("tools/call missing result: {response}"));
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool response text");
        ToolCall {
            body: serde_json::from_str(text).unwrap_or_else(|_| json!({"raw_text":text})),
            is_error: result["isError"].as_bool().unwrap_or(false),
        }
    }

    fn write_trace(&self, path: &Path) {
        let parent = path.parent().expect("MCP trace parent");
        std::fs::create_dir_all(parent).expect("create MCP trace directory");
        let mut file = std::fs::File::create(path).expect("create raw MCP trace");
        for row in &self.trace {
            writeln!(
                file,
                "{}",
                serde_json::to_string(row).expect("serialize trace row")
            )
            .expect("write raw MCP trace");
        }
    }

    fn close_stdin_and_wait(&mut self) -> ExitStatus {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll MCP server exit") {
                self.waited = true;
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let status = self.child.wait().expect("reap timed-out MCP server");
                self.waited = true;
                panic!("native stdio MCP server did not exit after EOF: {status:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RawMcpClient {
    fn drop(&mut self) {
        if !self.waited {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.waited = true;
        }
    }
}
