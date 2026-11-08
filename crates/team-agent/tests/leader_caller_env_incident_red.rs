//! ---
//! purpose: black-box red oracle for BUG-ANALYSIS-ROLE-CREATION-FAILURE (视频制作)
//! contract:
//!   RT-6 doctor must evaluate the same caller tuple that `start` gates on: with
//!        a stale TEAM_AGENT_CALLER_* tuple, L6 may not report `PASS … matches`
//!   RT-7 the start refusal must surface the blocker code and field-level
//!        remediation, and must not claim "unknown effects" for a pre-spawn refusal
//! boundary (real-machine isolation, equivalent to HermeticTestEnv):
//!   - real `team-agent` binary, real tmux, non-ASCII workspaces
//!   - every tmux server and CLI child runs with `env_clear()` plus an explicit
//!     per-test HOME; TMUX/TMUX_PANE point only at the test-owned pane; no
//!     TEAM_AGENT_WORKSPACE or other TEAM_AGENT_* is ever inherited from the host
//!   - the Leader pane is a native `pi`-named stub (no provider, no model)
//!   - every tmux server/root is test-owned and removed on Drop
//! transcripts: $TA_RED_TRANSCRIPT_DIR/<RT-id>.log (default: temp_dir/ta-red-transcripts)
//! ---
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_team-agent");
const NON_ASCII: &str = "视频制作";
/// Exact values observed in the incident launcher env (pid 57477).
const STALE: [(&str, &str); 6] = [
    ("TEAM_AGENT_CALLER_PROVIDER", "claude"),
    ("TEAM_AGENT_CALLER_PANE_ID", "%46"),
    ("TEAM_AGENT_CALLER_TMUX_ENDPOINT", "/private/tmp/tmux-501/ta-a9fd5b7defbd"),
    ("TEAM_AGENT_ID", "recovery-haiku"),
    ("TEAM_AGENT_AGENT_ID", "recovery-haiku"),
    ("TEAM_AGENT_OWNER_TEAM_ID", "codex-effort-fix"),
];

static SEQ: AtomicU64 = AtomicU64::new(0);

fn transcript(id: &str, body: &str) {
    let dir = std::env::var_os("TA_RED_TRANSCRIPT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ta-red-transcripts"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{id}.log")), body);
    eprintln!("===== {id} transcript =====\n{body}\n===== end {id} =====");
}

fn render(label: &str, args: &[&str], env: &[(String, String)], out: &Output) -> String {
    format!(
        "$ {label}: team-agent {}\n  env: {:?}\n  exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}\n",
        args.join(" "),
        env,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A test-owned Leader pane: a native executable named `pi` (so tmux reports
/// `pane_current_command=pi`, as for the real Pi Leader) inside a non-ASCII
/// workspace, hosted on either the workspace socket or a private socket.
struct LeaderFixture {
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    socket_name: String,
    socket_path: String,
}

impl Drop for LeaderFixture {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-L", &self.socket_name, "kill-server"])
            .env_remove("TMUX").output();
        if !self.socket_path.is_empty() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl LeaderFixture {
    fn new(tag: &str, on_workspace_socket: bool) -> Self {
        let base = if cfg!(target_os = "macos") { "/private/tmp" } else { "/tmp" };
        let root = PathBuf::from(base).join(format!(
            "ta-rce-it-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let workspace = root.join(NON_ASCII);
        let home = root.join("home");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(home.join(".team-agent/leaders")).unwrap();
        let pi = root.join("bin/pi");
        let mut cc = Command::new("cc")
            .args(["-x", "c", "-", "-o"])
            .arg(&pi)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("PRECONDITION: a C compiler (cc) is required for the native pi stub");
        cc.stdin.take().unwrap()
            .write_all(b"#include <unistd.h>\nint main(void){for(;;)pause();}\n").unwrap();
        let built = cc.wait_with_output().unwrap();
        assert!(built.status.success(), "PRECONDITION: cc failed: {built:?}");
        let socket_name = if on_workspace_socket {
            format!("ta-{}", team_agent::tmux_backend::workspace_short_hash_pub(&workspace))
        } else {
            format!("ta-rce-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
        };
        let started = Command::new("tmux")
            .args(["-u", "-L", &socket_name, "-f", "/dev/null", "new-session", "-d", "-s", "fixture-caller", "-c"])
            .arg(&workspace)
            .arg(format!("exec '{}'", pi.display()))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("LANG", "en_US.UTF-8")
            .output()
            .unwrap();
        assert!(started.status.success(), "PRECONDITION: fixture tmux failed: {started:?}");
        let socket_path = String::from_utf8_lossy(
            &Command::new("tmux").args(["-L", &socket_name, "display-message", "-p", "#{socket_path}"])
                .env_remove("TMUX").output().unwrap().stdout,
        ).trim().to_string();
        let fixture = Self { root, workspace, home, socket_name, socket_path };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fixture.pane_command() != "pi" && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(fixture.pane_command(), "pi", "PRECONDITION: fixture pane must report pane_current_command=pi");
        fixture
    }

    fn pane_command(&self) -> String {
        String::from_utf8_lossy(
            &Command::new("tmux").args(["-L", &self.socket_name, "display-message", "-p", "-t", "%0", "#{pane_current_command}"])
                .env_remove("TMUX").output().unwrap().stdout,
        ).trim().to_string()
    }

    /// Environment of a command run from inside the Leader pane: the real pane tuple,
    /// the Leader's explicit provider, optionally the stale foreign caller tuple.
    fn caller_env(&self, stale: bool) -> Vec<(String, String)> {
        let mut env = vec![
            ("PATH".to_string(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_string(), self.home.to_string_lossy().into_owned()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ("TMUX".to_string(), format!("{},0,0", self.socket_path)),
            ("TMUX_PANE".to_string(), "%0".to_string()),
            ("TEAM_AGENT_LEADER_PROVIDER".to_string(), "pi".to_string()),
        ];
        if stale {
            env.extend(STALE.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        }
        env
    }

    fn cli(&self, stale: bool, args: &[&str]) -> (Vec<(String, String)>, Output) {
        let env = self.caller_env(stale);
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.workspace)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .unwrap();
        (env, out)
    }
}

fn link(doctor: &Value, id: &str) -> (String, String) {
    doctor["links"].as_array().and_then(|links| {
        links.iter().find(|l| l["id"] == id).map(|l| {
            (l["status"].as_str().unwrap_or("").to_string(), l["message"].as_str().unwrap_or("").to_string())
        })
    }).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// RT-6 (RED at baseline): doctor ignores the caller tuple start gates on.
// ---------------------------------------------------------------------------

#[test]
fn rt6_doctor_flags_stale_caller_tuple_instead_of_pass_matches() {
    let fx = LeaderFixture::new("rt6", true);
    let ws = fx.workspace.to_string_lossy().into_owned();
    let mut log = format!(
        "RT-6 doctor truthfulness (black-box, real binary)\nworkspace={ws}\nleader pane=%0@{} (pane_current_command=pi)\n\n",
        fx.socket_path
    );
    let claim_args = ["claim-leader", "--workspace", &ws, "--team", "current", "--json"];
    let (env, claim) = fx.cli(false, &claim_args);
    log.push_str(&render("claim (clean)", &claim_args, &env, &claim));
    let doctor_args = ["doctor", "--workspace", &ws, "--team", "current", "--json"];
    let (env, clean) = fx.cli(false, &doctor_args);
    log.push_str(&render("doctor (clean control)", &doctor_args, &env, &clean));
    let (env, stale) = fx.cli(true, &doctor_args);
    log.push_str(&render("doctor (stale caller tuple)", &doctor_args, &env, &stale));
    let clean_text = String::from_utf8_lossy(&clean.stdout).into_owned();
    let stale_text = String::from_utf8_lossy(&stale.stdout).into_owned();
    let clean_json: Value = serde_json::from_str(&clean_text).unwrap_or(Value::Null);
    let stale_json: Value = serde_json::from_str(&stale_text).unwrap_or(Value::Null);
    let (clean_l6, clean_l6_msg) = link(&clean_json, "L6");
    let (stale_l6, stale_l6_msg) = link(&stale_json, "L6");

    let mut failures = Vec::new();
    if stale_l6 == "PASS" && stale_l6_msg.contains("matches") {
        failures.push(format!("L6: expected non-PASS for invalid caller tuple; actual {stale_l6} \"{stale_l6_msg}\""));
    }
    if !stale_text.contains("invalid_caller_tuple") {
        failures.push("expected reason code `invalid_caller_tuple` in doctor output; actual: absent".into());
    }
    if !(stale_text.contains("TEAM_AGENT_CALLER_PANE_ID") && stale_text.contains("%46")) {
        failures.push("expected field-level detail `TEAM_AGENT_CALLER_PANE_ID` / `%46`; actual: absent".into());
    }
    if stale_json["ok"] != Value::Bool(false) {
        failures.push(format!("expected ok=false; actual ok={}", stale_json["ok"]));
    }
    log.push_str(&format!(
        "\nCONTROL clean: ok={} L6={clean_l6} \"{clean_l6_msg}\"\nSTALE: ok={} L6={stale_l6} \"{stale_l6_msg}\"\nRESULT failures:\n{}\n",
        clean_json["ok"], stale_json["ok"], failures.join("\n")
    ));
    transcript("RT-6", &log);

    assert!(claim.status.success(), "RT-6 PRECONDITION: claim-leader failed; see transcript");
    assert_eq!(clean_json["ok"], Value::Bool(true), "RT-6 CONTROL: clean doctor must be ok=true");
    assert!(clean_l6 == "PASS" && clean_l6_msg.contains("matches"), "RT-6 CONTROL: clean L6 must PASS/matches");
    assert!(!clean_text.contains("invalid_caller_tuple"), "RT-6 CONTROL: clean doctor must not report invalid_caller_tuple");
    assert!(failures.is_empty(), "RT-6 FAILED:\n{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// RT-7 (RED at baseline): the refusal hides its remediation and misreports effects.
// ---------------------------------------------------------------------------

#[test]
fn rt7_start_refusal_surfaces_blocker_and_remediation_without_unknown_effects() {
    let fx = LeaderFixture::new("rt7", false);
    let ws = fx.workspace.to_string_lossy().into_owned();
    let mut log = format!("RT-7 start refusal rendering (black-box)\nfresh workspace={ws}\n\n");
    let human_args = ["start", "workflow-architect", "--provider", "fake", "--workspace", &ws];
    let (env, human) = fx.cli(true, &human_args);
    log.push_str(&render("start human (stale caller tuple)", &human_args, &env, &human));
    let json_args = ["start", "workflow-architect", "--provider", "fake", "--workspace", &ws, "--json"];
    let (env, machine) = fx.cli(true, &json_args);
    log.push_str(&render("start --json (stale caller tuple)", &json_args, &env, &machine));
    let human_text = format!("{}{}", String::from_utf8_lossy(&human.stdout), String::from_utf8_lossy(&human.stderr));
    let json_text = String::from_utf8_lossy(&machine.stdout).into_owned();
    let json: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
    let receipt = &json["steps"][0]["receipt"];
    let state = fx.workspace.join(".team/runtime/state.json");
    let agents_after = std::fs::read(&state).ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|s| s["teams"]["current"]["agents"].clone())
        .unwrap_or(Value::Null);

    let workspace_socket = format!("ta-{}", team_agent::tmux_backend::workspace_short_hash_pub(&fx.workspace));
    let workspace_socket_live = Command::new("tmux").args(["-L", &workspace_socket, "list-sessions"])
        .env_remove("TMUX").output().is_ok_and(|o| o.status.success());

    let mut failures = Vec::new();
    if !human_text.contains("invalid_caller_tuple") {
        failures.push("human: expected blocker code `invalid_caller_tuple`; actual: absent".into());
    }
    if !(human_text.contains("TEAM_AGENT_CALLER_PANE_ID") && human_text.contains("%46")) {
        failures.push("human: expected field-level remediation naming `TEAM_AGENT_CALLER_PANE_ID` and `%46`; actual: absent".into());
    }
    if human_text.contains("unknown effects") {
        failures.push("human: pre-spawn refusal must not claim `unknown effects`; actual: present".into());
    }
    if !json_text.contains("TEAM_AGENT_CALLER_PANE_ID") {
        failures.push("json: expected remediation naming `TEAM_AGENT_CALLER_PANE_ID`; actual: absent".into());
    }
    if json["recovery"].as_str().is_some_and(|r| r.contains("unknown effects")) {
        failures.push(format!("json: recovery claims unknown effects although effects={}", json["effects"]));
    }
    log.push_str(&format!(
        "\nGUARD exit(human)={:?} exit(json)={:?} receipt.status={} receipt.blockers={} effects={} agents_after={} workspace_socket={workspace_socket} live={workspace_socket_live}\nRESULT failures:\n{}\n",
        human.status.code(), machine.status.code(), receipt["status"], receipt["blockers"], json["effects"], agents_after,
        failures.join("\n")
    ));
    transcript("RT-7", &log);

    // Guards: the fail-closed gate still refuses and nothing is spawned.
    assert!(!human.status.success() && !machine.status.success(), "RT-7 GUARD: refusal must exit non-zero");
    assert!(
        receipt["ok"] == Value::Bool(false) && receipt["status"].as_str().is_some_and(|s| s.contains("blocked")),
        "RT-7 GUARD: receipt must stay a blocked refusal (status blocked|preflight_blocked)"
    );
    assert!(receipt["blockers"].to_string().contains("invalid_caller_tuple"), "RT-7 GUARD: JSON blockers keep invalid_caller_tuple");
    let started_workers = json["effects"].as_array().map_or(0, |entries| {
        entries.iter().filter(|e| e["effects"]["worker_panes_started"].as_array().is_some_and(|w| !w.is_empty())).count()
    });
    assert_eq!(started_workers, 0, "RT-7 GUARD: no worker pane may be started");
    assert!(!workspace_socket_live, "RT-7 GUARD: the workspace tmux host must not be created by a refused start");
    assert!(agents_after.as_object().is_none_or(|a| a.is_empty()), "RT-7 GUARD: no worker seat created");
    assert!(failures.is_empty(), "RT-7 FAILED:\n{}", failures.join("\n"));
}

