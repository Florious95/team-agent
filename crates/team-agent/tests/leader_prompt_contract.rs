//! Public host-global storage and pre-spawn typed-refusal contracts, not live inference.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "support/hermetic.rs"]
mod hermetic;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static ID: AtomicU64 = AtomicU64::new(0);
const SENTINEL: &str = "LEADER_ONLY_SENTINEL_309_🙂";
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ta-leader-prompt-cli-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let home = root.join("home");
        let cwd = root.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir(&cwd).unwrap();
        Self { root, home, cwd }
    }
    fn path(&self) -> PathBuf {
        self.home.join(".team-agent/leader-prompt.txt")
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .current_dir(&self.cwd)
            .args(args)
            .env("HOME", &self.home)
            .env_remove("USERPROFILE")
            .env("TEAM_AGENT_CLI_ARGV_ROUTING", "off");
        for key in hermetic::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(
            !self.cwd.join(".team").exists(),
            "global command created workspace apparatus: {args:?}"
        );
        output
    }
    fn set(&self, text: &str) {
        let output = self.run(&["leader-prompt", "set", "--json", "--", text]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(text));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn leader_prompt_absent_show_clear_help_do_not_create_any_host_or_workspace_files() {
    let f = Fixture::new();
    for args in [
        vec!["leader-prompt"],
        vec!["leader-prompt", "show"],
        vec!["leader-prompt", "clear", "--json"],
        vec!["leader-prompt", "--help"],
    ] {
        let output = f
            .command(&args)
            .env("PATH", f.root.join("no-tools"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!f.home.join(".team-agent").exists());
        assert!(!f.cwd.join(".team").exists());
    }
    let output = f.run(&["leader-prompt", "show", "--json"]);
    let value = json(&output);
    assert_eq!(value["configured"], false);
    assert_eq!(value["prompt"], "");
    assert_eq!(value["config_path"], f.path().to_str().unwrap());
}
#[test]
fn leader_prompt_persists_exact_bytes_across_processes_and_cwds_without_mutation_echo() {
    let f = Fixture::new();
    let text = "  literal --help --json\r\nquote\"\\\t🙂\n";
    f.set(text);
    assert_eq!(f.run(&["leader-prompt"]).stdout, text.as_bytes());
    let other = f.root.join("other-project");
    fs::create_dir(&other).unwrap();
    let output = f
        .command(&["leader-prompt", "show", "--json"])
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(json(&output)["prompt"], text);
    assert!(!other.join(".team").exists());
    let output = f.run(&["leader-prompt", "append", "--json", "--", "--help"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("--help"));
    assert_eq!(
        fs::read(f.path()).unwrap(),
        format!("{text}\n\n--help").as_bytes()
    );
    let output = f.run(&["leader-prompt", "set", "--", "--json"]);
    assert!(output.status.success());
    assert_eq!(fs::read(f.path()).unwrap(), b"--json");
    let input = f.root.join("input with 中文.txt");
    fs::write(&input, text).unwrap();
    let output = f.run(&["leader-prompt", "set", "--file", input.to_str().unwrap()]);
    assert!(output.status.success());
    fs::write(&input, "changed source, not a permanent reference").unwrap();
    assert_eq!(f.run(&["leader-prompt", "show"]).stdout, text.as_bytes());
    assert!(f.run(&["leader-prompt", "clear"]).status.success());
    assert!(!f.path().exists());
}
#[test]
fn leader_prompt_errors_keep_old_text_and_report_actual_io_without_echo() {
    let f = Fixture::new();
    f.set(SENTINEL);
    for args in [
        vec!["leader-prompt", "set", "--json", "--", ""],
        vec!["leader-prompt", "append", "--json", "--", "one", "two"],
        vec!["leader-prompt", "set", "--json", "--unknown"],
        vec!["leader-prompt", "show", "--json", "--json"],
    ] {
        let output = f.run(&args);
        assert!(!output.status.success());
        assert_eq!(json(&output)["reason"], "leader_prompt_invalid_input");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
        assert_eq!(fs::read_to_string(f.path()).unwrap(), SENTINEL);
    }
    let missing = f.root.join("missing");
    let output = f.run(&[
        "leader-prompt",
        "set",
        "--json",
        "--file",
        missing.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_eq!(json(&output)["io_kind"], "NotFound");
    assert_eq!(json(&output)["config_path"], f.path().to_str().unwrap());
    assert_eq!(json(&output)["io_path"], missing.to_str().unwrap());
    let input = f.root.join("bad-utf8");
    fs::write(&input, [0xff]).unwrap();
    assert!(!f
        .run(&["leader-prompt", "set", "--file", input.to_str().unwrap()])
        .status
        .success());
    fs::write(&input, b"nul\0bytes").unwrap();
    assert!(!f
        .run(&["leader-prompt", "append", "--file", input.to_str().unwrap()])
        .status
        .success());
    assert_eq!(fs::read_to_string(f.path()).unwrap(), SENTINEL);
    fs::write(f.path(), [0xff]).unwrap();
    assert_eq!(
        json(&f.run(&["leader-prompt", "append", "--json", "--", "new"]))["reason"],
        "leader_prompt_unreadable"
    );
    assert_eq!(fs::read(f.path()).unwrap(), [0xff]);
    f.set(SENTINEL); // set repairs corrupt text without first decoding it.
    fs::write(f.path(), [0xff]).unwrap();
    assert!(f.run(&["leader-prompt", "clear"]).status.success());
}
#[test]
fn leader_prompt_multiple_cli_appends_serialize_without_lost_updates() {
    let f = Fixture::new();
    let mut children = Vec::new();
    for id in 0..8 {
        children.push(
            f.command(&[
                "leader-prompt",
                "append",
                "--json",
                "--",
                &format!("entry-{id}"),
            ])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let body = fs::read_to_string(f.path()).unwrap();
    let actual: std::collections::BTreeSet<String> =
        body.split("\n\n").map(str::to_string).collect();
    assert_eq!(actual, (0..8).map(|id| format!("entry-{id}")).collect());
    assert_eq!(body.split("\n\n").count(), 8);
}
#[test]
fn leader_prompt_management_requires_home_and_never_uses_workspace_fallback() {
    let f = Fixture::new();
    let absent_home = f.root.join("missing-home");
    let output = f
        .command(&["leader-prompt", "set", "--json", "--", "text"])
        .env("HOME", &absent_home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(json(&output)["reason"], "leader_prompt_write_failed");
    assert_eq!(json(&output)["io_kind"], "NotFound");
    assert_eq!(
        json(&output)["config_path"],
        absent_home
            .join(".team-agent/leader-prompt.txt")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        json(&output)["io_path"],
        absent_home.join(".team-agent").to_str().unwrap()
    );
    assert!(!absent_home.exists());
    for home in ["", "relative"] {
        let output = f
            .command(&["leader-prompt", "show", "--json"])
            .env("HOME", home)
            .output()
            .unwrap();
        #[cfg(not(windows))]
        assert_eq!(json(&output)["reason"], "leader_prompt_home_unavailable");
        assert!(!f.cwd.join(".team").exists());
    }
}

#[cfg(unix)]
#[test]
fn leader_prompt_permission_failures_are_typed_and_preserve_old_body() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let f = Fixture::new();
    f.set(SENTINEL);
    // Root builders use an unprivileged child, not a permission-test skip.
    let cli = f.root.join("team-agent");
    fs::copy(env!("CARGO_BIN_EXE_team-agent"), &cli).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    for path in [&f.root, &f.home, &f.cwd] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |args: &[&str]| {
        let mut command = Command::new(&cli);
        command
            .args(args)
            .current_dir(&f.cwd)
            .env("HOME", &f.home)
            .env_remove("USERPROFILE");
        for key in hermetic::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        if unsafe { libc::geteuid() } == 0 {
            command.gid(65534).uid(65534);
        }
        command.output().unwrap()
    };
    fs::set_permissions(
        f.path().parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o000)).unwrap();
    let output = run(&["leader-prompt", "show", "--json"]);
    assert!(!output.status.success());
    assert_eq!(json(&output)["reason"], "leader_prompt_unreadable");
    assert_eq!(json(&output)["io_kind"], "PermissionDenied");
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(
        f.path().parent().unwrap(),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let output = run(&["leader-prompt", "set", "--json", "--", "new text"]);
    assert!(!output.status.success());
    assert_eq!(json(&output)["reason"], "leader_prompt_write_failed");
    assert_eq!(json(&output)["io_kind"], "PermissionDenied");
    assert_eq!(fs::read_to_string(f.path()).unwrap(), SENTINEL);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
    fs::set_permissions(
        f.path().parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[cfg(unix)]
fn native_canaries(f: &Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = f.root.join("native-canaries");
    fs::create_dir(&bin).unwrap();
    for name in ["pi", "codex", "claude", "grok", "agent", "copilot", "tmux"] {
        let path = bin.join(name);
        let log = f.root.join(format!("{name}-executed"));
        // A missing session is a real preflight result, not a synthetic caller tuple.
        let script = if name == "tmux" {
            "#!/bin/sh\nif [ \"$1\" = -V ]; then printf 'tmux 3.3\\n'; exit 0; fi\nexit 1\n"
                .to_string()
        } else {
            format!(
                "#!/bin/sh\nprintf executed >> '{}'\nexit 77\n",
                log.display()
            )
        };
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}
#[cfg(unix)]
#[test]
fn leader_prompt_unreadable_and_unsupported_leaders_fail_before_native_spawn_or_state() {
    for provider in ["pi", "claude", "codex", "grok", "cursor", "copilot"] {
        let f = Fixture::new();
        f.set(SENTINEL);
        let bin = native_canaries(&f);
        fs::write(f.path(), [0xff]).unwrap();
        let output = f
            .command(&[provider, "--external-leader", "--json"])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{provider}: {:?}", output);
        assert_eq!(
            json(&output)["reason"],
            "leader_prompt_unreadable",
            "{provider}: {:?}",
            output
        );
        assert_eq!(json(&output)["io_kind"], "InvalidData");
        assert_eq!(json(&output)["config_path"], f.path().to_str().unwrap());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
        assert!(!f.cwd.join(".team").exists());
        for name in ["pi", "claude", "codex", "grok", "agent", "copilot"] {
            assert!(!f.root.join(format!("{name}-executed")).exists());
        }
    }
    for provider in ["cursor", "copilot"] {
        let f = Fixture::new();
        f.set(SENTINEL);
        let bin = native_canaries(&f);
        let output = f
            .command(&[provider, "--external-leader", "--json"])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert_eq!(json(&output)["reason"], "leader_prompt_unsupported");
        assert!(!output.status.success());
        assert!(!f.cwd.join(".cursor").exists());
        assert!(!f.cwd.join(".team").exists());
        assert!(!f.home.join(".team-agent/runtime/leader-prompts").exists());
    }
}
#[cfg(unix)]
#[test]
fn leader_prompt_native_slot_conflicts_are_typed_without_partial_team_state() {
    for args in [
        vec![
            "codex",
            "--external-leader",
            "--json",
            "--",
            "-c",
            "developer_instructions='unparsed'",
        ],
        vec![
            "claude",
            "--external-leader",
            "--json",
            "--",
            "--append-system-prompt",
            "one",
            "--append-system-prompt=two",
        ],
        vec![
            "grok",
            "--external-leader",
            "--json",
            "--",
            "--rules",
            "one",
            "--rules",
            "two",
        ],
    ] {
        let f = Fixture::new();
        f.set(SENTINEL);
        let bin = native_canaries(&f);
        let output = f.command(&args).env("PATH", &bin).output().unwrap();
        assert_eq!(
            json(&output)["reason"],
            "leader_prompt_conflict",
            "{:?}",
            output
        );
        assert!(!output.status.success());
        assert!(!f.cwd.join(".team").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
    }
}
