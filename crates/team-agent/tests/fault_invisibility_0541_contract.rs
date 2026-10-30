//! 0.5.41 RED contracts: stale runtime truth must be visible, not hidden behind
//! cached state / agent_health rows.
//!
//! Reference: `.team/artifacts/fault-invisibility-locate.md` §9.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/brief_probe.rs"]
mod brief_probe;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use serial_test::serial;

use team_agent::coordinator::{
    coordinator_meta_path, coordinator_pid_path, MetadataSource, Pid, WorkspacePath,
    PROTOCOL_VERSION,
};
use team_agent::model::paths::runtime_dir;
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

const TEAM: &str = "current";
const WORKER: &str = "helper";
const TEAM_SESSION: &str = "team-current";
const WORKER_PANE: &str = "%541";
const LEADER_PANE: &str = "%0";
const TMUX_ENDPOINT: &str = "/Volumes/nvme/tmp/ta-0541-fault-invisibility.sock";

fn brief_node<'a>(status: &'a Value, name: &str) -> Option<&'a Value> {
    status
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.iter().find(|node| {
            node.get("name").and_then(Value::as_str) == Some(name)
        }))
}

#[test]
#[serial(env)]
fn cross_boot_stale_bindings_are_visible_in_status_and_diagnose_without_mutation() {
    let case = CliCase::new("red1-boot-stale");
    case.write_fake_tmux("codex", "");
    case.seed_active_state(stale_busy_worker());
    case.write_coordinator_metadata(
        std::process::id(),
        Some(cli_binary_path()),
        Some(current_version()),
        PROTOCOL_VERSION,
    );
    case.write_coordinator_tick("old-boot");
    let before = case.snapshot_runtime_files();

    let status = case.status_json_with_host_boot("new-boot");
    let diagnose = case.diagnose_json_with_host_boot("new-boot");
    let worker = brief_node(&status, WORKER)
        .unwrap_or_else(|| panic!("RED1 setup: brief status missing helper; status={status}"));
    assert_eq!(
        worker.get("runtime_status").and_then(Value::as_str),
        Some("unknown"),
        "RED1: cross-boot stale worker without an accepted probe must expose unknown runtime; worker={worker}"
    );
    assert_eq!(
        worker.get("activity").and_then(Value::as_str),
        Some("unknown"),
        "RED1: without a valid probe, stale activity must remain unknown; worker={worker}"
    );
    assert_eq!(
        worker.get("health").and_then(Value::as_str),
        Some("unknown"),
        "RED1: without a valid probe, stale health must remain unknown; worker={worker}"
    );
    let human = case.run_ta(
        &["status", "--workspace", case.workspace_str(), "--team", TEAM],
        &[],
    );
    let human_text = String::from_utf8_lossy(&human.stdout);
    assert!(
        human_text.contains("name: helper") && human_text.contains("runtime_status: unknown"),
        "RED1: human status must use the same unknown brief projection; output={human_text}"
    );
    assert!(
        value_contains(&diagnose, "runtime_bindings_stale_after_boot")
            && value_contains(&diagnose, "team-agent restart"),
        "RED1: diagnose --json must include runtime_bindings_stale_after_boot and a restart hint; diagnose={diagnose}"
    );
    case.assert_runtime_files_unchanged(before, "RED1 status/diagnose read-only");
}

#[test]
fn real_machine_fault_invisibility_gate_is_declared() {
    let candidates = candidate_gate_files();
    let required = [
        "FAULT_INVISIBILITY_0541_REAL_MACHINE",
        "TEAM_AGENT_TEST_HOST_BOOT_ID",
        "runtime_bindings_stale_after_boot",
        "worker_provider_exit_marker",
        "team-agent restart",
        "status",
        "doctor",
    ];
    let matches = candidates
        .iter()
        .filter(|(_, text)| required.iter().all(|needle| text.contains(needle)))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();

    assert!(
        !matches.is_empty(),
        "RED5: a real-machine gate declaration must cover host-boot stale bindings plus wrapper provider-exit visibility. Missing marker FAULT_INVISIBILITY_0541_REAL_MACHINE; scanned {} files.",
        candidates.len()
    );
}

struct CliCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl CliCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        env.assert_no_real_tmux();
        let workspace = env.workspace(tag);
        let fake_bin = env.root().join(format!("fake-bin-{tag}"));
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        fs::create_dir_all(runtime_dir(&workspace)).expect("create runtime dir");
        // M4: no SQLite message store fixture (MessageStore::open is retired).
        Self {
            env,
            workspace,
            fake_bin,
        }
    }

    fn seed_active_state(&self, worker: Value) {
        save_runtime_state(&self.workspace, &base_state(&self.workspace, worker))
            .expect("save active state");
    }


    fn write_fake_tmux(&self, worker_command: &str, capture_text: &str) {
        write_fake_tmux(
            &self.fake_bin,
            &self.workspace,
            worker_command,
            capture_text,
        );
    }

    fn write_coordinator_metadata(
        &self,
        pid: u32,
        binary_path: Option<String>,
        binary_version: Option<String>,
        protocol_version: u32,
    ) {
        write_raw_coordinator_metadata(
            &WorkspacePath::new(self.workspace.clone()),
            Pid::new(pid),
            binary_path,
            binary_version,
            protocol_version,
        );
    }

    fn write_coordinator_tick(&self, host_boot_id: &str) {
        fs::write(
            runtime_dir(&self.workspace).join("coordinator_tick.json"),
            serde_json::to_string_pretty(&json!({
                "coordinator_tick_iteration_count": 7,
                "pid": std::process::id(),
                "boot_id": "daemon-old",
                "host_boot_id": host_boot_id,
                "last_phase": "tick_finished",
                "last_tick_status": "ok",
                "updated_at": "2026-07-14T00:00:00Z"
            }))
            .expect("serialize coordinator tick"),
        )
        .expect("write coordinator tick");
    }

    fn status_json_with_host_boot(&self, boot_id: &str) -> Value {
        self.status_json_with_extra_env(&[("TEAM_AGENT_TEST_HOST_BOOT_ID", boot_id)])
    }

    fn status_json_with_extra_env(&self, extra: &[(&str, &str)]) -> Value {
        parse_json_output(
            &self.run_ta(
                &[
                    "status",
                    "--workspace",
                    self.workspace_str(),
                    "--team",
                    TEAM,
                    "--json",
                    "--detail",
                ],
                extra,
            ),
            "status --json --detail",
        )
    }

    fn diagnose_json_with_host_boot(&self, boot_id: &str) -> Value {
        self.diagnose_json_with_extra_env(&[("TEAM_AGENT_TEST_HOST_BOOT_ID", boot_id)])
    }

    fn diagnose_json_with_extra_env(&self, extra: &[(&str, &str)]) -> Value {
        parse_json_output(
            &self.run_ta(
                &["doctor", "--workspace", self.workspace_str(), "--json"],
                extra,
            ),
            "doctor --json",
        )
    }

    fn run_ta(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{TMUX_ENDPOINT},12345,0"))
            .env("TMUX_PANE", LEADER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0541-red");
        for (key, value) in extra {
            command.env(key, value);
        }
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn snapshot_runtime_files(&self) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        [
            runtime_state_path(&self.workspace),
            runtime_dir(&self.workspace).join("team.db"),
            runtime_dir(&self.workspace).join("coordinator.pid"),
            runtime_dir(&self.workspace).join("coordinator.json"),
            runtime_dir(&self.workspace).join("coordinator_tick.json"),
            self.workspace.join(".team/logs/events.jsonl"),
        ]
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
    }

    fn assert_runtime_files_unchanged(&self, before: Vec<(PathBuf, Option<Vec<u8>>)>, label: &str) {
        for (path, expected) in before {
            let actual = fs::read(&path).ok();
            assert_eq!(
                actual,
                expected,
                "{label}: status/diagnose must be read-only; changed {}",
                path.display()
            );
        }
    }
}

fn base_state(workspace: &Path, worker: Value) -> Value {
    json!({
        "active_team_key": TEAM,
        "team_key": TEAM,
        "session_name": TEAM_SESSION,
        "team_dir": workspace.to_string_lossy(),
        "workspace": workspace.to_string_lossy(),
        "tmux_endpoint": TMUX_ENDPOINT,
        "tmux_socket": TMUX_ENDPOINT,
        "agents": {
            WORKER: worker.clone()
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": LEADER_PANE,
            "session_name": "leader-0541",
            "window_name": "leader",
            "owner_epoch": 1
        },
        "teams": {
            TEAM: {
                "active_team_key": TEAM,
                "team_key": TEAM,
                "session_name": TEAM_SESSION,
                "team_dir": workspace.to_string_lossy(),
                "workspace": workspace.to_string_lossy(),
                "tmux_endpoint": TMUX_ENDPOINT,
                "tmux_socket": TMUX_ENDPOINT,
                "agents": {
                    WORKER: worker
                }
            }
        }
    })
}

fn stale_busy_worker() -> Value {
    let mut worker = running_worker();
    worker["worker_state"] = json!("BUSY");
    worker["activity"] = json!({
        "status": "working",
        "rationale": "adversarial stale cache"
    });
    worker
}

fn running_worker() -> Value {
    json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "spawn_epoch": 0,
        "spawned_at": "2026-07-14T00:00:00+00:00",
        "spawn_cwd": "/tmp",
        "owner_team_id": TEAM
    })
}


fn write_raw_coordinator_metadata(
    workspace: &WorkspacePath,
    pid: Pid,
    binary_path: Option<String>,
    binary_version: Option<String>,
    protocol_version: u32,
) {
    fs::write(coordinator_pid_path(workspace), pid.to_string()).expect("write pid");
    fs::write(
        coordinator_meta_path(workspace),
        serde_json::to_string_pretty(&json!({
            "pid": pid.get(),
            "protocol_version": protocol_version,
            "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
            "binary_path": binary_path,
            "binary_version": binary_version,
            "source": MetadataSource::Boot,
            "updated_at": "2026-07-14T00:00:00Z"
        }))
        .expect("serialize metadata"),
    )
    .expect("write metadata");
}

fn write_fake_tmux(fake_bin: &Path, workspace: &Path, worker_command: &str, capture_text: &str) {
    let tmux = fake_bin.join("tmux");
    let worker_line = pane_line(
        WORKER_PANE,
        TEAM_SESSION,
        WORKER,
        worker_command,
        workspace,
        std::process::id(),
    );
    let leader_line = pane_line(
        LEADER_PANE,
        "leader-0541",
        "leader",
        "codex",
        workspace,
        std::process::id(),
    );
    let script = format!(
        r#"#!/bin/sh
target=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then
    target="$arg"
  fi
  previous="$arg"
done
case " $* " in
  *" has-session "*) exit 0 ;;
  *" list-windows "*) printf '%s\n' '0: {worker}'; exit 0 ;;
  *" list-sessions "*) printf '%s\n' '{team_session}: 1 windows'; exit 0 ;;
  *" list-panes "*) printf '%s' '{leader_line}'; printf '%s' '{worker_line}'; exit 0 ;;
  *" capture-pane "*) printf '%s\n' '{capture_text}'; exit 0 ;;
  *" display-message "*)
    case "$target" in
      %*) printf '%s\n' "$target"; exit 0 ;;
      *"{worker}"*) printf '%s\n' '{worker_pane}'; exit 0 ;;
      *) printf '%s\n' '{leader_pane}'; exit 0 ;;
    esac
    ;;
  *) exit 0 ;;
esac
"#,
        worker = WORKER,
        team_session = TEAM_SESSION,
        worker_line = shell_single_quoted_payload(&worker_line),
        leader_line = shell_single_quoted_payload(&leader_line),
        capture_text = shell_single_quoted_payload(capture_text),
        worker_pane = WORKER_PANE,
        leader_pane = LEADER_PANE,
    );
    fs::write(&tmux, script).expect("write fake tmux");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod fake tmux");
    }
}

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0541\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn parse_json_output(output: &Output, label: &str) -> Value {
    assert!(
        !output.stdout.is_empty(),
        "{label}: expected JSON stdout; status={} stderr={}",
        output.status,
        text(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label}: parse JSON failed: {error}; stdout={} stderr={}",
            text(&output.stdout),
            text(&output.stderr)
        )
    })
}

fn value_contains(value: &Value, needle: &str) -> bool {
    value.to_string().contains(needle)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}

fn cli_binary_path() -> String {
    fs::canonicalize(env!("CARGO_BIN_EXE_team-agent"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_team-agent")))
        .to_string_lossy()
        .to_string()
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn candidate_gate_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for relative_dir in [
        "crates/team-agent/tests",
        "crates/team-agent/tests/e2e/cases",
        "tools",
        ".team/artifacts/gate-harness",
    ] {
        let dir = root.join(relative_dir);
        if dir.exists() {
            collect_text_files(&root, &dir, &mut files);
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.ends_with("fault_invisibility_0541_contract.rs"))
        .collect()
}

fn collect_text_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("read candidate dir") {
        let path = entry.expect("read candidate entry").path();
        if path.is_dir() {
            collect_text_files(root, &path, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(ext, "rs" | "sh" | "md" | "json" | "toml" | "yaml" | "yml") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            out.push((relative_path(root, &path), text));
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
