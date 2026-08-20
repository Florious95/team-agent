//!
//! Provider session-scan boundary.
//!
//! `ProviderAdapter` keeps the public polling facade in `adapter.rs`; this
//! module owns one-shot backing-store scans and provider-specific attribution
//! filters.

use std::path::PathBuf;

use crate::provider::types::{CapturedSession, ProviderError, SessionId};
use crate::provider::Provider;

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod common;
pub(crate) mod copilot;
pub(crate) mod grok;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSessionContext {
    pub agent_id: String,
    pub spawn_cwd: PathBuf,
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub spawned_at: Option<String>,
    pub expected_session_id: Option<SessionId>,
    pub provider_projects_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSessionCandidate {
    pub captured: CapturedSession,
    pub embedded_agent_id: Option<String>,
    pub positive_agent_id_match: bool,
    pub agent_path_match: bool,
}

pub(crate) fn scan_session_candidates_once(
    provider: Provider,
    context: &CaptureSessionContext,
) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
    if matches!(provider, Provider::Copilot) {
        return Ok(copilot::scan_session_store(context));
    }
    if matches!(provider, Provider::Grok) {
        return Ok(grok::scan_session_store(context));
    }
    if matches!(provider, Provider::Claude | Provider::ClaudeCode)
        && context.expected_session_id.is_some()
    {
        return Ok(claude::scan_expected_session(context));
    }

    let candidates = common::candidate_session_files(provider, context)?;
    let mut out = common::parse_candidate_files(provider, context, candidates);
    if matches!(provider, Provider::Codex) {
        codex::retain_spawn_cwd(context, &mut out);
    }
    common::apply_spawned_at_filter(provider, context, &mut out);

    if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return claude::apply_expected_session_filter(context, out);
    }
    if matches!(provider, Provider::Codex) {
        out = codex::apply_expected_session_filter(context, out);
    }

    common::apply_spawn_time_window_if_unique(provider, context, &mut out);
    common::sort_expected_first_if_needed(context, &mut out);
    Ok(out)
}

pub(crate) use claude::rollout_path_has_leader_marker as rollout_path_has_claude_leader_marker;
