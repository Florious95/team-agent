//! SUB prep Codex trust + roundtrip contracts.
//!
//! Anchors:
//! - `.team/artifacts/rs-subprep-codex-trust-roundtrip-root-cause.md`
//! - `.team/artifacts/macmini-e2e/subprep-codex-probe5-20260605T071044Z/`
//!
//! Real Codex subscription behavior still needs rt smoke for the provider pane/report_result parts;
//! these cargo contracts lock the local recognizer, delivery state, command-shape, and rollout
//! capture surfaces that caused the probe failures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[path = "support/temp_workspace.rs"]
mod temp_workspace;

use rusqlite::params;
use serde_json::json;
use temp_workspace::TempWorkspace;
use team_agent::event_log::EventLog;
use team_agent::lifecycle::launch_with_transport;
use team_agent::message_store::MessageStore;
use team_agent::messaging::delivery::deliver_pending_message;
use team_agent::messaging::trust::attempt_trust_auto_answer;
use team_agent::messaging::PaneWidthQuery;
use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{
    classify, get_adapter, ProcessLiveness, TurnState,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const PROBE5_TRUST_PANE: &str = r#"> You are in /private/tmp/ta-subprep-SUB-PROBE-1-trust-startup.Ey2Xk6/teamdir

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue
"#;

const PROBE5_READY_AFTER_TRUST_CONSUMED: &str = r#"> You are in /private/tmp/ta-subprep-SUB-PROBE-2-send-turn-idle.uxarHT/teamdir

╭─────────────────────────────────────────────╮
│ >_ OpenAI Codex (v0.136.0)                  │
│                                             │
│ model:     gpt-5.5 xhigh   /model to change │
│ directory: /private/tmp/…/teamdir           │
╰─────────────────────────────────────────────╯

› Run /review on my current changes
"#;

const SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART: &str = r#"> You are in /private/tmp/ta-subroot-SUB-PROBE-1-trust-startup.uL9Boc/teamdir

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue

╭─────────────────────────────────────────────────╮
│ ✨ Update available! 0.136.0 -> 0.137.0         │
│ Run npm install -g @openai/codex to update.     │
│                                                 │
│ See full release notes:                         │
│ https://github.com/openai/codex/releases/latest │
╰─────────────────────────────────────────────────╯

╭───────────────────────────────────────╮
│ >_ OpenAI Codex (v0.136.0)            │
│                                       │
│ model:     loading   /model to change │
│ directory: /private/tmp/…/teamdir     │
╰───────────────────────────────────────╯


› Find and fix a bug in @filename

  gpt-5.5 default · /private/tmp/ta-subroot-SUB-PROBE-1-trust-startup.uL9Boc/te…





"#;

fn tmp_dir(tag: &str) -> TempWorkspace {
    TempWorkspace::new("ta-rs-subprep", tag)
}

#[test]
fn temp_workspace_guard_cleans_after_panic_unwind() {
    if TempWorkspace::keep_enabled() {
        return;
    }

    let mut created = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ws = tmp_dir("guard-panic");
        std::fs::write(ws.join("marker"), "created").unwrap();
        created = Some(ws.to_path_buf());
        panic!("intentional panic to prove TempWorkspace Drop cleanup");
    }));

    assert!(
        result.is_err(),
        "fixture sanity: panic branch must actually unwind for cleanup verification"
    );
    let created = created.expect("fixture sanity: panic branch should record temp path");
    assert!(
        !created.exists(),
        "TempWorkspace must remove its root during panic unwinding unless TEAM_AGENT_KEEP_TEST_TMP=1; leaked={}",
        created.display()
    );
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn truncated_workspace_token(canonical_teamdir: &Path) -> String {
    let tail = canonical_teamdir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("teamdir");
    let head = canonical_teamdir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| canonical_teamdir.parent().unwrap_or(canonical_teamdir));
    let mut head = head.to_string_lossy().to_string();
    if !head.ends_with(std::path::MAIN_SEPARATOR) {
        head.push(std::path::MAIN_SEPARATOR);
    }
    format!("{head}…/{tail}")
}

fn foreign_workspace_path(canonical_teamdir: &Path) -> PathBuf {
    canonical_teamdir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| canonical_teamdir.parent().unwrap_or(canonical_teamdir))
        .join("foreign-team")
        .join(canonical_teamdir.file_name().unwrap_or_else(|| std::ffi::OsStr::new("teamdir")))
}

#[test]
fn contract_a_subroot_full_pane_trust_menu_still_actionable_despite_bottom_ready_prompt() {
    use team_agent::provider::{classify_codex_startup_screen, StartupScreenDecision};

    let decision = classify_codex_startup_screen(SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART);

    assert_eq!(
        decision,
        StartupScreenDecision::AnswerWorkspaceTrust,
        "Contract A real-machine fixture: the full Codex pane keeps an actionable trust menu even when an update box, Codex banner, and bottom `› Find...` input prompt are visible in the same capture"
    );
}

#[test]
fn contract_a_subroot_tick_answers_full_pane_trust_and_persists_startup_handled() {
    use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
    use team_agent::provider::ProviderAdapter;

    struct RealAdapterRegistry;

    impl ProviderRegistry for RealAdapterRegistry {
        fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
            team_agent::provider::get_adapter(provider)
        }

        fn error_lists(&self, _provider: Provider) -> ErrorLists {
            ErrorLists::default()
        }
    }

    let ws = tmp_dir("a-subroot-tick-full-pane");
    let mcp_config = ws.join(".team").join("runtime").join("mcp").join("w1.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-ta-subroot-SUB-PROBE-1-trust-startup",
            "team_dir": ws.join("teamdir").to_string_lossy().to_string(),
            "agents": {
                "w1": {
                    "provider": "codex",
                    "status": "running",
                    "startup_prompts": "pending",
                    "startup_prompt_status": "pending",
                    "window": "w1",
                    "pane_id": "%7",
                    "mcp_config": mcp_config.to_string_lossy().to_string()
                }
            }
        }),
    )
    .unwrap();

    let transport = RecordingTransport::new(vec![
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
    ])
    .with_session_present(true)
    .with_windows(vec![WindowName::new("w1")]);
    let coord = Coordinator::new(
        WorkspacePath::new(ws.to_path_buf()),
        Box::new(RealAdapterRegistry),
        Box::new(transport.clone()),
    );

    let report = coord.tick().expect("coordinator tick should stay typed/fallible");
    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    let agent = &state["agents"]["w1"];
    let events = read_events(&ws);

    assert!(
        report.ok && !report.stop,
        "Contract A driver: startup trust handling must not stop or degrade the tick; report={report:?}"
    );
    assert_eq!(
        transport.send_keys_count(),
        1,
        "Contract A driver: full-pane own-workspace trust prompt must send exactly one Enter"
    );
    assert_eq!(
        agent["startup_prompts"],
        json!("handled"),
        "Contract A driver: tick must persist startup_prompts=handled after answering trust; state={state}"
    );
    assert_eq!(
        agent["startup_prompt_handled"][0]["prompt"],
        json!("codex_workspace_trust"),
        "Contract A driver: handled prompt must identify codex workspace trust; state={state}"
    );
    assert_eq!(
        agent["startup_prompt_handled"][0]["action"],
        json!("sent_enter"),
        "Contract A driver: handled prompt action must be sent_enter; state={state}"
    );
    assert!(
        events.contains("leader_panes.trust_auto_answered")
            || events.contains("startup_prompt_handled"),
        "Contract A driver: trust answer must be visible in events as leader_panes.trust_auto_answered or startup_prompt_handled; events={events}"
    );
}

#[test]
fn contract_a_step1_trust_auto_answer_is_idempotent_across_repeated_ticks() {
    use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
    use team_agent::provider::ProviderAdapter;

    struct RealAdapterRegistry;

    impl ProviderRegistry for RealAdapterRegistry {
        fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
            team_agent::provider::get_adapter(provider)
        }

        fn error_lists(&self, _provider: Provider) -> ErrorLists {
            ErrorLists::default()
        }
    }

    let ws = tmp_dir("a-subroot-tick-idempotent");
    let mcp_config = ws.join(".team").join("runtime").join("mcp").join("w1.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-ta-subroot-SUB-PROBE-1-trust-startup",
            "team_dir": ws.join("teamdir").to_string_lossy().to_string(),
            "agents": {
                "w1": {
                    "provider": "codex",
                    "status": "running",
                    "startup_prompts": "pending",
                    "startup_prompt_status": "pending",
                    "window": "w1",
                    "pane_id": "%7",
                    "mcp_config": mcp_config.to_string_lossy().to_string()
                }
            }
        }),
    )
    .unwrap();

    let transport = RecordingTransport::new(vec![
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
    ])
    .with_session_present(true)
    .with_windows(vec![WindowName::new("w1")]);
    let coord = Coordinator::new(
        WorkspacePath::new(ws.to_path_buf()),
        Box::new(RealAdapterRegistry),
        Box::new(transport.clone()),
    );

    coord.tick().expect("first tick should answer the startup trust prompt");
    coord
        .tick()
        .expect("second tick over the same pane must not answer trust again");

    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    let agent = &state["agents"]["w1"];
    let events = read_events(&ws);
    let handled_event_count = count_event(&events, "startup_prompt_handled");
    let sent_enter_count = count_handled_action(&events, "sent_enter");

    assert_eq!(
        transport.send_keys_count(),
        1,
        "Contract A step1/idempotency: the same actionable trust prompt across repeated coordinator ticks must receive Enter exactly once"
    );
    assert_eq!(
        handled_event_count, 1,
        "Contract A step1/idempotency: startup_prompt_handled must be emitted exactly once after trust is answered; events={events}"
    );
    assert_eq!(
        sent_enter_count, 1,
        "Contract A step1/idempotency: sent_enter handling for codex_workspace_trust must be recorded exactly once; events={events}"
    );
    assert_eq!(
        agent["startup_prompts"],
        json!("handled"),
        "Contract A step1/idempotency: answering trust must persist startup_prompts=handled/complete so later ticks skip re-answering; state={state}"
    );
    assert_eq!(
        agent["startup_prompt_handled"].as_array().map(Vec::len),
        Some(1),
        "Contract A step1/idempotency: state must retain one handled prompt record, not append duplicates; state={state}"
    );
}

#[test]
fn contract_a_own_workspace_trust_is_actionable_not_ready_and_foreign_is_pending() {
    use team_agent::provider::{classify_codex_startup_screen, StartupScreenDecision};

    assert_eq!(
        classify_codex_startup_screen(PROBE5_TRUST_PANE),
        StartupScreenDecision::AnswerWorkspaceTrust,
        "Contract A/N15: real Codex trust menu with `› 1. Yes, continue` is an actionable trust prompt, not Ready from a bare single-glyph marker"
    );

    let root = tmp_dir("a-truncated-root");
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(&teamdir).unwrap();
    let canonical_teamdir = std::fs::canonicalize(&teamdir).unwrap();
    let own_tail = format!(
        "Do you trust the contents of this directory?\n  directory: {}",
        truncated_workspace_token(&canonical_teamdir)
    );
    let own_transport = RecordingTransport::new(vec![own_tail.to_string()]);
    let own_log = EventLog::new(&root);
    let own = attempt_trust_auto_answer(
        &canonical_teamdir,
        &own_transport,
        Some(&PaneId::new("%7")),
        &own_tail,
        &PaneWidthQuery::Ok { pane_width: 80 },
        &own_log,
    )
    .unwrap();
    assert!(
        own.answered,
        "Contract A/#167: own-workspace matcher must accept platform-local Codex truncated directory display via the shared realpath/truncation matcher; canonical_teamdir={} tail={own_tail:?} got {own:?}",
        canonical_teamdir.display()
    );
    assert_eq!(
        own_transport.inject_count(),
        1,
        "Contract A: own trust answer is exactly one local Enter/inject action"
    );

    let foreign_transport = RecordingTransport::new(vec![PROBE5_TRUST_PANE.to_string()]);
    let foreign = attempt_trust_auto_answer(
        &canonical_teamdir,
        &foreign_transport,
        Some(&PaneId::new("%8")),
        &format!(
            "Do you trust the contents of this directory?\n> You are in {}",
            foreign_workspace_path(&canonical_teamdir).display()
        ),
        &PaneWidthQuery::Ok { pane_width: 120 },
        &own_log,
    )
    .unwrap();
    assert!(
        !foreign.answered && foreign.reason == "workspace_dir_mismatch",
        "Contract A: foreign workspace trust prompt stays pending and is never auto-answered; got {foreign:?}"
    );
    assert_eq!(
        foreign_transport.inject_count(),
        0,
        "Contract A: foreign trust prompt must not receive Enter"
    );
}

#[test]
fn contract_a_probe5_quick_start_ready_is_invalid_while_actionable_trust_remains() {
    use team_agent::cli::{cmd_wait_ready, CmdOutput, WaitReadyArgs};
    use team_agent::provider::{classify_codex_startup_screen, StartupScreenDecision};

    assert_eq!(
        classify_codex_startup_screen(PROBE5_TRUST_PANE),
        StartupScreenDecision::AnswerWorkspaceTrust,
        "fixture sanity: probe5 pane text is an actionable own-workspace trust prompt"
    );

    let ws = tmp_dir("a-wait-ready-trust");
    let mcp_config = ws.join(".team").join("runtime").join("mcp").join("w1.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-subprep",
            "agents": {
                "w1": {
                    "provider": "codex",
                    "status": "awaiting_trust_prompt",
                    "startup_prompt_status": "awaiting_trust_prompt",
                    "startup_prompts": "awaiting_trust_prompt",
                    "pane_id": "%7",
                    "window": "w1",
                    "mcp_config": mcp_config.to_string_lossy().to_string(),
                    "task_prompt_delivered": true
                }
            }
        }),
    )
    .unwrap();

    let result = cmd_wait_ready(&WaitReadyArgs {
        workspace: ws.to_path_buf(),
        timeout: 0.0,
        json: true,
    })
    .expect("wait-ready SUT should return shaped JSON");
    let value = match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("wait-ready --json must return JSON output, got {other:?}"),
    };

    assert_eq!(
        value["ok"],
        json!(false),
        "Contract A: quick-start/wait-ready readiness must not report ok=true while actionable trust remains; got {value}"
    );
    assert_eq!(
        value["status"],
        json!("pending"),
        "Contract A: actionable trust prompt should keep readiness pending, not ready; got {value}"
    );
    assert_eq!(
        value["reason"],
        json!("awaiting_trust_prompt"),
        "Contract A: not-ready reason must identify the startup trust boundary; got {value}"
    );
    assert_eq!(
        value["readiness"]["awaiting_trust_prompt"],
        json!(true),
        "Contract A: ready logic must surface awaiting_trust_prompt=true; got {value}"
    );
    assert_eq!(
        value["readiness"]["ready"],
        json!(false),
        "Contract A: ready must stay false until the actionable trust prompt is cleared; got {value}"
    );
}

#[test]
fn contract_b_delivery_does_not_mark_delivered_or_inject_task_while_trust_prompt_is_present() {
    let ws = tmp_dir("b-delivery-trust");
    let store = MessageStore::open(&ws).unwrap();
    let message_id = store
        .create_message_with_id(
            "msg_subprep_trust",
            None,
            "leader",
            "w1",
            "SUB prep probe: reply with one sentence and call report_result exactly once.",
            None,
            true,
            None,
        )
        .unwrap();
    let state = json!({
        "session_name": "team-subprep",
        "agents": {
            "w1": {
                "provider": "codex",
                "status": "running",
                "pane_id": "%7",
                "window": "w1"
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&ws, &state).unwrap();
    let transport = RecordingTransport::new(vec![PROBE5_TRUST_PANE.to_string()]);
    let event_log = EventLog::new(&ws);

    let outcome = deliver_pending_message(&ws, &store, &transport, &message_id, &event_log, &state)
        .unwrap();
    let status = message_status(store.db_path(), &message_id);
    let events = read_events(&ws);

    assert!(
        !matches!(outcome.status, team_agent::messaging::DeliveryStatus::Delivered),
        "Contract B/MUST-10: physical injection into a trust menu is not provider delivery; outcome={outcome:?}"
    );
    assert!(
        matches!(status.as_str(), "accepted" | "pending" | "queued_until_trust"),
        "Contract B: same message_id must remain retryable/pending until trust clears, not delivered; db_status={status}"
    );
    assert_eq!(
        transport.inject_count(),
        0,
        "Contract B: task text must not be injected into the trust menu; delivery should answer/queue trust first"
    );
    assert!(
        !events.contains("\"message.delivered\"") && !events.contains("turn_open.armed_after_delivery"),
        "Contract B: no message.delivered or turn-open arm before provider main input is ready; events={events}"
    );
}

#[test]
fn contract_b_probe5_delivered_without_result_is_rejected_as_lost_task_evidence() {
    let ws = tmp_dir("b-probe5-delivery-sut");
    let store = MessageStore::open(&ws).unwrap();
    let message_id = store
        .create_message_with_id(
            "msg_97eaf80c4958",
            None,
            "leader",
            "w1",
            "SUB prep probe: reply with one sentence and call report_result exactly once.",
            None,
            true,
            None,
        )
        .unwrap();
    let state = json!({
        "session_name": "team-ta-subprep-SUB-PROBE-2-send-turn-idle",
        "agents": {
            "w1": {
                "provider": "codex",
                "status": "running",
                "pane_id": "%7",
                "window": "w1"
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&ws, &state).unwrap();
    let transport = RecordingTransport::new(vec![PROBE5_TRUST_PANE.to_string()]);
    let event_log = EventLog::new(&ws);

    let outcome = deliver_pending_message(&ws, &store, &transport, &message_id, &event_log, &state)
        .unwrap();
    let status = message_status(store.db_path(), &message_id);
    let events = read_events(&ws);

    assert_eq!(
        outcome.message_id.as_deref(),
        Some("msg_97eaf80c4958"),
        "Contract B: delivery trust gate must preserve the same probe5 message_id for retry; got {outcome:?}"
    );
    assert_eq!(
        status,
        "queued_until_trust",
        "Contract B: worker-at-trust-menu must keep the message retryable, not mark delivered; db_status={status}"
    );
    assert_eq!(
        transport.inject_count(),
        0,
        "Contract B: task protocol block must not be injected into the trust menu"
    );
    assert!(
        !events.contains("\"message.delivered\"") && !events.contains("turn_open.armed_after_delivery"),
        "Contract B: no false delivered event or turn-open arm before trust clears; events={events}"
    );
}

#[test]
fn contract_b_delivery_gate_trusts_startup_handled_state_over_pane_scrollback() {
    let pending_ws = tmp_dir("b-gate-pending-scrollback");
    let pending_store = MessageStore::open(&pending_ws).unwrap();
    let pending_message_id = pending_store
        .create_message_with_id(
            "msg_scrollback_pending",
            None,
            "leader",
            "w1",
            "SUB step2 probe: pending startup trust should defer this task.",
            None,
            true,
            None,
        )
        .unwrap();
    let pending_state = json!({
        "session_name": "team-ta-subroot-SUB-PROBE-2-send-turn-idle",
        "agents": {
            "w1": {
                "provider": "codex",
                "status": "running",
                "startup_prompts": "pending",
                "startup_prompt_status": "pending",
                "pane_id": "%7",
                "window": "w1"
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&pending_ws, &pending_state).unwrap();
    let pending_transport =
        RecordingTransport::new(vec![SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string()]);
    let pending_log = EventLog::new(&pending_ws);
    let pending = deliver_pending_message(
        &pending_ws,
        &pending_store,
        &pending_transport,
        &pending_message_id,
        &pending_log,
        &pending_state,
    )
    .unwrap();

    assert!(
        !matches!(pending.status, team_agent::messaging::DeliveryStatus::Delivered),
        "Contract B reverse: pending startup_prompts with actionable trust text must still defer; outcome={pending:?}"
    );
    assert_eq!(
        message_status(pending_store.db_path(), &pending_message_id),
        "queued_until_trust",
        "Contract B reverse: pending trust must leave message queued_until_trust"
    );
    assert_eq!(
        pending_transport.inject_count(),
        0,
        "Contract B reverse: pending trust must not inject task text into the trust menu"
    );
    assert!(
        read_events(&pending_ws).contains("delivery.deferred_startup_prompt"),
        "Contract B reverse: pending trust defer must remain observable"
    );

    let handled_ws = tmp_dir("b-gate-handled-scrollback");
    let handled_store = MessageStore::open(&handled_ws).unwrap();
    let handled_message_id = handled_store
        .create_message_with_id(
            "msg_scrollback_handled",
            None,
            "leader",
            "w1",
            "SUB step2 probe: handled startup trust must deliver despite scrollback residue.",
            None,
            true,
            None,
        )
        .unwrap();
    let handled_state = json!({
        "session_name": "team-ta-subroot-SUB-PROBE-2-send-turn-idle",
        "agents": {
            "w1": {
                "provider": "codex",
                "status": "running",
                "startup_prompts": "handled",
                "startup_prompt_status": "handled",
                "startup_prompt_handled": [
                    {"prompt": "codex_workspace_trust", "action": "sent_enter"}
                ],
                "pane_id": "%7",
                "window": "w1"
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&handled_ws, &handled_state).unwrap();
    let handled_transport =
        RecordingTransport::new(vec![SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string()]);
    let handled_log = EventLog::new(&handled_ws);
    let handled = deliver_pending_message(
        &handled_ws,
        &handled_store,
        &handled_transport,
        &handled_message_id,
        &handled_log,
        &handled_state,
    )
    .unwrap();

    assert!(
        matches!(handled.status, team_agent::messaging::DeliveryStatus::Delivered),
        "Contract B step2/scrollback: startup_prompts=handled must trust state and deliver, even if full pane scrollback still contains `Do you trust` and `› 1. Yes, continue`; outcome={handled:?}"
    );
    assert_eq!(
        message_status(handled_store.db_path(), &handled_message_id),
        "delivered",
        "Contract B step2/scrollback: handled startup prompt state must prevent permanent queued_until_trust; outcome={handled:?}"
    );
    assert_eq!(
        handled_transport.inject_count(),
        1,
        "Contract B step2/scrollback: handled state must allow exactly one physical task inject"
    );
    assert!(
        read_events(&handled_ws).contains("\"message.delivered\""),
        "Contract B step2/scrollback: successful handled-state delivery must emit message.delivered"
    );
}

#[test]
fn contract_b_step2_retry_delivers_queued_message_after_startup_trust_is_handled() {
    use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
    use team_agent::provider::ProviderAdapter;

    struct RealAdapterRegistry;

    impl ProviderRegistry for RealAdapterRegistry {
        fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
            team_agent::provider::get_adapter(provider)
        }

        fn error_lists(&self, _provider: Provider) -> ErrorLists {
            ErrorLists::default()
        }
    }

    let ws = tmp_dir("b-step2-trust-retry");
    let mcp_config = ws.join(".team").join("runtime").join("mcp").join("w1.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    let state = json!({
        "session_name": "team-ta-subroot-SUB-PROBE-2-send-turn-idle",
        "team_dir": ws.join("teamdir").to_string_lossy().to_string(),
        "agents": {
            "w1": {
                "provider": "codex",
                "status": "running",
                "startup_prompts": "pending",
                "startup_prompt_status": "pending",
                "pane_id": "%7",
                "window": "w1",
                "mcp_config": mcp_config.to_string_lossy().to_string()
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&ws, &state).unwrap();

    let store = MessageStore::open(&ws).unwrap();
    let message_id = store
        .create_message_with_id(
            "msg_subroot_step2_retry",
            None,
            "leader",
            "w1",
            "SUB step2 probe: reply with one sentence and call report_result exactly once.",
            None,
            true,
            None,
        )
        .unwrap();
    let transport = RecordingTransport::new(vec![
        PROBE5_TRUST_PANE.to_string(),
        SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART.to_string(),
        PROBE5_READY_AFTER_TRUST_CONSUMED.to_string(),
        PROBE5_READY_AFTER_TRUST_CONSUMED.to_string(),
    ])
    .with_session_present(true)
    .with_windows(vec![WindowName::new("w1")]);
    let event_log = EventLog::new(&ws);

    let deferred = deliver_pending_message(&ws, &store, &transport, &message_id, &event_log, &state)
        .unwrap();
    assert!(
        !matches!(deferred.status, team_agent::messaging::DeliveryStatus::Delivered),
        "fixture sanity: the first delivery attempt must defer while the trust prompt is actionable; outcome={deferred:?}"
    );
    assert_eq!(
        message_status(store.db_path(), &message_id),
        "queued_until_trust",
        "fixture sanity: send while trust is present must leave the message queued_until_trust"
    );
    assert_eq!(
        message_delivery_attempts(store.db_path(), &message_id),
        1,
        "fixture sanity: the deferred send consumed exactly one delivery attempt"
    );
    assert_eq!(
        transport.inject_count(),
        0,
        "fixture sanity: task content must not be injected into the startup trust menu"
    );
    assert!(
        read_events(&ws).contains("delivery.deferred_startup_prompt"),
        "fixture sanity: delivery defer must be observable as delivery.deferred_startup_prompt"
    );

    let coord = Coordinator::new(
        WorkspacePath::new(ws.to_path_buf()),
        Box::new(RealAdapterRegistry),
        Box::new(transport.clone()),
    );
    coord
        .tick()
        .expect("coordinator tick should answer trust and retry queued delivery");

    let events = read_events(&ws);
    assert_eq!(
        count_event(&events, "startup_prompt_handled"),
        1,
        "Contract B step2 fixture: coordinator tick must handle the startup trust prompt once before retrying delivery; events={events}"
    );
    assert_eq!(
        message_status(store.db_path(), &message_id),
        "delivered",
        "Contract B step2/retry: once startup trust is handled, the same queued_until_trust message_id must be redelivered instead of remaining stuck; events={events}"
    );
    assert!(
        message_delivery_attempts(store.db_path(), &message_id) > 1,
        "Contract B step2/retry: retry after startup_prompt_handled must increase delivery_attempts; attempts={} events={events}",
        message_delivery_attempts(store.db_path(), &message_id)
    );
    assert_eq!(
        transport.inject_count(),
        1,
        "Contract B step2/retry: retry must physically inject the original task into the worker pane exactly once after trust clears"
    );
    assert!(
        events.contains("\"message.delivered\""),
        "Contract B step2/retry: retry must emit message.delivered for the original message_id; events={events}"
    );
}

#[test]
fn contract_c_lifecycle_launch_command_carries_role_tools_and_real_mcp_context() {
    let root = tmp_dir("c-launch-context");
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(teamdir.join("agents")).unwrap();
    std::fs::write(
        teamdir.join("TEAM.md"),
        "---\nname: subprep\nobjective: Verify worker can report results.\nprovider: codex\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        teamdir.join("agents").join("w1.md"),
        "---\nname: w1\nrole: Worker must call report_result exactly once.\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAlways use report_result for completion.\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&teamdir).unwrap();
    let spec_path = teamdir.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).unwrap();

    let transport = RecordingTransport::new(vec![PROBE5_READY_AFTER_TRUST_CONSUMED.to_string()]);
    let report = launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch should reach transport spawn");
    assert_eq!(report.started.len(), 1, "fixture sanity: one Codex worker starts");
    let argv = transport.spawn_argv().join(" ");

    assert!(
        argv.contains("report_result") || argv.contains("Worker must call report_result"),
        "Contract C/F6.4: lifecycle must pass resolved role/developer instructions into Codex command construction; argv={argv:?}"
    );
    assert!(
        argv.contains("mcp") || argv.contains("team_orchestrator") || argv.contains("team-agent"),
        "Contract C/MUST-8: Codex worker command must include a real Team Agent tool/MCP capability mechanism, not prompt-only state metadata; argv={argv:?}"
    );
    assert!(
        !argv.contains("/tmp/ta-provider-ws") && !argv.contains("worker1") && !argv.contains("teamA"),
        "Contract C: provider command/config must not leak placeholder MCP context; argv={argv:?}"
    );
}

#[test]
fn contract_c_lifecycle_sources_use_context_aware_provider_builders() {
    let launch = std::fs::read_to_string(manifest_dir().join("src/lifecycle/launch.rs")).unwrap();
    let restart_common =
        std::fs::read_to_string(manifest_dir().join("src/lifecycle/restart/common.rs")).unwrap();
    let restart_agent =
        std::fs::read_to_string(manifest_dir().join("src/lifecycle/restart/agent.rs")).unwrap();

    let offenders = [
        ("launch.rs", &launch),
        ("restart/common.rs", &restart_common),
        ("restart/agent.rs", &restart_agent),
    ]
    .into_iter()
    .filter_map(|(name, src)| {
        src.contains(".build_command(auth_mode, None, None, model)")
            .then_some(name)
    })
    .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "Contract C: lifecycle launch/restart/start-agent must use build_command_with_tools/build_resume_command_with_context with resolved system_prompt, tools, and per-worker MCP config; context-free callsites remain: {offenders:?}"
    );
}

#[test]
fn contract_c_provider_mcp_config_is_parameterized_not_placeholder() {
    let adapter = get_adapter(Provider::Codex);
    let config = adapter.mcp_config(AuthMode::Subscription).unwrap();
    let raw = config.raw.to_string();
    let command = config.raw["team_orchestrator"]["command"]
        .as_str()
        .expect("Contract C step3: team_orchestrator MCP config must expose a command string");

    assert!(
        !raw.contains("/tmp/ta-provider-ws") && !raw.contains("worker1") && !raw.contains("teamA"),
        "Contract C: provider MCP config must be parameterized by real workspace/agent/team, never placeholder-shaped; raw={raw}"
    );
    assert!(
        raw.contains("TEAM_AGENT_WORKSPACE")
            || raw.contains("--workspace")
            || raw.contains("team-agent"),
        "Contract C: provider MCP config must point at a real Team Agent MCP server mechanism; raw={raw}"
    );
    assert_ne!(
        command, "team-agent",
        "Contract C step3/MCP startup: provider config must not use bare `team-agent`, because PATH can resolve to an older installed Python CLI; raw={raw}"
    );
    assert!(
        Path::new(command).is_absolute(),
        "Contract C step3/MCP startup: provider config command must be the current team-agent binary as an absolute path (`std::env::current_exe` semantics), not PATH-dependent; command={command:?} raw={raw}"
    );
}

#[test]
fn contract_d_codex_capture_rejects_team_agent_events_jsonl_as_rollout() {
    let ws = tmp_dir("d-events-rollout");
    let log_dir = ws.join(".team").join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let events_path = log_dir.join("events.jsonl");
    std::fs::write(
        &events_path,
        r#"{"event":"coordinator.boot","workspace":"/tmp/ta-subprep"}"#,
    )
    .unwrap();

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter.capture_session_id("w1", &ws, 0).unwrap();

    assert!(
        captured.is_none(),
        "Contract D/F5/N11: Team Agent `.team/logs/events.jsonl` is not a Codex provider transcript and must not become rollout_path; captured={captured:?}"
    );
}

#[test]
fn contract_d_codex_rollout_requires_provider_lifecycle_shape_or_stays_unknown() {
    let team_agent_events = r#"{"event":"message.delivered","message_id":"msg_1"}"#;
    let classified =
        classify(Provider::Codex, team_agent_events, ProcessLiveness::Unverifiable, 0.0).unwrap();
    assert_eq!(
        classified.state,
        TurnState::Unknown,
        "Contract D: Team Agent event rows do not contain Codex lifecycle facts and must classify unknown"
    );
    assert!(
        !classified.state.is_idle_for_takeover(),
        "Contract D/F5/N11: no valid Codex transcript means honest unknown, never fake idle"
    );

    let ws = tmp_dir("d-valid-rollout");
    let valid = ws.join("codex-session.jsonl");
    std::fs::write(
        &valid,
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"ct1"}}"#,
    )
    .unwrap();
    let adapter = get_adapter(Provider::Codex);
    let captured = adapter.capture_session_id("w1", &ws, 0).unwrap();
    assert!(
        captured
            .as_ref()
            .and_then(|c| c.rollout_path.as_ref())
            .is_some_and(|p| p.as_path() == valid),
        "Contract D: valid Codex lifecycle-shaped transcript may be persisted as rollout_path; captured={captured:?}"
    );
}

fn message_status(db_path: &Path, message_id: &str) -> String {
    let conn = team_agent::db::schema::open_db(db_path).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap()
}

fn message_delivery_attempts(db_path: &Path, message_id: &str) -> i64 {
    let conn = team_agent::db::schema::open_db(db_path).unwrap();
    conn.query_row(
        "select delivery_attempts from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
}

fn read_events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team").join("logs").join("events.jsonl"))
        .unwrap_or_default()
}

fn count_event(events: &str, event_name: &str) -> usize {
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == event_name)
        .count()
}

fn count_handled_action(events: &str, action: &str) -> usize {
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "startup_prompt_handled")
        .filter_map(|event| event["handled"].as_array().cloned())
        .flat_map(|handled| handled.into_iter())
        .filter(|handled| handled["action"] == action)
        .count()
}

#[derive(Debug, Default)]
struct TransportState {
    captures: Vec<String>,
    capture_idx: usize,
    inject_count: usize,
    send_keys_count: usize,
    session_present: bool,
    windows: Vec<WindowName>,
    spawn_argv: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
struct RecordingTransport {
    state: std::sync::Arc<Mutex<TransportState>>,
}

impl RecordingTransport {
    fn new(captures: Vec<String>) -> Self {
        Self {
            state: std::sync::Arc::new(Mutex::new(TransportState {
                captures,
                ..TransportState::default()
            })),
        }
    }

    fn with_session_present(self, present: bool) -> Self {
        self.state.lock().unwrap().session_present = present;
        self
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.state.lock().unwrap().windows = windows;
        self
    }

    fn inject_count(&self) -> usize {
        self.state.lock().unwrap().inject_count
    }

    fn send_keys_count(&self) -> usize {
        self.state.lock().unwrap().send_keys_count
    }

    fn spawn_argv(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .spawn_argv
            .first()
            .cloned()
            .unwrap_or_default()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        pane_prefix: &str,
    ) -> SpawnResult {
        let mut state = self.state.lock().unwrap();
        state.spawn_argv.push(argv.to_vec());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}{}", pane_prefix, state.spawn_argv.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, "first"))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, "into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.state.lock().unwrap().inject_count += 1;
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        self.state.lock().unwrap().send_keys_count += 1;
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let mut state = self.state.lock().unwrap();
        let text = state
            .captures
            .get(state.capture_idx)
            .or_else(|| state.captures.last())
            .cloned()
            .unwrap_or_default();
        state.capture_idx = state.capture_idx.saturating_add(1);
        Ok(CapturedText { text, range })
    }

    fn query(
        &self,
        _target: &Target,
        field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
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

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.state.lock().unwrap().session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.state.lock().unwrap().windows.clone())
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
