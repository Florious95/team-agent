use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cli::CliError;
use crate::coordinator::health::{
    coordinator_metadata_ok, pid_is_running, read_coordinator_metadata, terminate_pid_tree,
};
use crate::coordinator::types::{OrphanReason, Pid, WorkspacePath};
use crate::tmux_backend::TmuxBackend;
use crate::transport::{SessionName, Transport};

#[derive(Debug, Clone)]
struct OrphanRecord {
    kind: &'static str,
    pid: Option<Pid>,
    session: Option<String>,
    tmux_socket: Option<String>,
    workspace: Option<PathBuf>,
    reason: OrphanReason,
    command: Option<String>,
    action: &'static str,
}

#[derive(Debug, Clone)]
struct ScanReport {
    scanned: usize,
    orphans: Vec<OrphanRecord>,
}

pub fn orphan_gate_json(fix: bool, confirm: bool) -> Result<Value, CliError> {
    if fix && !confirm {
        return Ok(json!({
            "ok": false,
            "gate": "orphans",
            "status": "refused",
            "reason": "fix_requires_confirm",
            "action": "re-run with --gate orphans --fix --confirm",
        }));
    }
    let report = scan_orphans_bounded(false);
    if report.orphans.is_empty() {
        return Ok(json!({
            "ok": true,
            "gate": "orphans",
            "status": "passed",
            "scanned": report.scanned,
            "dry_run": !fix,
            "scanned_at": chrono::Utc::now().to_rfc3339(),
            "action_required": false,
            "fix": fix,
            "orphans": [],
        }));
    }
    if fix {
        return fix_orphans(report);
    }
    Ok(json!({
        "ok": false,
        "gate": "orphans",
        "status": "failed",
        "scanned": report.scanned,
        "dry_run": true,
        "scanned_at": chrono::Utc::now().to_rfc3339(),
        "action_required": true,
        "fix": false,
        "orphans": orphan_values(&report.orphans),
    }))
}

pub fn cleanup_orphans_json(confirm: bool) -> Result<Value, CliError> {
    let report = scan_orphans_bounded(false);
    if confirm {
        if report.orphans.is_empty() {
            return Ok(json!({
                "ok": true,
                "scanned": report.scanned,
                "orphans": [],
                "dry_run": false,
                "scanned_at": chrono::Utc::now().to_rfc3339(),
                "killed": [],
                "failed": [],
            }));
        }
        return cleanup_confirmed(report);
    }
    Ok(json!({
        "ok": true,
        "scanned": report.scanned,
        "orphans": orphan_values(&report.orphans),
        "dry_run": true,
        "scanned_at": chrono::Utc::now().to_rfc3339(),
        "action_required": "re-run with --confirm to send SIGTERM",
    }))
}

pub fn has_orphan_residue() -> bool {
    !scan_orphans_bounded(false).orphans.is_empty()
}

pub fn orphan_blocker_detail() -> String {
    let report = scan_orphans_bounded(false);
    if report.orphans.is_empty() {
        return "no orphan coordinator residue detected".to_string();
    }
    report
        .orphans
        .iter()
        .map(|orphan| {
            let target = orphan
                .pid
                .map(|pid| format!("pid={pid}"))
                .or_else(|| orphan.session.as_ref().map(|s| format!("session={s}")))
                .unwrap_or_else(|| "target=unknown".to_string());
            let workspace = orphan
                .workspace
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "workspace=unknown".to_string());
            format!(
                "{} {target} workspace={workspace} reason={}",
                orphan.kind,
                reason_key(&orphan.reason)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn fix_orphans(report: ScanReport) -> Result<Value, CliError> {
    let cleanup = cleanup_report(report);
    let residual = scan_orphans(false);
    Ok(json!({
        "ok": residual.orphans.is_empty() && cleanup.failed.is_empty(),
        "gate": "orphans",
        "status": if residual.orphans.is_empty() && cleanup.failed.is_empty() { "fixed" } else { "failed" },
        "scanned": cleanup.scanned,
        "dry_run": false,
        "scanned_at": chrono::Utc::now().to_rfc3339(),
        "action_required": !residual.orphans.is_empty() || !cleanup.failed.is_empty(),
        "fix": true,
        "orphans": orphan_values(&residual.orphans),
        "killed": cleanup.killed,
        "failed": cleanup.failed,
    }))
}

fn cleanup_confirmed(report: ScanReport) -> Result<Value, CliError> {
    let cleanup = cleanup_report(report);
    let residual = scan_orphans(false);
    Ok(json!({
        "ok": residual.orphans.is_empty() && cleanup.failed.is_empty(),
        "scanned": cleanup.scanned,
        "orphans": orphan_values(&residual.orphans),
        "dry_run": false,
        "scanned_at": chrono::Utc::now().to_rfc3339(),
        "killed": cleanup.killed,
        "failed": cleanup.failed,
    }))
}

struct CleanupReport {
    scanned: usize,
    killed: Vec<Value>,
    failed: Vec<Value>,
}

fn cleanup_report(report: ScanReport) -> CleanupReport {
    let protected = protected_pids();
    let mut killed = Vec::new();
    let mut failed = Vec::new();
    for orphan in &report.orphans {
        if let Some(pid) = orphan.pid {
            if protected.contains(&pid.get()) {
                failed.push(orphan_value(orphan, "skipped"));
                continue;
            }
            if terminate_pid_tree(pid) {
                killed.push(orphan_value(orphan, "killed"));
            } else {
                failed.push(orphan_value(orphan, "failed"));
            }
            continue;
        }
        if kill_tmux_session(orphan) {
            killed.push(orphan_value(orphan, "killed"));
        } else {
            failed.push(orphan_value(orphan, "failed"));
        }
    }
    CleanupReport {
        scanned: report.scanned,
        killed,
        failed,
    }
}

fn scan_orphans(include_unparsed: bool) -> ScanReport {
    let protected = protected_pids();
    let mut scanned = 0;
    let mut orphans = Vec::new();
    for process in coordinator_processes() {
        if protected.contains(&process.pid.get()) {
            continue;
        }
        scanned += 1;
        let Some(workspace) = parse_workspace_arg(&process.command) else {
            if include_unparsed {
                orphans.push(OrphanRecord {
                    kind: "coordinator_process",
                    pid: Some(process.pid),
                    session: None,
                    tmux_socket: None,
                    workspace: None,
                    reason: OrphanReason::CmdlineUnparsed,
                    command: Some(process.command),
                    action: "would_kill",
                });
            }
            continue;
        };
        if let Some(reason) = classify_workspace_orphan(&workspace, process.pid) {
            orphans.push(OrphanRecord {
                kind: "coordinator_process",
                pid: Some(process.pid),
                session: None,
                tmux_socket: None,
                workspace: Some(workspace),
                reason,
                command: Some(process.command),
                action: "would_kill",
            });
        }
    }
    for orphan in coordinator_pid_file_orphans() {
        scanned += 1;
        orphans.push(orphan);
    }
    for orphan in tmux_session_orphans() {
        scanned += 1;
        orphans.push(orphan);
    }
    for orphan in provider_mcp_process_orphans() {
        scanned += 1;
        orphans.push(orphan);
    }
    ScanReport { scanned, orphans }
}

fn coordinator_pid_file_orphans() -> Vec<OrphanRecord> {
    temp_scan_roots()
        .into_iter()
        .flat_map(|root| match std::fs::read_dir(root) {
            Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        })
        .filter_map(|entry| {
            let workspace = entry.path();
            if !workspace.is_dir() || ephemeral_workspace_hint(&workspace).is_none() {
                return None;
            }
            let pid_path = crate::model::paths::runtime_dir(&workspace).join("coordinator.pid");
            let pid = read_pid_file(&pid_path)?;
            let workspace_path = WorkspacePath::new(workspace.clone());
            let metadata = read_coordinator_metadata(&workspace_path);
            let reason = if pid_is_running(pid).ok() != Some(true) {
                OrphanReason::PidNotRunning
            } else if metadata.is_some() && !coordinator_metadata_ok(metadata.as_ref(), pid) {
                OrphanReason::MetadataMismatch
            } else {
                OrphanReason::EphemeralTempdirPattern {
                    hint: ephemeral_workspace_hint(&workspace)
                        .unwrap_or_else(|| "ephemeral_workspace".to_string()),
                }
            };
            Some(OrphanRecord {
                kind: "coordinator_metadata",
                pid: Some(pid),
                session: None,
                tmux_socket: None,
                workspace: Some(workspace),
                reason,
                command: None,
                action: "would_kill",
            })
        })
        .collect()
}

fn tmux_session_orphans() -> Vec<OrphanRecord> {
    tmux_socket_names()
        .into_iter()
        .flat_map(|socket| {
            tmux_list_panes(&socket)
                .into_iter()
                .filter_map(move |pane| {
                    let workspace = pane.workspace?;
                    if !is_orphan_marker_workspace(&workspace) {
                        return None;
                    }
                    let reason = classify_workspace_without_pid(&workspace)?;
                    Some(OrphanRecord {
                        kind: "tmux_session",
                        pid: None,
                        session: Some(pane.session),
                        tmux_socket: Some(socket.clone()),
                        workspace: Some(workspace),
                        reason,
                        command: pane.command,
                        action: "would_kill",
                    })
                })
        })
        .collect()
}

#[derive(Debug)]
struct TmuxPaneRow {
    session: String,
    workspace: Option<PathBuf>,
    command: Option<String>,
}

fn tmux_socket_names() -> Vec<String> {
    let mut names = BTreeSet::new();
    for root in tmux_socket_roots() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ta-") {
                names.insert(name);
            }
        }
    }
    names.into_iter().collect()
}

fn tmux_socket_roots() -> Vec<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let mut roots = vec![PathBuf::from(format!("/tmp/tmux-{uid}"))];
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir).join(format!("tmux-{uid}")));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn tmux_list_panes(socket: &str) -> Vec<TmuxPaneRow> {
    TmuxBackend::for_socket_name(socket)
        .list_targets()
        .unwrap_or_default()
        .into_iter()
        .map(|pane| TmuxPaneRow {
            session: pane.session.as_str().to_string(),
            workspace: pane.current_path,
            command: pane.current_command,
        })
        .collect()
}

fn provider_mcp_process_orphans() -> Vec<OrphanRecord> {
    ps_command_rows()
        .into_iter()
        .filter(|row| is_provider_or_mcp_workspace_command(&row.command))
        .filter_map(|process| {
            let workspace = parse_workspace_arg(&process.command)?;
            if !is_orphan_marker_workspace(&workspace) {
                return None;
            }
            let reason = classify_workspace_without_pid(&workspace)?;
            Some(OrphanRecord {
                kind: if process.command.contains("mcp-server") {
                    "mcp_process"
                } else {
                    "provider_process"
                },
                pid: Some(process.pid),
                session: None,
                tmux_socket: None,
                workspace: Some(workspace),
                reason,
                command: Some(process.command),
                action: "would_kill",
            })
        })
        .collect()
}

fn temp_scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir));
    }
    roots.push(std::env::temp_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn read_pid_file(path: &Path) -> Option<Pid> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = text.trim().parse::<u32>().ok()?;
    Some(Pid::new(pid))
}

fn scan_orphans_bounded(include_unparsed: bool) -> ScanReport {
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut scanned = 0;
    let mut by_key = BTreeMap::new();
    loop {
        let report = scan_orphans(include_unparsed);
        scanned = scanned.max(report.scanned);
        for orphan in report.orphans {
            by_key.insert(orphan_key(&orphan), orphan);
        }
        if !by_key.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    ScanReport {
        scanned,
        orphans: by_key.into_values().collect(),
    }
}

fn orphan_key(orphan: &OrphanRecord) -> String {
    if let Some(pid) = orphan.pid {
        return format!("pid:{pid}");
    }
    if let Some(session) = &orphan.session {
        return format!(
            "session:{}:{session}",
            orphan.tmux_socket.as_deref().unwrap_or("default")
        );
    }
    orphan.kind.to_string()
}

#[derive(Debug, Clone)]
struct ProcessRow {
    pid: Pid,
    command: String,
}

fn coordinator_processes() -> Vec<ProcessRow> {
    ps_command_rows()
        .into_iter()
        .filter(|row| is_team_agent_coordinator_command(&row.command))
        .collect()
}

fn ps_command_rows() -> Vec<ProcessRow> {
    let output = match crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps").args(["-axww", "-o", "pid=,command="]),
        "ps_table",
        None,
    )
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_command_line)
        .collect()
}

fn parse_ps_command_line(line: &str) -> Option<ProcessRow> {
    let line = line.trim_start();
    let split = line
        .find(char::is_whitespace)
        .unwrap_or(line.len());
    let pid = line.get(..split)?.trim().parse::<u32>().ok()?;
    let command = line.get(split..)?.trim().to_string();
    Some(ProcessRow {
        pid: Pid::new(pid),
        command,
    })
}

fn is_team_agent_coordinator_command(command: &str) -> bool {
    command.contains("team-agent")
        && command.contains("coordinator")
        && command.contains("--workspace")
}

fn is_provider_or_mcp_workspace_command(command: &str) -> bool {
    command.contains("--workspace")
        && (command.contains("mcp-server")
            || command.contains(" codex ")
            || command.ends_with(" codex")
            || command.contains(" claude ")
            || command.ends_with(" claude")
            || command.contains("claude-code")
            || command.contains("fake-worker"))
}

fn parse_workspace_arg(command: &str) -> Option<PathBuf> {
    let mut parts = command.split_whitespace().peekable();
    while let Some(part) = parts.next() {
        if let Some(value) = part.strip_prefix("--workspace=") {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
        if part == "--workspace" {
            return parts.peek().map(PathBuf::from);
        }
    }
    None
}

fn classify_workspace_orphan(workspace: &Path, pid: Pid) -> Option<OrphanReason> {
    if !workspace.is_absolute() {
        return None;
    }
    if let Some(hint) = ephemeral_workspace_hint(workspace) {
        return Some(OrphanReason::EphemeralTempdirPattern { hint });
    }
    if !workspace.exists() {
        return Some(OrphanReason::WorkspacePathMissing);
    }
    let workspace_path = WorkspacePath::new(workspace.to_path_buf());
    let metadata = read_coordinator_metadata(&workspace_path);
    if metadata.is_some() && !coordinator_metadata_ok(metadata.as_ref(), pid) {
        return Some(OrphanReason::MetadataMismatch);
    }
    if pid_is_running(pid).ok() == Some(false) {
        return Some(OrphanReason::PidNotRunning);
    }
    None
}

fn classify_workspace_without_pid(workspace: &Path) -> Option<OrphanReason> {
    if !workspace.is_absolute() {
        return None;
    }
    if let Some(hint) = ephemeral_workspace_hint(workspace) {
        return Some(OrphanReason::EphemeralTempdirPattern { hint });
    }
    if !workspace.exists() {
        return Some(OrphanReason::WorkspacePathMissing);
    }
    None
}

fn ephemeral_workspace_hint(workspace: &Path) -> Option<String> {
    let text = workspace.to_string_lossy();
    let patterns = [
        "ta_doctor_comms_orphans-",
        "team-agent-watcher-dedupe",
    ];
    patterns
        .iter()
        .find(|pattern| text.contains(**pattern))
        .map(|pattern| (*pattern).to_string())
}

fn is_orphan_marker_workspace(workspace: &Path) -> bool {
    ephemeral_workspace_hint(workspace).is_some()
}

fn protected_pids() -> BTreeSet<u32> {
    let mut protected = BTreeSet::new();
    let current = std::process::id();
    protected.insert(current);
    let parents = ps_parent_map();
    let mut cursor = current;
    while let Some(parent) = parents.get(&cursor).copied() {
        if parent == 0 || !protected.insert(parent) {
            break;
        }
        cursor = parent;
    }
    protected
}

fn ps_parent_map() -> BTreeMap<u32, u32> {
    let output = match crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps").args(["-axo", "pid=,ppid="]),
        "ps_parent",
        None,
    )
    {
        Ok(output) if output.status.success() => output,
        _ => return BTreeMap::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse::<u32>().ok()?;
            let ppid = parts.next()?.parse::<u32>().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

fn orphan_values(orphans: &[OrphanRecord]) -> Vec<Value> {
    orphans
        .iter()
        .map(|orphan| orphan_value(orphan, orphan.action))
        .collect()
}

fn orphan_value(orphan: &OrphanRecord, action: &str) -> Value {
    let mut value = json!({
        "kind": orphan.kind,
        "reason": reason_key(&orphan.reason),
        "action": action,
    });
    if let Some(pid) = orphan.pid {
        value["pid"] = json!(pid.get());
    }
    if let Some(session) = &orphan.session {
        value["session"] = json!(session);
    }
    if let Some(socket) = &orphan.tmux_socket {
        value["tmux_socket"] = json!(socket);
    }
    if let Some(workspace) = &orphan.workspace {
        value["workspace"] = json!(workspace.to_string_lossy().to_string());
    }
    if let Some(command) = &orphan.command {
        value["command"] = json!(command);
    }
    if let OrphanReason::EphemeralTempdirPattern { hint } = &orphan.reason {
        value["hint"] = json!(hint);
    }
    value
}

fn kill_tmux_session(orphan: &OrphanRecord) -> bool {
    let (Some(socket), Some(session)) = (&orphan.tmux_socket, &orphan.session) else {
        return false;
    };
    TmuxBackend::for_socket_name(socket)
        .kill_session(&SessionName::new(session.clone()))
        .is_ok()
}

fn reason_key(reason: &OrphanReason) -> &'static str {
    match reason {
        OrphanReason::WorkspacePathMissing => "workspace_path_missing",
        OrphanReason::EphemeralTempdirPattern { .. } => "ephemeral_tempdir_pattern",
        OrphanReason::WorkspaceAlive => "workspace_alive",
        OrphanReason::CmdlineUnparsed => "cmdline_unparsed",
        OrphanReason::MetadataMismatch => "metadata_mismatch",
        OrphanReason::PidNotRunning => "pid_not_running",
    }
}
