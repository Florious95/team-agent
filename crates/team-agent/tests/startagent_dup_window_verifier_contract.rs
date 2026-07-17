//! Independent-verifier RED contract: `start-agent` must never leave the same
//! agent with duplicate windows/panes, and a refused start must not change the
//! physical tmux topology.
//!
//! Case source (phenomenon only, information-isolated from owner locate/implement):
//! `.team/artifacts/pipeline-runs/startagent-dup-window/case.md` —
//! a user running `start-agent` online saw the same agent opened twice
//! (same-name double window / double pane). Acceptance base line from the case
//! doc: false-green and false-red are both bugs; "window already exists" must
//! be judged by atomic identity, not name-weak keys.
//!
//! Contract objects (as assigned by leader):
//!  1. `start-agent --force` must not leave duplicate same-role panes.
//!  2. A failed/refused start path must not change physical topology.
//!
//! All triggers are canonical user commands (MUST-15): `quick-start`,
//! `start-agent <agent> --force`, a user retry of the same command, `shutdown`.
//! No state surgery, no fault injection. Real binary + real tmux on an
//! isolated per-workspace socket (physical-topology bug ⇒ physical evidence).
//!
//! Frozen assertion source SHA256:
//! `4758302899f19c924139b67cdd971c1837fbdbb04a49e1550d1f485513421b76`.
//! CI adaptation is limited to the topology reader's printable field frame
//! and bounded visibility poll; the frozen assertions and messages are intact.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use serial_test::serial;

const TEAM_NAME: &str = "sdwcase";

struct DupWindowCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
}

impl DupWindowCase {
    /// quick-start a real fake-provider team via the real binary.
    /// `per_agent_windows=true` passes `--no-display` (one window per agent);
    /// false keeps the default adaptive display path.
    fn start(tag: &str, workers: &[&str], per_agent_windows: bool) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        write_team_docs(&workspace, workers);
        let mut args = vec![
            "quick-start",
            "--workspace",
            workspace.to_str().expect("workspace utf8"),
            "--yes",
            "--json",
        ];
        if per_agent_windows {
            args.push("--no-display");
        }
        let output = env.run_cli(&workspace, &args);
        // A leaderless CLI quick-start reports degraded (`leader_receiver_unbound`,
        // non-zero exit) while still spawning every worker — the same state a
        // real user is in right before `claim-leader`. The fixture only needs
        // the workers to be physically up.
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "quick-start --json must emit JSON for the {tag} fixture; stdout={} stderr={}",
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
            "quick-start must spawn all workers for the {tag} fixture; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // The team's own workspace-hash socket (from `leader_attach_command`)
        // fails `register_owned_tmux_socket` fixture-provenance naming, so
        // cleanup is done explicitly by `shutdown()` + `kill-server` instead.
        let socket = discover_socket(&env, &workspace);
        Self {
            env,
            workspace,
            socket,
        }
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn start_agent_force(&self, agent: &str) -> (Output, Option<Value>) {
        let output = self.run_cli(&[
            "start-agent",
            agent,
            "--workspace",
            self.workspace.to_str().expect("workspace utf8"),
            "--force",
            "--json",
        ]);
        let value = serde_json::from_slice::<Value>(&output.stdout).ok();
        (output, value)
    }

    /// Physical topology: `(window_name, pane_id)` of every LIVE pane on the
    /// team socket, straight from real tmux (the same view a user has).
    fn live_panes(&self) -> Vec<(String, String)> {
        // Keep the physical frame explicit across tmux/client environments.
        // Poll only the exact test-owned socket; the frozen topology assertions
        // below remain unchanged.
        for attempt in 0..=40 {
            let output = std::process::Command::new("tmux")
                .args([
                    "-S",
                    self.socket.to_str().expect("socket utf8"),
                    "list-panes",
                    "-a",
                    "-F",
                    "#{window_name}__TA_FIELD__#{pane_id}__TA_FIELD__#{pane_dead}",
                ])
                .output()
                .expect("run tmux list-panes");
            assert!(
                output.status.success(),
                "tmux list-panes must succeed on the team socket {}; stderr={}",
                self.socket.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            let panes = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut cols = line.split("__TA_FIELD__");
                    let window = cols.next()?.to_string();
                    let pane = cols.next()?.to_string();
                    let dead = cols.next()?.trim();
                    (dead == "0").then_some((window, pane))
                })
                .collect::<Vec<_>>();
            if !panes.is_empty() || attempt == 40 {
                return panes;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        unreachable!("bounded topology poll always returns on its final attempt")
    }

    fn live_panes_for_window(&self, window: &str) -> Vec<String> {
        self.live_panes()
            .into_iter()
            .filter(|(name, _)| name == window)
            .map(|(_, pane)| pane)
            .collect()
    }

    /// Recorded pane binding for an agent, read from the runtime state file
    /// (top-level agents map or any team projection).
    fn recorded_pane_id(&self, agent: &str) -> Option<String> {
        let raw = std::fs::read_to_string(
            self.workspace
                .join(".team")
                .join("runtime")
                .join("state.json"),
        )
        .ok()?;
        let state: Value = serde_json::from_str(&raw).ok()?;
        let from_agents = |node: &Value| -> Option<String> {
            node.get("agents")?
                .get(agent)?
                .get("pane_id")?
                .as_str()
                .filter(|pane| !pane.is_empty())
                .map(ToString::to_string)
        };
        if let Some(pane) = from_agents(&state) {
            return Some(pane);
        }
        state
            .get("teams")?
            .as_object()?
            .values()
            .find_map(from_agents)
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace.to_str().expect("workspace utf8"),
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

fn write_team_docs(workspace: &Path, workers: &[&str]) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM_NAME}\nobjective: start-agent duplicate window contract.\nprovider: fake\n---\n"
        ),
    )
    .expect("write TEAM.md");
    for worker in workers {
        std::fs::write(
            workspace.join("agents").join(format!("{worker}.md")),
            format!(
                "---\nname: {worker}\nrole: {worker}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{worker}.\n"
            ),
        )
        .expect("write worker role doc");
    }
}

fn discover_socket(env: &HermeticTestEnv, workspace: &Path) -> PathBuf {
    let output = env.run_cli(
        workspace,
        &[
            "status",
            "--workspace",
            workspace.to_str().expect("workspace utf8"),
            "--json",
        ],
    );
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
        .expect("status --json must expose leader_attach_command");
    // shape: `tmux -S <socket> attach -t <target>`
    let socket = attach
        .split_whitespace()
        .skip_while(|token| *token != "-S")
        .nth(1)
        .expect("leader_attach_command must carry -S <socket>");
    PathBuf::from(socket)
}

fn command_reported_ok(value: Option<&Value>) -> bool {
    value
        .and_then(|v| v.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// RED 1 — per-agent window layout (`--no-display`): after a single
/// `start-agent w1 --force` against a healthy live worker, tmux must contain
/// exactly ONE live pane in windows named `w1`, whatever the command's exit.
/// Two live panes = the reported same-name double-window regression.
#[test]
#[serial(env)]
fn force_start_on_live_worker_leaves_exactly_one_same_role_pane_per_agent_window() {
    let case = DupWindowCase::start("sdw-red1", &["w1"], true);
    let before = case.live_panes_for_window("w1");
    assert_eq!(
        before.len(),
        1,
        "fixture: quick-start must leave exactly one live w1 pane; got {before:?}"
    );
    let original = before[0].clone();

    let (output, value) = case.start_agent_force("w1");
    let after = case.live_panes_for_window("w1");
    let ok = command_reported_ok(value.as_ref());

    assert_eq!(
        after.len(),
        1,
        "RED1: start-agent --force must not leave duplicate same-role panes/windows; \
         user-visible topology now has {} live panes in windows named w1 ({after:?}); \
         command ok={ok} code={:?} stdout={} stderr={}",
        after.len(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let survivor = after[0].clone();
    let recorded = case.recorded_pane_id("w1");
    if ok {
        assert_eq!(
            recorded.as_deref(),
            Some(survivor.as_str()),
            "RED1: on reported success the recorded binding must equal the one \
             observed live pane (atomic identity, not name-weak key); recorded={recorded:?} observed={survivor}"
        );
    } else {
        assert_eq!(
            survivor, original,
            "RED1: a refused start must leave the original pane untouched; original={original} survivor={survivor}"
        );
        assert_eq!(
            recorded.as_deref(),
            Some(original.as_str()),
            "RED1: a refused start must not rebind state away from the live original pane; recorded={recorded:?}"
        );
    }
    case.shutdown();
}

/// RED 2 — default adaptive display path (no `--no-display`), two workers:
/// forcing w1 must neither duplicate w1 nor disturb the co-tenant w2 pane.
#[test]
#[serial(env)]
fn force_start_on_adaptive_layout_does_not_duplicate_or_disturb_cotenants() {
    let case = DupWindowCase::start("sdw-red2", &["w1", "w2"], false);
    let w2_before = case.live_panes_for_window("w2");
    assert_eq!(
        w2_before.len(),
        1,
        "fixture: quick-start must leave exactly one live w2 pane; got {w2_before:?}"
    );

    let (output, value) = case.start_agent_force("w1");
    let w1_after = case.live_panes_for_window("w1");
    let w2_after = case.live_panes_for_window("w2");
    let ok = command_reported_ok(value.as_ref());

    assert_eq!(
        w1_after.len(),
        1,
        "RED2: start-agent --force on the default display path must not leave \
         duplicate same-role panes; got {w1_after:?}; command ok={ok} code={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        w2_after, w2_before,
        "RED2: forcing w1 must not disturb the co-tenant w2 pane; before={w2_before:?} after={w2_after:?}"
    );
    case.shutdown();
}

/// RED 3 — user retry: running `start-agent w1 --force` twice in a row (the
/// natural user reaction to a weird first result) must still end with exactly
/// one live w1 pane, and any refused attempt must leave the whole team
/// topology byte-identical (failure path must not change physical topology).
#[test]
#[serial(env)]
fn repeated_force_start_converges_to_one_pane_and_refusals_freeze_topology() {
    let case = DupWindowCase::start("sdw-red3", &["w1"], true);

    for round in 1..=2 {
        let topology_before = case.live_panes();
        let (output, value) = case.start_agent_force("w1");
        let topology_after = case.live_panes();
        let ok = command_reported_ok(value.as_ref());
        if !ok {
            assert_eq!(
                topology_after,
                topology_before,
                "RED3 round {round}: a refused start-agent must not change the physical \
                 topology at all; before={topology_before:?} after={topology_after:?} \
                 stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let w1_panes = case.live_panes_for_window("w1");
        assert_eq!(
            w1_panes.len(),
            1,
            "RED3 round {round}: repeated --force must converge to exactly one live \
             w1 pane, not accumulate one pane per invocation; got {w1_panes:?}"
        );
    }
    case.shutdown();
}
