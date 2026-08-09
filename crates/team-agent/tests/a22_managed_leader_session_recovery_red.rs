//! A-22 RED: managed leader startup must recover a stale registered session.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;
use serial_test::serial;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport};

use hermetic_guard::HermeticTestEnv;

#[test]
#[serial(env)]
fn a22_managed_start_recovers_stale_registered_leader_session() {
    let env = HermeticTestEnv::enter("a22-stale-leader");
    let workspace = env.workspace("workspace");
    let backend = TmuxBackend::for_workspace(&workspace);
    let endpoint = backend
        .tmux_endpoint()
        .expect("workspace backend has isolated tmux endpoint");
    let _cleanup = TmuxCleanup {
        endpoint: endpoint.clone(),
    };

    let existing = SessionName::new("team-agent-leader-claude_code-existing");
    let created = Command::new("tmux")
        .args([
            "-L",
            &endpoint,
            "new-session",
            "-d",
            "-s",
            existing.as_str(),
            "-n",
            "claude_code",
            "sh",
            "-lc",
            "sleep 30",
        ])
        .output()
        .expect("create existing leader session");
    assert!(
        created.status.success(),
        "create leader session: {created:?}"
    );

    let stale = "team-agent-leader-claude_code-stale-old";
    save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "current",
            "is_external_leader": false,
            "teams": {
                "current": {
                    "session_name": "team-current",
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "claude_code",
                        "pane_id": "%stale",
                        "session_name": stale,
                        "window_name": "claude_code",
                        "tmux_socket": endpoint,
                        "owner_epoch": 1
                    },
                    "team_owner": {"pane_id": "%stale", "owner_epoch": 1},
                    "agents": {}
                }
            },
            "agents": {}
        }),
    )
    .expect("seed stale leader registration");

    let bin = workspace.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && echo fake-claude && exit 0\nexit 0\n",
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["claude", "--json", "--", "--version"])
        .current_dir(&workspace)
        .env("HOME", env.home())
        .env("PATH", path)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run managed launcher against stale registration");
    assert!(
        !output.status.success(),
        "non-TTY attach must exercise the failure path; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostics_path = combined
        .split_whitespace()
        .find_map(|field| field.strip_prefix("launcher_diagnostics="))
        .map(|path| path.trim_matches(|ch| matches!(ch, '"' | ',' | ';')))
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            std::fs::read_dir(workspace.join(".team").join("logs"))
                .ok()?
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("cli-error-"))
                })
                .filter_map(|path| std::fs::read_to_string(path).ok())
                .flat_map(|text| {
                    text.lines()
                        .filter_map(|line| line.split_once("launcher_diagnostics="))
                        .map(|(_, path)| {
                            PathBuf::from(path.trim_matches(|ch| matches!(ch, '"' | ',' | ';')))
                        })
                        .collect::<Vec<_>>()
                })
                .find(|path| path.exists())
        })
        .expect("A-22 attach failure must name launcher diagnostics");
    let diagnostics = std::fs::read_to_string(&diagnostics_path)
        .expect("A-22 attach failure diagnostics must be persisted");
    assert!(
        diagnostics.contains("startup_stage=")
            && diagnostics.contains("child_stdout=")
            && diagnostics.contains("child_stderr="),
        "A-22 attach-or-create failure must persist both child streams and startup stage; diagnostics={diagnostics:?}"
    );

    let state = load_runtime_state(&workspace).expect("load refreshed state");
    assert_eq!(
        state["teams"]["current"]["leader_receiver"]["session_name"],
        json!(existing.as_str()),
        "A-22: stale registration must be refreshed to an existing leader session matched by prefix; state={state}"
    );
    assert!(
        backend.has_session(&existing).unwrap_or(false),
        "A-22: the matched existing leader session must survive attach failure"
    );
}

#[test]
#[serial(env)]
fn a22b_managed_attach_failure_rediscovers_after_selected_session_disappears() {
    let env = HermeticTestEnv::enter("a22b-attach-recovery");
    let workspace = env.workspace("workspace");
    let backend = TmuxBackend::for_workspace(&workspace);
    let endpoint = backend
        .tmux_endpoint()
        .expect("workspace backend has isolated tmux endpoint");
    let _cleanup = TmuxCleanup {
        endpoint: endpoint.clone(),
    };
    let real_tmux = Command::new("sh")
        .args(["-lc", "command -v tmux"])
        .output()
        .expect("locate real tmux")
        .stdout;
    let real_tmux = String::from_utf8_lossy(&real_tmux).trim().to_string();
    assert!(!real_tmux.is_empty(), "tmux must be installed for A-22b");

    let existing = SessionName::new("team-agent-leader-claude_code-selected");
    let created = Command::new(&real_tmux)
        .args([
            "-L",
            &endpoint,
            "new-session",
            "-d",
            "-s",
            existing.as_str(),
            "-n",
            "claude_code",
            "sh",
            "-lc",
            "sleep 30",
        ])
        .output()
        .expect("create selected leader session");
    assert!(
        created.status.success(),
        "create selected leader session: {created:?}"
    );

    save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "current",
            "is_external_leader": false,
            "teams": {
                "current": {
                    "session_name": "team-current",
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "claude_code",
                        "pane_id": "%stale",
                        "session_name": "team-agent-leader-claude_code-stale-registration",
                        "window_name": "claude_code",
                        "tmux_socket": endpoint,
                        "owner_epoch": 1
                    },
                    "team_owner": {"pane_id": "%stale", "owner_epoch": 1},
                    "agents": {}
                }
            },
            "agents": {}
        }),
    )
    .expect("seed A-22b state");

    let bin = workspace.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_executable(&bin.join("claude"), "#!/bin/sh\necho fake-claude\nexit 0\n");
    let shim_bin = workspace.join("tmux-shim");
    std::fs::create_dir_all(&shim_bin).unwrap();
    let attach_log = workspace.join("attach-targets.log");
    let attach_count = workspace.join("attach-count");
    write_executable(
        &shim_bin.join("tmux"),
        "#!/bin/sh
real=${TA_A22_REAL_TMUX:?}
log=${TA_A22_ATTACH_LOG:?}
count_file=${TA_A22_ATTACH_COUNT:?}
socket=
if [ \"${1:-}\" = \"-L\" ]; then
  socket=$2
  shift 2
fi
if [ \"${1:-}\" = \"attach-session\" ]; then
  target=
  previous=
  for arg in \"$@\"; do
    if [ \"$previous\" = \"-t\" ]; then target=$arg; fi
    previous=$arg
  done
  printf 'target=%s\\n' \"$target\" >> \"$log\"
  count=0
  if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi
  if [ \"$count\" -eq 0 ]; then
    session=${target%%:*}
    \"$real\" -L \"$socket\" kill-session -t \"$session\" >/dev/null 2>&1 || true
  fi
  printf '%s\\n' $((count + 1)) > \"$count_file\"
  printf '%s\\n' 'open terminal failed: injected A-22b attach failure' >&2
  exit 1
fi
exec \"$real\" -L \"$socket\" \"$@\"
",
    );
    let path = format!(
        "{}:{}:{}",
        shim_bin.display(),
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = env.with_env("PATH", &path);
    let _real_tmux = env.with_env("TA_A22_REAL_TMUX", &real_tmux);
    let _attach_log = env.with_env("TA_A22_ATTACH_LOG", &attach_log.to_string_lossy());
    let _attach_count = env.with_env("TA_A22_ATTACH_COUNT", &attach_count.to_string_lossy());

    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["claude", "--json", "--", "--version"])
        .current_dir(&workspace)
        .env("HOME", env.home())
        .env("PATH", &path)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run A-22b managed launcher");
    assert!(
        !output.status.success(),
        "both injected attach attempts must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let targets = std::fs::read_to_string(&attach_log)
        .expect("attach shim must record both targets")
        .lines()
        .filter_map(|line| line.strip_prefix("target="))
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        targets.len(),
        2,
        "A-22b must retry exactly once after the selected session disappears; targets={targets:?}"
    );
    assert_eq!(
        targets[0],
        format!("{}:claude_code", existing.as_str()),
        "first attach must use the selected existing candidate"
    );
    let retry_session = targets[1]
        .strip_suffix(":claude_code")
        .expect("retry target must contain provider window");
    assert_ne!(
        retry_session,
        existing.as_str(),
        "retry must not reuse the selected session after it disappears"
    );
    assert!(
        retry_session.starts_with("team-agent-leader-claude_code-"),
        "retry must use a discovered leader candidate or nonce: {retry_session}"
    );

    let state = load_runtime_state(&workspace).expect("load refreshed A-22b state");
    assert_eq!(
        state["teams"]["current"]["leader_receiver"]["session_name"],
        json!(retry_session),
        "A-22b retry must refresh the binding before the second attach; state={state}"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("leader launcher exited with status"),
        "second failure must retain the structured launcher error: {combined}"
    );
    assert!(
        !combined.contains("transport error"),
        "A-22b must not expose a bare transport error: {combined}"
    );
}

struct TmuxCleanup {
    endpoint: String,
}

impl Drop for TmuxCleanup {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.endpoint, "kill-server"])
            .output();
    }
}

fn write_executable(path: &Path, body: &str) {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    let mut permissions = file.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}
