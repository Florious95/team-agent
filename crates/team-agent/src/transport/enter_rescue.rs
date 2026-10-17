//! Independent watchdog receipts. Registration is unconditional; key authority is not.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::enums::Provider;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterRescueContext {
    pub message_id: String,
    pub owner_team_id: String,
    pub recipient: String,
    pub payload_sha256: String,
    pub target: Option<EnterRescueTarget>,
    pub proof: Option<EnterRescueProof>,
}

impl EnterRescueContext {
    pub fn marker(&self) -> String {
        format!("[team-agent-token:{}]", self.message_id)
    }

    /// Binds an opaque-paste receipt to this message and this original carrier.
    pub fn scope_sha256(&self) -> Option<String> {
        let bytes = serde_json::to_vec(&(
            &self.message_id,
            &self.owner_team_id,
            &self.recipient,
            &self.payload_sha256,
            &self.target,
        ))
        .ok()?;
        Some(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn valid_proof(&self) -> bool {
        self.proof.as_ref().is_some_and(|proof| {
            proof.valid()
                && proof.owned_capsule.as_ref().is_none_or(|capsule| {
                    capsule.valid() && self.scope_sha256().as_ref() == Some(&capsule.scope_sha256)
                })
        })
    }

    pub fn valid_message(&self) -> bool {
        self.message_id.starts_with("msg_")
            && self
                .message_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !self.owner_team_id.is_empty()
            && !self.recipient.is_empty()
            && self.recipient != "leader"
            && valid_digest(&self.payload_sha256)
    }
}

/// `selected` records canonical original routing, not a late adoption of a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterRescueTarget {
    pub provider: Provider,
    pub endpoint: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub pane_height: u32,
    pub carrier_birth: String,
    pub binding_nonce: String,
    pub session: String,
    pub window: String,
}

impl EnterRescueTarget {
    pub fn selected(
        provider: Provider,
        pane_id: String,
        pane_pid: u32,
        session: String,
        window: String,
    ) -> Self {
        Self {
            provider,
            pane_id,
            pane_pid,
            session,
            window,
            endpoint: String::new(),
            pane_height: 0,
            carrier_birth: String::new(),
            binding_nonce: String::new(),
        }
    }

    pub fn same_selection(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.pane_id == other.pane_id
            && self.pane_pid == other.pane_pid
            && self.session == other.session
            && self.window == other.window
    }

    pub fn bound(&self) -> bool {
        std::path::Path::new(&self.endpoint).is_absolute()
            && self.pane_id.starts_with('%')
            && self.pane_pid > 1
            && self.pane_height >= 10
            && !self.carrier_birth.is_empty()
            && !self.binding_nonce.is_empty()
            && !self.session.is_empty()
            && !self.window.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterRescueProof {
    pub ui_identity: String,
    pub footer_sha256: String,
    pub visible_marker: bool,
    pub transcript_marker_absent: bool,
    pub owned_capsule: Option<OwnedCapsule>,
}

impl EnterRescueProof {
    pub fn valid(&self) -> bool {
        !self.ui_identity.is_empty()
            && valid_digest(&self.footer_sha256)
            && (self.visible_marker || self.owned_capsule.as_ref().is_some_and(OwnedCapsule::valid))
    }
}

/// Only a prior empty-baseline/single-owned-paste transition may create this.
/// Snapshotting a visible capsule label alone never creates ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedCapsule {
    pub scope_sha256: String,
    pub label: String,
    pub baseline_sha256: String,
    pub baseline_capsules: Vec<String>,
    pub observed_after_owned_paste: bool,
}

impl OwnedCapsule {
    pub fn valid(&self) -> bool {
        self.observed_after_owned_paste
            && valid_digest(&self.scope_sha256)
            && !self.label.is_empty()
            && self.label.len() <= 80
            && valid_digest(&self.baseline_sha256)
            && !self.baseline_capsules.contains(&self.label)
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterRescueOutcome {
    Consumed,
    NoEffect {
        reason: &'static str,
        reobserve: bool,
    },
    Entered {
        accepted: bool,
    },
}
