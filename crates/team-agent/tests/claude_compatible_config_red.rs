//! Claude compatible-api managed config contracts for Batch A Lane 2.
//!
//! The profile resolver owns file artifacts; the provider only consumes `managed_mcp_config`.
//! These tests verify the public files/env/argv shape expected by Python 0.2.11 parity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{
    get_adapter, McpConfig, ProviderCommandContext, ProviderCommandOverrides, ProviderProfileLaunch,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
fn compatible_api_profile_writes_claude_settings_and_project_state_with_mcp_servers() {
    let ws = tmp_dir("compatible-config-files");
    let team = write_claude_profile_team(&ws, "cfgteam", "clauder");
    let transport = RecordingTransport::default();

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cfgteam"),
        &transport,
    )
    .expect("quick-start through recording transport should resolve compatible profile config");

    let spawn = transport.single_spawn();
    let config_dir = spawn
        .env
        .get("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .expect("CLAUDE_CONFIG_DIR must be injected");
    let settings = read_json(&config_dir.join("settings.json"));
    let project_state = read_json(&config_dir.join(".claude.json"));

    assert!(
        settings_contains_trust_onboarding_and_theme(&settings),
        "settings.json must pre-accept workspace trust/onboarding/theme for managed compatible_api Claude; path={} settings={settings}",
        config_dir.join("settings.json").display()
    );
    assert!(
        project_state
            .pointer("/mcpServers/team_orchestrator")
            .or_else(|| project_state.pointer("/projects/cfgteam/mcpServers/team_orchestrator"))
            .is_some(),
        ".claude.json must contain the Team Agent MCP server project state; path={} project_state={project_state}",
        config_dir.join(".claude.json").display()
    );
    assert!(
        !argv_has_flag(&spawn.argv, "--mcp-config"),
        "managed compatible_api launch uses CLAUDE_CONFIG_DIR artifacts, not provider argv --mcp-config; argv={:?}",
        spawn.argv
    );
}

#[test]
fn managed_profile_command_plan_omits_mcp_config_even_when_mcp_config_is_available() {
    let root = tmp_dir("managed-plan-config");
    let profile = ProviderProfileLaunch {
        env_overlay: BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            root.join("claude-config").display().to_string(),
        )]),
        env_unset: BTreeSet::from(["ANTHROPIC_API_KEY".to_string()]),
        command_overrides: ProviderCommandOverrides {
            model: Some("profile-effective-haiku".to_string()),
            ..ProviderCommandOverrides::default()
        },
        claude_config_dir: Some(root.join("claude-config")),
        claude_projects_root: Some(root.join("claude-config").join("projects")),
        managed_mcp_config: true,
    };
    let config = McpConfig {
        raw: json!({
            "team_orchestrator": {
                "type": "stdio",
                "command": "team-agent",
                "args": ["mcp-server", "--workspace", "{workspace}"]
            }
        }),
    };
    let tools = ["mcp_team"];

    let plan = get_adapter(Provider::Claude)
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::CompatibleApi,
            mcp_config: Some(&config),
            system_prompt: Some("worker prompt requiring native MCP"),
            model: Some("profile-effective-haiku"),
            tools: &tools,
            profile_launch: Some(&profile),
            agent_id_hint: None,
                effort: None,
        })
        .expect("managed compatible_api command plan should build");

    assert!(
        plan.managed_mcp_config,
        "CommandPlan.managed_mcp_config must reflect ProviderProfileLaunch.managed_mcp_config; plan={plan:?}"
    );
    assert!(
        !argv_has_flag(&plan.argv, "--mcp-config"),
        "managed mode must not pass --mcp-config JSON/string in argv; config is materialized under CLAUDE_CONFIG_DIR; argv={:?}",
        plan.argv
    );
}

fn settings_contains_trust_onboarding_and_theme(settings: &Value) -> bool {
    let text = settings.to_string();
    ["trust", "onboarding", "theme"]
        .iter()
        .all(|needle| text.to_ascii_lowercase().contains(needle))
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("expected managed Claude config file {}: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("managed Claude config file must be JSON {}: {err}; text={text}", path.display()))
}

fn write_claude_profile_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("current").join("profiles")).unwrap();
    std::fs::write(
        ws.join(".team").join("current").join("profiles").join("local.env"),
        "ANTHROPIC_BASE_URL=https://llm.local/v1\nANTHROPIC_AUTH_TOKEN=local-secret\nANTHROPIC_MODEL=profile-effective-haiku\n",
    )
    .unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: Claude compatible config contract.\nprovider: claude\nauth_mode: compatible_api\n---\n\nTeam.\n"
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

fn argv_has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-claude-compatible-config-{tag}-{}-{}",
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
            child_pid: Some(20_000 + spawns.len() as u32),
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
