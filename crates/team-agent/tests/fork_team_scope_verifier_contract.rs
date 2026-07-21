//! Independent-verifier RED contract: `fork-agent` returning ok MUST yield a
//! clone that is actually usable in the selected team scope.
//!
//! Case source (phenomenon only, information-isolated from owner locate/implement
//! reasoning): `.team/artifacts/pipeline-runs/fork-team-scope-bug/case.md` —
//! real-machine first use of fork: `fork-agent ... --team T` returned
//! `ok:true`, the tmux window/pane physically exists, yet
//! `send --to <clone> --team T` refuses `target_not_in_team`,
//! `send --to-name 'ws::T/<clone>'` refuses `name_not_resolvable`, and
//! `status --team T` does not list the clone. ok-but-unusable = false success.
//!
//! Contract (as assigned by leader): fork ok ⇒
//!   1. clone is send-addressable by short id within the team,
//!   2. clone is send-addressable by fully-qualified name,
//!   3. clone is visible in team status,
//!   4. clone can be retired (stop-agent succeeds).
//!
//! Harness: real binary + real tmux on the team's own socket; provider is a
//! `claude` PATH shim (repo convention for external CLIs — zero tokens); the
//! source agent's session backing tuple is seeded as fixture prep because a
//! real captured claude session cannot exist hermetically. All verbs the user
//! runs are canonical: quick-start / fork-agent / send / status / stop-agent /
//! shutdown.
//!
//! Frozen assertion source SHA256:
//! `0422159c46da07e1878907bdd118fe59cc18319502ce75f4d8dcfb40ff597b73`.
//! The workspace-local `claude` PATH shim keeps this contract CI-runnable and
//! subscription-free; no assertion is host-only.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};
use serial_test::serial;

const TEAM_NAME: &str = "forkscope";
const SOURCE: &str = "base";
const CLONE: &str = "forked";

struct ForkScopeCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: PathBuf,
}

impl ForkScopeCase {
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
        // `--team forkscope` selector would exercise a different (pre-existing,
        // out-of-case) spec-name-vs-runtime-key divergence instead of the fork
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
        case.seed_source_session_tuple();
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

    fn state_path(&self) -> PathBuf {
        self.workspace
            .join(".team")
            .join("runtime")
            .join("state.json")
    }

    /// Fixture prep: a hermetic claude shim has no real captured session, so
    /// seed the complete backing tuple (session_id + rollout_path +
    /// captured_at + captured_via, rollout file existing on disk) into every
    /// projection of the source agent row — simulating what session capture
    /// records for a real worker.
    fn seed_source_session_tuple(&self) {
        let rollout = self.workspace.join("fixture-rollout.jsonl");
        std::fs::write(&rollout, "{\"type\":\"fixture\"}\n").expect("write rollout fixture");
        let raw = std::fs::read_to_string(self.state_path()).expect("read state.json");
        let mut state: Value = serde_json::from_str(&raw).expect("parse state.json");
        let tuple = json!({
            "session_id": "sess-fork-scope-contract",
            "rollout_path": rollout.to_string_lossy(),
            "captured_at": "2026-07-17T00:00:00Z",
            "captured_via": "contract-fixture"
        });
        let mut patched = 0;
        if let Some(row) = state
            .get_mut("agents")
            .and_then(|a| a.get_mut(SOURCE))
            .and_then(Value::as_object_mut)
        {
            for (key, value) in tuple.as_object().expect("tuple object") {
                row.insert(key.clone(), value.clone());
            }
            patched += 1;
        }
        if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
            for team in teams.values_mut() {
                if let Some(row) = team
                    .get_mut("agents")
                    .and_then(|a| a.get_mut(SOURCE))
                    .and_then(Value::as_object_mut)
                {
                    for (key, value) in tuple.as_object().expect("tuple object") {
                        row.insert(key.clone(), value.clone());
                    }
                    patched += 1;
                }
            }
        }
        assert!(
            patched >= 1,
            "fixture: source agent row must exist in state.json to seed; state={state}"
        );
        std::fs::write(
            self.state_path(),
            serde_json::to_string_pretty(&state).expect("serialize state"),
        )
        .expect("write seeded state.json");
    }

    fn fork_clone(&self) -> Value {
        let output = self.run_cli(&[
            "fork-agent",
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
                "fork-agent --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert!(
            output.status.success()
                && value.get("ok").and_then(Value::as_bool) == Some(true)
                && value.get("new_agent_id").and_then(Value::as_str) == Some(CLONE),
            "fixture: fork-agent must report ok (the case phenomenon starts AFTER a \
             reported-ok fork); code={:?} stdout={} stderr={}",
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
            "fixture: fork ok must have physically created the clone window; windows={names}"
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
            "---\nname: {TEAM_NAME}\nobjective: fork-agent team-scope addressability contract.\nprovider: claude\n---\n"
        ),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{SOURCE}.\n"
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
  printf '{"sessionId":"%s","type":"fork-backing"}\n' "$sid" > "$dir/$sid.jsonl"
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

/// RED 1 — short-id send within the team: after fork ok, `send <clone>
/// --team <team>` must accept the message (any delivery state), never refuse
/// with `target_not_in_team`.
#[test]
#[serial(env)]
fn fork_ok_clone_is_send_addressable_by_short_id_in_team() {
    let case = ForkScopeCase::start("fts-red1");
    case.fork_clone();
    let output = case.run_cli(&[
        "send",
        CLONE,
        "fork scope probe",
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
        "RED1: fork reported ok, so the clone must be addressable via \
         `send {CLONE} --team {TEAM_NAME}`; got ok={ok} reason={reason} value={value}"
    );
    case.shutdown();
}

/// RED 2 — fully-qualified name send: `send --to-name '<ws>::<team>/<clone>'`
/// must resolve, never `name_not_resolvable`.
#[test]
#[serial(env)]
fn fork_ok_clone_is_send_addressable_by_qualified_name() {
    let case = ForkScopeCase::start("fts-red2");
    case.fork_clone();
    let qualified = format!("{}::{}/{}", case.workspace_str(), TEAM_NAME, CLONE);
    let output = case.run_cli(&[
        "send",
        "--to-name",
        qualified.as_str(),
        "qualified fork scope probe",
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
        "RED2: fork reported ok, so the stable qualified name `{qualified}` must \
         resolve; got ok={ok} reason={reason} value={value}"
    );
    case.shutdown();
}

/// RED 3 — team status visibility: `status --team <team>` must list the clone
/// in the team-scoped agents projection.
#[test]
#[serial(env)]
fn fork_ok_clone_is_visible_in_team_status() {
    let case = ForkScopeCase::start("fts-red3");
    case.fork_clone();
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
        "RED3: fork reported ok, so `status --team {TEAM_NAME}` must list the \
         clone; agents={:?}",
        value
            .get("agents")
            .and_then(Value::as_object)
            .map(|a| a.keys().cloned().collect::<Vec<_>>())
    );
    case.shutdown();
}

/// RED 4 — retirement: a forked clone must be stoppable through the normal
/// lifecycle verb; `stop-agent <clone> --team <team>` must succeed.
#[test]
#[serial(env)]
fn fork_ok_clone_can_be_retired_with_stop_agent() {
    let case = ForkScopeCase::start("fts-red4");
    case.fork_clone();
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
        "RED4: fork reported ok, so the clone must be retirable via \
         `stop-agent {CLONE} --team {TEAM_NAME}`; got value={value} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    case.shutdown();
}
