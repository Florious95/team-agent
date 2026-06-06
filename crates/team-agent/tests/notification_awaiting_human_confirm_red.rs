//! #236 awaiting-human-confirm notification contracts.
//!
//! User-facing invariant: a live worker with a structural approval prompt that Team Agent must not
//! answer gets exactly one leader notification per (team, agent, fingerprint). The notification path
//! never presses keys.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::provider::{extract_approval_prompt, runtime_mcp_tool_allowlisted, ApprovalKind};

#[test]
fn awaiting_human_confirm_uses_active_tail_structural_prompt_and_dedupes_per_fingerprint() {
    let assign_task_prompt = concat!(
        "Allow the team_orchestrator MCP server to run tool \"assign_task\"?\n",
        "  1. Allow\n",
        "  2. Deny\n",
        "Enter to submit | Esc to cancel\n"
    );
    let prompt = extract_approval_prompt("worker_a", assign_task_prompt)
        .expect("fixture precondition: active-tail MCP approval prompt");
    assert_eq!(prompt.kind, ApprovalKind::McpTool);
    assert_eq!(prompt.tool.as_deref(), Some("assign_task"));
    assert!(
        !runtime_mcp_tool_allowlisted("assign_task"),
        "fixture precondition: assign_task is not an auto-approved Team Agent MCP tool"
    );
    assert!(
        extract_approval_prompt(
            "worker_a",
            "Allow the team_orchestrator MCP server to run tool \"assign_task\"?\n  1. Allow\nEnter to submit | Esc to cancel\nlater output\n"
        )
        .is_none(),
        "active-tail parser must reject stale prompts with non-empty output after the control line"
    );

    let src = production_sources();
    let mut failures = Vec::new();

    if !src.contains("worker.awaiting_human_confirm") {
        failures.push("must emit worker.awaiting_human_confirm for blocked structural approval prompts".to_string());
    }
    if !src.contains("extract_approval_prompt") || !src.contains("active_approval_control_index") {
        failures.push(
            "notification trigger must reuse provider/approvals/parsing.rs active-tail structural parser"
                .to_string(),
        );
    }
    if !src.contains("runtime_mcp_tool_allowlisted") || !src.contains("tool_not_allowlisted") {
        failures.push(
            "non-allowlisted MCP tools must be classified as notify(reason=tool_not_allowlisted)"
                .to_string(),
        );
    }
    if !src.contains("ApprovalFingerprint")
        || !src.contains("fingerprint")
        || !src.contains("awaiting_human_confirm_seen")
    {
        failures.push(
            "same prompt across ticks must dedupe by (team, agent_id, fingerprint)"
                .to_string(),
        );
    }
    if !src.contains("send_to_leader_receiver") || !src.contains("deliver_to_leader.submit") {
        failures.push(
            "awaiting_human_confirm must notify through the shared N32 leader funnel"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "awaiting_human_confirm deterministic notification contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn awaiting_human_confirm_path_never_presses_keys_or_auto_elevates_restricted_workers() {
    let src = production_sources();
    let awaiting_region = region_around(&src, "worker.awaiting_human_confirm", 2000);
    let mut failures = Vec::new();

    if awaiting_region.contains("approval_choice_keys")
        || awaiting_region.contains("choose_internal_mcp_approval_choice")
        || awaiting_region.contains("Key::Enter")
        || awaiting_region.contains("inject_keys")
    {
        failures.push(
            "awaiting_human_confirm is a notification path; it must not submit approval keys"
                .to_string(),
        );
    }
    if !src.contains("leader_restricted")
        && !src.contains("DangerousApproval")
        && !src.contains("effective_runtime_config")
    {
        failures.push(
            "command/tool approval notification must be gated by leader-derived approval safety, not worker self-elevation"
                .to_string(),
        );
    }
    if !src.contains("prompt_kind") || !src.contains("next_step") {
        failures.push(
            "leader notification payload must include prompt_kind and next_step fields for operator action"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "awaiting_human_confirm must stay notification-only:\n{}",
        failures.join("\n")
    );
}

fn production_sources() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = String::new();
    append_rs_sources(&root, &mut out);
    out
}

fn region_around(source: &str, needle: &str, len: usize) -> String {
    let Some(start) = source.find(needle) else {
        return String::new();
    };
    source[start..source.len().min(start + len)].to_string()
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
