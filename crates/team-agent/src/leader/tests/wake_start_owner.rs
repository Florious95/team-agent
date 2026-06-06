use super::*;

    // =====================================================================
    // 4. wake 层纯函数(unimplemented → RED)。golden:probe_leader.py 5 分支 + boundary。
    // =====================================================================

    #[test]
    fn should_reread_no_file_when_current_mtime_none() {
        // current_mtime is None → {reread:false, no_file}(wake.py:31-32)。
        let d = should_reread(None, None, None, 100.0, 60.0);
        assert!(!d.reread);
        assert_eq!(d.reason, RereadReason::NoFile);
    }

    #[test]
    fn should_reread_never_classified() {
        // last_classified_mtime is None → {reread:true, never_classified}。
        let d = should_reread(None, Some(10.0), None, 100.0, 60.0);
        assert!(d.reread);
        assert_eq!(d.reason, RereadReason::NeverClassified);
    }

    #[test]
    fn should_reread_file_changed() {
        // current != last_classified → {reread:true, file_changed}。
        let d = should_reread(Some(5.0), Some(20.0), Some(10.0), 100.0, 60.0);
        assert!(d.reread);
        assert_eq!(d.reason, RereadReason::FileChanged);
    }

    #[test]
    fn should_reread_quiescent_already_classified_at_and_past_debounce() {
        // current==last_classified 且 silent_for >= debounce → quiescent(不再重读)。
        // silent_for = max(0, now-current_mtime) = 100-10 = 90 >= 60。
        let d = should_reread(Some(10.0), Some(10.0), Some(10.0), 100.0, 60.0);
        assert!(!d.reread);
        assert_eq!(d.reason, RereadReason::QuiescentAlreadyClassified);
        // 边界:silent_for 恰 == debounce(now-current=60)→ 仍 quiescent(>= 比较)。
        let b = should_reread(Some(40.0), Some(40.0), Some(40.0), 100.0, 60.0);
        assert!(!b.reread);
        assert_eq!(b.reason, RereadReason::QuiescentAlreadyClassified);
    }

    #[test]
    fn should_reread_unchanged_within_debounce() {
        // current==last_classified 且 silent_for < debounce → unchanged。
        // now-current = 100-95 = 5 < 60。注:last_mtime 在 Python body 中未被使用。
        let d = should_reread(Some(10.0), Some(95.0), Some(95.0), 100.0, 60.0);
        assert!(!d.reread);
        assert_eq!(d.reason, RereadReason::Unchanged);
    }

    #[test]
    fn on_file_changed_adds_node_sorted_and_records_mtime() {
        // golden probe_leader.py:add b → pending ["b"];add a → sorted ["a","b"]。
        let s0 = on_file_changed(None, "b", 1.0);
        assert_eq!(s0.pending, vec!["b".to_string()]);
        assert_eq!(s0.mtimes.get("b"), Some(&1.0));
        let s1 = on_file_changed(Some(&s0), "a", 2.0);
        assert_eq!(s1.pending, vec!["a".to_string(), "b".to_string()], "pending 必须 sorted");
        assert_eq!(s1.mtimes.get("a"), Some(&2.0));
        // 重复 add b:pending 仍是 set(不重复),mtime 被更新到 3.0。
        let s2 = on_file_changed(Some(&s1), "b", 3.0);
        assert_eq!(s2.pending, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(s2.mtimes.get("b"), Some(&3.0));
    }

    #[test]
    fn take_pending_drains_sorted_and_clears_pending_but_keeps_mtimes() {
        // golden probe_leader.py:drain → (["a","b"], state{pending:[], mtimes 保留})。
        let mut st = on_file_changed(None, "b", 3.0);
        st = on_file_changed(Some(&st), "a", 2.0);
        let (drained, after) = take_pending(Some(&st));
        assert_eq!(drained, vec!["a".to_string(), "b".to_string()]);
        assert!(after.pending.is_empty());
        // mtimes 不被 drain 清空。
        assert_eq!(after.mtimes.get("a"), Some(&2.0));
        assert_eq!(after.mtimes.get("b"), Some(&3.0));
        // 再 drain → 空。
        let (drained2, _after2) = take_pending(Some(&after));
        assert!(drained2.is_empty());
        // drain None → ([], default state)。
        let (none_drained, none_state) = take_pending(None);
        assert!(none_drained.is_empty());
        assert!(none_state.pending.is_empty());
    }

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
        assert!(s.starts_with("team-agent-leader-codex-My_Proj___name-"), "got {s}");
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
        assert!(got.as_str().starts_with("team-agent-leader-claude_code-proj-"), "got {}", got.as_str());
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

    // emit_owner_bound_event:成功绑定 hook(owner.bound_from_caller_pane;leader_binding.py:162-183)。
    // 强化(no-full-uuid-leak 命门):事件只写 derived_uuid_prefix == derived[:12](12 hex),
    // old uuid 为 None → old_uuid_prefix == ""(空串,非缺省);全 32 hex uuid 绝不出现在任何字段。
    // unimplemented → RED。
    #[test]
    fn emit_owner_bound_event_logs_prefix_only_never_full_uuid() {
        let ws = std::env::temp_dir().join(format!("ta_rs_emit_{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let caller = PaneId::new("%7");
        let derived = uuid("fp", "/ws", "u", "default");
        let full = derived.as_str().to_string();
        assert_eq!(full.len(), 32, "derive 产 32 hex");
        let prefix12 = full[..12].to_string();
        emit_owner_bound_event(&ws, &caller, "codex", &derived, &TeamKey::new("default"), None).unwrap();
        // 读回审计事件:恰一条 owner.bound_from_caller_pane。
        let events = crate::event_log::EventLog::new(&ws).tail(50).unwrap();
        let ev = events
            .iter()
            .find(|e| e["event"] == serde_json::json!("owner.bound_from_caller_pane"))
            .expect("必写 owner.bound_from_caller_pane");
        assert_eq!(ev["caller_pane_id"], serde_json::json!("%7"));
        assert_eq!(ev["caller_current_command"], serde_json::json!("codex"));
        assert_eq!(ev["team_id"], serde_json::json!("default"));
        // derived_uuid_prefix == derived[:12](只前缀,12 hex)。
        assert_eq!(ev["derived_uuid_prefix"], serde_json::json!(prefix12));
        // old uuid=None → old_uuid_prefix == ""(空串,非 null/缺省;golden probe 已验)。
        assert_eq!(ev["old_uuid_prefix"], serde_json::json!(""));
        // no-full-uuid-leak:整条事件序列化文本里绝不出现完整 32-hex uuid。
        let raw = serde_json::to_string(ev).unwrap();
        assert!(!raw.contains(&full), "审计事件绝不泄露完整 leader_session_uuid");
        // 审计事件名字节锁。
        assert_eq!(LeaderEvent::OwnerBoundFromCallerPane.name(), "owner.bound_from_caller_pane");
    }
