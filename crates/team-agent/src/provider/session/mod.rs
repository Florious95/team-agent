//! unit-6 (Stage 2) — provider session namespace.
//!
//! Houses session-specific provider concerns: resume preflight,
//! session capture (planned move from `crate::session_capture` in
//! unit-6's full landing), session backing checks, etc.

pub mod resume;

pub use resume::{
    ProviderBackingCheck, ResumeDecisionDetail, ResumePreflight, ResumePreflightOutcome,
    ResumeRefusalReason,
};
