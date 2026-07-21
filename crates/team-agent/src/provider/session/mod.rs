//! unit-6 (Stage 2) — provider session namespace.
//!
//! Houses session-specific provider concerns: resume preflight,
//! session capture (moved from `crate::session_capture` in unit-6),
//! session backing checks, etc.

pub mod capture;
mod context_fork;
pub mod resume;

pub use context_fork::ContextForkProof;
pub(crate) use context_fork::{verify_context_fork, ContextBackingSnapshot};

pub use resume::{
    ProviderBackingCheck, RecoveryHint, ResumeDecisionDetail, ResumePreflight,
    ResumePreflightOutcome, ResumeRefusalReason,
};
