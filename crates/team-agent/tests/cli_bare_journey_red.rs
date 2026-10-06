//! Issue #286: bare guidance, template execution, real private dispatch, and safety.
//! Frozen product 768c9352; no developer implementation is read.
//! HermeticTestEnv owns HOME/cwd and scrubs caller identity. TestWorkspace owns
//! exact coordinator/tmux cleanup for the real fake-provider lifecycle controls.
#![cfg(unix)]
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[path = "../src/app_server_test_support.rs"]
mod app_server_test_support;
#[path = "e2e/framework.rs"]
mod framework;
#[path = "support/hermetic.rs"]
mod hermetic;

use hermetic::HermeticTestEnv;
use serde_json::{json, Value};
use serial_test::serial;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn body(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("single JSON object required: {e}: {}", text(out)))
}
fn ok(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{}", text(out));
    let v = body(out);
    assert_eq!(v["ok"], true, "{v}");
    v
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, at: &Path, map: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(at).unwrap() {
            let p = entry.unwrap().path();
            let key = p.strip_prefix(root).unwrap().to_path_buf();
            if p.is_dir() {
                map.insert(key, None);
                visit(root, &p, map);
            } else {
                map.insert(key, Some(fs::read(p).unwrap()));
            }
        }
    }
    let mut map = BTreeMap::new();
    visit(root, root, &mut map);
    map
}
fn no_jargon(text: &str) {
    let lower = text.to_lowercase();
    for banned in [
        "fully-qualified",
        "fully qualified",
        "logical recipient",
        "logical to",
        "logical target",
        "persist message",
        "persisted message",
        "harness",
        "stable-qualified",
        "qualified name",
        "case_id",
    ] {
        assert!(
            !lower.contains(banned),
            "H3 generated guidance contains {banned}: {text}"
        );
    }
}
fn role(id: &str) -> String {
    format!("---\nname: {id}\nrole: assistant\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\nReply to leader.\n")
}
fn inputs(ws: &Path) {
    fs::write(
        ws.join("TEAM.md"),
        "---\nname: help-demo\n---\nFixture team.\n",
    )
    .unwrap();
    fs::create_dir_all(ws.join("agents")).unwrap();
    fs::write(ws.join("agents/worker.md"), role("worker")).unwrap();
}
fn seeded(env: &HermeticTestEnv) -> PathBuf {
    let ws = env.workspace("scope");
    inputs(&ws);
    let spec = team_agent::compiler::compile_team(&ws).unwrap();
    fs::write(
        ws.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    let agents =
        json!({"worker":{"status":"stopped","provider":"fake","model":"fake","session_id":null}});
    let tasks = json!([{"id":"task-286","title":"case-286","case_id":"case-286","assignee":"worker","status":"done"}]);
    save_runtime_state(&ws,&json!({"active_team_key":"alpha","session_name":"team-alpha","agents":agents,"tasks":tasks,"teams":{"alpha":{"status":"alive","session_name":"team-alpha","agents":agents,"tasks":tasks}}})).unwrap();
    let store = MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let envelope = json!({"schema_version":"result_envelope_v1","result_id":"res-286","task_id":"task-286","agent_id":"worker","status":"success","summary":"RESULT_COMPAT_286","presentation":{"case_id":"case-286"},"artifacts":[{"path":"artifact://286","nested":{"preserve":true}}]});
    conn.execute("insert into results(result_id,owner_team_id,task_id,agent_id,envelope,status,created_at) values (?1,?2,?3,?4,?5,?6,?7)",rusqlite::params!["res-286","alpha","task-286","worker",envelope.to_string(),"success","2026-10-06T00:00:00Z"]).unwrap();
    ws
}

#[test]
#[serial(env)]
fn h5_bare_root_equals_help_and_writes_nothing() {
    let env = HermeticTestEnv::enter("bare-root");
    let ws = env.workspace("empty");
    let before = snapshot(env.root());
    let bare = env.run_cli(&ws, &[]);
    let help = env.run_cli(&ws, &["--help"]);
    assert_eq!(snapshot(env.root()), before, "H5 root mutated files");
    assert_eq!(
        bare.status.code(),
        Some(0),
        "H5 bare root should guide: {}",
        text(&bare)
    );
    assert_eq!(
        bare.stdout, help.stdout,
        "H5 bare root must be same four-step Human navigation"
    );
    assert!(bare.stderr.is_empty());
}
fn missing(args: &[&str]) {
    let env = HermeticTestEnv::enter(args[0]);
    let ws = env.workspace("empty");
    let before = snapshot(env.root());
    let out = env.run_cli(&ws, args);
    let t = text(&out);
    assert_eq!(
        snapshot(env.root()),
        before,
        "H5 shape-only missing input wrote logs/state/global data: {args:?}: {t}"
    );
    assert_eq!(out.status.code(), Some(2), "H5 Usage exit2: {args:?}: {t}");
    no_jargon(&t);
    assert!(
        t.contains("下一步") && (t.contains("示例") || t.contains("怎么用")),
        "H4 local complete help/Next Action missing: {t}"
    );
    assert!(
        t.contains(&format!("team-agent {}", args[0])),
        "H4 missing command-local guidance: {t}"
    );
}
macro_rules! missing_properties { ($($id:ident => [$($arg:literal),+]),+ $(,)?) => {$(
    #[test] #[serial(env)] fn $id() { missing(&[$($arg),+]); }
)+}; }
missing_properties! {
    h5_send_missing => ["send"], h5_add_missing => ["add-agent"], h5_start_missing => ["start-agent"],
    h5_stop_missing => ["stop-agent"], h5_reset_missing => ["reset-agent"], h5_clone_missing => ["clone-agent"],
    h5_fork_missing => ["fork-agent"], h5_remove_missing => ["remove-agent"], h5_inbox_missing => ["inbox"],
    h5_peer_missing => ["allow-peer-talk"], h5_profile_verb_missing => ["profile"],
    h5_profile_name_missing => ["profile", "show"], h5_route_set_missing => ["route", "set", "pi"],
    h5_route_add_missing => ["route", "add", "pi"],
}
#[test]
#[serial(env)]
fn h5_json_missing_input_is_single_object_and_side_effect_free() {
    let env = HermeticTestEnv::enter("missing-json");
    let ws = env.workspace("empty");
    let before = snapshot(env.root());
    let out = env.run_cli(&ws, &["send", "--json"]);
    assert_eq!(
        snapshot(env.root()),
        before,
        "H5 missing JSON input logged/wrote files: {}",
        text(&out)
    );
    assert_eq!(out.status.code(), Some(2));
    let v = body(&out);
    assert_eq!(v["ok"], false);
    assert!(v.get("error").and_then(Value::as_str).is_some(), "{v}");
}
#[test]
#[serial(env)]
fn h6_empty_input_prints_two_complete_safe_templates_and_real_input_path_without_creation() {
    let env = HermeticTestEnv::enter("empty-template");
    let cwd = env.workspace("project");
    let input = env.root().join("roles ' with spaces");
    fs::create_dir(&input).unwrap();
    let before = snapshot(env.root());
    let out = env.run_cli(
        &cwd,
        &[
            "quick-start",
            input.to_str().unwrap(),
            "--workspace",
            cwd.to_str().unwrap(),
        ],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert_eq!(
        snapshot(env.root()),
        before,
        "H6 guidance silently created files/logs or started a team: {t}"
    );
    for required in [
        input.to_str().unwrap(),
        "TEAM.md",
        "agents/worker.md",
        "provider: pi",
        "model:",
        "auth_mode: subscription",
        "dangerously_skip_permissions: false",
        "下一步",
        "team-agent pi",
        "team-agent quick-start",
    ] {
        assert!(
            t.contains(required),
            "H6 missing template/actual input/retry detail {required}: {t}"
        );
    }
    assert!(
        t.matches("---").count() >= 4,
        "H6 two complete front-matter documents required: {t}"
    );
    no_jargon(&t);
    for machine in [
        "team-agent results",
        "team-agent wait",
        "team-agent preflight",
    ] {
        assert!(!t.contains(machine), "H6 private tutorial leaked: {t}");
    }
}
fn strings(v: &Value, values: &mut Vec<String>) {
    match v {
        Value::String(s) => values.push(s.clone()),
        Value::Array(a) => {
            for v in a {
                strings(v, values)
            }
        }
        Value::Object(o) => {
            for v in o.values() {
                strings(v, values)
            }
        }
        _ => {}
    }
}
#[test]
#[serial(env)]
fn h6_json_templates_compile_and_private_preflight_pass_without_native_model_calls() {
    let env = HermeticTestEnv::enter("json-template");
    let ws = env.workspace("input");
    let before = snapshot(env.root());
    let out = env.run_cli(&ws, &["quick-start", ".", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = body(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(
        snapshot(env.root()),
        before,
        "H6 JSON error must not create files"
    );
    let mut values = Vec::new();
    strings(&v, &mut values);
    // Accept path/content objects or a complete template embedded in action text;
    // no particular new JSON key is assumed and no test-authored template is substituted.
    let mut documents = Vec::new();
    for value in &values {
        for (offset, _) in value.match_indices("---\n") {
            let rest = &value[offset + 4..];
            if let Some(close) = rest.find("\n---\n") {
                let header = &rest[..close];
                if !header.contains("name:") {
                    continue;
                }
                let mut end = value.len();
                let body_start = offset + 4 + close + 5;
                for marker in ["\n```", "\nagents/worker.md", "\n下一步", "\nNext Action"] {
                    if let Some(relative) = value[body_start..].find(marker) {
                        end = end.min(body_start + relative);
                    }
                }
                documents.push(value[offset..end].to_string());
            }
        }
    }
    let team = documents
        .iter()
        .find(|s| !s.contains("provider:"))
        .unwrap_or_else(|| panic!("H6 complete TEAM template absent: {v}"));
    let worker = documents
        .iter()
        .find(|s| s.contains("provider:") && s.contains("dangerously_skip_permissions:"))
        .unwrap_or_else(|| panic!("H6 complete worker template absent: {v}"));
    assert!(worker.contains("dangerously_skip_permissions: false"));
    let retry = env.workspace("copied-template");
    fs::write(retry.join("TEAM.md"), team).unwrap();
    fs::create_dir(retry.join("agents")).unwrap();
    fs::write(retry.join("agents/worker.md"), worker).unwrap();
    let compiled = team_agent::compiler::compile_team(&retry)
        .expect("H6 displayed templates must compile, no test-authored substitute");
    assert!(compiled.get("agents").is_some());
    let validated = env.run_cli(&retry, &["validate", ".", "--json"]);
    assert_eq!(validated.status.code(), Some(0), "{}", text(&validated));
    let preflight = ok(&env.run_cli(&retry, &["preflight", ".", "--json"]));
    assert!(
        preflight["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "compile" && c["ok"] == true),
        "{preflight}"
    );
}
#[test]
#[serial(env)]
fn h6_other_compile_errors_do_not_claim_empty_project_or_overwrite_inputs() {
    let env = HermeticTestEnv::enter("other-compile");
    let ws = env.workspace("bad");
    inputs(&ws);
    fs::write(
        ws.join("agents/worker.md"),
        "---\nname: worker\nprovider: not-a-provider\n---\nExisting user role.\n",
    )
    .unwrap();
    let original = fs::read(ws.join("agents/worker.md")).unwrap();
    let out = env.run_cli(&ws, &["quick-start", "."]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(fs::read(ws.join("agents/worker.md")).unwrap(), original);
    assert!(
        !text(&out).contains("dangerously_skip_permissions: false"),
        "H6 unrelated error wrongly emits new-project template: {}",
        text(&out)
    );
}

#[test]
#[serial(env)]
fn h8_results_retains_real_scoped_nested_result_reader() {
    let env = HermeticTestEnv::enter("compat-results");
    let ws = seeded(&env);
    let v = ok(&env.run_cli(
        &ws,
        &[
            "results",
            "--case",
            "case-286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--json",
        ],
    ));
    assert_eq!(v["case_id"], "case-286");
    assert_eq!(v["workspace"], ws.to_str().unwrap());
    let rows = v["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{v}");
    assert_eq!(rows[0]["result_id"], "res-286");
    assert!(
        rows[0].to_string().contains("artifact://286") && rows[0].to_string().contains("preserve")
    );
}
#[test]
#[serial(env)]
fn h8_wait_retains_real_completed_result_fifo_semantics() {
    let env = HermeticTestEnv::enter("compat-wait");
    let ws = seeded(&env);
    let v = body(&env.run_cli(
        &ws,
        &[
            "wait",
            "--task",
            "task-286",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(v["task_id"], "task-286");
    assert_eq!(v["result_id"], "res-286");
    assert_eq!(v["waited"], false);
}
#[test]
#[serial(env)]
fn h8_identity_retains_real_scoped_machine_fields() {
    let env = HermeticTestEnv::enter("compat-identity");
    let ws = seeded(&env);
    let v = ok(&env.run_cli(
        &ws,
        &[
            "identity",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--json",
        ],
    ));
    assert_eq!(v["team_id"], "alpha");
    assert_eq!(v["workspace_abspath"], ws.to_str().unwrap());
    assert!(v["uuid_prefix"].is_string());
    assert!(v.get("current_pane_id").is_some());
}
#[test]
#[serial(env)]
fn h8_sessions_retains_actual_worker_and_context_machine_projection() {
    let env = HermeticTestEnv::enter("compat-sessions");
    let ws = seeded(&env);
    let v = ok(&env.run_cli(
        &ws,
        &[
            "sessions",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--json",
        ],
    ));
    assert_eq!(v["workspace"], ws.to_str().unwrap());
    assert!(v["sessions"].to_string().contains("worker"), "{v}");
}
#[test]
#[serial(env)]
fn h8_validate_retains_real_compiler_function_not_help() {
    let env = HermeticTestEnv::enter("compat-validate");
    let ws = env.workspace("roles");
    inputs(&ws);
    let out = env.run_cli(&ws, &["validate", ".", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(body(&out).is_object());
}
#[test]
#[serial(env)]
fn h8_preflight_retains_real_compile_and_preparation_checks() {
    let env = HermeticTestEnv::enter("compat-preflight");
    let ws = env.workspace("roles");
    inputs(&ws);
    let v = ok(&env.run_cli(&ws, &["preflight", ".", "--json"]));
    assert!(v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "compile" && c["ok"] == true));
}
#[test]
#[serial(env)]
fn h8_wait_ready_retains_timeout_and_false_readiness() {
    let env = HermeticTestEnv::enter("compat-ready");
    let ws = seeded(&env);
    let v = body(&env.run_cli(
        &ws,
        &[
            "wait-ready",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--timeout",
            "0",
            "--json",
        ],
    ));
    assert_eq!(v["ok"], false);
    assert_eq!(v["readiness"]["ready"], false);
    assert!(v["status"].is_string());
    assert!(v["reason"].is_string());
}
#[test]
#[serial(env)]
fn h8_peek_retains_real_raw_screen_gate_and_scoped_unavailable_shape() {
    let env = HermeticTestEnv::enter("compat-peek");
    let ws = seeded(&env);
    let denied = env.run_cli(
        &ws,
        &[
            "peek",
            "worker",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
    );
    // This machine safety refusal has always been a runtime error (exit1),
    // not the new Human missing-input Usage contract.
    assert_eq!(denied.status.code(), Some(1));
    let denied_body = body(&denied);
    assert_eq!(denied_body["ok"], false);
    assert!(denied_body["error"]
        .as_str()
        .unwrap()
        .contains("--allow-raw-screen"));
    let v = body(&env.run_cli(
        &ws,
        &[
            "peek",
            "worker",
            "--allow-raw-screen",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(v["agent_id"], "worker");
    assert_eq!(v["ok"], false);
    assert!(v.get("reason").is_some() || v.get("error").is_some(), "{v}");
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let _ = self.0.wait();
    }
}
#[test]
#[serial(env)]
fn h8_watch_retains_real_stream_of_scoped_result_events() {
    let env = HermeticTestEnv::enter("compat-watch");
    let ws = seeded(&env);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    cmd.current_dir(&ws)
        .args([
            "watch",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
        ])
        .env("HOME", env.home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in hermetic::CALLER_IDENTITY_ENVS {
        cmd.env_remove(key);
    }
    let mut child = OwnedChild(cmd.spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send((result.is_ok(), line));
    });
    let (read_ok, line) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("watch must emit seeded actual result without help");
    assert!(
        read_ok && line.contains("result_received") && line.contains("RESULT_COMPAT_286"),
        "H8 watch: {line}"
    );
    drop(child);
    reader.join().unwrap();
}
#[test]
#[serial(env)]
fn h8_coordinator_retains_real_single_tick_function() {
    let env = HermeticTestEnv::enter("compat-coordinator");
    let ws = seeded(&env);
    let out = env.run_cli(
        &ws,
        &[
            "coordinator",
            "--once",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "H8 coordinator --once: {}",
        text(&out)
    );
    assert!(load_runtime_state(&ws).unwrap().is_object());
}
#[test]
#[serial(env)]
fn h8_e2e_retains_real_fake_provider_pipeline_and_result_fields() {
    let env = HermeticTestEnv::enter("compat-e2e");
    let ws = env.workspace("pipeline");
    let v = ok(&env.run_cli(
        &ws,
        &[
            "e2e",
            "--workspace",
            ws.to_str().unwrap(),
            "--providers",
            "fake",
            "--json",
        ],
    ));
    for field in ["launch", "send", "report", "shutdown"] {
        assert!(
            v["providers"]["fake"].get(field).is_some(),
            "H8 e2e {field}: {v}"
        );
    }
}
#[test]
#[serial(env)]
fn h8_app_server_binding_retains_real_probe_and_owner_write() {
    let env = HermeticTestEnv::enter("compat-app-server");
    let ws = seeded(&env);
    let server = app_server_test_support::FakeAppServer::start(
        "286-private",
        app_server_test_support::FakeAppServerScript::happy(
            "thread-286",
            "session-286",
            ws.to_str().unwrap(),
        ),
    );
    let v = ok(&env.run_cli(
        &ws,
        &[
            "attach-app-server-leader",
            "--socket",
            server.endpoint(),
            "--thread-id",
            "thread-286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--json",
        ],
    ));
    assert_eq!(v["team"], "alpha");
    assert!(v["owner_epoch"].as_u64().unwrap() > 0);
    let state = load_runtime_state(&ws).unwrap();
    assert_eq!(
        state.pointer("/teams/alpha/leader_receiver/app_server/thread_id"),
        Some(&json!("thread-286"))
    );
}
#[test]
#[serial(env)]
fn h8_plain_fq_and_legacy_address_forms_remain_functional_machine_json() {
    let env = HermeticTestEnv::enter("compat-address");
    let ws = seeded(&env);
    let fq = format!("{}::alpha/worker", ws.display());
    for target in ["worker", fq.as_str()] {
        let v = ok(&env.run_cli(
            &ws,
            &[
                "send",
                target,
                "ADDRESS_COMPAT_286",
                "--workspace",
                ws.to_str().unwrap(),
                "--team",
                "alpha",
                "--mailbox",
                "--json",
            ],
        ));
        assert!(v["message_id"].is_string());
        assert_eq!(v["status"], "stored_only");
    }
    let v = ok(&env.run_cli(
        &ws,
        &[
            "send",
            "--to-name",
            "worker",
            "ADDRESS_LEGACY_286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--mailbox",
            "--json",
        ],
    ));
    assert!(v["message_id"].is_string());
}
#[test]
#[serial(env)]
fn h3_real_send_success_deprecation_and_typo_guidance_are_plain_and_private() {
    let env = HermeticTestEnv::enter("human-send");
    let ws = seeded(&env);
    let accepted = env.run_cli(
        &ws,
        &[
            "send",
            "worker",
            "USER_PRIVATE_286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--mailbox",
        ],
    );
    assert_eq!(accepted.status.code(), Some(0), "{}", text(&accepted));
    assert!(!text(&accepted).contains("USER_PRIVATE_286"));
    no_jargon(&text(&accepted));
    for internal in ["verification:", "turn_verification:", "channel:", "stage:"] {
        assert!(
            !text(&accepted).contains(internal),
            "H3 default send dumps internal fields: {}",
            text(&accepted)
        );
    }
    let legacy = env.run_cli(
        &ws,
        &[
            "send",
            "--to-name",
            "worker",
            "USER_LEGACY_286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--mailbox",
        ],
    );
    no_jargon(&text(&legacy));
    let typo = env.run_cli(
        &ws,
        &[
            "send",
            "wroker",
            "USER_TYPO_286",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "alpha",
            "--mailbox",
        ],
    );
    assert_eq!(typo.status.code(), Some(1));
    no_jargon(&text(&typo));
    assert!(
        text(&typo).contains("team-agent status"),
        "H3 typo needs safe local status next step: {}",
        text(&typo)
    );
}
#[test]
#[serial(env)]
fn h7_delimiter_literal_route_help_data_and_priority_remain_unchanged() {
    let env = HermeticTestEnv::enter("route-literals");
    let ws = env.workspace("empty");
    let set = ok(&env.run_cli(
        &ws,
        &[
            "route",
            "set",
            "pi",
            "--json",
            "--",
            "--help",
            "",
            "literal --help",
            "--mode",
            "text",
            "--json",
        ],
    ));
    assert_eq!(
        set["argv"],
        json!(["--help", "", "literal --help", "--mode", "text", "--json"])
    );
    let default = ok(&env.run_cli(&ws, &["route", "status", "--json"]));
    assert_eq!(default["effective_enabled"], false);
    ok(&env.run_cli(&ws, &["route", "enable", "--json"]));
    let off = ok(&env.run_cli_env(
        &ws,
        &["route", "status", "--json"],
        &[("TEAM_AGENT_CLI_ARGV_ROUTING", "off")],
    ));
    assert_eq!(off["persisted_enabled"], true);
    assert_eq!(off["effective_enabled"], false);
    assert_eq!(off["enabled_source"], "env");
}
#[test]
#[serial(env)]
fn h7_native_delimiter_help_preserves_existing_provider_rules_and_raw_order() {
    let env = HermeticTestEnv::enter("native-boundary");
    let bin = env.root().join("native");
    fs::create_dir(&bin).unwrap();
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    for (wrapper, native) in [("pi", "pi"), ("codex", "codex"), ("claude", "claude"), ("copilot", "copilot"), ("grok", "grok"), ("cursor", "agent")] {
        let ws = env.workspace(wrapper);
        let socket = hermetic::short_tmux_socket("286-native");
        let capture = env.root().join(format!("{wrapper}.argv"));
        let exe = bin.join(native);
        fs::write(&exe, format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nexit 0\n", quote(capture.to_str().unwrap()))).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let stdout = ws.join("native.stdout");
        let stderr = ws.join("native.stderr");
        let receipt = ws.join("native.exit");
        let script = ws.join("invoke.sh");
        // Run inside this genuine PTY, not an SSH child merely claiming its
        // TMUX/PANE identity: the frozen caller controlling-TTY gate remains intact.
        let argv = [env!("CARGO_BIN_EXE_team-agent"), wrapper, "--external-leader", "--", "--help", "literal value", ""];
        let command = argv.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
        fs::write(&script, format!("#!/bin/sh\nexport HOME={} PATH={} TEAM_AGENT_CLI_ARGV_ROUTING=off\n{} > {} 2> {}\nprintf '%s\\n' \"$?\" > {}\nexec sleep 600\n", quote(env.home().to_str().unwrap()), quote(&path), command, quote(stdout.to_str().unwrap()), quote(stderr.to_str().unwrap()), quote(receipt.to_str().unwrap()))).unwrap();
        let pane_command = format!("/bin/sh {}", quote(script.to_str().unwrap()));
        let started = Command::new("tmux").args(["-S", socket.to_str().unwrap(), "new-session", "-d", "-s", "native286", "-c", ws.to_str().unwrap(), &pane_command]).output().unwrap();
        assert!(started.status.success(), "owned PTY fixture: {}", text(&started));
        env.register_owned_tmux_socket(&socket);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !receipt.exists() { assert!(Instant::now() < deadline, "owned PTY CLI did not return: {wrapper}"); thread::sleep(Duration::from_millis(10)); }
        let code = fs::read_to_string(&receipt).unwrap().trim().parse::<i32>().unwrap();
        let rendered = format!("{}{}", fs::read_to_string(stdout).unwrap(), fs::read_to_string(stderr).unwrap());
        eprintln!("H7_NATIVE_DELIMITER_PROBE wrapper={wrapper} exit={code} actual_pty=true output={rendered:?}");
        if wrapper == "pi" {
            // Pi's existing native --model/--thinking gate rejects --help. Do
            // not widen it or misclassify post-delimiter data as wrapper help.
            assert_eq!(code, 1, "{rendered}");
            assert!(rendered.contains("Pi leader") && rendered.contains("--help"), "{rendered}");
            assert!(!capture.exists());
        } else {
            assert_eq!(code, 0, "H7 native boundary: {rendered}");
            let actual = fs::read(&capture).expect("native process must actually be invoked");
            let literal = b"--help\0literal value\0\0";
            assert!(actual.windows(literal.len()).any(|bytes| bytes == literal), "H7 literal order/empty bytes changed; provider-owned defaults may remain: {actual:?}");
        }
        let raw = vec!["--".into(), "--help".into(), "literal value".into(), "".into()];
        assert_eq!(team_agent::cli::provider_args(&raw), vec!["--help", "literal value", ""]);
    }
}
fn quiesce(ws: &framework::TestWorkspace) {
    use team_agent::coordinator::{pid_is_running, stop_coordinator, Pid, WorkspacePath};
    let raw = fs::read_to_string(ws.coordinator_pid_file())
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(ws.pid_is_owned_coordinator(raw));
    let pid = Pid::new(raw);
    let stopped = stop_coordinator(&WorkspacePath::new(ws.path().to_path_buf())).unwrap();
    assert!(stopped.ok);
    assert_eq!(stopped.pid, Some(pid));
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_is_running(pid).unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[serial(env)]
fn h9_actual_worker_stop_start_and_refusal_guards_keep_owned_resources_and_roles() {
    let env = HermeticTestEnv::enter("lifecycle-guards");
    let ws = framework::TestWorkspace::new("286-safety").with_fake_spec(&["worker"]);
    let started = framework::quick_start_fake(&ws, "safety286");
    assert!(
        framework::quick_start_workers_available(&started),
        "{} {}",
        started.stdout,
        started.stderr
    );
    ws.retain_owned_session_placeholder_window("hold");
    quiesce(&ws);
    let state = ws.read_state();
    let role = fs::read(ws.path().join("agents/worker.md")).unwrap();
    for args in [
        vec!["reset-agent", "worker", "--json"],
        vec!["remove-agent", "worker", "--json"],
        vec![
            "add-agent",
            "worker",
            "--provider",
            "fake",
            "--model",
            "fake",
            "--bypass",
            "false",
            "--json",
        ],
        vec!["fork-agent", "worker", "--as", "child", "--json"],
    ] {
        let out = framework::run_ta_env(&ws, &args, &[("HOME", env.home().to_str().unwrap())]);
        assert_ne!(
            out.exit_code, 0,
            "H9 missing consent/unsupported fork must refuse: {}",
            out.stdout
        );
        assert_eq!(
            ws.read_state(),
            state,
            "H9 refusal mutated strict runtime state: {args:?}"
        );
        assert_eq!(fs::read(ws.path().join("agents/worker.md")).unwrap(), role);
    }
    let stopped = framework::run_ta_env(
        &ws,
        &["stop-agent", "worker", "--json"],
        &[("HOME", env.home().to_str().unwrap())],
    );
    assert!(
        stopped.is_success(),
        "{} {}",
        stopped.stdout,
        stopped.stderr
    );
    assert_eq!(fs::read(ws.path().join("agents/worker.md")).unwrap(), role);
    let resumed = framework::run_ta_env(
        &ws,
        &["start-agent", "worker", "--json"],
        &[("HOME", env.home().to_str().unwrap())],
    );
    assert!(
        resumed.is_success(),
        "{} {}",
        resumed.stdout,
        resumed.stderr
    );
    assert!(
        ws.read_state()["agents"]["worker"]["pane_id"].is_string(),
        "actual worker spawn must persist a pane, not a mock argv result"
    );
}
