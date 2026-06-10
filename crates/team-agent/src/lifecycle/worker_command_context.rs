use std::path::Path;

use crate::lifecycle::types::{DangerousApproval, LifecycleError};
use crate::model::enums::{Enforcement, Provider};
use crate::model::ids::AgentId;
use crate::model::permissions::{resolve_permissions, AgentPermissionInput};

const RUNTIME_CONTRACT_SECTION: &str = r#"# Team Agent Teammate Runtime Contract

You are a teammate in a Team Agent runtime, not the user's primary assistant.
The user normally talks to the team lead. Plain text you write in this worker
session is local to this session and is not a team message.

Use Team Agent MCP tools for team-visible coordination:
- Send progress, blockers, permission needs, tool failures, scope changes, and
  long-running status updates with team_orchestrator.send_message(to='leader',
  content='<short message>').
- Send to another teammate by agent id when coordination is useful, or use
  to='*' to notify every other team member. The runtime resolves only this team
  and excludes your own worker.
- When the task is complete, call team_orchestrator.report_result exactly once.
- Do not pass sender, task_id, agent_id, schema_version, or ack fields unless
  doing a low-level compatibility diagnostic. The MCP runtime fills protocol
  fields from the current worker and task state.

If you are blocked or cannot continue, message the leader promptly instead of
waiting silently. If work takes several minutes, send a short progress update.

When any Team Agent worker hits a 500/529/rate-limit/overloaded API error,
slow the team down before retrying: wait 1-2 minutes, keep active workers low,
and avoid blind immediate retries."#;

const RESULT_ENVELOPE_OUTPUT_CONTRACT: &str =
    "For progress or blockers, call team_orchestrator.send_message(to='leader', content='<short message>'); \
for teammate coordination, send to another agent id or to='*' for every other team member. \
do not pass sender, task_id, or requires_ack because the MCP runtime fills protocol fields. \
the runtime injects it into the attached Codex leader pane when the leader has run attach-leader. \
If no leader is attached, the tool returns a fallback/failed result instead of completion. \
Final completion must call team_orchestrator.report_result exactly once with a short summary \
and optional status/changes/tests; MCP fills schema_version, task_id, and agent_id.";

pub(crate) struct WorkerCommandAgent {
    id: Option<String>,
    provider: Provider,
    role: Option<String>,
    declared_tools: Option<Vec<String>>,
    system_prompt_inline: Option<String>,
    system_prompt_file: Option<String>,
    output_contract_format: Option<String>,
}

impl WorkerCommandAgent {
    pub(crate) fn from_yaml(
        agent: &crate::model::yaml::Value,
        fallback_id: Option<&str>,
        provider: Provider,
    ) -> Self {
        let system_prompt = agent.get("system_prompt");
        Self {
            id: agent
                .get("id")
                .and_then(crate::model::yaml::Value::as_str)
                .or(fallback_id)
                .map(str::to_string),
            provider,
            role: agent
                .get("role")
                .and_then(crate::model::yaml::Value::as_str)
                .map(str::to_string),
            declared_tools: agent
                .get("tools")
                .and_then(crate::model::yaml::Value::as_list)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(crate::model::yaml::Value::as_str)
                        .map(str::to_string)
                        .collect()
                }),
            system_prompt_inline: system_prompt
                .and_then(|prompt| prompt.get("inline"))
                .and_then(crate::model::yaml::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            system_prompt_file: system_prompt
                .and_then(|prompt| prompt.get("file"))
                .filter(|value| value.is_truthy())
                .and_then(crate::model::yaml::Value::as_str)
                .map(str::to_string),
            output_contract_format: agent
                .get("output_contract")
                .and_then(|contract| contract.get("format"))
                .and_then(crate::model::yaml::Value::as_str)
                .map(str::to_string),
        }
    }

    pub(crate) fn from_json(
        agent: &serde_json::Value,
        fallback_id: Option<&str>,
        provider: Provider,
    ) -> Self {
        let system_prompt = agent.get("system_prompt");
        Self {
            id: agent
                .get("id")
                .and_then(serde_json::Value::as_str)
                .or(fallback_id)
                .map(str::to_string),
            provider,
            role: agent
                .get("role")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            declared_tools: agent
                .get("tools")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                }),
            system_prompt_inline: system_prompt
                .and_then(|prompt| prompt.get("inline"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            system_prompt_file: system_prompt
                .and_then(|prompt| prompt.get("file"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            output_contract_format: agent
                .get("output_contract")
                .and_then(|contract| contract.get("format"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }
}

pub(crate) fn compile_worker_system_prompt(
    agent: &WorkerCommandAgent,
) -> Result<String, LifecycleError> {
    // Python prompt.py:39 — chunks = [identity, TEAMMATE_SYSTEM_PROMPT, ...]: the worker
    // identity line anchors the very first section (live Python worker argv confirms).
    // C-1 cr verdict / B2 灵魂件 — identity 必须 FIRST(MUST-4 行为层守:空白上下文问
    // "你是谁"必须先答 Team Agent worker 身份)。runtime contract 跟后。
    let mut chunks = vec![
        identity_section(agent),
        runtime_contract_section(),
        role_body(agent)?,
    ];
    if let Some(contract) = output_contract(agent) {
        chunks.push(contract);
    }
    if let Some(notes) = permission_notes(agent)? {
        chunks.push(notes);
    }
    Ok(chunks
        .into_iter()
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

pub(crate) fn resolved_tool_strings_for_command(
    agent: &WorkerCommandAgent,
    provider: Provider,
    safety: &DangerousApproval,
) -> Result<Vec<String>, LifecycleError> {
    let mut tools: Vec<String> = resolve_agent_permissions(agent, provider)?
        .sorted_tool_strings()
        .into_iter()
        .map(str::to_string)
        .collect();
    if safety.enabled && !tools.iter().any(|tool| tool == "dangerous_auto_approve") {
        tools.push("dangerous_auto_approve".to_string());
    }
    Ok(tools)
}

fn resolve_agent_permissions(
    agent: &WorkerCommandAgent,
    provider: Provider,
) -> Result<crate::model::permissions::ResolvedPermissions, LifecycleError> {
    resolve_permissions(&AgentPermissionInput {
        id: agent.id.as_deref().map(AgentId::new),
        provider,
        role: agent.role.clone(),
        tools: agent.declared_tools.clone(),
    })
    .map_err(|e| LifecycleError::Compile(e.to_string()))
}

fn runtime_contract_section() -> String {
    RUNTIME_CONTRACT_SECTION.to_string()
}

fn identity_section(agent: &WorkerCommandAgent) -> String {
    format!(
        "You are Team Agent worker `{}` with role `{}`. When asked about your role or identity, answer with this Team Agent worker identity first, not only the generic provider product identity.",
        agent.id.as_deref().unwrap_or("unknown"),
        agent.role.as_deref().unwrap_or("developer")
    )
}

fn role_body(agent: &WorkerCommandAgent) -> Result<String, LifecycleError> {
    let mut chunks = Vec::new();
    if let Some(inline) = &agent.system_prompt_inline {
        chunks.push(inline.clone());
    }
    if let Some(path) = &agent.system_prompt_file {
        let body = std::fs::read_to_string(Path::new(path))
            .map_err(|e| LifecycleError::Compile(format!("read system_prompt.file {path}: {e}")))?;
        if !body.is_empty() {
            chunks.push(body);
        }
    }
    Ok(chunks.join("\n\n"))
}

fn output_contract(agent: &WorkerCommandAgent) -> Option<String> {
    (agent.output_contract_format.as_deref() == Some("result_envelope_v1"))
        .then(|| RESULT_ENVELOPE_OUTPUT_CONTRACT.to_string())
}

fn permission_notes(agent: &WorkerCommandAgent) -> Result<Option<String>, LifecycleError> {
    let permissions = resolve_agent_permissions(agent, agent.provider)?;
    // C-2-1/C-2-2 cr verdict — Copilot 一期 framework 不替决 fs_read/fs_list/git_diff/
    // provider_builtin(provider prompt 控);为诚实(MUST-NOT-13)在 system prompt 内
    // 总是声明这些 provider-level prompt_only 工具,即便角色未显式声明。
    let provider_prompt_only_extras = provider_default_prompt_only_tools(agent.provider);
    let mut prompt_only: std::collections::BTreeSet<String> = permissions
        .resolved_tools
        .iter()
        .filter(|tool| tool.enforcement == Enforcement::PromptOnly)
        .filter_map(|tool| serde_json::to_value(tool.tool).ok())
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    for tool in provider_prompt_only_extras {
        prompt_only.insert((*tool).to_string());
    }
    if prompt_only.is_empty() {
        return Ok(None);
    }
    let prompt_only: Vec<String> = prompt_only.into_iter().collect();
    Ok(Some(format!(
        "Permission note: these tools are prompt-only for this provider and not hard-enforced: {}",
        prompt_only.join(", ")
    )))
}

/// C-2-1 cr verdict — provider-level prompt_only tools that the framework cannot
/// hard-enforce. The system prompt declares them so the worker is honest about
/// where consent gates actually live (provider prompt vs framework).
fn provider_default_prompt_only_tools(provider: Provider) -> &'static [&'static str] {
    match provider {
        // C-2-1: fs_read / fs_list / git_diff / provider_builtin 由 provider prompt
        // 控制,framework 不替决(prompt_only 诚实声明)。
        Provider::Copilot => &["fs_list", "fs_read", "git_diff", "provider_builtin"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::types::DangerousApprovalSource;

    fn disabled_safety() -> DangerousApproval {
        DangerousApproval {
            enabled: false,
            source: DangerousApprovalSource::Disabled,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: false,
            ancestry_binary_name: None,
            unexpected_binary: false,
        }
    }

    #[test]
    fn empty_tools_use_role_defaults_and_aliases_resolve_before_command() {
        let agent = WorkerCommandAgent {
            id: Some("dev".to_string()),
            provider: Provider::ClaudeCode,
            role: Some("developer".to_string()),
            declared_tools: Some(Vec::new()),
            system_prompt_inline: None,
            system_prompt_file: None,
            output_contract_format: None,
        };
        let tools =
            resolved_tool_strings_for_command(&agent, Provider::ClaudeCode, &disabled_safety())
                .unwrap();
        assert_eq!(
            tools,
            [
                "execute_bash",
                "fs_list",
                "fs_read",
                "fs_write",
                "git_diff",
                "mcp_team",
                "provider_builtin"
            ]
        );

        let agent = WorkerCommandAgent {
            declared_tools: Some(vec!["fs_*".to_string(), "@team-orchestrator".to_string()]),
            ..agent
        };
        let tools =
            resolved_tool_strings_for_command(&agent, Provider::ClaudeCode, &disabled_safety())
                .unwrap();
        assert_eq!(tools, ["fs_list", "fs_read", "fs_write", "mcp_team"]);
    }

    #[test]
    fn system_prompt_uses_identity_then_runtime_contract_python_order() {
        // #264 D6: Python truth source (prompt.py:39, live ps confirmed) builds
        // chunks = [identity, TEAMMATE_SYSTEM_PROMPT, role_body, output, permissions].
        // The previous assertion locked the inverted contract-first order with no
        // Python evidence; this is the corrected golden.
        // 0.3.5 union (copilot v2 C-1 / B2 MUST-4 行为层守): identity 必须 FIRST —
        // 空白上下文问"你是谁"的第一行答案必须先答 Team Agent worker 身份;
        // Copilot 适配的 C-1-3 行为层要求与其它 provider 同步。
        let agent = WorkerCommandAgent {
            id: Some("coder".to_string()),
            provider: Provider::Codex,
            role: Some("Runtime Developer".to_string()),
            declared_tools: Some(vec!["mcp_team".to_string()]),
            system_prompt_inline: Some("Implement the assigned slice.".to_string()),
            system_prompt_file: None,
            output_contract_format: Some("result_envelope_v1".to_string()),
        };
        let prompt = compile_worker_system_prompt(&agent).unwrap();
        assert!(
            prompt.starts_with("You are Team Agent worker `coder` with role `Runtime Developer`."),
            "compiled prompt must start with the identity section (Python prompt.py:39); head={:?}",
            prompt.chars().take(120).collect::<String>()
        );
        let identity = prompt.find("worker `coder`").unwrap();
        let runtime = prompt
            .find(RUNTIME_CONTRACT_SECTION.lines().next().unwrap_or(""))
            .unwrap();
        let role = prompt.find("Implement the assigned slice.").unwrap();
        let output = prompt
            .find("Final completion must call team_orchestrator.report_result exactly once")
            .unwrap();
        let permissions = prompt.find("Permission note:").unwrap();
        assert!(identity < runtime && runtime < role && role < output && output < permissions);
        let slowdown_phrase = format!("500/{}", 500 + 29);
        assert!(prompt.contains(&slowdown_phrase));
        assert!(prompt.contains("Runtime Developer"));
    }
}
