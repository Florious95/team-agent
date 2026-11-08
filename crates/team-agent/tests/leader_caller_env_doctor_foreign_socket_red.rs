//! ---
//! purpose: RT-9 oracle addendum for BUG-ANALYSIS-ROLE-CREATION-FAILURE (视频制作)
//! contract:
//!   RT-9 doctor must judge the caller tuple exactly like `start`: a VALID worker
//!        tuple (pane + endpoint = the caller's own live pane) is accepted even
//!        when the inspected team lives on a different tmux server. Regression
//!        guard: GREEN at baseline 36c890b4, RED at candidate 6262c310.
//! boundary (real-machine isolation, equivalent to HermeticTestEnv):
//!   - real `team-agent` binary, two test-owned tmux servers, non-ASCII workspace
//!   - every tmux server and CLI child runs with `env_clear()` plus an explicit
//!     per-test HOME; TMUX/TMUX_PANE point only at test-owned panes; no
//!     TEAM_AGENT_WORKSPACE or other TEAM_AGENT_* is ever inherited from the host
//! transcripts: $TA_RED_TRANSCRIPT_DIR/RT-9.log (default: temp_dir/ta-red-transcripts)
//! ---
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_team-agent");

fn transcript(id: &str, body: &str) {
    let dir = std::env::var_os("TA_RED_TRANSCRIPT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ta-red-transcripts"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{id}.log")), body);
    eprintln!("===== {id} transcript =====\n{body}\n===== end {id} =====");
}

struct Owned {
    root: PathBuf,
    sockets: Vec<String>,
}

impl Drop for Owned {
    fn drop(&mut self) {
        for name in &self.sockets {
            let path = socket_path(name);
            let _ = Command::new("tmux").args(["-L", name, "kill-server"]).env_remove("TMUX").output();
            if !path.is_empty() {
                let _ = std::fs::remove_file(path);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn socket_path(name: &str) -> String {
    let out = Command::new("tmux").args(["-L", name, "display-message", "-p", "#{socket_path}"])
        .env_remove("TMUX").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Start a test-owned server `name` whose single pane runs `program` in `cwd`.
fn serve(name: &str, cwd: &Path, home: &Path, program: &Path) -> String {
    let started = Command::new("tmux")
        .args(["-u", "-L", name, "-f", "/dev/null", "new-session", "-d", "-s", "fixture", "-c"])
        .arg(cwd)
        .arg(format!("exec '{}'", program.display()))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("LANG", "en_US.UTF-8")
        .output()
        .unwrap();
    assert!(started.status.success(), "RT-9 PRECONDITION: tmux {name} failed: {started:?}");
    socket_path(name)
}

fn cli(cwd: &Path, env: &[(String, String)], args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .unwrap()
}

fn render(label: &str, args: &[&str], env: &[(String, String)], out: &Output) -> String {
    format!(
        "$ {label}: team-agent {}\n  env: {env:?}\n  exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}\n",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn rt9_doctor_accepts_valid_worker_tuple_from_foreign_socket() {
    let base = if cfg!(target_os = "macos") { "/private/tmp" } else { "/tmp" };
    let root = PathBuf::from(base).join(format!("ta-rce-rt9-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let workspace = root.join("视频制作");
    let worker_cwd = root.join("worker-home-team");
    let home = root.join("home");
    for dir in [&workspace, &worker_cwd, &home.join(".team-agent/leaders")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let leader_socket = format!("ta-rce-rt9l-{}", std::process::id());
    let worker_socket = format!("ta-rce-rt9w-{}", std::process::id());
    let _owned = Owned { root: root.clone(), sockets: vec![leader_socket.clone(), worker_socket.clone()] };

    // Native `pi`-named stub so tmux reports pane_current_command=pi for the Leader.
    let pi = root.join("bin/pi");
    let mut cc = Command::new("cc").args(["-x", "c", "-", "-o"]).arg(&pi)
        .stdin(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .expect("RT-9 PRECONDITION: a C compiler (cc) is required for the native pi stub");
    cc.stdin.take().unwrap().write_all(b"#include <unistd.h>\nint main(void){for(;;)pause();}\n").unwrap();
    assert!(cc.wait_with_output().unwrap().status.success(), "RT-9 PRECONDITION: cc failed");

    // Socket B: the inspected team's Leader. Socket A: an unrelated live worker pane.
    let leader_path = serve(&leader_socket, &workspace, &home, &pi);
    let worker_path = serve(&worker_socket, &worker_cwd, &home, &pi);
    let base_env = |tmux: &str| {
        vec![
            ("PATH".to_string(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ("TMUX".to_string(), format!("{tmux},0,0")),
            ("TMUX_PANE".to_string(), "%0".to_string()),
        ]
    };
    let ws = workspace.to_string_lossy().into_owned();
    let mut log = format!(
        "RT-9 doctor vs valid foreign-socket worker tuple\nworkspace={ws}\nleader=%0@{leader_path}\nworker=%0@{worker_path}\n\n"
    );

    let mut leader_env = base_env(&leader_path);
    leader_env.push(("TEAM_AGENT_LEADER_PROVIDER".into(), "pi".into()));
    let claim_args = ["claim-leader", "--workspace", &ws, "--team", "current", "--json"];
    let claim = cli(&workspace, &leader_env, &claim_args);
    log.push_str(&render("claim (leader pane, clean)", &claim_args, &leader_env, &claim));

    // A framework-shaped worker caller: its tuple names its OWN live pane and endpoint.
    let mut worker_env = base_env(&worker_path);
    worker_env.extend([
        ("TEAM_AGENT_CALLER_PROVIDER".to_string(), "claude".to_string()),
        ("TEAM_AGENT_CALLER_PANE_ID".to_string(), "%0".to_string()),
        ("TEAM_AGENT_CALLER_TMUX_ENDPOINT".to_string(), worker_path.clone()),
        ("TEAM_AGENT_ID".to_string(), "inspector".to_string()),
        ("TEAM_AGENT_AGENT_ID".to_string(), "inspector".to_string()),
        ("TEAM_AGENT_OWNER_TEAM_ID".to_string(), "other-team".to_string()),
    ]);
    let doctor_args = ["doctor", "--workspace", &ws, "--team", "current", "--json"];
    let doctor = cli(&worker_cwd, &worker_env, &doctor_args);
    log.push_str(&render("doctor (valid worker tuple on foreign socket)", &doctor_args, &worker_env, &doctor));

    let text = String::from_utf8_lossy(&doctor.stdout).into_owned();
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let (l6_status, l6_message) = json["links"].as_array()
        .and_then(|links| links.iter().find(|l| l["id"] == "L6"))
        .map(|l| (l["status"].as_str().unwrap_or("").to_string(), l["message"].as_str().unwrap_or("").to_string()))
        .unwrap_or_default();
    let mut failures = Vec::new();
    if text.contains("invalid_caller_tuple") {
        failures.push("expected a valid worker tuple to be accepted; actual: doctor reports invalid_caller_tuple".to_string());
    }
    if l6_status != "PASS" {
        failures.push(format!("expected L6 PASS (leader reachable, caller differs); actual {l6_status} \"{l6_message}\""));
    }
    log.push_str(&format!(
        "\nexpected: L6 PASS, no invalid_caller_tuple\nactual: L6={l6_status} \"{l6_message}\"\nRESULT failures:\n{}\n",
        failures.join("\n")
    ));
    transcript("RT-9", &log);

    assert!(claim.status.success(), "RT-9 PRECONDITION: claim-leader failed; see transcript");
    assert!(json.is_object(), "RT-9 PRECONDITION: doctor must emit JSON; see transcript");
    assert!(failures.is_empty(), "RT-9 FAILED:\n{}", failures.join("\n"));
}
