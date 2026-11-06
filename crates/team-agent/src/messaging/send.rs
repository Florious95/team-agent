//! Sender identity captured by the current CLI/MCP authority boundary.

use crate::model::ids::AgentId;

/// 发件目标:单 target / 广播 `*` / 扇出 list (`send.py:36` `target: str|list[str]|None`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageTarget {
    /// 单 agent / pane target。
    Single(String),
    /// `"*"` 广播全队。
    Broadcast,
    /// 扇出到显式 list。
    Fanout(Vec<String>),
}

/// Sender identity captured from a framework-owned runtime context.
///
/// Public CLI/MCP inputs never construct this value from a caller-supplied
/// string. The wrapper keeps that trust boundary visible throughout delivery
/// instead of degrading the identity back to an untyped option field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedSender {
    agent_id: AgentId,
    full_name: Option<String>,
}

impl TrustedSender {
    pub fn from_runtime_identity_with_source(
        agent_id: AgentId,
        workspace: impl std::fmt::Display,
        team: &str,
    ) -> Self {
        let full_name = format!("{}::{}/{}", workspace, team, agent_id.as_str());
        Self {
            agent_id,
            full_name: Some(full_name),
        }
    }

    pub fn leader() -> Self {
        Self {
            agent_id: AgentId::new("leader"),
            full_name: None,
        }
    }

    pub fn as_str(&self) -> &str {
        self.agent_id.as_str()
    }

    pub fn display_name(&self) -> &str {
        self.full_name.as_deref().unwrap_or_else(|| self.as_str())
    }
}

impl serde::Serialize for TrustedSender {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.display_name())
    }
}
