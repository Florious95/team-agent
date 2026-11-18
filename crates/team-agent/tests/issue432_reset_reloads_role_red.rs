//! Issue #432 RED (public CLI journey): `team-agent agent reset <AGENT> --discard-session`
//! must reload the seat's role file from disk before respawning it.
//!
//! Scenario card (public CLI only):
//!   1. `.team/current/agents/worker.md` declares `dangerously_skip_permissions: false`
//!      and `model: gpt-5.5`; `team-agent start --from .team/current ...` launches it.
//!   2. The operator edits that file on disk to `dangerously_skip_permissions: true`
//!      and `model: gpt-5.4`.
//!   3. `team-agent agent reset worker --discard-session ...` exits 0.
//!   4. Expected: the respawned process argv and the seat row in state.json carry the
//!      edited declaration. Actual (defect): the stale launch-time declaration is reused.
//!
//! The provider is a public-PATH `copilot` wrapper (legacy runtime, bypass flag
//! `--allow-all`) that records its real launch argv and then execs the built-in fake
//! worker: no installed provider, authentication, or model cost. Only public artifacts
//! are asserted: exit codes, recorded process argv, and the on-disk state.json row.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

#[path = "e2e/framework.rs"]
mod framework;
use framework::*;

const TEAM: &str = "resetcfg";
const AGENT: &str = "worker";
const OLD_MODEL: &str = "gpt-5.5";
const NEW_MODEL: &str = "gpt-5.4";
const BYPASS_FLAG: &str = "--allow-all";

struct Fixture {
    ws: TestWorkspace,
    path: String,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag).with_owned_caller();
        let root = ws.path().to_path_buf();
        let team_dir = root.join(".team").join("current");
        fs::create_dir_all(team_dir.join("agents")).unwrap();
        fs::write(team_dir.join("TEAM.md"), "Coordinate the controlled team.\n").unwrap();
        fs::write(team_dir.join("agents").join(format!("{AGENT}.md")), role(false, OLD_MODEL)).unwrap();
        let bin = root.join("test-bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.join(".role-config-launch")).unwrap();
        let script = format!(
            "#!/usr/bin/env bash\nset -eu\ncase \"${{1:-}}\" in\n --version) echo 'copilot-test-target 1.0.0'; exit 0 ;;\n --list-models) printf '{OLD_MODEL}\\n{NEW_MODEL}\\n'; exit 0 ;;\nesac\nif [ -z \"${{TEAM_AGENT_AGENT_ID:-}}\" ] || [ -z \"${{TEAM_AGENT_WORKSPACE:-}}\" ]; then exit 0; fi\nprintf '%s\\0' 'copilot' \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-config-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\nexec -a copilot {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n",
            quote(&ta_binary())
        );
        let executable = bin.join("copilot");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        Self { ws, path }
    }

    fn root(&self) -> &str {
        self.ws.path().to_str().unwrap()
    }

    fn team_dir(&self) -> PathBuf {
        self.ws.path().join(".team").join("current")
    }

    fn role_path(&self) -> PathBuf {
        self.team_dir().join("agents").join(format!("{AGENT}.md"))
    }

    fn argv_path(&self) -> PathBuf {
        self.ws.path().join(format!(".role-config-launch/{AGENT}.argv"))
    }

    fn run(&self, args: &[&str]) -> TaResult {
        let result = run_ta_env(&self.ws, args, &[("PATH", &self.path)]);
        eprintln!(
            "[i432] $ {}\n[i432]   exit={}\n[i432]   stdout={}\n[i432]   stderr={}",
            result.argv.join(" "),
            result.exit_code,
            result.stdout.trim(),
            result.stderr.trim()
        );
        result
    }

    fn wait_argv(&self, phase: &str) -> Vec<String> {
        let path = self.argv_path();
        wait_for_or_panic(
            &format!("[i432] {phase}: public copilot wrapper recorded the real launch argv at {}", path.display()),
            || path.is_file(),
            Duration::from_secs(20),
        );
        let bytes = fs::read(&path).unwrap();
        let argv: Vec<String> = bytes
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect();
        eprintln!("[i432] {phase}: launch argv = {argv:?}");
        argv
    }

    fn row(&self, phase: &str) -> Value {
        let state = self.ws.read_state();
        let row = state["agents"][AGENT].clone();
        eprintln!(
            "[i432] {phase}: state.json={} agents.{AGENT} = {}",
            self.ws.state_json_path().display(),
            serde_json::to_string_pretty(&row).unwrap()
        );
        row
    }

    fn compiled_specs(&self) -> String {
        let runtime = self.ws.path().join(".team").join("runtime");
        let mut out = String::new();
        if let Ok(entries) = fs::read_dir(&runtime) {
            for entry in entries.flatten() {
                let spec = entry.path().join("team.spec.yaml");
                if let Ok(text) = fs::read_to_string(&spec) {
                    out.push_str(&format!("--- {} ---\n{text}\n", spec.display()));
                }
            }
        }
        if out.is_empty() {
            out.push_str(&format!("(no compiled team.spec.yaml under {})", runtime.display()));
        }
        out
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn role(bypass: bool, model: &str) -> String {
    format!(
        "---\nprovider: copilot\nmodel: {model}\ndangerously_skip_permissions: {bypass}\ntools: [mcp_team]\n---\nHandle the assigned message.\n"
    )
}

fn model_of(argv: &[String]) -> Option<&str> {
    argv.windows(2).find(|pair| pair[0] == "--model").map(|pair| pair[1].as_str())
}

fn bypass_count(argv: &[String]) -> usize {
    argv.iter().filter(|arg| *arg == BYPASS_FLAG).count()
}

#[test]
fn agent_reset_discard_session_respawns_with_role_file_edited_on_disk() {
    let fx = Fixture::new("i432-reset-role");

    // Step 1: launch the team with bypass=false / model=gpt-5.5 declared on disk.
    let team_dir = fx.team_dir();
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
        "[i432] fixture precondition: team start must launch {AGENT}\nstdout={}\nstderr={}",
        started.stdout,
        started.stderr
    );
    let launch_argv = fx.wait_argv("after start");
    let launch_row = fx.row("after start");
    // Fixture control: the original declaration is what was launched.
    assert_eq!(
        (bypass_count(&launch_argv), model_of(&launch_argv)),
        (0, Some(OLD_MODEL)),
        "[i432] fixture precondition: initial launch must honor bypass=false/model={OLD_MODEL}; argv={launch_argv:?}"
    );
    assert_eq!(
        launch_row["as_launched_dangerously_skip_permissions"],
        Value::Bool(false),
        "[i432] fixture precondition: initial seat row must record bypass=false; row={launch_row}"
    );

    // Step 2: edit the role file on disk.
    let edited = role(true, NEW_MODEL);
    fs::write(fx.role_path(), &edited).unwrap();
    eprintln!("[i432] edited role file {}:\n{edited}", fx.role_path().display());
    fs::remove_file(fx.argv_path()).unwrap();

    // Step 3: reset the seat, discarding its session.
    let reset = fx.run(&[
        "agent",
        "reset",
        AGENT,
        "--discard-session",
        "--workspace",
        fx.root(),
        "--team",
        TEAM,
        "--json",
    ]);
    assert!(
        reset.is_success(),
        "[i432] `agent reset {AGENT} --discard-session` must succeed on an idle seat\nexit={}\nstdout={}\nstderr={}",
        reset.exit_code,
        reset.stdout,
        reset.stderr
    );

    // Step 4: the respawned process and the seat row must carry the edited declaration.
    let reset_argv = fx.wait_argv("after reset");
    let reset_row = fx.row("after reset");
    let role_on_disk = fs::read_to_string(fx.role_path()).unwrap();
    let mut failures = Vec::new();
    if bypass_count(&reset_argv) != 1 {
        failures.push(format!(
            "respawn argv must contain `{BYPASS_FLAG}` exactly once (role file now says dangerously_skip_permissions: true); found {} occurrence(s)",
            bypass_count(&reset_argv)
        ));
    }
    if model_of(&reset_argv) != Some(NEW_MODEL) {
        failures.push(format!(
            "respawn argv must pass `--model {NEW_MODEL}` (role file now says model: {NEW_MODEL}); found --model {:?}",
            model_of(&reset_argv)
        ));
    }
    for (field, expected) in [
        ("/as_launched_dangerously_skip_permissions", Value::Bool(true)),
        ("/effective_approval_policy/enabled", Value::Bool(true)),
        ("/model", Value::String(NEW_MODEL.into())),
    ] {
        let actual = reset_row.pointer(field).cloned().unwrap_or(Value::Null);
        if actual != expected {
            failures.push(format!(
                "state.json agents.{AGENT}{field} must be {expected} after reset; found {actual}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "[i432] RED: `agent reset {AGENT} --discard-session` respawned with the STALE launch-time declaration instead of the role file on disk.\n\
         failures:\n  - {}\n\
         role file ({}):\n{role_on_disk}\n\
         launch argv (before edit): {launch_argv:?}\n\
         reset argv  (after edit):  {reset_argv:?}\n\
         seat row before reset: {}\n\
         seat row after reset:  {}\n\
         reset receipt: exit={} stdout={} stderr={}\n\
         compiled runtime specs after reset:\n{}",
        failures.join("\n  - "),
        fx.role_path().display(),
        serde_json::to_string_pretty(&launch_row).unwrap(),
        serde_json::to_string_pretty(&reset_row).unwrap(),
        reset.exit_code,
        reset.stdout.trim(),
        reset.stderr.trim(),
        fx.compiled_specs(),
    );
}
