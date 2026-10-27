//! agent-clone-fork · successor RED batch 4 (verifier) — R8 Copilot fork
//! (caps re-charter) + R7 permission-clamp GUARDRAIL.
//!
//! From locate.md §7 R7/R8 + §1.4 (Copilot 1.0.61 real-machine proof: directory
//! copy + SQLite transaction under COPILOT_HOME is feasible) + §3 surface
//! (`provider/adapter.rs` Copilot caps=false; `provider/session_scan/copilot.rs`
//! hard-reads `$HOME/.copilot`; `fork_agent.rs:173-288` provider/permission
//! inheritance) + MUST-16, NOT the §4 design. Baseline `9e6be51` (v0.5.52).
//!
//! - **R8 Copilot fork caps RE-CHARTER (RED + verifier signature)**: baseline
//!   locks "Copilot fork MUST be refused (capability unsupported)"
//!   (`copilot_provider_red.rs::copilot_fork_returns_structured_capability_unsupported`).
//!   §1.4 proves Copilot CAN fork (directory copy + atomic SQLite transaction
//!   under an isolated COPILOT_HOME). This batch RED asserts that a Copilot fork
//!   is no longer rejected by a blanket capability gate. Baseline red: the fork
//!   is refused with a copilot/fork capability-unsupported error. Retiring the old
//!   "must refuse" contract is a re-charter that REQUIRES a verifier signature —
//!   recorded in
//!   `.team/artifacts/pipeline-runs/agent-clone-fork/verifier-r8-caps-recharter-signoff.md`.
//!   (The SQLite foreign-key table integrity + exact-nonce recall are the GREEN
//!   half / subscription-E2E per §1.4 — not asserted offline here.)
//!
//! - **R7 permission-clamp GUARDRAIL (retired)**: R7/R7b drove a legacy Claude
//!   seat fork through a backing shim; that runtime is gone, so they left with it.
//!   Original charter: a
//!   fork's effective approval policy must be derived from the LEADER
//!   (`source=leader_process`, `worker_capability_above_leader=false`), never a
//!   raw clone of a source-declared capability above the leader (MUST-16).
//!   Measured: baseline already derives approval from the leader and marks
//!   `worker_capability_above_leader=false`, so there is no offline structural
//!   red. Following the L1 r2 naturally-green-guardrail precedent, this pins the
//!   clamp as a guardrail whose teeth engage after implement recompiles the role
//!   via RoleSourceResolver (§4.2): any impl that raw-clones a compiled permission
//!   above the leader trips it on the spot. R7's escalation scenario (a source
//!   role declaring capability above the leader) cannot be constructed offline —
//!   the role model forces `permission_mode=restricted` and inherits approval from
//!   the leader — so the true escalation guard is a subscription/real-machine
//!   concern; the guardrail defends the invariant that already holds.
//!
//! R9 clone-semantics is NOT frozen in this batch: offline, `clone-agent` does not
//! exist (already RED in batch1 R10), and its distinguishing semantics
//! (fresh-session-distinct-from-fork, carries no source nonce) are subscription
//! real-machine concerns (locate §4.5, §5 obs 6). It is recorded against R10 +
//! the E2E gate, not asserted offline here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::lifecycle::launch::fork_agent_with_transport;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch4";
const SOURCE: &str = "cp_worker";
const NEW: &str = "cp_fork";
const R8_CHILD: &str = "TEAM_AGENT_TEST_CF_R8_CHILD";
const ISOLATION_GUARD_CHILD: &str = "TEAM_AGENT_TEST_CF_ISOLATION_GUARD_CHILD";
const PARENT_PID_ENV: &str = "TEAM_AGENT_TEST_CF_PARENT_PID";
const R8_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "r8_copilot_fork_is_no_longer_capability_refused"
);
const ISOLATION_GUARD_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "copilot_clone_fork_env_fixtures_run_outside_parent_process"
);

/// These fixtures need process-global HOME and TEAM_AGENT_* overrides, while the
/// lib-test harness runs sibling tests in parallel. Execute each body as the sole
/// selected test in a child process so HermeticTestEnv never mutates the parent
/// test process. Fixture-caller tests still hold `serial(env)` while their child
/// runs: the child inherits the parent's PATH, and a sibling env test may have a
/// fake `tmux` on it that never creates the fixture caller socket.
fn run_process_isolated(marker: &str, test_name: &str, body: impl FnOnce()) {
    if std::env::var_os(marker).is_some() {
        body();
        return;
    }

    let output = Command::new(std::env::current_exe().expect("current lib-test executable"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(marker, "1")
        .env(PARENT_PID_ENV, std::process::id().to_string())
        .output()
        .expect("run fixture in isolated child test process");
    assert!(
        output.status.success(),
        "isolated child test failed: test={test_name} status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn copilot_clone_fork_env_fixtures_run_outside_parent_process() {
    run_process_isolated(ISOLATION_GUARD_CHILD, ISOLATION_GUARD_TEST, || {
        let parent_pid = std::env::var(PARENT_PID_ENV)
            .expect("isolated child receives the parent test process id");
        assert_ne!(
            std::process::id().to_string(),
            parent_pid,
            "clone/fork env fixtures must never run in the parent lib-test process"
        );
    });
}

/// Copilot team docs for the library-entry R8 (a separate agents dir so the
/// OfflineTransport quick-start compiles a copilot source).
fn write_copilot_team_docs(workspace: &Path) -> PathBuf {
    let team_dir = workspace.join("cf-r8-team");
    std::fs::create_dir_all(team_dir.join("agents")).expect("create team agents dir");
    std::fs::write(
        team_dir.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch4 copilot recharter.\nprovider: copilot\n---\n\nTeam fixture.\n"),
    )
    .expect("write TEAM.md");
    std::fs::write(
        team_dir.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: Copilot Worker\nprovider: copilot\nmodel: gpt-5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nCopilot source body.\n"
        ),
    )
    .expect("write source role doc");
    team_dir
}

/// Seed the SOURCE's complete session tuple directly in runtime state (library
/// path) so the copilot fork reaches the capability gate, not a missing-session
/// gate. Mirrors the old refusal contract's `seed_session_id`.
fn seed_source_tuple_in_state(workspace: &Path, agent: &str) {
    let mut state = load_runtime_state(workspace).expect("load runtime state");
    // Per-test path under this workspace. Do not touch $HOME/.copilot —
    // that races with sibling --lib fork tests that also mutate process HOME
    // and CREATE TABLE sessions on a shared session-store.db.
    // r8 only asserts the unverified refuse, which fires before any store I/O.
    let source_session_id = format!(
        "r8-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let rollout = workspace
        .join("copilot-fixture")
        .join(&source_session_id)
        .join("unused.db");
    std::fs::create_dir_all(rollout.parent().expect("parent")).expect("create per-test copilot dir");
    std::fs::write(&rollout, b"").expect("touch unused db placeholder");
    let tuple = json!({
        "status": "running",
        "provider": "copilot",
        "session_id": source_session_id,
        "rollout_path": rollout.to_string_lossy(),
        "captured_at": "2026-07-21T00:00:00Z",
        "captured_via": "contract-fixture",
    });
    let apply = |obj: &mut serde_json::Map<String, Value>| {
        for (k, v) in tuple.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
    };
    if let Some(obj) = state
        .pointer_mut(&format!("/agents/{agent}"))
        .and_then(Value::as_object_mut)
    {
        apply(obj);
    }
    if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
        for team in teams.values_mut() {
            if let Some(obj) = team
                .get_mut("agents")
                .and_then(|a| a.get_mut(agent))
                .and_then(Value::as_object_mut)
            {
                apply(obj);
            }
        }
    }
    save_runtime_state(workspace, &state).expect("save seeded state");
}

/// R8 Copilot fork caps RE-CHARTER — a Copilot fork must no longer be refused by a
/// blanket capability gate (§1.4 proves the directory-copy + SQLite-transaction
/// primitive is feasible under an isolated COPILOT_HOME). Baseline red: the fork
/// is refused with a copilot/fork capability-unsupported error.
///
/// Retiring the old "Copilot fork must be refused" contract is a re-charter
/// requiring a verifier signature — see
/// `verifier-r8-caps-recharter-signoff.md`. The SQLite FK-table integrity and
/// exact-nonce recall remain the GREEN half / subscription-E2E (§1.4).
#[test]
#[serial_test::serial(env)]
fn r8_copilot_fork_is_no_longer_capability_refused() {
    run_process_isolated(R8_CHILD, R8_TEST, || {
        // R8 uses the LIBRARY entry `fork_agent_with_transport` (the same entry the old
        // refusal contract exercises), because the CLI `fork-agent` path reports
        // ok:true for a copilot source (registration-only success) and never reaches
        // the capability gate — a CLI-shaped R8 could never be RED. An in-process
        // RecordingTransport stands the team up without a live copilot TUI (no
        // ready-screen hang).
        let env = HermeticTestEnv::enter_with_leader("cf-r8");
        let workspace = env.workspace("ws");
        super::launch_spawn::seed_healthy_coordinator(&workspace);
        let team_dir = write_copilot_team_docs(&workspace);
        // Session ABSENT at quick-start so it can create the team session; spawn_first
        // flips session_present true, so the later fork sees a live session. (Starting
        // with session_present=true causes a SessionConflict at quick-start.)
        let transport = RecordingTransport::default();
        quick_start_with_transport_in_workspace(
            &workspace,
            &team_dir,
            Some(TEAM_NAME),
            true,
            Some("cfr8"),
            &transport,
        )
        .expect("quick-start seeds the copilot team");
        // Seed the source's complete session tuple so the fork reaches the capability
        // gate (not a missing-session gate) — mirrors the old refusal contract's seed.
        seed_source_tuple_in_state(&workspace, SOURCE);

        // The team KEY is the quick-start team_id ("cfr8"), not the display name.
        let result = fork_agent_with_transport(
            &workspace,
            &AgentId::new(SOURCE),
            &AgentId::new(NEW),
            None,
            false,
            Some("cfr8"),
            &transport,
        );

        // Baseline: copilot fork is refused with a structured copilot/fork
        // capability-unsupported error (copilot_provider_red.rs C-4-2/C-4-3). R8 pins
        // that this blanket capability refusal is GONE post-recharter.
        let text = format!("{result:?}").to_lowercase();
        assert!(
            result.is_err() && (text.contains("unverified") || text.contains("未验证")),
            "Copilot in-window fork is unverified; result={result:?}"
        );
    });
}

// ---------------------------------------------------------------------------
// Minimal in-process Transport for the R8 library-entry test (mirrors the
// RecordingTransport the existing copilot refusal contract uses). Kept local so
// R8 exercises the real fork lifecycle capability gate without a live copilot TUI.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    window: String,
}

#[derive(Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: Mutex<bool>,
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
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            env: env.clone(),
            window: window.as_str().to_string(),
        });
        *self.session_present.lock().unwrap() = true;
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(30_000 + spawns.len() as u32),
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
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
            text: "Claude Code\n> \nOpenAI Codex\ncodex>\nCopilot".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session_present.lock().unwrap())
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
        *self.session_present.lock().unwrap() = false;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
