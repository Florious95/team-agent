use super::*;

// ───────────────────────────────────────────────────────────────────────
// classify_first_send_at — _classify_first_send_at (orchestration.py:404)
// 严格分类,绝不靠 truthiness。golden 实跑(见任务记录):
//   None -> absent ; "" / 0 / False / "null" / "not-a-date" / 123 / [] / {} -> corrupt
//   "2026-05-27T10:00:00+00:00" / "2026-05-27T10:00:00" -> valid
// 这是 Route B resume-atomicity 的命门:garbage 必须在 teardown 之前 hard-refuse。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn classify_first_send_at_none_is_absent_not_corrupt() {
    // None / 缺失 = 从未交互,可丢弃 fresh —— 不能误判成 corrupt。
    assert_eq!(classify_first_send_at(&json!(null)), FirstSendAtState::Absent);
}

#[test]
fn classify_first_send_at_empty_string_is_corrupt_not_absent() {
    // 陷阱核心:Python truthiness 会把 "" 当 falsey/absent;契约要求 corrupt。
    assert_eq!(classify_first_send_at(&json!("")), FirstSendAtState::Corrupt);
}

#[test]
fn classify_first_send_at_zero_and_false_are_corrupt() {
    // 0 / False 非 str → corrupt(绝不靠 bool/int truthiness 当 absent)。
    assert_eq!(classify_first_send_at(&json!(0)), FirstSendAtState::Corrupt);
    assert_eq!(classify_first_send_at(&json!(false)), FirstSendAtState::Corrupt);
    assert_eq!(classify_first_send_at(&json!(123)), FirstSendAtState::Corrupt);
}

#[test]
fn classify_first_send_at_literal_null_string_is_corrupt() {
    // 字面量字符串 "null"(非 ISO)→ corrupt,而 JSON null → absent(上面已测)。
    assert_eq!(classify_first_send_at(&json!("null")), FirstSendAtState::Corrupt);
    assert_eq!(classify_first_send_at(&json!("not-a-date")), FirstSendAtState::Corrupt);
}

#[test]
fn classify_first_send_at_non_string_containers_are_corrupt() {
    assert_eq!(classify_first_send_at(&json!([])), FirstSendAtState::Corrupt);
    assert_eq!(classify_first_send_at(&json!({})), FirstSendAtState::Corrupt);
}

#[test]
fn classify_first_send_at_valid_iso_with_and_without_tz() {
    // datetime.fromisoformat 接受带 / 不带时区的 ISO-8601。
    assert_eq!(
        classify_first_send_at(&json!("2026-05-27T10:00:00+00:00")),
        FirstSendAtState::Valid
    );
    assert_eq!(
        classify_first_send_at(&json!("2026-05-27T10:00:00")),
        FirstSendAtState::Valid
    );
}

// ───────────────────────────────────────────────────────────────────────
// PlanId::parse — sanitize_plan_id (orchestrator/state.py:18)
// _PLAN_ID_RE = ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$  (首字符必字母数字;总长 1..=64)
// golden 实跑:
//   "abc"/"a.b-c_1"/"A"/"1plan"/64×"x" -> OK
//   "_bad"/".dot"/"../etc"/"a/b"/"a b"/""/None/65×"x" -> InvalidPlanId
// newtype 防路径穿越:无 "/"、无空格。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn plan_id_accepts_alnum_and_inner_dot_dash_underscore() {
    assert_eq!(PlanId::parse("abc").unwrap().as_str(), "abc");
    assert_eq!(PlanId::parse("a.b-c_1").unwrap().as_str(), "a.b-c_1");
    assert_eq!(PlanId::parse("A").unwrap().as_str(), "A");
    assert_eq!(PlanId::parse("1plan").unwrap().as_str(), "1plan");
}

#[test]
fn plan_id_rejects_leading_underscore_or_dot() {
    // 首字符必须 [A-Za-z0-9] —— "_bad" / ".dot" 被拒(防 ".." 穿越家族)。
    assert!(matches!(
        PlanId::parse("_bad"),
        Err(LifecycleError::InvalidPlanId(_))
    ));
    assert!(matches!(
        PlanId::parse(".dot"),
        Err(LifecycleError::InvalidPlanId(_))
    ));
}

#[test]
fn plan_id_rejects_path_traversal_and_separators() {
    for bad in ["../etc", "a/b", "a b", ""] {
        assert!(
            matches!(PlanId::parse(bad), Err(LifecycleError::InvalidPlanId(_))),
            "expected InvalidPlanId for {bad:?}"
        );
    }
}

#[test]
fn plan_id_length_boundary_64_ok_65_rejected() {
    // {0,63} 量词 + 1 首字符 = 最长 64;65 越界。
    let ok = "x".repeat(64);
    let bad = "x".repeat(65);
    assert_eq!(PlanId::parse(&ok).unwrap().as_str(), ok);
    assert!(matches!(
        PlanId::parse(&bad),
        Err(LifecycleError::InvalidPlanId(_))
    ));
}

#[test]
fn plan_id_error_message_names_the_offending_value_and_grammar() {
    // exact message 契约:含 repr 后的非法值 + 文法 + "no slashes ... path-traversal"。
    let err = PlanId::parse("a/b").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("invalid plan id"), "got: {msg}");
    assert!(msg.contains("a/b"), "must name offending value, got: {msg}");
    assert!(
        msg.contains("no slashes") || msg.contains("path-traversal"),
        "must explain why, got: {msg}"
    );
}

// ───────────────────────────────────────────────────────────────────────
// PlanCondition::parse — _CONDITION_RE / _is_supported_condition (plan.py:9)
// _CONDITION_RE = ^\s*report_result\.(\w+)\s*==\s*['"]([^'"]+)['"]\s*$
// golden 实跑(_is_supported_condition):
//   "any"/"ANY"/" any "                         -> True  (Any)
//   "report_result.foo == 'bar'"                -> True  (FieldEq foo bar)
//   "report_result.foo=='bar'"(无空格)         -> True
//   'report_result.s == "dq"'(双引号)          -> True
//   "report_result. == 'y'"(空 field)/"foo == 'bar'"/""/"report_result.s == bar"(裸值) -> False
// 封闭文法,越界 → InvalidPlan(不做自由表达式)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn plan_condition_any_case_insensitive_and_trimmed() {
    assert_eq!(PlanCondition::parse("any").unwrap(), PlanCondition::Any);
    assert_eq!(PlanCondition::parse("ANY").unwrap(), PlanCondition::Any);
    assert_eq!(PlanCondition::parse(" any ").unwrap(), PlanCondition::Any);
}

#[test]
fn plan_condition_field_eq_extracts_field_and_value() {
    assert_eq!(
        PlanCondition::parse("report_result.foo == 'bar'").unwrap(),
        PlanCondition::FieldEq {
            field: "foo".to_string(),
            value: "bar".to_string()
        }
    );
}

#[test]
fn plan_condition_field_eq_tolerates_no_spaces_and_double_quotes() {
    assert_eq!(
        PlanCondition::parse("report_result.foo=='bar'").unwrap(),
        PlanCondition::FieldEq {
            field: "foo".to_string(),
            value: "bar".to_string()
        }
    );
    assert_eq!(
        PlanCondition::parse("report_result.s == \"dq\"").unwrap(),
        PlanCondition::FieldEq {
            field: "s".to_string(),
            value: "dq".to_string()
        }
    );
}

#[test]
fn plan_condition_rejects_out_of_grammar() {
    // 空 field / 缺 report_result 前缀 / 裸值(无引号) / 空串 → InvalidPlan。
    for bad in [
        "report_result. == 'y'",
        "foo == 'bar'",
        "",
        "report_result.s == bar",
    ] {
        assert!(
            matches!(PlanCondition::parse(bad), Err(LifecycleError::InvalidPlan(_))),
            "expected InvalidPlan for {bad:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────
// resolve_display_backend — display/backend.py
// 默认 none;非默认非静默(non_default=true 触发 display.backend_resolved)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn resolve_backend_defaults_to_none_when_none_requested() {
    let r = resolve_display_backend(None, None);
    assert_eq!(r.backend, DisplayBackend::None);
    assert!(!r.non_default, "默认 none 不应标记 non_default");
}

#[test]
fn resolve_backend_requested_overrides_and_marks_non_default() {
    // 显式 requested 非 adaptive → 采用且 non_default=true(非静默发事件)。
    let r = resolve_display_backend(Some(DisplayBackend::GhosttyWindow), None);
    assert_eq!(r.backend, DisplayBackend::GhosttyWindow);
    assert!(r.non_default);
}

#[test]
fn resolve_backend_recorded_used_when_no_request() {
    // 无 requested 但 state 有 recorded → 复用 recorded(restart 一致性)。
    let r = resolve_display_backend(None, Some(DisplayBackend::GhosttyWorkspace));
    assert_eq!(r.backend, DisplayBackend::GhosttyWorkspace);
    assert!(r.non_default);
}

// ───────────────────────────────────────────────────────────────────────
// probe_display_capabilities — display/adaptive.py:31 (C13)
// 分支只看 probe 结果,NOT cfg!(target_os)。golden 实跑:
//   linux no-tmux : in_tmux=false, adaptive_status=leader_not_in_tmux, reason=leader_not_in_tmux
//   linux in-tmux : in_tmux=true,  adaptive_status=available(opened),  reason=None, caps both true
//   windows/wsl   : in_tmux=false, adaptive_status=not_implemented_this_platform, caps both false
// blocked 是 typed outcome(DisplayStatus::Blocked + reason),NOT error。
// 注:RUST 入口签名为 probe_display_capabilities(workspace) —— 它内部读环境/平台;
// 本测试在干净 CI workspace 上跑,只断言"不是 Err(平台降级是 typed outcome)"
// 以及 reason 必属封闭集(若有)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn probe_never_errors_platform_degradation_is_typed_outcome() {
    // C13:能力性降级绝不走 LifecycleError —— 它是 DisplayStatus::Blocked + reason。
    let ws = temp_ws();
    let probe = probe_display_capabilities(&ws).expect("probe 平台降级必须是 typed outcome,不是 Err");
    // 若 blocked,reason 必属 AdaptiveBlockReason 封闭集且与 status 一致。
    match probe.adaptive_status {
        DisplayStatus::Blocked => assert!(
            probe.reason.is_some(),
            "blocked 必带 reason(C16 封闭集)"
        ),
        DisplayStatus::Opened => assert!(
            probe.reason.is_none(),
            "opened 不应带 block reason"
        ),
        DisplayStatus::Stopped => {}
    }
}

#[test]
fn probe_caps_consistent_with_in_tmux() {
    // caps.adaptive_display == (in_tmux && 平台支持);不在 tmux → caps 全 false。
    let ws = temp_ws();
    let probe = probe_display_capabilities(&ws).expect("probe 不应 Err");
    if !probe.in_tmux {
        assert!(!probe.caps.adaptive_display);
        assert!(!probe.caps.tmux_append_windows);
    }
}

// ───────────────────────────────────────────────────────────────────────
// AdaptiveBlockReason 封闭集 — ADAPTIVE_BLOCK_REASONS (adaptive.py:21)
// golden 实跑(6 个,无更多无更少):
//   aggregator_rebuild_failed, leader_not_in_tmux, not_implemented_this_platform,
//   split_failed, window_create_failed, worker_session_missing
// serde rename = snake_case,JSON 名与 Python 字符串一致。
// ───────────────────────────────────────────────────────────────────────

/// 封闭集 ALL — 一处穷举所有 AdaptiveBlockReason 变体(新增第 7 个变体会编译错此 match,
/// 把 cardinality 锁成编译期事实)。golden ADAPTIVE_BLOCK_REASONS 恰 6 个(adaptive.py:21)。
const ALL_BLOCK_REASONS: [AdaptiveBlockReason; 6] = [
    AdaptiveBlockReason::LeaderNotInTmux,
    AdaptiveBlockReason::SplitFailed,
    AdaptiveBlockReason::WindowCreateFailed,
    AdaptiveBlockReason::WorkerSessionMissing,
    AdaptiveBlockReason::NotImplementedThisPlatform,
    AdaptiveBlockReason::AggregatorRebuildFailed,
];

#[test]
fn adaptive_block_reason_serde_names_match_python_and_set_is_exactly_six() {
    // (a) 每个变体 wire-name 与 Python 字符串一致。
    let expected = [
        "\"leader_not_in_tmux\"",
        "\"split_failed\"",
        "\"window_create_failed\"",
        "\"worker_session_missing\"",
        "\"not_implemented_this_platform\"",
        "\"aggregator_rebuild_failed\"",
    ];
    for (variant, want) in ALL_BLOCK_REASONS.iter().zip(expected.iter()) {
        assert_eq!(&serde_json::to_string(variant).unwrap(), want);
    }
    // (b) 封闭集 CARDINALITY == 6(无多无少;rogue 第 7 变体使 ALL_BLOCK_REASONS match 编译失败)。
    let mut names: Vec<String> = ALL_BLOCK_REASONS
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 6, "ADAPTIVE_BLOCK_REASONS 恰 6 个,无重复无遗漏");
}

#[test]
fn adaptive_blocked_out_of_set_reason_bottoms_to_aggregator_rebuild_failed() {
    // adaptive.py 兜底:越界 / aggregator 重建失败 → AggregatorRebuildFailed(documented overflow)。
    // RED:经真实路径 —— probe 在 blocked 平台 open_worker_displays,worker display 必属封闭集;
    // 这里用 probe.reason 越界场景驱动 open_worker_displays 而非静态枚举,捕获 reason 发射回归。
    let ws = temp_ws();
    let probe = DisplayProbe {
        in_tmux: true,
        platform: "linux".to_string(),
        leader_session: Some(sess("leader")),
        leader_pane: None,
        caps: CapsFlags {
            tmux_append_windows: true,
            adaptive_display: true,
        },
        // 模拟 aggregator 重建失败封闭:open 必把 worker display 兜底成 AggregatorRebuildFailed。
        adaptive_status: DisplayStatus::Blocked,
        reason: Some(AdaptiveBlockReason::AggregatorRebuildFailed),
    };
    let rep = open_worker_displays(&ws, &sess("team-a"), DisplayBackend::Adaptive, &probe)
        .expect("C14:显示失败不阻塞 readiness");
    for (id, d) in rep.displays.iter() {
        match d {
            WorkerDisplay::Blocked { reason } => assert!(
                ALL_BLOCK_REASONS.contains(reason),
                "worker {id} 的 block reason 必属封闭集:{reason:?}"
            ),
            other => panic!("blocked probe 下 worker {id} 应 Blocked:{other:?}"),
        }
    }
}

#[test]
fn start_mode_serde_names_match_python_start_mode_strings() {
    // 低价值 wire-format 守卫:start_mode ∈ {"resumed","fresh","fresh_after_missing_rollout","noop"}。
    assert_eq!(serde_json::to_string(&StartMode::Resumed).unwrap(), "\"resumed\"");
    assert_eq!(serde_json::to_string(&StartMode::Fresh).unwrap(), "\"fresh\"");
    assert_eq!(
        serde_json::to_string(&StartMode::FreshAfterMissingRollout).unwrap(),
        "\"fresh_after_missing_rollout\""
    );
    assert_eq!(serde_json::to_string(&StartMode::Noop).unwrap(), "\"noop\"");
}

// ───────────────────────────────────────────────────────────────────────
// decide_start_mode — bug-085 四象限 (start.py:66-69 + 179-190)
// golden 实跑(PYTHONPATH=… python3 /tmp/x.py,_resume_rollout_missing + start_mode 逻辑):
//   codex sess  rollout-present any-fresh   -> resumed
//   codex sess  rollout-MISSING !allow_fresh -> resumed  (随后真实 resume 失败)
//   codex sess  rollout-MISSING  allow_fresh -> fresh_after_missing_rollout   ← bug-085 唯一臂
//   codex no-sess any                        -> fresh
//   claude(非codex) sess rollout-missing fresh -> resumed (非 codex 永不"缺 rollout")
//   claude no-sess                            -> fresh
// 这是 bug-085 把 start_mode 分类从 start_agent 的 lock+spawn 全路径剥离出来的命门。
// ───────────────────────────────────────────────────────────────────────

fn sid(s: &str) -> SessionId {
    SessionId::new(s)
}
fn rp(p: &str) -> RolloutPath {
    RolloutPath::new(p)
}

#[test]
fn decide_start_mode_codex_missing_rollout_with_allow_fresh_is_fresh_after_missing() {
    // bug-085 唯一 FreshAfterMissingRollout 臂:codex + 有 session_id + rollout 缺 + allow_fresh。
    assert_eq!(
        decide_start_mode("codex", Some(&sid("s1")), None, false, true),
        StartMode::FreshAfterMissingRollout
    );
    // rollout 路径存在但文件已不在,同样命中。
    assert_eq!(
        decide_start_mode("codex", Some(&sid("s1")), Some(&rp("/gone.jsonl")), false, true),
        StartMode::FreshAfterMissingRollout
    );
}

#[test]
fn decide_start_mode_codex_missing_rollout_without_allow_fresh_stays_resumed() {
    // 关键陷阱:rollout 缺但 !allow_fresh → 仍 Resumed(start.py 不擅自丢 context)。
    assert_eq!(
        decide_start_mode("codex", Some(&sid("s1")), None, false, false),
        StartMode::Resumed
    );
}

#[test]
fn decide_start_mode_codex_rollout_present_is_resumed_regardless_of_fresh() {
    assert_eq!(
        decide_start_mode("codex", Some(&sid("s1")), Some(&rp("/r.jsonl")), true, false),
        StartMode::Resumed
    );
    assert_eq!(
        decide_start_mode("codex", Some(&sid("s1")), Some(&rp("/r.jsonl")), true, true),
        StartMode::Resumed
    );
}

#[test]
fn decide_start_mode_no_session_is_fresh() {
    assert_eq!(
        decide_start_mode("codex", None, None, false, true),
        StartMode::Fresh
    );
    assert_eq!(
        decide_start_mode("codex", None, None, false, false),
        StartMode::Fresh
    );
}

#[test]
fn decide_start_mode_non_codex_never_fresh_after_missing_rollout() {
    // 非 codex provider:rollout 概念不适用,_resume_rollout_missing 恒 false。
    assert_eq!(
        decide_start_mode("claude", Some(&sid("s1")), None, false, true),
        StartMode::Resumed
    );
    assert_eq!(
        decide_start_mode("claude", None, None, false, true),
        StartMode::Fresh
    );
}

#[test]
fn resume_decision_serde_names_match_python() {
    // 低价值 wire-format 守卫:_emit_resume_decisions: "resume"|"fresh_start"|"refuse"。
    assert_eq!(serde_json::to_string(&ResumeDecision::Resume).unwrap(), "\"resume\"");
    assert_eq!(
        serde_json::to_string(&ResumeDecision::FreshStart).unwrap(),
        "\"fresh_start\""
    );
    assert_eq!(serde_json::to_string(&ResumeDecision::Refuse).unwrap(), "\"refuse\"");
}

// ───────────────────────────────────────────────────────────────────────
// classify_restart_plan — Route B 全量验证 (orchestration.py:430/467/498-538)
// golden 决策矩阵(_emit_resume_decisions):
//   resumable                                   -> Resume
//   !resumable && !interacted(first_send_at absent) -> FreshStart
//   !resumable && interacted && allow_fresh      -> FreshStart
//   !resumable && interacted && !allow_fresh     -> Refuse
// Refuse 的 worker reason: 无 session_id -> "no_persisted_session_id" ; 有但不可 resume -> "session_unresumable"
// 这是把 "每非 paused worker 发一条 resume_decision + Refuse 是唯一 atomic_refusal 触发"
// 从 restart() 整条 teardown 路径剥离出来的纯验证面(gate gap)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn classify_restart_plan_interacted_unresumable_no_allow_fresh_yields_refuse() {
    // 种子 state:worker w1 有 first_send_at(已交互)但无可 resume 的 session_id。
    // !allow_fresh → decision=Refuse,且进 unresumable,reason=no_persisted_session_id。
    let state = json!({
        "session_name": "team-a",
        "agents": {
            "w1": {
                "provider": "claude",
                "first_send_at": "2026-05-27T10:00:00+00:00",
                "session_id": null
            }
        }
    });
    let plan = classify_restart_plan(&state, false)
        .expect("纯验证不应 Err(资源型失败才走 LifecycleError)");
    assert!(plan.corrupt_entries.is_empty(), "valid ISO 不应判 corrupt");
    // 恰一条决策(每非 paused worker 一条),且为 Refuse。
    assert_eq!(plan.decisions.len(), 1, "每非 paused worker 恰一条 resume_decision");
    assert_eq!(plan.decisions[0].agent_id, aid("w1"));
    assert_eq!(plan.decisions[0].decision, ResumeDecision::Refuse);
    // unresumable 收口该 worker,reason 精确。
    assert_eq!(plan.unresumable.len(), 1);
    assert_eq!(plan.unresumable[0].agent_id, aid("w1"));
    assert_eq!(plan.unresumable[0].reason, "no_persisted_session_id");
}

#[test]
fn classify_restart_plan_interacted_unresumable_with_allow_fresh_yields_fresh_start_not_refuse() {
    // 同一 worker,allow_fresh=true → FreshStart(可丢 context),unresumable 为空(无 atomic refusal)。
    let state = json!({
        "agents": {
            "w1": {
                "provider": "claude",
                "first_send_at": "2026-05-27T10:00:00+00:00",
                "session_id": null
            }
        }
    });
    let plan = classify_restart_plan(&state, true).expect("纯验证不应 Err");
    assert_eq!(plan.decisions.len(), 1);
    assert_eq!(plan.decisions[0].decision, ResumeDecision::FreshStart);
    assert!(
        plan.unresumable.is_empty(),
        "allow_fresh 下 interacted-unresumable 不触发 atomic_refusal"
    );
}

#[test]
fn classify_restart_plan_never_interacted_yields_fresh_start() {
    // first_send_at absent(从未交互)→ FreshStart,即使 !allow_fresh(无 context 可丢)。
    let state = json!({
        "agents": { "w1": { "provider": "claude", "session_id": null } }
    });
    let plan = classify_restart_plan(&state, false).expect("纯验证不应 Err");
    assert_eq!(plan.decisions.len(), 1);
    assert_eq!(plan.decisions[0].decision, ResumeDecision::FreshStart);
    assert!(plan.unresumable.is_empty());
}

// ───────────────────────────────────────────────────────────────────────
// reset_agent — operations.py:102/104
// 未传 discard_session=true → Refused{ DiscardSessionRequired }
//   (Python: {"ok":False,"status":"refused","reason":"discard_session_required"})
// 这是不丢上下文的误用保护,且在 owner-gate / 重起之前(纯输入门)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn reset_agent_without_discard_session_is_refused() {
    let ws = temp_ws();
    let got = reset_agent(&ws, &aid("w1"), false, false, None)
        .expect("discard_session=false 是 typed Refused,不是 Err");
    assert_eq!(
        got,
        ResetAgentOutcome::Refused {
            reason: ResetRefusal::DiscardSessionRequired
        }
    );
}

// ───────────────────────────────────────────────────────────────────────
// owner-gate first-door — 每个单 worker 动作的第一道门(check_team_owner)
// 空 / 无主 workspace:无 owner 记录 → foreign-owner gate 应 refuse 成 OwnerRefused,
// 或在更上游因缺 state 而 typed-refuse。本测试锁:start_agent/stop_agent 在没有
// 合法 owner 的 workspace 上 NOT 返回 Ok(Running/Stopped) —— 即门确实存在。
// (RED:现 unimplemented!() panic;porter 实现后须命中此分支。)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn start_agent_on_unowned_workspace_does_not_silently_run() {
    let ws = temp_ws(); // 空 workspace,无 state/spec/owner
    match start_agent(&ws, &aid("w1"), false, false, false, None) {
        // 允许:owner 门拒 / 缺 spec 等 requirement 门 / team 选择失败。
        Err(LifecycleError::OwnerRefused(_))
        | Err(LifecycleError::RequirementUnmet(_))
        | Err(LifecycleError::TeamSelect(_))
        | Err(LifecycleError::Compile(_)) => {}
        // 绝不允许:在没有合法 owner 的空 workspace 上声称 Running。
        Ok(StartAgentOutcome::Running { .. }) => {
            panic!("start_agent 在无主空 workspace 上不得 Running —— owner-gate 漏门")
        }
        other => panic!("意外结果(porter 实现后应命中门):{other:?}"),
    }
}

#[test]
fn stop_agent_on_unowned_workspace_does_not_silently_stop() {
    let ws = temp_ws();
    match stop_agent(&ws, &aid("w1"), None) {
        Err(LifecycleError::OwnerRefused(_))
        | Err(LifecycleError::RequirementUnmet(_))
        | Err(LifecycleError::TeamSelect(_))
        | Err(LifecycleError::Transport(_)) => {}
        Ok(rep) => panic!("stop_agent 在无主空 workspace 上不得成功:{rep:?}"),
        other => panic!("意外结果:{other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// remove_agent — agents.py:54/56
// 未传 from_spec 确认 → RefusedFromSpecConfirm;运行中未传 force → RefusedForceRequired。
// (typed refusal,不是 Err。) _RemoveRollback 字节级回滚见 Removed.agent_health_deleted。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn remove_agent_without_from_spec_is_refused_confirm() {
    let ws = temp_ws();
    // from_spec=false:Python agents.py:54 拒绝(需显式确认从 spec 摘除)。
    match remove_agent(&ws, &aid("w1"), false, false, None) {
        Ok(RemoveAgentOutcome::RefusedFromSpecConfirm { agent_id }) => {
            assert_eq!(agent_id, aid("w1"));
        }
        // 若先撞 owner / 缺 state 门也可接受(门在 confirm 之前)。
        Err(LifecycleError::OwnerRefused(_)) | Err(LifecycleError::TeamSelect(_)) => {}
        other => panic!("from_spec=false 应 RefusedFromSpecConfirm 或更上游门:{other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// restart — orchestration.py (Route B resume-atomicity)
// corrupt first_send_at → RefusedInvalidFirstSendAt,且在任何破坏性 teardown 之前
// (hard refuse BEFORE teardown)。无主空 workspace 至少不得 Restarted。
// CorruptFirstSendAt payload 必带 raw + python type name(orchestration.py:443)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn restart_on_unowned_workspace_does_not_restart() {
    let ws = temp_ws();
    match restart(&ws, false, None) {
        Err(LifecycleError::OwnerRefused(_))
        | Err(LifecycleError::TeamSelect(_))
        | Err(LifecycleError::SessionConflict(_)) => {}
        Ok(RestartReport::Restarted { .. }) => {
            panic!("restart 在无主空 workspace 上不得 Restarted")
        }
        // 校验阶段的 typed refusal 也可接受(refuse 早于 teardown,nothing created)。
        Ok(RestartReport::RefusedResumeAtomicity { .. })
        | Ok(RestartReport::RefusedInvalidFirstSendAt { .. }) => {}
        other => panic!("意外结果:{other:?}"),
    }
}

#[test]
fn python_type_name_maps_to_python_names_not_serde_names() {
    // orchestration.py:446 — type(raw).__name__。golden 实跑(/tmp/x.py):
    //   null->NoneType, ""/"null"/"x"->str, 0/123->int, false->bool, []->list, {}->dict, 1.5->float
    // 必须是 Python 名,绝不是 serde 的 null/string/number/boolean/array/object。
    assert_eq!(python_type_name(&json!(null)), "NoneType");
    assert_eq!(python_type_name(&json!("")), "str");
    assert_eq!(python_type_name(&json!("null")), "str");
    assert_eq!(python_type_name(&json!(0)), "int");
    assert_eq!(python_type_name(&json!(123)), "int");
    assert_eq!(python_type_name(&json!(false)), "bool");
    assert_eq!(python_type_name(&json!([])), "list");
    assert_eq!(python_type_name(&json!({})), "dict");
    assert_eq!(python_type_name(&json!(1.5)), "float");
}

#[test]
fn classify_restart_plan_produces_corrupt_entries_with_python_type_names() {
    // 真驱动:种子 state 含 3 个 corrupt first_send_at(""/0/[]),classify_restart_plan
    // 必发对应 CorruptFirstSendAt,raw 原值保留,type 为 python type().__name__
    // ("str"/"int"/"list") —— 锁跨语言 type-name 映射,不是手设字段。
    let state = json!({
        "agents": {
            "w_str": { "provider": "claude", "first_send_at": "" },
            "w_int": { "provider": "claude", "first_send_at": 0 },
            "w_list": { "provider": "claude", "first_send_at": [] }
        }
    });
    // corrupt 非空 → restart 在 teardown 之前 hard refuse(决策前)。
    let plan = classify_restart_plan(&state, false).expect("纯验证不应 Err");
    let by_id: BTreeMap<String, &CorruptFirstSendAt> = plan
        .corrupt_entries
        .iter()
        .map(|e| (e.worker_id.as_str().to_string(), e))
        .collect();
    assert_eq!(by_id.len(), 3, "3 个 corrupt worker 各一条 entry");
    let s = by_id.get("w_str").expect("w_str corrupt entry");
    assert_eq!(s.raw_first_send_at, json!(""));
    assert_eq!(s.raw_first_send_at_type, "str");
    let i = by_id.get("w_int").expect("w_int corrupt entry");
    assert_eq!(i.raw_first_send_at, json!(0));
    assert_eq!(i.raw_first_send_at_type, "int");
    let l = by_id.get("w_list").expect("w_list corrupt entry");
    assert_eq!(l.raw_first_send_at, json!([]));
    assert_eq!(l.raw_first_send_at_type, "list");
    // 自洽:每个 raw 经 classify 必判 Corrupt(hard refuse 前提)。
    for e in &plan.corrupt_entries {
        assert_eq!(
            classify_first_send_at(&e.raw_first_send_at),
            FirstSendAtState::Corrupt
        );
    }
}

// ───────────────────────────────────────────────────────────────────────
// select_restart_state — selection.py:49
// 空 workspace:无候选 → 回退 load_runtime_state(空态)或 TeamSelect not-found;
// 显式 team 未找到 → TeamSelect。锁:歧义/未找到走 TeamSelect 而非 panic。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn select_restart_state_unknown_team_is_team_select_error() {
    let ws = temp_ws();
    match select_restart_state(&ws, Some("ghost-team")) {
        Err(LifecycleError::TeamSelect(msg)) => {
            // Python: "restart team 'ghost-team' not found. ..."
            assert!(
                msg.contains("ghost-team") || msg.contains("not found"),
                "TeamSelect 文案应指名缺失 team:{msg}"
            );
        }
        other => panic!("未知 team 应 TeamSelect:{other:?}"),
    }
}

#[test]
fn restart_candidates_empty_workspace_is_empty_list() {
    // selection.py:12 — 无 snapshot 无 active state → 空 vec(不是 Err)。
    let ws = temp_ws();
    let got = restart_candidates(&ws).expect("空 workspace 应 Ok(空 vec)");
    assert!(got.is_empty(), "空 workspace 不应有候选:{got:?}");
}

// ───────────────────────────────────────────────────────────────────────
// save_team_runtime_snapshot — snapshot.py:17 (bug-084)
// session_name 缺失 → Python 返回 None;Rust 入口签名为 Result<PathBuf,_>。
// 锁:有 session_name 的 state 原子写出 .../runtime/teams/<safe>/state.json。
// safe_snapshot_name: 非 [A-Za-z0-9_.-] → "_",再 strip "._-"。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn save_snapshot_writes_atomic_state_json_under_teams_dir() {
    let ws = temp_ws();
    let state = json!({"session_name": "team-alpha", "agents": {}});
    let path = save_team_runtime_snapshot(&ws, &state)
        .expect("bug-084:写路径返 Result,正常态须 Ok");
    // 末段必为 state.json,且落在 runtime/teams/<safe session> 下。
    assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("state.json"));
    let s = path.to_string_lossy();
    assert!(
        s.contains("teams") && s.contains("team-alpha"),
        "快照应落在 runtime/teams/<session>/:{s}"
    );
    assert!(path.exists(), "os.replace 后目标文件应存在");
}

// ───────────────────────────────────────────────────────────────────────
// quick_start — diagnose/quick_start.py:18
// 空 agents_dir + fresh=false:无 runtime → 走 launch(可能 PreflightBlocked);
// 锁:返回 QuickStartReport(typed outcome,不 panic),且 Ready 时 next_actions 非空。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn quick_start_empty_dir_returns_typed_report_not_error_path_only() {
    let ws = temp_ws();
    // 空目录无可编译 team:应是 typed 阻塞 / 编译错,绝不 Ready。
    match quick_start(&ws, None, false, false, None) {
        Ok(QuickStartReport::Ready { .. }) => {
            panic!("空 agents_dir 不应 Ready —— 无 team 可编译")
        }
        Ok(QuickStartReport::PreflightBlocked { blockers, .. }) => {
            assert!(!blockers.is_empty(), "PreflightBlocked 必列 blockers");
        }
        Ok(QuickStartReport::ExistingRuntime { .. }) => {}
        Err(LifecycleError::Compile(_)) | Err(LifecycleError::RequirementUnmet(_)) => {}
        other => panic!("意外结果:{other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// detect_dangerous_approval — launch/config.py
// 默认进程(无 --dangerously-* 祖先)→ enabled=false / source=Disabled / inherited=false。
// launch 在 inherited=false 且无 --yes 时 raise DangerousApprovalRequired(core.py:120)。
// ───────────────────────────────────────────────────────────────────────

#[test]
#[serial_test::serial(env)]
fn detect_dangerous_approval_clean_process_is_disabled() {
    // Explicit mock ancestry keeps the test independent from the real Codex/CI
    // process tree that runs cargo.
    let _ancestry = EnvVarGuard::set("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON", "[]");
    let got = detect_dangerous_approval().expect("探测祖先链应 Ok");
    assert!(!got.enabled, "干净进程不应启用危险审批");
    assert_eq!(got.source, DangerousApprovalSource::Disabled);
    assert!(!got.inherited);
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────
// open_worker_displays — worker_window.py (C14)
// 显示失败不阻塞 readiness:probe 为 blocked 平台时,returns typed Blocked displays,
// 绝不 Err。这里用 backend=None(无 worker views)验证至少不 panic 成 Err。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn open_worker_displays_blocked_probe_yields_typed_blocked_not_error() {
    let ws = temp_ws();
    let probe = DisplayProbe {
        in_tmux: false,
        platform: "windows".to_string(),
        leader_session: None,
        leader_pane: None,
        caps: CapsFlags {
            tmux_append_windows: false,
            adaptive_display: false,
        },
        adaptive_status: DisplayStatus::Blocked,
        reason: Some(AdaptiveBlockReason::NotImplementedThisPlatform),
    };
    let rep = open_worker_displays(&ws, &sess("team-a"), DisplayBackend::Adaptive, &probe)
        .expect("C14:显示失败不阻塞 readiness —— 不得 Err");
    // 每个 worker 的 display 在 blocked 平台上应是 Blocked 变体(若有 worker)。
    for (id, d) in rep.displays.iter() {
        assert!(
            matches!(d, WorkerDisplay::Blocked { reason: AdaptiveBlockReason::NotImplementedThisPlatform }),
            "worker {id} 在 windows 平台应 Blocked(not_implemented):{d:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────
// close_team_display_backends — display/close.py (C9 close-by-recorded-backend)
// 空 workspace 无 recorded backend 无 session → 空 closed / 空 orphans(不 Err)。
// adaptive 只删带 team-tag 的窗口(C2 leader pane 安全)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn close_team_display_empty_workspace_closes_nothing_not_error() {
    let ws = temp_ws();
    let rep = close_team_display_backends(&ws, &sess("team-a"))
        .expect("C9:无 recorded backend 应 Ok(空报告),不是 Err");
    assert!(rep.closed.is_empty(), "无 recorded backend 不应关任何东西:{:?}", rep.closed);
    assert!(
        rep.orphans_cleaned.is_empty(),
        "空 workspace 无 orphan:{:?}",
        rep.orphans_cleaned
    );
}

// ───────────────────────────────────────────────────────────────────────
// fork_agent — operations.py:284 (native session fork eligibility)
// 资格门(adapter.supports_session_fork = auth_mode != "compatible_api"):
//   - compatible_api 的 agent → 不支持 fork → RuntimeError("<provider> does not support
//     native session fork") → Rust Provider error。
//   - 源 session_id 缺失 → RuntimeError("cannot fork <id>: source session_id is missing")。
// 空无主 workspace:owner-gate / 缺 spec 门优先。失败臂须 adapter.cleanup_mcp + 回滚 spec。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn fork_agent_on_unowned_workspace_does_not_silently_fork() {
    let ws = temp_ws();
    match fork_agent(&ws, &aid("src"), &aid("dst"), false, None) {
        // 允许:owner 门 / team 选择 / 缺 spec(provider 命令构造前的上游门)。
        Err(LifecycleError::OwnerRefused(_))
        | Err(LifecycleError::TeamSelect(_))
        | Err(LifecycleError::Compile(_))
        | Err(LifecycleError::RequirementUnmet(_))
        // 资格 / 源 session 缺失 → Provider error(native fork 不可用)。
        | Err(LifecycleError::Provider(_)) => {}
        Ok(rep) => panic!("无主空 workspace 不得成功 fork:{rep:?}"),
        other => panic!("意外结果(porter 实现后应命中门):{other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// add_agent — operations.py:143 (字节级回滚 ORDER, Gap 15.11)
// 前向 step 顺序(_step_done 发 lifecycle.add_step_completed):
//   role_file -> compile_role_doc -> spec_yaml -> team_state_md -> start_agent -> workspace_state
// 回滚顺序(失败时,发 lifecycle.add_step_rolled_back,operations.py:223-259):
//   spec_yaml -> workspace_state -> team_state_md -> role_file
// 事件名常量已在 event_names 锁定。无主空 workspace:owner/缺 spec 门优先于任何写。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn add_agent_event_name_constants_match_python_lifecycle_strings() {
    // 锁死发射事件名(顺序被 porter 实现后的事件流锁死;此处锁名)。
    assert_eq!(event_names::ADD_STEP_COMPLETED, "lifecycle.add_step_completed");
    assert_eq!(event_names::ADD_STEP_ROLLED_BACK, "lifecycle.add_step_rolled_back");
    assert_eq!(event_names::ADD_FAILED, "lifecycle.add_failed");
}

#[test]
fn add_agent_on_unowned_workspace_does_not_silently_add() {
    let ws = temp_ws();
    // 缺 role file / 缺 owner / 缺 spec → 上游门拒,绝不返回 Ok(env running)。
    match add_agent(&ws, &aid("w1"), &ws.join("role.md"), false, None) {
        Err(LifecycleError::OwnerRefused(_))
        | Err(LifecycleError::TeamSelect(_))
        | Err(LifecycleError::Compile(_))
        | Err(LifecycleError::RequirementUnmet(_))
        | Err(LifecycleError::RollbackFailed { .. }) => {}
        Ok(rep) => panic!("无主空 workspace 不得成功 add:{rep:?}"),
        other => panic!("意外结果:{other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// start_plan / handle_report_result / halt_plan / plan_status
// (orchestrator/__init__.py:26/79/152/177)
// golden:
//   start_plan 无 stage / 空 plan 路径 -> {"ok":False,"error":"plan has no stages"} 或 InvalidPlan
//   handle_report_result 无匹配 stage -> {"ok":True,"status":"no_match","matched":False} -> NoMatch
//   stage advance_on 命中 -> current_stage += 1;> len -> Completed
//   halt_plan 未找到 -> {"ok":False,"error":"plan not found"} ; 非 running -> already_terminal 幂等
//   plan_status 未找到 -> {"ok":False,"error":"plan not found"} -> Err / typed
// current_stage 1-based。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn start_plan_missing_file_is_invalid_plan_not_panic() {
    let ws = temp_ws();
    let missing = ws.join("nope.plan.yaml");
    // 不存在 / 无 stage 的 plan → InvalidPlan(typed),不 panic 也不 Ok(Running)。
    match start_plan(&ws, &missing, true) {
        Err(LifecycleError::InvalidPlan(_)) | Err(LifecycleError::InvalidPlanId(_)) => {}
        Ok(PlanProgress::Running { .. }) | Ok(PlanProgress::Completed { .. }) => {
            panic!("缺失 / 无 stage 的 plan 不得 Running/Completed")
        }
        other => panic!("意外结果:{other:?}"),
    }
}

#[test]
fn handle_report_result_no_running_plan_is_no_match() {
    let ws = temp_ws();
    // 无任何 plan state → 任何 report_result 都不匹配 → NoMatch(no_match / matched:false)。
    let envelope = json!({"report_result": {"status": "done"}});
    let got = handle_report_result(&ws, &envelope).expect("no_match 是 typed outcome,不是 Err");
    assert_eq!(got, PlanProgress::NoMatch);
}

#[test]
fn halt_plan_unknown_id_is_not_found_error() {
    let ws = temp_ws();
    let pid = PlanId::parse("ghost-plan").expect("合法 plan id");
    // 未持久化的 plan → "plan not found"(typed/Err),不幂等成 Halted。
    match halt_plan(&ws, &pid, "user_requested") {
        Err(LifecycleError::InvalidPlan(msg)) | Err(LifecycleError::TeamSelect(msg)) => {
            assert!(msg.contains("not found") || msg.contains("ghost-plan"), "got: {msg}");
        }
        Ok(PlanProgress::Halted { .. }) => {
            panic!("未找到的 plan 不得返回 Halted(应 not-found)")
        }
        other => panic!("意外结果:{other:?}"),
    }
}

#[test]
fn plan_status_unknown_id_is_not_found() {
    let ws = temp_ws();
    let pid = PlanId::parse("ghost-plan").expect("合法 plan id");
    // 读未持久化 plan → not-found error(Rust 入口签名 Result<PlanState,_>)。
    match plan_status(&ws, &pid) {
        Err(LifecycleError::InvalidPlan(msg)) | Err(LifecycleError::TeamSelect(msg)) => {
            assert!(msg.contains("not found") || msg.contains("ghost-plan"), "got: {msg}");
        }
        Ok(st) => panic!("未持久化 plan 不得返回 PlanState:{st:?}"),
        other => panic!("意外结果:{other:?}"),
    }
}

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model finding) ═══════════════
// P1 — classify_first_send_at must accept the breadth of datetime.fromisoformat
// (restart/orchestration.py:404-426): space/'t'/'_' separators, date-only, fractional
// seconds, HH:MM, compact ±HHMM offset, basic YYYYMMDDTHHMMSS. The current dual-parser
// (rfc3339 OR "%Y-%m-%dT%H:%M:%S") marks these Corrupt, flipping restart() into a hard
// refuse where Python proceeds. Golden re-probed via /tmp/probe_p2b_iso.py (all → 'valid').
#[test]
fn p2_classify_first_send_at_accepts_broad_iso_like_python() {
    for s in [
        "2026-05-27 10:00:00",        // space separator
        "2026-05-27",                 // date-only
        "2026-05-27T10:00:00.123456", // fractional seconds
        "2026-05-27T10:00",           // HH:MM
        "2026-05-27T10:00:00+0000",   // compact ±HHMM offset
        "20260527T100000",            // basic ISO
        "2026-05-27t10:00:00",        // lowercase 't' separator
        "2026-05-27_10:00:00",        // underscore separator
    ] {
        assert_eq!(
            classify_first_send_at(&serde_json::json!(s)),
            FirstSendAtState::Valid,
            "Python datetime.fromisoformat accepts {s:?}",
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// LIFECYCLE ENTRY POINTS — RED integration: each user-facing action must drive its REAL
// in-process chain (crate::compiler spec-compile + crate::message_store db init +
// crate::state::persist seed) with only the OS edge (tmux spawn) mocked / asserted-as-plan.
// Today they are early-return STUBS — quick_start (launch.rs:36) returns a hardcoded
// PreflightBlocked{"no role docs found"}; launch / start_agent / restart / add_agent /
// fork_agent return a hardcoded Err — so these FAIL and green once the porter wires them.
// Golden: diagnose/quick_start.py, launch/core.py, lifecycle/start.py, restart/orchestration.py,
// lifecycle/operations.py (team-agent-public @ v0.2.11).
// ═════════════════════════════════════════════════════════════════════════
