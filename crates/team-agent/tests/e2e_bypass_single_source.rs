//! Bypass single-source security, using public-PATH Copilot wrappers to capture
//! actual launch argv without installed-provider, authentication, or model costs.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
#[path = "e2e/framework.rs"]
mod framework;
use framework::*;

struct Fixture {
    ws: TestWorkspace,
    path: String,
}
impl Fixture {
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag).with_owned_caller();
        let root = ws.path();
        fs::create_dir_all(root.join("agents")).unwrap();
        fs::write(root.join("TEAM.md"), "Coordinate the controlled team.\n").unwrap();
        for (name, enabled) in [("worker_true", true), ("worker_false", false)] {
            fs::write(root.join(format!("agents/{name}.md")), role(enabled)).unwrap();
        }
        let bin = root.join("test-bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.join(".role-config-launch")).unwrap();
        let script = format!("#!/usr/bin/env bash\nset -eu\ncase \"${{1:-}}\" in\n --version) echo 'copilot-test-target 1.0.0'; exit 0 ;;\n --list-models) printf 'gpt-5.5\\n'; exit 0 ;;\nesac\nif [ -z \"${{TEAM_AGENT_AGENT_ID:-}}\" ] || [ -z \"${{TEAM_AGENT_WORKSPACE:-}}\" ]; then exit 0; fi\nprintf '%s\\0' 'copilot' \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-config-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\nexec -a copilot {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n", quote(&ta_binary()));
        let executable = bin.join("copilot");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Self { ws, path }
    }
    fn run(&self, args: &[&str]) -> TaResult {
        run_ta_env(&self.ws, args, &[("PATH", &self.path)])
    }
    fn launch(&self) {
        let root = self.ws.path().to_str().unwrap();
        let team = self.ws.path().file_name().unwrap().to_str().unwrap();
        let output = self.run(&[
            "quick-start",
            root,
            "--workspace",
            root,
            "--team-id",
            team,
            "--yes",
            "--no-display",
            "--json",
        ]);
        assert!(
            quick_start_workers_available(&output),
            "{} {}",
            output.stdout,
            output.stderr
        );
        self.assert_policy("worker_true", true);
        self.assert_policy("worker_false", false);
    }
    fn argv_path(&self, agent: &str) -> std::path::PathBuf {
        self.ws
            .path()
            .join(format!(".role-config-launch/{agent}.argv"))
    }
    fn assert_policy(&self, agent: &str, enabled: bool) {
        let path = self.argv_path(agent);
        wait_for_or_panic(
            "public Copilot wrapper captured actual startup argv",
            || path.is_file(),
            Duration::from_secs(12),
        );
        let bytes = fs::read(path).unwrap();
        let args: Vec<_> = bytes
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect();
        assert_eq!(args[0], "copilot");
        assert_eq!(
            args.iter().filter(|arg| *arg == "--allow-all").count(),
            usize::from(enabled),
            "launch argv: {args:?}"
        );
        let state = self.ws.read_state();
        let row = &state["agents"][agent];
        assert_eq!(row["provider"], "copilot");
        assert_eq!(row["as_launched_dangerously_skip_permissions"], enabled);
        assert_eq!(row["effective_approval_policy"]["enabled"], enabled);
        if enabled {
            assert_eq!(row["effective_approval_policy"]["source"], "runtime_config");
            assert_eq!(row["effective_approval_policy"]["flag"], "--allow-all");
        }
    }
    fn edit_true_role(&self, enabled: bool) {
        fs::write(self.ws.path().join("agents/worker_true.md"), role(enabled)).unwrap();
    }
}
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
fn role(enabled: bool) -> String {
    format!("---\nprovider: copilot\nmodel: gpt-5.5\ndangerously_skip_permissions: {enabled}\ntools: [mcp_team]\n---\nHandle the assigned message.\n")
}

#[test]
fn true_and_false_roles_match_real_launch_argv_and_recorded_policy() {
    let fixture = Fixture::new("bypass-alignment");
    fixture.launch();
}

#[test]
fn editing_role_text_does_not_rewrite_the_live_process_policy() {
    let fixture = Fixture::new("bypass-live-drift");
    fixture.launch();
    let argv = fs::read(fixture.argv_path("worker_true")).unwrap();
    fixture.edit_true_role(false);
    fixture.assert_policy("worker_true", true);
    assert_eq!(fs::read(fixture.argv_path("worker_true")).unwrap(), argv);
    // A read-only start plan must reject changed launch policy, without a respawn.
    let plan = fixture.run(&["start", "worker_true", "--dry-run", "--json"]);
    assert!(
        !plan.is_success(),
        "changed policy must not be silently adopted: {}",
        plan.stdout
    );
    assert!(format!("{}{}", plan.stdout, plan.stderr).contains("drift"));
    assert_eq!(fs::read(fixture.argv_path("worker_true")).unwrap(), argv);
    fixture.assert_policy("worker_true", true);
}

#[test]
fn explicit_stop_then_start_reapplies_the_edited_role_policy() {
    let fixture = Fixture::new("bypass-reapply");
    fixture.launch();
    fixture.edit_true_role(false);
    let stopped = fixture.run(&["stop", "worker_true", "--json"]);
    assert!(
        stopped.is_success(),
        "{} {}",
        stopped.stdout,
        stopped.stderr
    );
    fs::remove_file(fixture.argv_path("worker_true")).unwrap();
    let started = fixture.run(&["start", "worker_true", "--session", "auto", "--json"]);
    assert!(
        started.is_success(),
        "{} {}",
        started.stdout,
        started.stderr
    );
    fixture.assert_policy("worker_true", false);
}

#[test]
fn removed_force_flag_cannot_bypass_restart_admission() {
    let fixture = Fixture::new("bypass-force-refusal");
    fixture.launch();
    fixture.edit_true_role(false);
    let before = fs::read(fixture.ws.state_json_path()).unwrap();
    let argv = fs::read(fixture.argv_path("worker_true")).unwrap();
    let refused = fixture.run(&["restart", "--force", "--json"]);
    assert_eq!(refused.exit_code, 2);
    assert_eq!(fs::read(fixture.ws.state_json_path()).unwrap(), before);
    assert_eq!(fs::read(fixture.argv_path("worker_true")).unwrap(), argv);
    fixture.assert_policy("worker_true", true);
}
