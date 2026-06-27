//! Claude compatible-api profile launch contracts for Batch A Lane 2.
//!
//! The entrypoints are lifecycle/CLI surfaces with a recording transport, plus the S0 public
//! `ProviderProfileLaunch` shape. Before S0 lands this file compile-reds on the shared type; after
//! S0 it behavior-reds on lifecycle/profile wiring.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::provider::{ProviderCommandOverrides, ProviderProfileLaunch};
use team_agent::state::persist::load_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn compatible_api_profile_quick_start_spawns_claude_with_profile_env_and_state_metadata() {
    assert_s0_profile_launch_shape();

    let ws = tmp_dir("profile-launch");
    let team = write_claude_profile_team(&ws, "claudeprof", "clauder");
    let transport = RecordingTransport::default();

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("claudeprof"),
        &transport,
    )
    .expect("quick-start through recording transport should reach the real lifecycle spawn path");

    let spawn = transport.single_spawn();
    for (key, expected) in [
        ("CLAUDE_CONFIG_DIR", None),
        ("ANTHROPIC_BASE_URL", Some("https://llm.local/v1")),
        ("ANTHROPIC_AUTH_TOKEN", Some("local-secret")),
        ("ANTHROPIC_MODEL", Some("profile-effective-haiku")),
    ] {
        let got = spawn.env.get(key).map(String::as_str);
        match expected {
            Some(expected) => assert_eq!(
                got,
                Some(expected),
                "Claude compatible_api profile launch must inject {key} from .team/current/profiles/local.env; env={:?}",
                spawn.env
            ),
            None => assert!(
                got.is_some_and(|value| !value.is_empty()),
                "Claude compatible_api profile launch must inject non-empty {key}; env={:?}",
                spawn.env
            ),
        }
    }
    assert!(
        !spawn.env.contains_key("ANTHROPIC_API_KEY"),
        "compatible_api must unset ANTHROPIC_API_KEY and use ANTHROPIC_AUTH_TOKEN instead; env={:?}",
        spawn.env
    );
    assert!(
        argv_contains_adjacent(&spawn.argv, &["--model", "profile-effective-haiku"]),
        "profile model must become the effective Claude model when role model is null; argv={:?}",
        spawn.argv
    );

    let state = load_runtime_state(&ws).unwrap();
    let agent = &state["teams"]["claudeprof"]["agents"]["clauder"];
    assert!(
        agent["_pending_session_id"].as_str().is_some_and(|value| !value.is_empty()),
        "lifecycle must persist CommandPlan.expected_session_id as agents[*]._pending_session_id; state={state}"
    );
    assert!(
        agent["claude_projects_root"].as_str().is_some_and(|value| !value.is_empty()),
        "lifecycle must persist CommandPlan.provider_projects_root/profile claude_projects_root for capture; state={state}"
    );
}

#[test]
#[serial(env)]
fn subscription_profile_keeps_default_claude_config_dir_for_logged_in_quota() {
    let ws = tmp_dir("subscription-config");
    let team = write_claude_subscription_profile_team(&ws, "subteam", "clauder");
    let transport = RecordingTransport::default();

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("subteam"),
        &transport,
    )
    .expect("subscription profile should launch without managed config isolation");

    let spawn = transport.single_spawn();
    assert!(
        !spawn.env.contains_key("CLAUDE_CONFIG_DIR"),
        "subscription Claude workers must inherit the user's default ~/.claude login state; env={:?}",
        spawn.env
    );
    assert!(
        !spawn.env.contains_key("ANTHROPIC_API_KEY")
            && !spawn.env.contains_key("ANTHROPIC_AUTH_TOKEN"),
        "subscription launch must not switch into API-token auth via profile env; env={:?}",
        spawn.env
    );

    let state = load_runtime_state(&ws).unwrap();
    let agent = &state["teams"]["subteam"]["agents"]["clauder"];
    assert_eq!(
        agent.get("claude_projects_root"),
        None,
        "without a supported fine-grained Claude projects-root env, subscription workers must not fake an isolated transcript root; state={state}"
    );
}

#[test]
fn launch_restart_start_add_and_fork_all_delegate_to_the_profile_launch_resolver() {
    let launch = source("src/lifecycle/launch.rs");
    let restart_common = source("src/lifecycle/restart/common.rs");
    let restart_agent = source("src/lifecycle/restart/agent.rs");

    let surfaces = [
        ("launch quick-start", &launch, "spawn_agents"),
        ("add-agent", &launch, "add_agent_with_transport"),
        ("fork-agent", &launch, "fork_agent_with_transport"),
        ("restart", &restart_common, "restart_with_transport"),
        ("start-agent", &restart_agent, "start_agent_at_paths"),
    ];
    for (surface, source, entry) in surfaces {
        let section = source
            .find(entry)
            .map(|idx| &source[idx..])
            .unwrap_or(source.as_str());
        assert!(
            section.contains("prepare_provider_profile_launch"),
            "{surface} must call the single profile resolver before building/spawning provider argv; entry={entry}"
        );
    }
}

#[test]
fn diagnose_missing_profile_is_rc1_ok_false_and_zero_token() {
    let ws = tmp_dir("diagnose-missing-profile");
    let team = write_claude_profile_team(&ws, "diagteam", "clauder");
    std::fs::remove_file(ws.join(".team").join("current").join("profiles").join("local.env"))
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "diagnose",
            "--workspace",
            ws.to_str().unwrap(),
            "--json",
        ])
        .current_dir(&team)
        .output()
        .expect("diagnose subprocess should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json = serde_json::from_str::<Value>(stdout.trim()).unwrap_or_else(|_| json!({}));

    assert!(
        !output.status.success(),
        "diagnose with missing referenced profile must exit non-zero before any token/provider call; stdout={stdout} stderr={stderr}"
    );
    assert_eq!(
        json["ok"],
        json!(false),
        "diagnose missing profile must emit ok:false JSON; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("profile") || stderr.contains("profile"),
        "diagnose missing profile must name the profile problem without invoking Claude; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("tokens") && !stderr.contains("tokens"),
        "diagnose dry-run must not consume provider tokens or report token usage; stdout={stdout} stderr={stderr}"
    );
}

fn assert_s0_profile_launch_shape() {
    let profile = ProviderProfileLaunch {
        env_overlay: BTreeMap::from([("ANTHROPIC_MODEL".to_string(), "profile-effective-haiku".to_string())]),
        env_unset: BTreeSet::from(["ANTHROPIC_API_KEY".to_string()]),
        command_overrides: ProviderCommandOverrides {
            model: Some("profile-effective-haiku".to_string()),
            ..ProviderCommandOverrides::default()
        },
        claude_config_dir: Some(PathBuf::from("/tmp/claude-config")),
        claude_projects_root: Some(PathBuf::from("/tmp/claude-config/projects")),
        managed_mcp_config: true,
    };
    assert!(profile.managed_mcp_config);
}

fn write_claude_profile_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("current").join("profiles")).unwrap();
    std::fs::write(
        ws.join(".team").join("current").join("profiles").join("local.env"),
        "ANTHROPIC_BASE_URL=https://llm.local/v1\nANTHROPIC_AUTH_TOKEN=local-secret\nANTHROPIC_MODEL=profile-effective-haiku\nANTHROPIC_API_KEY=\n",
    )
    .unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: Claude compatible profile launch contract.\nprovider: claude\nauth_mode: compatible_api\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Claude Worker\nprovider: claude\nauth_mode: compatible_api\nprofile: local\nmodel: null\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn write_claude_subscription_profile_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("current").join("profiles")).unwrap();
    std::fs::write(
        ws.join(".team").join("current").join("profiles").join("local.env"),
        "AUTH_MODE=subscription\nMODEL=claude-sonnet-4-6\n",
    )
    .unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: Claude subscription profile launch contract.\nprovider: claude\nauth_mode: subscription\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Claude Worker\nprovider: claude\nauth_mode: subscription\nprofile: local\nmodel: null\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn source(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-claude-profile-launch-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
}

impl RecordingTransport {
    fn single_spawn(&self) -> RecordedSpawn {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1, "fixture should record one Claude worker spawn; spawns={spawns:?}");
        spawns[0].clone()
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
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_into(session, window, argv, _cwd, env)
    }

    fn spawn_into(
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
        });
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(10_000 + spawns.len() as u32),
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

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<team_agent::transport::PaneLiveness, TransportError> {
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
