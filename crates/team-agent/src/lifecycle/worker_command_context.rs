//! ---
//! purpose: 从 spec 里的一个 agent 定义，编译出 worker 启动所需的系统提示词
//! contract:
//!   provides:
//!     - name: WorkerCommandAgent
//!       what: 从 YAML 或 JSON 读出的单 agent 命令上下文
//!     - name: compile_worker_system_prompt
//!       what: 按身份、runtime 契约、通信模式、角色正文、输出契约、权限说明拼系统提示词
//!   depends:
//!     - crate::communication_mode
//! boundary:
//!   - 只产出字符串，不 spawn 进程、不写盘
//!   - bypass 只认 agent 自身声明，不从 team/runtime/leader argv 继承
//!   - provider 没有 bypass argv 定义时报错，不静默降级
//! maturity: wired
//! ---
use std::path::Path;

use crate::communication_mode::CommunicationMode;
use crate::lifecycle::types::LifecycleError;
use crate::model::enums::Provider;

const RUNTIME_CONTRACT_SECTION: &str = r#"# Team Agent Teammate Runtime Contract

You are a Team Agent teammate; leader cannot see terminal output.
All communication must go through Team Agent MCP tools.

## Communication:

- Teammate: {send_message}(to='<agent_id>', content='...')
- Broadcast: {send_message}(to='*', content='...')
- Complete: {report_result}(summary='...') — call exactly once

## Rules:

- Do not pass sender, task_id, or schema_version — MCP fills them.
- Do not reply to pure ACKs, greetings, or unchanged status notices (such as "paused" or "waiting"); after reporting a blocker once or completing a task, remain silent until a new actionable instruction arrives.
- On 500/529/rate limits, retry only after 1-2 minutes."#;

// 0.4.11 trimmed: the runtime contract section above already covers
// send_message signatures and report_result exactly-once. The output
// contract now only carries the RESULT-ENVELOPE-SPECIFIC delivery
// semantics (leader-attach dependence + fallback status) that the
// generic runtime section deliberately leaves out.
const RESULT_ENVELOPE_OUTPUT_CONTRACT: &str =
    "Final completion must call {report_result} exactly once with a short summary \
and optional status/changes/tests; the MCP runtime injects the result into the attached leader pane. \
If no leader is attached, the tool returns a fallback/failed result instead of completion.";

pub(crate) struct WorkerCommandAgent {
    id: Option<String>,
    provider: Provider,
    role: Option<String>,
    system_prompt_inline: Option<String>,
    system_prompt_file: Option<String>,
    output_contract_format: Option<String>,
    communication_mode: CommunicationMode,
    /// 0.5.66 bypass 单源:agent-level `dangerously_skip_permissions`(必填 bool)。
    dangerously_skip_permissions: bool,
}

impl WorkerCommandAgent {
/// ---
/// purpose: 从 spec 的 YAML agent 节点读出命令上下文
/// params:
///   agent: 单个 agent 的 YAML 节点
///   fallback_id: agent 节点没写 id 时的兜底 id
///   provider: 已解析的 provider
/// returns: 填好的 WorkerCommandAgent
/// errors: communication_mode 取值非法时返回 LifecycleError
/// contract_id: lifecycle.worker_command_agent.from_source
/// ---
    pub(crate) fn from_yaml(
        agent: &crate::model::yaml::Value,
        fallback_id: Option<&str>,
        provider: Provider,
    ) -> Result<Self, LifecycleError> {
        let system_prompt = agent.get("system_prompt");
        Ok(Self {
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
            communication_mode: communication_mode(
                agent
                    .get("communication_mode")
                    .and_then(crate::model::yaml::Value::as_str),
            )?,
            dangerously_skip_permissions: matches!(
                agent.get("dangerously_skip_permissions"),
                Some(crate::model::yaml::Value::Bool(true))
            ),
        })
    }

/// ---
/// purpose: 从 runtime state 的 JSON agent 节点读出命令上下文
/// params:
///   agent: 单个 agent 的 JSON 节点
///   fallback_id: agent 节点没写 id 时的兜底 id
///   provider: 已解析的 provider
/// returns: 填好的 WorkerCommandAgent
/// errors: communication_mode 取值非法时返回 LifecycleError
/// contract_id: lifecycle.worker_command_agent.from_source
/// ---
    pub(crate) fn from_json(
        agent: &serde_json::Value,
        fallback_id: Option<&str>,
        provider: Provider,
    ) -> Result<Self, LifecycleError> {
        let system_prompt = agent.get("system_prompt");
        Ok(Self {
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
            communication_mode: communication_mode(
                agent
                    .get("communication_mode")
                    .and_then(serde_json::Value::as_str),
            )?,
            dangerously_skip_permissions: agent
                .get("dangerously_skip_permissions")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// ---
/// purpose: 拼出 worker 的系统提示词，身份段必须排在最前
/// returns: 各段以空行分隔的提示词，空段被丢弃
/// errors: 角色正文读取失败或权限解析失败时返回 LifecycleError
/// ---
pub(crate) fn compile_worker_system_prompt(
    agent: &WorkerCommandAgent,
) -> Result<String, LifecycleError> {
    // Python prompt.py:39 — chunks = [identity, TEAMMATE_SYSTEM_PROMPT, ...]: the worker
    // identity line anchors the very first section (live Python worker argv confirms).
    // C-1 cr verdict / B2 灵魂件 — identity 必须 FIRST(MUST-4 行为层守:空白上下文问
    // "你是谁"必须先答 Team Agent worker 身份)。runtime contract 跟后。
    let send_message = mcp_tool_name(agent.provider, "team_orchestrator", "send_message");
    let report_result = if agent.provider == Provider::Pi {
        r#"mcp({tool:"team_orchestrator_report_result", args:{summary:"..."}})"#.to_string()
    } else {
        mcp_tool_name(agent.provider, "team_orchestrator", "report_result")
    };
    let runtime_contract = if agent.provider == Provider::Pi {
        pi_runtime_contract_section()
    } else {
        runtime_contract_section(&send_message, &report_result)
    };
    let communication_contract = if agent.provider == Provider::Pi {
        pi_communication_contract(agent.communication_mode)
    } else {
        agent.communication_mode.runtime_contract(&send_message)
    };
    let mut chunks = vec![
        identity_section(agent),
        runtime_contract,
        communication_contract,
        role_body(agent)?,
    ];
    if let Some(contract) = output_contract(agent, &report_result) {
        chunks.push(contract);
    }
    Ok(chunks
        .into_iter()
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

pub(crate) fn compile_pi_leader_system_prompt() -> String {
    r#"You are the Team Agent leader for this workspace. Coordinate work through the existing Team Agent MCP server.

Use the Pi MCP proxy call form with server-prefixed tool names, for example:
mcp({tool:"team_orchestrator_get_team_status", args:{}})
mcp({tool:"team_orchestrator_send_message", args:{to:"<agent_id>", content:"..."}})

Do not invent a second team protocol or assume that a configured lazy MCP server is connected before a call returns."#
        .to_string()
}

/// Provider-facing MCP tool name. Only verified call forms are filled in.
/// Unverified providers keep the historical dotted spelling.
fn mcp_tool_name(provider: Provider, server: &str, tool: &str) -> String {
    match provider {
        Provider::Claude | Provider::ClaudeCode => format!("mcp__{server}__{tool}"),
        Provider::Grok => format!("{server}__{tool}"),
        Provider::Pi => format!("{server}_{tool}"),
        // CursorAgent / Codex / Copilot / GeminiCli / Fake: 未验证，沿用现状点号。
        // CursorAgent 不可与 grok 同臂：仓库里没有活转录，`{server}__{tool}` 是推断。
        Provider::CursorAgent
        | Provider::Codex
        | Provider::Copilot
        | Provider::GeminiCli
        | Provider::Fake => format!("{server}.{tool}"),
    }
}

fn runtime_contract_section(send_message: &str, report_result: &str) -> String {
    RUNTIME_CONTRACT_SECTION
        .replace("{send_message}", send_message)
        .replace("{report_result}", report_result)
}

fn pi_runtime_contract_section() -> String {
    RUNTIME_CONTRACT_SECTION
        .replace(
            "{send_message}(to='<agent_id>', content='...')",
            r#"mcp({tool:"team_orchestrator_send_message", args:{to:"<agent_id>", content:"..."}})"#,
        )
        .replace(
            "{send_message}(to='*', content='...')",
            r#"mcp({tool:"team_orchestrator_send_message", args:{to:"*", content:"..."}})"#,
        )
        .replace(
            "{report_result}(summary='...')",
            r#"mcp({tool:"team_orchestrator_report_result", args:{summary:"..."}})"#,
        )
}

fn pi_communication_contract(mode: CommunicationMode) -> String {
    match mode {
        CommunicationMode::LeaderCentric => r#"# Team Agent communication contract: leader_centric

- Progress, blockers, questions: mcp({tool:"team_orchestrator_send_message", args:{to:"leader", content:"..."}})

Respond through Team Agent MCP tools only to actionable requests or questions; writing in your terminal does not deliver it."#
            .to_string(),
        CommunicationMode::Orchestrated => mode.runtime_contract("mcp"),
    }
}

fn communication_mode(value: Option<&str>) -> Result<CommunicationMode, LifecycleError> {
    let Some(value) = value else {
        return Ok(CommunicationMode::default());
    };
    CommunicationMode::parse(value)
        .ok_or_else(|| LifecycleError::Compile(format!("unknown communication_mode {value:?}")))
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

fn output_contract(agent: &WorkerCommandAgent, report_result: &str) -> Option<String> {
    (agent.output_contract_format.as_deref() == Some("result_envelope_v1"))
        .then(|| RESULT_ENVELOPE_OUTPUT_CONTRACT.replace("{report_result}", report_result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_uses_identity_then_runtime_contract_python_order() {
        // #264 D6: Python truth source (prompt.py:39, live ps confirmed) builds
        // chunks = [identity, TEAMMATE_SYSTEM_PROMPT, role_body, output].
        // The previous assertion locked the inverted contract-first order with no
        // Python evidence; this is the corrected golden.
        // 0.3.5 union (copilot v2 C-1 / B2 MUST-4 行为层守): identity 必须 FIRST —
        // 空白上下文问"你是谁"的第一行答案必须先答 Team Agent worker 身份;
        // Copilot 适配的 C-1-3 行为层要求与其它 provider 同步。
        let agent = WorkerCommandAgent {
            id: Some("coder".to_string()),
            provider: Provider::Codex,
            role: Some("Runtime Developer".to_string()),
            system_prompt_inline: Some("Implement the assigned slice.".to_string()),
            system_prompt_file: None,
            output_contract_format: Some("result_envelope_v1".to_string()),
            communication_mode: CommunicationMode::default(),
            dangerously_skip_permissions: false,
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
        assert!(identity < runtime && runtime < role && role < output);
        let slowdown_phrase = format!("500/{}", 500 + 29);
        assert!(prompt.contains(&slowdown_phrase));
        assert!(prompt.contains("Runtime Developer"));
    }

    #[test]
    fn all_provider_mode_prompts_share_silence_contract_without_unconditional_replies() {
        for provider in [Provider::Codex, Provider::Pi] {
            for mode in CommunicationMode::ALL.iter().copied() {
                let agent = WorkerCommandAgent {
                    id: Some("worker".to_string()),
                    provider,
                    role: Some("developer".to_string()),
                    system_prompt_inline: Some("worker body".to_string()),
                    system_prompt_file: None,
                    output_contract_format: Some("result_envelope_v1".to_string()),
                    communication_mode: mode,
                    dangerously_skip_permissions: false,
                };
                let prompt = compile_worker_system_prompt(&agent).unwrap();
                assert!(prompt.contains(
                    "Do not reply to pure ACKs, greetings, or unchanged status notices (such as \"paused\" or \"waiting\"); after reporting a blocker once or completing a task, remain silent until a new actionable instruction arrives."
                ));
                assert!(!prompt.contains("Silence and resumption (mandatory)"));
                assert!(!prompt.contains("When you receive a message from the leader or a teammate, you MUST respond"));
                match mode {
                    CommunicationMode::LeaderCentric => {
                        assert!(prompt.contains(
                            "Respond through Team Agent MCP tools only to actionable requests or questions; writing in your terminal does not deliver it."
                        ));
                    }
                    CommunicationMode::Orchestrated => {
                        assert!(prompt.contains("Respond to task-related messages through Team Agent MCP tools."));
                        assert!(prompt.contains("A pure ACK, unrelated status, or non-task message does not require a response."));
                    }
                }
            }
        }
    }
}
