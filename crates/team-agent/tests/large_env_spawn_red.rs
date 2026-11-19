//! Large inherited environment RED (public CLI journey): spawning a worker must not depend
//! on the size of the operator's exported environment, and inherited values must not be
//! published on the worker pane's command line.
//!
//! Defect (v0.6.2): the legacy tmux spawn inlines every inherited `KEY='value'` into the one
//! `tmux new-session/new-window` command. Once the environment nears the ~16 KiB tmux command
//! envelope, the spawn is refused with `command_too_long ... limit_bytes=16000`; below that,
//! every inherited value (tokens included) is readable in `#{pane_start_command}` and in the
//! pane shell's process argv.
//!
//! Scenario card (public CLI only):
//!   1. `.team/current` declares two Claude workers: the first spawn opens the session
//!      (new-session), the second joins it (new-window).
//!   2. The operator's environment carries ~20 KB of exported variables (more than the whole
//!      tmux envelope), a token-shaped secret, a value full of shell metacharacters, and
//!      `CLAUDE_EFFORT`, which Team Agent must never hand to a worker.
//!   3. `team-agent start --from .team/current ...` must succeed. Each worker process sees
//!      every operator variable byte-for-byte plus its `TEAM_AGENT_*` overlay, and
//!      `CLAUDE_EFFORT` stays unset.
//!   4. No operator value appears in any worker pane's `#{pane_start_command}` or in the
//!      pane process argv.
//!   5. `team-agent restart --allow-fresh` respawns both workers under the same contract.
//!
//! The provider is a public-PATH `claude` wrapper that records its real environment
//! (NUL-separated) and execs the built-in fake worker: no installed provider, auth, or
//! model cost. Only public artifacts are asserted: exit codes, the recorded worker
//! environment, state.json, tmux pane metadata, and `ps`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[path = "e2e/framework.rs"]
mod framework;
use framework::*;

const TEAM: &str = "largeenv";
const AGENTS: [&str; 2] = ["alpha", "beta"];
const PAD_VARS: usize = 40;
const LAUNCH_DIR: &str = ".large-env-launch";
/// Inherited by the operator shell; Team Agent must unset it for every worker.
const UNSET_KEY: &str = "CLAUDE_EFFORT";

struct Fixture {
    ws: TestWorkspace,
    path: String,
    operator_env: Vec<(String, String)>,
    secret: String,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag).with_owned_caller();
        let root = ws.path().to_path_buf();
        let team_dir = root.join(".team").join("current");
        fs::create_dir_all(team_dir.join("agents")).unwrap();
        fs::write(team_dir.join("TEAM.md"), "Coordinate the controlled team.\n").unwrap();
        for agent in AGENTS {
            fs::write(
                team_dir.join("agents").join(format!("{agent}.md")),
                "---\nprovider: claude\ndangerously_skip_permissions: false\ntools: [mcp_team]\n---\nHandle the assigned message.\n",
            )
            .unwrap();
        }
        fs::create_dir_all(root.join(LAUNCH_DIR)).unwrap();
        let bin = root.join("test-bin");
        fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/usr/bin/env bash\nset -eu\nif [ -n \"${{TEAM_AGENT_AGENT_ID:-}}\" ] && [ -n \"${{TEAM_AGENT_WORKSPACE:-}}\" ]; then\n  out=\"${{TEAM_AGENT_WORKSPACE}}/{LAUNCH_DIR}/${{TEAM_AGENT_AGENT_ID}}.env\"\n  env -0 > \"$out.tmp\" && mv \"$out.tmp\" \"$out\"\n  exec -a claude {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\nfi\ncase \"${{1:-}}\" in\n  --version|-v) echo '2.1.309 (Claude Code)'; exit 0 ;;\nesac\nexit 0\n",
            quote(&ta_binary())
        );
        let executable = bin.join("claude");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let secret = format!("sk-largeenv-{:x}{nanos:x}", std::process::id());
        let mut operator_env: Vec<(String, String)> = (0..PAD_VARS)
            .map(|i| (format!("LARGE_ENV_PAD_{i:02}"), format!("pad{i:02}:{}:end{i:02}", "0123456789abcdef".repeat(30))))
            .collect();
        operator_env.push(("LARGE_ENV_FAKE_API_TOKEN".into(), secret.clone()));
        operator_env.push((
            "LARGE_ENV_QUOTING".into(),
            "it's \"quoted\" $HOME `tick` back\\slash ; & | * ? ~ # %s\nsecond line".into(),
        ));
        Self { ws, path, operator_env, secret }
    }

    fn root(&self) -> &str {
        self.ws.path().to_str().unwrap()
    }

    fn operator_bytes(&self) -> usize {
        self.operator_env.iter().map(|(k, v)| k.len() + v.len() + 2).sum()
    }

    fn run(&self, args: &[&str]) -> TaResult {
        let mut env: Vec<(&str, &str)> = vec![("PATH", self.path.as_str()), (UNSET_KEY, "max")];
        env.extend(self.operator_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let result = run_ta_env(&self.ws, args, &env);
        eprintln!(
            "[largeenv] $ {}\n[largeenv]   exit={}\n[largeenv]   stdout={}\n[largeenv]   stderr={}",
            result.argv.join(" "),
            result.exit_code,
            result.stdout.trim(),
            result.stderr.trim()
        );
        result
    }

    fn env_path(&self, agent: &str) -> PathBuf {
        self.ws.path().join(LAUNCH_DIR).join(format!("{agent}.env"))
    }

    fn worker_env(&self, phase: &str, agent: &str) -> BTreeMap<String, String> {
        let path = self.env_path(agent);
        wait_for_or_panic(
            &format!("[largeenv] {phase}: claude wrapper recorded {agent}'s process environment at {}", path.display()),
            || path.is_file(),
            Duration::from_secs(30),
        );
        fs::read(&path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| {
                let entry = String::from_utf8_lossy(entry);
                entry.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect()
    }

    /// Every check of the contract for one phase; returns the failures.
    fn check(&self, phase: &str) -> Vec<String> {
        let mut failures = Vec::new();
        let state = self.ws.read_state();
        let socket = state["tmux_socket"].as_str().unwrap_or_default().to_string();
        let workspace = fs::canonicalize(self.ws.path()).unwrap();
        for agent in AGENTS {
            let env = self.worker_env(phase, agent);
            for (key, expected) in &self.operator_env {
                match env.get(key) {
                    Some(actual) if actual == expected => {}
                    Some(actual) => failures.push(format!(
                        "{phase}: {agent} saw {key} altered ({} bytes, expected {}): {:?}",
                        actual.len(),
                        expected.len(),
                        actual.chars().take(80).collect::<String>()
                    )),
                    None => failures.push(format!("{phase}: {agent} did not inherit operator variable {key}")),
                }
            }
            if env.get("TEAM_AGENT_AGENT_ID").map(String::as_str) != Some(agent) {
                failures.push(format!("{phase}: {agent} TEAM_AGENT_AGENT_ID={:?}", env.get("TEAM_AGENT_AGENT_ID")));
            }
            let seen_ws = env.get("TEAM_AGENT_WORKSPACE").and_then(|p| fs::canonicalize(p).ok());
            if seen_ws.as_deref() != Some(workspace.as_path()) {
                failures.push(format!(
                    "{phase}: {agent} TEAM_AGENT_WORKSPACE={:?}, expected {}",
                    env.get("TEAM_AGENT_WORKSPACE"),
                    workspace.display()
                ));
            }
            if let Some(value) = env.get(UNSET_KEY) {
                failures.push(format!("{phase}: {agent} must not inherit {UNSET_KEY}; found {value:?}"));
            }

            let row = state.pointer(&format!("/agents/{agent}")).cloned().unwrap_or(Value::Null);
            let pane_id = row["pane_id"].as_str().unwrap_or_default();
            let start_command = tmux_query(&socket, pane_id, "#{pane_start_command}");
            let pane_argv = row["pane_pid"].as_u64().map(process_argv).unwrap_or_default();
            if start_command.trim().is_empty() || pane_argv.trim().is_empty() {
                failures.push(format!(
                    "{phase}: {agent} pane metadata unreadable (socket={socket:?} pane_id={pane_id:?} pane_pid={}): start_command={start_command:?} argv={pane_argv:?}",
                    row["pane_pid"]
                ));
            }
            for (surface, text) in [("#{pane_start_command}", &start_command), ("pane process argv", &pane_argv)] {
                let leaked: Vec<&str> = self
                    .operator_env
                    .iter()
                    .filter(|(_, value)| text.contains(value.as_str()))
                    .map(|(key, _)| key.as_str())
                    .collect();
                if text.contains(&self.secret) || !leaked.is_empty() {
                    failures.push(format!(
                        "{phase}: {agent} {surface} publishes inherited values for {} operator variable(s) (secret leaked: {}): {leaked:?}",
                        leaked.len(),
                        text.contains(&self.secret)
                    ));
                }
            }
            eprintln!(
                "[largeenv] {phase}: {agent} pane_id={pane_id} {} bytes in #{{pane_start_command}}, {} bytes in pane argv",
                start_command.len(),
                pane_argv.len()
            );
        }
        failures
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn tmux_query(socket: &str, target: &str, format: &str) -> String {
    if socket.is_empty() || target.is_empty() {
        return String::new();
    }
    Command::new("tmux")
        .args(["-S", socket, "display-message", "-p", "-t", target, format])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn process_argv(pid: u64) -> String {
    Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[test]
fn workers_spawn_and_respawn_under_an_operator_environment_larger_than_the_tmux_command_envelope() {
    let fx = Fixture::new("large-env-spawn");
    eprintln!(
        "[largeenv] operator environment adds {} variables / {} bytes",
        fx.operator_env.len() + 1,
        fx.operator_bytes()
    );
    assert!(fx.operator_bytes() > 16_384, "fixture precondition: the operator variables alone exceed the tmux envelope");

    // Step 1-3: launch both Claude workers from the large environment.
    let team_dir = fx.ws.path().join(".team").join("current");
    let started = fx.run(&[
        "start",
        "--from",
        team_dir.to_str().unwrap(),
        "--workspace",
        fx.root(),
        "--team",
        TEAM,
        "--no-display",
        "--json",
    ]);
    assert!(
        quick_start_workers_available(&started),
        "[largeenv] RED: `team-agent start` must launch both workers regardless of the operator's environment size \
         ({} bytes added)\nexit={}\nstdout={}\nstderr={}",
        fx.operator_bytes(),
        started.exit_code,
        started.stdout,
        started.stderr
    );
    let mut failures = fx.check("after start");

    // Step 5: respawn both workers from the same environment.
    for agent in AGENTS {
        let _ = fs::remove_file(fx.env_path(agent));
    }
    let restarted = fx.run(&["restart", "--workspace", fx.root(), "--team", TEAM, "--allow-fresh", "--json"]);
    if restarted.is_success() {
        failures.extend(fx.check("after restart"));
    } else {
        failures.push(format!(
            "after restart: `team-agent restart --allow-fresh` must respawn both workers\nexit={}\nstdout={}\nstderr={}",
            restarted.exit_code,
            restarted.stdout.trim(),
            restarted.stderr.trim()
        ));
    }

    assert!(
        failures.is_empty(),
        "[largeenv] RED: worker spawn under a large operator environment broke the contract.\nfailures:\n  - {}\nseat rows: {}",
        failures.join("\n  - "),
        serde_json::to_string_pretty(&fx.ws.read_state()["agents"]).unwrap()
    );
}
