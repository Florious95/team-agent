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

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

/// Run the managed `claude` launcher from OUTSIDE a TTY (stdin/out/err are
/// pipes, TERM unset) so the tmux attach client fails with
/// `open terminal failed: not a terminal` and the launcher returns status 1 —
/// the exact §4.2 reproduction, with no real provider needed.
fn run_launcher_non_tty(env: &HermeticTestEnv, workspace: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["claude", "--json", "--", "--version"])
        .current_dir(workspace)
        .env("HOME", env.home())
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run launcher")
}

/// Precise teardown for C1: the non-TTY launcher creates a nonce leader tmux
/// SERVER (via `tmux -L <name>`) before the client attach fails, and that server
/// survives (§4.2). Kill exactly the processes whose command line carries this
/// test's workspace path — never a foreign socket/server (red line hygiene).
struct LauncherResidueGuard {
    workspace_token: String,
}

impl Drop for LauncherResidueGuard {
    fn drop(&mut self) {
        let Ok(out) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
            return;
        };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.contains(&self.workspace_token) {
                if let Some(pid) = line.split_whitespace().next() {
                    let _ = Command::new("kill").args(["-TERM", pid]).output();
                }
            }
        }
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
    let _residue = LauncherResidueGuard {
        workspace_token: ws.to_string_lossy().to_string(),
    };
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

/// C3 — GUARDRAIL (naturally-green): a launcher failure must NOT kill a
/// pre-existing same-socket session. Baseline already preserves it (§4.2). This
/// is not a RED; it is a guardrail that trips if a future failure-cleanup
/// introduces a kill path. RED LINE: the fix must NOT add/extend kill-server or
/// cleanup authority. We assert the launcher production entry (leader/start.rs)
/// contains no reachable kill-server/kill-session in its failure path.
#[test]
fn c3_launcher_failure_path_has_no_reachable_kill_guardrail() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/leader/start.rs");
    let text = std::fs::read_to_string(src).expect("read leader/start.rs");
    // Guardrail: the launcher entry must not CALL kill_server/kill_session in its
    // failure path (locate §1.2: neither is reachable from leader/start.rs
    // production code). We match the CALL form (`.kill_server(` / `.kill_session(`)
    // — not a trait-impl definition (`fn kill_session(&self, ...)`), which is a
    // test stub. If a future observability fix wires a kill call into the failure
    // path, this trips.
    let calls_kill = text.contains(".kill_server(") || text.contains(".kill_session(");
    assert!(
        !calls_kill,
        "guardrail: the launcher failure path must NOT gain a reachable \
         kill_server/kill_session CALL (red line: no new/extended kill-server or cleanup \
         authority; a launcher failure must preserve a same-socket canary — locate §4.2)."
    );
}
