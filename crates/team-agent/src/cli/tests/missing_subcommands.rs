use super::*;

    // =========================================================================
    // WAVE-2 NON-SUB CHECKPOINT — 9 MISSING CLI subcommands (ABSENT from cli/emit.rs dispatch).
    // emit.rs:53-78 `dispatch` has NO arm for: sessions, peek, collect, e2e, diagnose, repair-state,
    // validate-result, preflight, wait-ready -> they fall to `_ => Ok(ExitCode::Error)`. These REDs
    // assert the dispatch ROUTES each subcommand: `run([sub,...]) == ExitCode::Ok` for a golden
    // EXIT-0 scenario (today unrouted -> ExitCode::Error -> RED; green once the porter adds the
    // dispatch arm + handler). Golden exit codes + JSON shapes probed via `python3 -m team_agent <sub>`.
    //
    // OBSERVABILITY NOTE: `run()` exposes only ExitCode (Ok=0/Error=1); it prints via println!, which
    // libtest intercepts (thread-local capture), so an fd-level stdout byte-capture is unreliable
    // under `cargo test`. The exact golden --json byte-shape is therefore LOCKED in each doc-comment
    // as the porter's parity obligation; the in-process assertion is the routing (exit code). A
    // follow-up can byte-lock output once each handler exists as a callable `cmd_*`/`*_port` symbol.
    //
    // Only golden-EXIT-0, tmux-SAFE scenarios make a clean RED (an exit-1 scenario's Error is
    // indistinguishable from the unknown-subcommand Error -> false-green, forbidden). preflight/
    // wait-ready/e2e/peek cannot reach golden-exit-0 on CI without real tmux/providers/a live team,
    // so they are #[ignore] real-effect seams (documented shape), NOT false-green exit-1 asserts.
    // =========================================================================

    fn cli_argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// The exact golden fake spec (cli/e2e.py `_fake_spec`, dumped via simple_yaml) with the literal
    /// `/WS` placeholder substituted to the real workspace — the minimal VALID team.spec.yaml that
    /// `collect`/`repair-state` (load_spec) accept. (load_spec rejects partial specs: requires
    /// communication/context/leader/routing/runtime/tasks.)
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

    // ── sessions ── golden cli/parser.py:230 `cmd_sessions` -> runtime.sessions(ws). EXIT 0.
    // `team-agent sessions --workspace <ws> --json` on an empty ws ->
    //   {"ok":true,"sessions":[],"workspace":"<ws>"}  (--json sort_keys). RED: unrouted -> Error.
    #[test]
    fn dispatch_routes_sessions_subcommand() {
        let ws = tmp_workspace();
        let code = run(&cli_argv(&["sessions", "--workspace", &ws.to_string_lossy(), "--json"]), &ws);
        assert_eq!(
            code,
            ExitCode::Ok,
            "`sessions` must ROUTE to cmd_sessions (golden parser.py:230, exit 0 {{ok,sessions,workspace}}); \
             today it falls to the unknown-subcommand arm (emit.rs:77) -> Error"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── validate-result ── golden parser.py:312 `cmd_validate_result` (commands.py:206). A FULL valid
    // result_envelope_v1 -> {"agent_id":"a1","ok":true,"status":"success","task_id":"t1"} EXIT 0.
    // RED: unrouted -> Error. (A partial envelope golden-exits 1 — that would be false-green, so the
    // RED uses the complete envelope that golden accepts.)
    #[test]
    fn dispatch_routes_validate_result_valid_envelope() {
        let envelope = r#"{"schema_version":"result_envelope_v1","task_id":"t1","agent_id":"a1","status":"success","summary":"done","artifacts":[],"changes":[],"tests":[],"risks":[],"next_actions":[]}"#;
        let code = run(&cli_argv(&["validate-result", envelope, "--json"]), std::path::Path::new("."));
        assert_eq!(
            code,
            ExitCode::Ok,
            "`validate-result <valid envelope> --json` must ROUTE to cmd_validate_result (parser.py:312) \
             and exit 0 with {{agent_id,ok,status,task_id}}; today -> unknown-subcommand Error"
        );
    }

    // ── collect ── golden parser.py:292 `cmd_collect` -> runtime.collect(ws). With a valid
    // team.spec.yaml present and nothing to collect -> EXIT 0, golden:
    //   {"collected":[],"collected_results":[],"coordinator":{"ok":false,"status":"not_required"},
    //    "delivered_messages":[],"invalid_results":[],"ok":true,"results":{...},"state_file":"<ws>/team_state.md"}
    // RED: unrouted -> Error.
    #[test]
    fn dispatch_routes_collect_with_spec() {
        let ws = tmp_workspace();
        seed_team_spec(&ws);
        let code = run(&cli_argv(&["collect", "--workspace", &ws.to_string_lossy(), "--json"]), &ws);
        assert_eq!(
            code,
            ExitCode::Ok,
            "`collect` must ROUTE to cmd_collect (parser.py:292); with a valid spec golden exits 0 \
             {{collected,collected_results,coordinator,delivered_messages,invalid_results,ok,results,state_file}}; \
             today -> unknown-subcommand Error"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── repair-state ── golden parser.py:303 `cmd_repair_state` -> runtime.repair_state (quick_start.py:285).
    // `--task <id>` required; `--status` must be in TASK_STATUSES{blocked,cancelled,done,failed,
    // needs_retry,pending,ready,running}. With a seeded task + --status done -> EXIT 0:
    //   {"after":{...},"before":{...},"ok":true,"state_file":"<ws>/team_state.md","task_id":"fake_impl"}
    // RED: unrouted -> Error.
    #[test]
    fn dispatch_routes_repair_state_with_task() {
        let ws = tmp_workspace();
        seed_team_spec(&ws);
        std::fs::write(
            ws.join(".team").join("runtime").join("state.json"),
            serde_json::to_vec(&json!({
                "leader": {"id": "leader"},
                "tasks": [{"id": "fake_impl", "title": "impl", "status": "open", "assignee": "fake_impl", "type": "implementation"}],
            }))
            .unwrap(),
        )
        .unwrap();
        let code = run(
            &cli_argv(&[
                "repair-state", "--workspace", &ws.to_string_lossy(),
                "--task", "fake_impl", "--status", "done", "--summary", "ok", "--json",
            ]),
            &ws,
        );
        assert_eq!(
            code,
            ExitCode::Ok,
            "`repair-state --task <id> --status done` must ROUTE to cmd_repair_state (parser.py:303) and \
             exit 0 {{after,before,ok,state_file,task_id}}; today -> unknown-subcommand Error"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── diagnose ── golden parser.py:298 `cmd_diagnose` -> runtime.diagnose(ws) (diagnose/health.py:19).
    // ok:true only when there are zero issues. Seed leader_receiver=attached + NO session_name + NO
    // agents -> issues=[] -> EXIT 0, golden top keys (sort): event_log,issues,ok,runtime,suggested_repairs.
    // RED: unrouted -> Error.
    #[test]
    fn dispatch_routes_diagnose_healthy_leader() {
        let ws = tmp_workspace();
        std::fs::write(
            ws.join(".team").join("runtime").join("state.json"),
            serde_json::to_vec(&json!({
                "leader": {"id": "leader"},
                "leader_receiver": {"mode": "direct_tmux", "status": "attached", "pane_id": "%1", "provider": "codex"},
            }))
            .unwrap(),
        )
        .unwrap();
        let code = run(&cli_argv(&["diagnose", "--workspace", &ws.to_string_lossy(), "--json"]), &ws);
        assert_eq!(
            code,
            ExitCode::Ok,
            "`diagnose` must ROUTE to cmd_diagnose (parser.py:298); a healthy (attached, no-session, \
             no-agent) state yields zero issues -> exit 0 {{event_log,issues,ok,runtime,suggested_repairs}}; \
             today -> unknown-subcommand Error"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── preflight (#[ignore] real-machine) ── golden parser.py:160 `cmd_preflight` -> runtime.preflight
    // (Path(args.team)). Validates a team role-doc dir: checks compile(TEAM.md)/tmux/ghostty/rust_core/
    // profile_dir; golden -> {"blockers":[...],"checks":[...],"details_log":...,"next_actions":[...],
    // "ok":bool,"summary":...}. Reaches ok:true (exit 0) ONLY with a valid TEAM.md + ghostty installed,
    // which CI cannot supply (ghostty absent -> blocker) -> not a clean in-process exit-0 RED.
    #[test]
    #[ignore = "real-machine: `preflight --team <dir>` needs a valid TEAM.md role-doc dir + ghostty to \
                reach ok:true; on CI it golden-exits 1 (compile/ghostty blockers) which is \
                indistinguishable from the unknown-subcommand Error (false-green). Routes to cmd_preflight \
                (parser.py:160); shape {blockers,checks,details_log,next_actions,ok,summary}"]
    fn dispatch_routes_preflight_real_machine() {
        let ws = tmp_workspace();
        let code = run(&cli_argv(&["preflight", "--team", &ws.to_string_lossy(), "--json"]), &ws);
        assert_eq!(code, ExitCode::Ok, "preflight must ROUTE + exit 0 on a valid team dir");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── wait-ready (#[ignore] real-machine) ── golden parser.py:171 `cmd_wait_ready` -> runtime.wait_ready
    // (ws, timeout). Polls worker readiness; golden -> {"details_log":...,"next_actions":[...],"ok":bool,
    // "readiness":{cli_prompt_ready,mcp_ready,process_started,task_prompt_delivered},"summary":...}.
    // ok:true (exit 0) needs a LIVE ready team; CI has none -> golden-exits 1 (false-green vs unknown).
    #[test]
    #[ignore = "real-machine: `wait-ready` polls a LIVE team's readiness; with no workers golden-exits 1 \
                (false-green vs unknown-subcommand Error). Routes to cmd_wait_ready (parser.py:171); shape \
                {details_log,next_actions,ok,readiness{cli_prompt_ready,mcp_ready,process_started,task_prompt_delivered},summary}"]
    fn dispatch_routes_wait_ready_real_machine() {
        let ws = tmp_workspace();
        let code = run(&cli_argv(&["wait-ready", "--workspace", &ws.to_string_lossy(), "--timeout", "1", "--json"]), &ws);
        assert_eq!(code, ExitCode::Ok, "wait-ready must ROUTE + exit 0 once the team is ready");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── e2e --providers fake (#[ignore] real-machine) ── golden parser.py:449 `cmd_e2e` (cli/e2e.py:12).
    // `--providers fake` runs a REAL end-to-end: runtime.launch(spec, auto_approve=True) (spawns a tmux
    // team) -> send_message -> sleep -> collect -> shutdown. Result envelope (e2e.py:16,48-56):
    //   {"workspace":str,"providers":{"fake":{"ok":bool,"launch":{...},"send":{...},"collect":{...},
    //    "shutdown":{...}}},"ok":bool}. Real tmux spawn -> #[ignore] (NOT runnable in-process).
    #[test]
    #[ignore = "real-machine: `e2e --providers fake` calls runtime.launch -> spawns a REAL tmux team \
                (send/collect/shutdown). Routes to cmd_e2e (parser.py:449 / e2e.py:12); envelope \
                {workspace,providers:{fake:{ok,launch,send,collect,shutdown}},ok}"]
    fn dispatch_routes_e2e_fake_real_machine() {
        let ws = tmp_workspace();
        let code = run(&cli_argv(&["e2e", "--providers", "fake", "--workspace", &ws.to_string_lossy(), "--json"]), &ws);
        assert_eq!(code, ExitCode::Ok, "e2e --providers fake must ROUTE + run the fake end-to-end (ok:true)");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── peek (#[ignore] real-machine) ── golden parser.py:201 `cmd_peek` (commands.py:118). Requires
    // `--allow-raw-screen` (else TeamAgentError) + a mutually-exclusive --head/--tail/--search; then
    // runtime.peek captures a LIVE tmux pane (tmux capture-pane). Real tmux -> #[ignore]. Routes to
    // cmd_peek; --json returns the peek dict (text + capture metadata).
    #[test]
    #[ignore = "real-machine: `peek <agent> --tail N --allow-raw-screen` captures a LIVE tmux pane \
                (tmux capture-pane); needs a running worker pane. Routes to cmd_peek (parser.py:201); \
                --json returns the peek dict (raw screen text + metadata)"]
    fn dispatch_routes_peek_real_machine() {
        let ws = tmp_workspace();
        let code = run(
            &cli_argv(&["peek", "fake_impl", "--workspace", &ws.to_string_lossy(), "--tail", "20", "--allow-raw-screen", "--json"]),
            &ws,
        );
        assert_eq!(code, ExitCode::Ok, "peek must ROUTE + capture the live pane (real machine)");
        let _ = std::fs::remove_dir_all(&ws);
    }

    // CONTRACT (shared-root, real-machine-driven; golden = correct-behavior baseline): wait_readiness
    // derives cli_prompt_ready from the LIFECYCLE status — an alive worker (status="running") IS
    // cli_prompt_ready (golden quick_start.py:173 `status ∈ {running, busy}`). Rust (diagnose.rs:280)
    // requires cli_prompt_ready flag / startup_prompts=="complete" / status=="ready" and does NOT accept
    // "running" → an alive fake worker never becomes ready → wait_ready times out. Same shared root as the
    // deferred_busy regression: lifecycle "running" is the authoritative alive/ready signal, not a
    // turn-level flag. (process_started/mcp_ready/task_prompt_delivered are satisfied here so `ready`
    // hinges solely on the cli_prompt_ready derivation.)
    #[test]
    fn contract_alive_worker_running_is_cli_prompt_ready_and_ready() {
        let state = serde_json::json!({"agents": {"w1": {
            "status": "running",
            "pane_id": "%1",
            "mcp_ready": true,
            "first_send_at": "2026-01-01T00:00:00Z"
        }}});
        let r = crate::cli::diagnose::wait_readiness(&state);
        assert_eq!(
            r.get("cli_prompt_ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: an alive worker (lifecycle status=running) is cli_prompt_ready (golden status∈{{running,busy}}); got {r:?}"
        );
        assert_eq!(
            r.get("ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "status=running + pane_id + mcp_ready + first_send_at → all four readiness signals derive true → ready (no timeout); got {r:?}"
        );
    }

    // CONTRACT (real-machine wait_ready product FAIL @ 8ea5df5): a live fake quick-start worker times out
    // with process_started=false + mcp_ready=false. golden (quick_start.py:172/174):
    //   process_started = bool(last["tmux_session_present"])  (the live tmux session exists)
    //   mcp_ready       = all(Path(agent["mcp_config"]).exists())  (each agent's mcp_config FILE exists)
    // The Rust launch (launch.rs:220-228) persists status="running" + mcp_config (path) but NO pane_id/pid
    // and NO mcp_ready flag. wait_readiness reads per-agent pane_id/pid for process_started (-> false) and an
    // mcp_ready FLAG for mcp_ready (-> false), so a live worker never becomes ready. The shared-root fix only
    // corrected cli_prompt_ready (status=running); these two signals still read the wrong source. This RED
    // uses the REALISTIC post-launch state (no synthetic pane_id / mcp_ready flag).
    #[test]
    fn contract_wait_ready_derives_process_started_from_session_and_mcp_ready_from_file() {
        let dir = std::env::temp_dir().join(format!(
            "ta-wr-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mcp = dir.join("mcp.json");
        std::fs::write(&mcp, "{}").unwrap();
        let state = serde_json::json!({
            "session_name": "ta-fake",
            "tmux_session_present": true, // golden status() top-level signal: the live tmux session exists
            "agents": { "w1": {
                "status": "running",
                "mcp_config": mcp.to_string_lossy(),
                "first_send_at": "2026-01-01T00:00:00Z"
            }}
        });
        let r = crate::cli::diagnose::wait_readiness(&state);
        assert_eq!(
            r.get("process_started").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: process_started derives from tmux_session_present (golden quick_start.py:172), NOT \
             per-agent pane_id/pid (the launch writes neither); got {r:?}"
        );
        assert_eq!(
            r.get("mcp_ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: mcp_ready derives from each agent's mcp_config FILE existence (golden quick_start.py:174), \
             NOT an mcp_ready flag (never set); got {r:?}"
        );
        assert_eq!(
            r.get("ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "a live fake quick-start worker (session present + status running + mcp_config file + task sent) \
             must be ready, not timeout; got {r:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
