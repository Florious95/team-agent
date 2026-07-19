//! #232 Codex MCP approval inheritance command-shape contracts.
//!
//! User-facing invariant: a worker Codex process must mirror the leader's effective approval mode,
//! but never silently exceed it. These are cargo-only command-shape contracts; no subscription or
//! real Codex process is started.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "support/composite_source.rs"]
mod composite_source;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use serial_test::serial;
use team_agent::lifecycle::{
    add_agent_with_transport, fork_agent_with_transport, launch_with_transport,
    reset_agent_with_transport, restart_with_transport, start_agent_with_transport,
};
use team_agent::lifecycle::{
    DangerousApprovalSource, LaunchReport, LifecycleError, StartAgentOutcome, StartMode,
};
use team_agent::model::ids::AgentId;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const CODEX_BYPASS: &str = "--dangerously-bypass-approvals-and-sandbox";
const ANCESTRY_ENV: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const APPROVAL_ENV_KEYS: &[&str] = &[
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_LEADER_BYPASS_SOURCE",
    "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
    "TEAM_AGENT_LEADER_BYPASS_FLAG",
    "TEAM_AGENT_MCP_AUTO_APPROVE",
    "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
];

#[test]
#[serial(env)]
fn codex_worker_inherits_leader_bypass_flag_on_fresh_launch() {
    let _ancestry = MockAncestry::codex_bypass();
    let team = compiled_team_dir("fresh-inherited", false, &[("worker_a", "Worker A")]);
    let transport = RecordingTransport::new();

    let report = launch_with_transport(&team.join("team.spec.yaml"), false, true, true, &transport)
        .expect("fresh launch should spawn worker under recording transport");
    let spawn = transport.only_spawn("fresh launch");

    assert_codex_bypass(&spawn.argv, "fresh launch inherited leader Codex bypass");
    assert_bypass_env(&spawn, "fresh launch inherited leader Codex bypass");
    assert_eq!(
        report.safety.source,
        DangerousApprovalSource::LeaderProcess,
        "AI-1: dry/launch safety source must be leader_process when inherited from Codex ancestry"
    );
    assert!(
        report.safety.inherited,
        "AI-1: inherited approval must be marked inherited=true"
    );
    assert_eq!(report.safety.provider.as_deref(), Some("codex"));
    assert_eq!(report.safety.flag.as_deref(), Some(CODEX_BYPASS));
}

#[test]
#[serial(env)]
fn codex_worker_restricted_when_leader_restricted_reverse_core() {
    let _ancestry = MockAncestry::restricted();
    let team = compiled_team_dir("fresh-restricted", false, &[("worker_a", "Worker A")]);
    let transport = RecordingTransport::new();

    launch_with_transport(&team.join("team.spec.yaml"), false, true, true, &transport)
        .expect("restricted fresh launch should still spawn worker");
    let argv = transport.only_spawn_argv("restricted fresh launch");

    assert_codex_restricted(&argv, "AI-3: leader restricted -> worker restricted");
}

#[test]
#[serial(env)]
fn runtime_config_dangerous_requires_yes_and_then_applies_with_audit() {
    let _ancestry = MockAncestry::restricted();
    let team = compiled_team_dir("runtime-dangerous", true, &[("worker_a", "Worker A")]);
    let spec = team.join("team.spec.yaml");

    let dry_run = launch_with_transport(&spec, true, false, true, &RecordingTransport::new())
        .expect("dry-run should expose explicit runtime config safety");
    assert_eq!(
        dry_run.safety.source,
        DangerousApprovalSource::RuntimeConfig
    );
    assert!(!dry_run.safety.inherited);

    let without_yes = launch_with_transport(&spec, false, false, true, &RecordingTransport::new());
    assert!(
        without_yes.is_err(),
        "AI-9: explicit dangerous_auto_approve not inherited from leader must require --yes consent"
    );

    let transport = RecordingTransport::new();
    let with_yes = launch_with_transport(&spec, false, true, true, &transport)
        .expect("explicit runtime dangerous with --yes should launch");
    assert!(
        launch_safety_json(&with_yes).contains("worker_capability_above_leader"),
        "AI-6: explicit RuntimeConfig override above restricted leader must expose worker_capability_above_leader audit field; safety={:?}",
        with_yes.safety
    );
    assert_codex_bypass(
        &transport.only_spawn_argv("runtime config --yes"),
        "runtime config + --yes",
    );
}

#[test]
#[serial(env)]
fn inherited_bypass_non_dry_run_does_not_require_yes_but_explicit_raise_does() {
    let _inherited = MockAncestry::codex_bypass();
    let inherited_team = compiled_team_dir("inherited-no-yes", false, &[("worker_a", "Worker A")]);
    let inherited_transport = RecordingTransport::new();

    let inherited = launch_with_transport(
        &inherited_team.join("team.spec.yaml"),
        false,
        false,
        true,
        &inherited_transport,
    )
    .expect("AI-9: inherited leader-process bypass is mirrored consent and must not require --yes");
    assert_eq!(
        inherited.safety.source,
        DangerousApprovalSource::LeaderProcess,
        "AI-9 precondition: inherited bypass must be sourced from leader process"
    );
    assert!(
        inherited.safety.inherited,
        "AI-9 precondition: inherited bypass must be marked inherited"
    );
    assert_codex_bypass(
        &inherited_transport.only_spawn_argv("inherited bypass without --yes"),
        "AI-9 inherited non-dry-run without --yes",
    );

    drop(_inherited);

    let _restricted = MockAncestry::restricted();
    let explicit_team = compiled_team_dir("explicit-no-yes", true, &[("worker_a", "Worker A")]);
    let explicit = launch_with_transport(
        &explicit_team.join("team.spec.yaml"),
        false,
        false,
        true,
        &RecordingTransport::new(),
    );
    assert!(
        matches!(explicit, Err(LifecycleError::DangerousApprovalRequired(_))),
        "AI-9: explicit dangerous_auto_approve above a restricted leader is capability elevation and must require --yes; got {explicit:?}"
    );
}

#[test]
#[serial(env)]
fn inherited_bypass_is_preserved_across_restart_start_add_and_fork() {
    let _ancestry = MockAncestry::codex_bypass();

    let restart_ws = runtime_workspace("restart-inherited", &[("worker_a", "Worker A")]);
    seed_agent_state(&restart_ws, "worker_a", "Worker A", true);
    let restart_transport = RecordingTransport::new().with_session_present(false);
    let mut failures = Vec::new();

    restart_with_transport(&restart_ws, false, None, &restart_transport)
        .expect("restart fixture should spawn worker");
    let restart_spawn = restart_transport.only_spawn("restart resumed");
    failures.extend(codex_bypass_failures(&restart_spawn.argv, "AI-1 restart"));
    assert_bypass_env(&restart_spawn, "AI-1 restart resumed");

    let restart_fresh_ws = runtime_workspace("restart-fresh", &[("worker_a", "Worker A")]);
    seed_agent_state(&restart_fresh_ws, "worker_a", "Worker A", false);
    let restart_fresh_transport = RecordingTransport::new().with_session_present(false);
    restart_with_transport(&restart_fresh_ws, true, None, &restart_fresh_transport)
        .expect("fresh restart fixture should spawn worker");
    assert_bypass_env(
        &restart_fresh_transport.only_spawn("restart fresh"),
        "AI-1 restart fresh",
    );

    let start_ws = runtime_workspace("start-inherited", &[("worker_a", "Worker A")]);
    seed_agent_state(&start_ws, "worker_a", "Worker A", false);
    let start_transport = RecordingTransport::new().with_session_present(false);
    start_agent_with_transport(
        &start_ws,
        &AgentId::new("worker_a"),
        true,
        false,
        true,
        None,
        &start_transport,
    )
    .expect("start-agent fixture should spawn worker");
    let start_spawn = start_transport.only_spawn("start-agent fresh");
    failures.extend(codex_bypass_failures(&start_spawn.argv, "AI-1 start-agent"));
    assert_bypass_env(&start_spawn, "AI-1 start-agent fresh");

    let missing_ws = runtime_workspace("start-missing", &[("worker_a", "Worker A")]);
    seed_agent_state(&missing_ws, "worker_a", "Worker A", true);
    std::fs::remove_file(missing_ws.join("worker_a-rollout.jsonl")).unwrap();
    let missing_transport = RecordingTransport::new().with_session_present(false);
    let missing_outcome = start_agent_with_transport(
        &missing_ws,
        &AgentId::new("worker_a"),
        true,
        false,
        true,
        None,
        &missing_transport,
    )
    .expect("missing rollout with allow-fresh should spawn worker");
    assert!(matches!(
        missing_outcome,
        StartAgentOutcome::Running {
            start_mode: StartMode::FreshAfterMissingRollout,
            ..
        }
    ));
    assert_bypass_env(
        &missing_transport.only_spawn("start-agent fresh after missing rollout"),
        "AI-1 start-agent fresh after missing rollout",
    );

    let reset_team = team_dir_with_running_workspace("reset-inherited");
    let reset_ws = team_workspace(&reset_team);
    let reset_transport = RecordingTransport::new().with_session_present(false);
    reset_agent_with_transport(
        &reset_ws,
        &AgentId::new("worker_a"),
        true,
        false,
        None,
        &reset_transport,
    )
    .expect("reset fixture should spawn worker");
    assert_bypass_env(
        &reset_transport.only_spawn("reset-agent fresh"),
        "AI-1 reset-agent fresh",
    );
    assert_persisted_bypass_policy(&reset_ws, "worker_a", "AI-1 reset-agent fresh");

    let add_team = team_dir_with_running_workspace("add-inherited");
    let add_role = add_team.join("worker_b.md");
    std::fs::write(&add_role, role_doc("worker_b", "Worker B")).unwrap();
    let add_transport = RecordingTransport::new().with_session_present(true);
    add_agent_with_transport(
        &add_team,
        &AgentId::new("worker_b"),
        &add_role,
        false,
        None,
        &add_transport,
    )
    .expect("add-agent fixture should spawn worker");
    let add_spawn = add_transport.only_spawn("add-agent");
    failures.extend(codex_bypass_failures(&add_spawn.argv, "AI-1 add-agent"));
    assert_bypass_env(&add_spawn, "AI-1 add-agent");

    let fork_team = team_dir_with_forkable_source("fork-inherited");
    let fork_transport = RecordingTransport::new().with_session_present(true);
    fork_agent_with_transport(
        &fork_team,
        &AgentId::new("worker_a"),
        &AgentId::new("worker_fork"),
        None,
        false,
        None,
        &fork_transport,
    )
    .expect("fork fixture should spawn worker");
    let fork_spawn = fork_transport.only_spawn("fork-agent");
    failures.extend(codex_bypass_failures(&fork_spawn.argv, "AI-1 fork"));
    assert_bypass_env(&fork_spawn, "AI-1 fork");

    assert!(
        failures.is_empty(),
        "inherited bypass must be preserved across restart/start/add/fork:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn add_agent_preserves_restricted_default_and_fork_preserves_restricted_origin() {
    let _ancestry = MockAncestry::restricted();

    let add_team = team_dir_with_running_workspace("add-restricted");
    let add_role = add_team.join("worker_b.md");
    std::fs::write(&add_role, role_doc("worker_b", "Worker B")).unwrap();
    let add_transport = RecordingTransport::new().with_session_present(true);
    add_agent_with_transport(
        &add_team,
        &AgentId::new("worker_b"),
        &add_role,
        false,
        None,
        &add_transport,
    )
    .expect("add-agent restricted fixture should spawn worker");
    assert_codex_restricted(
        &add_transport.only_spawn_argv("restricted add-agent"),
        "add-agent restricted default",
    );

    let fork_team = team_dir_with_forkable_source("fork-restricted");
    let fork_transport = RecordingTransport::new().with_session_present(true);
    fork_agent_with_transport(
        &fork_team,
        &AgentId::new("worker_a"),
        &AgentId::new("worker_fork"),
        None,
        false,
        None,
        &fork_transport,
    )
    .expect("fork restricted fixture should spawn worker");
    assert_codex_restricted(
        &fork_transport.only_spawn_argv("restricted fork-agent"),
        "fork preserves restricted origin",
    );

    let reset_team = team_dir_with_running_workspace("reset-restricted");
    let reset_ws = team_workspace(&reset_team);
    let reset_transport = RecordingTransport::new().with_session_present(false);
    reset_agent_with_transport(
        &reset_ws,
        &AgentId::new("worker_a"),
        true,
        false,
        None,
        &reset_transport,
    )
    .expect("restricted reset fixture should spawn worker");
    assert_restricted_env(
        &reset_transport.only_spawn("restricted reset-agent"),
        "reset-agent restricted default",
    );
}

#[test]
#[serial(env)]
fn approval_detection_uses_token_boundaries_and_warns_unexpected_binary() {
    let _substring =
        MockAncestry::raw(&["codex", &format!("--comment=literal-{CODEX_BYPASS}-inside")]);
    let detected = team_agent::lifecycle::detect_dangerous_approval()
        .expect("detect dangerous approval with substring fixture");
    assert!(
        !detected.enabled,
        "AI-4: unrelated substring must not trigger inheritance; detection must match argv tokens exactly, got {detected:?}"
    );

    let _unexpected = MockAncestry::raw(&["bash", CODEX_BYPASS]);
    let detected = team_agent::lifecycle::detect_dangerous_approval()
        .expect("detect dangerous approval with unexpected binary fixture");
    assert!(
        detected.enabled,
        "AI-5: wrapper/alias parity still allows inheritance"
    );
    assert_eq!(detected.provider.as_deref(), Some("codex"));
    assert!(
        detected
            .flag
            .as_deref()
            .is_some_and(|flag| flag == CODEX_BYPASS),
        "AI-5: detected flag should be recorded"
    );
    assert!(
        event_or_source_contains("dangerous_flag_in_unexpected_binary")
            && event_or_source_contains("ancestry_binary_name"),
        "AI-5: unexpected binary must emit dangerous_flag_in_unexpected_binary with ancestry_binary_name audit"
    );
}

#[test]
#[serial(env)]
fn prompt_handler_does_not_raise_worker_above_leader() {
    let provider_sources = source_tree("src/provider");
    let coordinator_sources = source_tree("src/coordinator");
    let all = format!("{provider_sources}\n{coordinator_sources}");
    let mut failures = Vec::new();

    if !all.contains("awaiting_human_confirm")
        && !all.contains("runtime_approval.blocked_by_leader_safety")
        && !all.contains("leader_restricted")
    {
        failures.push(
            "AI-7: restricted approval prompts must be surfaced as awaiting_human_confirm / blocked_by_leader_safety / leader_restricted, not auto-consented"
                .to_string(),
        );
    }
    let runtime_uses_approval_choice = coordinator_sources
        .contains("choose_internal_mcp_approval_choice")
        || source("src/provider/mod.rs").contains("choose_internal_mcp_approval_choice");
    if runtime_uses_approval_choice {
        if !coordinator_sources.contains("runtime_approval_policy_from_agent") {
            failures.push(
                "AI-7/#232: coordinator auto-answer path must read persisted per-agent approval policy via runtime_approval_policy_from_agent, not re-detect leader process ancestry"
                    .to_string(),
            );
        }
        if !coordinator_sources.contains("effective_approval_policy") {
            failures.push(
                "AI-7/#232: auto-answer gate must be based on state.agents[*].effective_approval_policy"
                    .to_string(),
            );
        }
        if !coordinator_sources.contains("auto_answer_allowed") {
            failures.push(
                "AI-7/#232: persisted policy must expose an auto_answer_allowed gate before approval_choice_keys/send_keys can run"
                    .to_string(),
            );
        }
        if coordinator_sources.contains("detect_dangerous_approval()") {
            failures.push(
                "AI-7/#232: coordinator approval handling must not call detect_dangerous_approval(); coordinator process ancestry is not the worker's approval source"
                    .to_string(),
            );
        }
    }
    if !provider_sources
        .contains("ApprovalKind::Command => Some(\"command_approval_requires_human\")")
    {
        failures.push(
            "AI-7/#232 C5: command approval must stay on the human-confirm path even when persisted policy allows MCP auto-approval"
                .to_string(),
        );
    }
    if all.contains("Automation is consent") {
        failures.push(
            "AI-2: automation is not consent; prompt handler must not encode the opposite policy"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "AI-7 prompt handler approval ceiling contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn approval_inheritance_static_guards_cover_ai_1_through_ai_9() {
    let launch = source("src/lifecycle/launch.rs");
    let adapter = source("src/provider/adapter.rs");
    let restart_common = source("src/lifecycle/restart/common.rs");
    let mut failures = Vec::new();

    if function_body(&launch, "detect_dangerous_approval")
        .is_some_and(|body| body.contains(".contains(") && !body.contains("split('\\0')"))
    {
        failures.push("AI-4: detect_dangerous_approval must not use raw cmdline contains without argv token splitting".to_string());
    }
    if !launch.contains("split('\\0')")
        && !launch.contains("argv_tokens")
        && !launch.contains(ANCESTRY_ENV)
    {
        failures.push(
            "AI-4: detect_dangerous_approval must expose argv-token parsing over NUL/argv tokens"
                .to_string(),
        );
    }
    if !launch.contains("dangerous_flag_in_unexpected_binary")
        || !launch.contains("ancestry_binary_name")
    {
        failures.push("AI-5: dangerous flag in non-provider binary must emit warning audit with ancestry_binary_name".to_string());
    }
    if !launch.contains("worker_capability_above_leader") {
        failures.push(
            "AI-6: RuntimeConfig override audit must include worker_capability_above_leader"
                .to_string(),
        );
    }
    if !launch.contains("effective_runtime_config") && !launch.contains("effective_worker") {
        failures.push("AI-8: worker spawn approval policy must be centralized in an effective runtime/config helper".to_string());
    }
    if !adapter.contains("dangerous_auto_approve") {
        failures
            .push("AI-1: Codex adapter must still have a bypass command-shape input".to_string());
    }
    for (surface, text) in [
        ("fresh launch", launch.as_str()),
        ("restart common", restart_common.as_str()),
        ("fork", launch.as_str()),
    ] {
        if text.contains("agent_tool_strings(agent)")
            && !text.contains("effective_runtime_config")
            && !text.contains("worker_tool_refs")
        {
            failures.push(format!(
                "AI-8: {surface} calls agent_tool_strings(agent) without centralized effective approval helper"
            ));
        }
    }
    let fork_body_without_comments = function_body(&launch, "fork_agent_with_transport")
        .map(strip_line_comments)
        .unwrap_or_default();
    if !(fork_body_without_comments.contains(".fork_plan(")
        || fork_body_without_comments.contains(".fork_with_context("))
    {
        failures.push("AI-1/AI-8: fork path must call fork_plan or fork_with_context in executable code, not bare fork or comment-only guard text".to_string());
    }

    assert!(
        failures.is_empty(),
        "MCP approval inheritance static guard failed:\n{}",
        failures.join("\n")
    );
}

fn assert_codex_bypass(argv: &[String], label: &str) {
    let failures = codex_bypass_failures(argv, label);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn assert_bypass_env(spawn: &RecordedSpawn, label: &str) {
    let expected = BTreeMap::from([
        ("TEAM_AGENT_LEADER_BYPASS", "1"),
        ("TEAM_AGENT_LEADER_BYPASS_SOURCE", "leader_process"),
        ("TEAM_AGENT_LEADER_BYPASS_PROVIDER", "codex"),
        ("TEAM_AGENT_LEADER_BYPASS_FLAG", CODEX_BYPASS),
        ("TEAM_AGENT_MCP_AUTO_APPROVE", "team_orchestrator"),
        ("TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE", "leader_bypass"),
    ]);
    for (key, value) in expected {
        assert_eq!(
            spawn.env.get(key).map(String::as_str),
            Some(value),
            "{label}: final worker env must carry {key}={value}; spawn={spawn:?}"
        );
        assert!(
            !spawn.env_unset.iter().any(|unset| unset == key),
            "{label}: final env_unset must not erase {key}; spawn={spawn:?}"
        );
    }
}

fn assert_restricted_env(spawn: &RecordedSpawn, label: &str) {
    assert_eq!(
        spawn
            .env
            .get("TEAM_AGENT_LEADER_BYPASS")
            .map(String::as_str),
        Some("0"),
        "{label}: restricted worker must carry explicit bypass=0; spawn={spawn:?}"
    );
    for key in &APPROVAL_ENV_KEYS[1..] {
        assert!(
            !spawn.env.contains_key(*key),
            "{label}: restricted worker must not carry {key}; spawn={spawn:?}"
        );
    }
}

fn assert_persisted_bypass_policy(workspace: &Path, agent_id: &str, label: &str) {
    let state = team_agent::state::persist::load_runtime_state(workspace).unwrap();
    let policy = &state["agents"][agent_id]["effective_approval_policy"];
    assert_eq!(policy["enabled"], true, "{label}: policy={policy}");
    assert_eq!(
        policy["source"], "leader_process",
        "{label}: policy={policy}"
    );
    assert_eq!(policy["inherited"], true, "{label}: policy={policy}");
}

fn codex_bypass_failures(argv: &[String], label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if !argv.iter().any(|arg| arg == CODEX_BYPASS) {
        failures.push(format!(
            "{label}: Codex worker argv must inherit bypass flag; argv={argv:?}"
        ));
    }
    if argv
        .iter()
        .any(|arg| arg == "--sandbox" || arg == "--ask-for-approval")
    {
        failures.push(format!(
            "{label}: bypass argv must not also include restricted sandbox/approval flags; argv={argv:?}"
        ));
    }
    if !argv.iter().any(|arg| arg == "codex") {
        failures.push(format!(
            "{label}: provider executable must remain user PATH command name `codex`; argv={argv:?}"
        ));
    }
    if argv.iter().any(|arg| arg.ends_with("/codex")) {
        failures.push(format!(
            "{label}: Codex executable must not be hard-coded as an absolute path; argv={argv:?}"
        ));
    }
    failures
}

fn assert_codex_restricted(argv: &[String], label: &str) {
    assert!(
        has_adjacent(argv, &["--sandbox", "read-only"])
            || has_adjacent(argv, &["--sandbox", "workspace-write"]),
        "{label}: restricted Codex worker argv must include sandbox mode; argv={argv:?}"
    );
    assert!(
        has_adjacent(argv, &["--ask-for-approval", "on-request"]),
        "{label}: restricted Codex worker argv must ask on-request; argv={argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == CODEX_BYPASS),
        "{label}: restricted Codex worker argv must not contain bypass flag; argv={argv:?}"
    );
    assert_codex_command_name(argv, label);
}

fn assert_codex_command_name(argv: &[String], label: &str) {
    assert!(
        argv.iter().any(|arg| arg == "codex"),
        "{label}: provider executable must remain user PATH command name `codex`; argv={argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg.ends_with("/codex")),
        "{label}: Codex executable must not be hard-coded as an absolute path; argv={argv:?}"
    );
}

fn has_adjacent(argv: &[String], needle: &[&str]) -> bool {
    argv.windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| a == b))
}

fn launch_safety_json(report: &LaunchReport) -> String {
    serde_json::to_string(&report.safety).unwrap()
}

fn compiled_team_dir(tag: &str, dangerous: bool, agents: &[(&str, &str)]) -> PathBuf {
    let team = tmp_dir(tag).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {tag}\nobjective: Approval mirror contract.\nprovider: codex\ndangerous_auto_approve: {dangerous}\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    for (name, role) in agents {
        std::fs::write(
            team.join("agents").join(format!("{name}.md")),
            role_doc(name, role),
        )
        .unwrap();
    }
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    seed_healthy_coordinator(&team_workspace(&team));
    team
}

fn runtime_workspace(tag: &str, agents: &[(&str, &str)]) -> PathBuf {
    compiled_team_dir(tag, false, agents)
}

fn team_workspace(team: &Path) -> PathBuf {
    team.parent().unwrap().to_path_buf()
}

fn team_dir_with_running_workspace(tag: &str) -> PathBuf {
    let team = compiled_team_dir(tag, false, &[("worker_a", "Worker A")]);
    let ws = team_workspace(&team);
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": tag,
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": format!("team-{tag}"),
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": "codex",
                    "auth_mode": "subscription",
                    "role": "Worker A",
                    "window": "worker_a",
                    "owner_team_id": tag,
                    "tools": ["mcp_team"]
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    team
}

fn team_dir_with_forkable_source(tag: &str) -> PathBuf {
    let team = compiled_team_dir(tag, false, &[("worker_a", "Worker A")]);
    let ws = team_workspace(&team);
    let rollout = ws.join("worker_a-rollout.jsonl");
    std::fs::write(&rollout, "{\"session_id\":\"sess-worker-a\"}\n").unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": tag,
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": format!("team-{tag}"),
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": "codex",
                    "auth_mode": "subscription",
                    "role": "Worker A",
                    "window": "worker_a",
                    "owner_team_id": tag,
                    "session_id": "sess-worker-a",
                    "rollout_path": rollout.to_string_lossy().to_string(),
                    // 0.4.6 tuple-atomic contract: fork requires complete
                    // source tuple. Seed captured_at + captured_via so the
                    // fork-source backing guard passes and the test
                    // exercises the inheritance behaviour being asserted.
                    "captured_at": "2026-06-25T10:00:00+00:00",
                    "captured_via": "session.captured",
                    "tools": ["mcp_team"]
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    team
}

fn seed_agent_state(workspace: &Path, agent_id: &str, role: &str, resumable: bool) {
    let rollout = workspace.join(format!("{agent_id}-rollout.jsonl"));
    if resumable {
        std::fs::write(&rollout, "{\"session_id\":\"sess-worker-a\"}\n").unwrap();
    }
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "team-a",
            "team_dir": workspace.to_string_lossy().to_string(),
            "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-approval",
            "agents": {
                agent_id: {
                    "status": "running",
                    "provider": "codex",
                    "auth_mode": "subscription",
                    "role": role,
                    "window": agent_id,
                    "owner_team_id": "team-a",
                    "session_id": if resumable { "sess-worker-a" } else { "" },
                    "rollout_path": if resumable { rollout.to_string_lossy().to_string() } else { String::new() },
                    // 0.4.6 tuple-atomic contract: when resumable, seed the
                    // full authoritative tuple so fork/restart can probe a
                    // real backing.
                    "captured_at": if resumable { "2026-06-25T10:00:00+00:00" } else { "" },
                    "captured_via": if resumable { "session.captured" } else { "" },
                    "first_send_at": "2026-06-05T09:00:00+00:00",
                    "tools": ["mcp_team"]
                }
            }
        }),
    )
    .unwrap();
    std::fs::write(workspace.join("team.spec.yaml"), "name: team-a\n").unwrap();
    seed_healthy_coordinator(workspace);
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

fn role_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

fn source(rel: &str) -> String {
    composite_source::composite_source(rel)
}

fn source_tree(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = String::new();
    append_rs_sources(&root, &mut out);
    out
}

fn append_rs_sources(path: &Path, out: &mut String) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            append_rs_sources(&entry.unwrap().path(), out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) == Some("rs") {
        out.push_str(&std::fs::read_to_string(path).unwrap());
        out.push('\n');
    }
}

fn function_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let start = source.find(&format!("fn {name}"))?;
    let after = &source[start..];
    let open = after.find('{')?;
    let mut depth = 0usize;
    for (idx, ch) in after[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return after.get(open + 1..open + idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn event_or_source_contains(needle: &str) -> bool {
    source_tree("src").contains(needle)
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-codex-approval-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct MockAncestry {
    previous: Option<String>,
}

impl MockAncestry {
    fn codex_bypass() -> Self {
        Self::raw(&["codex", CODEX_BYPASS])
    }

    fn restricted() -> Self {
        Self::raw(&[
            "codex",
            "--sandbox",
            "read-only",
            "--ask-for-approval",
            "on-request",
        ])
    }

    fn raw(argv: &[&str]) -> Self {
        let previous = std::env::var(ANCESTRY_ENV).ok();
        unsafe {
            std::env::set_var(ANCESTRY_ENV, serde_json::to_string(argv).unwrap());
        }
        Self { previous }
    }
}

impl Drop for MockAncestry {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(ANCESTRY_ENV, previous);
            } else {
                std::env::remove_var(ANCESTRY_ENV);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    kind: &'static str,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    env_unset: Vec<String>,
    session: SessionName,
    window: WindowName,
    pane_id: PaneId,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: bool,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn only_spawn_argv(&self, label: &str) -> Vec<String> {
        self.only_spawn(label).argv
    }

    fn only_spawn(&self, label: &str) -> RecordedSpawn {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(
            spawns.len(),
            1,
            "{label}: expected exactly one spawn; spawns={spawns:?}"
        );
        let _kind = spawns[0].kind;
        spawns[0].clone()
    }

    fn record_spawn(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        let pane_id = PaneId::new(format!("%{}", spawns.len() + 1));
        spawns.push(RecordedSpawn {
            kind,
            argv: argv.to_vec(),
            env: env.clone(),
            env_unset: env_unset.to_vec(),
            session: session.clone(),
            window: window.clone(),
            pane_id: pane_id.clone(),
        });
        SpawnResult {
            pane_id,
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
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_first", session, window, argv, env, &[]))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_into", session, window, argv, env, &[]))
    }

    fn spawn_first_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_first", session, window, argv, env, env_unset))
    }

    fn spawn_into_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_into", session, window, argv, env, env_unset))
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
            text: "OpenAI Codex\ncodex>".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
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
        Ok(self
            .spawns
            .lock()
            .unwrap()
            .iter()
            .map(|spawn| PaneInfo {
                pane_id: spawn.pane_id.clone(),
                session: spawn.session.clone(),
                window_index: None,
                window_name: Some(spawn.window.clone()),
                pane_index: None,
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            })
            .collect())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present || !self.spawns.lock().unwrap().is_empty())
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
