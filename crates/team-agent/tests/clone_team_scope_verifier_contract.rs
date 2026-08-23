//! Independent-verifier contract: `clone-agent` returning ok MUST yield a NEW
//! SEAT that is actually usable in the selected team scope.
//!
//! # 2026-08-23 rehome: this contract used to be written on `fork-agent`
//!
//! It was moved to `clone-agent` because the behaviour it asserts has no
//! landing point in `fork` at all — see `wiki/C1/分身与克隆.md`:
//!
//! - `fork` injects the provider's own command (grok `/fork`, claude
//!   `/branch`) into the SOURCE pane; the provider opens its own window, the
//!   session id is provider-assigned, and **the window has no name**. So
//!   "address the new thing by name / see it in status / retire it with
//!   stop-agent" cannot be satisfied by fork — by design, not by defect
//!   (`fork_agent.rs` reports `new_agent_id == source_agent_id` and
//!   `session_id: None`, and refuses `--as <other>`).
//! - `clone` goes through `add-agent`, materialises the role file, and
//!   produces a NAMED new seat. Every claim below belongs here.
//!
//! ⛔ This is a rewrite to clone semantics, NOT the old fork fixture moved
//! across: the fork-only scaffolding (seeding the source session backing
//! tuple, so that fork had something to copy) is gone, because clone is
//! zero-context by definition and consumes no source backing.
//!
//! Case source (phenomenon only): the injury this contract exists to prevent
//! is recorded in `.team/artifacts/pipeline-runs/fork-team-scope-bug/case.md`
//! — a real-machine run where creating a second seat returned `ok:true` and
//! the tmux window/pane physically existed, yet `send --to <new> --team T`
//! refused `target_not_in_team`, `send --to-name 'ws::T/<new>'` refused
//! `name_not_resolvable`, and `status --team T` did not list it. The seat was
//! registered in the ROOT agents map but not in the team scope, while `send`
//! resolves by team scope. ok-but-unusable = false success. That registration
//! path is what `clone-agent` uses today, so the case is still live here.
//!
//! Contract: clone ok ⇒
//!   1. new seat is send-addressable by short id within the team,
//!   2. new seat is send-addressable by fully-qualified name,
//!   3. new seat is visible in team status,
//!   4. new seat can be retired (stop-agent succeeds).
//!
//! Harness: real binary + real tmux on the team's own socket; provider is a
//! `claude` PATH shim (repo convention for external CLIs — zero tokens). All
//! verbs the user runs are canonical: quick-start / clone-agent / send /
//! status / stop-agent / shutdown.
//!
//! ⚠️ The pre-rehome frozen assertion SHA256
//! (`0422159c46da07e1878907bdd118fe59cc18319502ce75f4d8dcfb40ff597b73`)
//! covered the fork-worded file and no longer applies; the four assertions
//! themselves are carried over unchanged in substance.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use serial_test::serial;

const TEAM_NAME: &str = "clonescope";
const SOURCE: &str = "base";
const CLONE: &str = "cloned";

struct CloneScopeCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: PathBuf,
}

impl CloneScopeCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        write_team_docs(&workspace);
        let shim_dir = write_claude_shim(&workspace);
        let shim_path = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").expect("PATH present")
        );
        let case = Self {
            env,
            workspace,
            shim_path,
            socket: PathBuf::new(),
        };
        // `--team-id` pins the runtime team key to the spec name — the same
        // shape as the real-machine case (`--team-id scopeprobe`). Without it
        // the runtime key derives from the workspace dir and every later
        // `--team clonescope` selector would exercise a different (pre-existing,
        // out-of-case) spec-name-vs-runtime-key divergence instead of the clone
        // registration path under test.
        let output = case.run_cli(&[
            "quick-start",
            "--workspace",
            case.workspace_str(),
            "--team-id",
            TEAM_NAME,
            "--yes",
            "--no-display",
            "--json",
        ]);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "quick-start --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let all_spawned = value
            .get("worker_readiness")
            .and_then(|node| node.get("all_workers_spawned"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        assert!(
            all_spawned,
            "quick-start must spawn the source worker; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let mut case = case;
        case.socket = case.discover_socket();
        case
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    /// Every CLI call carries the claude-shim PATH so spawned workers resolve
    /// `claude` to the offline shim (zero provider tokens).
    fn run_cli(&self, args: &[&str]) -> Output {
        self.env
            .run_cli_env(&self.workspace, args, &[("PATH", self.shim_path.as_str())])
    }

    fn discover_socket(&self) -> PathBuf {
        let output = self.run_cli(&["status", "--workspace", self.workspace_str(), "--json"]);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "status --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let attach = value
            .get("leader_attach_command")
            .and_then(Value::as_str)
            .expect("status must expose leader_attach_command");
        let socket = attach
            .split_whitespace()
            .skip_while(|token| *token != "-S")
            .nth(1)
            .expect("leader_attach_command must carry -S <socket>");
        PathBuf::from(socket)
    }

    fn clone_seat(&self) -> Value {
        let output = self.run_cli(&[
            "clone-agent",
            SOURCE,
            "--as",
            CLONE,
            "--workspace",
            self.workspace_str(),
            "--team",
            TEAM_NAME,
            "--no-display",
            "--json",
        ]);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "clone-agent --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert!(
            output.status.success()
                && value.get("ok").and_then(Value::as_bool) == Some(true)
                && value.get("new_agent_id").and_then(Value::as_str) == Some(CLONE),
            "fixture: clone-agent must report ok (the case phenomenon starts AFTER a \
             reported-ok clone); code={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Physical sanity — the clone window exists on the team socket, exactly
        // as observed in the case evidence.
        let windows = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "list-windows",
                "-a",
                "-F",
                "#{window_name}",
            ])
            .output()
            .expect("tmux list-windows");
        let names = String::from_utf8_lossy(&windows.stdout);
        assert!(
            names.lines().any(|name| name == CLONE),
            "fixture: clone ok must have physically created the new seat window; windows={names}"
        );
        value
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--yes",
            "--json",
        ]);
        let _ = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "kill-server",
            ])
            .output();
    }
}

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM_NAME}\nobjective: clone-agent team-scope addressability contract.\nprovider: claude\n---\n"
        ),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\n{SOURCE}.\n"
        ),
    )
    .expect("write source role doc");
}

fn write_claude_shim(workspace: &Path) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    std::fs::write(
        &shim,
        r#"#!/bin/sh
sid=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--session-id" ]; then sid="$arg"; fi
  prev="$arg"
done
if [ -n "$sid" ]; then
  encoded=$(printf '%s' "$PWD" | sed 's/[^a-zA-Z0-9]/-/g')
  dir="$HOME/.claude/projects/$encoded"
  mkdir -p "$dir"
  printf '{"sessionId":"%s","type":"clone-backing"}\n' "$sid" > "$dir/$sid.jsonl"
fi
echo "claude shim ready"
exec sleep 3600
"#,
    )
    .expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

fn json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: expected JSON stdout; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// RED 1 — short-id send within the team: after clone ok, `send <new seat>
/// --team <team>` must accept the message (any delivery state), never refuse
/// with `target_not_in_team`.
#[test]
#[serial(env)]
fn clone_ok_new_seat_is_send_addressable_by_short_id_in_team() {
    let case = CloneScopeCase::start("fts-red1");
    case.clone_seat();
    let output = case.run_cli(&[
        "send",
        CLONE,
        "clone scope probe",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM_NAME,
        "--no-wait",
        "--json",
    ]);
    let value = json_stdout(&output, "RED1 send short id");
    let ok = value.get("ok").and_then(Value::as_bool) == Some(true);
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("");
    assert!(
        ok && reason != "target_not_in_team",
        "RED1: clone reported ok, so the new seat must be addressable via \
         `send {CLONE} --team {TEAM_NAME}`; got ok={ok} reason={reason} value={value}"
    );
    case.shutdown();
}

/// RED 2 — fully-qualified name send: `send --to-name '<ws>::<team>/<clone>'`
/// must resolve, never `name_not_resolvable`.
#[test]
#[serial(env)]
fn clone_ok_new_seat_is_send_addressable_by_qualified_name() {
    let case = CloneScopeCase::start("fts-red2");
    case.clone_seat();
    let qualified = format!("{}::{}/{}", case.workspace_str(), TEAM_NAME, CLONE);
    let output = case.run_cli(&[
        "send",
        "--to-name",
        qualified.as_str(),
        "qualified clone scope probe",
        "--workspace",
        case.workspace_str(),
        "--no-wait",
        "--json",
    ]);
    let value = json_stdout(&output, "RED2 send qualified name");
    let ok = value.get("ok").and_then(Value::as_bool) == Some(true);
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("");
    assert!(
        ok && reason != "name_not_resolvable",
        "RED2: clone reported ok, so the stable qualified name `{qualified}` must \
         resolve; got ok={ok} reason={reason} value={value}"
    );
    case.shutdown();
}

/// RED 3 — team status visibility: `status --team <team>` must list the new seat
/// in the team-scoped agents projection.
#[test]
#[serial(env)]
fn clone_ok_new_seat_is_visible_in_team_status() {
    let case = CloneScopeCase::start("fts-red3");
    case.clone_seat();
    let output = case.run_cli(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM_NAME,
        "--json",
    ]);
    let value = json_stdout(&output, "RED3 status");
    let listed = value
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| agents.contains_key(CLONE));
    assert!(
        listed,
        "RED3: clone reported ok, so `status --team {TEAM_NAME}` must list the \
         clone; agents={:?}",
        value
            .get("agents")
            .and_then(Value::as_object)
            .map(|a| a.keys().cloned().collect::<Vec<_>>())
    );
    case.shutdown();
}

/// RED 4 — retirement: a cloned seat must be stoppable through the normal
/// lifecycle verb; `stop-agent <clone> --team <team>` must succeed.
#[test]
#[serial(env)]
fn clone_ok_new_seat_can_be_retired_with_stop_agent() {
    let case = CloneScopeCase::start("fts-red4");
    case.clone_seat();
    let output = case.run_cli(&[
        "stop-agent",
        CLONE,
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM_NAME,
        "--json",
    ]);
    let value = json_stdout(&output, "RED4 stop-agent");
    let ok = value.get("ok").and_then(Value::as_bool) == Some(true);
    assert!(
        ok,
        "RED4: clone reported ok, so the new seat must be retirable via \
         `stop-agent {CLONE} --team {TEAM_NAME}`; got value={value} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    case.shutdown();
}
