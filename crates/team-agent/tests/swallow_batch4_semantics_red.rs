//! Swallow-family batch 4: the two cr-gated semantic-change points.
//!
//! Basis: `.team/artifacts/swallow-family-slices.md` batch 4 +
//! `swallow-batch4-cr-verdict.md` (workspace artifacts; BOTH points APPROVE'd,
//! 8 constraints C1-C8 + 6 reverse cases — folded below).
//!
//! Point 1 (C1-C4): a corrupt provider-config JSON must FAIL EXPLICITLY — today
//!   profile_launch.rs:639-649 `read_json_object` turns any parse failure into an
//!   empty map and `write_json_object` then flattens the user's file (silent
//!   destructive rewrite; MUST-NOT-9 / MUST-NOT-13 violation).
//! Point 2 (C5-C8): MCP scope validation must FAIL CLOSED when the runtime state is
//!   unreadable — today `load_runtime_state(..).unwrap_or(json!({}))`
//!   (mcp_server/tools.rs:574/:232) silently substitutes an empty state, so a
//!   multi-team workspace degrades to "legacy single-team" and an owner-less client
//!   slips through the scope gate (silent privilege widening; MUST-16).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use serde_json::json;
use serial_test::serial;
use team_agent::lifecycle::launch_with_transport;

/// C1-C4 + reverse cases `profile_invalid_json_returns_error_and_does_not_overwrite_file`
/// / `..._includes_path_and_parse_detail_and_next_action`: launching a claude
/// compatible_api profile over a CORRUPT `.claude.json` must return a structured error
/// (path + parse detail + next action) and leave the corrupt file byte-identical —
/// never rewrite it with a flattened empty object.
#[test]
#[serial(b4_mcp)]
fn b4_profile_invalid_json_fails_explicit_and_never_flattens_the_file() {
    // Pin a neutral process ancestry so the launch path is deterministic regardless of
    // which provider flags this test process inherited (same guard as the #264 batch).
    std::env::set_var(
        "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON",
        "[\"/bin/zsh\"]",
    );
    let ws = tmp_ws("b4-profile");
    let team_dir = write_claude_compatible_team(&ws);
    let config_dir = ws.join(".team/runtime/provider-config/worker_a/claude");
    std::fs::create_dir_all(&config_dir).unwrap();
    let corrupt = b"{ this is the user's corrupt claude state".to_vec();
    std::fs::write(config_dir.join(".claude.json"), &corrupt).unwrap();

    let result = launch_with_transport(
        &team_dir.join("team.spec.yaml"),
        false,
        true,
        true,
        &offline_transport::RecordingTransport::new(),
    );

    let mut failures = Vec::new();
    match &result {
        Ok(report) => failures.push(format!(
            "C1/C2: a corrupt provider-config JSON must fail the launch with a \
structured profile_invalid_json error, not silently continue; got Ok({report:?})"
        )),
        Err(error) => {
            let text = error.to_string();
            if !text.contains(".claude.json") {
                failures.push(format!(
                    "C1/N38: the error must name the offending path; got {text}"
                ));
            }
        }
    }
    let after = std::fs::read(config_dir.join(".claude.json")).unwrap_or_default();
    if after != corrupt {
        failures.push(format!(
            "C3/C4: the user's corrupt file must stay byte-identical (framework never \
flattens user files on load failure — MUST-NOT-9); before={} bytes, after={} bytes: {:?}",
            corrupt.len(),
            after.len(),
            String::from_utf8_lossy(&after)
        ));
    }
    std::env::remove_var("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON");
    assert!(
        failures.is_empty(),
        "batch4 point-1 profile fail-explicit contract failed:\n{}",
        failures.join("\n")
    );
}

/// C5-C8 + reverse cases `scope_unverifiable_when_state_read_fails_returns_structured_refusal`
/// / `scope_unverifiable_state_recovers_retry_passes`: with the runtime state
/// unreadable, an owner-less MCP client in a (really) multi-team workspace must be
/// REFUSED (scope unverifiable, fail-closed), not silently treated as legacy
/// single-team; once the state is restored the same call gets the honest multi-team
/// refusal path (verifiable again).
#[test]
#[serial(b4_mcp)]
fn b4_scope_validation_fails_closed_when_state_unreadable() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_b", "");
    let state_path = harness.workspace_path().join(".team/runtime/state.json");
    let good_state = std::fs::read(&state_path).unwrap();
    std::fs::write(&state_path, "{ unreadable state").unwrap();

    let broken = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "b4 fail-closed probe"}),
    );

    let mut failures = Vec::new();
    let broken_text = broken.body.to_string();
    if !broken.is_error
        || !(broken_text.contains("unverifiable") || broken_text.contains("scope"))
        || broken_text.contains("internal_runtime_error")
    {
        failures.push(format!(
            "C5/C6/N38: with state unreadable the refusal must be the STRUCTURED \
scope_unverifiable kind (reason='state read failed: ..' + retry next_action), not a \
silent legacy-single-team pass (tools.rs unwrap_or(json!({{}}))) and not an opaque \
internal_runtime_error leak; body={}",
            broken.body
        ));
    }

    // C7: restore the state — the same call must hit the honest, verifiable gate again
    // (here: the multi-team owner-required refusal), proving the outage refusal was
    // transient, not durable.
    std::fs::write(&state_path, &good_state).unwrap();
    let recovered = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "b4 recovery probe"}),
    );
    let recovered_text = recovered.body.to_string();
    if !recovered_text.contains("TEAM_AGENT_OWNER_TEAM_ID")
        && !recovered_text.contains("scope_refused")
    {
        failures.push(format!(
            "C7: after state recovery the same call must reach the verifiable multi-team \
gate (owner-required refusal), not some degraded path; body={}",
            recovered.body
        ));
    }

    assert!(
        failures.is_empty(),
        "batch4 point-2 fail-closed scope contract failed:\n{}",
        failures.join("\n")
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

/// Compiled claude compatible_api team with a profile (the path that maintains the
/// managed `.claude.json`). Mirrors the #264 profile fixture, claude flavor.
fn write_claude_compatible_team(ws: &Path) -> PathBuf {
    let team = ws.join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(team.join("profiles")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: b4team\nobjective: batch4 profile fixture.\nprovider: claude\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("agents/worker_a.md"),
        "---\nname: worker_a\nrole: Compat Worker\nprovider: claude\nmodel: claude-haiku\nauth_mode: compatible_api\nprofile: relay\ntools:\n  - mcp_team\n---\n\nCompat worker.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("profiles/relay.env"),
        "AUTH_MODE=compatible_api\nBASE_URL=https://relay.example/v1\nAPI_KEY=sk-relay-b4\nMODEL=claude-haiku\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&team).expect("compile fixture team");
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team
}

mod offline_transport {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use team_agent::transport::{
        AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
        InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
        SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
        TurnVerification, WindowName,
    };

    #[derive(Default)]
    pub struct RecordingTransport {
        session_present: Mutex<bool>,
    }

    impl RecordingTransport {
        pub fn new() -> Self {
            Self::default()
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
            _argv: &[String],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            *self.session_present.lock().unwrap() = true;
            Ok(SpawnResult {
                pane_id: PaneId::new("%1"),
                session: session.clone(),
                window: window.clone(),
                child_pid: None,
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
                text: "Claude Code\n> ".to_string(),
                range,
            })
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
            Ok(())
        }

        fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
            Ok(())
        }

        fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
            Ok(AttachOutcome::Attached)
        }
    }
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-swallow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
