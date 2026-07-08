//! 0.5.16 RED contract: result attribution must follow the physical submit boundary.
//!
//! References:
//! - `.team/artifacts/result-attribution-race-locate.md` §9 RED Contract.
//! - Locate §7.3 SubmitObserver / split-phase is an implementation suggestion only;
//!   these tests assert externally visible attribution behavior, not API shape.
//!
//! User story: when a worker receives a direct leader message and reports immediately
//! without a task_id, the result belongs to that message, not to stale startup task
//! state. A message row that only reached target_resolved, with no physical submit,
//! is not enough to claim that worker's next no-task report.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::event_log::EventLog;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::message_store::MessageStore;
use team_agent::messaging::deliver_pending_message;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};

const TEAM: &str = "race-team";
const WORKER: &str = "probe-worker";
const TASK_INITIAL: &str = "task_initial";
const MSG_RACE: &str = "msg_race";
const MSG_TARGET_ONLY: &str = "msg_target_only";

#[test]
fn physical_submit_window_report_result_without_task_id_belongs_to_direct_message() {
    let case = RaceCase::new("physical-submit-window");
    case.seed_state_with_task_initial();
    let store = MessageStore::open(case.path()).unwrap();
    store
        .create_message_with_id(
            MSG_RACE,
            None,
            "leader",
            WORKER,
            "report immediately without task_id",
            None,
            false,
            Some(TEAM),
        )
        .unwrap();

    let summary = "RACE_PHYSICAL_SUBMIT_REPORT";
    let runner = SubmitWindowRunner::new(case.path().to_path_buf(), summary);
    let report_called = runner.report_called();
    let backend = TmuxBackend::with_runner(Box::new(runner));
    let state = load_runtime_state(case.path()).unwrap();

    let out = deliver_pending_message(
        case.path(),
        &store,
        &backend,
        MSG_RACE,
        &EventLog::new(case.path()),
        &state,
    )
    .unwrap();
    assert!(
        out.ok,
        "RED setup: the controlled tmux runner physically submits and verifies {MSG_RACE}; got {out:?}"
    );
    assert!(
        report_called.load(Ordering::SeqCst),
        "RED setup: worker report_result must run inside the post-submit/pre-return verification window"
    );

    let rows = result_rows(case.path(), summary);
    assert_eq!(
        rows.len(),
        1,
        "RED setup: exactly one worker result should be persisted for {summary}; rows={rows:?}"
    );
    assert_eq!(
        rows[0].task_id, MSG_RACE,
        "0.5.16 RED: no-task report_result emitted after physical submit but before delivery verification returns must attribute to the direct message id, not stale {TASK_INITIAL}; rows={rows:?}"
    );
    assert!(
        result_rows_for_task(case.path(), TASK_INITIAL, summary).is_empty(),
        "0.5.16 RED: stale startup task {TASK_INITIAL} must not receive the race-window result; rows={:?}",
        result_rows_for_task(case.path(), TASK_INITIAL, summary)
    );

    let events = read_events(case.path());
    let armed = first_event_index(&events, "turn_open.armed_after_inject").unwrap_or_else(|| {
        panic!("0.5.16 RED: current_turn must be recorded at physical submit; events={events}")
    });
    let report = first_event_index(&events, "mcp.report_result")
        .unwrap_or_else(|| panic!("RED setup: report_result event missing; events={events}"));
    let delivered = first_event_index(&events, "message.delivered")
        .unwrap_or_else(|| panic!("RED setup: message.delivered event missing; events={events}"));
    assert!(
        armed < report && report < delivered,
        "0.5.16 RED: physical-submit current_turn must be armed before a worker report emitted during post-submit verification, and before message.delivered; indexes armed={armed} report={report} delivered={delivered}; events={events}"
    );
}

#[test]
fn target_resolved_without_physical_submit_does_not_steal_no_task_report() {
    let case = RaceCase::new("target-resolved-only");
    case.seed_state_without_tasks();
    let store = MessageStore::open(case.path()).unwrap();
    store
        .create_message_with_id(
            MSG_TARGET_ONLY,
            None,
            "leader",
            WORKER,
            "this row is claimed but never physically submitted",
            None,
            false,
            Some(TEAM),
        )
        .unwrap();
    assert!(
        store.claim_for_delivery(MSG_TARGET_ONLY).unwrap(),
        "RED setup: target_resolved claim should succeed"
    );

    let summary = "RACE_TARGET_RESOLVED_ONLY_REPORT";
    let tools = TeamOrchestratorTools::with_identity(
        case.path(),
        Some(AgentId::new(WORKER)),
        Some(TeamKey::new(TEAM)),
    );
    let out = tools
        .report_result(
            None,
            Some(summary),
            ResultStatus::Success,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let out = Value::Object(out.fields);
    assert_ne!(
        out.get("task_id").and_then(Value::as_str),
        Some(MSG_TARGET_ONLY),
        "0.5.16 RED: target_resolved is only a delivery claim, not physical submit proof; no-task report_result must not attribute to {MSG_TARGET_ONLY}; out={out}"
    );
    assert_eq!(
        out.get("task_id").and_then(Value::as_str),
        Some("manual"),
        "0.5.16 RED: with no current physical turn and no assigned task, fallback is manual; out={out}"
    );
    assert!(
        result_rows_for_task(case.path(), MSG_TARGET_ONLY, summary).is_empty(),
        "0.5.16 RED: target_resolved-only message must have no result row; rows={:?}",
        result_rows_for_task(case.path(), MSG_TARGET_ONLY, summary)
    );
}

struct RaceCase {
    path: PathBuf,
}

impl RaceCase {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = root.join(format!(
            "ta-result-attribution-race-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self {
            path: std::fs::canonicalize(path).unwrap(),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn seed_state_with_task_initial(&self) {
        self.save_state(json!([{
            "id": TASK_INITIAL,
            "assignee": WORKER,
            "status": "pending"
        }]));
    }

    fn seed_state_without_tasks(&self) {
        self.save_state(json!([]));
    }

    fn save_state(&self, tasks: Value) {
        let team_state = json!({
            "team_key": TEAM,
            "session_name": "team-result-attribution-race",
            "agents": {
                WORKER: {
                    "status": "running",
                    "provider": "fake",
                    "window": WORKER,
                    "pane_id": "%7"
                }
            },
            "tasks": tasks,
            "coordinator": {}
        });
        let state = json!({
            "active_team_key": TEAM,
            "session_name": "team-result-attribution-race",
            "agents": {
                WORKER: {
                    "status": "running",
                    "provider": "fake",
                    "window": WORKER,
                    "pane_id": "%7"
                }
            },
            "tasks": team_state.get("tasks").cloned().unwrap(),
            "teams": {
                TEAM: team_state
            }
        });
        save_runtime_state(&self.path, &state).unwrap();
    }
}

impl Drop for RaceCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

struct SubmitWindowRunner {
    workspace: PathBuf,
    summary: &'static str,
    report_called: Arc<AtomicBool>,
    payload: Mutex<Option<String>>,
    enter_sent: AtomicBool,
    captures: Mutex<VecDeque<String>>,
}

impl SubmitWindowRunner {
    fn new(workspace: PathBuf, summary: &'static str) -> Self {
        Self {
            workspace,
            summary,
            report_called: Arc::new(AtomicBool::new(false)),
            payload: Mutex::new(None),
            enter_sent: AtomicBool::new(false),
            captures: Mutex::new(VecDeque::new()),
        }
    }

    fn report_called(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.report_called)
    }

    fn run_report_result_once(&self) {
        if self.report_called.swap(true, Ordering::SeqCst) {
            return;
        }
        let tools = TeamOrchestratorTools::with_identity(
            &self.workspace,
            Some(AgentId::new(WORKER)),
            Some(TeamKey::new(TEAM)),
        );
        tools
            .report_result(
                None,
                Some(self.summary),
                ResultStatus::Success,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }

    fn capture_text(&self) -> String {
        if let Some(text) = self.captures.lock().unwrap().pop_front() {
            return text;
        }
        if self.enter_sent.load(Ordering::SeqCst) {
            self.run_report_result_once();
            String::new()
        } else {
            self.payload.lock().unwrap().clone().unwrap_or_default()
        }
    }
}

impl CommandRunner for SubmitWindowRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        let stdout = if argv_contains(argv, "capture-pane") {
            self.capture_text()
        } else {
            if argv_contains(argv, "send-keys") && argv.iter().any(|arg| arg == "Enter") {
                self.enter_sent.store(true, Ordering::SeqCst);
            }
            String::new()
        };
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        if argv_contains(argv, "load-buffer") {
            *self.payload.lock().unwrap() = Some(stdin.to_string());
        }
        self.run(argv)
    }
}

#[derive(Debug)]
struct ResultRow {
    task_id: String,
}

fn result_rows(workspace: &Path, summary: &str) -> Vec<ResultRow> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let mut stmt = conn
        .prepare(
            "select task_id, envelope from results
             where agent_id = ?1
             order by created_at asc, result_id asc",
        )
        .unwrap();
    stmt.query_map(params![WORKER], |row| {
        let task_id: String = row.get(0)?;
        let envelope_text: String = row.get(1)?;
        Ok((task_id, envelope_text))
    })
    .unwrap()
    .filter_map(Result::ok)
    .filter_map(|(task_id, envelope_text)| {
        let envelope: Value = serde_json::from_str(&envelope_text).ok()?;
        (envelope.get("summary").and_then(Value::as_str) == Some(summary))
            .then_some(ResultRow { task_id })
    })
    .collect()
}

fn result_rows_for_task(workspace: &Path, task_id: &str, summary: &str) -> Vec<ResultRow> {
    result_rows(workspace, summary)
        .into_iter()
        .filter(|row| row.task_id == task_id)
        .collect()
}

fn read_events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn first_event_index(events: &str, event: &str) -> Option<usize> {
    events.lines().enumerate().find_map(|(idx, line)| {
        let parsed: Value = serde_json::from_str(line).ok()?;
        (parsed.get("event").and_then(Value::as_str) == Some(event)).then_some(idx)
    })
}

fn argv_contains(argv: &[String], needle: &str) -> bool {
    argv.iter().any(|arg| arg == needle)
}
