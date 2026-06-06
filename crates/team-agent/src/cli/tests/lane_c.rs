use super::*;

    // =========================================================================
    // WAVE-2 LANE C — diagnose/status byte-parity DIVERGENCES + target-scan wiring.
    // The status/diagnose handlers are IMPLEMENTED (status) or STUBBED (approvals/inbox/doctor/
    // comms_selftest/orphan_gate/cleanup_orphans/fix_schema) — these REDs lock the EXACT golden JSON
    // shapes (keys/values/insertion-order) each one DIVERGES from. Golden bytes captured via
    // PYTHONPATH=.../src python3 /tmp/lanec_probe*.py on a fresh workspace.
    // =========================================================================

    /// Seed `.team/runtime/state.json` with `state` so `status_port::status` reads a real fixture.
    fn seed_status_state(state: serde_json::Value) -> std::path::PathBuf {
        let ws = tmp_workspace();
        std::fs::write(
            ws.join(".team").join("runtime").join("state.json"),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        ws
    }

    // ── status #1: `team` = state["leader"]["id"] (default "leader"), NOT state["team"] ──────────────
    // GOLDEN queries.py:66 `state.get("leader", {}).get("id", "leader")`. RUST mod.rs:94 reads
    // `state.get("team")`. Seed BOTH distinct -> golden binds leader.id, Rust binds the team key. RED.
    #[test]
    fn status_team_is_leader_id_not_team_key() {
        let ws = seed_status_state(json!({"team": "TEAMKEY", "leader": {"id": "LEADERID"}}));
        let v = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status");
        assert_eq!(
            v["team"],
            json!("LEADERID"),
            "golden status.team = state['leader']['id'] (queries.py:66), NOT state['team']; got {:?}",
            v["team"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #2: messages = store.message_counts() ({} when empty), results = store.result_counts()
    // ({total,uncollected,collected,invalid,by_status}) — NOT the Rust {"count": N} placeholders. ──────
    // GOLDEN queries.py:73,75. RUST mod.rs:101,103 emit `{"count": N}`. RED.
    #[test]
    fn status_counts_are_store_count_maps_not_count_int() {
        let ws = tmp_workspace(); // empty store
        let v = status_port::status(&ws, false, true).expect("status");
        assert_eq!(
            v["messages"],
            json!({}),
            "golden empty store.message_counts() == {{}} (queries.py:73), NOT {{count:0}}; got {:?}",
            v["messages"]
        );
        assert_eq!(
            v["results"],
            json!({"total": 0, "uncollected": 0, "collected": 0, "invalid": 0, "by_status": {}}),
            "golden store.result_counts() full shape (queries.py:75), NOT {{count:0}}; got {:?}",
            v["results"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #3: coordinator = coordinator_health(ws) FULL 8-key shape in golden order ─────────────
    // GOLDEN coordinator_health: {ok,status,pid,metadata,metadata_ok,schema_ok,schema_error,schema}.
    // RUST mod.rs:105-112 emits a 6-key subset {status,ok,pid,metadata_ok,schema_ok,schema_version}
    // (no metadata/schema_error; flat schema_version instead of nested schema). RED on key-order. ──────
    #[test]
    fn status_coordinator_is_full_health_eight_key_shape() {
        let ws = tmp_workspace();
        let v = status_port::status(&ws, false, true).expect("status");
        let coord = v["coordinator"].as_object().expect("coordinator dict");
        let order: Vec<&str> = coord.keys().map(String::as_str).collect();
        assert_eq!(
            order,
            vec!["ok", "status", "pid", "metadata", "metadata_ok", "schema_ok", "schema_error", "schema"],
            "golden coordinator_health 8-key insertion order; Rust emits a 6-key subset. got {order:?}"
        );
        assert!(
            coord.get("schema_version").is_none(),
            "golden coordinator has NO flat `schema_version` (it nests schema:{{message_store_schema_version}}); \
             Rust emits the flat key. got {coord:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #4: last_events = EventLog.tail(10) ([] when no events), NOT the literal placeholder ──
    // GOLDEN queries.py:78 `EventLog(workspace).tail(10)`. RUST mod.rs:113 (non-compact) emits the
    // hardcoded placeholder `[{"event":"status.full"}]`. On a fresh log golden == []. RED. ────────────
    #[test]
    fn status_last_events_is_event_tail_not_status_full_placeholder() {
        let ws = tmp_workspace(); // no events.jsonl
        let v = status_port::status(&ws, false, true).expect("status");
        assert_ne!(
            v["last_events"],
            json!([{"event": "status.full"}]),
            "golden last_events = EventLog.tail(10), NOT the literal [{{event:status.full}}] placeholder"
        );
        assert_eq!(
            v["last_events"],
            json!([]),
            "golden last_events on a fresh event log is [] (tail of empty); got {:?}",
            v["last_events"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #5: the full (non-compact) status dict has NO `detail` key ────────────────────────────
    // GOLDEN queries.py:65-79 builds exactly 13 keys; there is no `detail`. RUST mod.rs:115-119 injects
    // `detail:true` whenever `!compact || detail`. The full --detail call MUST NOT carry detail. RED. ──
    #[test]
    fn status_noncompact_has_no_detail_key() {
        let ws = tmp_workspace();
        let v = status_port::status(&ws, /*compact=*/ false, /*detail=*/ true).expect("status");
        assert!(
            v.as_object().unwrap().get("detail").is_none(),
            "golden status dict has NO `detail` key (queries.py:65-79); Rust injects detail:true. got {v:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #6: each agent entry is enriched with an `interacted` marker (C3, queries.py:53-64) ────
    // GOLDEN: valid ISO first_send_at passes through; any other shape (invalid str / missing) -> the
    // literal "never" (_interacted_marker, queries.py:15-30). RUST mod.rs:81-84 passes agents through
    // RAW with no enrichment -> no `interacted` key. RED. ────────────────────────────────────────────
    #[test]
    fn status_agents_enriched_with_interacted_marker() {
        let ws = seed_status_state(json!({
            "leader": {"id": "leader"},
            "agents": {
                "alpha": {"provider": "codex", "first_send_at": "2026-05-27T10:00:00+00:00"},
                "beta": {"provider": "claude", "first_send_at": "not-a-date"},
                "gamma": {"provider": "claude"},
            }
        }));
        let v = status_port::status(&ws, false, true).expect("status");
        let agents = &v["agents"];
        assert_eq!(
            agents["alpha"]["interacted"],
            json!("2026-05-27T10:00:00+00:00"),
            "valid ISO first_send_at must pass through as the interacted marker (queries.py:24-29)"
        );
        assert_eq!(
            agents["beta"]["interacted"],
            json!("never"),
            "an unparseable first_send_at must render as the literal 'never' (queries.py:27-28)"
        );
        assert_eq!(
            agents["gamma"]["interacted"],
            json!("never"),
            "a missing first_send_at must render as 'never' (queries.py:30)"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── status #7 (TARGET-SCAN WIRING, in-process): tmux_session_present uses a LIVE tmux probe ───────
    // GOLDEN queries.py:52 `_tmux_session_exists(session_name)` (real `tmux has-session`). RUST mod.rs:96
    // shortcuts to `session_name.is_some()`. Seed a present-but-nonexistent session -> golden False,
    // Rust True. This is the status-level pane-discovery wiring the dangling list_targets feeds. RED. ──
    #[test]
    fn status_tmux_session_present_uses_live_tmux_probe_not_is_some() {
        let absent = format!("ta-lanec-absent-{}", std::process::id());
        let ws = seed_status_state(json!({"leader": {"id": "leader"}, "session_name": absent}));
        let v = status_port::status(&ws, false, true).expect("status");
        assert_eq!(
            v["tmux_session_present"],
            json!(false),
            "golden tmux_session_present = _tmux_session_exists(name) (a live probe; queries.py:52); a \
             present-but-nonexistent session name must be False, not is_some()==true. got {:?}",
            v["tmux_session_present"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── approvals: golden 5-key {ok,waiting,waiting_count,approvals,scan} (status/approvals.py:30-36) ──
    // RUST mod.rs:131-133 stub {ok,agent,approvals}. RED. ───────────────────────────────────────────
    #[test]
    fn approvals_golden_shape_has_waiting_count_and_scan_not_agent() {
        let ws = tmp_workspace();
        let v = status_port::approvals(&ws, None, true).expect("approvals");
        let obj = v.as_object().expect("approvals dict");
        let order: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            order,
            vec!["ok", "waiting", "waiting_count", "approvals", "scan"],
            "golden approvals 5-key order (approvals.py:30-36); Rust stub emits {{ok,agent,approvals}}. got {order:?}"
        );
        assert!(!obj.contains_key("agent"), "golden approvals has NO `agent` key");
        assert_eq!(
            v["scan"],
            json!({"mode": "tail", "lines": 120, "raw_output": false}),
            "golden scan == {{mode:tail,lines:120,raw_output:false}} (APPROVAL_SCAN_LINES=120); got {:?}",
            v["scan"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── inbox: golden {ok,agent_id,messages,since} (status/inbox.py:35-38) — key is `agent_id`, carries
    // `since` (not `agent`/`limit`). RUST mod.rs:136-144 stub {ok,agent,limit,messages}. RED. ──────────
    #[test]
    fn inbox_golden_shape_is_agent_id_and_since_not_agent_limit() {
        let ws = tmp_workspace();
        let v = status_port::inbox(&ws, "alpha", 20, None, true).expect("inbox");
        let obj = v.as_object().expect("inbox dict");
        let order: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            order,
            vec!["ok", "agent_id", "messages", "since"],
            "golden inbox 4-key order {{ok,agent_id,messages,since}} (inbox.py:38); Rust stub uses agent/limit. got {order:?}"
        );
        assert_eq!(v["agent_id"], json!("alpha"), "golden key is `agent_id` (not `agent`)");
        assert!(obj.contains_key("since"), "golden inbox carries `since` (echoed, null here)");
        assert!(!obj.contains_key("agent"), "golden inbox has NO bare `agent` key");
        assert!(!obj.contains_key("limit"), "golden inbox has NO `limit` key in the result dict");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── comms_selftest: golden {ok,status,run_id,scope,boundary,checks} (diagnose/comms.py:40-47) ─────
    // RUST mod.rs:525-527 stub {ok,team,gate,provider_sdk_calls:int}. RED. Locks COMMS_BOUNDARY_TEXT +
    // the deterministic check sub-shapes (run_id is a random uuid; not value-locked). ────────────────
    #[test]
    fn comms_selftest_golden_boundary_scope_and_check_shapes() {
        let boundary = "validates live pane binding consistency. Does NOT perform live runtime message \
                        round-trip. comms contract suite deferred to 0.2.9 (test files not shipped). \
                        (zero token, zero pollution)";
        let ws = tmp_workspace();
        let v = diagnose_port::comms_selftest(&ws, None, None).expect("comms_selftest");
        let obj = v.as_object().expect("comms dict");
        assert_eq!(v["boundary"], json!(boundary), "golden COMMS_BOUNDARY_TEXT prefix (comms.py:11-14)");
        assert_eq!(v["scope"], json!("binding_consistency"), "golden scope");
        assert_eq!(v["status"], json!("pass"), "empty-state selftest passes (all checks pass/deferred)");
        assert!(obj.contains_key("run_id"), "golden carries a run_id (uuid hex[:12])");
        assert!(!obj.contains_key("team"), "golden has NO `team` key");
        assert!(!obj.contains_key("gate"), "golden has NO `gate` key");
        assert_eq!(
            v["checks"]["contract_suite"],
            json!({
                "status": "deferred",
                "deferred_to": "0.2.9",
                "reason": "contract test files not shipped with package",
                "message": "comms contract verification deferred to 0.2.9; contract test files not shipped with package",
            }),
            "golden contract_suite check (comms.py:132-139)"
        );
        assert_eq!(
            v["checks"]["provider_sdk_calls"]["calls"],
            json!({"anthropic": 0, "openai": 0, "httpx": 0}),
            "golden provider_sdk_calls.calls (comms.py:142-151); Rust stub is a bare int 0"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── orphan_gate(fix=true,confirm=false): golden REFUSED envelope (orphan_cleanup.py:304-311) ──────
    // {ok:false,gate:"orphans",status:"refused",reason:"fix_requires_confirm",action:...}. RUST mod.rs:
    // 530-531 stub {ok:true,fix,confirm,orphans:[]}. Fully deterministic (no subprocess). RED. ─────────
    #[test]
    fn orphan_gate_fix_without_confirm_is_refused_envelope() {
        let v = diagnose_port::orphan_gate(/*fix=*/ true, /*confirm=*/ false).expect("orphan_gate");
        assert_eq!(
            v,
            json!({
                "ok": false,
                "gate": "orphans",
                "status": "refused",
                "reason": "fix_requires_confirm",
                "action": "re-run with --gate orphans --fix --confirm",
            }),
            "golden orphan_gate fix-without-confirm refused envelope (orphan_cleanup.py:304-311); got {v:?}"
        );
        let order: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(order, vec!["ok", "gate", "status", "reason", "action"], "golden refused key order");
    }

    // ── cleanup_orphans(confirm=false): golden DRY-RUN envelope (orphan_cleanup.py cleanup_…) ─────────
    // {ok,scanned,orphans,dry_run:true,scanned_at,action_required}. RUST mod.rs:534-535 stub
    // {ok,confirm,cleaned}. scanned/scanned_at are machine/clock-derived; lock the deterministic ones. RED.
    #[test]
    fn cleanup_orphans_dryrun_golden_envelope() {
        let v = diagnose_port::cleanup_orphans(/*confirm=*/ false).expect("cleanup_orphans");
        let obj = v.as_object().expect("cleanup dict");
        assert_eq!(v["dry_run"], json!(true), "no --confirm => dry_run:true (cleanup_orphan_coordinators)");
        assert_eq!(v["orphans"], json!([]), "golden lists orphans (empty when none)");
        assert_eq!(
            v["action_required"],
            json!("re-run with --confirm to send SIGTERM"),
            "golden dry-run action_required text"
        );
        assert!(obj.contains_key("scanned_at"), "golden carries scanned_at timestamp");
        assert!(obj.contains_key("scanned"), "golden carries scanned count");
        assert!(!obj.contains_key("confirm"), "golden has NO `confirm` key");
        assert!(!obj.contains_key("cleaned"), "golden has NO `cleaned` key (uses `orphans`)");
    }

    // ── fix_schema: golden fix_schema_layout diagnosis envelope (schema_migration.py:258) ─────────────
    // {ok,status,db_path,schema_version,user_version,layout_diffs,recommended_action,would_backup_path,
    // fixed,rebuilds}. RUST mod.rs:538-540 stub {ok,fixed:false}. db_path/would_backup_path are
    // path/clock-derived; lock the deterministic fields. SCHEMA_VERSION==3. RED. ─────────────────────
    #[test]
    fn fix_schema_golden_layout_diagnosis_envelope() {
        let ws = tmp_workspace();
        let v = diagnose_port::fix_schema(&ws).expect("fix_schema");
        let obj = v.as_object().expect("fix_schema dict");
        assert_eq!(v["status"], json!("missing"), "missing db: status missing");
        assert_eq!(v["schema_version"], json!(3), "golden schema_version == SCHEMA_VERSION (3)");
        assert_eq!(v["user_version"], json!(0), "missing db user_version == 0");
        assert_eq!(v["layout_diffs"], json!([]), "missing db => empty layout_diffs list");
        assert_eq!(v["recommended_action"], json!("none"), "no drift => recommended_action 'none'");
        assert_eq!(v["fixed"], json!(false), "missing db is diagnostic-only and does not create a db");
        assert_eq!(v["rebuilds"], json!([]), "no rebuilds on a fresh db");
        assert!(obj.contains_key("db_path"), "golden carries db_path");
        assert!(obj.contains_key("would_backup_path"), "golden carries would_backup_path");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── doctor: golden checks dict (diagnose/health.py:179-241) — tmux/workspace/workspace_is_git_repo/
    // providers/mcp/coordinator/ok. RUST mod.rs:520-522 stub {ok,spec,blockers}. Lock the golden
    // top-level keys (values are machine-derived) + absence of the stub's spec/blockers. RED. ─────────
    #[test]
    fn doctor_golden_checks_shape_not_spec_blockers_stub() {
        let ws = tmp_workspace();
        let v = diagnose_port::doctor(&ws, None).expect("doctor");
        let obj = v.as_object().expect("doctor dict");
        let tmux = v["tmux"].as_object().expect("golden doctor.tmux is a dict {installed,path}");
        assert!(tmux.contains_key("installed") && tmux.contains_key("path"), "tmux check shape");
        assert!(v["workspace"].is_string(), "golden doctor.workspace is a string path");
        assert!(v["workspace_is_git_repo"].is_boolean(), "golden doctor.workspace_is_git_repo bool");
        assert!(v["providers"].is_object(), "golden doctor.providers is a dict");
        assert_eq!(
            v["mcp"]["local_module"],
            json!(true),
            "golden doctor.mcp.local_module == true (health.py:197-200)"
        );
        assert!(v["coordinator"].is_object(), "golden doctor.coordinator is the coordinator_health dict");
        assert!(!obj.contains_key("spec"), "golden doctor has NO `spec` key (Rust stub adds it)");
        assert!(!obj.contains_key("blockers"), "golden doctor has NO `blockers` key (Rust stub adds it)");
        let _ = std::fs::remove_dir_all(&ws);
    }
