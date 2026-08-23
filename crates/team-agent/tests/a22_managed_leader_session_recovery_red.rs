//! A-22: managed leader startup — attach-failure diagnostics and existing-session safety.
//!
//! 🔴 2026-08-23: this file used to be titled "must recover a stale registered
//! session". That title stated a SELF-HEAL requirement the product does not have
//! (`.team/artifacts/test-asset-liabilities.md:103-104`); the assertion carrying it
//! was deleted from the first test. See that test's inline RETIRED note.
//! ⚠️ 作用域：本次处置只碰第一个测试。`a22b_...rediscovers_after_selected_session_disappears`
//! 未在派单范围内 ⇒ ⛔ 未动，其"重新发现"语义是否同源另议。

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
fn a22_managed_start_attach_failure_persists_diagnostics_and_preserves_existing_session() {
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

    // 🔴 2026-08-23 RETIRED:
    //   assert_eq!(state[..]["leader_receiver"]["session_name"], json!(existing.as_str()),
    //       "A-22: stale registration must be refreshed to an existing leader session
    //        matched by prefix")
    // It assumed a SELF-HEAL mechanism: 发现注册陈旧 ⇒ 探测哪个 session 还活着 ⇒ 覆盖注册.
    // 落盘需求 (`.team/artifacts/test-asset-liabilities.md:103-104`, 用户原话):
    // **从来没有设计过自愈，claim 就够了** ——「判活 → 决定覆盖」产品里不存在.
    // 架构页同向 (`wiki/C3/所有权与租约.md:9/:17/:21`): 只有用户明确触发的绑定命令能改
    // leader receiver；即使发现旧 pane 已死也无权进入写路径；输出要么完整绑定、要么明确
    // 拒绝，**没有半成功**. 而本用例上面刚断言 attach 必须失败 ⇒ 它要的正是那个"半成功".
    // ⇒ deleted, not weakened.
    // ⚠️ ⛔ 未替换成反向断言（「失败后注册必须保持原样」/「必须清空」）——两者哪个正确
    // **没有落盘**，判为「待向人确认」，⛔ 不由本格代裁。
    assert!(
        backend.has_session(&existing).unwrap_or(false),
        "A-22: the pre-existing leader session must survive attach failure"
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

    // The window nonce is `<pid_hex>-<epoch_nanos_hex>` minted inside this very
    // launcher process (leader/start.rs:961-966), so capture the child pid and
    // the wall-clock window around the launch: both are asserted below.
    let launch_started_nanos = epoch_nanos_now();
    let child = Command::new(env!("CARGO_BIN_EXE_team-agent"))
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
        .spawn()
        .expect("spawn A-22b managed launcher");
    let launcher_pid = child.id();
    let output = child
        .wait_with_output()
        .expect("run A-22b managed launcher");
    let launch_finished_nanos = epoch_nanos_now();
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
    // 0.5.x multi-leader window contract (leader/start.rs:3-7 and :916-918):
    // the first managed leader keeps the bare `claude_code` window, a later
    // leader joining a session where that name is already taken MUST get a
    // unique `claude_code-<nonce>` window, otherwise tmux answers
    // `attach-session -t SESSION:claude_code` with `can't find window`.
    // This test creates the selected session with `-n claude_code` above, so
    // the base name is occupied and the suffix is REQUIRED, not optional.
    // The intent of this assertion is unchanged: the first attach must go to
    // the selected existing candidate. Only the window literal — which pinned
    // an implementation shape that the contract above replaced — is relaxed,
    // and it is replaced by stricter checks on the nonce itself.
    let (first_session, first_window) = split_attach_target(&targets[0]);
    assert_eq!(
        first_session,
        existing.as_str(),
        "first attach must use the selected existing candidate; target={}",
        targets[0]
    );
    let (first_pid_hex, first_nanos_hex) = parse_provider_window(&first_window, "first")
        .unwrap_or_else(|| {
            panic!(
                "the selected session already owns a `claude_code` window, so the first attach \
                 must target a nonce-suffixed window (leader/start.rs:919-940); window={first_window}"
            )
        });
    // `nonce` = `<pid_hex>-<epoch_nanos_hex>` (leader/start.rs:857-859, minted
    // for the window at :961-966). Pin it to THIS launch so a constant or
    // hardcoded nonce cannot satisfy the shape check above.
    assert_eq!(
        u32::from_str_radix(&first_pid_hex, 16).expect("nonce pid segment must parse as hex"),
        launcher_pid,
        "window nonce pid segment must be this launcher's pid; nonce_pid_hex={first_pid_hex} \
         launcher_pid={launcher_pid} window={first_window}"
    );
    let first_nonce_nanos =
        u128::from_str_radix(&first_nanos_hex, 16).expect("nonce epoch segment must parse as hex");
    assert!(
        (launch_started_nanos..=launch_finished_nanos).contains(&first_nonce_nanos),
        "window nonce epoch segment must be minted during this launch; \
         nonce_nanos={first_nonce_nanos} window=[{launch_started_nanos},{launch_finished_nanos}]"
    );

    let (retry_session, retry_window) = split_attach_target(&targets[1]);
    // The retry may land in a session where `claude_code` is free, so the bare
    // base name is legal here; what is NOT legal is a malformed window.
    let _ = parse_provider_window(&retry_window, "retry");
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

fn epoch_nanos_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_nanos()
}

/// Split a tmux attach target `SESSION:WINDOW` recorded by the shim.
fn split_attach_target(target: &str) -> (String, String) {
    let (session, window) = target
        .split_once(':')
        .unwrap_or_else(|| panic!("attach target must be SESSION:WINDOW; target={target}"));
    (session.to_string(), window.to_string())
}

/// Validate a managed-leader provider window and return its nonce segments.
///
/// Contract (leader/start.rs:3-7, :916-918, :919-940): the window is either the
/// bare provider wire name when it is free, or `claude_code-<pid_hex>-<epoch_nanos_hex>`
/// when that name is already taken (nonce format documented at :857-859, minted
/// for the window at :961-966). Anything else is a contract violation.
fn parse_provider_window(window: &str, whose: &str) -> Option<(String, String)> {
    if window == "claude_code" {
        return None;
    }
    let nonce = window.strip_prefix("claude_code-").unwrap_or_else(|| {
        panic!(
            "{whose} attach window must be `claude_code` or `claude_code-<nonce>`; window={window}"
        )
    });
    let (pid_hex, nanos_hex) = nonce.split_once('-').unwrap_or_else(|| {
        panic!("{whose} window nonce must be `<pid_hex>-<epoch_nanos_hex>`; nonce={nonce}")
    });
    assert!(
        !pid_hex.is_empty() && pid_hex.chars().all(|ch| ch.is_ascii_hexdigit()),
        "{whose} window nonce pid segment must be non-empty hex; nonce={nonce}"
    );
    assert!(
        !nanos_hex.is_empty() && nanos_hex.chars().all(|ch| ch.is_ascii_hexdigit()),
        "{whose} window nonce epoch segment must be non-empty hex; nonce={nonce}"
    );
    Some((pid_hex.to_string(), nanos_hex.to_string()))
}

fn write_executable(path: &Path, body: &str) {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    let mut permissions = file.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}
