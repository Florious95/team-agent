#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::{file_serial, serial};
use team_agent::leader::leader_session_name;
use team_agent::provider::Provider;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::Transport;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn managed_claude_leader_plan_uses_workspace_socket_tmux_not_default_server() {
    let workspace = tmp_dir("plan-socket");
    let fake = FakeLauncherTools::new(&workspace);
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let plan = team_agent::leader::leader_start_plan(
        Provider::ClaudeCode,
        &["--model".to_string(), "sonnet".to_string()],
        &workspace,
        false,
        false,
        None,
        false,
    )
    .expect("fake claude/tmux make leader_start_plan reachable");

    assert_eq!(
        plan.mode,
        team_agent::leader::LeaderStartMode::ManagedTmuxClient
    );
    // 0.3.28 Step 2: managed leader window is named after `provider_wire`
    // (e.g. `claude_code`), not the literal `leader`. Architecture parity
    // with Python `leader/__init__.py:114-131`.
    assert!(
        plan.argv.len() >= 5
            && plan.argv[0] == "tmux"
            && plan.argv[1] == "-L"
            && plan.argv[2].starts_with("ta-")
            && plan.argv.iter().any(|arg| arg == "attach-session")
            && plan.argv.iter().any(|arg| arg.ends_with(":claude_code")),
        "0.3.28 Step 2: managed leader client argv must be workspace-socketed \
         (`tmux -L ta-* attach-session -t <leader_session>:claude_code`); \
         argv={:?}",
        plan.argv
    );
}

#[test]
#[serial(env)]
fn cli_claude_json_does_not_report_success_without_starting_provider_or_tmux() {
    let workspace = tmp_dir("cli-noop");
    let fake = FakeLauncherTools::new(&workspace);
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let out = Command::new(bin())
        .args(["claude", "--json", "--", "--contract-canary"])
        .current_dir(&workspace)
        .output()
        .expect("run team-agent claude");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tmux_log = read_to_string(&fake.tmux_log);
    let provider_log = read_to_string(&fake.provider_log);
    let ok_true = output_claims_ok_true(&out);
    let started_anything = !tmux_log.trim().is_empty() || !provider_log.trim().is_empty();

    assert!(
        started_anything || !ok_true,
        "`team-agent claude --json` must either start/attach a real leader or return an honest \
         non-success result. It must not emit ok:true while doing no provider/tmux work. \
         code={:?} stdout={stdout:?} stderr={stderr:?} tmux_log={tmux_log:?} provider_log={provider_log:?}",
        out.status.code()
    );
}

#[test]
#[serial(env)]
fn cli_leader_launcher_does_not_mutate_leader_receiver_or_team_owner() {
    let workspace = tmp_dir("owner-stable");
    let before = json!({
        "active_team_key": "current",
        "session_name": "team-current",
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%old",
            "owner_epoch": 7
        },
        "team_owner": {
            "provider": "codex",
            "pane_id": "%old",
            "owner_epoch": 7,
            "claimed_via": "attach-leader"
        },
        "teams": {
            "current": {
                "session_name": "team-current",
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "provider": "codex",
                    "pane_id": "%old",
                    "owner_epoch": 7
                },
                "team_owner": {
                    "provider": "codex",
                    "pane_id": "%old",
                    "owner_epoch": 7,
                    "claimed_via": "attach-leader"
                },
                "agents": {}
            }
        },
        "agents": {}
    });
    save_runtime_state(&workspace, &before).expect("seed runtime state");
    let fake = FakeLauncherTools::new(&workspace);
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let _ = Command::new(bin())
        .args(["claude", "--external-leader", "--json"])
        .current_dir(&workspace)
        .output()
        .expect("run team-agent claude");
    let after = load_runtime_state(&workspace).expect("load runtime state");

    assert_owner_binding_unchanged(&before, &after, "/leader_receiver");
    assert_owner_binding_unchanged(&before, &after, "/team_owner");
    assert_owner_binding_unchanged(&before, &after, "/teams/current/leader_receiver");
    assert_owner_binding_unchanged(&before, &after, "/teams/current/team_owner");
}

#[test]
#[serial(env)]
fn external_leader_opt_out_is_honored_for_all_provider_passthrough_commands() {
    for command in ["codex", "claude", "copilot"] {
        let workspace = tmp_dir(&format!("external-opt-out-{command}"));
        let fake = FakeLauncherTools::new(&workspace);
        let _env = EnvGuard::set([
            (
                "PATH",
                Some(format!(
                    "{}:{}",
                    fake.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                )),
            ),
            ("TMUX", None),
            ("TMUX_PANE", None),
        ]);

        let out = Command::new(bin())
            .args([command, "--external-leader", "--json"])
            .current_dir(&workspace)
            .output()
            .unwrap_or_else(|err| panic!("run team-agent {command}: {err}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{command} --external-leader should parse and launch external topology; stdout={stdout:?} stderr={stderr:?}"
        );
        let value: Value =
            serde_json::from_slice(&out.stdout).expect("leader launcher emits json");
        assert_eq!(value["is_external_leader"], json!(true), "{command}");
        assert_eq!(value["leader_topology"], json!("external"), "{command}");
        assert_eq!(value["leader_window"], Value::Null, "{command}");
        assert_eq!(value["leader_attach_command"], Value::Null, "{command}");
        let tmux_log = read_to_string(&fake.tmux_log);
        assert!(
            !tmux_log.contains(":leader"),
            "{command} --external-leader must not create or attach the managed :leader window; tmux_log={tmux_log:?}"
        );
        let status = Command::new(bin())
            .args(["status", "--workspace", workspace.to_str().unwrap(), "--json"])
            .output()
            .expect("status after external leader launch");
        let status_stdout = String::from_utf8_lossy(&status.stdout);
        let status_json: Value =
            serde_json::from_slice(&status.stdout).unwrap_or_else(|err| {
                panic!("status json parse failed: {err}; stdout={status_stdout:?}")
            });
        assert_eq!(status_json["is_external_leader"], json!(true), "{command}");
        assert_eq!(status_json["leader_topology"], json!("external"), "{command}");
    }
}

#[test]
fn external_leader_after_dashdash_fails_with_visible_guidance() {
    let workspace = tmp_dir("external-after-dashdash");

    let out = Command::new(bin())
        .args(["codex", "--", "--external-leader"])
        .current_dir(&workspace)
        .output()
        .expect("run team-agent codex");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "misplaced --external-leader must fail before provider exec; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "human error path should not write stdout; stdout={stdout:?}"
    );
    assert!(
        stderr.contains("error: Team Agent launcher flag --external-leader must appear before --"),
        "stderr must name the misplaced flag; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("action: move the Team Agent launcher flag before `--`"),
        "stderr must tell the user how to fix the flag boundary; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("log: "),
        "stderr must include the cli-error log path; stderr={stderr:?}"
    );
}

#[test]
#[serial(env)]
fn attach_leader_uses_state_recorded_tmux_socket_endpoint() {
    let workspace = tmp_dir("attach-state-socket");
    let socket_path = workspace.join("state-socket");
    save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "workspace": workspace.to_string_lossy().to_string(),
            "tmux_socket": socket_path.to_string_lossy().to_string(),
            "agents": {}
        }),
    )
    .expect("seed state with recorded socket endpoint");
    let fake = FakeLauncherTools::new(&workspace);
    fake.write_tmux_script_for_attach_probe(&socket_path, "%77");
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let attach = Command::new(bin())
        .args([
            "attach-leader",
            "--workspace",
            workspace.to_str().unwrap(),
            "--pane",
            "%77",
            "--provider",
            "claude",
            "--json",
        ])
        .output()
        .expect("attach leader using state socket");
    let stdout = String::from_utf8_lossy(&attach.stdout);
    let stderr = String::from_utf8_lossy(&attach.stderr);
    let tmux_log = read_to_string(&fake.tmux_log);
    assert!(
        attach.status.success(),
        "attach-leader must resolve pane through state tmux_socket using -S; stdout={stdout:?} stderr={stderr:?} tmux_log={tmux_log:?}"
    );
    let value: Value = serde_json::from_slice(&attach.stdout).expect("attach json");
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["leader_receiver"]["pane_id"], json!("%77"));
    assert!(
        tmux_log
            .lines()
            .any(|line| line.contains("-S") && line.contains(socket_path.to_str().unwrap())),
        "attach-leader must probe the recorded socket endpoint with -S; tmux_log={tmux_log:?}"
    );
}

#[test]
#[serial(env)]
fn attach_leader_uses_team_scoped_state_socket_endpoint() {
    let workspace = tmp_dir("attach-team-socket");
    let top_socket = workspace.join("top-socket");
    let child_socket = workspace.join("child-socket");
    save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "workspace": workspace.to_string_lossy().to_string(),
            "tmux_socket": top_socket.to_string_lossy().to_string(),
            "teams": {
                "current": {
                    "session_name": "team-current",
                    "workspace": workspace.to_string_lossy().to_string(),
                    "tmux_socket": top_socket.to_string_lossy().to_string(),
                    "agents": {}
                },
                "child": {
                    "session_name": "team-child",
                    "workspace": workspace.to_string_lossy().to_string(),
                    "tmux_socket": child_socket.to_string_lossy().to_string(),
                    "agents": {}
                }
            },
            "agents": {}
        }),
    )
    .expect("seed state with team-scoped socket endpoint");
    let fake = FakeLauncherTools::new(&workspace);
    fake.write_tmux_script_for_attach_probe(&child_socket, "%88");
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let attach = Command::new(bin())
        .args([
            "attach-leader",
            "--workspace",
            workspace.to_str().unwrap(),
            "--team",
            "child",
            "--pane",
            "%88",
            "--provider",
            "claude",
            "--json",
        ])
        .output()
        .expect("attach leader using child team socket");
    let stdout = String::from_utf8_lossy(&attach.stdout);
    let stderr = String::from_utf8_lossy(&attach.stderr);
    let tmux_log = read_to_string(&fake.tmux_log);
    assert!(
        attach.status.success(),
        "attach-leader --team child must resolve pane through child state's tmux_socket; stdout={stdout:?} stderr={stderr:?} tmux_log={tmux_log:?}"
    );
    let value: Value = serde_json::from_slice(&attach.stdout).expect("attach json");
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["team"], json!("child"));
    assert_eq!(value["leader_receiver"]["pane_id"], json!("%88"));
    assert_eq!(
        value["leader_receiver"]["tmux_socket"],
        json!(child_socket.to_string_lossy().to_string())
    );
    let state = load_runtime_state(&workspace).expect("load state after attach");
    assert_eq!(
        state["teams"]["child"]["leader_receiver"]["tmux_socket"],
        json!(child_socket.to_string_lossy().to_string())
    );
}

#[test]
#[serial(env)]
fn managed_leader_launcher_writes_client_diagnostics_outside_owner_gate() {
    let workspace = tmp_dir("managed-client-diagnostic");
    let fake = FakeLauncherTools::new(&workspace);
    let _env = EnvGuard::set([
        (
            "PATH",
            Some(format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            )),
        ),
        ("TMUX", None),
        ("TMUX_PANE", None),
    ]);

    let out = Command::new(bin())
        .args(["claude", "--json"])
        .current_dir(&workspace)
        .output()
        .expect("run team-agent claude");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "fake managed launcher should complete; stdout={stdout:?} stderr={stderr:?}"
    );
    let state = load_runtime_state(&workspace).expect("load runtime state");

    assert_eq!(state["is_external_leader"], json!(false));
    assert_eq!(state["leader_client"]["diagnostic_only"], json!(true));
    assert_eq!(state["leader_receiver"]["pane_id"], json!("%42"));
    assert_eq!(state["team_owner"]["pane_id"], json!("%42"));
    for path in ["/leader_receiver", "/team_owner"] {
        let value = state.pointer(path).expect("owner/receiver");
        assert!(
            value.get("diagnostic_only").is_none()
                && value.get("attach_mode").is_none()
                && value.get("tmux").is_none(),
            "client diagnostic fields must not enter owner gate at {path}: {value}"
        );
    }
}

#[test]
fn managed_topology_source_guard_keeps_external_leader_protections_path_aware() {
    let cli_source = include_str!("../src/cli/mod.rs");

    assert!(
        cli_source.contains("fn state_uses_external_leader")
            && cli_source.contains("if !state_uses_external_leader(state)")
            && cli_source.contains("managed_leader_socket_cleanup"),
        "managed topology must be an explicit path branch, not a deletion of external leader handling"
    );
    assert!(
        cli_source.contains("extend_protection_with_leader_panes")
            && cli_source.contains("LEADER_SESSION_PREFIX"),
        "external leader spare/protection guards must remain alongside managed topology"
    );
}

#[test]
fn claude_code_is_not_a_public_leader_launcher_command() {
    let workspace = tmp_dir("claude-code-command");
    let out = Command::new(bin())
        .args(["claude_code", "--json"])
        .current_dir(&workspace)
        .output()
        .expect("run team-agent claude_code");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success() && !output_claims_ok_true(&out),
        "`claude_code` is the internal provider id, not a public launcher command. \
         Only `team-agent claude` is user-facing. code={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
}

#[test]
#[ignore = "real-machine: launches a fake provider inside real workspace-socket tmux"]
#[file_serial(tmux)]
fn real_tmux_claude_launcher_creates_workspace_socket_leader_pane_attachable_by_attach_leader() {
    let workspace = tmp_dir("real-tmux");
    let backend = TmuxBackend::for_workspace(&workspace);
    backend.kill_server();
    let _cleanup = RealTmuxCleanup {
        workspace: workspace.clone(),
    };
    let fake = FakeLauncherTools::new(&workspace);
    let session = leader_session_name(Provider::ClaudeCode, &workspace);

    let out = Command::new(bin())
        .arg("claude")
        .current_dir(&workspace)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run team-agent claude real tmux launcher");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "non-tty managed launcher should detach cleanly after creating the workspace-socket leader session; \
         code={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
    wait_until(Duration::from_secs(5), || {
        backend.has_session(&session).unwrap_or(false)
    });
    assert!(
        backend.has_session(&session).unwrap_or(false),
        "leader launcher must create session {} on the workspace socket, not the default tmux server. \
         stdout={stdout:?} stderr={stderr:?}",
        session.as_str()
    );

    let panes = backend.list_targets().expect("list workspace tmux panes");
    let pane = panes
        .iter()
        .find(|pane| pane.session.as_str() == session.as_str())
        .map(|pane| &pane.pane_id)
        .unwrap_or_else(|| {
            panic!("leader pane must be visible on workspace socket; panes={panes:?}")
        });

    let attach = Command::new(bin())
        .args([
            "attach-leader",
            "--workspace",
            workspace.to_str().unwrap(),
            "--pane",
            pane.as_str(),
            "--provider",
            "claude",
            "--confirm",
            "--json",
        ])
        .output()
        .expect("attach launched leader pane");
    let attach_stdout = String::from_utf8_lossy(&attach.stdout);
    let attach_stderr = String::from_utf8_lossy(&attach.stderr);
    let attach_json = serde_json::from_slice::<Value>(&attach.stdout).unwrap_or(Value::Null);
    assert!(
        attach.status.success() && attach_json["ok"] == json!(true),
        "attach-leader must resolve the launcher-created pane through the same workspace socket; \
         pane={} attach_stdout={attach_stdout:?} attach_stderr={attach_stderr:?}",
        pane.as_str()
    );
}

struct FakeLauncherTools {
    bin: PathBuf,
    tmux_log: PathBuf,
    provider_log: PathBuf,
}

impl FakeLauncherTools {
    fn new(workspace: &Path) -> Self {
        let bin = workspace.join("fake-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tmux_log = workspace.join("tmux-argv.log");
        let provider_log = workspace.join("claude-argv.log");
        write_executable(
            &bin.join("tmux"),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi
case " $* " in
  *" has-session "*)
    exit 1
    ;;
  *" new-session "*)
    exit 0
    ;;
  *" display-message "*)
    printf '%%42\n'
    exit 0
    ;;
  *" list-panes "*)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
                tmux_log.display()
            ),
        );
        write_executable(
            &bin.join("claude"),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "--version" ]; then
  printf 'Claude Code 1.0.0\n'
  exit 0
fi
while :; do sleep 1; done
"#,
                provider_log.display()
            ),
        );
        for command in ["codex", "copilot"] {
            write_executable(
                &bin.join(command),
                &format!(
                    r#"#!/bin/sh
printf '%s %s\n' '{}' "$*" >> '{}'
if [ "$1" = "--version" ]; then
  printf '{} 1.0.0\n'
  exit 0
fi
exit 0
"#,
                    command,
                    provider_log.display(),
                    command
                ),
            );
        }
        Self {
            bin,
            tmux_log,
            provider_log,
        }
    }

    fn write_tmux_script_for_attach_probe(&self, socket_path: &Path, pane_id: &str) {
        write_executable(
            &self.bin.join("tmux"),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi
case " $* " in
  *"-S {} list-panes"*)
    printf '%s\tteam-current\t0\tleader\t0\t/dev/ttys001\tclaude\t1\t%s\t1\t0\t12345\n' '{}' '{}'
    exit 0
    ;;
  *" list-panes "*)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
                self.tmux_log.display(),
                socket_path.display(),
                pane_id,
                self.bin
                    .parent()
                    .expect("workspace fake-bin has parent")
                    .to_string_lossy()
            ),
        );
    }
}

struct RealTmuxCleanup {
    workspace: PathBuf,
}

impl Drop for RealTmuxCleanup {
    fn drop(&mut self) {
        TmuxBackend::for_workspace(&self.workspace).kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(values: [(&'static str, Option<String>); 3]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn write_executable(path: &Path, body: &str) {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    let mut perms = file.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn output_claims_ok_true(output: &Output) -> bool {
    if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return true;
        }
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| matches!(line.trim(), "ok: true" | "ok: True"))
}

fn assert_owner_binding_unchanged(before: &Value, after: &Value, pointer: &str) {
    for field in [
        "mode",
        "status",
        "provider",
        "pane_id",
        "owner_epoch",
        "claimed_via",
    ] {
        assert_eq!(
            after.pointer(&format!("{pointer}/{field}")),
            before.pointer(&format!("{pointer}/{field}")),
            "leader launcher must not implicitly claim or rewrite {pointer}.{field}; \
             use attach-leader/claim-leader for binding. before={before} after={after}"
        );
    }
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-leader-launcher-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
