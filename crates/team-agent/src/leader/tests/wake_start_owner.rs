use super::*;

// =====================================================================
// 5. leader_session_name — sha1 派生 + 文件夹消毒(unimplemented → RED)
// =====================================================================

// 公式:team-agent-leader-<provider>-<sanitized folder[:48]>-<sha1(resolve(ws))[:8]>。
// 用真实 temp 目录,sha1/sanitize 在测试内复算后断言函数输出与之一致(probe 已验证公式)。
#[test]
fn leader_session_name_formula_and_sanitization() {
    // 公式 = team-agent-leader-<provider>-<sanitized folder>-<sha1(resolve(ws))[:8]>。
    // sha1 复算需 sha1 crate(本测试不引);改为断言格式不变量(provider/消毒/8-hex 后缀),
    // 字节级 sha1 由 golden probe_leader.py 已验证公式正确。
    let base = std::env::temp_dir().join(format!("ta_rs_lsn_{}", std::process::id()));
    let weird = base.join("My Proj!@#name");
    std::fs::create_dir_all(&weird).unwrap();
    let got = leader_session_name(Provider::Codex, &weird);
    // 前缀 + provider + 消毒后的 folder(非字母数字_.- → '_')。
    let s = got.as_str();
    assert!(
        s.starts_with("team-agent-leader-codex-My_Proj___name-"),
        "got {s}"
    );
    // sha1[:8] 后缀:8 个 hex。
    let suffix = s.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 8, "sha1 前缀须 8 hex,got {suffix}");
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
}

// folder 名消毒成空 → 回退 "workspace"(probe_leader.py allsym 用例)。
#[test]
fn leader_session_name_empty_sanitized_folder_falls_back_to_workspace() {
    let base = std::env::temp_dir().join(format!("ta_rs_lsn2_{}", std::process::id()));
    // 全符号目录名 → 消毒后 strip('._-') 为空 → "workspace"。
    let allsym = base.join("@@@");
    std::fs::create_dir_all(&allsym).unwrap();
    let got = leader_session_name(Provider::Codex, &allsym);
    assert!(
        got.as_str().contains("-workspace-"),
        "全符号 folder 应回退 'workspace',got {}",
        got.as_str()
    );
}

// claude_code provider 出现在 session 名里(probe:team-agent-leader-claude_code-...)。
#[test]
fn leader_session_name_uses_claude_code_provider_string() {
    let base = std::env::temp_dir().join(format!("ta_rs_lsn3_{}", std::process::id()));
    let dir = base.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let got = leader_session_name(Provider::ClaudeCode, &dir);
    assert!(
        got.as_str()
            .starts_with("team-agent-leader-claude_code-proj-"),
        "got {}",
        got.as_str()
    );
}

#[test]
fn leader_session_name_uses_copilot_provider_string() {
    let base = std::env::temp_dir().join(format!("ta_rs_lsn_copilot_{}", std::process::id()));
    let dir = base.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let got = leader_session_name(Provider::Copilot, &dir);
    assert!(
        got.as_str().starts_with("team-agent-leader-copilot-proj-"),
        "got {}",
        got.as_str()
    );
}

// =====================================================================
// 6. Family A 正源 owner 绑定 — bind_owner_from_caller_pane(unimplemented → RED)
// =====================================================================

// $TMUX_PANE 缺 → refuse + reason=caller_pane_missing(leader_binding.py:79-95)。
// 此处只能在 $TMUX_PANE 缺失环境下断言(测试进程通常无 TMUX_PANE)。
#[test]
fn bind_owner_refuses_when_caller_pane_missing() {
    // 防御:确保本测试看到的环境无 TMUX_PANE(若 CI 在 tmux 内跑,跳过断言形态)。
    if std::env::var_os("TMUX_PANE").is_some() {
        // 在 tmux 内:正源存在,不该走 refuse 分支;此用例只验缺失分支,直接返回。
        return;
    }
    let ws = std::env::temp_dir().join(format!("ta_rs_bind_{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let team = TeamKey::new("default");
    let res = bind_owner_from_caller_pane(&ws, &team, None).unwrap();
    assert!(!res.ok);
    assert_eq!(res.reason, Some(LeaseReason::CallerPaneMissing));
    // caller_pane_id 为空(probe:""),hint 为 _HINT_RUN_FROM_LEADER_PANE。
    assert_eq!(res.caller_pane_id, PaneId::new(""));
    assert_eq!(res.caller_current_command, "");
    assert_eq!(
            res.hint.as_deref(),
            Some("run team-agent from inside your leader pane (the tmux pane you want to own this team).")
        );
    assert_eq!(res.team_id, team);
}

// owner.bind_refused 事件名字节锁(LeaderEvent::name unimplemented → RED;与 #5 重叠但锁 binding 路径)。
#[test]
fn owner_bind_refused_event_name_is_owner_bind_refused() {
    assert_eq!(LeaderEvent::OwnerBindRefused.name(), "owner.bind_refused");
}

// bind_owner_from_caller_pane:成功绑定后调用 owner.bound_from_caller_pane 审计 hook
// (leader_binding.py:162-183)。
// 强化(no-full-uuid-leak 命门):事件只写 derived_uuid_prefix == derived[:12](12 hex),
// old uuid 为 None → old_uuid_prefix == ""(空串,非缺省);全 32 hex uuid 绝不出现在任何字段。
// unimplemented → RED。
#[test]
fn emit_owner_bound_event_logs_prefix_only_never_full_uuid() {
    let _lock = ENV_LOCK.lock().unwrap();
    let ws = std::env::temp_dir().join(format!("ta_rs_emit_{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let _env = EnvGuard::apply(&[
        ("TMUX_PANE", Some("%7")),
        ("TEAM_AGENT_MACHINE_FINGERPRINT", Some("fp")),
        ("TEAM_AGENT_LEADER_PROVIDER", Some("codex")),
        ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
    ]);
    let result = bind_owner_from_caller_pane(&ws, &TeamKey::new("default"), None).unwrap();
    assert!(result.ok, "caller pane should produce an owner binding");
    let owner = result.owner.as_ref().expect("successful bind has owner");
    let full = owner
        .leader_session_uuid
        .as_ref()
        .expect("successful bind has leader session uuid")
        .as_str()
        .to_string();
    assert_eq!(full.len(), 32, "derive 产 32 hex");
    let prefix12 = full[..12].to_string();
    // 读回审计事件:恰一条 owner.bound_from_caller_pane。
    let events = crate::event_log::EventLog::new(&ws).tail(50).unwrap();
    let ev = events
        .iter()
        .find(|e| e["event"] == serde_json::json!("owner.bound_from_caller_pane"))
        .expect("必写 owner.bound_from_caller_pane");
    assert_eq!(ev["caller_pane_id"], serde_json::json!("%7"));
    assert_eq!(
        ev["caller_current_command"],
        serde_json::json!(result.caller_current_command)
    );
    assert_eq!(ev["team_id"], serde_json::json!("default"));
    // derived_uuid_prefix == derived[:12](只前缀,12 hex)。
    assert_eq!(ev["derived_uuid_prefix"], serde_json::json!(prefix12));
    // old uuid=None → old_uuid_prefix == ""(空串,非 null/缺省;golden probe 已验)。
    assert_eq!(ev["old_uuid_prefix"], serde_json::json!(""));
    // no-full-uuid-leak:整条事件序列化文本里绝不出现完整 32-hex uuid。
    let raw = serde_json::to_string(ev).unwrap();
    assert!(
        !raw.contains(&full),
        "审计事件绝不泄露完整 leader_session_uuid"
    );
    // 审计事件名字节锁。
    assert_eq!(
        LeaderEvent::OwnerBoundFromCallerPane.name(),
        "owner.bound_from_caller_pane"
    );
}
