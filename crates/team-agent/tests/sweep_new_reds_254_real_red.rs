//! #254 second-pass real RED contracts.
//!
//! These tests intentionally enter through the public binary and real tmux/
//! coordinator/fake-worker surfaces. They are not source greps, seeded AlwaysLive
//! fixtures, or direct calls into implementation helpers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::state::persist::load_runtime_state;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{CaptureRange, PaneId, SessionName, Target, Transport};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn rc6_shutdown_real_cli_fake_team_json_no_residuals_and_runner_survives() {
    let case = RealCase::new("rc6-shutdown");
    let parent = case.unique_team("parent");
    write_fake_team(case.root(), &parent, "wrk1");
    let quick = case.quick_start(&parent, "wrk1", &[]);
    assert_success_json(
        "fixture quick-start must start a real fake team before shutdown",
        &quick,
    );
    let pre_state = state_value(case.root());
    assert!(
        pre_state.pointer(&format!("/teams/{parent}")).is_some(),
        "RC6 fixture must prove quick-start created real runtime state before shutdown; ok:true alone is not evidence. state={pre_state}"
    );
    assert!(
        case.has_session(&format!("team-{parent}")),
        "RC6 fixture must prove quick-start created the real tmux session before shutdown"
    );
    let shutdown = case.run_cli(
        &[
            "shutdown",
            "--workspace",
            case.root_str().as_str(),
            "--keep-logs",
            "--json",
        ],
        &[],
        // shutdown may legitimately wait through graceful stop_coordinator/
        // terminate_pid phases: SIGTERM 5s + SIGKILL 5s is about 10s. 8s
        // creates a false timeout red; 20s still catches a real hang.
        Duration::from_secs(20),
    );
    let report = parse_json_or_null(&shutdown.stdout);
    let residuals = workspace_processes(case.root());

    assert!(
        !shutdown.timed_out,
        "RC6 real shutdown must return; timed out stdout={} stderr={} residuals={residuals:?}",
        shutdown.stdout, shutdown.stderr
    );
    assert_eq!(
        shutdown.code,
        Some(0),
        "RC6 real shutdown subprocess must exit rc=0, not 143/SIGTERM. stdout={} stderr={} residuals={residuals:?}",
        shutdown.stdout,
        shutdown.stderr
    );
    assert!(
        report["ok"] == json!(true),
        "RC6 real shutdown stdout must be valid JSON ok=true; report={report} stdout={} stderr={}",
        shutdown.stdout,
        shutdown.stderr
    );
    assert!(
        residuals.is_empty(),
        "RC6 shutdown must leave no workspace coordinator/fake-worker/provider residuals; residuals={residuals:?} report={report}"
    );
    assert!(
        std::process::id() > 0,
        "runner/test process is still alive only if shutdown did not reap its caller"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn depth_real_nested_cli_persists_child_depth_and_refuses_grandchild() {
    let case = RealCase::new("depth-nested");
    let parent = case.unique_team("parent");
    let child = case.unique_team("child");
    let grandchild = case.unique_team("grandchild");
    write_fake_team(case.root(), &parent, "parent_worker");
    write_fake_team(case.root(), &child, "child_worker");
    write_fake_team(case.root(), &grandchild, "grand_worker");

    let parent_out = case.quick_start(&parent, "parent_worker", &[]);
    assert_success_json("parent quick-start fixture", &parent_out);
    let child_env = [
        ("TEAM_AGENT_OWNER_TEAM_ID", parent.as_str()),
        ("TEAM_AGENT_TEAM_ID", parent.as_str()),
        ("TEAM_AGENT_ID", "parent_worker"),
    ];
    let child_out = case.quick_start(&child, "child_worker", &child_env);
    assert_success_json("child quick-start from worker context", &child_out);

    let state = state_value(case.root());
    assert_eq!(
        state.pointer(&format!("/teams/{child}/team_depth")).and_then(Value::as_u64),
        Some(2),
        "child quick-start from TEAM_AGENT_OWNER_TEAM_ID=parent must persist teams.child.team_depth=2 after final launch writeback; state={state}"
    );
    assert_eq!(
        state
            .pointer(&format!("/teams/{child}/parent_team_key"))
            .and_then(Value::as_str),
        Some(parent.as_str()),
        "child quick-start must persist teams.child.parent_team_key=parent; state={state}"
    );

    let grand_env = [
        ("TEAM_AGENT_OWNER_TEAM_ID", child.as_str()),
        ("TEAM_AGENT_TEAM_ID", child.as_str()),
        ("TEAM_AGENT_ID", "child_worker"),
    ];
    let grand_out = case.quick_start(&grandchild, "grand_worker", &grand_env);
    let after = maybe_state_value(case.root()).unwrap_or(Value::Null);
    assert!(
        grand_out.code != Some(0)
            || parse_json_or_null(&grand_out.stdout).get("ok").and_then(Value::as_bool) == Some(false),
        "grandchild quick-start from child context must fail before state/tmux mutation; rc={:?} stdout={} stderr={} state={after}",
        grand_out.code,
        grand_out.stdout,
        grand_out.stderr
    );
    assert!(
        after.pointer(&format!("/teams/{grandchild}")).is_none(),
        "grandchild refusal must not write teams.grandchild; state={after}"
    );
    assert!(
        !case.has_session(&format!("team-{grandchild}")),
        "grandchild refusal must not create tmux session team-{grandchild}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
#[serial_test::serial(env)]
fn depth_independent_domain_teams_remain_roots_with_leader_named_workers() {
    let _hermetic_env =
        hermetic_guard::HermeticTestEnv::enter_with_leader("depth-independent-domains");
    let case = RealCase::new("depth-independent-domains");
    let tmux = std::env::var("TMUX").unwrap();
    let pane = std::env::var("TMUX_PANE").unwrap();
    let caller = [
        ("TMUX", tmux.as_str()),
        ("TMUX_PANE", pane.as_str()),
        ("TEAM_AGENT_LEADER_PROVIDER", "fake"),
    ];
    for domain in [
        "market-research",
        "grandchildcare-research",
        "childcare-research",
        "childrens-library",
        "child-independent",
    ] {
        let team = case.unique_team(domain);
        write_fake_team(case.root(), &team, "leader");
        let out = case.quick_start(&team, "leader", &caller);
        assert_success_json("independent domain team quick-start", &out);
        let state = state_value(case.root());
        let entry = state.pointer(&format!("/teams/{team}")).expect("started team");
        assert_eq!(entry["team_depth"], json!(1), "{team}: {state}");
        assert!(entry.get("parent_team_key").is_none(), "{team}: {state}");
        assert!(
            !out.stderr.contains("WorkerWindowNamedLeader")
                && !out.stdout.contains("WorkerWindowNamedLeader"),
            "leader is a valid worker agent_id; stdout={} stderr={}",
            out.stdout,
            out.stderr
        );
    }
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn rc3_unclaimed_child_leader_requires_claim_without_sentinel() {
    let case = nested_case("rc3-unclaimed");
    let send = case.case.run_cli(
        &[
            "send",
            "leader",
            "RC3_UNCLAIMED_CHILD",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--json",
        ],
        &[("TEAM_AGENT_ID", "child_worker")],
        Duration::from_secs(6),
    );
    let out = parse_json_or_null(&send.stdout);
    let state = maybe_state_value(case.case.root()).unwrap_or(Value::Null);

    assert!(
        out["reason"] == json!("leader_not_attached")
            || out["reason"] == json!("rebind_required")
            || out["verification"].as_str().is_some_and(|v| v.contains("claim-leader")),
        "before explicit claim, child worker -> child leader must be rebind_required with a claim hint; rc={:?} out={out} stderr={} state={state}",
        send.code,
        send.stderr
    );
    assert!(
        !state_contains_pane(&state, &case.child, "__team_agent_unbound__"),
        "unclaimed child leader must not be represented as a fake attached sentinel pane; state={state}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn rc3_claim_child_leader_persists_under_live_coordinator_tick() {
    let case = nested_case("rc3-claim-live");
    let driver = case.case.spawn_driver_pane("child-claim-driver");
    let claim = case.case.run_cli(
        &[
            "claim-leader",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--confirm",
            "--json",
        ],
        &[("TMUX_PANE", driver.as_str())],
        Duration::from_secs(6),
    );
    assert_success_json("claim-leader --team child fixture", &claim);

    let state = state_value(case.case.root());
    let receiver = state.pointer(&format!("/teams/{}/leader_receiver/pane_id", case.child));
    let epoch = state
        .pointer(&format!("/teams/{}/owner_epoch", case.child))
        .and_then(Value::as_u64);
    assert_eq!(
        receiver.and_then(Value::as_str),
        Some(driver.as_str()),
        "real CLI claim-leader --team child must persist teams.child.leader_receiver.pane_id after the completed claim; state={state}"
    );
    assert!(
        epoch.unwrap_or(0) > 0,
        "real CLI claim-leader --team child must persist owner_epoch>0 under teams.child after the completed claim; state={state}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn rc3_after_claim_child_worker_result_reaches_child_leader() {
    let case = nested_case("rc3-result");
    let driver = case.case.spawn_driver_pane("child-leader");
    let claim = case.case.run_cli(
        &[
            "claim-leader",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--confirm",
            "--json",
        ],
        &[("TMUX_PANE", driver.as_str())],
        Duration::from_secs(6),
    );
    assert_success_json("claim child leader fixture", &claim);

    let send = case.case.run_cli(
        &[
            "send",
            "child_worker",
            "RC3_CHILD_RESULT",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--json",
        ],
        &[],
        Duration::from_secs(6),
    );
    assert_success_json("send to child worker fixture", &send);
    let scopes = [(case.child.as_str(), "child_worker"), (case.child.as_str(), "leader")];
    wait_for_inbox(case.case.root(), &scopes, Duration::from_secs(5), |rows| {
        rows.iter().any(|row| row["kind"] == "result" && row["team"] == case.child)
            && rows.iter().any(|row| row["team"] == case.child && row["recipient"] == "leader"
                && row["content"].as_str().is_some_and(|text| text.contains("Fake worker handled message")))
    });

    let rows = inbox_records(case.case.root(), &scopes);
    assert!(rows.iter().any(|row| row["kind"] == "result" && row["team"] == case.child
            && row["envelope"]["agent_id"] == "child_worker"),
        "after explicit claim, the child result envelope must be durable in child scope: {rows:?}");
    assert!(rows.iter().any(|row| row["team"] == case.child && row["recipient"] == "leader"
            && row["content"].as_str().is_some_and(|text| text.contains("Fake worker handled message"))),
        "the generated child result must be addressed to the child leader: {rows:?}");
    let leader_capture = case.case.capture_pane(&driver);
    assert!(
        leader_capture.contains("Fake worker handled message") || leader_capture.contains("RC3_CHILD_RESULT"),
        "after explicit claim, child leader pane capture must show the delivered child result; capture={leader_capture:?} rows={rows:?}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn tit_fake_worker_auto_report_keeps_selected_team_when_active_sibling_differs() {
    let case = RealCase::new("tit-owner-leak");
    let team_a = case.unique_team("teamA");
    let team_b = case.unique_team("teamB");
    write_fake_team(case.root(), &team_a, "wrk1");
    write_fake_team(case.root(), &team_b, "wrk1");
    assert_success_json(
        "teamA quick-start fixture",
        &case.quick_start(&team_a, "wrk1", &[]),
    );
    assert_success_json(
        "teamB quick-start fixture",
        &case.quick_start(&team_b, "wrk1", &[]),
    );

    let send = case.run_cli(
        &[
            "send",
            "wrk1",
            "TIT_OWNER_SCOPE_A",
            "--workspace",
            case.root_str().as_str(),
            "--team",
            team_a.as_str(),
            "--json",
        ],
        &[],
        Duration::from_secs(6),
    );
    assert_success_json("teamA send fixture", &send);
    let scopes = [(team_a.as_str(), "wrk1"), (team_a.as_str(), "leader"),
        (team_b.as_str(), "wrk1"), (team_b.as_str(), "leader")];
    wait_for_inbox(case.root(), &scopes, Duration::from_secs(5), |rows| {
        rows.iter().any(|row| row["kind"] == "result" && row["envelope"]["agent_id"] == "wrk1")
            || rows.iter().any(|row| row["content"].as_str().is_some_and(|text| text.contains("Fake worker handled message")))
    });

    let rows = inbox_records(case.root(), &scopes);
    let direct = rows.iter().filter(|row| row["content"].as_str().is_some_and(|text| text.contains("TIT_OWNER_SCOPE_A"))).collect::<Vec<_>>();
    let generated_messages = rows.iter().filter(|row| row["content"].as_str().is_some_and(|text| text.contains("Fake worker handled message"))).collect::<Vec<_>>();
    let results = rows.iter().filter(|row| row["kind"] == "result").collect::<Vec<_>>();
    assert!(direct.iter().any(|row| row["team"] == team_a),
        "direct CLI send --team teamA must be durable under teamA: {rows:?}");
    assert!(results.iter().any(|row| row["team"] == team_a && row["envelope"]["agent_id"] == "wrk1"),
        "auto-report must retain teamA when active_team_key is teamB: {rows:?}");
    assert!(generated_messages.iter().any(|row| row["team"] == team_a && row["recipient"] == "leader"),
        "generated leader notification must retain teamA scope: {rows:?}");
    assert!(results.iter().all(|row| row["team"] != team_b || !row["envelope"].to_string().contains("TIT_OWNER_SCOPE_A"))
            && generated_messages.iter().all(|row| row["team"] != team_b),
        "no teamA result/notification may leak into active sibling teamB: direct={direct:?} rows={rows:?}");
    let state = state_value(case.root());
    let worker_pane = state
        .pointer(&format!("/teams/{team_a}/agents/wrk1/pane_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let worker_capture = case.capture_pane(worker_pane);
    assert!(
        worker_capture.contains("TIT_OWNER_SCOPE_A") || worker_capture.contains("Fake worker handled message"),
        "teamA fake-worker pane capture must show it consumed the teamA message; worker_pane={worker_pane:?} capture={worker_capture:?} state={state}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn tit_claim_child_leader_survives_live_tick_projection_race() {
    let case = nested_case("tit-claim-race");
    let driver = case.case.spawn_driver_pane("race-claimer");
    let claim = case.case.run_cli(
        &[
            "claim-leader",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--confirm",
            "--json",
        ],
        &[("TMUX_PANE", driver.as_str())],
        Duration::from_secs(6),
    );
    assert_success_json("claim race fixture", &claim);
    let status = case.case.run_cli(
        &[
            "status",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--json",
            "--detail",
        ],
        &[],
        Duration::from_secs(6),
    );
    assert_success_json("status --team child fixture", &status);

    let raw = state_value(case.case.root());
    let projected = parse_json_or_null(&status.stdout);
    assert_eq!(
        raw.pointer(&format!("/teams/{}/leader_receiver/pane_id", case.child))
            .and_then(Value::as_str),
        Some(driver.as_str()),
        "raw state must keep child leader_receiver after the completed claim and status projection; raw={raw}"
    );
    assert!(
        projected.to_string().contains(driver.as_str()),
        "status projection for child must expose the same claimed receiver; projected={projected} raw={raw}"
    );
}

struct NestedCase {
    case: RealCase,
    child: String,
}

fn nested_case(tag: &str) -> NestedCase {
    let case = RealCase::new(tag);
    let parent = case.unique_team("parent");
    let child = case.unique_team("child");
    write_fake_team(case.root(), &parent, "parent_worker");
    write_fake_team(case.root(), &child, "child_worker");
    assert_success_json(
        "nested parent quick-start fixture",
        &case.quick_start(&parent, "parent_worker", &[]),
    );
    assert_success_json(
        "nested child quick-start fixture",
        &case.quick_start(
            &child,
            "child_worker",
            &[
                ("TEAM_AGENT_OWNER_TEAM_ID", parent.as_str()),
                ("TEAM_AGENT_TEAM_ID", parent.as_str()),
                ("TEAM_AGENT_ID", "parent_worker"),
            ],
        ),
    );
    NestedCase { case, child }
}

struct RealCase {
    root: PathBuf,
    backend: TmuxBackend,
    id: u64,
}

impl RealCase {
    fn new(tag: &str) -> Self {
        let id = next_id();
        let root =
            std::env::temp_dir().join(format!("ta254-real-{tag}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let backend = TmuxBackend::for_workspace(&root);
        backend.kill_server();
        Self { root, backend, id }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    fn unique_team(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.id)
    }

    fn quick_start(&self, team_key: &str, _agent: &str, env: &[(&str, &str)]) -> CliRun {
        self.run_cli(
            &[
                "quick-start",
                self.root.join(team_key).to_string_lossy().as_ref(),
                "--workspace",
                self.root_str().as_str(),
                "--team-id",
                team_key,
                "--name",
                team_key,
                "--yes",
                "--json",
            ],
            env,
            Duration::from_secs(10),
        )
    }

    fn run_cli(&self, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> CliRun {
        run_cli_at(&self.root, args, env, timeout)
    }


    fn spawn_driver_pane(&self, label: &str) -> String {
        let session = SessionName::new(format!("team-{}-{}", label, self.id));
        let window = team_agent::transport::WindowName::new("driver");
        let result = self
            .backend
            .spawn_first(
                &session,
                &window,
                &[
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "while :; do sleep 60; done".to_string(),
                ],
                &self.root,
                &std::collections::BTreeMap::new(),
            )
            .expect("spawn driver pane");
        result.pane_id.as_str().to_string()
    }

    fn has_session(&self, session: &str) -> bool {
        self.backend
            .has_session(&SessionName::new(session))
            .unwrap_or(false)
    }

    fn capture_pane(&self, pane: &str) -> String {
        if pane.is_empty() {
            return String::new();
        }
        self.backend
            .capture(&Target::Pane(PaneId::new(pane)), CaptureRange::Tail(120))
            .map(|captured| captured.text)
            .unwrap_or_default()
    }
}

impl Drop for RealCase {
    fn drop(&mut self) {
        self.backend.kill_server();
        kill_default_sessions_with_id(self.id);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}


#[derive(Debug)]
struct CliRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn run_cli_at(cwd: &Path, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> CliRun {
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TEAM_AGENT_ID")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn team-agent {args:?}: {e}"));
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait().expect("wait killed child");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    CliRun {
        code: status.code(),
        stdout,
        stderr,
        timed_out,
    }
}

fn write_fake_team(root: &Path, team_key: &str, agent: &str) {
    let team = root.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: #254 real RED fixture.\nprovider: fake\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent}.md")),
        format!(
            "---\nname: {agent}\nrole: Real RED fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
}

fn assert_success_json(label: &str, run: &CliRun) {
    let body = parse_json_or_null(&run.stdout);
    assert!(
        !run.timed_out && run.code == Some(0) && body["ok"] == json!(true),
        "{label}: expected rc=0 JSON ok=true; rc={:?} timed_out={} stdout={} stderr={}",
        run.code,
        run.timed_out,
        run.stdout,
        run.stderr
    );
}

fn parse_json_or_null(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or(Value::Null)
}

fn state_value(root: &Path) -> Value {
    load_runtime_state(root).unwrap_or_else(|e| {
        panic!(
            "runtime state must exist and parse for real fixture: {e}; root={}",
            root.display()
        )
    })
}

fn maybe_state_value(root: &Path) -> Option<Value> {
    load_runtime_state(root).ok()
}

fn inbox_records(root: &Path, scopes: &[(&str, &str)]) -> Vec<Value> {
    scopes.iter().flat_map(|(team, recipient)| {
        let inbox = team_agent::inbox_store::read(root, recipient, 1000, Some(team))
            .expect("read scoped durable inbox");
        inbox["messages"].as_array().expect("inbox messages").clone()
    }).collect()
}

fn wait_for_inbox(root: &Path, scopes: &[(&str, &str)], timeout: Duration, predicate: impl Fn(&[Value]) -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if predicate(&inbox_records(root, scopes)) { return; }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn state_contains_pane(state: &Value, team: &str, pane: &str) -> bool {
    state
        .pointer(&format!("/teams/{team}/leader_receiver/pane_id"))
        .and_then(Value::as_str)
        == Some(pane)
        || state
            .pointer(&format!("/teams/{team}/team_owner/pane_id"))
            .and_then(Value::as_str)
            == Some(pane)
}


fn workspace_processes(root: &Path) -> Vec<String> {
    let needle = root.to_string_lossy();
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle.as_ref()))
        .filter(|line| {
            line.contains("team-agent coordinator")
                || line.contains("team-agent fake-worker")
                || line.contains("mcp-server")
        })
        .map(ToString::to_string)
        .collect()
}

fn kill_default_sessions_with_id(id: u64) {
    let marker = id.to_string();
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();
    let Ok(output) = output else {
        return;
    };
    for name in String::from_utf8_lossy(&output.stdout).lines() {
        if name.starts_with("team-") && name.contains(&marker) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", name])
                .status();
        }
    }
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (nanos & 0xffff_ffff).saturating_add(NEXT.fetch_add(1, Ordering::Relaxed))
}
