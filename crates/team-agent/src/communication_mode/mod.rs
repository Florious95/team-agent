//!
//! Official communication modes shared by configuration and projection consumers.

/// The complete product-supported communication-mode catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunicationMode {
    LeaderCentric,
    Orchestrated,
}

const LEADER_CENTRIC_RUNTIME_CONTRACT: &str = r#"# Team Agent communication contract: leader_centric

- Progress, blockers, questions: team_orchestrator.send_message(to='leader', content='...')

When you receive a message from the leader or a teammate, you MUST respond
through MCP tools. Writing a reply in your terminal does nothing — the sender
will never see it."#;

const ORCHESTRATED_RUNTIME_CONTRACT: &str = r#"# Team Agent communication contract: orchestrated

- Send progress only through the declared channel for the assigned task.
- Respond to task-related messages through Team Agent MCP tools.
- A pure ACK, unrelated status, or non-task message does not require a response."#;

impl CommunicationMode {
    pub const ALL: &[Self] = &[Self::LeaderCentric, Self::Orchestrated];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeaderCentric => "leader_centric",
            Self::Orchestrated => "orchestrated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.as_str() == value)
    }

    pub const fn runtime_contract(self) -> &'static str {
        match self {
            Self::LeaderCentric => LEADER_CENTRIC_RUNTIME_CONTRACT,
            Self::Orchestrated => ORCHESTRATED_RUNTIME_CONTRACT,
        }
    }
}

impl Default for CommunicationMode {
    fn default() -> Self {
        Self::LeaderCentric
    }
}
