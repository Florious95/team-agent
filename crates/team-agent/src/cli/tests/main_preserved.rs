use super::*;

fn cli_argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const FAKE_SPEC_YAML: &str = r#"version: 1
team:
  name: "fake-e2e"
  mode: "supervisor_worker"
  objective: "Exercise fake provider orchestration."
  workspace: "/WS"
leader:
  id: "leader"
  role: "leader"
  provider: "fake"
  model: null
  tools:
    - "fs_read"
    - "fs_list"
    - "mcp_team"
  context_policy:
    keep_user_thread: true
    receive_worker_outputs: "structured_only"
    max_worker_result_tokens: 2000
agents:
  - id: "fake_impl"
    role: "implementation_engineer"
    provider: "fake"
    model: null
    working_directory: "/WS"
    system_prompt:
      inline: "Handle fake implementation tasks."
      file: null
    tools:
      - "fs_read"
      - "fs_write"
      - "fs_list"
      - "execute_bash"
      - "git_diff"
      - "mcp_team"
      - "provider_builtin"
    permission_mode: "restricted"
    preferred_for:
      - "implementation"
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields:
        - "task_id"
        - "status"
        - "summary"
        - "artifacts"
routing:
  default_assignee: "leader"
  rules:
    - id: "implementation-to-fake"
      match:
        type:
          - "implementation"
      assign_to: "fake_impl"
      priority: 10
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 2
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-agent-fake-e2e"
  auto_launch: true
  require_user_approval_before_launch: false
  max_active_agents: 1
  startup_order:
    - "fake_impl"
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks:
  - id: "task_impl"
    title: "Fake implementation"
    type: "implementation"
    assignee: null
    deps: []
    acceptance:
      - "fake result collected"
    status: "pending"
    requires_tools:
      - "fs_write"
      - "execute_bash"
    files:
      - "src/example.py"
    risk: "low"
"#;

fn seed_team_spec(ws: &std::path::Path) {
    let spec = FAKE_SPEC_YAML.replace("/WS", &ws.to_string_lossy());
    std::fs::write(ws.join("team.spec.yaml"), spec).unwrap();
}

    // ── ACK-CRACK [P1 byte-shape] — acknowledge_idle must write golden's TTL suppression shape ───────
    // golden runtime.py:680-688: manual-acknowledge persists
    //   coordinator.idle_acknowledged[team] = {acknowledged_at, expires_at, ttl_seconds}
    //   coordinator.suppressed_idle_alerts[team][worker].idle_fallback =
    //     {suppressed_at, suppressed_by:"manual_acknowledge", manual_acknowledge:true, expires_at, ttl_seconds}
    // The clear logic does datetime.fromisoformat(entry["expires_at"]); a MISSING expires_at -> ValueError
    // -> "invalid_suppression_timestamp" -> immediate self-clear (latent crack once detect_idle_fallbacks
    // is ported). So BOTH idle_acknowledged and the entry MUST carry a non-empty expires_at.
    #[test]
    fn acknowledge_idle_writes_golden_ttl_suppression_shape() {
        let ws = tmp_workspace();
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "active_team_key": "teamX",
                "agents": {"w1": {"status": "running", "provider": "codex"}}
            }),
        )
        .unwrap();
        let _ = lifecycle_port::acknowledge_idle(&ws, None).expect("acknowledge_idle ok");
        let state = crate::state::persist::load_runtime_state(&ws).unwrap();
        let ack = &state["coordinator"]["idle_acknowledged"]["teamX"];
        assert!(
            ack.get("expires_at").and_then(serde_json::Value::as_str).is_some_and(|s| !s.is_empty()),
            "ACK-CRACK: idle_acknowledged[team] must carry a non-empty expires_at (golden); got {ack}"
        );
        assert!(ack.get("ttl_seconds").is_some(), "idle_acknowledged[team] must carry ttl_seconds; got {ack}");
        let entry = &state["coordinator"]["suppressed_idle_alerts"]["teamX"]["w1"]["idle_fallback"];
        assert!(
            entry.get("expires_at").and_then(serde_json::Value::as_str).is_some_and(|s| !s.is_empty()),
            "ACK-CRACK: the manual-ack suppression entry must carry expires_at (else clear logic ValueErrors \
             -> instant self-clear); got {entry}"
        );
        assert_eq!(entry["suppressed_by"], serde_json::json!("manual_acknowledge"), "golden suppressed_by; got {entry}");
        assert_eq!(entry["manual_acknowledge"], serde_json::json!(true), "golden manual_acknowledge:true; got {entry}");
    }
    // ── ACK return-shape [P1 byte-parity] — acknowledge_idle must RETURN golden's keys ───────────────
    // golden runtime.py:691: return {ok, team, agent_id, acknowledged_at, expires_at, ttl_seconds}.
    // Rust (cli/mod.rs) returns only {ok, team, ttl_seconds} -> missing agent_id, acknowledged_at,
    // expires_at. RED. (acknowledged_at/expires_at are the same values written into idle_acknowledged.)
    #[test]
    fn acknowledge_idle_return_carries_golden_keys() {
        let ws = tmp_workspace();
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({ "active_team_key": "teamX", "agents": {"w1": {"status": "running", "provider": "codex"}} }),
        )
        .unwrap();
        let r = lifecycle_port::acknowledge_idle(&ws, None).expect("acknowledge_idle ok");
        let obj = r.as_object().expect("ack returns a dict");
        for key in ["ok", "team", "agent_id", "acknowledged_at", "expires_at", "ttl_seconds"] {
            assert!(
                obj.contains_key(key),
                "ACK return-shape: golden return carries `{key}` (runtime.py:691: ok/team/agent_id/\
                 acknowledged_at/expires_at/ttl_seconds); Rust omits it. got keys {:?}",
                obj.keys().collect::<Vec<_>>()
            );
        }
        assert!(
            obj.get("expires_at").and_then(serde_json::Value::as_str).is_some_and(|s| !s.is_empty()),
            "ACK return-shape: expires_at must be a non-empty timestamp; got {r}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
    // ── BUG-2 [real bug] — inbox must RETURN the stored messages, not a hardcoded []. ────────────────
    // Golden status/inbox.py:35-38 -> MessageStore.inbox(agent_id) (core.py:242, owner_team_id=None):
    //   select <MESSAGE_SELECT> from messages where sender = ? or recipient = ? order by created_at desc
    //   limit ?  -> then reversed(rows) (chronological asc). At THIS call site owner_team_id is None, so
    //   there is NO team filter — a sent-and-stored message (recipient=w1, status='accepted') must show
    //   in `inbox w1`. Rust mod.rs:144 is a stub: `let _=(workspace,limit,as_json); "messages":[]`. So
    //   the row is in team.db but inbox always returns [] -> RED. The shape test above only proves the
    //   empty-state envelope; THIS proves the message actually surfaces.
    #[test]
    fn inbox_returns_stored_message_for_recipient() {
        let ws = tmp_workspace();
        let store = crate::message_store::MessageStore::open(&ws).unwrap();
        let mid = store
            .create_message(None, "leader", "w1", "hello w1", None, true, None)
            .unwrap();
        let v = status_port::inbox(&ws, "w1", 20, None, true).expect("inbox");
        let messages = v["messages"].as_array().expect("messages array");
        assert_eq!(
            messages.len(),
            1,
            "golden inbox(w1) must return the stored recipient=w1 row; the stub returns [] -> RED. got {v}"
        );
        let m = &messages[0];
        assert_eq!(m["message_id"], json!(mid), "the returned row is the message we stored");
        assert_eq!(m["recipient"], json!("w1"));
        assert_eq!(m["sender"], json!("leader"));
        assert_eq!(m["content"], json!("hello w1"));
        assert_eq!(m["status"], json!("accepted"), "create_message persists status='accepted'");
        // NULL owner_team_id semantics: status.inbox() calls MessageStore.inbox(agent) with
        // owner_team_id=None (no team clause), so a NULL-owner message MUST surface for its recipient.
        assert_eq!(m["owner_team_id"], json!(null), "the stored message's owner_team_id is NULL and still returned");
        // byte-faithful raw-row columns: requires_ack is the 0/1 INT; artifact_refs the literal text "[]".
        assert_eq!(m["requires_ack"], json!(1), "requires_ack is the 0/1 int, not a bool");
        assert_eq!(m["artifact_refs"], json!("[]"), "artifact_refs is the raw text column, not parsed");
        let _ = std::fs::remove_dir_all(&ws);
    }
    // ── BUG-2 (match scope) — inbox(agent) returns rows where sender==agent OR recipient==agent, and
    // EXCLUDES messages for other agents. Membership+exclusion form (not strict index order) so the
    // test is deterministic regardless of created_at sub-second ties; golden order is chronological asc. ─
    #[test]
    fn inbox_matches_sender_or_recipient_and_excludes_others() {
        let ws = tmp_workspace();
        let store = crate::message_store::MessageStore::open(&ws).unwrap();
        store.create_message(None, "leader", "w1", "to w1", None, true, None).unwrap();
        store.create_message(None, "w1", "leader", "from w1", None, true, None).unwrap();
        store.create_message(None, "leader", "w2", "unrelated to w2", None, true, None).unwrap();
        let v = status_port::inbox(&ws, "w1", 20, None, true).expect("inbox");
        let messages = v["messages"].as_array().expect("messages array");
        let mut contents: Vec<String> =
            messages.iter().map(|m| m["content"].as_str().unwrap().to_string()).collect();
        contents.sort();
        assert_eq!(
            contents,
            vec!["from w1".to_string(), "to w1".to_string()],
            "inbox(w1) must return BOTH the recipient=w1 and sender=w1 rows and EXCLUDE the w2 message; \
             the stub returns [] -> RED. got {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
    // ── BUG-4 [real bug] — peek must resolve the agent terminal via state `session:window` (golden),
    // NOT a stored `pane_id` field. A live worker present in state (with a `window`, on the team's `-L`
    // socket) must NOT be fabricated as "agent pane not found". ──────────────────────────────────────
    // Golden status/peek.py:35-44: agent = state["agents"][id]; window = agent.get("window", id);
    //   if not session_name or not _tmux_window_exists(session_name, window): raise "agent terminal is
    //   not available: <id>"; else `tmux capture-pane -t session:window`. It NEVER reads a stored pane_id.
    // Probed live (/tmp/probe_peek.py): a present worker whose window is absent on the socket raises
    // `agent terminal is not available: w1`; a missing agent raises `unknown agent id: <id>`. Rust
    // cmd_peek (adapters.rs:231 + agent_pane_id:279) keys off agent_state pane_id/pane/tmux_pane_id and
    // returns {ok:false,error:"agent pane not found"} when absent — so a NORMAL live worker (window in
    // state, no pane_id field) is mis-reported as not found. That is the CP-1 pane-resolution divergence.
    //
    // Deterministic without real tmux: on a host with no live session, the golden-correct peek resolves
    // session:window, finds the window absent on the socket, and yields "agent terminal is not available:
    // w1" — NOT "agent pane not found". (The window-on-socket -> real raw-screen capture positive case is
    // real-machine; see the #[ignore] dispatch_routes_peek_real_machine.)
    #[test]
    fn peek_resolves_live_worker_via_session_window_not_pane_id_field() {
        let ws = tmp_workspace();
        // a live worker: present in state with a `window`, session_name set, but NO stored pane_id field.
        // session name is unique so a real tmux session on the dev host can't accidentally satisfy it.
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "session_name": "team-peek-red-probe-x9q",
                "agents": {"w1": {"status": "running", "provider": "codex", "window": "w1"}}
            }),
        )
        .unwrap();
        let args = PeekArgs {
            agent: "w1".to_string(),
            workspace: ws.clone(),
            tail: 20,
            allow_raw_screen: true,
            json: true,
        };
        let text = outcome_text(cmd_peek(&args));
        assert!(
            !text.contains("agent pane not found"),
            "peek keys off a stored pane_id field and fabricates 'agent pane not found' for a live worker \
             that has a `window` in state; golden resolves session:window and never reads pane_id. got: {text}"
        );
        assert!(
            text.contains("agent terminal is not available: w1"),
            "golden status/peek.py: a worker whose window is not on the socket yields \
             'agent terminal is not available: w1' (window-existence via session:window), NOT a \
             pane_id-keyed error. got: {text}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
    #[test]
    fn ux_doctor_secret_scan_is_present_and_non_triggering_for_normal_paths() {
        let ws = tmp_workspace();
        std::fs::write(ws.join("normal-role.md"), "---\nname: worker\nprovider: codex\n---\nUse /tmp/team-agent.\n").unwrap();
        let value = json_output(cmd_doctor(&DoctorArgs {
            spec: None,
            workspace: ws.clone(),
            gate: None,
            comms: false,
            team: None,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
            json: true,
        }).expect("doctor"));
        assert_eq!(value.pointer("/secret_scan/ok"), Some(&json!(true)));
        assert_eq!(value.pointer("/secret_scan/findings"), Some(&json!([])));
        let _ = std::fs::remove_dir_all(&ws);
    }
    #[test]
    fn ux_doctor_secret_scan_findings_name_the_exact_trigger() {
        let ws = tmp_workspace();
        std::fs::write(ws.join("leaky-role.md"), "OPENAI_API_KEY=sk-test-red-contract\n").unwrap();
        let value = json_output(cmd_doctor(&DoctorArgs {
            spec: None,
            workspace: ws.clone(),
            gate: None,
            comms: false,
            team: None,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
            json: true,
        }).expect("doctor"));
        let finding = value
            .pointer("/secret_scan/findings/0")
            .and_then(serde_json::Value::as_object)
            .expect("secret-scan must report the concrete trigger");
        for key in ["path", "line", "rule", "match_excerpt"] {
            assert!(finding.contains_key(key), "secret-scan finding missing `{key}`: {finding:?}");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
    #[test]
    fn ux_wait_ready_does_not_report_ready_true_without_ready_runtime_state() {
        let ws = tmp_workspace();
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "agents": {"w1": {"status": "starting"}},
                "tasks": [{"id": "t1", "assignee": "w1", "status": "pending"}],
                "leader_receiver": {"status": "attached"},
            }),
        )
        .unwrap();
        let value = json_output(cmd_wait_ready(&WaitReadyArgs {
            workspace: ws.clone(),
            timeout: 0.0,
            json: true,
        }).expect("wait-ready"));
        assert_eq!(value["ok"], json!(false), "wait-ready must not fake success before workers are ready");
        assert_eq!(value.pointer("/readiness/ready"), Some(&json!(false)));
        assert!(
            value["summary"].as_str().unwrap_or("").contains("not ready"),
            "wait-ready false state should explain not-ready status, got {value:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
    #[test]
    fn wait_ready_fake_quick_start_counts_mcp_config_and_task_prompt_delivery() {
        let ws = tmp_workspace();
        let mcp_config = ws.join(".team").join("runtime").join("agents").join("fake_impl").join("mcp_config.json");
        std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
        std::fs::write(&mcp_config, r#"{"mcpServers":{"team-agent":{}}}"#).unwrap();
        crate::state::persist::save_runtime_state(
            &ws,
            &json!({
                "session_name": "team-fake-ready",
                "agents": {
                    "fake_impl": {
                        "status": "running",
                        "provider": "fake",
                        "mcp_config": mcp_config.to_string_lossy(),
                    }
                },
                "tasks": [{
                    "id": "task_impl",
                    "assignee": "fake_impl",
                    "status": "pending",
                }],
                "leader_receiver": {"status": "attached"},
            }),
        )
        .unwrap();
        let store = crate::message_store::MessageStore::open(&ws).unwrap();
        store
            .create_message(Some("task_impl"), "leader", "fake_impl", "initial task prompt", None, true, None)
            .unwrap();
        let value = json_output(cmd_wait_ready(&WaitReadyArgs {
            workspace: ws.clone(),
            timeout: 0.0,
            json: true,
        }).expect("wait-ready"));
        assert_eq!(
            value.pointer("/readiness/mcp_ready"),
            Some(&json!(true)),
            "fake quick-start readiness must treat an existing per-agent mcp_config file as mcp_ready"
        );
        assert_eq!(
            value.pointer("/readiness/task_prompt_delivered"),
            Some(&json!(true)),
            "fake quick-start readiness must treat message_counts>0 / persisted initial task prompt as task_prompt_delivered"
        );
        assert_eq!(
            value.pointer("/readiness/ready"),
            Some(&json!(true)),
            "process_started + cli_prompt_ready alone is incomplete; mcp_ready and task_prompt_delivered must also be satisfied"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
    fn valid_result_envelope() -> serde_json::Value {
        json!({
            "schema_version": "result_envelope_v1",
            "task_id": "task_impl",
            "agent_id": "fake_impl",
            "status": "success",
            "summary": "done",
            "artifacts": [],
            "changes": [],
            "tests": [{"command": "cargo test", "status": "passed"}],
            "risks": [],
            "next_actions": []
        })
    }
    fn seed_collect_state(ws: &std::path::Path) {
        seed_team_spec(ws);
        crate::state::persist::save_runtime_state(
            ws,
            &json!({
                "agents": {"fake_impl": {"status": "idle"}},
                "tasks": [{
                    "id": "task_impl",
                    "title": "Fake implementation",
                    "type": "implementation",
                    "assignee": "fake_impl",
                    "deps": [],
                    "acceptance": ["fake result collected"],
                    "status": "pending",
                    "requires_tools": [],
                    "files": [],
                    "risk": "low"
                }],
                "session_name": Value::Null,
                "active_team_key": Value::Null,
                "spec_path": ws.join("team.spec.yaml").to_string_lossy()
            }),
        )
        .unwrap();
    }
    fn seed_uncollected_result(ws: &std::path::Path, result_id: &str) {
        let store = crate::message_store::MessageStore::open(ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        conn.execute(
            "insert into results(
                result_id, owner_team_id, task_id, agent_id, envelope, status, created_at
             ) values (?1, null, 'task_impl', 'fake_impl', ?2, 'success', '2026-06-02T10:00:00+00:00')",
            rusqlite::params![result_id, valid_result_envelope().to_string()],
        )
        .unwrap();
    }
    fn read_state(ws: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(crate::state::persist::runtime_state_path(ws)).unwrap(),
        )
        .unwrap()
    }
    fn read_events(ws: &std::path::Path) -> Vec<serde_json::Value> {
        crate::event_log::EventLog::new(ws).tail(50).unwrap()
    }
    fn seeded_team_key(ws: &std::path::Path) -> String {
        ws.file_name().unwrap().to_string_lossy().to_string()
    }
    fn json_output(result: CmdResult) -> serde_json::Value {
        match result.output {
            CmdOutput::Json(v) => v,
            other => panic!("expected JSON output, got {other:?}"),
        }
    }
    #[test]
    fn validate_result_file_good_and_inline_garbage_are_distinct() {
        let ws = tmp_workspace();
        let envelope_path = ws.join("result.json");
        std::fs::write(&envelope_path, valid_result_envelope().to_string()).unwrap();
        let good = run(
            &cli_argv(&["validate-result", "--file", &envelope_path.to_string_lossy(), "--json"]),
            &ws,
        );
        assert_eq!(
            good,
            ExitCode::Ok,
            "Python cmd_validate_result accepts --file and returns {{ok:true,task_id,agent_id,status}} for a valid envelope"
        );
        let garbage = run(&cli_argv(&["validate-result", "{garbage", "--json"]), &ws);
        assert_eq!(
            garbage,
            ExitCode::Error,
            "garbage JSON must be invalid, not indistinguishable from the good-envelope path"
        );
    }
    #[test]
    fn collect_uncollected_result_marks_db_and_outputs_result() {
        let ws = tmp_workspace();
        seed_collect_state(&ws);
        seed_uncollected_result(&ws, "res_collect_red");
        let out = json_output(
            cmd_collect(&CollectArgs {
                workspace: ws.clone(),
                result_file: None,
                json: true,
            })
            .unwrap(),
        );
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["collected_results"][0]["result_id"], json!("res_collect_red"));
        assert_eq!(out["collected_results"][0]["scope"], json!("task"));
        assert_eq!(
            out["results"],
            json!({"total": 1, "uncollected": 0, "collected": 1, "invalid": 0, "by_status": {}})
        );
        let store = crate::message_store::MessageStore::open(&ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        let status: String = conn
            .query_row(
                "select status from results where result_id = 'res_collect_red'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "collected");
        let state = read_state(&ws);
        assert_eq!(state["tasks"][0]["status"], json!("done"));
        assert_eq!(state["tasks"][0]["accepted_result_id"], json!("res_collect_red"));
        assert!(
            read_events(&ws)
                .iter()
                .any(|e| e["event"] == json!("collect.result") && e["result_id"] == json!("res_collect_red")),
            "collect must emit collect.result for the stored result"
        );
    }
    #[test]
    fn stuck_cancel_persists_suppression_and_stuck_list_reads_state() {
        let ws = tmp_workspace();
        seed_collect_state(&ws);
        let out = json_output(
            cmd_stuck_cancel(&StuckCancelArgs {
                agent: "fake_impl".to_string(),
                workspace: ws.clone(),
                alert_type: None,
                json: true,
            })
            .unwrap(),
        );
        let team_key = seeded_team_key(&ws);
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["alert_types"], json!(["cross_worker_deadlock", "idle_fallback", "stuck"]));
        assert!(out["suppressed"]["idle_fallback"]["snapshot"]["assigned_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!("task_impl")));
        let state = read_state(&ws);
        assert_eq!(
            state["coordinator"]["suppressed_idle_alerts"][&team_key]["fake_impl"]["idle_fallback"]["suppressed_by"],
            json!("leader")
        );
        assert!(
            read_events(&ws)
                .iter()
                .any(|e| e["event"] == json!("coordinator.idle_alert_suppressed")
                    && e["agent_id"] == json!("fake_impl")),
            "stuck_cancel must write coordinator.idle_alert_suppressed"
        );
        let listed = json_output(
            cmd_stuck_list(&StuckListArgs {
                workspace: ws.clone(),
                json: true,
            })
            .unwrap(),
        );
        assert_eq!(
            listed["suppressed_idle_alerts"]["fake_impl"]["stuck"]["suppressed_by"],
            json!("leader"),
            "stuck-list must read the persisted state mirror, not return a hard-coded empty list"
        );
    }
    #[test]
    fn stuck_cancel_invalid_alert_type_is_rejected() {
        let ws = tmp_workspace();
        seed_collect_state(&ws);
        let code = run(
            &cli_argv(&[
                "stuck-cancel",
                "fake_impl",
                "--workspace",
                &ws.to_string_lossy(),
                "--alert-type",
                "bogus",
                "--json",
            ]),
            &ws,
        );
        assert_eq!(
            code,
            ExitCode::Error,
            "Python rejects alert_type outside stuck/idle_fallback/cross_worker_deadlock/all; Rust must not silently coerce bogus to stuck"
        );
    }
    #[test]
    fn acknowledge_idle_records_manual_idle_fallback_suppression_and_event() {
        let ws = tmp_workspace();
        seed_collect_state(&ws);
        let out = json_output(
            cmd_acknowledge_idle(&AcknowledgeIdleArgs {
                team: None,
                workspace: ws.clone(),
                json: true,
            })
            .unwrap(),
        );
        let team_key = seeded_team_key(&ws);
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["team"], json!(team_key));
        assert_eq!(out["ttl_seconds"], json!(1800));
        let state = read_state(&ws);
        let ack = &state["coordinator"]["idle_acknowledged"][&team_key];
        assert_eq!(ack["ttl_seconds"], json!(1800));
        assert!(ack["acknowledged_at"].as_str().is_some());
        assert_eq!(
            state["coordinator"]["suppressed_idle_alerts"][&team_key]["fake_impl"]["idle_fallback"]["suppressed_by"],
            json!("manual_acknowledge")
        );
        assert_eq!(
            state["coordinator"]["suppressed_idle_alerts"][&team_key]["fake_impl"]["idle_fallback"]["manual_acknowledge"],
            json!(true)
        );
        assert!(
            read_events(&ws)
                .iter()
                .any(|e| e["event"] == json!("coordinator.idle_acknowledged")
                    && e["team"] == json!(team_key)
                    && e["ttl_seconds"] == json!(1800)),
            "acknowledge-idle must emit coordinator.idle_acknowledged"
        );
    }
    #[test]
    fn repair_state_done_persists_after_status_and_summary() {
        let ws = tmp_workspace();
        seed_collect_state(&ws);
        let out = json_output(
            cmd_repair_state(&RepairStateArgs {
                workspace: ws.clone(),
                task_id: "task_impl".to_string(),
                assignee: None,
                status: "done".to_string(),
                summary: Some("manual repair accepted".to_string()),
                json: true,
            })
            .expect("repair-state should not fail on a valid runtime state"),
        );
        assert_eq!(out["ok"], json!(true));
        assert_eq!(
            out["after"]["status"],
            json!("done"),
            "repair-state --status done must return after.status=done; ok:true with null after fields is a false success"
        );
        assert_eq!(out["after"]["assignee"], json!("fake_impl"));
        assert_eq!(out["after"]["last_result_summary"], json!("manual repair accepted"));
        let state = read_state(&ws);
        let task = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == json!("task_impl"))
            .unwrap();
        assert_eq!(
            task["status"],
            json!("done"),
            "repair-state --status done must persist the task status, not only emit a success envelope"
        );
        assert_eq!(task["last_result_summary"], json!("manual repair accepted"));
        let _ = std::fs::remove_dir_all(&ws);
    }
