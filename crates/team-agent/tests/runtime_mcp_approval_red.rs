//! #232 runtime MCP approval contracts.
//!
//! User-facing invariant: Team Agent may auto-approve its own low-risk MCP tools only when the
//! worker's effective approval mode mirrors a bypass/dangerous leader. Other tools and restricted
//! workers must remain blocked for the later awaiting_human_confirm notification slice.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::provider::{
    approval_choice_keys, awaiting_human_confirm_reason, choose_internal_mcp_approval_choice,
    extract_approval_prompt, get_adapter, runtime_approval_decision, ApprovalKind, AuthMode,
    Provider, RuntimeApprovalDecision, SessionId,
};

const ALLOWLISTED_MCP_TOOLS: [&str; 4] = [
    "send_message",
    "report_result",
    "get_team_status",
    "request_human",
];
const CLAUDE_DANGEROUS: &str = "--dangerously-skip-permissions";
const CLAUDE_PERMISSION_MODE: &str = "--permission-mode";
const CLAUDE_PERMISSION_DEFAULT: &str = "default";

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
    if !all_sources.contains("runtime_approval.command_approval_requires_human") {
        failures.push(
            "C5: command approval must always require human confirmation, even under dangerous policy"
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
fn command_approval_is_never_auto_approved_even_under_dangerous_policy() {
    let capture = command_approval_prompt_capture();
    let prompt = extract_approval_prompt("worker_a", &capture)
        .expect("fixture should be a live command approval prompt");
    assert_eq!(prompt.kind, ApprovalKind::Command);
    assert_eq!(prompt.command.as_deref(), Some("Bash(rm -rf /tmp/team-agent-danger)"));

    assert_eq!(
        awaiting_human_confirm_reason(&prompt, true),
        Some("command_approval_requires_human"),
        "C5: command approval is OS-impacting and must never enter the auto-approval scope, even when effective_approval_policy.enabled=true"
    );
    assert_eq!(
        runtime_approval_decision(&prompt, true),
        RuntimeApprovalDecision::AwaitingHumanConfirm,
        "C5: command approval must stay on the awaiting-human path under dangerous policy"
    );
}

#[test]
fn claude_argv_three_state_and_mutual_exclusion_cover_launch_resume_and_fork() {
    let source_session = SessionId::new("11111111-2222-4333-8444-555555555555");
    let mut failures = Vec::new();

    for provider in [Provider::Claude, Provider::ClaudeCode] {
        let adapter = get_adapter(provider);
        let disabled_launch = adapter
            .build_command_with_tools(AuthMode::Subscription, None, Some("Worker"), Some("claude-sonnet-4-6"), &[])
            .expect("disabled Claude launch argv");
        failures.extend(claude_default_failures(&disabled_launch, &format!("{provider:?} disabled fresh launch")));

        let dangerous_launch = adapter
            .build_command_with_tools(
                AuthMode::Subscription,
                None,
                Some("Worker"),
                Some("claude-sonnet-4-6"),
                &["mcp_team", "dangerous_auto_approve"],
            )
            .expect("dangerous Claude launch argv");
        failures.extend(claude_dangerous_failures(
            &dangerous_launch,
            &format!("{provider:?} runtime_config --yes fresh launch"),
        ));

        let dangerous_resume = adapter
            .build_resume_command_with_context(
                Some(&source_session),
                AuthMode::Subscription,
                None,
                Some("Worker"),
                Some("claude-sonnet-4-6"),
                &["dangerous_auto_approve"],
            )
            .expect("dangerous Claude resume argv");
        failures.extend(claude_dangerous_failures(
            &dangerous_resume,
            &format!("{provider:?} dangerous restart/resume"),
        ));

        let dangerous_fork = adapter
            .fork_with_context(
                Some(&source_session),
                AuthMode::Subscription,
                None,
                Some("Worker"),
                Some("claude-sonnet-4-6"),
                &["dangerous_auto_approve"],
            )
            .expect("dangerous Claude fork argv");
        failures.extend(claude_dangerous_failures(
            &dangerous_fork,
            &format!("{provider:?} dangerous fork"),
        ));
    }

    assert!(
        failures.is_empty(),
        "C14-C16: Claude argv must mirror Codex approval parity across launch/restart/start-agent/add-agent/fork command construction:\n{}",
        failures.join("\n")
    );
}

#[test]
fn running_agent_state_persists_effective_policy_schema_and_single_helper_across_spawn_paths() {
    let launch = source("src/lifecycle/launch.rs");
    let restart_common = source("src/lifecycle/restart/common.rs");
    let restart_agent = source("src/lifecycle/restart/agent.rs");
    let all = format!("{launch}\n{restart_common}\n{restart_agent}");
    let mut failures = Vec::new();

    for field in [
        "effective_approval_policy",
        "enabled",
        "source",
        "inherited",
        "explicit_yes_confirmed",
        "provider",
        "flag",
        "worker_capability_above_leader",
    ] {
        if !all.contains(field) {
            failures.push(format!(
                "C7/C12: running_agent_state must persist effective_approval_policy.{field}"
            ));
        }
    }

    let helper_markers = [
        "persist_effective_approval_policy",
        "effective_approval_policy_for_agent",
        "write_effective_approval_policy",
        "agent_state_with_effective_approval_policy",
    ];
    let helper = helper_markers
        .iter()
        .find(|marker| all.contains(**marker))
        .copied();
    let Some(helper) = helper else {
        failures.push(
            "C10: spawn-time effective_approval_policy must be written by one named helper shared by launch/restart/start-agent/add-agent/fork"
                .to_string(),
        );
        assert!(
            failures.is_empty(),
            "effective approval policy persistence contract failed:\n{}",
            failures.join("\n")
        );
        return;
    };

    for (label, section) in [
        ("fresh launch", source_section(&launch, "fn persist_spawn_agent_state", "fn launch_report")),
        ("restart", source_section(&restart_common, "pub(super) fn spawn_agent_window", "fn claude_session_spawn_cwd")),
        ("start-agent", source_section(&restart_agent, "pub(crate) fn start_agent_at_paths", "fn write_start_agent_noop_event")),
        ("add-agent", source_section(&launch, "fn add_agent_with_transport_at_paths", "fn materialize_added_role_file")),
        ("fork-agent", source_section(&launch, "pub fn fork_agent", "fn rollback_fork_after_spawn")),
    ] {
        if !section.contains(helper) {
            failures.push(format!(
                "C10: {label} path must call the shared {helper} helper so all spawn paths persist the same effective_approval_policy shape"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "effective approval policy persistence contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn coordinator_auto_approval_reads_state_not_process_ancestry_and_emits_audit_payload() {
    let tick = source("src/coordinator/tick.rs");
    let handler = source_section(
        &tick,
        "fn handle_runtime_approval_prompts",
        "fn coordinator_status",
    );
    let auto_event = source_section(&handler, "\"runtime_approval.auto_approved\"", "RuntimeApprovalDecision::AwaitingHumanConfirm");
    let mut failures = Vec::new();

    if handler.contains("detect_dangerous_approval") || tick.contains("fn runtime_approval_auto_answer_allowed()") {
        failures.push(
            "C8/C11: coordinator runtime approval must read per-agent effective_approval_policy from state, not recompute dangerous approval from coordinator process ancestry"
                .to_string(),
        );
    }
    if !handler.contains("effective_approval_policy") {
        failures.push(
            "C8/C11: handle_runtime_approval_prompts must use state.agents[*].effective_approval_policy as the single approval truth source"
                .to_string(),
        );
    }
    for field in [
        "policy_source",
        "inherited",
        "explicit_yes_confirmed",
        "worker_capability_above_leader",
    ] {
        if !auto_event.contains(field) {
            failures.push(format!(
                "C9/C12: runtime_approval.auto_approved event must include {field} from persisted policy; event_source={auto_event}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "coordinator persisted-policy approval contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn worker_mcp_rpc_arguments_cannot_widen_team_scope_or_bypass_owner_gate() {
    let wire = source("src/mcp_server/wire.rs");
    let tools = source("src/mcp_server/tools.rs");
    let send_schema = source_section(&wire, "McpTool::SendMessage =>", "McpTool::AssignTask =>");
    let send_dispatch = source_section(&wire, "McpTool::SendMessage =>", "McpTool::ReportResult =>");
    let mut failures = Vec::new();

    for forbidden in ["\"scope\"", "\"team\"", "args.get(\"scope\")", "args.get(\"team\")"] {
        if send_schema.contains(forbidden) || send_dispatch.contains(forbidden) {
            failures.push(format!(
                "C6: worker MCP send_message must not expose or honor per-call {forbidden}; scope is spawn-time TEAM_AGENT_OWNER_TEAM_ID. Public shape: .team/artifacts/232-scope-contract-shape.md"
            ));
        }
    }
    if !tools.contains("canonicalize_owner_team_id") {
        failures.push(
            "C6/C13: MCP worker tools must canonicalize spawn-time TEAM_AGENT_OWNER_TEAM_ID before accepting/rejecting calls; missing canonicalize_owner_team_id"
                .to_string(),
        );
    }
    if !tools.contains("mcp.scope_refused") {
        failures.push(
            "C6/C13: foreign team/workspace RPC override must return/emit mcp.scope_refused; missing observable mcp.scope_refused refusal"
                .to_string(),
        );
    }
    for required_payload in ["owner_team_id", "requested_team", "requested_scope"] {
        if !tools.contains(required_payload) {
            failures.push(format!(
                "C6/C13: mcp.scope_refused payload must include {required_payload} so --nocapture shows spawn owner vs requested override"
            ));
        }
    }
    if !tools.contains("messaging::send_message") {
        failures.push(
            "C6/C13: MCP worker-recipient route must delegate to messaging::send_message so CLI/MCP owner gates stay identical; direct MessageStore writes bypass owner gates"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "worker MCP scope ceiling contract failed:\n{}",
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

fn command_approval_prompt_capture() -> String {
    "Bash(rm -rf /tmp/team-agent-danger)\nWould you like to run the following command?\n  1. Yes\n  2. No\nEnter to submit | Esc to cancel\n".to_string()
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

fn source_section(source: &str, start: &str, end: &str) -> String {
    let Some(start_idx) = source.find(start) else {
        return String::new();
    };
    let tail = &source[start_idx..];
    let end_idx = tail.find(end).unwrap_or(tail.len());
    tail[..end_idx].to_string()
}

fn has_adjacent(argv: &[String], needle: &[&str]) -> bool {
    argv.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn claude_dangerous_failures(argv: &[String], label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if !argv.iter().any(|arg| arg == CLAUDE_DANGEROUS) {
        failures.push(format!(
            "{label}: Claude dangerous worker argv must contain {CLAUDE_DANGEROUS}; argv={argv:?}"
        ));
    }
    if has_adjacent(argv, &[CLAUDE_PERMISSION_MODE, CLAUDE_PERMISSION_DEFAULT]) {
        failures.push(format!(
            "{label}: Claude dangerous worker argv must not also contain --permission-mode default; argv={argv:?}"
        ));
    }
    failures
}

fn claude_default_failures(argv: &[String], label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if !has_adjacent(argv, &[CLAUDE_PERMISSION_MODE, CLAUDE_PERMISSION_DEFAULT]) {
        failures.push(format!(
            "{label}: Claude disabled/restricted worker argv must contain --permission-mode default; argv={argv:?}"
        ));
    }
    if argv.iter().any(|arg| arg == CLAUDE_DANGEROUS) {
        failures.push(format!(
            "{label}: Claude disabled/restricted worker argv must not contain {CLAUDE_DANGEROUS}; argv={argv:?}"
        ));
    }
    failures
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
