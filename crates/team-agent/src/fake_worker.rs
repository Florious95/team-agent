//! `team-agent fake-worker` — Rust port of Python `team_agent.fake_worker` (SKELETON).
//!
//! Truth source (READ-ONLY) `team-agent-public` @ v0.2.11: `src/team_agent/fake_worker.py`.
//! A subscription-free backing program for `Provider::Fake`: it lets the real spawn path
//! (`launch(dry_run=false)` → tmux window) be exercised with NO real provider/subscription, so the
//! acceptance framework's cheap real-machine Tier-1 can drive the real daemon.
//!
//! Behavior (port target):
//!   - print `TEAM_AGENT_FAKE_READY agent=<id>` (idle marker the daemon's classifier recognizes).
//!   - read input line-by-line; on a non-empty line print `TEAM_AGENT_FAKE_WORKING agent=<id>`;
//!     a `TEAM_AGENT_MESSAGE <json>` line OR a rendered `Team Agent message from …:` block (ending
//!     `[team-agent-token:<tok>]`) → build a `result_envelope_v1` and report it via
//!     `messaging::report_result`, echoing the envelope JSON; then print READY again.
//!
//! §84/MUST-NOT-13: a fake worker, no provider client. §10: implementation must be panic-free
//! (porter adds the deny + body; this skeleton is `unimplemented!()`).
#![allow(dead_code)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use std::io::{BufRead, Write};
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FakeWorkerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("report_result: {0}")]
    Report(String),
}

/// Run the fake-worker loop against `input`/`output` (stdin/stdout in `main`, in-memory in tests).
/// Port of `fake_worker.py:main` — the argv parsing (`--workspace`/`--agent-id`) lives in the
/// `fake-worker` subcommand dispatch; this entry takes them already resolved so it is unit-testable.
pub fn run(
    workspace: &Path,
    agent_id: &str,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), FakeWorkerError> {
    writeln!(output, "TEAM_AGENT_FAKE_READY agent={agent_id}")?;
    let mut block: Option<RenderedBlock> = None;
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(block) = block.as_mut() {
                block.lines.push(String::new());
            }
            continue;
        }

        writeln!(output, "TEAM_AGENT_FAKE_WORKING agent={agent_id}")?;
        if let Some(raw) = trimmed.strip_prefix("TEAM_AGENT_MESSAGE ") {
            let payload: serde_json::Value = serde_json::from_str(raw)?;
            let message_id = payload
                .get("message_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let task_id = payload.get("task_id").and_then(serde_json::Value::as_str);
            report_fake_result(workspace, agent_id, message_id, task_id, None, &mut output)?;
            block = None;
        } else if let Some(parsed) = parse_rendered_header(trimmed) {
            block = Some(parsed);
        } else if let Some(token) = parse_token(trimmed) {
            if let Some(current) = block.take() {
                let content = current.content();
                report_fake_result(
                    workspace,
                    agent_id,
                    &token,
                    current.task_id.as_deref(),
                    Some(&content),
                    &mut output,
                )?;
            }
        } else if let Some(current) = block.as_mut() {
            current.lines.push(trimmed.to_string());
        }
        writeln!(output, "TEAM_AGENT_FAKE_READY agent={agent_id}")?;
    }
    Ok(())
}

#[derive(Debug)]
struct RenderedBlock {
    task_id: Option<String>,
    lines: Vec<String>,
}

impl RenderedBlock {
    fn content(&self) -> String {
        self.lines.join("\n").trim().to_string()
    }
}

fn parse_rendered_header(line: &str) -> Option<RenderedBlock> {
    let rest = line.strip_prefix("Team Agent message from ")?;
    let header = rest.strip_suffix(':')?;
    let task_id = header
        .split_once(" for ")
        .map(|(_, task)| task.trim().to_string())
        .filter(|task| !task.is_empty());
    Some(RenderedBlock {
        task_id,
        lines: Vec::new(),
    })
}

fn parse_token(line: &str) -> Option<String> {
    let token = line
        .strip_prefix("[team-agent-token:")
        .and_then(|rest| rest.strip_suffix(']'))?
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn report_fake_result(
    workspace: &Path,
    agent_id: &str,
    message_id: &str,
    task_id: Option<&str>,
    source_content: Option<&str>,
    output: &mut impl Write,
) -> Result<(), FakeWorkerError> {
    let envelope = fake_envelope(workspace, agent_id, message_id, task_id);
    let owner_team = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let _ = source_content;
    crate::messaging::report_result_for_owner_team(workspace, &envelope, owner_team.as_deref())
        .map_err(|e| FakeWorkerError::Report(e.to_string()))?;
    writeln!(output, "{}", serde_json::to_string(&envelope)?)?;
    Ok(())
}

fn fake_envelope(
    workspace: &Path,
    agent_id: &str,
    message_id: &str,
    task_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": "result_envelope_v1",
        "task_id": task_id.unwrap_or("manual"),
        "agent_id": agent_id,
        "status": "success",
        "summary": format!("Fake worker handled message {message_id}"),
        "changes": [],
        "tests": [{"command": "fake-provider", "status": "passed"}],
        "risks": [],
        "artifacts": [{
            "path": workspace.join(".team").join("logs").join(format!("{agent_id}.scrollback")).to_string_lossy(),
            "description": "tmux scrollback for fake worker"
        }],
        "next_actions": []
    })
}

#[cfg(test)]
mod tests {
    //! FAKE-WORKER RED — `run` is the `unimplemented!()` skeleton today, so these PANIC (RED) until the
    //! porter ports `fake_worker.py`. Golden captured live from
    //! `PYTHONPATH=…/team-agent-public/src python3 -m team_agent.fake_worker --workspace <ws> --agent-id w1`:
    //! both the `TEAM_AGENT_MESSAGE {json}` line and the rendered `Team Agent message from leader for t1:`
    //! block (ending `[team-agent-token:m1]`) report the SAME `result_envelope_v1` via the real
    //! `messaging::report_result`, and the daemon-recognizable READY/WORKING markers are printed.
    use super::run;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::db::schema::open_db;
    use crate::message_store::MessageStore;

    fn fake_ws() -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-rs-fakew-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Read back the single result the fake worker stored via `messaging::report_result`.
    fn stored_result(ws: &std::path::Path) -> (String, String, String, serde_json::Value) {
        let store = MessageStore::open(ws).unwrap();
        let conn = open_db(store.db_path()).unwrap();
        let mut stmt = conn
            .prepare("select task_id, agent_id, status, envelope from results")
            .unwrap();
        let row = stmt
            .query_row([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .expect("exactly one stored fake-worker result");
        let envelope: serde_json::Value = serde_json::from_str(&row.3).unwrap();
        (row.0, row.1, row.2, envelope)
    }

    /// Assert the stored envelope matches the captured Python golden (message_id m1, task_id t1).
    fn assert_golden_envelope(ws: &std::path::Path) {
        let (task_id, agent_id, status, env) = stored_result(ws);
        assert_eq!(
            (task_id.as_str(), agent_id.as_str(), status.as_str()),
            ("t1", "w1", "success"),
            "stored result row must be task_id=t1 agent_id=w1 status=success"
        );
        assert_eq!(
            env["schema_version"],
            serde_json::json!("result_envelope_v1")
        );
        assert_eq!(
            env["summary"],
            serde_json::json!("Fake worker handled message m1")
        );
        assert_eq!(
            env["tests"],
            serde_json::json!([{"command": "fake-provider", "status": "passed"}])
        );
        let artifact_path = env["artifacts"][0]["path"].as_str().unwrap_or_default();
        assert!(
            artifact_path.ends_with(".team/logs/w1.scrollback"),
            "artifact path must be <ws>/.team/logs/w1.scrollback; got {artifact_path}"
        );
    }

    // TEAM_AGENT_MESSAGE form: `TEAM_AGENT_MESSAGE {json}` line → real report_result stores the golden
    // result_envelope_v1; READY (idle) + WORKING markers printed. Golden _report_fake_result.
    #[test]
    fn fake_worker_team_agent_message_reports_golden_envelope() {
        let ws = fake_ws();
        let input = "TEAM_AGENT_MESSAGE {\"message_id\":\"m1\",\"task_id\":\"t1\"}\n";
        let mut out: Vec<u8> = Vec::new();
        run(&ws, "w1", Cursor::new(input.as_bytes()), &mut out).expect("fake worker run");

        assert_golden_envelope(&ws);
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("TEAM_AGENT_FAKE_READY agent=w1"),
            "must print the READY idle marker; got {printed}"
        );
        assert!(
            printed.contains("TEAM_AGENT_FAKE_WORKING agent=w1"),
            "must print the WORKING marker on a non-empty line; got {printed}"
        );
    }

    // Rendered-message form: a `Team Agent message from leader for t1:` block ending `[team-agent-token:m1]`
    // parses to the SAME envelope (message_id m1 from the token, task_id t1 from `for t1`). Golden
    // _parse_rendered_message + _report_fake_result.
    #[test]
    fn fake_worker_rendered_message_reports_same_golden_envelope() {
        let ws = fake_ws();
        let input = "Team Agent message from leader for t1:\nplease do X\n[team-agent-token:m1]\n";
        let mut out: Vec<u8> = Vec::new();
        run(&ws, "w1", Cursor::new(input.as_bytes()), &mut out).expect("fake worker run");

        assert_golden_envelope(&ws);
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("TEAM_AGENT_FAKE_WORKING agent=w1"),
            "rendered form prints WORKING per line; got {printed}"
        );
    }
}
