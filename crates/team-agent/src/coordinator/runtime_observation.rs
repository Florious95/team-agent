//! Shared coordinator runtime observation seam.
//!
//! S0 only defines the typed capture/result surface. Lane 1 fills capture facts;
//! Lane 2 fills detector results from those facts.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::model::enums::Provider;
use crate::model::ids::{AgentId, TeamKey};
use crate::provider::{ProcessLiveness, RolloutPath};
use crate::transport::{PaneId, PaneInfo, SessionName, WindowName};

use super::types::{CompactionResult, LeaderApiError, SessionDriftResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRuntimeFact {
    pub team_key: Option<TeamKey>,
    pub agent_id: AgentId,
    pub provider: Option<Provider>,
    pub session_name: Option<SessionName>,
    pub window: Option<WindowName>,
    pub pane_id: Option<PaneId>,
    pub scrollback_tail: String,
    pub pane_info: Option<PaneInfo>,
    pub agent_state_snapshot: Value,
    pub stored_session_id: Option<String>,
    pub last_output_at: Option<String>,
    pub rollout_path: Option<RolloutPath>,
    pub process_liveness: Option<ProcessLiveness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderCaptureFact {
    pub team_key: Option<TeamKey>,
    pub leader_receiver: Option<Value>,
    pub pane_id: Option<PaneId>,
    pub scrollback_tail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeObservationResults {
    pub captures_by_agent: BTreeMap<AgentId, CapturedRuntimeFact>,
    pub compaction: Vec<CompactionResult>,
    pub session_drift: Vec<SessionDriftResult>,
    pub api_errors: Vec<LeaderApiError>,
}

pub fn observe(
    workspace: &Path,
    state: &mut Value,
    captures_by_agent: BTreeMap<AgentId, CapturedRuntimeFact>,
    leader_capture: Option<LeaderCaptureFact>,
) -> RuntimeObservationResults {
    super::runtime_detectors::observe_runtime(workspace, state, captures_by_agent, leader_capture)
}
