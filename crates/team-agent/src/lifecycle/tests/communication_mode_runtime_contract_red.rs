//! 0.5.61 communication-mode runtime-contract assembly RED.
//!
//! The observation boundary is the real quick-start command plan captured by a
//! transport. The test consumes `CommunicationMode::ALL`; it does not copy the
//! product templates. Only stable section ids and requirement-level semantic
//! markers are asserted.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use team_agent::communication_mode::CommunicationMode;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const CONTRACT_HEADING_PREFIX: &str = "# Team Agent communication contract: ";

#[test]
fn t06_each_official_mode_selects_one_runtime_contract_in_real_spawn_prompt() {
    let root = fixture("official", None);
    let transport = RecordingTransport::default();
    quick_start_with_transport_in_workspace(
        &root,
        &root,
        None,
        true,
        Some("communication-contract-red"),
        &transport,
    )
    .expect("official modes must reach real worker command assembly");

    let spawns = transport.spawns.lock().unwrap();
    assert_eq!(
        spawns.len(),
        CommunicationMode::ALL.len(),
        "T06: fixture must exercise every product-catalog mode exactly once"
    );
    for mode in CommunicationMode::ALL.iter().copied() {
        let id = mode.as_str();
        let spawn = spawns
            .iter()
            .find(|spawn| spawn.window.as_str().contains(id))
            .unwrap_or_else(|| panic!("T06/{id}: no spawn recorded for product-catalog mode"));
        let prompt = flag_value(&spawn.argv, "--append-system-prompt");
        let own_heading = format!("{CONTRACT_HEADING_PREFIX}{id}");
        assert!(
            prompt.contains(&own_heading),
            "T06/{id}: assembled launch prompt lacks the selected official contract section; expected structural heading {own_heading:?}; prompt={prompt:?}"
        );
        for other in CommunicationMode::ALL
            .iter()
            .copied()
            .filter(|other| *other != mode)
        {
            let other_heading = format!("{CONTRACT_HEADING_PREFIX}{}", other.as_str());
            assert!(
                !prompt.contains(&other_heading),
                "T06/{id}: assembled prompt contains mutually exclusive contract {other_heading:?}"
            );
        }
        assert!(
            prompt.contains(&format!("ROLE BODY SENTINEL {id}"))
                && prompt.contains("Team Agent Teammate Runtime Contract")
                && prompt.contains("report_result exactly once"),
            "T06/{id}: mode selection must preserve persona body and common exact-once contract"
        );
    }
}

#[test]
fn t06_selected_templates_project_the_two_signed_communication_boundaries() {
    let root = fixture("semantics", None);
    let transport = RecordingTransport::default();
    quick_start_with_transport_in_workspace(
        &root,
        &root,
        None,
        true,
        Some("communication-contract-semantics"),
        &transport,
    )
    .expect("official modes must assemble");
    let spawns = transport.spawns.lock().unwrap();
    let prompt = |mode: CommunicationMode| {
        let id = mode.as_str();
        let spawn = spawns
            .iter()
            .find(|spawn| spawn.window.as_str().contains(id))
            .unwrap_or_else(|| panic!("T06/{id}: missing recorded spawn"));
        flag_value(&spawn.argv, "--append-system-prompt")
    };

    let leader = prompt(CommunicationMode::LeaderCentric);
    for marker in ["Progress", "blocker", "question"] {
        assert!(
            leader.contains(marker),
            "T06/leader_centric: official template must retain {marker:?} guidance; prompt={leader:?}"
        );
    }

    let orchestrated = prompt(CommunicationMode::Orchestrated);
    for marker in ["declared channel", "task-related", "ACK"] {
        assert!(
            orchestrated.contains(marker),
            "T06/orchestrated: official template must state the on-demand channel/response boundary via {marker:?}; prompt={orchestrated:?}"
        );
    }
}

#[test]
fn t07_final_spawn_prompt_does_not_leak_leader_centric_obligations_into_orchestrated() {
    let root = fixture("negative-boundary", None);
    let transport = RecordingTransport::default();
    quick_start_with_transport_in_workspace(
        &root,
        &root,
        None,
        true,
        Some("communication-contract-negative-boundary"),
        &transport,
    )
    .expect("official modes must assemble");
    let spawns = transport.spawns.lock().unwrap();
    let orchestrated = spawns
        .iter()
        .find(|spawn| {
            spawn
                .window
                .as_str()
                .contains(CommunicationMode::Orchestrated.as_str())
        })
        .map(|spawn| flag_value(&spawn.argv, "--append-system-prompt"))
        .expect("T07/orchestrated: missing recorded spawn");

    for required in [
        "All communication must go through Team Agent MCP tools.",
        "If blocked or waiting, send_message to the leader. Do not wait silently.",
        "report_result exactly once",
    ] {
        assert!(
            orchestrated.contains(required),
            "T07/orchestrated positive control: removing unconditional duties must preserve common MCP, blocked-reporting, and exact-once obligations; missing {required:?}; prompt={orchestrated:?}"
        );
    }

    let default_root = default_fixture("leader-centric-positive-control");
    let default_transport = RecordingTransport::default();
    quick_start_with_transport_in_workspace(
        &default_root,
        &default_root,
        None,
        true,
        Some("communication-contract-default-positive-control"),
        &default_transport,
    )
    .expect("omitted communication_mode must assemble as leader_centric");
    let default_spawns = default_transport.spawns.lock().unwrap();
    let default_prompt = default_spawns
        .first()
        .map(|spawn| flag_value(&spawn.argv, "--append-system-prompt"))
        .expect("T07/leader_centric positive control: missing recorded default spawn");
    for required in [
        "Progress, blockers, questions:",
        "When you receive a message from the leader or a teammate, you MUST respond",
    ] {
        assert!(
            default_prompt.contains(required),
            "T07/leader_centric positive control: omitted mode must retain {required:?}; prompt={default_prompt:?}"
        );
    }

    let leaked = [
        "Progress, blockers, questions:",
        "When you receive a message from the leader or a teammate, you MUST respond",
    ]
    .into_iter()
    .filter(|forbidden| orchestrated.contains(forbidden))
    .collect::<Vec<_>>();
    assert!(
        leaked.is_empty(),
        "T07/orchestrated: assembled final prompt leaks unconditional leader_centric obligations {leaked:?}; prompt={orchestrated:?}"
    );
}

#[test]
fn t06_third_mode_is_rejected_before_any_worker_command_is_assembled() {
    let root = fixture("unknown", Some("synthetic_third_mode"));
    let transport = RecordingTransport::default();
    let error = quick_start_with_transport_in_workspace(
        &root,
        &root,
        None,
        true,
        Some("communication-contract-unknown"),
        &transport,
    )
    .expect_err("T06: a third runtime contract shape must fail closed before launch");
    assert!(
        error.to_string().contains("communication_mode")
            && error.to_string().contains("synthetic_third_mode"),
        "T06: rejection must identify the invalid communication mode; error={error}"
    );
    assert!(
        transport.spawns.lock().unwrap().is_empty(),
        "T06: invalid mode must not leave a partially launched roster"
    );
}

fn fixture(tag: &str, team_mode: Option<&str>) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!(
        "communication-mode-runtime-red-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("agents")).unwrap();
    let team_mode = team_mode
        .map(|mode| format!("communication_mode: {mode}\n"))
        .unwrap_or_default();
    std::fs::write(
        root.join("TEAM.md"),
        format!(
            "---\nname: communication-mode-runtime-red\nprovider: claude\n{team_mode}---\n\nTeam fixture.\n"
        ),
    )
    .unwrap();
    for mode in CommunicationMode::ALL.iter().copied() {
        let id = mode.as_str();
        std::fs::write(
            root.join("agents").join(format!("worker_{id}.md")),
            format!(
                "---\nname: worker_{id}\nrole: Communication Worker\nprovider: claude\ndangerously_skip_permissions: false\ncommunication_mode: {id}\ntools:\n  - mcp_team\n---\n\nROLE BODY SENTINEL {id}\n"
            ),
        )
        .unwrap();
    }
    root
}

fn default_fixture(tag: &str) -> PathBuf {
    let root = fixture(tag, None);
    for mode in CommunicationMode::ALL {
        std::fs::remove_file(
            root.join("agents")
                .join(format!("worker_{}.md", mode.as_str())),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("agents").join("worker_default.md"),
        "---\nname: worker_default\nrole: Communication Worker\nprovider: claude\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nROLE BODY SENTINEL default\n",
    )
    .unwrap();
    root
}

fn flag_value(argv: &[String], flag: &str) -> String {
    argv.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    window: WindowName,
    argv: Vec<String>,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
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
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_into(session, window, argv, cwd, env)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            window: window.clone(),
            argv: argv.to_vec(),
        });
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(30_000 + spawns.len() as u32),
        })
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
    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok((field == PaneField::PaneWidth).then(|| "120".to_string()))
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
        Ok(false)
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
