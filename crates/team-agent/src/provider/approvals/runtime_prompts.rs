//! Runtime approval prompt decisions shared by coordinator hooks.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::provider::{ApprovalFingerprint, ApprovalKind, ApprovalPrompt};

pub const RUNTIME_MCP_APPROVAL_SERVER: &str = "team_orchestrator";

pub fn runtime_mcp_tool_allowlisted(tool: &str) -> bool {
    !tool.trim().is_empty()
}

pub fn runtime_mcp_prompt_allowlisted(prompt: &ApprovalPrompt) -> bool {
    prompt.server.as_deref() == Some(RUNTIME_MCP_APPROVAL_SERVER)
        && prompt.tool.as_deref().is_some_and(runtime_mcp_tool_allowlisted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeApprovalDecision {
    AutoApprove,
    AwaitingHumanConfirm,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitingHumanConfirmFact {
    pub team: String,
    pub agent_id: String,
    pub fingerprint: ApprovalFingerprint,
    pub dedupe_key: String,
    pub prompt_kind: String,
    pub prompt: String,
    pub tool: Option<String>,
    pub command: Option<String>,
    pub reason: String,
    pub next_step: String,
}

impl AwaitingHumanConfirmFact {
    pub fn to_event_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "event": "worker.awaiting_human_confirm",
            "team": self.team,
            "team_id": self.team,
            "owner_team_id": self.team,
            "agent_id": self.agent_id,
            "fingerprint": self.fingerprint.as_str(),
            "dedupe_key": self.dedupe_key,
            "prompt_kind": self.prompt_kind,
            "prompt": self.prompt,
            "tool": self.tool,
            "command": self.command,
            "reason": self.reason,
            "next_step": self.next_step,
        })
    }

    pub fn to_leader_message_content(&self) -> String {
        self.to_event_payload().to_string()
    }
}

pub fn runtime_approval_decision(
    prompt: &ApprovalPrompt,
    leader_auto_approval_allowed: bool,
) -> RuntimeApprovalDecision {
    match awaiting_human_confirm_reason(prompt, leader_auto_approval_allowed) {
        Some(_) => RuntimeApprovalDecision::AwaitingHumanConfirm,
        None if prompt.kind == ApprovalKind::McpTool => RuntimeApprovalDecision::AutoApprove,
        None => RuntimeApprovalDecision::Ignore,
    }
}

pub fn awaiting_human_confirm_reason(
    prompt: &ApprovalPrompt,
    leader_auto_approval_allowed: bool,
) -> Option<&'static str> {
    match prompt.kind {
        ApprovalKind::McpTool => {
            if !runtime_mcp_prompt_allowlisted(prompt) {
                Some("tool_not_allowlisted")
            } else if !leader_auto_approval_allowed {
                Some("leader_restricted")
            } else {
                None
            }
        }
        ApprovalKind::Command => Some("command_approval_requires_human"),
        ApprovalKind::Unknown => Some("approval_requires_human"),
    }
}

pub fn approval_prompt_fingerprint(team: &str, agent_id: &str, prompt: &ApprovalPrompt) -> ApprovalFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(team.as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_bytes());
    hasher.update([0]);
    hasher.update(prompt.prompt.as_bytes());
    hasher.update([0]);
    if let Some(tool) = prompt.tool.as_deref() {
        hasher.update(tool.as_bytes());
    }
    hasher.update([0]);
    if let Some(server) = prompt.server.as_deref() {
        hasher.update(server.as_bytes());
    }
    hasher.update([0]);
    if let Some(command) = prompt.command.as_deref() {
        hasher.update(command.as_bytes());
    }
    hasher.update([0]);
    hasher.update(format!("{:?}", prompt.kind).as_bytes());
    let digest = hasher.finalize();
    let fingerprint = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ApprovalFingerprint::new(fingerprint)
}

pub fn awaiting_human_confirm_dedupe_key(
    team: &str,
    agent_id: &str,
    fingerprint: &ApprovalFingerprint,
) -> String {
    format!("awaiting_human_confirm:{team}:{agent_id}:{}", fingerprint.as_str())
}

pub fn awaiting_human_confirm_fact(
    team: &str,
    agent_id: &str,
    prompt: &ApprovalPrompt,
    reason: &str,
) -> AwaitingHumanConfirmFact {
    let fingerprint = approval_prompt_fingerprint(team, agent_id, prompt);
    AwaitingHumanConfirmFact {
        team: team.to_string(),
        agent_id: agent_id.to_string(),
        dedupe_key: awaiting_human_confirm_dedupe_key(team, agent_id, &fingerprint),
        fingerprint,
        prompt_kind: prompt_kind_wire(prompt.kind).to_string(),
        prompt: prompt.prompt.clone(),
        tool: prompt.tool.clone(),
        command: prompt.command.clone(),
        reason: reason.to_string(),
        next_step: "review the worker pane and approve or deny the prompt manually".to_string(),
    }
}

fn prompt_kind_wire(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::McpTool => "mcp_tool",
        ApprovalKind::Command => "command",
        ApprovalKind::Unknown => "unknown",
    }
}
