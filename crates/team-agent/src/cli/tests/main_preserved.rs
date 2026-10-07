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
    let v = status_port::inbox(&ws, "w1", 20, None).expect("inbox");
    let messages = v["messages"].as_array().expect("messages array");
    assert_eq!(
            messages.len(),
            1,
            "golden inbox(w1) must return the stored recipient=w1 row; the stub returns [] -> RED. got {v}"
        );
    let m = &messages[0];
    assert_eq!(
        m["message_id"],
        json!(mid),
        "the returned row is the message we stored"
    );
    assert_eq!(m["recipient"], json!("w1"));
    assert_eq!(m["sender"], json!("leader"));
    assert_eq!(m["summary"], json!("hello w1"));
    assert_eq!(
        m["status"],
        json!("accepted"),
        "create_message persists status='accepted'"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
// ── BUG-2 (match scope) — inbox(agent) returns rows where sender==agent OR recipient==agent, and
// EXCLUDES messages for other agents. Membership+exclusion form (not strict index order) so the
// test is deterministic regardless of created_at sub-second ties; golden order is chronological asc. ─
#[test]
fn inbox_matches_sender_or_recipient_and_excludes_others() {
    let ws = tmp_workspace();
    let store = crate::message_store::MessageStore::open(&ws).unwrap();
    store
        .create_message(None, "leader", "w1", "to w1", None, true, None)
        .unwrap();
    store
        .create_message(None, "w1", "leader", "from w1", None, true, None)
        .unwrap();
    store
        .create_message(None, "leader", "w2", "unrelated to w2", None, true, None)
        .unwrap();
    let v = status_port::inbox(&ws, "w1", 20, None).expect("inbox");
    let messages = v["messages"].as_array().expect("messages array");
    let mut contents: Vec<String> = messages
        .iter()
        .map(|m| m["summary"].as_str().unwrap().to_string())
        .collect();
    contents.sort();
    assert_eq!(
            contents,
            vec!["from w1".to_string(), "to w1".to_string()],
            "inbox(w1) must return BOTH the recipient=w1 and sender=w1 rows and EXCLUDE the w2 message; \
             the stub returns [] -> RED. got {contents:?}"
        );
    let _ = std::fs::remove_dir_all(&ws);
}
#[test]
fn ux_doctor_secret_scan_is_present_and_non_triggering_for_normal_paths() {
    let ws = tmp_workspace();
    std::fs::write(
        ws.join("normal-role.md"),
        "---\nname: worker\nprovider: codex\n---\nUse /tmp/team-agent.\n",
    )
    .unwrap();
    let value = json_output(
        cmd_doctor(&DoctorArgs {
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
        })
        .expect("doctor"),
    );
    assert_eq!(value.pointer("/secret_scan/ok"), Some(&json!(true)));
    assert_eq!(value.pointer("/secret_scan/findings"), Some(&json!([])));
    let _ = std::fs::remove_dir_all(&ws);
}
#[test]
fn ux_doctor_secret_scan_findings_name_the_exact_trigger() {
    let ws = tmp_workspace();
    std::fs::write(
        ws.join("leaky-role.md"),
        "OPENAI_API_KEY=sk-test-red-contract\n",
    )
    .unwrap();
    let value = json_output(
        cmd_doctor(&DoctorArgs {
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
        })
        .expect("doctor"),
    );
    let finding = value
        .pointer("/secret_scan/findings/0")
        .and_then(serde_json::Value::as_object)
        .expect("secret-scan must report the concrete trigger");
    for key in ["path", "line", "rule"] {
        assert!(
            finding.contains_key(key),
            "secret-scan finding missing `{key}`: {finding:?}"
        );
    }
    assert_eq!(finding["path"], "leaky-role.md");
    assert_eq!(finding["line"], 1);
    assert_eq!(finding["rule"], "api_key_assignment");
    assert!(!finding.contains_key("match_excerpt"));
    assert!(!value.to_string().contains("sk-test-red-contract"));
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
fn json_output(result: CmdResult) -> serde_json::Value {
    match result.output {
        CmdOutput::Json(v) => v,
        other => panic!("expected JSON output, got {other:?}"),
    }
}
fn seed_remove_agent_workspace(ws: &std::path::Path, status: &str) {
    seed_team_spec(ws);
    crate::state::persist::save_runtime_state(
        ws,
        &json!({
            "session_name": "team-agent-fake-e2e",
            "agents": {
                "fake_impl": {
                    "status": status,
                    "provider": "fake",
                    "window": "fake_impl"
                }
            },
            "spec_path": ws.join("team.spec.yaml").to_string_lossy()
        }),
    )
    .unwrap();
}
#[test]
fn remove_agent_spec_running_refusal_lists_all_required_flags_once() {
    let ws = tmp_workspace();
    seed_remove_agent_workspace(&ws, "running");
    let out = json_output(
        cmd_remove_agent(&RemoveAgentArgs {
            agent: "fake_impl".to_string(),
            workspace: ws.clone(),
            team: None,
            from_spec: false,
            confirm: false,
            force: false,
            json: true,
        })
        .unwrap(),
    );
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("refused"));
    assert_eq!(out["reason"], json!("remove_agent_flags_required"));
    for flag in ["--from-spec", "--confirm", "--force"] {
        assert!(
            out["error"].as_str().is_some_and(|s| s.contains(flag))
                && out["action"].as_str().is_some_and(|s| s.contains(flag))
                && out["command"].as_str().is_some_and(|s| s.contains(flag)),
            "refusal must mention {flag} in error/action/copyable command; got {out}"
        );
    }
    let with_confirm = json_output(
        cmd_remove_agent(&RemoveAgentArgs {
            agent: "fake_impl".to_string(),
            workspace: ws.clone(),
            team: None,
            from_spec: false,
            confirm: true,
            force: false,
            json: true,
        })
        .unwrap(),
    );
    assert_eq!(with_confirm["reason"], json!("remove_agent_flags_required"));
    for flag in ["--from-spec", "--confirm", "--force"] {
        assert!(
                with_confirm["error"].as_str().is_some_and(|s| s.contains(flag))
                    && with_confirm["action"].as_str().is_some_and(|s| s.contains(flag))
                    && with_confirm["command"].as_str().is_some_and(|s| s.contains(flag)),
                "refusal with --confirm must still give the full command including {flag}; got {with_confirm}"
            );
    }
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state["agents"].get("fake_impl").is_some(),
        "refused remove-agent must not delete the spec-defined running agent"
    );
}
#[test]
fn remove_agent_running_refusal_is_not_success_envelope() {
    let ws = tmp_workspace();
    seed_remove_agent_workspace(&ws, "running");
    let out = json_output(
        cmd_remove_agent(&RemoveAgentArgs {
            agent: "fake_impl".to_string(),
            workspace: ws.clone(),
            team: None,
            from_spec: true,
            confirm: true,
            force: false,
            json: true,
        })
        .unwrap(),
    );
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("refused"));
    assert_eq!(out["reason"], json!("force_required"));
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state["agents"].get("fake_impl").is_some(),
        "refused remove-agent must not delete the running agent"
    );
}
#[test]
fn remove_agent_from_spec_refusal_is_not_success_envelope() {
    let ws = tmp_workspace();
    seed_remove_agent_workspace(&ws, "stopped");
    let out = json_output(
        cmd_remove_agent(&RemoveAgentArgs {
            agent: "fake_impl".to_string(),
            workspace: ws.clone(),
            team: None,
            from_spec: false,
            confirm: true,
            force: false,
            json: true,
        })
        .unwrap(),
    );
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["status"], json!("refused"));
    assert_eq!(out["reason"], json!("from_spec_confirm_required"));
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state["agents"].get("fake_impl").is_some(),
        "refused remove-agent must not delete the spec-defined agent"
    );
}
