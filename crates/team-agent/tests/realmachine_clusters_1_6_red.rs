//! Real-machine command-file defects, clusters 1-6.
//!
//! These are canonical Team Agent CLI flows from
//! `.team/test-designs/realmachine-product-defects.md`. The harness command files are the contract;
//! if they fail on a real machine, the Rust framework changes rather than weakening the commands.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::lifecycle::{quick_start_with_transport, QuickStartReport};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-rm-c1-c6-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout must be JSON; code={:?} stdout={} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

// OLD seed: flat top-level `agents` / `tasks` (Python parity, pre-Bug-1/2).
// NEW seed (Bug 1/2 — team-in-team state scope, see
// tests/team_in_team_state_scope_red.rs): send_message projects the raw state
// through `project_top_level_view(active_team_key)` which sources agents/tasks
// from `teams[<key>].*`. Without the nested copy a single-team send refuses
// `target_not_in_team` BEFORE persistence (no DB schema is ever created → the
// downstream `select count(*) from messages` query in c4 fails with "no such
// table"). The seed is widened to carry BOTH the flat keys (so c1/c2/c5/c6
// helpers that read flat state directly stay working) AND the nested team-scoped
// copy (so the send/persist path resolves `worker_a` as in-team). The behavior
// under test (caller --message-id + dedup) is unchanged; this is fixture-only
// sync to the new state shape.
fn seed_runtime_state(workspace: &Path) {
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "leader": {"id": "leader"},
            "agents": {
                "worker_a": {
                    "provider": "fake",
                    "role": "Worker A",
                    "status": "running"
                }
            },
            "tasks": [],
            "active_team_key": "current",
            "teams": {"current": {
                "leader": {"id": "leader"},
                "agents": {
                    "worker_a": {
                        "provider": "fake",
                        "role": "Worker A",
                        "status": "running"
                    }
                },
                "tasks": []
            }}
        }),
    )
    .unwrap();
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c1_lifecycle_commands_resolve_active_teamdir_from_root_workspace_selector() {
    let fixture = seed_c1_active_team_fixture("c1-lifecycle");
    let mut failures = Vec::new();
    let role_file = fixture.root.join(".team").join("roles").join("worker_b.md");

    let cases: Vec<(&str, Vec<String>, &[&str])> = vec![
        (
            "CR-007/021/052 restart",
            vec![
                "restart".into(),
                "--workspace".into(),
                fixture.root_str(),
                "--json".into(),
            ],
            &["missing spec for restart", "team.spec.yaml"],
        ),
        (
            "CR-031/055 stop-agent",
            vec![
                "stop-agent".into(),
                "worker_a".into(),
                "--workspace".into(),
                fixture.root_str(),
                "--json".into(),
            ],
            &["missing spec:", "team.spec.yaml"],
        ),
        (
            "CR-033 reset-agent",
            vec![
                "reset-agent".into(),
                "worker_a".into(),
                "--discard-session".into(),
                "--workspace".into(),
                fixture.root_str(),
                "--json".into(),
            ],
            &["missing spec:", "team.spec.yaml"],
        ),
        (
            "CR-030 add-agent",
            vec![
                "add-agent".into(),
                "worker_b".into(),
                "--role-file".into(),
                role_file.to_string_lossy().to_string(),
                "--workspace".into(),
                fixture.root_str(),
                "--json".into(),
            ],
            &["missing TEAM.md", "spec compile failed"],
        ),
        (
            "CR-030 remove-agent",
            vec![
                "remove-agent".into(),
                "worker_a".into(),
                "--from-spec".into(),
                "--confirm".into(),
                "--force".into(),
                "--workspace".into(),
                fixture.root_str(),
                "--json".into(),
            ],
            &["missing spec:", "team.spec.yaml"],
        ),
    ];

    for (label, args, forbidden) in cases {
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let out = run(&refs, &fixture.root);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{stdout}\n{stderr}");
        if out.status.code() != Some(0) || forbidden.iter().any(|needle| combined.contains(needle))
        {
            failures.push(format!(
                "{label}: expected resolve_active_team(root)->teamdir/spec_path, got code={:?} stdout={stdout:?} stderr={stderr:?}",
                out.status.code()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "Cluster 1 resolve_active_team contract: after `quick-start teamdir`, lifecycle commands using --workspace <root> must select state.spec_path/team_dir, not root TEAM.md/team.spec.yaml:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c1_runtime_commands_resolve_active_team_for_status_send_and_collect() {
    let fixture = seed_c1_active_team_fixture("c1-runtime");
    let mut failures = Vec::new();

    let status = run(
        &[
            "status",
            "--workspace",
            fixture.teamdir.to_str().unwrap(),
            "--json",
        ],
        &fixture.root,
    );
    let status_json = stdout_json(&status);
    if status.status.code() != Some(0)
        || status_json
            .get("agents")
            .and_then(Value::as_object)
            .is_none_or(|agents| !agents.contains_key("worker_a"))
    {
        failures.push(format!(
            "status should resolve --workspace <teamdir> to the active run workspace/team projection; code={:?} json={status_json}",
            status.status.code()
        ));
    }

    let send = run(
        &[
            "send",
            "worker_a",
            "selector ping",
            "--workspace",
            fixture.teamdir.to_str().unwrap(),
            "--json",
        ],
        &fixture.root,
    );
    let send_json = stdout_json(&send);
    if send.status.code() != Some(0)
        || send_json["ok"] != json!(true)
        || send_json["reason"] == json!("target_not_in_team")
    {
        failures.push(format!(
            "send should resolve --workspace <teamdir> to the active run workspace and route against worker_a; code={:?} json={send_json}",
            send.status.code()
        ));
    }

    let collect = run(
        &[
            "collect",
            "--workspace",
            fixture.teamdir.to_str().unwrap(),
            "--json",
        ],
        &fixture.root,
    );
    let collect_stdout = String::from_utf8_lossy(&collect.stdout);
    let collect_stderr = String::from_utf8_lossy(&collect.stderr);
    if collect.status.code() != Some(0)
        || collect_stdout.contains("Cannot read")
        || collect_stdout.contains("/team.spec.yaml")
    {
        failures.push(format!(
            "collect should reuse resolve_active_team root->teamdir spec selection; code={:?} stdout={collect_stdout:?} stderr={collect_stderr:?}",
            collect.status.code()
        ));
    }

    assert!(
        failures.is_empty(),
        "Cluster 1 resolve_active_team contract: status/send/collect using --workspace <teamdir> must operate on the active run workspace projection/spec, not an empty teamdir-local runtime:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c2_stop_team_level_command_exists_and_keeps_state_while_stopping_runtime() {
    let ws = tmp_dir("c2-stop");
    seed_runtime_state(&ws);

    let out = run(
        &["stop", "--workspace", ws.to_str().unwrap(), "--json"],
        &ws,
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("invalid choice: 'stop'"),
        "CR-005: canonical `team-agent stop --workspace <ws> --json` must be a registered team-level stop verb, not argparse invalid-choice; stderr={err:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "CR-005: stop should stop the runtime while keeping state on a valid workspace; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        err
    );
    let value = stdout_json(&out);
    assert_eq!(
        value["ok"],
        json!(true),
        "stop must return a successful JSON envelope"
    );
    assert!(
        team_agent::state::persist::runtime_state_path(&ws).exists(),
        "stop keeps runtime state for later restart"
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c2_canonical_verbs_expose_help_instead_of_invalid_choice() {
    let cwd = tmp_dir("c2-help-verbs");
    let mut failures = Vec::new();
    for verb in ["start", "restart-agent", "purge-agent"] {
        let out = run(&[verb, "--help"], &cwd);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(0)
            || stderr.contains("invalid choice:")
            || !(stdout.contains("usage") || stderr.contains("usage"))
        {
            failures.push(format!(
                "{verb}: code={:?} stdout={stdout:?} stderr={stderr:?}",
                out.status.code()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "CR-030/032/035/063: canonical lifecycle verbs must be registered and expose help, not invalid-choice:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c2_quick_start_help_is_zero_token_help_only_and_does_not_compile_workspace() {
    let cwd = tmp_dir("c2-quick-help");
    let out = run(&["quick-start", "--help"], &cwd);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");

    let mut failures = Vec::new();
    if out.status.code() != Some(0) {
        failures.push(format!(
            "help exited {:?}, expected 0; stdout={stdout:?} stderr={stderr:?}",
            out.status.code()
        ));
    }
    if !(combined.contains("usage") && combined.contains("quick-start")) {
        failures.push(format!(
            "help output did not include quick-start usage text; got {combined:?}"
        ));
    }
    if combined.contains("spec compile failed") || combined.contains("missing TEAM.md") {
        failures.push(format!(
            "help path compiled/validated the current workspace; got {combined:?}"
        ));
    }
    if cwd.join("team.spec.yaml").exists() || cwd.join(".team").exists() {
        failures.push(format!(
            "help path created artifacts: team.spec.yaml={} .team={}",
            cwd.join("team.spec.yaml").exists(),
            cwd.join(".team").exists()
        ));
    }
    assert!(
        failures.is_empty(),
        "CR-047: `team-agent quick-start --help` must be local help-only, zero-token, zero-pollution:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c3_repeated_quick_start_uses_requested_team_identity_for_session_names() {
    let root = tmp_dir("c3-multiteam");
    seed_healthy_coordinator(&root);
    let teamdir = write_fake_team_dir(&root, "command-harness-template");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &teamdir,
        Some("command-harness-parent"),
        true,
        Some("parent"),
        &transport,
    );
    let second = quick_start_with_transport(
        &teamdir,
        Some("command-harness-child"),
        true,
        Some("child"),
        &transport,
    );

    assert!(
        first.is_ok(),
        "CR-040/042 fixture sanity: first quick-start should succeed; got {first:?}"
    );
    assert!(
        second.is_ok(),
        "CR-040/042: second quick-start from the same template with --name/--team-id child must not collide on the template-derived tmux session; got {second:?}"
    );
    let sessions = transport.spawned_sessions();
    assert_eq!(
        sessions.len(),
        2,
        "two requested teams should produce two spawned sessions"
    );
    assert_ne!(
        sessions[0], sessions[1],
        "CR-040/042: session names must derive from requested team identity, not reused template name"
    );
    assert!(
        sessions.iter().any(|s| s.contains("parent"))
            && sessions.iter().any(|s| s.contains("child")),
        "session names should carry requested team ids/names; got {sessions:?}"
    );
    let report = second.unwrap();
    match report {
        QuickStartReport::Ready { session_name, .. } => assert!(
            session_name.as_str().contains("child"),
            "reported child session should carry requested child identity; got {session_name}"
        ),
        other => panic!("second quick-start should reach Ready; got {other:?}"),
    }
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c4_send_generates_distinct_ids_for_repeated_payloads() {
    let ws = tmp_dir("c4-message-id");
    seed_runtime_state(&ws);

    let first = run(
        &[
            "send",
            "worker_a",
            "duplicate once",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
        &ws,
    );
    let second = run(
        &[
            "send",
            "worker_a",
            "duplicate once",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
        &ws,
    );
    let first_json = stdout_json(&first);
    let second_json = stdout_json(&second);

    let mut failures = Vec::new();
    let first_id = first_json.get("message_id").and_then(Value::as_str);
    let second_id = second_json.get("message_id").and_then(Value::as_str);
    if first_json["ok"] != json!(true)
        || second_json["ok"] != json!(true)
        || first_id.is_none()
        || second_id.is_none()
        || first_id == second_id
    {
        failures.push(format!(
            "canonical sends must generate distinct correlation ids; first={} second={}",
            first_json, second_json
        ));
    }
    let db = ws.join(".team").join("runtime").join("team.db");
    let conn = rusqlite::Connection::open(db).unwrap();
    let total: i64 = conn
        .query_row("select count(*) from messages", [], |row| row.get(0))
        .unwrap();
    let content_rows: i64 = conn
        .query_row(
            "select count(*) from messages where content = 'duplicate once'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if total != 2 || content_rows != 2 {
        failures.push(format!(
            "message store should contain both generated-id rows with unchanged content; total={total} content_rows={content_rows}"
        ));
    }
    assert!(
        failures.is_empty(),
        "CR-015/054: caller-supplied --message-id must be metadata and duplicate key:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c5_send_without_target_reports_routing_ambiguous_not_target_not_in_team() {
    let ws = tmp_dir("c5-no-target");
    seed_runtime_state(&ws);

    let out = run(
        &[
            "send",
            "fix the build",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ],
        &ws,
    );
    let value = stdout_json(&out);
    assert_eq!(
        value["reason"],
        json!("routing_ambiguous"),
        "CR-061/N27: no-target send should reject as routing_ambiguous unless a default route exists; got {value}"
    );
    assert_eq!(
        value["content"],
        json!("fix the build"),
        "CR-061: the prompt text is content, not an agent target; got {value}"
    );
    assert!(
        value.get("agent_id").is_none() || value["agent_id"].is_null(),
        "CR-061: no-target send must not report the prompt text as agent_id; got {value}"
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn c6_invalid_workspace_status_returns_shaped_error_without_polluting_invalid_path() {
    let root = tmp_dir("c6-invalid-root");
    let invalid = root.join("no").join("such").join("team");
    assert!(!invalid.exists(), "fixture path starts nonexistent");

    let out = run(
        &["status", "--workspace", invalid.to_str().unwrap(), "--json"],
        &root,
    );
    let value = stdout_json(&out);
    let mut failures = Vec::new();
    if out.status.code() == Some(0) {
        failures.push(format!(
            "invalid workspace returned exit 0 / successful empty status: {value}"
        ));
    }
    if value["ok"] != json!(false) {
        failures.push(format!(
            "invalid workspace must return ok=false; got {value}"
        ));
    }
    if !value["error"]
        .as_str()
        .is_some_and(|error| error.contains("workspace") && !error.starts_with("io:"))
    {
        failures.push(format!(
            "error should clearly name invalid workspace, not an opaque IO/logging failure; got {value}"
        ));
    }
    if let Some(log) = value.get("log").and_then(Value::as_str) {
        if Path::new(log).starts_with(&invalid) {
            failures.push(format!(
                "error log must not be written or pointed inside invalid workspace; log={log}"
            ));
        }
    } else {
        failures.push(format!("invalid workspace response should include shaped error log outside invalid path; got {value}"));
    }
    if invalid.join(".team").exists() {
        failures.push(format!(
            "status created .team runtime/log state under invalid path: {}",
            invalid.join(".team").display()
        ));
    }
    assert!(
        failures.is_empty(),
        "CR-064/N20: invalid workspace must fail clearly without state/log pollution:\n{}",
        failures.join("\n")
    );
}

fn write_fake_team_dir(root: &Path, team_name: &str) -> PathBuf {
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(teamdir.join("agents")).unwrap();
    std::fs::write(
        teamdir.join("TEAM.md"),
        format!(
            "---\nname: {team_name}\nobjective: Multi-team session derivation.\nprovider: fake\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        teamdir.join("agents").join("worker_a.md"),
        "---\nname: worker_a\nrole: Worker A\nprovider: fake\ntools:\n  - mcp_team\n---\n\nWorker.\n",
    )
    .unwrap();
    teamdir
}

#[derive(Debug)]
struct C1Fixture {
    root: PathBuf,
    teamdir: PathBuf,
}

impl C1Fixture {
    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }
}

fn seed_c1_active_team_fixture(tag: &str) -> C1Fixture {
    let root = tmp_dir(tag);
    let teamdir = write_fake_team_dir(&root, "command-harness-template");
    let spec = team_agent::compiler::compile_team(&teamdir).unwrap();
    let spec_path = teamdir.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).unwrap();
    let roles_dir = root.join(".team").join("roles");
    std::fs::create_dir_all(&roles_dir).unwrap();
    std::fs::write(
        roles_dir.join("worker_b.md"),
        "---\nname: worker_b\nrole: Worker B\nprovider: fake\ntools:\n  - mcp_team\n---\n\nWorker B.\n",
    )
    .unwrap();
    let now = "2026-05-27T10:00:00+00:00";
    team_agent::state::persist::save_runtime_state(
        &root,
        &json!({
            "active_team_key": "command-harness-template",
            "spec_path": spec_path.to_string_lossy().to_string(),
            "team_dir": teamdir.to_string_lossy().to_string(),
            "session_name": "team-command-harness-template",
            "leader": {"id": "leader"},
            "agents": {
                "worker_a": {
                    "provider": "fake",
                    "role": "Worker A",
                    "status": "running",
                    "session_id": "sess-worker-a",
                    "first_send_at": now,
                    "pane_id": "%1",
                    "window": "worker_a"
                }
            },
            "tasks": [
                {
                    "id": "task_initial",
                    "title": "Initial task",
                    "type": "implementation",
                    "assignee": "worker_a",
                    "status": "running"
                }
            ],
            "teams": {
                "command-harness-template": {
                    "status": "running",
                    "spec_path": spec_path.to_string_lossy().to_string(),
                    "team_dir": teamdir.to_string_lossy().to_string(),
                    "session_name": "team-command-harness-template",
                    "leader": {"id": "leader"},
                    "agents": {
                        "worker_a": {
                            "provider": "fake",
                            "role": "Worker A",
                            "status": "running",
                            "session_id": "sess-worker-a",
                            "first_send_at": now,
                            "pane_id": "%1",
                            "window": "worker_a"
                        }
                    },
                    "tasks": [
                        {
                            "id": "task_initial",
                            "title": "Initial task",
                            "type": "implementation",
                            "assignee": "worker_a",
                            "status": "running"
                        }
                    ]
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&root);
    C1Fixture { root, teamdir }
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path())).unwrap();
    let _ = team_agent::message_store::MessageStore::open(workspace.as_path()).unwrap();
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .unwrap();
}

#[derive(Debug, Default)]
struct SessionRecordingTransport {
    sessions: Mutex<HashSet<String>>,
    spawned: Mutex<Vec<String>>,
}

impl SessionRecordingTransport {
    fn spawned_sessions(&self) -> Vec<String> {
        self.spawned.lock().unwrap().clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        kind: &'static str,
    ) -> SpawnResult {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.as_str().to_string());
        let mut spawned = self.spawned.lock().unwrap();
        spawned.push(session.as_str().to_string());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}-{kind}", spawned.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for SessionRecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, "first"))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, "into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.sessions.lock().unwrap().contains(session.as_str()))
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
