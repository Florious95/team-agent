//! 0.5.53 P0 · launcher stderr observability + kill-audit RED (verifier).
//!
//! From launcher-kills-team-p0-locate.md §1-5 (surfaces) + §4 (isolated
//! reproduction) + runtime-owner three criteria (msg_7cbb7af52c58), NOT any
//! root-cause design. Baseline `56231ce` (origin/main). Information isolation:
//! the RED is written from the OBSERVED phenomena (raw child stderr discarded,
//! no pre-kill audit) and the locate's file:line surfaces, not from the fix.
//!
//! Three criteria:
//! - **C1 stderr persistence** (RED): a forced non-TTY launcher failure must
//!   persist the BOUNDED, REDACTED raw child stderr (e.g. `not a terminal` /
//!   `[server exited]`) AND a classification into the workspace cli-error
//!   artifact — not only the normalized `leader launcher exited with status N`
//!   integer-status string. Baseline: `emit_cli_error` (cli/emit.rs) writes only
//!   the normalized safe_error; the child stderr (start.rs:1233-1238 inherited,
//!   474-481 only integer status) is discarded to the terminal.
//! - **C2 kill audit** (RED): every load-bearing `kill_server`/`kill_session`
//!   call site must, immediately before the call, emit an audit record carrying
//!   caller + argv + endpoint + targets. Baseline: the load-bearing kill call
//!   sites (cli/mod.rs, lifecycle/restart/rebuild.rs, diagnose/orphans.rs) have
//!   no pre-call audit; a failure cannot be attributed to a caller.
//!   CONTRACT RULING (verifier, re runtime-owner msg_7cd86c86f108): the audit is
//!   LOAD-BEARING and FAIL-CLOSED — if the audit record cannot be written, the
//!   destructive kill MUST be skipped (never "unaudited kill proceeds"). The
//!   audit only RECORDS; it must not wrap/add/extend any kill authority (red
//!   line). best-effort audit is NOT accepted.
//! - **C3 canary survives** (GUARDRAIL, naturally-green): a launcher failure must
//!   NOT kill a pre-existing same-socket session. Baseline already preserves it
//!   (§4.2: status-1 leaves the canary alive) — this is not a RED but a guardrail
//!   whose teeth engage if a future failure-cleanup introduces a kill path (red
//!   line: MUST NOT add/extend kill-server or cleanup authority).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport};

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

/// Gate6 CI-hermeticity revision: test-owned fake `claude` on PATH (PATH-shim
/// discipline). The launcher's `is_installed` gate runs BEFORE the non-TTY
/// attach; on a host without a real Claude binary (CI) the unshimmed fixture
/// dies at `Provider claude command 'claude' not found` and never reaches the
/// child-stderr persistence path under test. The shim mimics
/// `claude -- --version` (prints, exits 0); the asserted failure is the tmux
/// client attach, not the provider process, so assertion semantics are
/// unchanged.
fn write_fake_claude_shim(workspace: &Path) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    std::fs::write(&shim, "#!/bin/sh\necho 'claude shim 0.0.0-fake'\nexit 0\n")
        .expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

/// Run the managed `claude` launcher from OUTSIDE a TTY (stdin/out/err are
/// pipes, TERM unset) so the tmux attach client fails with
/// `open terminal failed: not a terminal` and the launcher returns status 1 —
/// the exact §4.2 reproduction, with no real provider needed (`claude` is a
/// test-owned PATH shim).
fn run_launcher_non_tty(env: &HermeticTestEnv, workspace: &Path) -> Output {
    let shim_dir = write_fake_claude_shim(workspace);
    let shim_path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").expect("PATH present")
    );
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["claude", "--json", "--", "--version"])
        .current_dir(workspace)
        .env("HOME", env.home())
        .env("PATH", shim_path)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run launcher")
}

/// Precise teardown for launcher tests: the non-TTY launcher creates a
/// workspace-scoped tmux server before the client attach fails, and that server
/// survives (§4.2). Kill exactly this test's endpoint, never a foreign server.
struct LauncherResidueGuard {
    endpoint: String,
}

impl Drop for LauncherResidueGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.endpoint, "kill-server"])
            .output();
    }
}

fn cli_error_logs(workspace: &Path) -> Vec<PathBuf> {
    let dir = workspace.join(".team").join("logs");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("cli-error-"))
            {
                out.push(p);
            }
        }
    }
    out
}

/// C1 — a forced non-TTY launcher failure must persist the raw child stderr
/// (bounded, redacted) + a classification into the cli-error artifact. Baseline
/// red: the artifact holds only the normalized `... exited with status N` string;
/// the decisive `not a terminal` / `server exited` raw diagnostic is discarded
/// (locate §2, §5 Confirmed #3).
#[test]
fn c1_launcher_failure_persists_bounded_redacted_child_stderr() {
    let env = HermeticTestEnv::enter("launcher-c1");
    let ws = env.workspace("ws");
    let endpoint = TmuxBackend::for_workspace(&ws)
        .tmux_endpoint()
        .expect("workspace backend has a tmux endpoint");
    let _residue = LauncherResidueGuard { endpoint };
    let out = run_launcher_non_tty(&env, &ws);
    // The launcher must have failed (non-TTY attach), otherwise the fixture did
    // not exercise the failure path.
    assert!(
        !out.status.success(),
        "fixture precondition: non-TTY launcher must fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let logs = cli_error_logs(&ws);
    let combined: String = logs
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n");

    // The raw child diagnostic the client actually printed (§4.2). At baseline
    // this text reaches only the terminal, never the persisted artifact.
    let has_raw_child_stderr = combined.contains("not a terminal")
        || combined.contains("server exited")
        || combined.contains("open terminal failed");
    // A classification/kind beyond the bare normalized status line.
    let has_classification = combined.contains("class")
        || combined.contains("kind")
        || combined.contains("category")
        || combined.contains("stderr");
    assert!(
        has_raw_child_stderr && has_classification,
        "a forced non-TTY launcher failure must persist the BOUNDED REDACTED raw child stderr \
         (e.g. `not a terminal`) AND a classification into the cli-error artifact, not only the \
         normalized `exited with status N` string. logs={logs:?} combined={combined:?}"
    );

    let diagnostics_path = logs
        .iter()
        .flat_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .find_map(|line| {
            line.split_once("launcher_diagnostics=")
                .map(|(_, path)| PathBuf::from(path))
        })
        .unwrap_or_else(|| {
            panic!(
                "launcher failure must name its full diagnostics artifact; logs={logs:?}; combined={combined:?}"
            )
        });
    let diagnostics = std::fs::read_to_string(&diagnostics_path).unwrap_or_else(|error| {
        panic!(
            "read launcher diagnostics {}: {error}",
            diagnostics_path.display()
        )
    });
    assert!(
        diagnostics.contains("startup_stage="),
        "launcher diagnostics must record startup stage: {diagnostics:?}"
    );
    assert!(
        diagnostics.contains("child_stdout=") && diagnostics.contains("child_stderr="),
        "launcher diagnostics must persist both child streams: {diagnostics:?}"
    );
}

/// A failed managed launcher may roll back only the leader session it created.
/// The pre-existing team session is a canary and must remain on the same tmux
/// socket. This is the A-20 failure shape, with an extra assertion that the
/// newly-created leader resource does not leak.
#[test]
fn a20_launcher_failure_preserves_existing_team_and_cleans_own_session() {
    let env = HermeticTestEnv::enter("launcher-a20");
    let ws = env.workspace("ws");
    let backend = TmuxBackend::for_workspace(&ws);
    let endpoint = backend
        .tmux_endpoint()
        .expect("workspace backend has a tmux endpoint");
    let _residue = LauncherResidueGuard {
        endpoint: endpoint.clone(),
    };
    let canary = SessionName::new("team-existing");
    let create = Command::new("tmux")
        .args([
            "-L",
            &endpoint,
            "new-session",
            "-d",
            "-s",
            canary.as_str(),
            "sh",
            "-lc",
            "sleep 30",
        ])
        .output()
        .expect("create pre-existing team canary");
    assert!(create.status.success(), "create canary: {:?}", create);
    let canary_pid = backend
        .list_targets()
        .expect("list canary pane")
        .into_iter()
        .find(|target| target.session == canary)
        .and_then(|target| target.pane_pid)
        .expect("canary pane pid");
    let server_pid = parent_pid(canary_pid).expect("canary pane must have a tmux server parent");

    let out = run_launcher_non_tty(&env, &ws);
    assert!(!out.status.success(), "fixture must fail launcher attach");
    assert!(
        backend.has_session(&canary).unwrap_or(false),
        "launcher failure must not kill pre-existing team session; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let leader_left = backend
        .list_targets()
        .unwrap_or_default()
        .iter()
        .any(|target| target.session.as_str().starts_with("team-agent-leader-"));
    assert!(
        !leader_left,
        "launcher failure must clean only its own newly-created leader session"
    );

    // A-20b: a failure-path backend is not the owner of the already-running
    // tmux server. Calling the server-level cleanup seam must be fail-safe,
    // even though the pre-existing session survived the launcher rollback.
    TmuxBackend::for_workspace(&ws).kill_server();
    assert!(
        backend.has_session(&canary).unwrap_or(false) && pid_alive(server_pid),
        "A-20b: launcher failure must leave the pre-existing session and its tmux server alive; canary={} server={server_pid} stdout={} stderr={}",
        canary.as_str(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn parent_pid(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// C2 — every LOAD-BEARING kill_server/kill_session call site must emit a
/// pre-call audit (caller/argv/endpoint/targets) so a server death is
/// attributable. Baseline red: the load-bearing kill call sites (cli/mod.rs,
/// lifecycle/restart/rebuild.rs, diagnose/orphans.rs — per runtime-owner surface
/// enumeration) have no pre-call audit; a launcher/lifecycle failure leaves no
/// evidence of who killed a server (locate §5 Not-established #1). Structural
/// static check over each load-bearing source file (the incident left no runtime
/// audit to read).
#[test]
fn c2_load_bearing_kill_calls_have_pre_call_audit() {
    // The load-bearing kill call sites, not the low-level TmuxBackend impl.
    let load_bearing = [
        "src/cli/mod.rs",
        "src/lifecycle/restart/rebuild.rs",
        "src/diagnose/orphans.rs",
    ];
    let mut offenders = Vec::new();
    for rel in load_bearing {
        let path = format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Does this file invoke a server/session kill (directly or via backend)?
        let kills = text.contains("kill_server")
            || text.contains("kill_session")
            || text.contains("kill-server")
            || text.contains("kill-session");
        if !kills {
            continue;
        }
        // If it kills, it must carry a pre-call audit record naming
        // caller+argv+endpoint+targets. Baseline has none.
        let has_audit = text.contains("kill_audit")
            || text.contains("pre_kill_audit")
            || (text.contains("caller") && text.contains("targets") && text.contains("endpoint"));
        if !has_audit {
            offenders.push(rel);
        }
    }
    assert!(
        offenders.is_empty(),
        "every load-bearing kill_server/kill_session call site must emit a pre-call audit carrying \
         caller+argv+endpoint+targets so a server death is attributable; these load-bearing files \
         kill without audit: {offenders:?} (locate §5 Not-established #1)."
    );
}

/// C3 — GUARDRAIL: a launcher failure must not kill the tmux server or an
/// unrelated session. Rollback is permitted only through the explicit
/// self-resource cleanup helper covered by the A-20 canary test.
#[test]
fn c3_launcher_failure_path_has_no_reachable_kill_guardrail() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/leader/start.rs");
    let text = std::fs::read_to_string(src).expect("read leader/start.rs");
    // A launcher failure must never gain kill-server authority. The only
    // permitted session/pane cleanup is centralized in the self-resource
    // rollback helper, which receives the pre-launch ownership bit.
    let calls_kill_server = text.contains(".kill_server(");
    assert!(
        !calls_kill_server
            && text.contains("cleanup_managed_leader_resources")
            && text.contains("session_existed_before"),
        "guardrail: launcher rollback must not gain kill-server authority and must be \
         constrained to resources tracked as self-created."
    );
}
