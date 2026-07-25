//! Official communication modes shared by configuration and projection consumers.

/// The complete product-supported communication-mode catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunicationMode {
    LeaderCentric,
    Orchestrated,
}

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
}

impl Default for CommunicationMode {
    fn default() -> Self {
        Self::LeaderCentric
    }
}
