//! ---
//! purpose: 运行时观测的类型缝——定义「一次 tick 捕到的事实」与「探测结果集合」两个形状，并把探测转交 runtime_detectors
//! contract:
//!   provides:
//!     - name: observe
//!       what: 把本 tick 的 per-agent 捕获事实与 leader 捕获事实交给探测器，返回结果集合
//!   depends:
//!     - super::runtime_detectors
//!     - super::types
//!     - crate::provider
//!     - crate::transport
//! boundary:
//!   - 不做捕获本身（scrollback/pane 快照由 tick 侧采集后传入）
//!   - 不做任何判定，判定全在 runtime_detectors
//! maturity: wired
//! ---
//!
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

/// ---
/// purpose: 本 tick 观测的统一入口，把捕获事实转交 runtime_detectors 并原样带回结果
/// params:
///   workspace: workspace 根，探测器据此写事件日志
///   state: 可变运行时状态；探测器在其中保存跨 tick 的去重/计数键
///   captures_by_agent: 本 tick 每个 agent 的捕获事实（scrollback 尾、pane 信息、已存 session id 等）
///   leader_capture: leader 侧捕获事实；缺失表示本 tick 没拿到 leader 屏幕，api-error 探测随之为空
/// returns: 捕获事实原样回传，外加 compaction / session_drift / leader api-error 三组探测结果
/// ---
pub fn observe(
    workspace: &Path,
    state: &mut Value,
    captures_by_agent: BTreeMap<AgentId, CapturedRuntimeFact>,
    leader_capture: Option<LeaderCaptureFact>,
) -> RuntimeObservationResults {
    super::runtime_detectors::observe_runtime(workspace, state, captures_by_agent, leader_capture)
}
