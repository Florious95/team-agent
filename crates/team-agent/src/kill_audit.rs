//! Durable, fail-closed observation immediately before destructive transport calls.

use std::path::Path;

use serde_json::{json, Value};

use crate::event_log::{EventLog, EventLogError};
use crate::transport::{BackendKind, Transport};

pub(crate) const KILL_SERVER: &str = "kill-server";
pub(crate) const KILL_SESSION: &str = "kill-session";

pub(crate) fn pre_kill_audit(
    event_log: &EventLog,
    transport: &dyn Transport,
    caller: &str,
    action: &str,
    targets: &[String],
) -> Result<(), EventLogError> {
    let endpoint = transport.tmux_endpoint();
    let argv = destructive_argv(transport.kind(), endpoint.as_deref(), action, targets);
    event_log.write(
        "transport.pre_kill_audit",
        json!({
            "phase": "pre_call",
            "caller": caller,
            "caller_pid": std::process::id(),
            "action": action,
            "argv": argv,
            "endpoint": endpoint,
            "targets": targets,
            "workspace": Value::Null,
            "team": Value::Null,
            "generation": Value::Null,
            "owner_evidence": "legacy_unscoped",
            "reason": "legacy_callsite",
        }),
    )?;
    Ok(())
}

pub(crate) fn pre_kill_audit_scoped(
    event_log: &EventLog,
    transport: &dyn Transport,
    caller: &str,
    action: &str,
    targets: &[String],
    workspace: &Path,
    team: Option<&str>,
    generation: Option<&str>,
    owner_evidence: &str,
    reason: &str,
) -> Result<(), EventLogError> {
    let endpoint = transport.tmux_endpoint();
    let argv = destructive_argv(transport.kind(), endpoint.as_deref(), action, targets);
    event_log.write(
        "transport.pre_kill_audit",
        json!({
            "phase": "pre_call",
            "caller": caller,
            "caller_pid": std::process::id(),
            "action": action,
            "argv": argv,
            "endpoint": endpoint,
            "targets": targets,
            "workspace": workspace.to_string_lossy(),
            "team": team,
            "generation": generation,
            "owner_evidence": owner_evidence,
            "reason": reason,
        }),
    )?;
    Ok(())
}

fn destructive_argv(
    kind: BackendKind,
    endpoint: Option<&str>,
    action: &str,
    targets: &[String],
) -> Vec<String> {
    let mut argv = if kind == BackendKind::Tmux {
        let mut argv = vec!["tmux".to_string()];
        if let Some(endpoint) = endpoint.filter(|endpoint| !endpoint.is_empty()) {
            if endpoint != "default" {
                argv.push(if Path::new(endpoint).is_absolute() {
                    "-S".to_string()
                } else {
                    "-L".to_string()
                });
                argv.push(endpoint.to_string());
            }
        }
        argv
    } else {
        vec![format!("{kind:?}").to_ascii_lowercase()]
    };
    argv.push(action.to_string());
    if action == KILL_SESSION || action == "kill-pane" {
        for target in targets {
            argv.push("-t".to_string());
            argv.push(target.clone());
        }
    }
    argv
}
