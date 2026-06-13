#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::event_log::EventLog;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[ignore = "real-machine: real shutdown CLI path with bounded OS-probe hang fixture"]
#[file_serial(tmux)]
fn shutdown_returns_bounded_json_when_lsof_residual_probe_hangs() {
    let mut case = ShutdownHangCase::new("gatea-shutdown-lsof-hang");
    let helper = case.spawn_provider_like_process_without_workspace_arg();
    case.write_runtime_state();
    case.install_hanging_os_probe_stubs(helper.pid);

    let run = run_shutdown_with_timeout(
        &case.workspace,
        &case.stub_path(),
        Duration::from_millis(2_500),
    );
    let stdout_json = serde_json::from_str::<Value>(&run.stdout).ok();
    let events = EventLog::new(&case.workspace).tail(0).unwrap_or_default();
    let state = load_runtime_state(&case.workspace).unwrap_or(Value::Null);
    let failures = shutdown_contract_failures(&run, stdout_json.as_ref(), &events, &state);

    assert!(
        failures.is_empty(),
        "GateA Bug A shutdown contract failed:\n{}\nrun={run:?}\nevents={events:?}\nstate={state}",
        failures.join("\n")
    );
}

#[test]
fn shutdown_cleanup_transport_does_not_read_leader_receiver_socket() {
    let source = include_str!("../src/cli/mod.rs");
    let shutdown_body = slice_between(
        source,
        "/// `runtime.shutdown`(`cmd_shutdown`)",
        "pub fn shutdown_with_transport",
    );
    let shutdown_with_state_body = slice_between(
        source,
        "fn shutdown_with_transport_and_state(",
        "fn shutdown_state_for_team",
    );
    let combined = format!("{shutdown_body}\n{shutdown_with_state_body}");

    assert!(
        !combined.contains("stored_tmux_endpoint")
            && !combined.contains("leader_receiver_tmux_socket"),
        "C2/C4: shutdown lifecycle cleanup must derive worker-session transport from TmuxBackend::for_workspace(run_workspace); \
         leader_receiver.tmux_socket is leader-delivery-only and must not select kill/list/verify transport. body:\n{combined}"
    );
    assert!(
        !source.contains("\"worker_tmux_socket\""),
        "C3: do not add persisted worker_tmux_socket shadow state; derive the worker endpoint from workspace"
    );
}

#[test]
#[ignore = "real-machine: real shutdown CLI path with split leader/workspace tmux endpoints"]
#[file_serial(tmux)]
fn shutdown_kills_worker_session_on_workspace_socket_when_leader_socket_differs() {
    let case = EndpointCase::new("gatea-endpoint-split");
    case.spawn_workspace_worker_session();
    let leader_socket = case.spawn_leader_socket("leader-split");
    case.write_runtime_state(Some(&leader_socket));

    let run = run_shutdown_with_timeout(
        &case.workspace,
        &std::env::var("PATH").unwrap_or_default(),
        Duration::from_secs(4),
    );
    let body = serde_json::from_str::<Value>(&run.stdout).ok();
    let workspace_session_live = case
        .workspace_backend
        .has_session(&case.session)
        .unwrap_or(false);
    let leader_session_live = case.leader_session_live(&leader_socket, "leader-split");

    assert!(
        !run.timed_out,
        "C5: shutdown must return while selecting the workspace endpoint; run={run:?}"
    );
    assert!(
        !workspace_session_live,
        "C5: when leader_receiver.tmux_socket != workspace socket, shutdown must kill `{}` on TmuxBackend::for_workspace(workspace), \
         not on the leader socket. body={body:?} run={run:?}",
        case.session.as_str()
    );
    assert!(
        leader_session_live,
        "C5: shutdown must not use leader_receiver.tmux_socket as the worker cleanup endpoint; leader delivery socket session was killed. body={body:?} run={run:?}"
    );
}

#[test]
#[ignore = "real-machine: real shutdown CLI path with normal shared leader/workspace tmux endpoint"]
#[file_serial(tmux)]
fn shutdown_keeps_existing_behavior_when_leader_socket_equals_workspace_socket() {
    let case = EndpointCase::new("gatea-endpoint-same");
    case.spawn_workspace_worker_session();
    let workspace_socket = workspace_socket_name(&case.workspace);
    case.write_runtime_state(Some(&workspace_socket));

    let run = run_shutdown_with_timeout(
        &case.workspace,
        &std::env::var("PATH").unwrap_or_default(),
        Duration::from_secs(4),
    );
    let body = serde_json::from_str::<Value>(&run.stdout).ok();
    let workspace_session_live = case
        .workspace_backend
        .has_session(&case.session)
        .unwrap_or(false);

    assert!(
        !run.timed_out,
        "C6: normal same-socket shutdown must still return; run={run:?}"
    );
    assert!(
        !workspace_session_live,
        "C6: when leader socket equals workspace socket, shutdown must preserve existing behavior and remove the worker session; body={body:?} run={run:?}"
    );
}

#[test]
#[ignore = "real-machine: real shutdown CLI path with OS-probe timeout partial verification"]
#[file_serial(tmux)]
fn shutdown_keeps_session_killed_true_when_os_probe_timeout_has_no_residuals() {
    let mut case = ShutdownPartialCase::new("gatea-shutdown-partial-bugc");
    case.spawn_workspace_worker_session();
    case.write_runtime_state();
    let helper = case.spawn_provider_like_process_without_workspace_arg();
    case.install_timeout_probe_stubs(helper.pid);

    let run = run_shutdown_with_timeout(&case.workspace, &case.stub_path(), Duration::from_secs(4));
    let body = serde_json::from_str::<Value>(&run.stdout)
        .unwrap_or_else(|error| panic!("shutdown must emit JSON; error={error} run={run:?}"));
    let session_live = case
        .workspace_backend
        .has_session(&case.session)
        .unwrap_or(false);
    let failures = shutdown_partial_contract_failures(&run, &body, helper.pid, session_live);

    assert!(
        failures.is_empty(),
        "GateA shutdown PARTIAL Bug C contract failed:\n{}\nrun={run:?}\nbody={body}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: real shutdown CLI path with nested team source-of-truth writeback"]
#[file_serial(tmux)]
fn full_shutdown_writes_killed_session_aliases_to_team_source_of_truth() {
    let case = ShutdownWritebackCase::new("gatea-shutdown-partial-bugd");
    case.spawn_workspace_worker_session();
    case.write_runtime_state_with_stale_aliases();

    let run = run_shutdown_with_timeout(
        &case.workspace,
        &std::env::var("PATH").unwrap_or_default(),
        Duration::from_secs(4),
    );
    let body = serde_json::from_str::<Value>(&run.stdout)
        .unwrap_or_else(|error| panic!("shutdown must emit JSON; error={error} run={run:?}"));
    let raw = load_runtime_state(&case.workspace).expect("load state after shutdown");
    let status_current = run_status_json(&case.workspace, None);
    let status_teamdir = run_status_json(&case.workspace, Some("teamdir"));
    let failures =
        shutdown_writeback_contract_failures(&run, &body, &raw, &status_current, &status_teamdir);

    assert!(
        failures.is_empty(),
        "GateA shutdown PARTIAL Bug D contract failed:\n{}\nrun={run:?}\nbody={body}\nraw={raw}\nstatus_current={status_current}\nstatus_teamdir={status_teamdir}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: fake-provider quick-start/shutdown/status tmux chain"]
#[file_serial(tmux)]
fn fake_provider_quick_start_shutdown_does_not_resurrect_killed_session_aliases() {
    let case = QuickStartShutdownChainCase::new("gatea-shutdown-bugd-chain");
    let team_dir = case.write_fake_team_dir();

    let quick_start = run_team_agent_with_timeout(
        &case.workspace,
        &[
            "quick-start",
            team_dir.to_str().unwrap(),
            "--workspace",
            case.workspace.to_str().unwrap(),
            "--team-id",
            "current",
            "--fresh",
            "--yes",
            "--json",
        ],
        Duration::from_secs(12),
    );
    let quick_start_body = serde_json::from_str::<Value>(&quick_start.stdout).unwrap_or_else(|error| {
        panic!("quick-start must emit JSON before shutdown chain; error={error} run={quick_start:?}")
    });
    assert!(
        !quick_start.timed_out && quick_start.code == Some(0),
        "quick-start fixture must launch the fake-provider team before shutdown; run={quick_start:?} body={quick_start_body}"
    );
    let pre_status = run_status_json(&case.workspace, None);
    assert_realmachine_quick_start_shape(&case.workspace);
    assert!(
        team_has_running_agent(pre_status.pointer("/agents")),
        "fixture sanity: fake-provider quick-start should have running top-level agents before shutdown; status={pre_status}"
    );

    let shutdown = run_shutdown_with_timeout(
        &case.workspace,
        &std::env::var("PATH").unwrap_or_default(),
        Duration::from_secs(6),
    );
    let shutdown_body = serde_json::from_str::<Value>(&shutdown.stdout)
        .unwrap_or_else(|error| panic!("shutdown must emit JSON; error={error} run={shutdown:?}"));
    let raw = load_runtime_state(&case.workspace).expect("load state after chain shutdown");
    let post_status = run_status_json(&case.workspace, None);
    let failures =
        quick_start_chain_shutdown_failures(&shutdown, &shutdown_body, &raw, &post_status);

    assert!(
        failures.is_empty(),
        "GateA shutdown Bug D real quick-start chain contract failed:\n{}\nquick_start={quick_start:?}\nshutdown={shutdown:?}\nshutdown_body={shutdown_body}\nraw={raw}\npost_status={post_status}",
        failures.join("\n")
    );
}

fn shutdown_contract_failures(
    run: &CliRun,
    stdout_json: Option<&Value>,
    events: &[Value],
    state: &Value,
) -> Vec<String> {
    let mut failures = Vec::new();
    if run.timed_out {
        failures.push(
            "C1: shutdown --json must return within the bounded timeout even when lsof/OS residual probe hangs; current run timed out and had to be killed"
                .to_string(),
        );
    }
    let Some(body) = stdout_json else {
        failures.push(format!(
            "C2: shutdown must emit non-empty JSON on timeout/partial cleanup; code={:?} stdout={:?} stderr={:?}",
            run.code, run.stdout, run.stderr
        ));
        if !events
            .iter()
            .any(|event| event_name(event) == Some("lifecycle.shutdown.started"))
        {
            failures.push(
                "C4: lifecycle.shutdown.started must be recorded before destructive cleanup / OS probes; no started event was observed"
                    .to_string(),
            );
        }
        if state
            .pointer("/agents/worker/status")
            .and_then(Value::as_str)
            == Some("running")
        {
            failures.push(
                "state honesty: shutdown hang must not leave the only worker silently running with no JSON/event explanation"
                    .to_string(),
            );
        }
        return failures;
    };

    if body.get("ok").and_then(Value::as_bool) != Some(false) {
        failures.push(format!(
            "C2: OS-probe timeout must return ok=false, not success; body={body}"
        ));
    }
    let status = body.get("status").and_then(Value::as_str);
    if !matches!(status, Some("timeout" | "partial")) {
        failures.push(format!(
            "C2: status must be \"timeout\" or \"partial\" for bounded OS-probe degradation; body={body}"
        ));
    }
    if !body
        .get("phase")
        .and_then(Value::as_str)
        .is_some_and(|phase| !phase.is_empty())
    {
        failures.push(format!(
            "C2: timeout/partial shutdown JSON must include non-empty phase; body={body}"
        ));
    }
    if !events
        .iter()
        .any(|event| event_name(event) == Some("lifecycle.shutdown.started"))
    {
        failures.push(format!(
            "C4: lifecycle.shutdown.started must be recorded before cleanup probes; events={events:?}"
        ));
    }
    if !os_probe_timed_out(body) {
        failures.push(format!(
            "C3: JSON must mark the OS process residual probe as degraded/timed out rather than blocking or pretending it completed; body={body}"
        ));
    }
    failures
}

fn os_probe_timed_out(body: &Value) -> bool {
    body.pointer("/residuals/process_probe/status")
        .and_then(Value::as_str)
        == Some("timeout")
        || body
            .pointer("/residuals/process_probe_timed_out")
            .and_then(Value::as_bool)
            == Some(true)
        || body
            .get("phase")
            .and_then(Value::as_str)
            .is_some_and(|phase| {
                phase.contains("process") || phase.contains("lsof") || phase.contains("os_probe")
            })
}

fn shutdown_partial_contract_failures(
    run: &CliRun,
    body: &Value,
    helper_pid: u32,
    session_live: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    if run.timed_out {
        failures.push(
            "R1/C-C7: shutdown must return bounded JSON even when the residual OS probe times out"
                .to_string(),
        );
    }
    if run.code != Some(1) {
        failures.push(format!(
            "R1/C-C9: probe timeout keeps the conservative partial rc contract; expected rc=1, got {:?}. body={body}",
            run.code
        ));
    }
    if body.get("ok").and_then(Value::as_bool) != Some(false)
        || body.get("status").and_then(Value::as_str) != Some("partial")
    {
        failures.push(format!(
            "R1/C-C9: probe timeout must remain ok=false,status=partial while reporting truthful kill state. body={body}"
        ));
    }
    if body.get("phase").and_then(Value::as_str) != Some("os_probe") {
        failures.push(format!(
            "R1/C-C10: probe timeout must keep phase=\"os_probe\". body={body}"
        ));
    }
    if session_live {
        failures.push("R1: fixture sanity failed: workspace tmux session is still live after shutdown, so this is not the partial-verification case".to_string());
    }
    if !json_array_empty(body.pointer("/residuals/sessions")) {
        failures.push(format!(
            "R1/C-C7: fixture must have no tmux session residuals; body={body}"
        ));
    }
    if !json_array_empty(body.pointer("/residuals/processes")) {
        failures.push(format!(
            "R1/C-C7: fixture must have no process residuals; body={body}"
        ));
    }
    if body.get("session_killed").and_then(Value::as_bool) != Some(true) {
        failures.push(format!(
            "R1/C-C7: empty session/process residuals and no kill error mean the tmux session was killed; OS-probe timeout must not falsify session_killed. body={body}"
        ));
    }
    if body.get("verification_degraded").and_then(Value::as_bool) != Some(true) {
        failures.push(format!(
            "R1/C-C8/C-C10: OS-probe timeout must be reported separately as verification_degraded=true and may coexist with session_killed=true. body={body}"
        ));
    }
    if body.get("probe_timeout_kind").and_then(Value::as_str) != Some("lsof_cwd") {
        failures.push(format!(
            "R2/C-C8: JSON must identify the timed-out probe with probe_timeout_kind=\"lsof_cwd\". body={body}"
        ));
    }
    match body.get("probe_timeout").and_then(Value::as_object) {
        Some(timeout) => {
            if timeout.get("probe").and_then(Value::as_str) != Some("lsof_cwd") {
                failures.push(format!(
                    "R2: probe_timeout.probe must identify the timed-out probe as lsof_cwd. body={body}"
                ));
            }
            if timeout.get("pid").and_then(Value::as_u64) != Some(u64::from(helper_pid)) {
                failures.push(format!(
                    "R2: probe_timeout.pid must identify the process whose cwd probe timed out ({helper_pid}). body={body}"
                ));
            }
            if timeout
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map_or(true, |timeout_ms| timeout_ms == 0)
            {
                failures.push(format!(
                    "R2: probe_timeout.timeout_ms must be a positive bounded timeout value. body={body}"
                ));
            }
        }
        None => failures.push(format!(
            "R2: OS-probe timeout JSON must include probe_timeout={{probe,pid,timeout_ms}} diagnostics. body={body}"
        )),
    }
    failures
}

fn shutdown_writeback_contract_failures(
    run: &CliRun,
    body: &Value,
    raw: &Value,
    status_current: &Value,
    status_teamdir: &Value,
) -> Vec<String> {
    let mut failures = Vec::new();
    if run.timed_out {
        failures.push(
            "R3: full shutdown must return bounded JSON before state writeback assertions"
                .to_string(),
        );
    }
    if body.get("session_killed").and_then(Value::as_bool) != Some(true) {
        failures.push(format!(
            "R3: fixture shutdown must kill team-current before checking teams source-of-truth writeback. body={body}"
        ));
    }
    for team in ["current", "teamdir"] {
        if team_has_running_agent(raw.pointer(&format!("/teams/{team}"))) {
            failures.push(format!(
                "R3/D-C1/D-C5/D-C6: raw state teams.{team}.agents is source-of-truth for killed session team-current and must not remain running. raw={raw}"
            ));
        }
    }
    if team_has_running_agent(raw.pointer("/agents")) {
        failures.push(format!(
            "R3/D-C6: top-level derived agents cache must agree with killed-session teams and not show running. raw={raw}"
        ));
    }
    if !team_has_running_agent(raw.pointer("/teams/other")) {
        // The seeded other team started running. If it is not running here, shutdown widened too far.
        failures.push(format!(
            "R4/D-C4: unmatched teams.other/session=team-other was changed; shutdown must only stop aliases matching killed session_name. raw={raw}"
        ));
    }
    if team_has_running_agent(status_current.pointer("/agents")) {
        failures.push(format!(
            "R3/D-C5: subsequent status --json --detail for active current must not resurrect running agents from stale teams.current. status={status_current}"
        ));
    }
    if team_has_running_agent(status_teamdir.pointer("/agents")) {
        failures.push(format!(
            "R4/D-C2/D-C5: selector --team teamdir must not resurrect running agents from alias teams.teamdir with the killed session. status={status_teamdir}"
        ));
    }
    failures
}

fn quick_start_chain_shutdown_failures(
    run: &CliRun,
    body: &Value,
    raw: &Value,
    post_status: &Value,
) -> Vec<String> {
    let mut failures = Vec::new();
    if run.timed_out {
        failures.push("real chain: shutdown must return bounded JSON".to_string());
    }
    if body.get("session_killed").and_then(Value::as_bool) != Some(true) {
        failures.push(format!(
            "real chain: shutdown must report the killed team-current session truthfully. body={body}"
        ));
    }
    let raw_running = running_teams_for_session(raw, "team-current");
    if !raw_running.is_empty() {
        failures.push(format!(
            "real chain/R3/R4: raw state must not leave any teams[*] with session_name=team-current running after shutdown; running={raw_running:?} raw={raw}"
        ));
    }
    if team_has_running_agent(raw.pointer("/agents")) {
        failures.push(format!(
            "real chain/R3: raw top-level agents cache must not remain running after killed session shutdown. raw={raw}"
        ));
    }
    if team_has_running_agent(post_status.pointer("/agents")) {
        failures.push(format!(
            "real chain/R3/R4: post-shutdown status --json --detail must not project killed-session agents as running. status={post_status}"
        ));
    }
    if post_status
        .get("tmux_session_present")
        .and_then(Value::as_bool)
        != Some(false)
    {
        failures.push(format!(
            "real chain: post-shutdown status must observe tmux_session_present=false for killed session. status={post_status}"
        ));
    }
    failures
}

fn assert_realmachine_quick_start_shape(workspace: &Path) {
    let state = load_runtime_state(workspace).expect("load state after quick-start");
    let current = state
        .pointer("/teams/current")
        .unwrap_or_else(|| panic!("quick-start fixture must persist teams.current; state={state}"));
    assert_eq!(
        current.get("session_name").and_then(Value::as_str),
        Some("team-current"),
        "quick-start fixture must match real-machine current session_name; state={state}"
    );
    assert!(
        current.get("team_key").is_none(),
        "quick-start fixture must be real-machine-shaped: teams.current does not carry payload team_key; state={state}"
    );
    let team_dir = current
        .get("team_dir")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("quick-start fixture must persist teams.current.team_dir; state={state}")
        });
    assert!(
        Path::new(team_dir)
            .file_name()
            .and_then(|name| name.to_str())
            == Some("teamdir"),
        "quick-start fixture must use a real teamdir basename so lossy save-key recomputation can be observed; state={state}"
    );
}

fn json_array_empty(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|array| array.is_empty())
}

fn team_has_running_agent(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let agents = value.get("agents").unwrap_or(value);
    agents.as_object().is_some_and(|agents| {
        agents
            .values()
            .any(|agent| agent.get("status").and_then(Value::as_str) == Some("running"))
    })
}

fn running_teams_for_session(state: &Value, session_name: &str) -> Vec<String> {
    state
        .get("teams")
        .and_then(Value::as_object)
        .map(|teams| {
            teams
                .iter()
                .filter(|(_, team)| {
                    team.get("session_name").and_then(Value::as_str) == Some(session_name)
                        && team_has_running_agent(Some(team))
                })
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn event_name(event: &Value) -> Option<&str> {
    event
        .get("event")
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)
}

fn slice_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_idx = source
        .find(start)
        .unwrap_or_else(|| panic!("missing start marker {start:?}"));
    let after_start = &source[start_idx..];
    let end_idx = after_start
        .find(end)
        .unwrap_or_else(|| panic!("missing end marker {end:?} after {start:?}"));
    &after_start[..end_idx]
}

#[derive(Debug)]
struct CliRun {
    code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

fn run_shutdown_with_timeout(workspace: &Path, stub_path: &str, timeout: Duration) -> CliRun {
    run_team_agent_with_path_timeout(
        workspace,
        &[
            "shutdown",
            "--workspace",
            workspace.to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
        stub_path,
        timeout,
    )
}

fn run_team_agent_with_timeout(workspace: &Path, args: &[&str], timeout: Duration) -> CliRun {
    run_team_agent_with_path_timeout(
        workspace,
        args,
        &std::env::var("PATH").unwrap_or_default(),
        timeout,
    )
}

fn run_team_agent_with_path_timeout(
    workspace: &Path,
    args: &[&str],
    path: &str,
    timeout: Duration,
) -> CliRun {
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(workspace)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn team-agent {args:?}: {error}"));
    wait_child(&mut child, timeout)
}

fn run_status_json(workspace: &Path, team: Option<&str>) -> Value {
    let mut args = vec![
        "status".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().to_string(),
        "--json".to_string(),
        "--detail".to_string(),
    ];
    if let Some(team) = team {
        args.push("--team".to_string());
        args.push(team.to_string());
    }
    let output = Command::new(bin())
        .args(&args)
        .current_dir(workspace)
        .output()
        .expect("run status --json --detail");
    assert!(
        output.status.success(),
        "status command failed team={team:?} stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "status must emit JSON team={team:?}; error={error} stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn wait_child(child: &mut Child, timeout: Duration) -> CliRun {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) = read_child_pipes(child);
                return CliRun {
                    code: status.code(),
                    timed_out: false,
                    stdout,
                    stderr,
                };
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let (stdout, stderr) = read_child_pipes(child);
                return CliRun {
                    code: None,
                    timed_out: true,
                    stdout,
                    stderr,
                };
            }
            Err(error) => panic!("wait shutdown child: {error}"),
        }
    }
}

fn read_child_pipes(child: &mut Child) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (stdout, stderr)
}

struct ShutdownHangCase {
    workspace: PathBuf,
    bin_dir: PathBuf,
    backend: TmuxBackend,
    helpers: Vec<Child>,
}

impl ShutdownHangCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let bin_dir = workspace.join("stub-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let backend = TmuxBackend::for_workspace(&workspace);
        backend.kill_server();
        Self {
            workspace,
            bin_dir,
            backend,
            helpers: Vec::new(),
        }
    }

    fn spawn_provider_like_process_without_workspace_arg(&mut self) -> HelperProcess {
        let script = self.bin_dir.join("node");
        write_executable(&script, "#!/bin/sh\nwhile true; do sleep 60; done\n");
        let child = Command::new(&script)
            .arg("provider-helper")
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn provider-like helper process");
        let pid = child.id();
        self.helpers.push(child);
        HelperProcess { pid }
    }

    fn write_runtime_state(&self) {
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "current",
                "session_name": "team-gatea-shutdown",
                "agents": {
                    "worker": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker",
                        "spawn_cwd": self.workspace.to_string_lossy().to_string()
                    }
                }
            }),
        )
        .unwrap();
    }

    fn install_hanging_os_probe_stubs(&self, helper_pid: u32) {
        let real_path = std::env::var("PATH").unwrap_or_default();
        write_executable(
            &self.bin_dir.join("ps"),
            &format!(
                r#"#!/bin/sh
case "$*" in
  "-axo pid=,ppid=")
    exit 0
    ;;
  "-axo pid=,ppid=,pgid=,sess=,command=")
    echo "{helper_pid} 1 {helper_pid} {helper_pid} node provider-helper"
    exit 0
    ;;
  *)
    PATH="{real_path}" exec /bin/ps "$@"
    ;;
esac
"#
            ),
        );
        write_executable(&self.bin_dir.join("lsof"), "#!/bin/sh\nsleep 60\n");
    }

    fn stub_path(&self) -> String {
        format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
}

struct EndpointCase {
    workspace: PathBuf,
    workspace_backend: TmuxBackend,
    session: SessionName,
}

impl EndpointCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let workspace_backend = TmuxBackend::for_workspace(&workspace);
        workspace_backend.kill_server();
        Self {
            workspace,
            workspace_backend,
            session: SessionName::new("team-current"),
        }
    }

    fn spawn_workspace_worker_session(&self) {
        self.workspace_backend
            .spawn_first(
                &self.session,
                &WindowName::new("worker"),
                &[
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "while true; do sleep 60; done".to_string(),
                ],
                &self.workspace,
                &std::collections::BTreeMap::new(),
            )
            .expect("spawn workspace worker tmux session");
    }

    fn spawn_leader_socket(&self, session: &str) -> String {
        let socket = self.leader_socket_name();
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(&socket)
            .args(["kill-server"])
            .output();
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&socket)
            .args([
                "new-session",
                "-d",
                "-s",
                session,
                "-n",
                "leader",
                "sleep 60",
            ])
            .output()
            .expect("spawn leader socket session");
        assert!(
            output.status.success(),
            "spawn leader socket session failed stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        socket
    }

    fn leader_socket_name(&self) -> String {
        format!(
            "ta-gatea-leader-{}-{}",
            std::process::id(),
            self.workspace
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("ws")
        )
    }

    fn leader_session_live(&self, socket: &str, session: &str) -> bool {
        Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .args(["has-session", "-t", session])
            .status()
            .is_ok_and(|status| status.success())
    }

    fn write_runtime_state(&self, leader_socket: Option<&str>) {
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "current",
                "session_name": self.session.as_str(),
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "pane_id": "%leader",
                    "tmux_socket": leader_socket,
                    "owner_epoch": 1
                },
                "agents": {
                    "worker": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker",
                        "spawn_cwd": self.workspace.to_string_lossy().to_string()
                    }
                }
            }),
        )
        .unwrap();
    }
}

impl Drop for EndpointCase {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(self.leader_socket_name())
            .args(["kill-server"])
            .output();
        self.workspace_backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct ShutdownPartialCase {
    workspace: PathBuf,
    bin_dir: PathBuf,
    workspace_backend: TmuxBackend,
    session: SessionName,
    helpers: Vec<Child>,
}

impl ShutdownPartialCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let bin_dir = workspace.join("stub-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let workspace_backend = TmuxBackend::for_workspace(&workspace);
        workspace_backend.kill_server();
        Self {
            workspace,
            bin_dir,
            workspace_backend,
            session: SessionName::new("team-current"),
            helpers: Vec::new(),
        }
    }

    fn spawn_workspace_worker_session(&self) {
        self.workspace_backend
            .spawn_first(
                &self.session,
                &WindowName::new("worker"),
                &[
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "while true; do sleep 60; done".to_string(),
                ],
                &self.workspace,
                &std::collections::BTreeMap::new(),
            )
            .expect("spawn workspace worker tmux session");
    }

    fn write_runtime_state(&self) {
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "current",
                "session_name": self.session.as_str(),
                "agents": {
                    "worker": running_agent(&self.workspace, "worker")
                }
            }),
        )
        .unwrap();
    }

    fn spawn_provider_like_process_without_workspace_arg(&mut self) -> HelperProcess {
        let script = self.bin_dir.join("node");
        write_executable(&script, "#!/bin/sh\nwhile true; do sleep 60; done\n");
        let child = Command::new(&script)
            .arg("provider-helper")
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn provider-like helper process");
        let pid = child.id();
        self.helpers.push(child);
        HelperProcess { pid }
    }

    fn install_timeout_probe_stubs(&self, helper_pid: u32) {
        let real_path = std::env::var("PATH").unwrap_or_default();
        write_executable(
            &self.bin_dir.join("ps"),
            &format!(
                r#"#!/bin/sh
case "$*" in
  "-axo pid=,ppid=")
    exit 0
    ;;
  "-axo pid=,ppid=,pgid=,sess=,command=")
    echo "{helper_pid} 1 {helper_pid} {helper_pid} node provider-helper"
    exit 0
    ;;
  *)
    PATH="{real_path}" exec /bin/ps "$@"
    ;;
esac
"#
            ),
        );
        write_executable(&self.bin_dir.join("lsof"), "#!/bin/sh\nsleep 60\n");
    }

    fn stub_path(&self) -> String {
        format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
}

impl Drop for ShutdownPartialCase {
    fn drop(&mut self) {
        for child in &mut self.helpers {
            cleanup_child(child);
        }
        self.workspace_backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct ShutdownWritebackCase {
    workspace: PathBuf,
    workspace_backend: TmuxBackend,
    session: SessionName,
}

impl ShutdownWritebackCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let workspace_backend = TmuxBackend::for_workspace(&workspace);
        workspace_backend.kill_server();
        Self {
            workspace,
            workspace_backend,
            session: SessionName::new("team-current"),
        }
    }

    fn spawn_workspace_worker_session(&self) {
        self.workspace_backend
            .spawn_first(
                &self.session,
                &WindowName::new("worker"),
                &[
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "while true; do sleep 60; done".to_string(),
                ],
                &self.workspace,
                &std::collections::BTreeMap::new(),
            )
            .expect("spawn workspace worker tmux session");
    }

    fn write_runtime_state_with_stale_aliases(&self) {
        let alias_team_dir = self.workspace.join("teamdir");
        let other_team_dir = self.workspace.join("other");
        std::fs::create_dir_all(&alias_team_dir).unwrap();
        std::fs::create_dir_all(&other_team_dir).unwrap();
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "current",
                "session_name": self.session.as_str(),
                "agents": {
                    "worker": stopped_agent(&self.workspace, "worker"),
                    "reviewer": stopped_agent(&self.workspace, "reviewer")
                },
                "teams": {
                    "current": team_state(&alias_team_dir, self.session.as_str(), "running"),
                    "teamdir": team_state(&alias_team_dir, self.session.as_str(), "running"),
                    "other": team_state(&other_team_dir, "team-other", "running")
                }
            }),
        )
        .unwrap();
    }
}

impl Drop for ShutdownWritebackCase {
    fn drop(&mut self) {
        self.workspace_backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct QuickStartShutdownChainCase {
    workspace: PathBuf,
    workspace_backend: TmuxBackend,
}

impl QuickStartShutdownChainCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        let workspace_backend = TmuxBackend::for_workspace(&workspace);
        workspace_backend.kill_server();
        Self {
            workspace,
            workspace_backend,
        }
    }

    fn write_fake_team_dir(&self) -> PathBuf {
        let team_dir = self.workspace.join("teamdir");
        std::fs::create_dir_all(team_dir.join("agents")).unwrap();
        std::fs::write(
            team_dir.join("TEAM.md"),
            "---\nname: current\nobjective: Gate A shutdown real quick-start fixture.\nprovider: fake\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team_dir.join("agents").join("worker_a.md"),
            role_doc("worker_a", "Worker A"),
        )
        .unwrap();
        std::fs::write(
            team_dir.join("agents").join("reviewer.md"),
            role_doc("reviewer", "Reviewer"),
        )
        .unwrap();
        team_dir
    }
}

impl Drop for QuickStartShutdownChainCase {
    fn drop(&mut self) {
        self.workspace_backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

fn workspace_socket_name(workspace: &Path) -> String {
    let argv = TmuxBackend::argv_for_workspace(
        workspace,
        &["tmux".to_string(), "has-session".to_string()],
    );
    argv.windows(2)
        .find_map(|pair| {
            if pair[0] == "-L" {
                Some(pair[1].clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("workspace backend argv must include -L socket; argv={argv:?}"))
}

impl Drop for ShutdownHangCase {
    fn drop(&mut self) {
        for child in &mut self.helpers {
            cleanup_child(child);
        }
        self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct HelperProcess {
    pid: u32,
}

fn running_agent(workspace: &Path, window: &str) -> Value {
    json!({
        "status": "running",
        "provider": "fake",
        "window": window,
        "spawn_cwd": workspace.to_string_lossy().to_string()
    })
}

fn stopped_agent(workspace: &Path, window: &str) -> Value {
    json!({
        "status": "stopped",
        "provider": "fake",
        "window": window,
        "spawn_cwd": workspace.to_string_lossy().to_string()
    })
}

fn team_state(team_dir: &Path, session_name: &str, agent_status: &str) -> Value {
    json!({
        "session_name": session_name,
        "team_dir": team_dir.to_string_lossy().to_string(),
        "agents": {
            "worker": {
                "status": agent_status,
                "provider": "fake",
                "window": "worker",
                "spawn_cwd": team_dir.to_string_lossy().to_string()
            },
            "reviewer": {
                "status": agent_status,
                "provider": "fake",
                "window": "reviewer",
                "spawn_cwd": team_dir.to_string_lossy().to_string()
            }
        }
    })
}

fn role_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

fn cleanup_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
