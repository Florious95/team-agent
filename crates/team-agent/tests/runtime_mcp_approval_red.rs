//! #232 runtime MCP approval contracts.
//!
//! User-facing invariant: Team Agent may auto-approve its own low-risk MCP tools only when the
//! worker's effective approval mode mirrors a bypass/dangerous leader. Other tools and restricted
//! workers must remain blocked for the later awaiting_human_confirm notification slice.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::provider::{
    approval_choice_keys, choose_internal_mcp_approval_choice, extract_approval_prompt,
    ApprovalKind,
};

const ALLOWLISTED_MCP_TOOLS: [&str; 4] = [
    "send_message",
    "report_result",
    "get_team_status",
    "request_human",
];

#[test]
fn allowlist_mcp_tool_auto_approved_by_runtime_prompt_step() {
    for tool in ALLOWLISTED_MCP_TOOLS {
        let capture = approval_prompt_capture(tool);
        let prompt = extract_approval_prompt("worker_a", &capture)
            .expect("fixture should be a live MCP approval prompt");
        assert_eq!(prompt.kind, ApprovalKind::McpTool);
        assert_eq!(prompt.tool.as_deref(), Some(tool));
        let choice = choose_internal_mcp_approval_choice(&prompt);
        let keys = approval_choice_keys(&prompt, &capture, &choice);
        assert!(
            keys.iter().any(|key| key == "Enter"),
            "precondition: allowlisted MCP prompt must have an Enter-submittable choice; tool={tool} keys={keys:?}"
        );
    }

    let runtime_step = coordinator_runtime_prompt_step();
    let all_sources = source_tree("src");
    let mut failures = Vec::new();

    if !runtime_step.contains("handle_runtime_approval")
        && !runtime_step.contains("handle_runtime_prompts")
        && !runtime_step.contains("runtime_approval_prompt")
    {
        failures.push(
            "coordinator tick runtime_prompts step must call a runtime approval handler, not leave approvals as a comment"
                .to_string(),
        );
    }
    let allowlist_source = allowlist_source_region(&all_sources);
    if allowlist_source.is_empty() {
        failures.push(
            "runtime MCP approval needs a named allowlist constant/function, not ad hoc tool checks"
                .to_string(),
        );
    }
    for tool in ALLOWLISTED_MCP_TOOLS {
        if !allowlist_source.contains(tool) {
            failures.push(format!(
                "runtime MCP approval allowlist must name `{tool}` so Team Agent's own tool prompt can be auto-approved"
            ));
        }
    }
    for required in [
        "extract_approval_prompt",
        "choose_internal_mcp_approval_choice",
        "approval_choice_keys",
        "Key::Enter",
    ] {
        if !all_sources.contains(required) {
            failures.push(format!(
                "runtime approval auto-answer path must use `{required}` to choose and submit the approval"
            ));
        }
    }
    if !all_sources.contains("runtime_approval.auto_approved")
        && !all_sources.contains("mcp_tool_auto_approved")
    {
        failures.push(
            "auto-approved allowlisted MCP prompts must emit a durable event such as runtime_approval.auto_approved"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "allowlisted MCP approval must be auto-approved and disappear:\n{}",
        failures.join("\n")
    );
}

#[test]
fn non_allowlist_mcp_tool_is_not_auto_approved_and_is_reported() {
    let capture = approval_prompt_capture("assign_task");
    let prompt = extract_approval_prompt("worker_a", &capture)
        .expect("fixture should be a live MCP approval prompt");
    assert_eq!(prompt.kind, ApprovalKind::McpTool);
    assert_eq!(prompt.tool.as_deref(), Some("assign_task"));

    let all_sources = source_tree("src");
    let allowlist_source = allowlist_source_region(&all_sources);
    let mut failures = Vec::new();

    if allowlist_source.is_empty() {
        failures.push(
            "runtime MCP approval needs a named allowlist constant/function before non-allowlisted tools can be rejected"
                .to_string(),
        );
    }
    if allowlist_source.contains("assign_task") {
        failures.push(
            "assign_task must not be in the runtime MCP auto-approval allowlist; it requires human notification"
                .to_string(),
        );
    }
    if !all_sources.contains("tool_not_allowlisted") {
        failures.push(
            "non-allowlisted MCP tool prompts must write tool_not_allowlisted instead of pressing Enter"
                .to_string(),
        );
    }
    if !all_sources.contains("runtime_approval.blocked")
        && !all_sources.contains("awaiting_human_confirm")
        && !all_sources.contains("blocked_on_human")
    {
        failures.push(
            "non-allowlisted MCP tool prompts must remain blocked for the awaiting-human notification path"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "non-allowlisted MCP tool must not be auto-approved:\n{}",
        failures.join("\n")
    );
}

#[test]
fn leader_bypass_worker_runtime_approvals_mirror_auto_approve_scope() {
    let launch = source("src/lifecycle/launch.rs");
    let adapter = source("src/provider/adapter.rs");
    let all_sources = source_tree("src");
    let mut failures = Vec::new();

    if !launch.contains("effective_runtime_config") {
        failures.push(
            "worker approval mode must be derived through effective_runtime_config, not a separate runtime prompt policy"
                .to_string(),
        );
    }
    if !adapter.contains("dangerous_auto_approve")
        || !adapter.contains("--dangerously-bypass-approvals-and-sandbox")
    {
        failures.push(
            "Codex command-shape mirror must still expose dangerous_auto_approve/bypass argv for bypass leaders"
                .to_string(),
        );
    }
    if !all_sources.contains("DangerousApproval")
        || (!all_sources.contains("safety.enabled") && !all_sources.contains("effective_runtime_config"))
    {
        failures.push(
            "runtime MCP approval auto-answer must be gated by the same leader-derived safety state as worker argv"
                .to_string(),
        );
    }
    if !all_sources.contains("command_approval_auto_approved")
        && !all_sources.contains("runtime_approval.command_auto_approved")
        && !all_sources.contains("ApprovalKind::Command")
    {
        failures.push(
            "bypass leaders should also allow command approval prompts for mirrored workers; restricted leaders must not"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "leader bypass/dangerous mode must mirror into worker runtime approval handling:\n{}",
        failures.join("\n")
    );
}

#[test]
fn leader_restricted_worker_runtime_approvals_stay_blocked() {
    let runtime_step = coordinator_runtime_prompt_step();
    let all_sources = source_tree("src");
    let mut failures = Vec::new();

    if runtime_step.contains("choose_internal_mcp_approval_choice")
        && !runtime_step.contains("safety.enabled")
        && !runtime_step.contains("effective_runtime_config")
    {
        failures.push(
            "runtime approval step must not choose/press an approval without checking leader-derived safety"
                .to_string(),
        );
    }
    if !all_sources.contains("--ask-for-approval")
        || !all_sources.contains("on-request")
        || !all_sources.contains("--sandbox")
    {
        failures.push(
            "restricted worker command-shape must preserve sandbox/on-request approval rather than elevating"
                .to_string(),
        );
    }
    if !all_sources.contains("runtime_approval.blocked")
        && !all_sources.contains("awaiting_human_confirm")
        && !all_sources.contains("blocked_on_human")
    {
        failures.push(
            "restricted runtime approval prompts must remain blocked for #236 awaiting_human_confirm"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "leader restricted/non-bypass workers must not be auto-elevated:\n{}",
        failures.join("\n")
    );
}

fn approval_prompt_capture(tool: &str) -> String {
    format!(
        "Allow the team_orchestrator MCP server to run tool \"{tool}\"?\n  1. Allow\n  2. Deny\nEnter to submit | Esc to cancel\n"
    )
}

fn coordinator_runtime_prompt_step() -> String {
    let tick = source("src/coordinator/tick.rs");
    let start = tick
        .find("record_step(\"runtime_prompts\")")
        .expect("coordinator tick must keep runtime_prompts step");
    let after = &tick[start..];
    let end = after
        .find("record_step(\"sync_health\")")
        .expect("runtime_prompts step must precede sync_health");
    after[..end].to_string()
}

fn allowlist_source_region(all_sources: &str) -> String {
    for marker in [
        "RUNTIME_MCP_APPROVAL_ALLOWLIST",
        "INTERNAL_MCP_APPROVAL_ALLOWLIST",
        "MCP_APPROVAL_ALLOWLIST",
        "allowlisted_mcp",
    ] {
        if let Some(start) = all_sources.find(marker) {
            return all_sources[start..all_sources.len().min(start + 1200)].to_string();
        }
    }
    String::new()
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}

fn source_tree(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = String::new();
    append_rs_sources(&root, &mut out);
    out
}

fn append_rs_sources(path: &Path, out: &mut String) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            append_rs_sources(&entry, out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) == Some("rs") {
        out.push_str(&std::fs::read_to_string(path).expect("read source file"));
        out.push('\n');
    }
}
