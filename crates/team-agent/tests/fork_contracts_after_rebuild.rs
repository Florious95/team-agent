//! ---
//! purpose: 钉死 fork 重建后不再静默吞 --as、不再空壳审计、不再留死分支
//! contract:
//!   provides:
//!     - name: A13-no-silent-shells
//!       what: --as 要么报错要么新名可寻址；成功分身写出可读审计；Pending 死臂已删
//! boundary:
//!   - 不改 claude/codex/copilot 能力断言
//!   - 基线红必须是断言失败，不是 unresolved import
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use serde_json::json;
use team_agent::lifecycle::launch::fork_agent_with_transport;
use team_agent::model::ids::AgentId;
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

struct ForkScreenRunner {
    screen: String,
}

impl CommandRunner for ForkScreenRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|a| a == "capture-pane") {
            return Ok(ok(&self.screen));
        }
        Ok(ok(""))
    }
}

fn grok_ws(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!(
        "ta-fork-contract-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: fx\nobjective: fork contract.\nprovider: grok\nauth_mode: subscription\n---\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("agents").join("seer.md"),
        "---\nname: seer\nrole: Seer\nprovider: grok\nmodel: grok-4.6\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - provider_builtin\n---\nseer.\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&ws).expect("compile");
    std::fs::write(ws.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-fx",
            "team_key": "fx",
            "agents": {
                "seer": {
                    "status": "running",
                    "provider": "grok",
                    "auth_mode": "subscription",
                    "window": "seer",
                    "pane_id": "%7"
                }
            }
        }),
    )
    .unwrap();
    ws
}

fn backend() -> TmuxBackend {
    TmuxBackend::with_runner(Box::new(ForkScreenRunner {
        screen: "Session new (forked from old-id)\n".into(),
    }))
}

/// Choice: in-place fork must REJECT --as <other>, not swallow it and
/// return the source name. (Registering a second addressable name is the
/// other legal option; this crate picked reject.)
#[test]
fn as_flag_is_refused_not_silently_rewritten_to_source() {
    let ws = grok_ws("as");
    let tx = backend();
    let result = fork_agent_with_transport(
        &ws,
        &AgentId::new("seer"),
        &AgentId::new("seer2"),
        None,
        false,
        None,
        &tx,
    );
    match result {
        Ok(report) => {
            assert_ne!(
                report.new_agent_id.as_str(),
                report.source_agent_id.as_str(),
                "must not accept --as seer2 then report new_agent_id=seer"
            );
            let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
            assert!(
                state
                    .get("agents")
                    .and_then(|a| a.get("seer2"))
                    .is_some(),
                "if --as is accepted, seer2 must be an addressable agent row"
            );
        }
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("--as")
                    || text.contains("as_agent")
                    || text.contains("in-place")
                    || text.contains("in place"),
                "refusal must name --as / in-place, not a generic error; got {text}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
}

/// Successful in-place fork must leave a readable audit row we can load back.
#[test]
fn successful_fork_writes_readable_audit_event() {
    let ws = grok_ws("audit");
    let tx = backend();
    let result = fork_agent_with_transport(
        &ws,
        &AgentId::new("seer"),
        &AgentId::new("seer"),
        None,
        false,
        None,
        &tx,
    );
    assert!(result.is_ok(), "same-name in-place fork must succeed: {result:?}");
    let events = ws.join(".team").join("logs").join("events.jsonl");
    let text = std::fs::read_to_string(&events).unwrap_or_default();
    assert!(
        text.contains("lifecycle.fork.in_place"),
        "success must append lifecycle.fork.in_place; events={text:?}"
    );
    assert!(
        text.contains("seer"),
        "audit row must name the source seat; events={text:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// PendingContextFork else-arm is dead (wait_for_forked_from never returns false).
/// After the fix the token must be gone from fork_agent.rs.
#[test]
fn pending_context_fork_dead_arm_is_gone() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/lifecycle/launch/fork_agent.rs");
    let src = std::fs::read_to_string(&path).expect("read fork_agent.rs (positive control)");
    assert!(
        src.contains("forked from"),
        "positive control: fork_agent.rs must still mention the screen mark"
    );
    assert!(
        !src.contains("PendingContextFork"),
        "dead else-arm ForkBackingState::PendingContextFork must be deleted from fork_agent.rs"
    );
}
