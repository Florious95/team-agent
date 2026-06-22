//! Coordinator core:daemon lifecycle 宿主 + 单次 tick 编排(19 步固定顺序)+ health/start/stop。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use crate::event_log::EventLog;
use crate::leader::{TakeoverReminderResult, TurnClassification, TurnStateClassifier};
use crate::provider::{
    approval_choice_keys, awaiting_human_confirm_fact, awaiting_human_confirm_reason,
    choose_internal_mcp_approval_choice, extract_approval_prompt, runtime_approval_decision,
    ProcessLiveness, RuntimeApprovalDecision, TurnState,
};

use super::health::{
    coordinator_log_path, coordinator_meta_path, coordinator_metadata_ok, coordinator_pid_path,
    pid_is_running, read_coordinator_metadata, write_coordinator_metadata,
};
use super::runtime_observation::{self, CapturedRuntimeFact};
use super::types::{
    AgentId, CoordinatorHealthStatus, HealthReport, MetadataSource, Pid, ProviderRegistry,
    SchemaHealth, StartError, StartOutcome, StartReport, StopError, StopOutcome, StopReport,
    TickStopReason, WorkspacePath,
};
use super::types::{
    CollectedResult, CompactionResult, DeadlockAlert, DeliveredMessage, FiredScheduledEvent,
    IdleAlert, LeaderApiError, SessionDriftResult,
};

const STARTUP_PROMPT_GRACE_SECS: i64 = 120;
const RUNTIME_APPROVAL_INITIAL_BACKOFF_SECS: i64 = 30;
const RUNTIME_APPROVAL_MAX_BACKOFF_SECS: i64 = 300;
const IDLE_HEALTH_CAPTURE_INTERVAL_SECS: i64 = 60;

// ===========================================================================
// TickReport / TickError(§10:tick(..) -> Result<TickReport, TickError>)
// ===========================================================================

/// 单次 tick 报告(`lifecycle.py:373-385` 成功 / `:349-363` degraded)。
/// degraded 用 `ok:false, reason: Some(PersistenceDegraded)`(card 表)。
/// `stop:true` 触发主循环退出(tmux_session_missing)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// `ok`(`lifecycle.py:374`)。
    pub ok: bool,
    /// `stop`(`lifecycle.py:279/375`)—— true 触发主循环 break。
    pub stop: bool,
    /// 非 ok 时的原因(`lifecycle.py:279,353`)。
    pub reason: Option<TickStopReason>,
    /// bug-084:tick-end save 是否成功落盘(`lifecycle.py:354`)。`None` ⇔ 未走到 save(早退)。
    pub persisted: Option<bool>,
    /// `_deliver_pending_messages` 投递条数/句柄(`lifecycle.py:285`)——cross-dep step 11。
    pub delivered: Vec<DeliveredMessage>,
    /// `_fire_due_scheduled_events` 触发的 scheduled(`lifecycle.py:286`)——cross-dep step 11。
    pub scheduled: Vec<FiredScheduledEvent>,
    /// `_detect_stuck_agents` 卡住的 agent(`lifecycle.py:287`)——cross-dep step 11。
    pub stuck: Vec<AgentId>,
    /// idle take-over 提醒(`lifecycle.py:303-308`)——should_ping 时一条。
    pub idle_alerts: Vec<IdleAlert>,
    /// `detect_cross_worker_deadlocks`(`lifecycle.py:309`)——cross-dep step 11。
    pub deadlock_alerts: Vec<DeadlockAlert>,
    /// `detect_compaction_degradation` 结果(`lifecycle.py:310-330`,仅 codex)——cross-dep step 11。
    pub compaction: Vec<CompactionResult>,
    /// `detect_session_drift` 结果(`lifecycle.py:331-343`,仅 codex)——cross-dep step 11。
    pub session_drift: Vec<SessionDriftResult>,
    /// `detect_leader_api_errors`(`lifecycle.py:344`)——cross-dep step 11。
    pub api_errors: Vec<LeaderApiError>,
    /// `_collect_results_and_notify_watchers`(`lifecycle.py:364`)——degraded 时为空(未走到)。
    pub results: Vec<CollectedResult>,
}

/// tick 失败错误(§10:daemon-path 返 Result)。bug-084:`save_runtime_state` 失败**不**走这里
/// (那是 degraded `TickReport`,主循环不 catch 它);本 enum 是 tick 编排其余环节(load state /
/// store 构造 / 原子调用)的硬失败,主循环 catch 后退避(`__main__.py:62`)。
#[derive(Debug, Error)]
pub enum TickError {
    /// `load_runtime_state` 失败(state.json 损坏 / 锁)。
    #[error("load runtime state failed: {0}")]
    StateLoad(#[from] crate::state::StateError),
    /// `MessageStore(workspace)` 构造失败(`lifecycle.py:275`)。
    #[error("message store: {0}")]
    MessageStore(#[from] crate::message_store::MessageStoreError),
    /// EventLog 写失败。
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    /// transport 探测失败(tmux session 存活查询等)。
    #[error("transport: {0}")]
    Transport(#[from] crate::transport::TransportError),
    /// provider trait 调用失败(startup/runtime prompt handlers, classifiers)。
    #[error("provider: {0}")]
    Provider(#[from] crate::provider::ProviderError),
    /// messaging subsystem failure(delivery/scheduler/result watchers).
    #[error("messaging: {0}")]
    Messaging(#[from] crate::messaging::MessagingError),
    /// coordinator.tick panic caught by the daemon loop.
    #[error("panic: {0}")]
    Panic(String),
}

/// Issue 2 (Round 3b gate review §6): when the runtime state has
/// `active_team_key` AND `teams.<key>` is a populated object, return the
/// team-scoped projection so the coordinator's tick reads `session_name` /
/// `agents` / `leader_receiver` from the team's nested entry rather than
/// the top-level (often stale) view. When the projection cannot be derived,
/// returns `None` and the tick falls back to the raw state — preserving
/// behavior for legacy single-team workspaces and tests that don't seed
/// `teams.<key>`. Sibling teams under `state.teams.*` are NOT touched.
fn coordinator_team_scoped_state(
    workspace: &std::path::Path,
    raw_state: &Value,
) -> Option<Value> {
    let active = raw_state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let has_team_entry = raw_state
        .get("teams")
        .and_then(Value::as_object)
        .map(|teams| teams.contains_key(active))
        .unwrap_or(false);
    if !has_team_entry {
        return None;
    }
    crate::state::projection::select_runtime_state(workspace, Some(active)).ok()
}

// ===========================================================================
// Coordinator struct(daemon lifecycle + tick orchestration)
// ===========================================================================

/// tick 末原子 save 失败注入钩(bug-084)。生产装配为 `None`(走真实 `save_runtime_state`);
/// 测试装配一个返回 `Err` 的闭包,在不触碰真实磁盘的前提下强制 save 失败,断言 degraded
/// `TickReport` 而非 panic/Err。porter 在 `tick` 的「ATOMIC save」包裹点先查它再落真实 save。
pub type SaveHook =
    Box<dyn Fn(&WorkspacePath, &Value) -> Result<(), crate::state::StateError> + Send + Sync>;

/// tick 链式副作用 ORDER 记录器(测试探针)。porter 在 `tick` 的每个原子调用点 push 一个
/// 稳定步骤名;测试断言固定序列。生产装配为 `None`(零开销,porter 用 `if let Some(rec)` 守卫)。
pub type OrderRecorder = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;

/// per-workspace coordinator。daemon 主循环 + 单次 tick 编排的宿主。
///
/// provider 调用一律经注入的 `ProviderAdapter` trait object(MUST-NOT-13:**绝不**依赖任何
/// provider client crate;测试 mock 断言调用计数 = 0)。transport 探测经注入的 `Transport` trait。
pub struct Coordinator {
    workspace: WorkspacePath,
    /// provider adapter 解析器(`get_provider_registry` 等价;经 trait 注入,可 mock)。
    #[allow(dead_code)]
    provider_registry: Box<dyn ProviderRegistry>,
    /// transport 控制面(tmux session 存活探测等;经 trait 注入,可 mock)。
    #[allow(dead_code)]
    transport: Box<dyn crate::transport::Transport>,
    /// bug-084 save 注入钩。`None` ⇔ 真实 `state::save_runtime_state`。
    #[allow(dead_code)]
    save_hook: Option<SaveHook>,
    /// tick 副作用 ORDER 探针。`None` ⇔ 不记录(生产)。
    #[allow(dead_code)]
    order_recorder: Option<OrderRecorder>,
}

impl Coordinator {
    /// 构造(注入 provider registry + transport)。spawn 出的 daemon 在 `run` 前装配它。
    pub fn new(
        workspace: WorkspacePath,
        provider_registry: Box<dyn ProviderRegistry>,
        transport: Box<dyn crate::transport::Transport>,
    ) -> Self {
        Self {
            workspace,
            provider_registry,
            transport,
            save_hook: None,
            order_recorder: None,
        }
    }

    /// 测试装配:直接构出 `Coordinator`(不经 `new` 的 `unimplemented!()`),注入 mock
    /// transport + mock provider registry + 可选 save 注入钩 + ORDER 探针。**纯 test-support
    /// 脚手架**(真实 impl,非 `unimplemented!()`):它只装配字段,不执行任何 daemon 逻辑;
    /// tick/health/start/stop 仍是 `unimplemented!()` 生产体,因此调它们的契约仍 RED。
    #[cfg(test)]
    pub(crate) fn for_test(
        workspace: WorkspacePath,
        provider_registry: Box<dyn ProviderRegistry>,
        transport: Box<dyn crate::transport::Transport>,
        save_hook: Option<SaveHook>,
        order_recorder: Option<OrderRecorder>,
    ) -> Self {
        Self {
            workspace,
            provider_registry,
            transport,
            save_hook,
            order_recorder,
        }
    }

    // ── tick 编排(lifecycle.py:250-385)──────────────────────────────────────

    /// 单次 tick(`coordinator_tick`,`lifecycle.py:250`)。固定顺序串 step 8-11 原子:
    /// load state → tmux session 存活门(missing → stop:true)→ capture missing sessions →
    /// refresh runtime statuses → provider startup/runtime prompts → sync health →
    /// deliver pending → fire scheduled → detect stuck → idle/takeover ping(should_ping 时一条)→
    /// deadlock/compaction/drift/api-error 只读探测 → **原子 save state(bug-084 唯一包裹点)** →
    /// collect results → prune dedupe log。
    ///
    /// §10:daemon-path 返 `Result<TickReport, TickError>`。bug-084:save 失败返
    /// degraded `Ok(TickReport{ok:false, reason:PersistenceDegraded, persisted:Some(false)})`
    /// (**不**走 `Err`,主循环不 catch degraded,只 catch `Err` 退避)。
    /// §84:无 pending obligation + event 时**绝不**注入探索性 prompt。
    ///
    /// PORTER:在 ATOMIC save 包裹点先查 `self.save_hook`(`Some` → 用它代替真实
    /// `state::save_runtime_state`,bug-084 测试注入失败);在每个 step8-11 原子调用点
    /// `if let Some(rec) = &self.order_recorder { rec.lock()...push(STEP_NAME) }`(tick
    /// 副作用 ORDER 测试断言固定序列)。生产两者均 `None`,零开销。
    pub fn tick(&self) -> Result<TickReport, TickError> {
        self.record_step("load_state");
        let raw_state = crate::state::persist::load_runtime_state(self.workspace.as_path())?;
        // Issue 2 (Round 3b gate review §6): when the runtime carries
        // `active_team_key` AND `teams.<key>` exists, project the team-scoped
        // view (session_name / agents / leader_receiver come from the team's
        // nested object) instead of the raw top-level state. Otherwise the
        // coordinator would probe the wrong tmux session (e.g. stale
        // `session_name=team-tmp` while the real team session is
        // `team-prerelease-040-round3b`) and emit `coordinator.session_missing`
        // even though the right session is alive. Fall back to raw state when
        // no team scope can be derived (legacy single-team workspaces).
        let mut state = coordinator_team_scoped_state(self.workspace.as_path(), &raw_state)
            .unwrap_or(raw_state);
        let store = crate::message_store::MessageStore::open(self.workspace.as_path())?;
        let event_log = EventLog::new(self.workspace.as_path());
        increment_coordinator_tick_iteration_count(&self.workspace);

        self.record_step("tmux_session_gate");
        if let Some(session_name) = state
            .get("session_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let session = crate::transport::SessionName::new(session_name);
            if !self.transport.has_session(&session)? {
                event_log.write(
                    "coordinator.session_missing",
                    serde_json::json!({"session": session_name}),
                )?;
                notify_session_missing(self.workspace.as_path(), &state, &event_log, session_name)?;
                return Ok(empty_tick_report(
                    false,
                    true,
                    Some(TickStopReason::TmuxSessionMissing),
                    None,
                ));
            }
        }

        self.record_step("capture_missing");
        if let Err(error) = self.capture_missing_sessions(&mut state, &event_log) {
            let _ = event_log.write(
                "coordinator.tick.capture_missing_failed",
                serde_json::json!({"error": error.to_string()}),
            );
        }

        // Slice 1 energy gate: one pane snapshot per tick feeds probe eligibility,
        // health sync, and abnormal-exit detection. Missing panes are filtered
        // before any capture-pane call.
        let pane_snapshot = self.transport.list_targets().unwrap_or_default();
        let window_snapshot = state
            .get("session_name")
            .and_then(Value::as_str)
            .map(crate::transport::SessionName::new)
            .and_then(|session| self.transport.list_windows(&session).ok())
            .unwrap_or_default();
        let has_work_obligation = tick_has_work_obligation(&store);

        self.record_step("refresh_statuses");
        // TODO(spine slice 2b): split lightweight runtime status refresh from health sync.

        self.record_step("startup_prompts");
        self.handle_startup_prompts(&mut state, &event_log, &pane_snapshot, &window_snapshot);

        // #229 step2-retry: once an agent's `startup_prompts` flipped to `handled`
        // (this tick OR earlier), `queued_until_trust` messages for that recipient
        // become deliverable. Reset them to `accepted` so the existing
        // `deliver_pending` step below picks them up on THIS tick. Reuses the
        // delivery pipeline; no new injector. Best-effort logging on inner errors.
        if let Err(error) =
            self.requeue_trust_retries_for_handled_agents(&state, &store, &event_log)
        {
            let _ = event_log.write(
                "messaging.trust_retry_requeue_failed",
                serde_json::json!({"error": error.to_string()}),
            );
        }

        // B-4 / 036b N36 三路可用 — 监测步(runtime_prompts / sync_health /
        // detect_abnormal_exits)失败必须降级+continue,**不能**用 `?` 中断 tick,
        // 否则 deliver_pending(下行投递主干)够不到,消息卡 accepted。
        // bug-084 哲学 + A-6 同族:每步独立 try,失败写 `coordinator.tick.<step>_failed`
        // 事件后继续走下一步;tick 本身仍返 Ok。
        self.record_step("runtime_prompts");
        if let Err(error) = self.handle_runtime_approval_prompts(
            &mut state,
            &event_log,
            &pane_snapshot,
            &window_snapshot,
        ) {
            let _ = event_log.write(
                "coordinator.tick.runtime_prompts_failed",
                serde_json::json!({"error": error.to_string()}),
            );
        }

        self.record_step("sync_health");
        // P5 (C-P5-1, N3): ONE pane snapshot per tick, shared by sync_health and the
        // abnormal-exit pass (same-tick reuse only — the snapshot does not outlive
        // this tick; every tick re-reads).
        let captures_by_agent = match self.sync_agent_health(
            &mut state,
            &store,
            &event_log,
            &pane_snapshot,
            &window_snapshot,
            has_work_obligation,
        ) {
            Ok(captures) => captures,
            Err(error) => {
                let _ = event_log.write(
                    "coordinator.tick.sync_health_failed",
                    serde_json::json!({"error": error.to_string()}),
                );
                BTreeMap::new()
            }
        };
        if let Err(error) = self.detect_abnormal_exits(&mut state, &event_log, &pane_snapshot) {
            let _ = event_log.write(
                "coordinator.tick.detect_abnormal_failed",
                serde_json::json!({"error": error.to_string()}),
            );
        }

        self.record_step("deliver_pending");
        let delivered = crate::messaging::deliver_pending_messages(
            self.workspace.as_path(),
            &state,
            self.transport.as_ref(),
            &event_log,
        )?
        .into_iter()
        .map(|message_id| DeliveredMessage { message_id })
        .collect::<Vec<_>>();

        self.record_step("fire_scheduled");
        let scheduled = crate::messaging::fire_due_scheduled_events(
            self.workspace.as_path(),
            &store,
            self.transport.as_ref(),
            &event_log,
        )?
        .into_iter()
        .map(|id| FiredScheduledEvent { id })
        .collect::<Vec<_>>();

        // #236 nag_removal (N35): the time/state-inferred idle/stuck/deadlock nag
        // generators are no longer wired in. Step labels stay (tick ORDER lock) but
        // each body is a strict "produce no nag output" — empty `stuck`, empty
        // `idle_alerts`, empty `deadlock_alerts`. Delivery primitives
        // (deliver_pending / fire_scheduled / collect_results) above and below this
        // block continue to flow unchanged. `_state` / `_store` here are intentionally
        // unused (the lookups they powered were nag inputs only).
        self.record_step("detect_stuck");
        let stuck: Vec<AgentId> = Vec::new();
        self.record_step("record_unknown_idle");
        self.record_step("evaluate_takeover");
        let idle_alerts: Vec<IdleAlert> = Vec::new();
        self.record_step("detect_deadlocks");
        let deadlock_alerts: Vec<DeadlockAlert> = Vec::new();
        let _ = &store;

        self.record_step("detect_compaction");
        self.record_step("detect_drift");
        self.record_step("detect_api_errors");
        let leader_capture = self.capture_leader_receiver(&state);
        let observations = runtime_observation::observe(
            self.workspace.as_path(),
            &mut state,
            captures_by_agent,
            leader_capture,
        );
        let mut collections = TickCollections {
            delivered,
            scheduled,
            stuck,
            idle_alerts,
            deadlock_alerts,
            compaction: observations.compaction,
            session_drift: observations.session_drift,
            api_errors: observations.api_errors,
            results: Vec::new(),
        };

        self.record_step("atomic_save");
        let saved = match &self.save_hook {
            Some(hook) => hook(&self.workspace, &state),
            None => {
                crate::state::projection::save_team_scoped_state(self.workspace.as_path(), &state)
            }
        };
        if saved.is_err() {
            return Ok(base_tick_report(
                false,
                false,
                Some(TickStopReason::PersistenceDegraded),
                Some(false),
                collections,
            ));
        }

        self.record_step("collect_results");
        collections.results =
            collect_results(crate::messaging::collect_results_and_notify_watchers(
                self.workspace.as_path(),
                &event_log,
            )?);
        self.record_step("prune_dedupe_log");
        Ok(base_tick_report(true, false, None, Some(true), collections))
    }

    // #236 nag_removal (N35): the framework-synthesized idle/stuck/deadlock nag
    // generators (record_unknown_idle_nodes / evaluate_takeover / build_idle_nodes)
    // were removed by design. Delivery primitives still flow through the rest of
    // the tick body unchanged.

    fn capture_missing_sessions(
        &self,
        state: &mut Value,
        event_log: &EventLog,
    ) -> Result<(), TickError> {
        let report = crate::session_capture::capture_missing_provider_sessions_once(
            state,
            &mut |provider| self.provider_registry.adapter_for(provider),
            true,
            0,
        )?;
        // RM-039-STAT-001 third-round fix (architect verdict 2026-06-22):
        // when capture_missing_sessions assigns a new rollout_path to an
        // agent, clear that agent's `coordinator_idle_capture_next_at` so
        // the JSONL classifier is not gated by stale warm-idle suppression
        // on the SAME tick that the rollout becomes readable. Without this,
        // an agent that was IDLE before attribution would carry the old
        // suppression timestamp into sync_agent_health and only the
        // expensive pane Tail(40) path would observe its working state.
        if !report.assigned.is_empty() {
            if let Some(agents) = state
                .get_mut("agents")
                .and_then(serde_json::Value::as_object_mut)
            {
                for assigned_agent_id in &report.assigned {
                    if let Some(agent_obj) = agents
                        .get_mut(assigned_agent_id)
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        agent_obj.remove("coordinator_idle_capture_next_at");
                    }
                }
            }
        }
        for failure in report.capture_failures {
            event_log.write(
                "coordinator.tick.capture_missing_failed",
                serde_json::json!({
                    "agent_id": failure.agent_id,
                    "error": failure.error,
                }),
            )?;
        }
        for ambiguous in report.ambiguous {
            event_log.write(
                "provider.session.attribution_ambiguous",
                serde_json::json!({
                    "agent_id": ambiguous.agent_id,
                    "spawn_cwd": ambiguous.spawn_cwd,
                }),
            )?;
        }
        Ok(())
    }

    fn sync_agent_health(
        &self,
        state: &mut Value,
        store: &crate::message_store::MessageStore,
        event_log: &EventLog,
        pane_infos: &[crate::transport::PaneInfo],
        window_snapshot: &[crate::transport::WindowName],
        has_work_obligation: bool,
    ) -> Result<BTreeMap<AgentId, CapturedRuntimeFact>, TickError> {
        let mut captures = BTreeMap::new();
        let snapshot = state.clone();
        let team = crate::state::projection::team_state_key(&snapshot);
        let team_key = Some(crate::model::ids::TeamKey::new(team.clone()));
        let session_name = state
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        // B-4 / 036b N36 三路可用 — sync_health 内 per-agent capture 失败本就降级
        // (写 coordinator.agent_capture_failed 后 continue),不打断 deliver_pending
        // 主干。但 contract 要求一条【tick 级】可观测的 step-failed 信号 —
        // sync_health 失败一旦发生就在末尾 emit `coordinator.tick.sync_health_failed`
        // (含 "tick" + "_failed" 双串),避免 silent。
        let mut had_capture_failure = false;
        // P5 (C-P5-2): one list-windows per SESSION per tick — memoized across the
        // agent loop instead of one fork per agent.
        let mut windows_by_session: BTreeMap<
            String,
            Result<Vec<crate::transport::WindowName>, String>,
        > = BTreeMap::new();
        if let Some(session_name) = session_name.as_deref() {
            windows_by_session.insert(session_name.to_string(), Ok(window_snapshot.to_vec()));
        }
        let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
            return Ok(captures);
        };
        for (agent_id, agent) in agents {
            if !agent_probe_base_eligible(agent) {
                continue;
            }
            // RM-039-STAT-001 third-round fix (architect verdict 2026-06-22):
            // try the provider JSONL classifier FIRST, BEFORE any pane-target
            // / window checks and BEFORE warm_idle_capture_suppressed. The
            // historical chain ran JSONL only inside the pane-capture branch,
            // so a stale pane-fallback `idle_prompt` could land first
            // (before session attribution completed), then
            // `warm_idle_capture_suppressed` would gate subsequent ticks for
            // IDLE_HEALTH_CAPTURE_INTERVAL_SECS even after JSONL gained a
            // `task_started` record + the worker process was alive. Now: if
            // the JSONL classifier returns a definite activity (not Unknown),
            // we write activity + agent_health here and bypass the warm-idle
            // suppression for the rest of this iteration. Pane capture still
            // runs so the CapturedRuntimeFact map stays populated for
            // downstream runtime detectors; only the activity classify
            // happens once.
            let jsonl_first = jsonl_activity_for_agent(agent);
            if let Some(activity) = jsonl_first.as_ref() {
                remember_idle_capture_schedule(agent, activity);
                write_activity(agent, activity, false);
                let last_output_at_now = agent
                    .get("last_output_at")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                write_agent_health(
                    store,
                    &team,
                    agent_id,
                    agent,
                    activity,
                    last_output_at_now.as_deref(),
                )?;
            }
            let Some((session, window, target)) =
                capture_window_target(agent, session_name.as_deref())
            else {
                continue;
            };
            let windows = match windows_by_session
                .entry(session.as_str().to_string())
                .or_insert_with(|| {
                    self.transport
                        .list_windows(&session)
                        .map_err(|error| error.to_string())
                }) {
                Ok(windows) => windows.clone(),
                Err(error) => {
                    had_capture_failure = true;
                    event_log.write(
                        "coordinator.agent_capture_failed",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "target": format!("{target:?}"),
                            "error": error.clone(),
                        }),
                    )?;
                    continue;
                }
            };
            if !windows.iter().any(|known| known == &window) {
                continue;
            }
            // Warm-idle suppression still gates pane fallback ONLY. When
            // JSONL above produced a definite activity, we already wrote it,
            // so warm-idle gating no longer matters for the activity path —
            // but we still want CapturedRuntimeFact populated for downstream
            // detectors, so fall through into the pane capture block below
            // (skipping the pane-classify activity write at the bottom).
            if jsonl_first.is_none() && warm_idle_capture_suppressed(agent, has_work_obligation) {
                continue;
            }
            let captured = match self
                .transport
                .capture(&target, crate::transport::CaptureRange::Tail(40))
            {
                Ok(captured) => captured,
                Err(error) => {
                    had_capture_failure = true;
                    event_log.write(
                        "coordinator.agent_capture_failed",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "target": format!("{target:?}"),
                            "error": error.to_string(),
                        }),
                    )?;
                    continue;
                }
            };
            let pane_in_mode = agent
                .get("pane_in_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let current_command = agent
                .get("pane_current_command")
                .or_else(|| agent.get("current_command"))
                .and_then(Value::as_str)
                .map(str::to_string);
            // Python approvals/status.py:68-73 — last_output_at advances ONLY when the
            // scrollback sha256 digest changed (last_output_hash gate), and it is
            // refreshed BEFORE classification (the classifier sees the updated value).
            // A non-empty but UNCHANGED capture must not dirty the state every tick
            // (P3 umbrella: steady second tick is a zero state write).
            let output_advanced =
                !captured.text.is_empty() && scrollback_digest_advanced(agent, &captured.text);
            if output_advanced {
                if let Some(agent_obj) = agent.as_object_mut() {
                    agent_obj.insert(
                        "last_output_at".to_string(),
                        serde_json::json!(chrono::Utc::now().to_rfc3339()),
                    );
                }
            }
            let last_output_at_now = agent
                .get("last_output_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            // E47 (0.3.24 P0, idle/busy 假阳): the provider JSONL classifier
            // already ran at the top of this iteration. If it returned a
            // definite fact (Some), activity + agent_health were written
            // there; we MUST NOT classify-and-overwrite here, otherwise the
            // pane fallback would flip the JSONL truth back to a stale
            // idle_prompt. Only run the pane fallback when JSONL returned
            // None (no readable rollout, TurnState::Unknown, or unparseable
            // log). RM-039-STAT-001 third-round fix (architect verdict
            // 2026-06-22).
            let last_output_at = last_output_at_now;
            if jsonl_first.is_none() {
                let activity = crate::messaging::classify_agent_activity(
                    &snapshot,
                    &captured.text,
                    pane_in_mode,
                    current_command.as_deref(),
                    last_output_at.as_deref(),
                );
                remember_idle_capture_schedule(agent, &activity);
                write_activity(agent, &activity, false);
                write_agent_health(
                    store,
                    &team,
                    agent_id,
                    agent,
                    &activity,
                    last_output_at.as_deref(),
                )?;
            }
            let pane_info = matching_capture_pane_info(agent, &session, &window, pane_infos);
            let pane_id = pane_info
                .as_ref()
                .map(|info| info.pane_id.clone())
                .or_else(|| agent_pane_id(agent));
            let rollout_path = agent_rollout_path(agent).map(crate::provider::RolloutPath::new);
            captures.insert(
                AgentId::new(agent_id.clone()),
                CapturedRuntimeFact {
                    team_key: team_key.clone(),
                    agent_id: AgentId::new(agent_id.clone()),
                    provider: agent
                        .get("provider")
                        .and_then(Value::as_str)
                        .and_then(parse_provider),
                    session_name: Some(session),
                    window: Some(window),
                    pane_id,
                    scrollback_tail: captured.text,
                    pane_info,
                    agent_state_snapshot: agent.clone(),
                    stored_session_id: agent
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    last_output_at,
                    rollout_path,
                    process_liveness: explicit_process_liveness(agent),
                },
            );
        }
        // B-4 step-level signal:若本 tick 有任一 capture 失败,emit
        // `coordinator.tick.sync_health_failed`(含 "tick" + "_failed")让 contract
        // 可观测,deliver_pending 主干不受影响。
        if had_capture_failure {
            let _ = event_log.write(
                "coordinator.tick.sync_health_failed",
                serde_json::json!({"step": "sync_health", "degraded": true}),
            );
        }
        Ok(captures)
    }

    fn capture_leader_receiver(
        &self,
        state: &Value,
    ) -> Option<runtime_observation::LeaderCaptureFact> {
        let receiver = state.get("leader_receiver")?.clone();
        let pane_id = receiver
            .get("pane_id")
            .and_then(Value::as_str)
            .filter(|pane_id| !pane_id.is_empty())
            .map(crate::transport::PaneId::new)?;
        let captured = self
            .transport
            .capture(
                &crate::transport::Target::Pane(pane_id.clone()),
                crate::transport::CaptureRange::Tail(40),
            )
            .ok()?;
        Some(runtime_observation::LeaderCaptureFact {
            team_key: Some(crate::model::ids::TeamKey::new(
                crate::state::projection::team_state_key(state),
            )),
            leader_receiver: Some(receiver),
            pane_id: Some(pane_id),
            scrollback_tail: captured.text,
        })
    }

    /// #236 `worker.abnormal_exit` watcher.
    ///
    /// Notify only when both signals are true: the provider process is dead AND the
    /// latest transcript/rollout JSONL record is an explicit provider error. Dead-only
    /// and error-only observations are written as check/suppressed audit events with
    /// `notification=false`; they never call the N32 leader funnel. This path is
    /// intentionally separate from the generic transcript-only abnormal fact track.
    fn detect_abnormal_exits(
        &self,
        state: &mut Value,
        event_log: &EventLog,
        targets: &[crate::transport::PaneInfo],
    ) -> Result<(), TickError> {
        let snapshot = state.clone();
        let team = crate::state::projection::team_state_key(&snapshot);
        let session_name = snapshot.get("session_name").and_then(Value::as_str);
        for agent in abnormal_watch_agents(&snapshot) {
            let rollout_path =
                resolve_agent_rollout_path(self.workspace.as_path(), &agent.rollout_path);
            let metadata = match std::fs::metadata(&rollout_path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    upsert_abnormal_watch(
                        state,
                        &agent.agent_id,
                        abnormal_watch_payload(
                            &agent,
                            None,
                            None,
                            "unverifiable",
                            None,
                            Some(error.to_string()),
                        ),
                    );
                    continue;
                }
            };
            let size = metadata.len();
            let mtime_ns = metadata_mtime_ns(&metadata);
            // P1 (C-P1-2/3): (size, mtime_ns) pair gate — an unchanged transcript is not
            // read at all (live sample: 332MB whole-file read per agent per 2s tick).
            // ANY field change (including a size shrink / truncate) falls through to the
            // re-read below.
            if let (Some(mtime), Some(stored)) = (
                mtime_ns,
                abnormal_watch_stored_metadata(&snapshot, &agent.agent_id),
            ) {
                if stored == (size, mtime) {
                    continue;
                }
            }
            // P1 (C-P1-1): bounded tail read — the abnormal decision only consumes the
            // LATEST transcript record; window matches Python `_TAIL_BYTES` (131072,
            // idle_takeover_wiring.py:13), never less.
            let text = match read_tail_text(&rollout_path, ABNORMAL_TAIL_BYTES) {
                Ok(text) => text,
                Err(error) => {
                    upsert_abnormal_watch(
                        state,
                        &agent.agent_id,
                        abnormal_watch_payload(
                            &agent,
                            Some(size),
                            mtime_ns,
                            "unverifiable",
                            None,
                            Some(error.to_string()),
                        ),
                    );
                    continue;
                }
            };
            let liveness =
                agent_process_liveness(&agent, session_name, targets, self.transport.as_ref());
            let fact = crate::provider::latest_explicit_error_fact(agent.provider, &text);
            let decision = abnormal_exit_decision(liveness.state, fact.as_ref());
            let check_key = abnormal_check_key(&agent, &liveness, fact.as_ref(), size);
            upsert_abnormal_watch(
                state,
                &agent.agent_id,
                abnormal_watch_payload(
                    &agent,
                    Some(size),
                    mtime_ns,
                    process_liveness_wire(liveness.state),
                    fact.as_ref().map(|f| f.signature.as_str()),
                    None,
                ),
            );
            if abnormal_last_check_key(state, &agent.agent_id).as_deref()
                != Some(check_key.as_str())
            {
                write_abnormal_check(
                    event_log,
                    &team,
                    &agent,
                    &liveness,
                    fact.as_ref(),
                    decision,
                    size,
                    mtime_ns,
                )?;
                mark_abnormal_checked(state, &agent.agent_id, &check_key);
            }
            let fact = match (decision, fact) {
                (AbnormalExitDecision::Notify, Some(fact)) => fact,
                (AbnormalExitDecision::Suppress(reason), _) => {
                    let suppress_key = abnormal_suppression_key(&agent, &liveness, reason, size);
                    if abnormal_last_suppressed_key(state, &agent.agent_id).as_deref()
                        != Some(suppress_key.as_str())
                    {
                        write_abnormal_suppressed(event_log, &team, &agent, &liveness, reason)?;
                        mark_abnormal_suppressed(state, &agent.agent_id, &suppress_key);
                    }
                    continue;
                }
                (AbnormalExitDecision::NoSignal, _) => continue,
                (AbnormalExitDecision::Notify, None) => continue,
            };
            let dedupe_key = abnormal_dedupe_key(&agent, &fact, size);
            if abnormal_last_notified_key(state, &agent.agent_id).as_deref()
                == Some(dedupe_key.as_str())
            {
                continue;
            }
            let content = format_abnormal_exit_message(&team, &agent, &fact, &liveness, size);
            let outcome = crate::messaging::send_to_leader_receiver(
                self.workspace.as_path(),
                state,
                "leader",
                &content,
                None,
                &agent.agent_id,
                false,
                Some(&dedupe_key),
                event_log,
            )?;
            let notification_status = if outcome.ok {
                "queued"
            } else if matches!(outcome.status, crate::messaging::DeliveryStatus::Blocked) {
                "rebind_required"
            } else {
                "refused"
            };
            event_log.write(
                "worker.abnormal_exit",
                serde_json::json!({
                    "team_id": team.as_str(),
                    "agent_id": agent.agent_id.as_str(),
                    "provider": provider_wire(agent.provider),
                    "path": agent.rollout_path_display.as_str(),
                    "dead_process": true,
                    "process_dead": true,
                    "provider_process_dead": true,
                    "latest_error": true,
                    "latest_explicit_error": true,
                    "dead_process_and_latest_error": true,
                    "dead_process_and_latest_explicit_error": true,
                    "process_dead_and_latest_explicit_error": true,
                    "provider_process_dead_and_latest_explicit_error": true,
                    "signature": fact.signature.as_str(),
                    "turn_id": fact.turn_id.as_ref().map(|id| id.as_str()),
                    "size": size,
                    "mtime_ns": mtime_ns,
                    "process_liveness": process_liveness_wire(liveness.state),
                    "pid_status": liveness.detail.as_str(),
                    "notification_message_id": outcome.message_id,
                    "notification_status": notification_status,
                    "notification_channel": outcome.channel,
                }),
            )?;
            mark_abnormal_notified(state, &agent.agent_id, &dedupe_key);
        }
        Ok(())
    }

    fn handle_startup_prompts(
        &self,
        state: &mut Value,
        event_log: &EventLog,
        pane_infos: &[crate::transport::PaneInfo],
        windows: &[crate::transport::WindowName],
    ) {
        let session_name = state
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
            return;
        };
        for (agent_id, agent) in agents {
            if !agent_probe_base_eligible(agent) {
                continue;
            }
            // #229 step1-idem: once trust is auto-answered, the row carries
            // `startup_prompts = "handled"` (or "complete"). Both are terminal for
            // this tick loop — repeated ticks must not re-classify, re-send Enter,
            // or re-emit `startup_prompt_handled`. Treating "handled" the same as
            // "complete" makes the observable artifact exactly-once across ticks.
            if agent
                .get("startup_prompts")
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "handled" | "complete"))
            {
                continue;
            }
            let Some(provider) = agent
                .get("provider")
                .and_then(Value::as_str)
                .and_then(parse_provider)
            else {
                continue;
            };
            let Some((session, window, target)) =
                capture_window_target(agent, session_name.as_deref())
            else {
                continue;
            };
            if !agent_window_present(agent, &session, &window, pane_infos, windows) {
                continue;
            }
            clear_startup_probe_disable_if_epoch_changed(agent);
            if startup_probe_disabled_for_epoch(agent) {
                continue;
            }
            if !startup_probe_within_grace(agent) {
                disable_startup_probe_for_epoch(agent);
                continue;
            }
            let adapter = self.provider_registry.adapter_for(provider);
            let outcome =
                adapter.handle_startup_prompts_outcome(self.transport.as_ref(), &target, 1, 0.0);
            // swallow batch 2 ② (A1): an unobservable pane is a surfaced failure, not a
            // silent "no prompts" — the agent's startup_prompts state stays un-handled.
            if let Some(error) = &outcome.capture_error {
                let _ = event_log.write(
                    "provider.startup_prompt_failed",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "target": format!("{target:?}"),
                        "error": error,
                    }),
                );
            }
            let handled = outcome.handled;
            if handled.is_empty() {
                continue;
            }
            let handled_payload = serde_json::Value::Array(
                handled
                    .into_iter()
                    .map(|prompt| {
                        serde_json::json!({
                            "prompt": prompt.prompt,
                            "action": prompt.action,
                        })
                    })
                    .collect(),
            );
            // #229 step1 observability: emit `startup_prompt_handled` so the trust
            // answer is observable in events.jsonl (was silent state-write only).
            // Best-effort — state write below is the source of truth.
            let _ = event_log.write(
                "startup_prompt_handled",
                serde_json::json!({
                    "agent_id": agent_id,
                    "provider": provider,
                    "handled": handled_payload.clone(),
                }),
            );
            let Some(agent_obj) = agent.as_object_mut() else {
                continue;
            };
            agent_obj.insert("startup_prompts".to_string(), serde_json::json!("handled"));
            agent_obj.insert(
                "startup_prompt_status".to_string(),
                serde_json::json!("handled"),
            );
            agent_obj.remove("startup_prompt_probe_disabled_at");
            agent_obj.insert("startup_prompt_handled".to_string(), handled_payload);
        }
    }

    /// #229 step2-retry: after `handle_startup_prompts` flips an agent's status to
    /// `handled`/`complete`, scan `messages` for `queued_until_trust` rows targeting
    /// that recipient and flip them back to `accepted` so this same tick's
    /// `deliver_pending` replays them. Same row, same message_id, same pipeline.
    fn requeue_trust_retries_for_handled_agents(
        &self,
        state: &Value,
        store: &crate::message_store::MessageStore,
        event_log: &EventLog,
    ) -> Result<(), crate::message_store::MessageStoreError> {
        let Some(agents) = state.get("agents").and_then(Value::as_object) else {
            return Ok(());
        };
        let handled_recipients: Vec<&str> = agents
            .iter()
            .filter(|(_, agent)| {
                agent
                    .get("startup_prompts")
                    .and_then(Value::as_str)
                    .is_some_and(|status| matches!(status, "handled" | "complete"))
            })
            .map(|(id, _)| id.as_str())
            .collect();
        if handled_recipients.is_empty() {
            return Ok(());
        }
        let conn = crate::db::schema::open_db(store.db_path())?;
        let mut stmt = conn.prepare(
            "select message_id, recipient from messages where status = 'queued_until_trust'",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<(String, String)>, _>>()?;
        for (message_id, recipient) in rows {
            if !handled_recipients.iter().any(|r| *r == recipient.as_str()) {
                continue;
            }
            store.mark(&message_id, "accepted", None)?;
            let _ = event_log.write(
                "messaging.trust_retry_requeued",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": recipient,
                    "reason": "startup_prompt_handled",
                }),
            );
        }
        Ok(())
    }

    fn handle_runtime_approval_prompts(
        &self,
        state: &mut Value,
        event_log: &EventLog,
        pane_infos: &[crate::transport::PaneInfo],
        windows: &[crate::transport::WindowName],
    ) -> Result<(), TickError> {
        let snapshot = state.clone();
        let team = crate::state::projection::team_state_key(&snapshot);
        let session_name = snapshot
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut dedup_updates = Vec::new();
        {
            let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
                return Ok(());
            };
            for (agent_id, agent) in agents {
                if !agent_probe_base_eligible(agent) {
                    clear_awaiting_human_confirm(agent);
                    continue;
                }
                let approval_policy = runtime_approval_policy_from_agent(agent);
                let auto_answer_allowed = approval_policy.auto_answer_allowed();
                let Some(target) = runtime_approval_target(agent, session_name.as_deref()) else {
                    clear_awaiting_human_confirm(agent);
                    dedup_updates.push(AwaitingDedupUpdate::Clear {
                        team: team.clone(),
                        agent_id: agent_id.to_string(),
                    });
                    continue;
                };
                let target_present = capture_window_target(agent, session_name.as_deref())
                    .map_or_else(
                        || runtime_approval_target_present(&target, pane_infos, windows),
                        |(session, window, _)| {
                            agent_window_present(agent, &session, &window, pane_infos, windows)
                        },
                    );
                if !target_present {
                    continue;
                }
                if runtime_approval_backoff_active(agent) {
                    continue;
                }
                let captured = match self
                    .transport
                    .capture(&target, crate::transport::CaptureRange::Tail(80))
                {
                    Ok(captured) => captured,
                    Err(error) => {
                        event_log.write(
                            "runtime_approval.capture_failed",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "target": format!("{target:?}"),
                                "error": error.to_string(),
                            }),
                        )?;
                        remember_runtime_approval_backoff(agent);
                        continue;
                    }
                };
                let Some(prompt) = extract_approval_prompt(agent_id, &captured.text) else {
                    clear_awaiting_human_confirm(agent);
                    dedup_updates.push(AwaitingDedupUpdate::Clear {
                        team: team.clone(),
                        agent_id: agent_id.to_string(),
                    });
                    remember_runtime_approval_backoff(agent);
                    continue;
                };
                clear_runtime_approval_backoff(agent);
                match runtime_approval_decision(&prompt, auto_answer_allowed) {
                    RuntimeApprovalDecision::AutoApprove => {
                        clear_awaiting_human_confirm(agent);
                        dedup_updates.push(AwaitingDedupUpdate::Clear {
                            team: team.clone(),
                            agent_id: agent_id.to_string(),
                        });
                        let choice = choose_internal_mcp_approval_choice(&prompt);
                        let keys = approval_choice_keys(&prompt, &captured.text, &choice)
                            .into_iter()
                            .filter_map(runtime_approval_key)
                            .collect::<Vec<_>>();
                        // A-6 / Python approvals/runtime_prompts.py:21-43: prompts are handled
                        // per-agent with run_cmd(check=False) — one agent's tmux failure must
                        // not abort the whole tick for the rest.
                        if let Err(error) = self.transport.send_keys(&target, &keys) {
                            event_log.write(
                                "runtime_approval.send_keys_failed",
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "target": format!("{target:?}"),
                                    "tool": prompt.tool,
                                    "error": error.to_string(),
                                }),
                            )?;
                            continue;
                        }
                        let after = self
                            .transport
                            .capture(&target, crate::transport::CaptureRange::Tail(80))
                            .ok()
                            .and_then(|capture| extract_approval_prompt(agent_id, &capture.text));
                        let cleared = after.as_ref().is_none_or(|after| {
                            after.prompt != prompt.prompt || after.tool != prompt.tool
                        });
                        event_log.write(
                        "runtime_approval.auto_approved",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "server": prompt.server.as_deref(),
                            "tool": prompt.tool.as_deref(),
                            "choice": choice,
                            "cleared": cleared,
                            "policy_source": approval_policy.source,
                            "inherited": approval_policy.inherited,
                            "explicit_yes_confirmed": approval_policy.explicit_yes_confirmed,
                            "worker_capability_above_leader": approval_policy.worker_capability_above_leader,
                        }),
                    )?;
                        event_log.write(
                            "mcp.tool.auto_approved",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "server": prompt.server.as_deref(),
                                "tool": prompt.tool.as_deref(),
                                "choice": choice,
                                "cleared": cleared,
                                "inherit_reason": approval_policy.inherit_reason(),
                                "bypass_source": approval_policy.source,
                                "provider": approval_policy.provider,
                                "flag": approval_policy.flag,
                                "inherited": approval_policy.inherited,
                                "explicit_yes_confirmed": approval_policy.explicit_yes_confirmed,
                                "worker_capability_above_leader": approval_policy.worker_capability_above_leader,
                            }),
                        )?;
                    }
                    RuntimeApprovalDecision::AwaitingHumanConfirm => {
                        let Some(reason) =
                            awaiting_human_confirm_reason(&prompt, auto_answer_allowed)
                        else {
                            continue;
                        };
                        let fact = awaiting_human_confirm_fact(&team, agent_id, &prompt, reason);
                        let previous = agent
                            .get("awaiting_human_confirm")
                            .and_then(|v| v.get("fingerprint"))
                            .and_then(Value::as_str);
                        if previous == Some(fact.fingerprint.as_str())
                            || state_awaiting_human_confirm_fingerprint(&snapshot, &team, agent_id)
                                .as_deref()
                                == Some(fact.fingerprint.as_str())
                        {
                            remember_awaiting_human_confirm(agent, &fact);
                            continue;
                        }
                        let notification = awaiting_human_confirm_payload(agent, &fact);
                        let content = notification.to_string();
                        let _ = crate::messaging::send_to_leader_receiver(
                            self.workspace.as_path(),
                            &snapshot,
                            "leader",
                            &content,
                            None,
                            agent_id,
                            false,
                            Some(&fact.dedupe_key),
                            event_log,
                        )?;
                        event_log.write("worker.awaiting_human_confirm", notification)?;
                        remember_awaiting_human_confirm(agent, &fact);
                        dedup_updates.push(AwaitingDedupUpdate::Remember(fact.clone()));
                        match reason {
                            "tool_not_allowlisted" => {
                                event_log.write(
                                    "runtime_approval.tool_not_allowlisted",
                                    serde_json::json!({
                                        "agent_id": agent_id,
                                        "tool": prompt.tool,
                                        "kind": prompt.kind,
                                        "prompt": prompt.prompt,
                                    }),
                                )?;
                            }
                            "leader_restricted" | "leader_safety_restricted" => {
                                event_log.write(
                                    "runtime_approval.blocked_by_leader_safety",
                                    serde_json::json!({
                                        "agent_id": agent_id,
                                        "tool": prompt.tool,
                                        "command": prompt.command,
                                        "kind": prompt.kind,
                                        "prompt": prompt.prompt,
                                    }),
                                )?;
                            }
                            "command_approval_requires_human" => {
                                event_log.write(
                                    "runtime_approval.command_approval_requires_human",
                                    serde_json::json!({
                                        "agent_id": agent_id,
                                        "tool": prompt.tool,
                                        "command": prompt.command,
                                        "kind": prompt.kind,
                                        "prompt": prompt.prompt,
                                    }),
                                )?;
                            }
                            _ => {}
                        }
                    }
                    RuntimeApprovalDecision::Ignore => {
                        clear_awaiting_human_confirm(agent);
                        dedup_updates.push(AwaitingDedupUpdate::Clear {
                            team: team.clone(),
                            agent_id: agent_id.to_string(),
                        });
                    }
                }
            }
        }
        for update in dedup_updates {
            match update {
                AwaitingDedupUpdate::Remember(fact) => {
                    remember_state_awaiting_human_confirm(state, &fact)
                }
                AwaitingDedupUpdate::Clear { team, agent_id } => {
                    clear_state_awaiting_human_confirm(state, &team, &agent_id)
                }
            }
        }
        Ok(())
    }

    // ── health / start / stop(lifecycle.py:26-247)───────────────────────────

    /// `coordinator_health`(`lifecycle.py:26`)。pid + meta + schema 三合一健康。
    /// doctor / start 前置调它。`ok = running ∧ metadata_ok ∧ schema_ok`。
    pub fn health(&self) -> Result<HealthReport, TickError> {
        let schema = self.schema_health();
        let pid_path = coordinator_pid_path(&self.workspace);
        let pid = read_pid_file(&pid_path);
        let (status, running) = match pid {
            Some(pid) if pid_is_running(pid).unwrap_or(false) => {
                (CoordinatorHealthStatus::Running, true)
            }
            Some(_) => (CoordinatorHealthStatus::Stale, false),
            None if pid_path.exists() => (CoordinatorHealthStatus::InvalidPid, false),
            None => (CoordinatorHealthStatus::Missing, false),
        };
        let metadata = read_coordinator_metadata(&self.workspace);
        let metadata_ok = pid.is_some_and(|p| coordinator_metadata_ok(metadata.as_ref(), p));
        Ok(HealthReport {
            ok: running && metadata_ok && schema.ok,
            status,
            pid,
            metadata,
            metadata_ok,
            schema,
        })
    }

    /// `start_coordinator`(`lifecycle.py:49`)。幂等启动:已健康 no-op;metadata 不兼容先 stop 再起;
    /// schema 不兼容拒启给 hint;否则 spawn 自身二进制子命令(`team-agent coordinator --workspace ..`,
    /// Python 是 `python -m team_agent.coordinator`,`lifecycle.py:108`)。
    /// **schema 兼容门**:三元任一不匹配 → restart_incompatible,**不可静默继续**(card §89)。
    pub fn start(&self) -> Result<StartReport, StartError> {
        let health = self
            .health()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if health.ok {
            return Ok(StartReport {
                ok: true,
                pid: health.pid,
                status: StartOutcome::AlreadyRunning,
                log: Some(coordinator_log_path(&self.workspace)),
                schema_error: None,
                action: None,
            });
        }
        if !health.schema.ok {
            return Ok(StartReport {
                ok: false,
                pid: health.pid,
                status: StartOutcome::SchemaIncompatible,
                log: None,
                schema_error: health.schema.error,
                action: health.schema.action,
            });
        }
        let pid = Pid::new(std::process::id());
        write_coordinator_metadata(&self.workspace, pid, MetadataSource::Start)?;
        std::fs::write(coordinator_pid_path(&self.workspace), pid.to_string())?;
        Ok(StartReport {
            ok: true,
            pid: Some(pid),
            status: StartOutcome::Started,
            log: Some(coordinator_log_path(&self.workspace)),
            schema_error: None,
            action: None,
        })
    }

    /// `stop_coordinator`(`lifecycle.py:229`)。SIGTERM + 清 pid/meta。pid 非整数 → 清文件返回。
    pub fn stop(&self) -> Result<StopReport, StopError> {
        let pid_path = coordinator_pid_path(&self.workspace);
        if !pid_path.exists() {
            return Ok(StopReport {
                ok: true,
                status: StopOutcome::Missing,
                pid: None,
            });
        }
        let pid = read_pid_file(&pid_path);
        remove_file_if_exists(&pid_path)?;
        remove_file_if_exists(&coordinator_meta_path(&self.workspace))?;
        match pid {
            Some(pid) => Ok(StopReport {
                ok: true,
                status: StopOutcome::Stopped,
                pid: Some(pid),
            }),
            None => Ok(StopReport {
                ok: true,
                status: StopOutcome::InvalidPidRemoved,
                pid: None,
            }),
        }
    }

    /// `message_store_schema_health`(`lifecycle.py:197`)。DB 列兼容门:区分 pre-init 必需列缺失
    /// (拒启)vs migratable 列缺失(可迁移)。`advanced repair-state --schema` 用其 action hint。
    pub fn schema_health(&self) -> SchemaHealth {
        // A-8: the gate must inspect the REAL team.db (Python lifecycle.py:197+
        // message_store_schema_health); a hardcoded ok:true left the card §89
        // restart_incompatible door permanently dead.
        super::health::message_store_schema_health(&self.workspace)
    }

    fn record_step(&self, step: &'static str) {
        if let Some(recorder) = &self.order_recorder {
            if let Ok(mut guard) = recorder.lock() {
                guard.push(step);
            }
        }
    }
}

fn base_tick_report(
    ok: bool,
    stop: bool,
    reason: Option<TickStopReason>,
    persisted: Option<bool>,
    collections: TickCollections,
) -> TickReport {
    TickReport {
        ok,
        stop,
        reason,
        persisted,
        delivered: collections.delivered,
        scheduled: collections.scheduled,
        stuck: collections.stuck,
        idle_alerts: collections.idle_alerts,
        deadlock_alerts: collections.deadlock_alerts,
        compaction: collections.compaction,
        session_drift: collections.session_drift,
        api_errors: collections.api_errors,
        results: collections.results,
    }
}

#[derive(Default)]
struct TickCollections {
    delivered: Vec<DeliveredMessage>,
    scheduled: Vec<FiredScheduledEvent>,
    stuck: Vec<AgentId>,
    idle_alerts: Vec<IdleAlert>,
    deadlock_alerts: Vec<DeadlockAlert>,
    compaction: Vec<CompactionResult>,
    session_drift: Vec<SessionDriftResult>,
    api_errors: Vec<LeaderApiError>,
    results: Vec<CollectedResult>,
}

fn empty_tick_report(
    ok: bool,
    stop: bool,
    reason: Option<TickStopReason>,
    persisted: Option<bool>,
) -> TickReport {
    base_tick_report(ok, stop, reason, persisted, TickCollections::default())
}

fn collect_results(value: Value) -> Vec<CollectedResult> {
    let Some(result_id) = value.get("result_id").and_then(Value::as_str) else {
        return Vec::new();
    };
    vec![CollectedResult {
        result_id: result_id.to_string(),
    }]
}

struct ProviderTurnClassifier;

impl TurnStateClassifier for ProviderTurnClassifier {
    fn classify(
        &self,
        provider: crate::provider::Provider,
        session_log_text: &str,
    ) -> Result<TurnClassification, crate::leader::LeaderError> {
        let result = crate::provider::classify(
            provider,
            session_log_text,
            ProcessLiveness::Unverifiable,
            0.0,
        )
        .map_err(|e| crate::leader::LeaderError::Validation(e.to_string()))?;
        Ok(TurnClassification {
            state: result.state,
            turn_id: result.turn_id.map(|id| id.as_str().to_string()),
            annotations: result.annotations,
            reason: Some(result.reason),
        })
    }
}

/// P3 (C-P3-1, N1): the tick counter is a transient diagnostic, NOT source-of-truth
/// state — keeping it in state.json made EVERY tick dirty and defeated both save
/// short-circuits. It lives in its own metadata file; old state files still carrying
/// `coordinator.coordinator_tick_iteration_count` load fine (read-compat, C-P3-3) —
/// new versions simply stop writing it.
fn increment_coordinator_tick_iteration_count(workspace: &WorkspacePath) {
    let path = crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator_tick.json");
    let next = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("coordinator_tick_iteration_count")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0)
        .saturating_add(1);
    let _ = std::fs::write(
        &path,
        serde_json::json!({"coordinator_tick_iteration_count": next}).to_string(),
    );
}

fn idle_node_value(node: &crate::leader::IdleNode) -> Value {
    serde_json::json!({
        "node_id": node.node_id,
        "role": match node.role {
            crate::leader::NodeRole::Worker => "worker",
            crate::leader::NodeRole::Leader => "leader",
        },
        "state": turn_state_wire(node.state),
    })
}

fn turn_state_wire(state: TurnState) -> &'static str {
    match state {
        TurnState::Idle => "idle",
        TurnState::Working => "working",
        TurnState::IdleInterrupted => "idle_interrupted",
        TurnState::BlockedOnHuman => "blocked_on_human",
        TurnState::Abnormal => "abnormal",
        TurnState::Unknown => "unknown",
    }
}

fn provider_wire(provider: crate::model::enums::Provider) -> &'static str {
    match provider {
        crate::model::enums::Provider::Claude => "claude",
        crate::model::enums::Provider::ClaudeCode => "claude_code",
        crate::model::enums::Provider::Codex => "codex",
        crate::model::enums::Provider::Copilot => "copilot",
        crate::model::enums::Provider::GeminiCli => "gemini_cli",
        crate::model::enums::Provider::Fake => "fake",
    }
}

#[derive(Debug, Clone)]
struct AbnormalWatchAgent {
    agent_id: String,
    provider: crate::model::enums::Provider,
    rollout_path: PathBuf,
    rollout_path_display: String,
    status: Option<String>,
    process_liveness: Option<ProcessLiveness>,
    window: Option<String>,
    pane_id: Option<String>,
    pid: Option<Pid>,
    current_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessCheck {
    state: ProcessLiveness,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbnormalExitDecision {
    Notify,
    Suppress(&'static str),
    NoSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AbnormalExitGate {
    provider_process_dead: bool,
    latest_explicit_error: bool,
}

impl AbnormalExitGate {
    fn new(process_liveness: ProcessLiveness, latest_explicit_error: bool) -> Self {
        Self {
            provider_process_dead: process_liveness == ProcessLiveness::Dead,
            latest_explicit_error,
        }
    }

    fn should_notify_worker_abnormal_exit(self) -> bool {
        should_notify_worker_abnormal_exit(self.provider_process_dead, self.latest_explicit_error)
    }

    fn suppressed_reason(self) -> Option<&'static str> {
        match (self.provider_process_dead, self.latest_explicit_error) {
            (true, false) => Some("dead_only"),
            (false, true) => Some("error_only"),
            _ => None,
        }
    }
}

fn abnormal_exit_decision(
    process_liveness: ProcessLiveness,
    latest_explicit_error: Option<&crate::provider::FaultFact>,
) -> AbnormalExitDecision {
    let gate = AbnormalExitGate::new(process_liveness, latest_explicit_error.is_some());
    if gate.should_notify_worker_abnormal_exit() {
        return AbnormalExitDecision::Notify;
    }
    match gate.suppressed_reason() {
        Some(reason) => AbnormalExitDecision::Suppress(reason),
        None => AbnormalExitDecision::NoSignal,
    }
}

fn should_notify_worker_abnormal_exit(
    provider_process_dead: bool,
    latest_explicit_error: bool,
) -> bool {
    provider_process_dead && latest_explicit_error
}

fn resolve_agent_rollout_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn abnormal_watch_agents(state: &Value) -> Vec<AbnormalWatchAgent> {
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return Vec::new();
    };
    agents
        .iter()
        .filter_map(|(agent_id, agent)| {
            if matches!(agent.get("status").and_then(Value::as_str), Some("paused")) {
                return None;
            }
            let provider = agent
                .get("provider")
                .and_then(Value::as_str)
                .and_then(parse_provider)?;
            let rollout_path_display = ["rollout_path", "transcript_path", "session_log_path"]
                .into_iter()
                .find_map(|key| agent.get(key).and_then(Value::as_str))
                .filter(|path| !path.is_empty())?
                .to_string();
            Some(AbnormalWatchAgent {
                agent_id: agent_id.clone(),
                provider,
                rollout_path: PathBuf::from(&rollout_path_display),
                rollout_path_display,
                status: agent
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                process_liveness: explicit_process_liveness(agent),
                window: agent
                    .get("window")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pane_id: agent
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pid: agent_pid(agent),
                current_command: agent
                    .get("pane_current_command")
                    .or_else(|| agent.get("current_command"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn agent_pid(agent: &Value) -> Option<Pid> {
    ["provider_pid", "process_id", "pid", "child_pid", "pane_pid"]
        .into_iter()
        .find_map(|key| json_u32(agent.get(key)).map(Pid::new))
}

fn explicit_process_liveness(agent: &Value) -> Option<ProcessLiveness> {
    if let Some(process) = agent
        .get("provider_process")
        .or_else(|| agent.get("process"))
    {
        if let Some(liveness) = explicit_process_liveness(process) {
            return Some(liveness);
        }
    }
    for key in [
        "provider_process_liveness",
        "process_liveness",
        "pane_liveness",
    ] {
        match agent.get(key).and_then(Value::as_str) {
            Some("dead") => return Some(ProcessLiveness::Dead),
            Some("alive" | "live") => return Some(ProcessLiveness::Alive),
            Some("unverifiable" | "unknown") => return Some(ProcessLiveness::Unverifiable),
            _ => {}
        }
    }
    for key in [
        "provider_process_alive",
        "process_alive",
        "provider_alive",
        "alive",
    ] {
        if let Some(alive) = agent.get(key).and_then(Value::as_bool) {
            return Some(if alive {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            });
        }
    }
    for key in [
        "provider_process_dead",
        "process_dead",
        "provider_dead",
        "dead",
    ] {
        if let Some(dead) = agent.get(key).and_then(Value::as_bool) {
            return Some(if dead {
                ProcessLiveness::Dead
            } else {
                ProcessLiveness::Alive
            });
        }
    }
    for key in ["status", "state", "liveness"] {
        match agent.get(key).and_then(Value::as_str) {
            Some("dead" | "exited" | "terminated" | "crashed" | "missing") => {
                return Some(ProcessLiveness::Dead);
            }
            Some("alive" | "live" | "running") => return Some(ProcessLiveness::Alive),
            Some("unverifiable" | "unknown") => return Some(ProcessLiveness::Unverifiable),
            _ => {}
        }
    }
    None
}

fn json_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        })
        .and_then(|n| u32::try_from(n).ok())
}

fn agent_process_liveness(
    agent: &AbnormalWatchAgent,
    session_name: Option<&str>,
    targets: &[crate::transport::PaneInfo],
    transport: &dyn crate::transport::Transport,
) -> ProcessCheck {
    if let Some(pid) = agent.pid {
        return pid_process_check("pid", pid);
    }
    if let Some(liveness) = agent.process_liveness {
        return process_check(
            liveness,
            format!("explicit:{}", process_liveness_wire(liveness)),
        );
    }
    if agent.status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "stopped" | "missing" | "error" | "dead" | "exited" | "terminated" | "crashed"
        )
    }) {
        return process_check(
            ProcessLiveness::Dead,
            format!("status:{}", agent.status.as_deref().unwrap_or("unknown")),
        );
    }
    if let Some(command) = agent.current_command.as_deref() {
        return command_process_check(agent.provider, command);
    }
    if let Some(target) = matching_agent_target(agent, session_name, targets) {
        if let Some(command) = target.current_command.as_deref() {
            return pane_command_process_check(agent.provider, target, command);
        }
        if let Some(pid) = target.pane_pid.map(Pid::new) {
            return pid_process_check("pane_pid", pid);
        }
        return process_check(
            ProcessLiveness::Unverifiable,
            "pane_present_pid_unknown".to_string(),
        );
    }
    if let Some(pane_id) = agent.pane_id.as_deref() {
        let pane = crate::transport::PaneId::new(pane_id);
        return match transport.liveness(&pane) {
            Ok(crate::transport::PaneLiveness::Dead) => {
                process_check(ProcessLiveness::Dead, format!("pane_dead:{pane_id}"))
            }
            Ok(crate::transport::PaneLiveness::Live) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_live_pid_unknown:{pane_id}"),
            ),
            Ok(crate::transport::PaneLiveness::Unknown) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_unknown:{pane_id}"),
            ),
            Err(error) => process_check(
                ProcessLiveness::Unverifiable,
                format!("pane_unverifiable:{pane_id}:{error}"),
            ),
        };
    }
    let (Some(session), Some(window)) = (session_name, agent.window.as_deref()) else {
        return process_check(
            ProcessLiveness::Unverifiable,
            "missing_session_or_window".to_string(),
        );
    };
    let session = crate::transport::SessionName::new(session);
    match transport.list_windows(&session) {
        Ok(windows) if windows.iter().any(|known| known.as_str() == window) => process_check(
            ProcessLiveness::Unverifiable,
            "window_present_pid_unknown".to_string(),
        ),
        Ok(_) => process_check(ProcessLiveness::Dead, format!("window_missing:{window}")),
        Err(error) => process_check(
            ProcessLiveness::Unverifiable,
            format!("window_unverifiable:{window}:{error}"),
        ),
    }
}

fn matching_agent_target<'a>(
    agent: &AbnormalWatchAgent,
    session_name: Option<&str>,
    targets: &'a [crate::transport::PaneInfo],
) -> Option<&'a crate::transport::PaneInfo> {
    if let Some(pane_id) = agent.pane_id.as_deref() {
        if let Some(target) = targets
            .iter()
            .find(|target| target.pane_id.as_str() == pane_id)
        {
            return Some(target);
        }
    }
    let (Some(session), Some(window)) = (session_name, agent.window.as_deref()) else {
        return None;
    };
    targets.iter().find(|target| {
        target.session.as_str() == session
            && target
                .window_name
                .as_ref()
                .is_some_and(|known| known.as_str() == window)
    })
}

fn pid_process_check(label: &str, pid: Pid) -> ProcessCheck {
    match pid_is_running(pid) {
        Ok(true) => process_check(ProcessLiveness::Alive, format!("{label}_running:{pid}")),
        Ok(false) => process_check(ProcessLiveness::Dead, format!("{label}_not_running:{pid}")),
        Err(error) => process_check(
            ProcessLiveness::Unverifiable,
            format!("{label}_unverifiable:{pid}:{error}"),
        ),
    }
}

fn command_process_check(provider: crate::model::enums::Provider, command: &str) -> ProcessCheck {
    if crate::leader::command_matches_provider(provider, command) {
        process_check(ProcessLiveness::Alive, format!("current_command:{command}"))
    } else {
        process_check(
            ProcessLiveness::Dead,
            format!("provider_not_foreground:{command}"),
        )
    }
}

fn pane_command_process_check(
    provider: crate::model::enums::Provider,
    pane: &crate::transport::PaneInfo,
    command: &str,
) -> ProcessCheck {
    if crate::leader::attribute_pane_provider(pane)
        .is_some_and(|candidate| crate::leader::provider_matches(candidate, provider))
    {
        process_check(ProcessLiveness::Alive, format!("current_command:{command}"))
    } else {
        process_check(
            ProcessLiveness::Dead,
            format!("provider_not_foreground:{command}"),
        )
    }
}

fn process_check(state: ProcessLiveness, detail: String) -> ProcessCheck {
    ProcessCheck { state, detail }
}

fn process_liveness_wire(state: ProcessLiveness) -> &'static str {
    match state {
        ProcessLiveness::Alive => "alive",
        ProcessLiveness::Dead => "dead",
        ProcessLiveness::Unverifiable => "unverifiable",
    }
}

fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> Option<u64> {
    let duration = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(
        duration
            .as_secs()
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(duration.subsec_nanos())),
    )
}

fn abnormal_watch_payload(
    agent: &AbnormalWatchAgent,
    size: Option<u64>,
    mtime_ns: Option<u64>,
    liveness: &str,
    signature: Option<&str>,
    error: Option<String>,
) -> Value {
    let dead_process = liveness == "dead";
    let latest_explicit_error = signature.is_some();
    let notify = dead_process && latest_explicit_error;
    let suppressed_reason = match (dead_process, latest_explicit_error) {
        (true, false) => Some("dead_only"),
        (false, true) => Some("error_only"),
        _ => None,
    };
    serde_json::json!({
        "path": agent.rollout_path_display.as_str(),
        "provider": provider_wire(agent.provider),
        "mtime_ns": mtime_ns,
        "size": size,
        "last_offset": size,
        "last_signature": signature,
        "last_liveness": liveness,
        "dead_process": dead_process,
        "process_dead": dead_process,
        "provider_process_dead": dead_process,
        "latest_error": latest_explicit_error,
        "latest_explicit_error": latest_explicit_error,
        "dead_process_and_latest_error": notify,
        "dead_process_and_latest_explicit_error": notify,
        "process_dead_and_latest_explicit_error": notify,
        "provider_process_dead_and_latest_explicit_error": notify,
        "suppressed_reason": suppressed_reason,
        "notification": notify,
        "last_error": error,
        "last_checked_at": chrono::Utc::now().to_rfc3339(),
    })
}

fn upsert_abnormal_watch(state: &mut Value, agent_id: &str, mut payload: Value) {
    let preserved = [
        "last_notified_key",
        "last_notified_at",
        "last_suppressed_key",
        "last_suppressed_at",
        "last_check_key",
        "last_check_at",
    ]
    .into_iter()
    .filter_map(|key| abnormal_watch_field(state, agent_id, key).map(|value| (key, value)))
    .collect::<Vec<_>>();
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        if let Some(payload_obj) = payload.as_object_mut() {
            for (key, value) in preserved {
                payload_obj.insert(key.to_string(), value);
            }
        }
        watch.insert(agent_id.to_string(), payload);
    }
}

fn coordinator_child_object<'a>(
    state: &'a mut Value,
    key: &str,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let state_obj = state.as_object_mut()?;
    let coordinator = state_obj
        .entry("coordinator".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !coordinator.is_object() {
        *coordinator = serde_json::json!({});
    }
    let coord_obj = coordinator.as_object_mut()?;
    let child = coord_obj
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !child.is_object() {
        *child = serde_json::json!({});
    }
    child.as_object_mut()
}

fn abnormal_last_notified_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_notified_key")
}

fn abnormal_last_suppressed_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_suppressed_key")
}

fn abnormal_last_check_key(state: &Value, agent_id: &str) -> Option<String> {
    abnormal_watch_str(state, agent_id, "last_check_key")
}

/// P1: Python `_TAIL_BYTES` parity (idle_takeover_wiring.py:13) — RS must not read less.
const ABNORMAL_TAIL_BYTES: u64 = 131_072;

/// P1: bounded tail read; a partial first line is harmless (the consumer only parses
/// the latest complete JSONL record) and lossy UTF-8 keeps a mid-codepoint seek safe.
fn read_tail_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// P1: the previous tick's `(size, mtime_ns)` pair from the abnormal watch payload.
fn abnormal_watch_stored_metadata(state: &Value, agent_id: &str) -> Option<(u64, u64)> {
    let watch = state
        .get("coordinator")?
        .get("abnormal_exit_watch")?
        .get(agent_id)?;
    Some((
        watch.get("size")?.as_u64()?,
        watch.get("mtime_ns")?.as_u64()?,
    ))
}

fn abnormal_watch_str(state: &Value, agent_id: &str, field: &str) -> Option<String> {
    state
        .get("coordinator")
        .and_then(|v| v.get("abnormal_exit_watch"))
        .and_then(|v| v.get(agent_id))
        .and_then(|v| v.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn abnormal_watch_field(state: &Value, agent_id: &str, field: &str) -> Option<Value> {
    state
        .get("coordinator")
        .and_then(|v| v.get("abnormal_exit_watch"))
        .and_then(|v| v.get(agent_id))
        .and_then(|v| v.get(field))
        .cloned()
}

fn mark_abnormal_notified(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_notified_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_notified_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn mark_abnormal_suppressed(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_suppressed_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_suppressed_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn mark_abnormal_checked(state: &mut Value, agent_id: &str, key: &str) {
    if let Some(watch) = coordinator_child_object(state, "abnormal_exit_watch") {
        let entry = watch
            .entry(agent_id.to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("last_check_key".to_string(), serde_json::json!(key));
            obj.insert(
                "last_check_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
        }
    }
}

fn write_abnormal_check(
    event_log: &EventLog,
    team: &str,
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    fact: Option<&crate::provider::FaultFact>,
    decision: AbnormalExitDecision,
    size: u64,
    mtime_ns: Option<u64>,
) -> Result<(), TickError> {
    let dead_process = liveness.state == ProcessLiveness::Dead;
    let latest_explicit_error = fact.is_some();
    event_log.write(
        "worker.abnormal_exit.check",
        serde_json::json!({
            "team_id": team,
            "agent_id": agent.agent_id.as_str(),
            "provider": provider_wire(agent.provider),
            "path": agent.rollout_path_display.as_str(),
            "size": size,
            "last_offset": size,
            "mtime_ns": mtime_ns,
            "dead_process": dead_process,
            "process_dead": dead_process,
            "provider_process_dead": dead_process,
            "latest_error": latest_explicit_error,
            "latest_explicit_error": latest_explicit_error,
            "dead_process_and_latest_error": dead_process && latest_explicit_error,
            "dead_process_and_latest_explicit_error": dead_process && latest_explicit_error,
            "process_dead_and_latest_explicit_error": dead_process && latest_explicit_error,
            "provider_process_dead_and_latest_explicit_error": dead_process && latest_explicit_error,
            "notification": matches!(decision, AbnormalExitDecision::Notify),
            "suppressed_reason": match decision {
                AbnormalExitDecision::Suppress(reason) => Some(reason),
                AbnormalExitDecision::Notify | AbnormalExitDecision::NoSignal => None,
            },
            "signature": fact.map(|fact| fact.signature.as_str()),
            "turn_id": fact.and_then(|fact| fact.turn_id.as_ref().map(|id| id.as_str())),
            "process_liveness": process_liveness_wire(liveness.state),
            "pid_status": liveness.detail.as_str(),
        }),
    )?;
    Ok(())
}

fn write_abnormal_suppressed(
    event_log: &EventLog,
    team: &str,
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    reason: &str,
) -> Result<(), TickError> {
    event_log.write(
        "abnormal_exit.single_signal_suppressed",
        serde_json::json!({
            "team_id": team,
            "agent_id": agent.agent_id.as_str(),
            "provider": provider_wire(agent.provider),
            "path": agent.rollout_path_display.as_str(),
            "reason": reason,
            "notification": false,
            "dead_process": liveness.state == ProcessLiveness::Dead,
            "process_dead": liveness.state == ProcessLiveness::Dead,
            "provider_process_dead": liveness.state == ProcessLiveness::Dead,
            "latest_error": reason == "error_only",
            "latest_explicit_error": reason == "error_only",
            "dead_process_and_latest_error": false,
            "dead_process_and_latest_explicit_error": false,
            "process_dead_and_latest_explicit_error": false,
            "provider_process_dead_and_latest_explicit_error": false,
            "process_liveness": process_liveness_wire(liveness.state),
            "pid_status": liveness.detail.as_str(),
        }),
    )?;
    Ok(())
}

fn abnormal_dedupe_key(
    agent: &AbnormalWatchAgent,
    fact: &crate::provider::FaultFact,
    size: u64,
) -> String {
    let bucket = fact
        .turn_id
        .as_ref()
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| size.to_string());
    format!(
        "worker.abnormal_exit:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        fact.signature.as_str(),
        bucket
    )
}

fn abnormal_suppression_key(
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    reason: &str,
    size: u64,
) -> String {
    format!(
        "abnormal_exit.single_signal_suppressed:{}:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        reason,
        process_liveness_wire(liveness.state),
        size
    )
}

fn abnormal_check_key(
    agent: &AbnormalWatchAgent,
    liveness: &ProcessCheck,
    fact: Option<&crate::provider::FaultFact>,
    size: u64,
) -> String {
    format!(
        "worker.abnormal_exit.check:{}:{}:{}:{}:{}",
        agent.agent_id,
        agent.rollout_path_display,
        process_liveness_wire(liveness.state),
        fact.map(|fact| fact.signature.as_str()).unwrap_or("-"),
        size
    )
}

fn format_abnormal_exit_message(
    team: &str,
    agent: &AbnormalWatchAgent,
    fact: &crate::provider::FaultFact,
    liveness: &ProcessCheck,
    size: u64,
) -> String {
    let turn_id = fact.turn_id.as_ref().map(|id| id.as_str()).unwrap_or("-");
    format!(
        "Team Agent detected a provider abnormal exit.\n\n\
event: worker.abnormal_exit\n\
team: {team}\n\
node: {node}\n\
provider: {provider}\n\
signature: {signature}\n\
turn_id: {turn_id}\n\
transcript: {path}\n\
last_offset: {size}\n\
pid_status: {pid_status}\n\n\
No automatic restart was performed.",
        node = agent.agent_id.as_str(),
        provider = provider_wire(agent.provider),
        signature = fact.signature.as_str(),
        path = agent.rollout_path_display.as_str(),
        pid_status = liveness.detail.as_str(),
    )
}

fn monotonic_seconds() -> f64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(_) => 0.0,
    }
}

fn parse_provider(raw: &str) -> Option<crate::model::enums::Provider> {
    match raw {
        "claude" => Some(crate::model::enums::Provider::Claude),
        "claude_code" => Some(crate::model::enums::Provider::ClaudeCode),
        "codex" => Some(crate::model::enums::Provider::Codex),
        "copilot" => Some(crate::model::enums::Provider::Copilot),
        "gemini_cli" => Some(crate::model::enums::Provider::GeminiCli),
        "fake" => Some(crate::model::enums::Provider::Fake),
        _ => None,
    }
}

fn capture_window_target(
    agent: &Value,
    session_name: Option<&str>,
) -> Option<(
    crate::transport::SessionName,
    crate::transport::WindowName,
    crate::transport::Target,
)> {
    let window = agent
        .get("window")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let session = session_name.filter(|s| !s.is_empty())?;
    let session = crate::transport::SessionName::new(session);
    let window = crate::transport::WindowName::new(window);
    Some((
        session.clone(),
        window.clone(),
        crate::transport::Target::SessionWindow { session, window },
    ))
}

fn tick_has_work_obligation(store: &crate::message_store::MessageStore) -> bool {
    let Ok(conn) = crate::db::schema::open_db(store.db_path()) else {
        return true;
    };
    let pending: i64 = conn.query_row(
        "select count(*) from messages where status in ('pending', 'accepted', 'target_resolved')",
        [],
        |row| row.get(0),
    ).unwrap_or(1);
    if pending > 0 {
        return true;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let due: i64 = conn
        .query_row(
            "select count(*) from scheduled_events where status = 'pending' and due_at <= ?1",
            [now],
            |row| row.get(0),
        )
        .unwrap_or(1);
    due > 0
}

fn agent_probe_base_eligible(agent: &Value) -> bool {
    let status = agent.get("status").and_then(Value::as_str);
    !matches!(
        status,
        Some("missing" | "stopped" | "dead" | "exited" | "terminated" | "removed" | "failed")
    )
}

fn agent_window_present(
    agent: &Value,
    session: &crate::transport::SessionName,
    window: &crate::transport::WindowName,
    pane_infos: &[crate::transport::PaneInfo],
    windows: &[crate::transport::WindowName],
) -> bool {
    if let Some(pane_id) = agent_pane_id(agent) {
        if pane_infos.iter().any(|info| info.pane_id == pane_id) {
            return true;
        }
    }
    if pane_infos.iter().any(|info| {
        &info.session == session
            && info
                .window_name
                .as_ref()
                .is_some_and(|known_window| known_window == window)
    }) {
        return true;
    }
    if !pane_infos.is_empty() {
        return false;
    }
    windows.is_empty() || windows.iter().any(|known| known == window)
}

fn runtime_approval_target_present(
    target: &crate::transport::Target,
    pane_infos: &[crate::transport::PaneInfo],
    windows: &[crate::transport::WindowName],
) -> bool {
    match target {
        crate::transport::Target::Pane(pane) => {
            if pane_infos.iter().any(|info| &info.pane_id == pane) {
                return true;
            }
            pane_infos.is_empty()
        }
        crate::transport::Target::SessionWindow { session, window } => {
            if pane_infos.iter().any(|info| {
                &info.session == session
                    && info
                        .window_name
                        .as_ref()
                        .is_some_and(|known_window| known_window == window)
            }) {
                return true;
            }
            if !pane_infos.is_empty() {
                return false;
            }
            windows.is_empty() || windows.iter().any(|known| known == window)
        }
    }
}

fn agent_process_epoch(agent: &Value) -> String {
    if let Some(pid) = agent.get("pane_pid").and_then(Value::as_u64) {
        return format!("pane_pid:{pid}");
    }
    if let Some(spawned_at) = agent.get("spawned_at").and_then(Value::as_str) {
        return format!("spawned_at:{spawned_at}");
    }
    if let Some(pane_id) = agent.get("pane_id").and_then(Value::as_str) {
        return format!("pane:{pane_id}");
    }
    agent
        .get("window")
        .and_then(Value::as_str)
        .map(|window| format!("window:{window}"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn startup_probe_disabled_for_epoch(agent: &Value) -> bool {
    let epoch = agent_process_epoch(agent);
    agent.get("startup_prompt_status").and_then(Value::as_str) == Some("disabled_for_epoch")
        && agent
            .get("startup_prompt_probe_epoch")
            .and_then(Value::as_str)
            == Some(epoch.as_str())
}

fn clear_startup_probe_disable_if_epoch_changed(agent: &mut Value) {
    if agent.get("startup_prompt_status").and_then(Value::as_str) != Some("disabled_for_epoch") {
        return;
    }
    let epoch = agent_process_epoch(agent);
    if agent
        .get("startup_prompt_probe_epoch")
        .and_then(Value::as_str)
        == Some(epoch.as_str())
    {
        return;
    }
    if let Some(agent) = agent.as_object_mut() {
        agent.remove("startup_prompt_status");
        agent.remove("startup_prompts");
        agent.remove("startup_prompt_probe_disabled_at");
    }
}

fn startup_probe_within_grace(agent: &Value) -> bool {
    let Some(spawned_at) = agent.get("spawned_at").and_then(Value::as_str) else {
        return true;
    };
    let Some(spawned_at) = parse_rfc3339_utc(spawned_at) else {
        return true;
    };
    chrono::Utc::now()
        .signed_duration_since(spawned_at)
        .num_seconds()
        <= STARTUP_PROMPT_GRACE_SECS
}

fn disable_startup_probe_for_epoch(agent: &mut Value) {
    let epoch = agent_process_epoch(agent);
    if let Some(agent) = agent.as_object_mut() {
        agent.insert(
            "startup_prompt_status".to_string(),
            serde_json::json!("disabled_for_epoch"),
        );
        agent.insert(
            "startup_prompts".to_string(),
            serde_json::json!("disabled_for_epoch"),
        );
        agent.insert(
            "startup_prompt_probe_epoch".to_string(),
            serde_json::json!(epoch),
        );
        agent.insert(
            "startup_prompt_probe_disabled_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
    }
}

fn runtime_approval_backoff_active(agent: &Value) -> bool {
    let Some(next) = agent
        .pointer("/runtime_approval_probe/next_probe_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
    else {
        return false;
    };
    next > chrono::Utc::now()
}

fn remember_runtime_approval_backoff(agent: &mut Value) {
    let previous = agent
        .pointer("/runtime_approval_probe/backoff_secs")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let backoff = if previous <= 0 {
        RUNTIME_APPROVAL_INITIAL_BACKOFF_SECS
    } else {
        previous
            .saturating_mul(2)
            .min(RUNTIME_APPROVAL_MAX_BACKOFF_SECS)
    };
    let next = chrono::Utc::now() + chrono::Duration::seconds(backoff);
    if let Some(agent) = agent.as_object_mut() {
        agent.insert(
            "runtime_approval_probe".to_string(),
            serde_json::json!({
                "backoff_secs": backoff,
                "next_probe_at": next.to_rfc3339(),
            }),
        );
    }
}

fn clear_runtime_approval_backoff(agent: &mut Value) {
    if let Some(agent) = agent.as_object_mut() {
        agent.remove("runtime_approval_probe");
    }
}

fn warm_idle_capture_suppressed(agent: &Value, has_work_obligation: bool) -> bool {
    if has_work_obligation {
        return false;
    }
    let status = agent
        .pointer("/activity/status")
        .and_then(Value::as_str)
        .or_else(|| agent.get("status").and_then(Value::as_str));
    if status != Some("idle") {
        return false;
    }
    if runtime_approval_backoff_active(agent) {
        return true;
    }
    agent
        .get("coordinator_idle_capture_next_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .is_some_and(|next| next > chrono::Utc::now())
}

fn remember_idle_capture_schedule(agent: &mut Value, activity: &crate::messaging::AgentActivity) {
    if activity.status != crate::messaging::ActivityStatus::Idle {
        return;
    }
    if let Some(agent) = agent.as_object_mut() {
        let next =
            chrono::Utc::now() + chrono::Duration::seconds(IDLE_HEALTH_CAPTURE_INTERVAL_SECS);
        agent.insert(
            "coordinator_idle_capture_next_at".to_string(),
            serde_json::json!(next.to_rfc3339()),
        );
    }
}

fn parse_rfc3339_utc(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
}

fn matching_capture_pane_info(
    agent: &Value,
    session: &crate::transport::SessionName,
    window: &crate::transport::WindowName,
    pane_infos: &[crate::transport::PaneInfo],
) -> Option<crate::transport::PaneInfo> {
    if let Some(pane_id) = agent_pane_id(agent) {
        if let Some(info) = pane_infos.iter().find(|info| info.pane_id == pane_id) {
            return Some(info.clone());
        }
    }
    pane_infos
        .iter()
        .find(|info| {
            &info.session == session
                && info
                    .window_name
                    .as_ref()
                    .is_some_and(|known_window| known_window == window)
        })
        .cloned()
}

fn agent_pane_id(agent: &Value) -> Option<crate::transport::PaneId> {
    agent
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|pane_id| !pane_id.is_empty())
        .map(crate::transport::PaneId::new)
}

fn agent_rollout_path(agent: &Value) -> Option<PathBuf> {
    ["rollout_path", "transcript_path", "session_log_path"]
        .into_iter()
        .find_map(|key| agent.get(key).and_then(Value::as_str))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Memory-growth fix (architect §5): bounded tail cap for JSONL activity reads.
/// Matches `ABNORMAL_TAIL_BYTES` (131_072 bytes) — the abnormal-exit path
/// already proved this size is sufficient to capture the latest lifecycle
/// records across all providers (claude / codex / copilot).
const JSONL_ACTIVITY_TAIL_BYTES: u64 = 131_072;

/// Memory-growth fix (architect §5): per-process `(path, size, mtime_ns) →
/// activity` cache. When a rollout file hasn't changed since the previous
/// tick, we skip the file read AND the classification entirely. This is the
/// dominant savings: a 538MB Claude transcript that updates every few seconds
/// is touched only when its size or mtime actually moves. Stored values are
/// small (Option<AgentActivity> = enum + short rationale string); we never
/// cache the transcript text or parsed JSON.
struct JsonlActivityCacheEntry {
    size: u64,
    mtime_ns: u64,
    activity: Option<crate::messaging::AgentActivity>,
}

fn jsonl_activity_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, JsonlActivityCacheEntry>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, JsonlActivityCacheEntry>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// E47 (0.3.24 P0, idle/busy 假阳): consult the authoritative provider JSONL
/// classifier and map to neutral `AgentActivity`. Returns `None` when the
/// classifier reports `TurnState::Unknown` (unreadable JSONL / no lifecycle
/// fact yet) so the caller falls back to the TUI scan — this honours the
/// IRON LAW (activity.rs:3 / bug-071/077/085): no-signal = Uncertain (not
/// silently coerced to Idle); but here Unknown means "JSONL gave no signal",
/// so we hand off to the TUI scanner which has its OWN no-signal → Uncertain
/// path. Copilot/Gemini/Fake providers (which don't have JSONL — classify.rs
/// returns Unknown for them) thus keep using TUI scanning unchanged.
///
/// Memory-growth fix (architect analysis 2026-06-23): bounded tail read +
/// metadata cache. Pre-fix `std::fs::read_to_string` on a 538MB Claude
/// transcript every 5s caused 200MB+ coordinator RSS plateaus from allocator
/// fragmentation. Now bounded to 128KiB tail and skipped entirely when
/// (size, mtime_ns) is unchanged.
fn jsonl_activity_for_agent(agent: &Value) -> Option<crate::messaging::AgentActivity> {
    let rollout_path = agent_rollout_path(agent)?;
    let provider = agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(parse_provider)?;

    // Metadata check + cache lookup. Cache hit when the rollout file has
    // not changed since the previous tick: return the cached classification
    // without re-reading the file. Truncation (size shrink with stable mtime)
    // still forces re-read because size is part of the cache key.
    let metadata = std::fs::metadata(&rollout_path).ok()?;
    let size = metadata.len();
    let mtime_ns = metadata_mtime_ns(&metadata)?;
    if let Ok(cache) = jsonl_activity_cache().lock() {
        if let Some(entry) = cache.get(&rollout_path) {
            if entry.size == size && entry.mtime_ns == mtime_ns {
                return entry.activity.clone();
            }
        }
    }

    // Cache miss: bounded tail read + classify. The classifier only needs the
    // latest lifecycle records to determine idle/working state; the
    // abnormal-exit path uses the same 128KiB tail and is sufficient for
    // claude / codex / copilot lifecycle markers.
    let log_text = read_tail_text(&rollout_path, JSONL_ACTIVITY_TAIL_BYTES).ok()?;
    let process = explicit_process_liveness(agent).unwrap_or(ProcessLiveness::Unverifiable);
    let activity = crate::provider::classify(provider, &log_text, process, 0.0)
        .ok()
        .and_then(|result| {
            use crate::messaging::{ActivityStatus, AgentActivity};
            use crate::provider::types::TurnState;
            let status = match result.state {
                TurnState::Idle => ActivityStatus::Idle,
                TurnState::IdleInterrupted => ActivityStatus::Idle,
                TurnState::Working => ActivityStatus::Working,
                TurnState::BlockedOnHuman | TurnState::Abnormal => ActivityStatus::Uncertain,
                TurnState::Unknown => return None,
            };
            Some(AgentActivity {
                status,
                confidence: 0.95,
                rationale: format!("provider_jsonl:{}", result.reason),
            })
        });

    // Store the classification (including None / Unknown) so the next tick
    // can short-circuit when the file is unchanged.
    if let Ok(mut cache) = jsonl_activity_cache().lock() {
        cache.insert(
            rollout_path,
            JsonlActivityCacheEntry {
                size,
                mtime_ns,
                activity: activity.clone(),
            },
        );
    }
    activity
}

fn runtime_approval_target(
    agent: &Value,
    session_name: Option<&str>,
) -> Option<crate::transport::Target> {
    if let Some(pane_id) = agent
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|pane_id| !pane_id.is_empty())
    {
        return Some(crate::transport::Target::Pane(
            crate::transport::PaneId::new(pane_id),
        ));
    }
    capture_window_target(agent, session_name).map(|(_, _, target)| target)
}

fn runtime_approval_key(raw: String) -> Option<crate::transport::Key> {
    match raw.as_str() {
        "Enter" => Some(crate::transport::Key::Enter),
        "Up" => Some(crate::transport::Key::Up),
        "Down" => Some(crate::transport::Key::Down),
        "Left" => Some(crate::transport::Key::Left),
        "Right" => Some(crate::transport::Key::Right),
        other => {
            let mut chars = other.chars();
            let ch = chars.next()?;
            if chars.next().is_none() {
                Some(crate::transport::Key::Char(ch))
            } else {
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeApprovalPolicy {
    enabled: bool,
    source: String,
    inherited: bool,
    explicit_yes_confirmed: bool,
    provider: Option<String>,
    flag: Option<String>,
    worker_capability_above_leader: bool,
}

impl RuntimeApprovalPolicy {
    fn auto_answer_allowed(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let source_allows = match self.source.as_str() {
            "leader_process" => self.inherited,
            "runtime_config" => self.explicit_yes_confirmed,
            _ => false,
        };
        source_allows
            && (!self.worker_capability_above_leader
                || (self.source == "runtime_config" && self.explicit_yes_confirmed))
    }

    fn inherit_reason(&self) -> &'static str {
        match self.source.as_str() {
            "leader_process" if self.inherited => "leader_bypass",
            "runtime_config" if self.explicit_yes_confirmed => "runtime_config_explicit_yes",
            _ => "none",
        }
    }
}

fn runtime_approval_policy_from_agent(agent: &Value) -> RuntimeApprovalPolicy {
    let policy = agent
        .get("effective_approval_policy")
        .and_then(Value::as_object);
    RuntimeApprovalPolicy {
        enabled: policy
            .and_then(|p| p.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source: policy
            .and_then(|p| p.get("source"))
            .and_then(Value::as_str)
            .unwrap_or("disabled")
            .to_string(),
        inherited: policy
            .and_then(|p| p.get("inherited"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        explicit_yes_confirmed: policy
            .and_then(|p| p.get("explicit_yes_confirmed"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        provider: policy
            .and_then(|p| p.get("provider"))
            .and_then(Value::as_str)
            .map(str::to_string),
        flag: policy
            .and_then(|p| p.get("flag"))
            .and_then(Value::as_str)
            .map(str::to_string),
        worker_capability_above_leader: policy
            .and_then(|p| p.get("worker_capability_above_leader"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn awaiting_human_confirm_payload(
    agent: &Value,
    fact: &crate::provider::AwaitingHumanConfirmFact,
) -> Value {
    let mut payload = fact.to_event_payload();
    let excerpt = fact
        .prompt
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(240)
        .collect::<String>();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("team_id".to_string(), serde_json::json!(fact.team));
        obj.insert("owner_team_id".to_string(), serde_json::json!(fact.team));
        if let Some(provider) = agent.get("provider").and_then(Value::as_str) {
            obj.insert("provider".to_string(), serde_json::json!(provider));
        }
        if let Some(pane_id) = agent.get("pane_id").and_then(Value::as_str) {
            obj.insert("pane_id".to_string(), serde_json::json!(pane_id));
        }
        obj.insert("excerpt".to_string(), serde_json::json!(excerpt));
    }
    payload
}

enum AwaitingDedupUpdate {
    Remember(crate::provider::AwaitingHumanConfirmFact),
    Clear { team: String, agent_id: String },
}

fn state_awaiting_human_confirm_fingerprint(
    state: &Value,
    team: &str,
    agent_id: &str,
) -> Option<String> {
    state
        .get("coordinator")
        .and_then(|coordinator| {
            coordinator
                .get("awaiting_human_confirm_seen")
                .or_else(|| coordinator.get("awaiting_human_confirm"))
        })
        .and_then(|by_team| by_team.get(team))
        .and_then(|by_agent| by_agent.get(agent_id))
        .and_then(|record| record.get("fingerprint"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn remember_state_awaiting_human_confirm(
    state: &mut Value,
    fact: &crate::provider::AwaitingHumanConfirmFact,
) {
    let Some(state_obj) = state.as_object_mut() else {
        return;
    };
    let coordinator = state_obj
        .entry("coordinator".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !coordinator.is_object() {
        *coordinator = serde_json::json!({});
    }
    let Some(coord_obj) = coordinator.as_object_mut() else {
        return;
    };
    let awaiting = coord_obj
        .entry("awaiting_human_confirm_seen".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !awaiting.is_object() {
        *awaiting = serde_json::json!({});
    }
    let Some(awaiting_obj) = awaiting.as_object_mut() else {
        return;
    };
    let team_entry = awaiting_obj
        .entry(fact.team.clone())
        .or_insert_with(|| serde_json::json!({}));
    if !team_entry.is_object() {
        *team_entry = serde_json::json!({});
    }
    if let Some(team_obj) = team_entry.as_object_mut() {
        team_obj.insert(
            fact.agent_id.clone(),
            serde_json::json!({
                "team": fact.team,
                "team_id": fact.team,
                "owner_team_id": fact.team,
                "agent_id": fact.agent_id,
                "fingerprint": fact.fingerprint,
                "dedupe_key": fact.dedupe_key,
                "prompt_kind": fact.prompt_kind,
                "reason": fact.reason,
            }),
        );
    }
}

fn clear_state_awaiting_human_confirm(state: &mut Value, team: &str, agent_id: &str) {
    let Some(awaiting_obj) = state
        .get_mut("coordinator")
        .and_then(|coordinator| coordinator.get_mut("awaiting_human_confirm_seen"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let remove_team = if let Some(team_value) = awaiting_obj.get_mut(team) {
        if let Some(team_obj) = team_value.as_object_mut() {
            team_obj.remove(agent_id);
            team_obj.is_empty()
        } else {
            true
        }
    } else {
        false
    };
    if remove_team {
        awaiting_obj.remove(team);
    }
}

fn remember_awaiting_human_confirm(
    agent: &mut Value,
    fact: &crate::provider::AwaitingHumanConfirmFact,
) {
    if let Some(agent_obj) = agent.as_object_mut() {
        agent_obj.insert(
            "awaiting_human_confirm".to_string(),
            serde_json::json!({
                "team": fact.team,
                "team_id": fact.team,
                "owner_team_id": fact.team,
                "agent_id": fact.agent_id,
                "fingerprint": fact.fingerprint,
                "dedupe_key": fact.dedupe_key,
                "prompt_kind": fact.prompt_kind,
                "reason": fact.reason,
            }),
        );
    }
}

fn clear_awaiting_human_confirm(agent: &mut Value) {
    if let Some(agent_obj) = agent.as_object_mut() {
        agent_obj.remove("awaiting_human_confirm");
    }
}

/// Python approvals/status.py:68-72 — sha256 the scrollback, compare to the stored
/// `last_output_hash`; only a CHANGED digest counts as advanced output (and stores
/// the new digest).
fn scrollback_digest_advanced(agent: &mut Value, text: &str) -> bool {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(text.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let unchanged = agent
        .get("last_output_hash")
        .and_then(Value::as_str)
        .is_some_and(|stored| stored == digest);
    if unchanged {
        return false;
    }
    if let Some(obj) = agent.as_object_mut() {
        obj.insert("last_output_hash".to_string(), serde_json::json!(digest));
    }
    true
}

fn write_activity(
    agent: &mut Value,
    activity: &crate::messaging::AgentActivity,
    output_advanced: bool,
) -> Option<String> {
    let previous_last_output = agent
        .get("last_output_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(agent_obj) = agent.as_object_mut() else {
        return previous_last_output;
    };
    let status = activity_status_wire(activity.status);
    agent_obj.insert(
        "activity".to_string(),
        serde_json::json!({
            "status": status,
            "confidence": activity.confidence,
            "rationale": activity.rationale,
        }),
    );
    if output_advanced {
        let last_output_at = chrono::Utc::now().to_rfc3339();
        agent_obj.insert(
            "last_output_at".to_string(),
            serde_json::json!(last_output_at.clone()),
        );
        return Some(last_output_at);
    }
    previous_last_output
}

fn activity_status_wire(status: crate::messaging::ActivityStatus) -> &'static str {
    match status {
        crate::messaging::ActivityStatus::Idle => "idle",
        crate::messaging::ActivityStatus::Working => "working",
        crate::messaging::ActivityStatus::Stuck => "stuck",
        crate::messaging::ActivityStatus::Uncertain => "uncertain",
    }
}

fn agent_health_status_wire(status: crate::messaging::ActivityStatus) -> &'static str {
    match status {
        crate::messaging::ActivityStatus::Idle => "IDLE",
        crate::messaging::ActivityStatus::Working => "WORKING",
        crate::messaging::ActivityStatus::Stuck => "STUCK",
        crate::messaging::ActivityStatus::Uncertain => "UNKNOWN",
    }
}

fn write_agent_health(
    store: &crate::message_store::MessageStore,
    team: &str,
    agent_id: &str,
    agent: &Value,
    activity: &crate::messaging::AgentActivity,
    last_output_at: Option<&str>,
) -> Result<(), crate::messaging::MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let status = agent_health_status_wire(activity.status);
    let updated_at = chrono::Utc::now().to_rfc3339();
    let context_usage_pct = agent
        .get("context_usage_pct")
        .or_else(|| agent.get("context_usage_percent"))
        .and_then(Value::as_i64);
    let current_task_id = agent
        .get("current_task_id")
        .or_else(|| agent.get("task_id"))
        .and_then(Value::as_str);
    conn.execute(
        "insert into agent_health(
             owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         on conflict(owner_team_id, agent_id) do update set
             status = excluded.status,
             last_output_at = coalesce(excluded.last_output_at, agent_health.last_output_at),
             context_usage_pct = excluded.context_usage_pct,
             current_task_id = excluded.current_task_id,
             updated_at = excluded.updated_at",
        rusqlite::params![
            team,
            agent_id,
            status,
            last_output_at,
            context_usage_pct,
            current_task_id,
            updated_at,
        ],
    )?;
    Ok(())
}

fn read_pid_file(path: &Path) -> Option<Pid> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = text.trim().parse::<u32>().ok()?;
    Some(Pid(pid))
}

fn remove_file_if_exists(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn notify_session_missing(
    workspace: &Path,
    state: &Value,
    event_log: &EventLog,
    session_name: &str,
) -> Result<(), TickError> {
    let content = format!(
        "coordinator.session_missing\nerror: tmux session {session_name} is missing; coordinator is stopping\naction: restart the team or recover the missing tmux session\nlog: .team/logs/events.jsonl"
    );
    let dedupe_key = format!("coordinator.session_missing:{session_name}");
    match crate::messaging::send_to_leader_receiver(
        workspace,
        state,
        "leader",
        &content,
        None,
        "coordinator",
        false,
        Some(&dedupe_key),
        event_log,
    ) {
        Ok(outcome) => {
            event_log.write(
                "coordinator.session_missing_alert",
                serde_json::json!({
                    "session": session_name,
                    "leader_notification_status": crate::messaging::helpers::status_wire(outcome.status),
                    "message_id": outcome.message_id,
                }),
            )?;
        }
        Err(error) => {
            event_log.write(
                "coordinator.session_missing_alert_failed",
                serde_json::json!({
                    "session": session_name,
                    "error": error.to_string(),
                    "action": "inspect .team/logs/events.jsonl and restart the team",
                }),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod u1_tests {
    use super::*;

    struct CaptureFailureRegistry;

    impl ProviderRegistry for CaptureFailureRegistry {
        fn adapter_for(
            &self,
            provider: crate::provider::Provider,
        ) -> Box<dyn crate::provider::ProviderAdapter> {
            Box::new(
                crate::session_capture::test_support::CaptureCandidatesAdapter::new(
                    provider,
                    Some("w1"),
                    "capture exploded",
                ),
            )
        }

        fn error_lists(
            &self,
            _provider: crate::provider::Provider,
        ) -> super::super::types::ErrorLists {
            super::super::types::ErrorLists::default()
        }
    }

    #[test]
    fn tick_logs_capture_missing_failure_and_continues() {
        let dir = std::env::temp_dir().join(format!(
            "team-agent-u1-capture-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        crate::state::persist::save_runtime_state(
            &dir,
            &serde_json::json!({
                "agents": {
                    "w1": {
                        "provider": "codex",
                        "status": "running",
                        "spawn_cwd": dir.to_string_lossy()
                    }
                }
            }),
        )
        .unwrap();
        let coordinator = Coordinator::for_test(
            WorkspacePath::new(dir.clone()),
            Box::new(CaptureFailureRegistry),
            Box::new(crate::transport::test_support::OfflineTransport::new()),
            None,
            None,
        );

        let report = coordinator
            .tick()
            .expect("capture_missing failure must be logged and not abort the tick");

        assert!(report.ok, "tick should continue to a successful report");
        let events_path = crate::model::paths::logs_dir(&dir).join("events.jsonl");
        let events = std::fs::read_to_string(events_path).unwrap();
        let has_capture_failure = events.lines().any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|event| {
                    event
                        .get("event")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .as_deref()
                == Some("coordinator.tick.capture_missing_failed")
        });
        assert!(
            has_capture_failure,
            "capture_missing failure must be visible in events.jsonl; got {events}"
        );
    }
}
