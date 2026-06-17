//! Copilot provider contracts (command-substitution tier, zero real sessions).
//!
//! Basis docs (sole truth sources — where the in-tree implementation deviates, these
//! contracts stay red):
//! - `.team/artifacts/copilot-provider-design.md` (fable-architect; §C1 argv shape,
//!   §C2 instructions degradation, §C5 permission mapping, §E test architecture)
//! - `.team/artifacts/copilot-provider-cr-verdict.md` (constitution-reviewer; 26
//!   constraints C-1-1..C-7-3, 22 reverse cases — the deterministic subset lands here;
//!   real-session-only cases (§E3: must4 identity self-report, runtime prompt gating,
//!   dangerous live verify, status_patterns sampling) stay on the user acceptance list).
//!
//! Python parity note: 0.2.11 ships copilot as an EXPLICIT unsupported plug
//! (provider_cli/copilot.py:6-8 -> ProviderCapabilityError), so there is no Python
//! behavior to diff against — the design doc IS the truth source (B2 lesson).
//!
//! Faces covered (leader task §1-7):
//!  1. worker argv golden (§C1 + C-5-1/3)        — copilot_argv_* tests
//!  2. per-worker AGENTS.md == compiled prompt    — copilot_agents_md_* tests (C-1-1/2,
//!     identity-first per the 0.3.4 B2/D6 lock)
//!  3. permission mapping (C-2-1, via the prompt's Permission note + argv deny flags)
//!  4. fork = structured CapabilityUnsupported (C-4-2/3)
//!  5. startup recognizer supports copilot trust/ready; turn-state Unknown never idles
//!     (N11, C-3-1/4)
//!  6. leader session naming falls under the B5 `team-agent-leader-` prefix protection
//!  7. session capture = sqlite point query, no directory traversal (PERF P2 lock)

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serial_test::serial;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::model::enums::Provider;
use team_agent::state::persist::load_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const ANCESTRY_KEY: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const NEUTRAL_ANCESTRY: &str = "[\"/bin/zsh\"]";

/// Face 1+3 (§C1 golden + C-5-3, verdict cases `copilot_argv_golden_per_role_tools`
/// and `non_dangerous_argv_contains_deny_flags_no_allow_all`): a restricted copilot
/// worker (mcp_team only) gets the full §C1 argv — noise control, inline/`@file` MCP
/// config carrying team_orchestrator, a pre-assigned `--session-id` equal to the
/// persisted `_pending_session_id`, `-C <workspace>`, deny flags for the missing
/// capabilities, and NO --allow-all family flag.
#[test]
#[serial(env)]
fn copilot_argv_golden_restricted_role() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("argv-restricted");
    let team_dir = write_copilot_team(&ws, "cp-restricted", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cprestricted"), &transport)
        .expect("quick-start should reach copilot command construction");
    let spawn = transport.single_spawn();
    let argv = &spawn.argv;
    let mut failures = Vec::new();

    if argv.first().map(String::as_str) != Some("copilot") {
        failures.push(format!("§C1: argv[0] must be `copilot`; argv={argv:?}"));
    }
    // RC-2 (C-1-2 v2): noise control now includes --no-remote (remote control off).
    for noise in ["--no-color", "--no-auto-update", "--no-remote"] {
        if !argv.iter().any(|arg| arg == noise) {
            failures.push(format!("RC-2/C-1-2: argv must contain {noise}"));
        }
    }
    // RC-6 (C-3-1): worker must disable the built-in github-mcp-server.
    if !argv.iter().any(|arg| arg == "--disable-builtin-mcps") {
        failures.push("RC-6/C-3-1: argv must carry --disable-builtin-mcps".to_string());
    }
    // RC-21 (C-7-1): session named by agent id (resume-by-name dividend).
    if flag_value(argv, "-n").as_deref() != Some("worker_a")
        && flag_value(argv, "--name").as_deref() != Some("worker_a")
    {
        failures.push(format!(
            "RC-21/C-7-1: argv must name the session `-n worker_a`; got {:?}",
            flag_value(argv, "-n")
        ));
    }
    // RC-20 (C-6-2): per-worker directed log dir + info level (idle aux source).
    let log_dir = flag_value(argv, "--log-dir").unwrap_or_default();
    if !(log_dir.contains(".team/logs/copilot") && log_dir.contains("worker_a")) {
        failures.push(format!(
            "RC-20/C-6-2: argv must carry --log-dir <ws>/.team/logs/copilot/<agent_id>; got {log_dir:?}"
        ));
    }
    if flag_value(argv, "--log-level").as_deref() != Some("info") {
        failures.push(format!(
            "RC-20/C-6-2: argv must carry --log-level info; got {:?}",
            flag_value(argv, "--log-level")
        ));
    }
    // RC-1/RC-16/C-5-8: forbidden flags must never appear.
    for forbidden in ["-i", "--interactive", "-p", "--share", "--share-gist", "--no-ask-user", "--yolo"] {
        if argv.iter().any(|arg| arg == forbidden) {
            failures.push(format!("RC-1/RC-16: forbidden flag {forbidden} in argv"));
        }
    }
    // MCP: --additional-mcp-config with either inline {"mcpServers":...} JSON or the
    // @file form pointing at the per-agent .team/runtime/mcp/<agent>.json (§C1 note).
    match flag_value(argv, "--additional-mcp-config") {
        None => failures.push("§C1: argv must carry --additional-mcp-config".to_string()),
        Some(value) => {
            let inline_ok = value.contains("mcpServers") && value.contains("team_orchestrator");
            let file_ok = value.starts_with('@') && value.contains(".team/runtime/mcp/worker_a.json");
            if !(inline_ok || file_ok) {
                failures.push(format!(
                    "§C1: --additional-mcp-config must be inline mcpServers JSON with \
team_orchestrator or @<.team/runtime/mcp/worker_a.json>; got {value:?}"
                ));
            }
        }
    }
    // Session pre-assignment (capture without scans, §C4): --session-id == persisted
    // _pending_session_id.
    let state = load_runtime_state(&ws).expect("state after quick-start");
    let pending = state
        .pointer("/agents/worker_a/_pending_session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match (flag_value(argv, "--session-id"), pending) {
        (Some(argv_sid), Some(state_sid)) if argv_sid == state_sid => {}
        (argv_sid, state_sid) => failures.push(format!(
            "§C4: --session-id must be pre-assigned and equal state._pending_session_id \
(claude --session-id parity); argv={argv_sid:?} state={state_sid:?}"
        )),
    }
    if flag_value(argv, "-C").as_deref() != Some(&*ws.to_string_lossy()) {
        failures.push(format!(
            "§C1: argv must pin the workspace via `-C <workspace>`; got {:?}",
            flag_value(argv, "-C")
        ));
    }
    // C-5-3: restricted role (no execute_bash / fs_write / network) → deny flags, and
    // no --allow-all family flag anywhere.
    let deny_tools = flag_values(argv, "--deny-tool");
    for denied in ["shell", "write"] {
        if !deny_tools.iter().any(|tool| tool.contains(denied)) {
            failures.push(format!(
                "C-5-3: restricted copilot worker must carry --deny-tool '{denied}'; deny={deny_tools:?}"
            ));
        }
    }
    // RC-15 (C-5-2 v2): network deny is `--deny-tool 'url'` (the help has only shell/
    // write/mcp/url tool kinds; v1's --deny-url is not the v2 form).
    if !deny_tools.iter().any(|tool| tool.contains("url")) {
        failures.push(format!(
            "RC-15/C-5-2: restricted copilot worker must deny network via --deny-tool 'url'; deny={deny_tools:?}"
        ));
    }
    // RC-10 (C-3-5/C-5-2): mcp_team is allowlisted by server name (no approval prompt).
    let allow_tools = flag_values(argv, "--allow-tool");
    if !allow_tools.iter().any(|tool| tool.contains("team_orchestrator")) {
        failures.push(format!(
            "RC-10/C-3-5: argv must carry --allow-tool 'team_orchestrator'; allow={allow_tools:?}"
        ));
    }
    if argv.iter().any(|arg| arg == "--allow-all" || arg == "--allow-all-tools") {
        failures.push(format!(
            "C-5-3: a non-dangerous worker must NOT carry --allow-all/--allow-all-tools; argv={argv:?}"
        ));
    }
    // C-1-5 (verdict `fallback_options_rejected`): the rejected fallbacks must not
    // appear — no `-i <prompt>` first-message injection, no --no-custom-instructions,
    // and the compiled system prompt text must NOT ride in the argv (it travels via
    // the instructions file + env, B2 degradation path).
    if argv.iter().any(|arg| arg == "-i" || arg == "--interactive" || arg == "--no-custom-instructions") {
        failures.push(format!(
            "C-1-5: rejected fallback flag present (-i/--interactive/--no-custom-instructions); argv={argv:?}"
        ));
    }
    if argv.iter().any(|arg| arg.contains("Team Agent Teammate Runtime Contract")) {
        failures.push("C-1-5/B2: the system prompt must not be smuggled into the argv".to_string());
    }
    assert!(
        failures.is_empty(),
        "copilot restricted argv golden failed:\n{}\nargv={argv:?}",
        failures.join("\n")
    );
}

/// Face 1 dangerous tier (C-5-1, verdict `dangerous_auto_approve_argv_contains_allow_all`):
/// dangerous_auto_approve must map to `--allow-all` (claude --dangerously-skip-permissions
/// parity, paths+urls included) — NOT the silently-narrower `--allow-all-tools`.
#[test]
#[serial(env)]
fn copilot_argv_dangerous_maps_to_allow_all() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("argv-dangerous");
    let team_dir = write_copilot_team(&ws, "cp-dangerous", &["mcp_team"], true);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpdangerous"), &transport)
        .expect("quick-start should reach copilot command construction");
    let argv = &transport.single_spawn().argv;

    let mut failures = Vec::new();
    if !argv.iter().any(|arg| arg == "--allow-all") {
        failures.push(format!(
            "C-5-1: dangerous_auto_approve must produce `--allow-all` (=--yolo, claude \
parity incl. paths+urls); argv={argv:?}"
        ));
    }
    if argv.iter().any(|arg| arg == "--allow-all-tools") {
        failures.push(format!(
            "C-5-1: `--allow-all-tools` is the silently-narrower flag and must not be \
used for the dangerous tier; argv={argv:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "copilot dangerous argv contract failed:\n{}",
        failures.join("\n")
    );
}

/// Face 2 (C-1-1/2, C-6-1..4; verdict `copilot_instructions_dir_env_injected`,
/// `agents_md_content_equals_compile_worker_system_prompt`,
/// `copilot_instructions_dir_path_contains_agent_id`,
/// `copilot_env_overlay_works_without_profile`,
/// `agents_md_per_agent_isolated_no_global_pollution`):
/// the spawn env must carry COPILOT_CUSTOM_INSTRUCTIONS_DIRS pointing at the
/// per-worker dir (path contains the agent id; works with NO profile configured), the
/// AGENTS.md there must BE the compiled worker system prompt (single source: identity
/// first, runtime contract, role body, output contract, permission note — 0.3.4 B2/D6
/// lock), and no global ~/.copilot/AGENTS.md may be written.
#[test]
#[serial(env)]
fn copilot_agents_md_is_the_compiled_prompt_and_env_points_at_it() {
    let home = tmp_ws("fake-home");
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("HOME", Box::leak(home.to_string_lossy().to_string().into_boxed_str())),
    ]);
    let ws = tmp_ws("agents-md");
    let team_dir = write_copilot_team(&ws, "cp-instructions", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpinstr"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let spawn = transport.single_spawn();

    let mut failures = Vec::new();
    let dir = spawn
        .env
        .get("COPILOT_CUSTOM_INSTRUCTIONS_DIRS")
        .cloned()
        .unwrap_or_default();
    if dir.is_empty() {
        failures.push(
            "C-6-1: spawn env must carry COPILOT_CUSTOM_INSTRUCTIONS_DIRS (env overlay, \
no profile required)"
                .to_string(),
        );
    }
    if !dir.contains("worker_a") {
        failures.push(format!(
            "C-6-2/4: the instructions dir must be per-agent (path contains the agent \
id); got {dir:?}"
        ));
    }
    if !dir.contains(".team/runtime/copilot-instructions") {
        failures.push(format!(
            "C-6-2: the instructions dir must live under \
<workspace>/.team/runtime/copilot-instructions/<agent_id>/; got {dir:?}"
        ));
    }
    let agents_md = std::fs::read_to_string(Path::new(&dir).join("AGENTS.md")).unwrap_or_default();
    if agents_md.is_empty() {
        failures.push(format!("C-1-1: per-worker AGENTS.md must exist in {dir:?}"));
    } else {
        let identity = "You are Team Agent worker `worker_a` with role `Copilot Worker`. \
When asked about your role or identity, answer with this Team Agent worker identity first, \
not only the generic provider product identity.";
        if !agents_md.starts_with(identity) {
            failures.push(format!(
                "C-1-1 + B2/D6 lock: AGENTS.md must BE compile_worker_system_prompt output \
with the identity section FIRST; head={:?}",
                agents_md.chars().take(160).collect::<String>()
            ));
        }
        for marker in [
            "# Team Agent Teammate Runtime Contract",
            "COPILOT ROLE BODY SENTINEL",
            "report_result exactly once",
            "Permission note: these tools are prompt-only for this provider and not hard-enforced:",
        ] {
            if !agents_md.contains(marker) {
                failures.push(format!(
                    "C-1-1: AGENTS.md is missing the compiled prompt section marker {marker:?}"
                ));
            }
        }
    }
    if home.join(".copilot/AGENTS.md").exists() {
        failures.push(
            "C-1-2/C-6-4: the framework must never write the GLOBAL ~/.copilot/AGENTS.md \
(per-worker isolation only)"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "copilot AGENTS.md/env contract failed:\n{}\nenv={:?}",
        failures.join("\n"),
        spawn.env.get("COPILOT_CUSTOM_INSTRUCTIONS_DIRS")
    );
}

/// Face 3 (C-2-1, verdict `permission_enforcement_table_copilot_row_prompt_only_for_fs`):
/// the copilot enforcement row must honestly register the no-hard-deny tools as
/// prompt-only. Observable single-source: the compiled prompt's Permission note (built
/// from PROVIDER_ENFORCEMENT) for a full-capability role must list exactly
/// fs_list/fs_read/git_diff/provider_builtin (sorted).
#[test]
#[serial(env)]
fn copilot_permission_note_registers_fs_tools_prompt_only() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("enforcement");
    let team_dir = write_copilot_team(
        &ws,
        "cp-enforcement",
        &["execute_bash", "fs_read", "fs_write", "fs_list", "git_diff", "mcp_team", "provider_builtin"],
        false,
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpenforce"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let spawn = transport.single_spawn();
    let dir = spawn
        .env
        .get("COPILOT_CUSTOM_INSTRUCTIONS_DIRS")
        .cloned()
        .unwrap_or_default();
    let agents_md = std::fs::read_to_string(Path::new(&dir).join("AGENTS.md")).unwrap_or_default();

    let note_line = agents_md
        .lines()
        .find(|line| line.starts_with("Permission note:"))
        .unwrap_or("")
        .to_string();
    assert!(
        note_line.contains("fs_list, fs_read, git_diff, provider_builtin"),
        "C-2-1: the copilot enforcement row must register exactly \
fs_read/fs_list/git_diff/provider_builtin as prompt-only (sorted in the Permission \
note; execute_bash/fs_write/network/mcp_team are hard); note={note_line:?} \
agents_md_present={}",
        !agents_md.is_empty()
    );
}

/// Face 4 (C-4-2/3, verdict `fork_agent_copilot_returns_capability_unsupported`):
/// forking a copilot worker must return a STRUCTURED capability error naming the
/// provider and the missing capability — never a silent fallback to a fresh restart.
#[test]
#[serial(env)]
fn copilot_fork_returns_structured_capability_unsupported() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("fork");
    let team_dir = write_copilot_team(&ws, "cp-fork", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpfork"), &transport)
        .expect("quick-start should seed the team");
    // Give the source a session id so the capability gate (not a missing-session gate)
    // is what fires.
    seed_session_id(&ws, "worker_a");

    let result = team_agent::lifecycle::launch::fork_agent_with_transport(
        &team_dir,
        &team_agent::model::ids::AgentId::new("worker_a"),
        &team_agent::model::ids::AgentId::new("worker_fork"),
        None,
        false,
        None,
        &RecordingTransport::new(),
    );

    let text = format!("{result:?}");
    let mut failures = Vec::new();
    if result.is_ok() {
        failures.push(format!(
            "C-4-3: copilot fork must be refused (no silent fallback to a fresh spawn); got Ok: {text}"
        ));
    }
    if !(text.contains("copilot") && text.contains("fork")) {
        failures.push(format!(
            "C-4-2: the refusal must be structured — naming provider `copilot` and the \
missing `fork` capability (Python unsupported-plug parity); got {text}"
        ));
    }
    assert!(
        failures.is_empty(),
        "copilot fork capability contract failed:\n{}",
        failures.join("\n")
    );
}

/// Face 5 (B12 + C-3-1/4 + N11): copilot startup trust/ready recognition is
/// supported from the fixture-backed recognizer, so a real tick over a copilot
/// worker must no longer emit the old `provider.classify.unsupported` event or
/// disturb an attached leader receiver. Turn-state facts remain absent for phase
/// 1, and an unknown copilot node still blocks idle take-over (unknown ≠ idle).
#[test]
#[serial(env)]
fn copilot_phase1_classify_is_explicit_unknown_and_never_idles() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let mut failures = Vec::new();

    // C-3-1: no turn-state facts for copilot, even over an error-shaped record.
    let fact = team_agent::provider::latest_explicit_error_fact(
        Provider::Copilot,
        "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
    );
    if fact.is_some() {
        failures.push(format!(
            "C-3-1: phase-1 copilot must not claim turn-state facts (classify -> None/Unknown); got {fact:?}"
        ));
    }

    if team_agent::provider::classify_copilot_startup_screen(
        team_agent::provider::COPILOT_TRUST_PROMPT_MARKER,
    ) != team_agent::provider::StartupScreenDecision::AnswerWorkspaceTrust
    {
        failures.push("B12: copilot trust fixture must be recognized as actionable".to_string());
    }
    if team_agent::provider::classify_copilot_startup_screen(
        team_agent::provider::COPILOT_READY_MARKER,
    ) != team_agent::provider::StartupScreenDecision::Ready
    {
        failures.push("B12: copilot ready fixture must be recognized as ready".to_string());
    }

    // C-3-4/B12: the old unsupported event was a phase-1 placeholder. Once the
    // fixture-backed recognizer exists, a real tick over a copilot worker must
    // not re-emit it or poison leader receiver attachment.
    let ws = tmp_ws("classify");
    let rollout = ws.join("rollout-cp1.jsonl");
    std::fs::write(&rollout, "{\"junk\":true}\n").unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-cp",
            "active_team_key": "team-cp",
            "agents": {
                "cp1": {
                    "status": "running", "provider": "copilot", "agent_id": "cp1",
                    "window": "cp1", "pane_id": "%41",
                    "session_id": "88888888-9999-4aaa-8bbb-cccccccccccc",
                    "rollout_path": rollout.to_string_lossy(),
                    "spawn_cwd": ws.to_string_lossy(),
                },
            },
            "leader_receiver": {
                "status": "attached",
                "mode": "direct_tmux",
                "pane_id": "%leader",
                "provider": "copilot",
                "tmux_socket": "default",
            },
        }),
    )
    .unwrap();
    let coord = team_agent::coordinator::Coordinator::new(
        team_agent::coordinator::WorkspacePath::new(ws.clone()),
        Box::new(RealAdapterRegistry),
        Box::new(RecordingTransport::with_session_present()),
    );
    let _ = coord.tick();
    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default();
    let unsupported_line = events
        .lines()
        .find(|line| line.contains("classify") && line.contains("unsupported"));
    if let Some(line) = unsupported_line {
        failures.push(format!(
            "B12/C-3-4: copilot startup classify is supported; tick must not emit the old \
provider.classify.unsupported placeholder event; line={line}"
        ));
    }
    let after = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    if after
        .pointer("/leader_receiver/status")
        .and_then(serde_json::Value::as_str)
        != Some("attached")
    {
        failures.push(format!(
            "B12: copilot leader_receiver must remain attached after tick; events tail={} state={after}",
            events.lines().rev().take(4).collect::<Vec<_>>().join(" | ")
        ));
    }

    // N11: an unknown copilot node must BLOCK the take-over ping.
    let nodes = vec![serde_json::json!({"node_id": "cp1", "role": "worker", "state": "unknown"})];
    let reminder = team_agent::provider::evaluate_takeover_reminder(&nodes, None, 100.0, 60.0)
        .expect("evaluate ok");
    if reminder.should_ping {
        failures.push("N11: an Unknown copilot worker must never be counted as idle".to_string());
    }
    assert!(
        failures.is_empty(),
        "copilot phase-1 classify contract failed:\n{}",
        failures.join("\n")
    );
}

struct RealAdapterRegistry;

impl team_agent::coordinator::ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn team_agent::provider::ProviderAdapter> {
        team_agent::provider::get_adapter(provider)
    }
    fn error_lists(&self, _provider: Provider) -> team_agent::coordinator::ErrorLists {
        team_agent::coordinator::ErrorLists { whitelist: Vec::new(), blacklist: Vec::new() }
    }
}

/// Face 6 (verdict `copilot_leader_session_name_prefix_protected_by_b5`): the
/// `team-agent copilot` leader entry derives its session from the REAL
/// leader_session_name generator, so the name carries the `team-agent-leader-` prefix
/// that the B5 shutdown protection keys on (pure-function tier; the behavioral
/// shutdown survival run lives with the B5 suite on the lane carrying c92b716).
#[test]
fn copilot_leader_session_name_carries_b5_protected_prefix() {
    let ws = tmp_ws("leader-name");
    let name = team_agent::leader::leader_session_name(Provider::Copilot, &ws);
    assert!(
        name.as_str().starts_with("team-agent-leader-copilot-"),
        "the copilot leader session must be named team-agent-leader-copilot-<folder>-<hash> \
(leader/start.rs naming; the B5 kill_server/protection sparing keys on the \
team-agent-leader- prefix); got {}",
        name.as_str()
    );
}

/// Face 7 (PERF P2 lock; design §B session capture row + §C4): copilot session capture
/// must be a sqlite point query against ~/.copilot/session-store.db — no recursive
/// directory traversal of the copilot home. A poisoned (invalid UTF-8) decoy file in
/// the home must not break the capture, and the matching sessions row (cwd ==
/// spawn_cwd) must be found.
#[test]
#[serial(env)]
fn copilot_session_capture_is_sqlite_point_query_not_directory_scan() {
    let home = tmp_ws("capture-home");
    let _guard = EnvGuard::set(&[(
        "HOME",
        Box::leak(home.to_string_lossy().to_string().into_boxed_str()),
    )]);
    let spawn_cwd = tmp_ws("capture-cwd");
    let copilot_dir = home.join(".copilot");
    std::fs::create_dir_all(copilot_dir.join("logs")).unwrap();
    // Poisoned decoy: a whole-directory scanner would read this and fail/skip.
    let mut poison = b"{\"junk\":true}\n".to_vec();
    poison.extend_from_slice(&[0xFF, 0xFE, 0xFF]);
    std::fs::write(copilot_dir.join("logs").join("process-rollout-1.jsonl"), &poison).unwrap();
    let conn = rusqlite::Connection::open(copilot_dir.join("session-store.db")).unwrap();
    conn.execute_batch(
        "create table sessions (id text primary key, cwd text, created_at text, updated_at text);
         create table turns (session_id text, turn_index integer, user_message text, assistant_response text);",
    )
    .unwrap();
    conn.execute(
        "insert into sessions(id, cwd, created_at, updated_at) values (?1, ?2, '2026-06-10T00:00:00Z', '2026-06-10T00:00:01Z')",
        rusqlite::params![
            "66666666-7777-4888-9999-aaaaaaaaaaaa",
            spawn_cwd.to_string_lossy()
        ],
    )
    .unwrap();
    drop(conn);

    // 断言1(方法锁·PERF P2):expected-id 点查命中(==seed 的那行 id)+ poison decoy 保留 →
    // capture 返回含该 id 的 candidate。证 sqlite 点查命中而非扫目录被毒文件炸。
    // (E11 后点查路径是 `where id = expected`,故 expected 必须 == seed 的 session id。)
    let method_lock = team_agent::provider::get_adapter(Provider::Copilot)
        .capture_session_candidates(
            &team_agent::provider::CaptureSessionContext {
                agent_id: "worker_a".to_string(),
                spawn_cwd: spawn_cwd.clone(),
                pane_id: None,
                pane_pid: None,
                spawned_at: None,
                expected_session_id: Some(team_agent::provider::SessionId::new(
                    "66666666-7777-4888-9999-aaaaaaaaaaaa",
                )),
                provider_projects_root: None,
            },
            0,
        )
        .expect("copilot capture scan must not error on a poisoned home file (point query, not traversal)");
    assert!(
        method_lock.iter().any(|candidate| {
            candidate.captured.session_id.as_ref().map(|id| id.as_str())
                == Some("66666666-7777-4888-9999-aaaaaaaaaaaa")
        }),
        "PERF P2 lock: the session must come from the session-store.db sqlite point \
query (id == expected_session_id), not from scanning provider-home files; candidates={method_lock:?}"
    );

    // 断言2(安全锁·E11 语义):同 db、同 cwd,expected_session_id: None → 返回空。
    // no-expected 不 cwd-latest 猜(防 leader+worker 同 cwd 共享 db 时抓 leader 的 latest)。
    let safety_lock = team_agent::provider::get_adapter(Provider::Copilot)
        .capture_session_candidates(
            &team_agent::provider::CaptureSessionContext {
                agent_id: "worker_a".to_string(),
                spawn_cwd: spawn_cwd.clone(),
                pane_id: None,
                pane_pid: None,
                spawned_at: None,
                expected_session_id: None,
                provider_projects_root: None,
            },
            0,
        )
        .expect("copilot capture must not error with no expected id");
    assert!(
        safety_lock.is_empty(),
        "E11 safety lock: no expected_session_id → empty (no cwd-latest guess that could grab \
leader's session); candidates={safety_lock:?}"
    );
}

/// Q4 P0 (C-4-1/4/7, RC-11/RC-12): the worker spawn env MUST carry
/// `COPILOT_DISABLE_TERMINAL_TITLE=1` (updateTerminalTitle defaults to true and would
/// rewrite the tmux window name = an N39 addressing-anchor drift, same severity as the
/// B5 leader misfire). The worker must spawn into the stable adaptive layout window.
#[test]
#[serial(env)]
fn copilot_p0_terminal_title_disabled_and_window_name_stable() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("p0-title");
    let team_dir = write_copilot_team(&ws, "cp-title", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cptitle"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let spawn = transport.single_spawn();

    let mut failures = Vec::new();
    if spawn.env.get("COPILOT_DISABLE_TERMINAL_TITLE").map(String::as_str) != Some("1") {
        failures.push(format!(
            "RC-11/C-4-1 (P0): worker spawn env MUST set COPILOT_DISABLE_TERMINAL_TITLE=1 \
(updateTerminalTitle defaults true -> window-name rewrite = N39 addressing drift, B5-class \
incident); got {:?}",
            spawn.env.get("COPILOT_DISABLE_TERMINAL_TITLE")
        ));
    }
    // 0.3.28 Step 4b: 1-window-per-agent; window name = agent_id (`worker_a`),
    // not team-w1 (the pre-0.3.28 adaptive layout anchor).
    if spawn.window != "worker_a" {
        failures.push(format!(
            "0.3.28 Step 4b / RC-12: the worker window must be the per-agent \
             window named after agent_id (`worker_a`); got {:?}",
            spawn.window
        ));
    }
    assert!(
        failures.is_empty(),
        "copilot P0 terminal-title contract failed:\n{}\nenv_keys={:?}",
        failures.join("\n"),
        spawn.env.keys().collect::<Vec<_>>()
    );
}

/// Q3 (C-3-2/3/7, RC-7/RC-8): a user-global ~/.copilot/mcp-config.json carrying a
/// non-team_orchestrator server (`foo`) must be observed and named-disabled — the
/// worker argv must add `--disable-mcp-server foo`, a
/// `provider.copilot.mcp_residual_detected` event must land, and the residual file
/// must be written (no silent unbounded MCP surface).
#[test]
#[serial(env)]
fn copilot_mcp_residual_named_disabled_and_recorded() {
    let home = tmp_ws("mcp-home");
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("HOME", Box::leak(home.to_string_lossy().to_string().into_boxed_str())),
    ]);
    // Real-machine grounded: copilot 1.0.59 `mcp list` under this HOME reads
    // ~/.copilot/mcp-config.json and prints, under a "User servers:" section,
    // `  foo (local)`. The adapter's residual scan must turn that into a
    // --disable-mcp-server foo (verified the CLI output shape directly).
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    std::fs::write(
        home.join(".copilot/mcp-config.json"),
        "{\"mcpServers\":{\"foo\":{\"command\":\"foo-bin\"}}}",
    )
    .unwrap();
    // Hermetic shim (0.3.5 ship-gate fix): the residual scan execs the REAL `copilot`
    // binary, absent on CI runners — a PATH shim replays the te-verified 1.0.59
    // `mcp list` output so the parse+wiring contract is deterministic everywhere.
    let shim_dir = home.join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        shim_dir.join("copilot"),
        "#!/bin/sh\nif [ \"$1\" = \"mcp\" ] && [ \"$2\" = \"list\" ]; then\n  printf 'User servers:\\n  foo (local)\\n'\n  exit 0\nfi\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(shim_dir.join("copilot"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", shim_dir.to_string_lossy()));
    let ws = tmp_ws("mcp-residual");
    let team_dir = write_copilot_team(&ws, "cp-residual", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpresidual"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let spawn = transport.single_spawn();

    let mut failures = Vec::new();
    if flag_values(&spawn.argv, "--disable-mcp-server").iter().all(|name| name != "foo") {
        failures.push(format!(
            "RC-7/C-3-2: a user-global MCP server `foo` must be named-disabled \
(--disable-mcp-server foo); argv={:?}",
            spawn.argv
        ));
    }
    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default();
    if !events.contains("mcp_residual_detected") {
        failures.push(format!(
            "RC-8/C-3-3: a non-empty MCP residual must emit \
provider.copilot.mcp_residual_detected (user-visible, not silent); events tail={}",
            events.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
        ));
    }
    let residual_file = ws.join(".team/logs/copilot/worker_a/mcp-residual.txt");
    if !residual_file.exists() {
        failures.push(format!(
            "C-3-3: the residual scan must land a per-agent record at {}",
            residual_file.display()
        ));
    }
    std::env::set_var("PATH", &old_path);
    assert!(
        failures.is_empty(),
        "copilot MCP residual contract failed:\n{}",
        failures.join("\n")
    );
}

/// Q3 downgrade branch (C-3-3, RC-8 negative): when the `copilot` binary is NOT on
/// PATH (e.g. CI runners), the residual scan must degrade honestly — it must NOT emit
/// a false-positive `provider.copilot.mcp_residual_detected`, must add NO
/// `--disable-mcp-server` flags, and must still land a per-agent `mcp-residual.txt`
/// recording the `unavailable` outcome. This is the coverage the populated-residual
/// case alone could not give (0.3.5 ship-gate hermetic follow-up).
#[test]
#[serial(env)]
fn copilot_mcp_residual_scan_unavailable_degrades_honestly_no_false_positive() {
    // Empty isolated HOME (no ~/.copilot config) and a PATH with NO copilot binary, so
    // the scan exercises the `Err(_)` / unavailable arm deterministically.
    let home = tmp_ws("residual-unavail-home");
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    let empty_bin = tmp_ws("no-copilot-bin");
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("HOME", Box::leak(home.to_string_lossy().to_string().into_boxed_str())),
        ("PATH", Box::leak(empty_bin.to_string_lossy().to_string().into_boxed_str())),
    ]);
    let ws = tmp_ws("residual-unavail");
    let team_dir = write_copilot_team(&ws, "cp-residual-unavail", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpresidunavail"), &transport)
        .expect("quick-start should spawn the copilot worker even without copilot on PATH");
    let spawn = transport.single_spawn();

    let mut failures = Vec::new();
    if spawn.argv.iter().any(|arg| arg == "--disable-mcp-server") {
        failures.push(format!(
            "C-3-3 downgrade: an unavailable copilot scan must add NO --disable-mcp-server \
flags (nothing was actually observed); argv={:?}",
            spawn.argv
        ));
    }
    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default();
    if events.contains("mcp_residual_detected") {
        failures.push(
            "RC-8 negative: an unavailable scan must NOT emit a false-positive \
provider.copilot.mcp_residual_detected (only a real non-empty residual emits)".to_string(),
        );
    }
    let residual_file = ws.join(".team/logs/copilot/worker_a/mcp-residual.txt");
    match std::fs::read_to_string(&residual_file) {
        Err(_) => failures.push(format!(
            "C-3-3: the residual record must still be written at {} even when copilot is \
unavailable (honest 'I could not check')",
            residual_file.display()
        )),
        Ok(text) => {
            if !text.contains("unavailable") {
                failures.push(format!(
                    "C-3-3: the residual record must state the unavailable outcome; got {text:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "copilot MCP residual downgrade contract failed:\n{}",
        failures.join("\n")
    );
}

/// Q3 (C-3-4, RC-9): the `--additional-mcp-config` JSON the adapter writes must use
/// copilot's expected field name. Copilot's `mcp add` schema field is `transport`
/// (stdio|http|sse), NOT the `type` field used by claude/codex configs — the contract
/// locks the adapter to the copilot field name so the worker actually loads the server.
#[test]
#[serial(env)]
fn copilot_mcp_config_uses_copilot_field_name() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("mcp-schema");
    let team_dir = write_copilot_team(&ws, "cp-schema", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpschema"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let argv = &transport.single_spawn().argv;

    // Resolve the config text whether inline or @file.
    let value = flag_value(argv, "--additional-mcp-config").unwrap_or_default();
    let config_text = if let Some(path) = value.strip_prefix('@') {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        value
    };
    assert!(
        config_text.contains("transport") && !config_text.contains("\"type\""),
        "RC-9/C-3-4: the copilot MCP config must use the `transport` field (copilot \
`mcp add` schema), not the claude/codex `type` field; config={config_text:?}"
    );
}

/// C-7-3 / RC-24: a resume command line must not carry `--session-id` and `--resume`
/// together (conflicting session semantics). Driven through the real restart path so
/// the public resume builder is exercised end to end.
#[test]
#[serial(env)]
fn copilot_resume_argv_has_no_session_id_resume_conflict() {
    let home = tmp_ws("resume-home");
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("HOME", Box::leak(home.to_string_lossy().to_string().into_boxed_str())),
    ]);
    let ws = tmp_ws("resume");
    let team_dir = write_copilot_team(&ws, "cp-resume", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpresume"), &RecordingTransport::new())
        .expect("quick-start should seed the team");
    seed_running_resumable(&ws, "worker_a");
    seed_copilot_session_store(&home, "99999999-aaaa-4bbb-8ccc-dddddddddddd");

    let restart_transport = RecordingTransport::new();
    let _ = team_agent::lifecycle::restart_with_transport(&ws, true, Some("cpresume"), &restart_transport);
    let argv = restart_transport.single_spawn().argv;

    let has_session_id = argv.iter().any(|arg| arg == "--session-id");
    let has_resume = argv.iter().any(|arg| arg == "--resume");
    assert!(
        has_resume && !has_session_id,
        "RC-24/C-7-3: a copilot resume must carry --resume and NOT --session-id \
(conflicting session semantics); argv={argv:?}"
    );
}

// ───────────────────── subscription tier (design §E2.5 A-layer) ─────────────────────

/// A-1 (design §E2.5): a subscription worker with a role model carries `--model <m>`;
/// a worker with NO model configured must NOT carry --model (no hardcoded default
/// guess — leave it to the account/config default).
#[test]
#[serial(env)]
fn copilot_subscription_model_argv_only_when_configured() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);

    let ws = tmp_ws("model-set");
    let team_dir = write_copilot_team_model(&ws, "cp-model", &["mcp_team"], Some("gpt-5.5"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpmodel"), &transport)
        .expect("quick-start with a role model");
    let with_model = transport.single_spawn().argv;

    let ws2 = tmp_ws("model-none");
    let team_dir2 = write_copilot_team_model(&ws2, "cp-nomodel", &["mcp_team"], None);
    seed_healthy_coordinator(&ws2);
    let transport2 = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws2, &team_dir2, None, true, true, Some("cpnomodel"), &transport2)
        .expect("quick-start without a role model");
    let no_model = transport2.single_spawn().argv;

    let mut failures = Vec::new();
    if flag_value(&with_model, "--model").as_deref() != Some("gpt-5.5") {
        failures.push(format!(
            "A-1: a configured role model must produce --model gpt-5.5; argv={with_model:?}"
        ));
    }
    if no_model.iter().any(|arg| arg == "--model") {
        failures.push(format!(
            "A-1: no configured model must mean NO --model flag (no hardcoded default); argv={no_model:?}"
        ));
    }
    assert!(failures.is_empty(), "A-1 subscription model argv failed:\n{}", failures.join("\n"));
}

/// A-2 (design §E2.5): net model precedence agent.model > profile MODEL — the same
/// command_overrides ordering #264 locked, reused cross-provider. With a profile MODEL
/// and a role model both present, the role model wins in argv.
#[test]
#[serial(env)]
fn copilot_subscription_model_precedence_role_over_profile() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_ws("model-prec");
    let team_dir = write_copilot_team_model(&ws, "cp-prec", &["mcp_team"], Some("role-model-x"));
    // A subscription profile carrying a different MODEL.
    let profiles = team_dir.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("subm.env"), "AUTH_MODE=subscription\nMODEL=profile-model-y\n").unwrap();
    // point the agent at the profile
    let agent_md = team_dir.join("agents/worker_a.md");
    let body = std::fs::read_to_string(&agent_md).unwrap()
        .replace("auth_mode: subscription\n", "auth_mode: subscription\nprofile: subm\n");
    std::fs::write(&agent_md, body).unwrap();
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpprec"), &transport)
        .expect("quick-start with role+profile model");
    let argv = transport.single_spawn().argv;
    assert_eq!(
        flag_value(&argv, "--model").as_deref(),
        Some("role-model-x"),
        "A-2: agent.model must win over profile MODEL (command_overrides net order, #264 \
cross-provider reuse); argv={argv:?}"
    );
}

/// A-3 (design §E2.5): a subscription worker must NOT inject any COPILOT_PROVIDER_* or
/// token env (auth rides the user's logged-in state), and the env overlay must NOT
/// overwrite a user's COPILOT_GITHUB_TOKEN already in the environment.
#[test]
#[serial(env)]
fn copilot_subscription_env_no_provider_or_token_injection() {
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("COPILOT_GITHUB_TOKEN", "user-tok-keepme"),
    ]);
    let ws = tmp_ws("sub-env");
    let team_dir = write_copilot_team(&ws, "cp-subenv", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpsubenv"), &transport)
        .expect("quick-start subscription worker");
    let env = transport.single_spawn().env;

    let mut failures = Vec::new();
    for injected in env.keys().filter(|key| key.starts_with("COPILOT_PROVIDER")) {
        failures.push(format!("A-3: subscription worker must not inject {injected}"));
    }
    for token_key in ["COPILOT_API_KEY", "COPILOT_PROVIDER_API_KEY", "GH_TOKEN"] {
        if env.contains_key(token_key) {
            failures.push(format!("A-3: subscription worker must not inject token env {token_key}"));
        }
    }
    // overlay must inherit, not overwrite, the user's token
    if env.get("COPILOT_GITHUB_TOKEN").map(String::as_str) != Some("user-tok-keepme") {
        failures.push(format!(
            "A-3: env overlay must not overwrite the user's COPILOT_GITHUB_TOKEN; got {:?}",
            env.get("COPILOT_GITHUB_TOKEN")
        ));
    }
    assert!(failures.is_empty(), "A-3 subscription env contract failed:\n{}", failures.join("\n"));
}

/// A-4 (design §E2.5): a compatible_api (BYOK) copilot profile must export
/// COPILOT_PROVIDER_BASE_URL/TYPE/API_KEY/WIRE_API + COPILOT_MODEL into the worker
/// spawn env, and a BYOK profile WITHOUT a model must fail the launch ("A model is
/// required for BYOK", help-providers). Driven through the public quick-start path.
#[test]
#[serial(env)]
fn copilot_byok_profile_exports_provider_env_and_requires_model() {
    let _guard = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let mut failures = Vec::new();

    // BYOK profile WITH a model → spawn env carries the provider tuple.
    let ws = tmp_ws("byok-ok");
    let team_dir = write_copilot_byok_team(&ws, "cp-byok", Some("byok-model-z"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    match quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cpbyok"), &transport) {
        Err(error) => failures.push(format!("A-4: a complete BYOK profile must launch; got Err({error})")),
        Ok(_) => {
            let env = transport.single_spawn().env;
            for (key, expected) in [
                ("COPILOT_PROVIDER_BASE_URL", "https://byok.example/v1"),
                ("COPILOT_PROVIDER_API_KEY", "sk-byok"),
                ("COPILOT_PROVIDER_WIRE_API", "chat"),
                ("COPILOT_MODEL", "byok-model-z"),
            ] {
                if env.get(key).map(String::as_str) != Some(expected) {
                    failures.push(format!(
                        "A-4: BYOK worker env must carry {key}={expected}; got {:?}",
                        env.get(key)
                    ));
                }
            }
            if !env.contains_key("COPILOT_PROVIDER_TYPE") {
                failures.push("A-4: BYOK worker env must carry COPILOT_PROVIDER_TYPE".to_string());
            }
        }
    }

    // BYOK profile WITHOUT a model → launch must be refused.
    let ws2 = tmp_ws("byok-nomodel");
    let team_dir2 = write_copilot_byok_team(&ws2, "cp-byok-nm", None);
    seed_healthy_coordinator(&ws2);
    match quick_start_with_transport_in_workspace(&ws2, &team_dir2, None, true, true, Some("cpbyoknm"), &RecordingTransport::new()) {
        Ok(_) => failures.push(
            "A-4: a BYOK profile without a model must fail (help-providers: 'A model is \
required for BYOK'); got Ok"
                .to_string(),
        ),
        Err(error) => {
            if !error.to_string().to_lowercase().contains("model") {
                failures.push(format!(
                    "A-4: the BYOK no-model error must name the missing model; got {error}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "A-4 BYOK contract failed:\n{}", failures.join("\n"));
}

/// A-5 (design §E2.5): copilot has NO `auth status` subcommand (main-help Commands),
/// so the subscription auth hint must be a WEAK/honest signal — it must NOT strongly
/// claim "logged in" (AuthHintStatus::Present). A weak/unknown status is the honest
/// answer (MUST-NOT-13: no false-green logged-in claim).
#[test]
fn copilot_subscription_auth_hint_is_weak_not_strong_present() {
    let status = team_agent::provider::get_adapter(Provider::Copilot)
        .auth_hint(team_agent::provider::AuthMode::Subscription);
    assert_ne!(
        format!("{status:?}"),
        "Present",
        "A-5: copilot has no `auth status` subcommand, so a strong Present is a false \
green; the hint must be weak/unknown (honest — can't confirm logged-in); got {status:?}"
    );
}

/// A-6 (design §E2.5): when the user shell carries GITHUB_TOKEN/GH_TOKEN (which copilot
/// prioritizes over the credential store — cmd-login), spawning a copilot worker must
/// emit a passthrough-warning event (phase-1 observe-only, no stripping).
#[test]
#[serial(env)]
fn copilot_token_passthrough_emits_warning_event() {
    // Fake HOME so the residual MCP scan reads an empty copilot config (isolation:
    // otherwise it reads the real ~/.copilot and muddies this token-focused case).
    let home = tmp_ws("token-home");
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("HOME", Box::leak(home.to_string_lossy().to_string().into_boxed_str())),
        ("GITHUB_TOKEN", "ghp-shell-passthrough"),
    ]);
    let ws = tmp_ws("token-warn");
    let team_dir = write_copilot_team(&ws, "cp-token", &["mcp_team"], false);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("cptoken"), &transport)
        .expect("quick-start with a shell token present");

    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default();
    assert!(
        events.contains("token") && (events.contains("passthrough") || events.contains("github_token")),
        "A-6: a shell GITHUB_TOKEN/GH_TOKEN (copilot prioritizes it over the credential \
store) must emit a passthrough-warning event at spawn (phase-1 observe-only); events tail={}",
        events.lines().rev().take(4).collect::<Vec<_>>().join(" | ")
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

struct TeamOpts;

fn write_copilot_team_model(ws: &Path, name: &str, tools: &[&str], model: Option<&str>) -> PathBuf {
    let team = write_copilot_team(ws, name, tools, false);
    let agent_md = team.join("agents/worker_a.md");
    let stripped = std::fs::read_to_string(&agent_md).unwrap()
        .lines()
        .filter(|line| !line.trim_start().starts_with("model:"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let body = match model {
        Some(model) => stripped.replace("provider: copilot\n", &format!("provider: copilot\nmodel: {model}\n")),
        None => stripped,
    };
    std::fs::write(&agent_md, body).unwrap();
    team
}

/// Compiled copilot team whose worker uses a compatible_api (BYOK) profile. When
/// `model` is None the profile omits MODEL (the "A model is required for BYOK" case).
fn write_copilot_byok_team(ws: &Path, name: &str, model: Option<&str>) -> PathBuf {
    let team = ws.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(team.join("profiles")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!("---\nname: {name}\nobjective: copilot BYOK fixture.\nprovider: copilot\n---\n\nTeam.\n"),
    )
    .unwrap();
    let model_field = model.map(|m| format!("model: {m}\n")).unwrap_or_default();
    std::fs::write(
        team.join("agents/worker_a.md"),
        format!(
            "---\nname: worker_a\nrole: Copilot Worker\nprovider: copilot\n{model_field}auth_mode: compatible_api\nprofile: byok\ntools:\n  - mcp_team\n---\n\nCOPILOT ROLE BODY SENTINEL: BYOK worker.\n"
        ),
    )
    .unwrap();
    let model_line = model.map(|m| format!("MODEL={m}\n")).unwrap_or_default();
    std::fs::write(
        team.join("profiles/byok.env"),
        format!(
            "AUTH_MODE=compatible_api\nBASE_URL=https://byok.example/v1\nAPI_KEY=sk-byok\nWIRE_API=chat\nPROVIDER_TYPE=openai\n{model_line}"
        ),
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&team).expect("compile BYOK fixture team");
    std::fs::write(team.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
    team
}

fn write_copilot_team(ws: &Path, name: &str, tools: &[&str], dangerous: bool) -> PathBuf {
    let _ = TeamOpts;
    let team = ws.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    let dangerous_line = if dangerous { "dangerous_auto_approve: true\n" } else { "" };
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: copilot provider contract fixture.\nprovider: copilot\n{dangerous_line}---\n\nTeam fixture.\n"
        ),
    )
    .unwrap();
    let tool_lines = tools.iter().map(|tool| format!("  - {tool}\n")).collect::<String>();
    std::fs::write(
        team.join("agents/worker_a.md"),
        format!(
            "---\nname: worker_a\nrole: Copilot Worker\nprovider: copilot\nmodel: gpt-5\nauth_mode: subscription\ntools:\n{tool_lines}---\n\nCOPILOT ROLE BODY SENTINEL: serve the copilot adapter contract.\n"
        ),
    )
    .unwrap();
    team
}

fn seed_running_resumable(ws: &Path, agent: &str) {
    let mut state = load_runtime_state(ws).expect("load state before restart");
    let patch = serde_json::json!({
        "status": "running",
        "provider": "copilot",
        "session_id": "99999999-aaaa-4bbb-8ccc-dddddddddddd",
        "first_send_at": "2026-06-10T00:00:00+00:00",
        "window": agent,
    });
    for pointer in [format!("/agents/{agent}"), format!("/teams/cpresume/agents/{agent}")] {
        if let Some(obj) = state.pointer_mut(&pointer).and_then(serde_json::Value::as_object_mut) {
            for (key, value) in patch.as_object().unwrap() {
                obj.insert(key.clone(), value.clone());
            }
        }
    }
    team_agent::state::persist::save_runtime_state(ws, &state).unwrap();
}

fn seed_copilot_session_store(home: &Path, session_id: &str) {
    let db = home.join(".copilot").join("session-store.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "create table if not exists sessions (
            id text primary key,
            name text,
            created_at text
        );",
    )
    .unwrap();
    conn.execute(
        "insert or replace into sessions(id, name, created_at) values (?1, 'resume-test', '2026-06-10T00:00:00Z')",
        rusqlite::params![session_id],
    )
    .unwrap();
}

fn seed_session_id(ws: &Path, agent: &str) {
    let mut state = load_runtime_state(ws).unwrap();
    for pointer in [format!("/agents/{agent}"), format!("/teams/cpfork/agents/{agent}")] {
        if let Some(obj) = state.pointer_mut(&pointer).and_then(serde_json::Value::as_object_mut) {
            obj.insert(
                "session_id".to_string(),
                serde_json::json!("77777777-8888-4999-aaaa-bbbbbbbbbbbb"),
            );
        }
    }
    team_agent::state::persist::save_runtime_state(ws, &state).unwrap();
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(
        team_agent::coordinator::coordinator_pid_path(&workspace)
            .parent()
            .unwrap(),
    )
    .unwrap();
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    argv.windows(2)
        .find_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
}

fn flag_values(argv: &[String], flag: &str) -> Vec<String> {
    argv.windows(2)
        .filter_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
        .collect()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(values: &[(&'static str, &'static str)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[derive(Debug, Clone)]
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

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present() -> Self {
        let transport = Self::default();
        *transport.session_present.lock().unwrap() = true;
        transport
    }

    fn single_spawn(&self) -> RecordedSpawn {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1, "fixture should record one spawn; got {spawns:?}");
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

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
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

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-copilot-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
