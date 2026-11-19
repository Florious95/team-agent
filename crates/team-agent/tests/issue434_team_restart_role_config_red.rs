//! Issue #434 RED (public CLI journey): a whole-team restart must persist the role file
//! edited on disk into the seat row in state.json, not only into the respawned argv.
//!
//! Scenario card (public CLI only), run for both `team restart` and bare `restart`:
//!   1. `.team/current/agents/worker.md` declares `dangerously_skip_permissions: false`,
//!      `model: gpt-5.5` and no profile; `team-agent start --from .team/current ...`.
//!   2. The operator edits that file on disk to `dangerously_skip_permissions: true`,
//!      `model: gpt-5.4` and `profile: alt`.
//!   3. The team restart command exits 0.
//!   4. Expected: the respawned argv AND the seat row (`model`, `profile`,
//!      `effective_approval_policy`, `as_launched_dangerously_skip_permissions`) carry the
//!      edited declaration. Actual (defect): the argv is fresh but the row keeps the stale
//!      launch-time `model`, `profile` and `effective_approval_policy`.
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

const TEAM: &str = "restartcfg";
const AGENT: &str = "worker";
const OLD_MODEL: &str = "gpt-5.5";
const NEW_MODEL: &str = "gpt-5.4";
const NEW_PROFILE: &str = "alt";
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
        fs::create_dir_all(team_dir.join("profiles")).unwrap();
        fs::write(team_dir.join("TEAM.md"), "Coordinate the controlled team.\n").unwrap();
        // A subscription profile exports no provider env; only its selection is observable.
        fs::write(team_dir.join("profiles").join(format!("{NEW_PROFILE}.env")), "AUTH_MODE=subscription\n").unwrap();
        fs::write(team_dir.join("agents").join(format!("{AGENT}.md")), role(false, OLD_MODEL, None)).unwrap();
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
            "[i434] $ {}\n[i434]   exit={}\n[i434]   stdout={}\n[i434]   stderr={}",
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
            &format!("[i434] {phase}: public copilot wrapper recorded the real launch argv at {}", path.display()),
            || path.is_file(),
            Duration::from_secs(30),
        );
        let argv: Vec<String> = fs::read(&path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect();
        eprintln!("[i434] {phase}: launch argv = {argv:?}");
        argv
    }

    fn row(&self, phase: &str) -> Value {
        let row = self.ws.read_state()["agents"][AGENT].clone();
        eprintln!(
            "[i434] {phase}: state.json={} agents.{AGENT} = {}",
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

fn role(bypass: bool, model: &str, profile: Option<&str>) -> String {
    let profile = profile.map(|name| format!("profile: {name}\n")).unwrap_or_default();
    format!(
        "---\nprovider: copilot\nmodel: {model}\n{profile}dangerously_skip_permissions: {bypass}\ntools: [mcp_team]\n---\nHandle the assigned message.\n"
    )
}

fn model_of(argv: &[String]) -> Option<&str> {
    argv.windows(2).find(|pair| pair[0] == "--model").map(|pair| pair[1].as_str())
}

fn bypass_count(argv: &[String]) -> usize {
    argv.iter().filter(|arg| *arg == BYPASS_FLAG).count()
}

fn team_restart_persists_edited_role(tag: &str, restart: &[&str]) {
    let fx = Fixture::new(tag);

    // Step 1: launch the team with bypass=false / model=gpt-5.5 / no profile declared on disk.
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
        "[i434] fixture precondition: team start must launch {AGENT}\nstdout={}\nstderr={}",
        started.stdout,
        started.stderr
    );
    let launch_argv = fx.wait_argv("after start");
    let launch_row = fx.row("after start");
    // Fixture control: the original declaration is what was launched and recorded.
    assert_eq!(
        (bypass_count(&launch_argv), model_of(&launch_argv)),
        (0, Some(OLD_MODEL)),
        "[i434] fixture precondition: initial launch must honor bypass=false/model={OLD_MODEL}; argv={launch_argv:?}"
    );
    assert_eq!(
        (
            launch_row["as_launched_dangerously_skip_permissions"].clone(),
            launch_row["effective_approval_policy"]["enabled"].clone(),
            launch_row["model"].clone(),
            launch_row["profile"].clone(),
        ),
        (Value::Bool(false), Value::Bool(false), Value::String(OLD_MODEL.into()), Value::Null),
        "[i434] fixture precondition: initial seat row must record the original declaration; row={launch_row}"
    );

    // Step 2: edit the role file on disk.
    let edited = role(true, NEW_MODEL, Some(NEW_PROFILE));
    fs::write(fx.role_path(), &edited).unwrap();
    eprintln!("[i434] edited role file {}:\n{edited}", fx.role_path().display());
    fs::remove_file(fx.argv_path()).unwrap();

    // Step 3: restart the whole team.
    let mut args = restart.to_vec();
    args.extend(["--workspace", fx.root(), "--team", TEAM, "--json"]);
    let restarted = fx.run(&args);
    assert!(
        restarted.is_success(),
        "[i434] `{}` must succeed on an idle team\nexit={}\nstdout={}\nstderr={}",
        args.join(" "),
        restarted.exit_code,
        restarted.stdout,
        restarted.stderr
    );

    // Step 4: the respawned process AND the seat row must carry the edited declaration.
    let restart_argv = fx.wait_argv("after restart");
    let restart_row = fx.row("after restart");
    let role_on_disk = fs::read_to_string(fx.role_path()).unwrap();
    let mut failures = Vec::new();
    if bypass_count(&restart_argv) != 1 {
        failures.push(format!(
            "respawn argv must contain `{BYPASS_FLAG}` exactly once (role file now says dangerously_skip_permissions: true); found {} occurrence(s)",
            bypass_count(&restart_argv)
        ));
    }
    if model_of(&restart_argv) != Some(NEW_MODEL) {
        failures.push(format!(
            "respawn argv must pass `--model {NEW_MODEL}` (role file now says model: {NEW_MODEL}); found --model {:?}",
            model_of(&restart_argv)
        ));
    }
    for (field, expected) in [
        ("/as_launched_dangerously_skip_permissions", Value::Bool(true)),
        ("/effective_approval_policy/enabled", Value::Bool(true)),
        ("/model", Value::String(NEW_MODEL.into())),
        ("/profile", Value::String(NEW_PROFILE.into())),
    ] {
        let actual = restart_row.pointer(field).cloned().unwrap_or(Value::Null);
        if actual != expected {
            failures.push(format!("state.json agents.{AGENT}{field} must be {expected} after team restart; found {actual}"));
        }
    }
    assert!(
        failures.is_empty(),
        "[i434] RED: `{}` did not carry the role file edited on disk into the respawned seat.\n\
         failures:\n  - {}\n\
         role file ({}):\n{role_on_disk}\n\
         launch argv  (before edit): {launch_argv:?}\n\
         restart argv (after edit):  {restart_argv:?}\n\
         seat row before restart: {}\n\
         seat row after restart:  {}\n\
         restart receipt: exit={} stdout={} stderr={}\n\
         compiled runtime specs after restart:\n{}",
        args.join(" "),
        failures.join("\n  - "),
        fx.role_path().display(),
        serde_json::to_string_pretty(&launch_row).unwrap(),
        serde_json::to_string_pretty(&restart_row).unwrap(),
        restarted.exit_code,
        restarted.stdout.trim(),
        restarted.stderr.trim(),
        fx.compiled_specs(),
    );
}

#[test]
fn team_restart_subcommand_persists_role_file_edited_on_disk() {
    team_restart_persists_edited_role("i434-team-restart", &["team", "restart"]);
}

#[test]
fn bare_restart_persists_role_file_edited_on_disk() {
    team_restart_persists_edited_role("i434-bare-restart", &["restart"]);
}
