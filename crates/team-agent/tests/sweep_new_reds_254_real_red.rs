//! #254 second-pass real RED contracts.
//!
//! These tests intentionally enter through the public binary and real tmux/
//! coordinator/fake-worker surfaces. They are not source greps, seeded AlwaysLive
//! fixtures, or direct calls into implementation helpers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
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
    let mut coordinator = case.spawn_live_coordinator();
    wait_for_process(&mut coordinator.child, Duration::from_millis(200));
    assert!(
        coordinator.child.try_wait().ok().flatten().is_none(),
        "RC6 fixture must keep a real coordinator subprocess alive before shutdown"
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
        shutdown.stdout,
        shutdown.stderr
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
        state.pointer(&format!("/teams/{child}/parent_team_key")).and_then(Value::as_str),
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
fn depth_real_cli_without_parent_env_refuses_ambiguous_nested_creation() {
    let case = RealCase::new("depth-ambiguous");
    let parent = case.unique_team("parent");
    let child = case.unique_team("child");
    write_fake_team(case.root(), &parent, "parent_worker");
    write_fake_team(case.root(), &child, "child_worker");

    let parent_out = case.quick_start(&parent, "parent_worker", &[]);
    assert_success_json("parent quick-start fixture", &parent_out);

    let child_out = case.quick_start(&child, "child_worker", &[]);
    let state = maybe_state_value(case.root()).unwrap_or(Value::Null);
    assert!(
        child_out.code != Some(0)
            || parse_json_or_null(&child_out.stdout).get("ok").and_then(Value::as_bool) == Some(false),
        "bare CLI child/grandchild intent in an existing workspace must be rejected as ambiguous unless a parent env/flag is present; rc={:?} stdout={} stderr={} state={state}",
        child_out.code,
        child_out.stdout,
        child_out.stderr
    );
    assert!(
        state.pointer(&format!("/teams/{child}")).is_none(),
        "ambiguous no-parent-env nested creation must not write the child team; state={state}"
    );
    assert!(
        !case.has_session(&format!("team-{child}")),
        "ambiguous no-parent-env nested creation must not spawn team-{child}"
    );
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
            "--sender",
            "child_worker",
            "--json",
            "--no-wait",
        ],
        &[],
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
    let mut coordinator = case.case.spawn_live_coordinator();
    wait_for_process(&mut coordinator.child, Duration::from_millis(250));
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
    std::thread::sleep(Duration::from_millis(700));

    let state = state_value(case.case.root());
    let receiver = state.pointer(&format!("/teams/{}/leader_receiver/pane_id", case.child));
    let epoch = state.pointer(&format!("/teams/{}/owner_epoch", case.child)).and_then(Value::as_u64);
    assert_eq!(
        receiver.and_then(Value::as_str),
        Some(driver.as_str()),
        "real CLI claim-leader --team child must persist teams.child.leader_receiver.pane_id after live coordinator ticks; state={state}"
    );
    assert!(
        epoch.unwrap_or(0) > 0,
        "real CLI claim-leader --team child must persist owner_epoch>0 under teams.child after live ticks; state={state}"
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

    let mut coordinator = case.case.spawn_live_coordinator();
    let send = case.case.run_cli(
        &[
            "send",
            "child_worker",
            "RC3_CHILD_RESULT",
            "--workspace",
            case.case.root_str().as_str(),
            "--team",
            case.child.as_str(),
            "--sender",
            "leader",
            "--json",
            "--no-wait",
        ],
        &[],
        Duration::from_secs(6),
    );
    assert_success_json("send to child worker fixture", &send);
    wait_for_rows(case.case.root(), Duration::from_secs(5), |rows| {
        rows.results.iter().any(|row| row.owner_team_id == case.child)
            && rows.messages.iter().any(|row| {
                row.owner_team_id == case.child
                    && row.recipient == "leader"
                    && row.content.contains("Fake worker handled message")
            })
    });
    let _ = coordinator.child.kill();

    let rows = db_rows(case.case.root());
    assert!(
        rows.results.iter().any(|row| row.owner_team_id == case.child && row.agent_id == "child_worker"),
        "after explicit claim, fake child_worker result must be stored under child owner scope; rows={rows:?}"
    );
    assert!(
        rows.messages.iter().any(|row| {
            row.owner_team_id == case.child
                && row.recipient == "leader"
                && row.content.contains("Fake worker handled message")
                && !row.error.contains("leader_not_attached")
        }),
        "after explicit claim, generated child result notification must reach child leader instead of rebind_required; rows={rows:?}"
    );
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
    assert_success_json("teamA quick-start fixture", &case.quick_start(&team_a, "wrk1", &[]));
    assert_success_json("teamB quick-start fixture", &case.quick_start(&team_b, "wrk1", &[]));

    let mut coordinator = case.spawn_live_coordinator();
    let send = case.run_cli(
        &[
            "send",
            "wrk1",
            "TIT_OWNER_SCOPE_A",
            "--workspace",
            case.root_str().as_str(),
            "--team",
            team_a.as_str(),
            "--sender",
            "leader",
            "--json",
            "--no-wait",
        ],
        &[],
        Duration::from_secs(6),
    );
    assert_success_json("teamA send fixture", &send);
    wait_for_rows(case.root(), Duration::from_secs(5), |rows| {
        rows.results.iter().any(|row| row.agent_id == "wrk1")
            || rows.messages.iter().any(|row| row.content.contains("Fake worker handled message"))
    });
    let _ = coordinator.child.kill();

    let rows = db_rows(case.root());
    let direct = rows
        .messages
        .iter()
        .filter(|row| row.content.contains("TIT_OWNER_SCOPE_A"))
        .collect::<Vec<_>>();
    let generated_messages = rows
        .messages
        .iter()
        .filter(|row| row.content.contains("Fake worker handled message"))
        .collect::<Vec<_>>();
    assert!(
        direct.iter().any(|row| row.owner_team_id == team_a),
        "direct CLI send --team teamA must create a teamA row; rows={rows:?}"
    );
    assert!(
        rows.results.iter().any(|row| row.owner_team_id == team_a && row.agent_id == "wrk1"),
        "fake worker auto-report for a teamA task must store the result under teamA even when active_team_key is teamB; rows={rows:?}"
    );
    assert!(
        generated_messages.iter().any(|row| row.owner_team_id == team_a && row.recipient == "leader"),
        "fake worker generated leader notification for teamA must be owner_team_id=teamA; rows={rows:?}"
    );
    assert!(
        rows.results.iter().all(|row| row.owner_team_id != team_b || !row.envelope.contains("TIT_OWNER_SCOPE_A"))
            && generated_messages.iter().all(|row| row.owner_team_id != team_b),
        "no teamA generated result/notification may leak into active sibling teamB; direct={direct:?} rows={rows:?}"
    );
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
    let mut coordinator = case.case.spawn_live_coordinator();
    wait_for_process(&mut coordinator.child, Duration::from_millis(250));
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
    std::thread::sleep(Duration::from_millis(800));
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
        "raw state must keep child leader_receiver after live tick stale-save race; raw={raw}"
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
    assert_success_json("nested parent quick-start fixture", &case.quick_start(&parent, "parent_worker", &[]));
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
        let root = std::env::temp_dir().join(format!("ta254-real-{tag}-{}-{id}", std::process::id()));
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

    fn spawn_live_coordinator(&self) -> ManagedChild {
        let child = Command::new(bin())
            .args([
                "coordinator",
                "--workspace",
                self.root_str().as_str(),
                "--tick-interval",
                "0.1",
            ])
            .current_dir(&self.root)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_TEAM_ID")
            .env_remove("TEAM_AGENT_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn real coordinator subprocess");
        ManagedChild { child }
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

struct ManagedChild {
    child: Child,
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
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
    CliRun { code: status.code(), stdout, stderr, timed_out }
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
            "---\nname: {agent}\nrole: Real RED fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
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
    load_runtime_state(root)
        .unwrap_or_else(|e| panic!("runtime state must exist and parse for real fixture: {e}; root={}", root.display()))
}

fn maybe_state_value(root: &Path) -> Option<Value> {
    load_runtime_state(root).ok()
}

#[derive(Debug, Clone)]
struct MessageRow {
    message_id: String,
    owner_team_id: String,
    sender: String,
    recipient: String,
    status: String,
    error: String,
    content: String,
}

#[derive(Debug, Clone)]
struct ResultRow {
    result_id: String,
    owner_team_id: String,
    agent_id: String,
    status: String,
    envelope: String,
}

#[derive(Debug, Default, Clone)]
struct DbRows {
    messages: Vec<MessageRow>,
    results: Vec<ResultRow>,
}

fn db_rows(root: &Path) -> DbRows {
    let path = root.join(".team").join("runtime").join("team.db");
    if !path.exists() {
        return DbRows::default();
    }
    let _ = MessageStore::open(root);
    let conn = open_db(&path).expect("open team.db");
    let messages = conn
        .prepare("select message_id, coalesce(owner_team_id,''), sender, recipient, status, coalesce(error,''), content from messages order by created_at, message_id")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok(MessageRow {
                    message_id: row.get(0)?,
                    owner_team_id: row.get(1)?,
                    sender: row.get(2)?,
                    recipient: row.get(3)?,
                    status: row.get(4)?,
                    error: row.get(5)?,
                    content: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default();
    let results = conn
        .prepare("select result_id, coalesce(owner_team_id,''), agent_id, status, envelope from results order by created_at, result_id")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok(ResultRow {
                    result_id: row.get(0)?,
                    owner_team_id: row.get(1)?,
                    agent_id: row.get(2)?,
                    status: row.get(3)?,
                    envelope: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default();
    DbRows { messages, results }
}

fn wait_for_rows(root: &Path, timeout: Duration, predicate: impl Fn(&DbRows) -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let rows = db_rows(root);
        if predicate(&rows) {
            return;
        }
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

fn wait_for_process(child: &mut Child, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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
            let _ = Command::new("tmux").args(["kill-session", "-t", name]).status();
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
