//! Swallow-family batch 2: tmux/transport query layer observability.
//!
//! Basis: `.team/artifacts/swallow-family-slices.md` batch 2 (7 items; family-level
//! contracts per the batch-1 precedent: inject a failure -> a failure event with a
//! non-null error must land AND the return value must not fake-green).
//! Rule source: user CLAUDE.md §5 — every "looks executed but had no effect" needs a
//! log line that explains why.
//!
//! Contract ①: tmux_backend.rs:816-818 — one pane's pane_pid query erroring at the
//!   runner level (`?`) currently fails the WHOLE list_targets call (single-point
//!   failure amplification); it must instead record `tmux.pane_pid_query_failed` and
//!   keep collecting the remaining panes (pane_pid=None for the failed one).
//! Contract ②: A1 proper — startup prompt handling that cannot even capture the pane
//!   currently swallows the failure (`let _ =` family); the tick must surface a
//!   startup-prompt failure event with a non-null error and must NOT mark the agent's
//!   startup prompts as handled/complete.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{
    Transport, };

/// Batch 2 ①: one pane's display-message pane_pid probe erroring must not fail the
/// whole list_targets — the other panes must survive with their pids, the failed pane
/// degrades to pane_pid=None, and a `pane_pid_query_failed` event with a non-null
/// error lands in the workspace event log.
#[test]
fn b2_list_targets_survives_single_pane_pid_query_failure() {
    let ws = tmp_ws("b2-panes");
    let backend = TmuxBackend::with_runner_for_workspace(Box::new(PanePidFailRunner), &ws);

    let result = backend.list_targets();

    let mut failures = Vec::new();
    match &result {
        Err(error) => failures.push(format!(
            "①: a single pane's probe failure must not fail the whole list_targets \
(tmux_backend.rs:816-818 `?` amplification); got Err({error})"
        )),
        Ok(panes) => {
            if panes.len() != 2 {
                failures.push(format!(
                    "①: both panes must survive the single-pane probe failure; got {panes:?}"
                ));
            }
            if !panes
                .iter()
                .any(|pane| pane.pane_id.as_str() == "%2" && pane.pane_pid == Some(4242))
            {
                failures.push(format!(
                    "①: the healthy pane %2 must keep its probed pid; got {panes:?}"
                ));
            }
        }
    }
    let events = events_text(&ws);
    let probe_line = events
        .lines()
        .find(|line| line.contains("pane_pid_query_failed"));
    match probe_line {
        None => failures.push(
            "①: the failed pane probe must write a `tmux.pane_pid_query_failed` event \
(CLAUDE.md §5: a query that silently yields nothing must explain why)"
                .to_string(),
        ),
        Some(line) => {
            if line.contains("\"error\":null") || line.contains("\"error\": null") {
                failures.push(format!(
                    "①: the failure event must carry a non-null error; line={line}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "batch2 ① list_targets resilience/observability contract failed:\n{}",
        failures.join("\n")
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

/// list-panes yields two panes (format string carries no pane_pid, forcing the
/// display-message fallback); the %1 probe errors at the runner level, %2 succeeds.
struct PanePidFailRunner;

impl CommandRunner for PanePidFailRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|arg| arg == "list-panes") {
            return Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: concat!(
                    "%1\tteam-x\t0\tw1\t0\t/dev/ttys001\tcodex\t1\t/tmp\t1\t0\n",
                    "%2\tteam-x\t1\tw2\t0\t/dev/ttys002\tcodex\t0\t/tmp\t1\t0\n",
                )
                .to_string(),
                stderr: String::new(),
            });
        }
        if argv.iter().any(|arg| arg == "display-message") {
            if argv.iter().any(|arg| arg == "%1") {
                return Err(std::io::Error::other("injected pane probe failure"));
            }
            return Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: "4242\n".to_string(),
                stderr: String::new(),
            });
        }
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

fn events_text(ws: &Path) -> String {
    std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-swallow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
