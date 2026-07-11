//! 0.5.30 RED contract: add-agent dynamic role files remain authoritative
//! across restart without weakening live-topology SaveConflict protection.
//!
//! References:
//! - `.team/artifacts/add-agent-restart-saveconflict-locate.md` §8 RED1-RED4.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::model::paths::runtime_spec_path;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::state::StateError;

static CASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[serial(env)]
fn canonical_add_agent_then_restart_keeps_dynamic_helper_alive() {
    let case = CliCase::new("canonical-add-restart");
    case.write_fake_team(&["worker"]);
    let helper = case.write_external_role("helper");

    let quick = case.quick_start();
    assert!(
        launched_ok(&quick_json(&quick)),
        "RED1 setup: quick-start must launch fake team enough to continue; output={}",
        output_text(&quick)
    );

    let add = case.run([
        "add-agent",
        "helper",
        "--role-file",
        helper.to_str().unwrap(),
        "--workspace",
        case.ws(),
        "--team",
        case.team(),
        "--no-display",
        "--json",
    ]);
    assert_success_json(&add, "RED1 setup: add-agent helper must succeed");

    let restart = case.restart();
    let restart_text = output_text(&restart);
    assert!(
        !restart_text.contains("state save conflict") && !restart_text.contains("SaveConflict"),
        "RED1: restart after successful add-agent must not hit SaveConflict; output={restart_text}"
    );
    assert!(
        !has_event_for_agent(&case, "restart.agent_skipped_not_in_spec", "helper"),
        "RED1: helper came from add-agent dynamic_role_file and must not be pruned as not-in-spec; events={}",
        events_text(&case)
    );
    assert!(
        restart.status.success() || launched_ok(&quick_json(&restart)),
        "RED1: restart may be leader-unbound degraded, but must complete the fake-team restart path; output={restart_text}"
    );

    let state = load_runtime_state(&case.workspace).expect("read state after restart");
    assert!(
        state.pointer("/agents/helper").is_some()
            && state
                .pointer(&format!("/teams/{}/agents/helper", case.team()))
                .is_some(),
        "RED1: dynamic helper must survive restart in root and selected team state; state={state}"
    );
    let spec = std::fs::read_to_string(runtime_spec_path(&case.workspace, case.team()))
        .expect("read runtime spec after restart");
    assert!(
        spec.contains("helper") && spec.contains("route-helper"),
        "RED1: runtime spec rebuilt for restart must include helper agent and route-helper; spec={spec}"
    );
}

#[test]
#[serial(env)]
fn missing_dynamic_role_file_fails_closed_without_pruning_live_helper() {
    let case = CliCase::new("missing-dynamic-role");
    case.write_fake_team(&["worker"]);
    let helper = case.write_external_role("helper");

    let quick = case.quick_start();
    assert!(
        launched_ok(&quick_json(&quick)),
        "RED2 setup: quick-start must launch fake team enough to continue; output={}",
        output_text(&quick)
    );
    let add = case.run([
        "add-agent",
        "helper",
        "--role-file",
        helper.to_str().unwrap(),
        "--workspace",
        case.ws(),
        "--team",
        case.team(),
        "--no-display",
        "--json",
    ]);
    assert_success_json(&add, "RED2 setup: add-agent helper must succeed");
    std::fs::remove_file(&helper).expect("remove dynamic helper role file");

    let restart = case.restart();
    let text = output_text(&restart);
    assert!(
        !restart.status.success(),
        "RED2: missing dynamic role file must fail closed; output={text}"
    );
    assert!(
        text.contains("dynamic role file missing") && text.contains(helper.to_str().unwrap()),
        "RED2: error must name the missing dynamic role file path; output={text}"
    );
    assert!(
        text.contains("restore") && text.contains("remove-agent"),
        "RED2: error action must point to restoring the role file or removing the dynamic agent; output={text}"
    );
    assert!(
        !text.contains("state save conflict") && !text.contains("SaveConflict"),
        "RED2: missing role source is the primary error; restart must not fall through to SaveConflict; output={text}"
    );
    let state = load_runtime_state(&case.workspace).expect("read state after missing-role restart");
    assert!(
        state.pointer("/agents/helper").is_some()
            && state
                .pointer(&format!("/teams/{}/agents/helper", case.team()))
                .is_some(),
        "RED2: fail-closed restart must not prune the live helper from state; state={state}"
    );
}

#[test]
#[serial(env)]
fn removed_static_role_still_prunes_non_dynamic_agent() {
    let case = CliCase::new("static-role-prune");
    case.write_fake_team(&["worker"]);

    let quick = case.quick_start();
    assert!(
        launched_ok(&quick_json(&quick)),
        "RED3 setup: quick-start must launch fake team enough to continue; output={}",
        output_text(&quick)
    );
    seed_stopped_static_agent(&case, "old");

    let restart = case.restart();
    let text = output_text(&restart);
    assert!(
        restart.status.success() || launched_ok(&quick_json(&restart)),
        "RED3 guard: static removed stopped role should still follow the normal restart prune path; output={text}"
    );
    let restart_json = quick_json(&restart);
    assert!(
        !json_string_array_contains(restart_json.get("agents"), "old"),
        "RED3 guard: removed static old role must not remain in restart roster; output={text}"
    );
    let events = events_text(&case);
    assert!(
        has_event_for_agent(&case, "restart.agent_skipped_not_in_spec", "old"),
        "RED3 guard: restart must still emit skipped-not-in-spec for removed static old role; events={events}"
    );
    let state = load_runtime_state(&case.workspace).expect("read state after static prune");
    assert!(
        !agent_has_live_topology(state.pointer("/agents/old"))
            && !agent_has_live_topology(
                state.pointer(&format!("/teams/{}/agents/old", case.team()))
            )
            && state.pointer("/agents/old/dynamic_role_file").is_none()
            && state
                .pointer(&format!("/teams/{}/agents/old/dynamic_role_file", case.team()))
                .is_none(),
        "RED3 guard: removed static old role may leave a roster stub, but must not be live or dynamic; state={state}"
    );
}

#[test]
fn persist_guard_still_rejects_live_topology_deletion() {
    let root = test_tmp_root().join(format!(
        "ta-0530-persist-{}-{}",
        std::process::id(),
        CASE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(workspace.join(".team/runtime")).expect("create runtime dir");
    let latest = json!({
        "session_name": "team-current",
        "active_team_key": "current",
        "team_key": "current",
        "agents": {
            "helper": {
                "agent_id": "helper",
                "provider": "fake",
                "status": "running",
                "window": "helper",
                "pane_id": "%7",
                "pane_pid": 70_007,
                "spawned_at": "2026-07-11T00:00:00Z",
                "spawn_epoch": 1
            }
        },
        "teams": {
            "current": {
                "team_key": "current",
                "session_name": "team-current",
                "agents": {
                    "helper": {
                        "agent_id": "helper",
                        "provider": "fake",
                        "status": "running",
                        "window": "helper",
                        "pane_id": "%7",
                        "pane_pid": 70_007,
                        "spawned_at": "2026-07-11T00:00:00Z",
                        "spawn_epoch": 1
                    }
                }
            }
        }
    });
    save_runtime_state(&workspace, &latest).expect("seed latest state");

    let incoming = json!({
        "session_name": "team-current",
        "active_team_key": "current",
        "team_key": "current",
        "agents": {},
        "teams": {
            "current": {
                "team_key": "current",
                "session_name": "team-current",
                "agents": {}
            }
        }
    });
    let err = save_runtime_state(&workspace, &incoming)
        .expect_err("RED4: live topology deletion must remain a SaveConflict");
    assert!(
        matches!(err, StateError::SaveConflict(_)),
        "RED4: persist guard must still return typed SaveConflict; err={err}"
    );
    let message = err.to_string();
    for field in ["pane_id", "pane_pid", "window", "spawned_at", "spawn_epoch"] {
        assert!(
            message.contains(field),
            "RED4: SaveConflict must still name protected live topology field {field}; message={message}"
        );
    }
    let saved = load_runtime_state(&workspace).expect("read state after conflict");
    for pointer in [
        "/agents/helper/window",
        "/agents/helper/pane_id",
        "/agents/helper/pane_pid",
        "/agents/helper/spawned_at",
        "/agents/helper/spawn_epoch",
        "/teams/current/agents/helper/window",
        "/teams/current/agents/helper/pane_id",
        "/teams/current/agents/helper/pane_pid",
        "/teams/current/agents/helper/spawned_at",
        "/teams/current/agents/helper/spawn_epoch",
    ] {
        assert_eq!(
            saved.pointer(pointer),
            latest.pointer(pointer),
            "RED4: failed stale save must leave protected live topology field {pointer} unchanged; saved={saved}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

struct CliCase {
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    team_dir: PathBuf,
    team: String,
}

impl CliCase {
    fn new(tag: &str) -> Self {
        let seq = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = test_tmp_root().join(format!("ta-0530-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let workspace = root.join("workspace");
        let team_dir = root.join("team");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let team = format!("aa0530{}{}", std::process::id(), seq);
        Self {
            root,
            home,
            workspace,
            team_dir,
            team,
        }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn team(&self) -> &str {
        &self.team
    }

    fn write_fake_team(&self, agents: &[&str]) {
        write(
            &self.team_dir.join("TEAM.md"),
            &format!(
                "---\nname: {}\nobjective: 0.5.30 add-agent restart contract.\nprovider: fake\ndisplay_backend: none\n---\n\nTeam.\n",
                self.team
            ),
        );
        let agents_dir = self.team_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents dir");
        for id in agents {
            write(&agents_dir.join(format!("{id}.md")), &role_doc(id));
        }
    }

    fn write_external_role(&self, id: &str) -> PathBuf {
        let path = self.root.join(format!("{id}.md"));
        write(&path, &role_doc(id));
        path
    }

    fn quick_start(&self) -> Output {
        self.run([
            "quick-start",
            self.team_dir.to_str().unwrap(),
            "--workspace",
            self.ws(),
            "--team-id",
            self.team(),
            "--yes",
            "--no-display",
            "--json",
        ])
    }

    fn restart(&self) -> Output {
        self.run(["restart", self.ws(), "--team", self.team(), "--json"])
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Output {
        let mut command = Command::new(team_agent_bin());
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("TEAM_AGENT_TEST_TMP", test_tmp_root())
            .env("TMPDIR", test_tmp_root());
        for key in [
            "TMUX",
            "TMUX_PANE",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_MACHINE_FINGERPRINT",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        let output = command.output().expect("run team-agent");
        eprintln!(
            "$ team-agent {}\nexit={}\nstdout={}\nstderr={}",
            command_line_for_log(&command),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for CliCase {
    fn drop(&mut self) {
        if self.workspace.exists() {
            let _ = Command::new(team_agent_bin())
                .args([
                    "shutdown",
                    "--workspace",
                    self.ws(),
                    "--team",
                    self.team(),
                    "--keep-logs",
                    "--json",
                ])
                .current_dir(&self.workspace)
                .env("HOME", &self.home)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .output();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn team_agent_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_team-agent")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe().ok().and_then(|exe| {
                let deps = exe.parent()?;
                let debug = deps.parent()?;
                let candidate = debug.join("team-agent");
                candidate.exists().then_some(candidate)
            })
        })
        .expect("team-agent test binary must be available")
}

fn test_tmp_root() -> PathBuf {
    std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn role_doc(id: &str) -> String {
    format!(
        "---\nname: {id}\nrole: Fake worker {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nFake worker {id}.\n"
    )
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().expect("path parent")).expect("create parent");
    std::fs::write(path, content)
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn quick_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}\noutput={}",
            output_text(output)
        )
    })
}

fn launched_ok(json: &Value) -> bool {
    if json.get("ok").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let status = json
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let all_workers = json
        .pointer("/readiness/all_workers_spawned")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || json
            .pointer("/worker_readiness/all_workers_spawned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    all_workers
        && matches!(
            status,
            "leader_receiver_unbound" | "pending_tool_load" | "pending_session_capture"
        )
}

fn json_string_array_contains(value: Option<&Value>, needle: &str) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(needle)))
}

fn agent_has_live_topology(agent: Option<&Value>) -> bool {
    let Some(agent) = agent else {
        return false;
    };
    ["pane_id", "pane_pid", "window", "spawned_at", "spawn_epoch"]
        .iter()
        .any(|field| agent.get(*field).is_some_and(json_truthy))
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_i64().is_none_or(|n| n != 0),
        Value::String(value) => !value.is_empty() && value != "null",
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn assert_success_json(output: &Output, context: &str) -> Value {
    let json = quick_json(output);
    assert!(
        output.status.success() && json.get("ok").and_then(Value::as_bool) == Some(true),
        "{context}; output={}",
        output_text(output)
    );
    json
}

fn events_text(case: &CliCase) -> String {
    std::fs::read_to_string(case.workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn has_event_for_agent(case: &CliCase, event: &str, agent_id: &str) -> bool {
    events_text(case).lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|value| {
            value.get("event").and_then(Value::as_str) == Some(event)
                && value.get("agent_id").and_then(Value::as_str) == Some(agent_id)
        })
    })
}

fn seed_stopped_static_agent(case: &CliCase, agent_id: &str) {
    let path = case.workspace.join(".team/runtime/state.json");
    let raw = std::fs::read_to_string(&path).expect("read runtime state for fixture mutation");
    let mut state: Value =
        serde_json::from_str(&raw).expect("parse runtime state for fixture mutation");
    let agent = json!({
        "agent_id": agent_id,
        "provider": "fake",
        "model": "fake",
        "auth_mode": "subscription",
        "role": format!("Fake worker {agent_id}"),
        "status": "stopped"
    });
    state
        .as_object_mut()
        .expect("state object")
        .entry("agents")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("agents object")
        .insert(agent_id.to_string(), agent.clone());
    if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
        for team in teams.values_mut() {
            team.as_object_mut()
                .expect("team object")
                .entry("agents")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .expect("team agents object")
                .insert(agent_id.to_string(), agent.clone());
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap())
        .expect("write static-stopped fixture state");
}

fn output_text(output: &Output) -> String {
    format!(
        "exit={} stdout={} stderr={}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn command_line_for_log(command: &Command) -> String {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
