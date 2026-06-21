//! unit-6 (Stage 2) — provider session namespace.
//!
//! Houses session-specific provider concerns: resume preflight,
//! session capture (moved from `crate::session_capture` in unit-6),
//! session backing checks, etc.

pub mod capture;
pub mod resume;

pub use resume::{
    ProviderBackingCheck, RecoveryHint, ResumeDecisionDetail, ResumePreflight,
    ResumePreflightOutcome, ResumeRefusalReason,
};
