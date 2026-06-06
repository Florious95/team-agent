use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const GOLDEN_SPEC: &str = r#"version: 1
team:
  name: teamspec-full-example
  mode: supervisor_worker
  objective: Build, research, review, and document a code change with Codex CLI workers.
  workspace: .
leader:
  id: leader
  role: leader
  provider: codex
  model: null
  tools:
    - fs_read
    - fs_list
    - mcp_team
    - provider_builtin
  context_policy:
    keep_user_thread: true
    receive_worker_outputs: structured_only
    max_worker_result_tokens: 4000
agents:
  - id: codex_implementer
    role: implementation_engineer
    provider: codex
    model: null
    working_directory: .
    system_prompt:
      inline: |
        You are the implementation worker. Make focused code changes, run relevant tests,
        and report a result_envelope_v1 with changed files, tests, risks, artifacts, and next actions.
      file: null
    tools:
      - fs_read
      - fs_write
      - fs_list
      - execute_bash
      - git_diff
      - mcp_team
      - provider_builtin
    permission_mode: restricted
    preferred_for:
      - implementation
      - bug_fix
      - test
    avoid_for:
      - final_risk_signoff
    output_contract:
      format: result_envelope_v1
      required_fields:
        - task_id
        - status
        - summary
        - artifacts
  - id: codex_researcher
    role: researcher
    provider: codex
    model: null
    working_directory: .
    system_prompt:
      inline: |
        You are the research worker. Prefer read-only analysis and summarize findings
        as result_envelope_v1. Do not edit files.
      file: null
    tools:
      - fs_read
      - fs_list
      - network
      - mcp_team
      - provider_builtin
    permission_mode: restricted
    preferred_for:
      - research
      - architecture
      - docs
    avoid_for:
      - implementation
    output_contract:
      format: result_envelope_v1
      required_fields:
        - task_id
        - status
        - summary
        - artifacts
  - id: codex_reviewer
    role: code_reviewer
    provider: codex
    model: null
    working_directory: .
    system_prompt:
      inline: |
        You are the reviewer. Find correctness, regression, security, and missing-test risks.
        Stay read-only unless the leader explicitly changes your permissions.
      file: null
    tools:
      - fs_read
      - fs_list
      - git_diff
      - mcp_team
      - provider_builtin
    permission_mode: restricted
    preferred_for:
      - review
      - risk_check
    avoid_for:
      - implementation
    output_contract:
      format: result_envelope_v1
      required_fields:
        - task_id
        - status
        - summary
        - artifacts
routing:
  default_assignee: leader
  rules:
    - id: implementation-to-codex
      when: task.type in ["implementation", "bug_fix", "test"]
      assign_to: codex_implementer
      priority: 100
    - id: research-to-codex
      when: task.type in ["research", "architecture", "docs"]
      assign_to: codex_researcher
      priority: 90
    - id: review-to-codex
      when: task.type in ["review", "risk_check"]
      assign_to: codex_reviewer
      priority: 90
communication:
  protocol: mcp_inbox
  topology: leader_centered
  worker_to_worker: false
  ack_timeout_sec: 60
  result_format: result_envelope_v1
  message_store:
    sqlite: .team/runtime/team.db
    mirror_files: .team/messages
runtime:
  backend: tmux
  display_backend: none
  session_name: teamspec-full-example
  auto_launch: true
  require_user_approval_before_launch: true
  dangerous_auto_approve: false
  max_active_agents: 3
  startup_order:
    - codex_implementer
    - codex_researcher
    - codex_reviewer
context:
  state_file: team_state.md
  artifact_dir: .team/artifacts
  log_dir: .team/logs
  summarization:
    worker_full_logs: retain_outside_leader_context
    state_update: after_each_result
tasks:
  - id: task_research
    title: Read the task context and identify design risks.
    type: research
    assignee: null
    deps: []
    acceptance:
      - Result envelope includes summary and risks.
    status: pending
    requires_tools:
      - fs_read
    files:
      - "**/*"
    risk: medium
    retry_limit: 1
    human_confirmation: false
  - id: task_impl
    title: Implement the requested code change and run tests.
    type: implementation
    assignee: null
    deps:
      - task_research
    acceptance:
      - Changed files and tests are reported.
    status: pending
    requires_tools:
      - fs_write
      - execute_bash
    files:
      - "src/**"
      - "tests/**"
    risk: medium
    retry_limit: 1
    human_confirmation: false
  - id: task_review
    title: Review implementation output and identify regressions.
    type: review
    assignee: null
    deps:
      - task_impl
    acceptance:
      - Findings are structured with risk and artifacts.
    status: pending
    requires_tools:
      - fs_read
      - git_diff
    files:
      - "**/*"
    risk: medium
    retry_limit: 0
    human_confirmation: false
"#;

const GOLDEN_STATE: &str = r#"# Team State

Updated: not launched

## Objective

Pending.

## Team

- Name: pending
- Runtime session: pending

## Agents

- Pending launch.

## Task Graph

- Pending task graph.

## Latest Results

- None.

## Blockers

- None.

## Next Step

- Run `team-agent validate team.spec.yaml`, review permissions, then run `team-agent launch team.spec.yaml --yes`.
"#;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_ws(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-init-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn spec_path(ws: &Path) -> PathBuf {
    ws.join(".team").join("current").join("team.spec.yaml")
}

fn state_path(ws: &Path) -> PathBuf {
    ws.join("team_state.md")
}

#[test]
fn init_json_creates_python_golden_files_and_event() {
    let ws = tmp_ws("json");
    let output = run(&["init", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert!(output.status.success(), "init must exit 0; stderr={}", stderr(&output));

    let spec = spec_path(&ws);
    let state = state_path(&ws);
    let expected_stdout = format!(
        "{{\n  \"ok\": true,\n  \"spec\": \"{}\",\n  \"state\": \"{}\"\n}}\n",
        spec.display(),
        state.display()
    );
    assert_eq!(
        stdout(&output),
        expected_stdout,
        "golden cli/helpers.py emit(--json) is json.dumps(indent=2, sort_keys=True)"
    );
    assert_eq!(stderr(&output), "");
    assert_eq!(std::fs::read_to_string(&spec).unwrap(), GOLDEN_SPEC);
    assert_eq!(std::fs::read_to_string(&state).unwrap(), GOLDEN_STATE);

    for rel in [".team", ".team/current", ".team/runtime", ".team/logs", ".team/messages", ".team/artifacts"] {
        assert!(ws.join(rel).is_dir(), "init must create {rel}");
    }

    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap();
    assert!(
        events.starts_with(&format!(
            "{{\"event\": \"init\", \"spec_path\": \"{}\", \"state_path\": \"{}\", \"ts\": ",
            spec.display(),
            state.display()
        )),
        "init event must record spec/state paths with Python key order; got {events:?}"
    );
    assert_eq!(events.lines().count(), 1);
}

#[test]
fn init_human_output_matches_python_dict_iteration_order() {
    let ws = tmp_ws("human");
    let output = run(&["init", "--workspace", ws.to_str().unwrap()], &ws);
    assert!(output.status.success(), "init must exit 0; stderr={}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "ok: True\nspec: {}\nstate: {}\n",
            spec_path(&ws).display(),
            state_path(&ws).display()
        )
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn init_existing_refuses_without_force_and_force_overwrites_templates() {
    let ws = tmp_ws("force");
    let first = run(&["init", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert!(first.status.success(), "first init failed: {}", stderr(&first));

    let second = run(&["init", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert!(!second.status.success(), "second init without --force must exit 1");
    let second_stdout = stdout(&second);
    assert!(stderr(&second).is_empty());
    assert!(
        second_stdout.starts_with(&format!(
            "{{\"ok\": false, \"error\": \"{} already exists; pass --force to overwrite\", \"action\": \"run `team-agent doctor` or inspect the log path shown here\", \"log\": \"",
            spec_path(&ws).display()
        )),
        "golden _emit_cli_error compact JSON prefix mismatch: {second_stdout:?}"
    );
    assert!(
        second_stdout.contains("/.team/logs/cli-error-"),
        "error envelope must point at .team/logs/cli-error-<ts>.log: {second_stdout:?}"
    );

    std::fs::write(spec_path(&ws), "CUSTOM_SPEC\n").unwrap();
    std::fs::write(state_path(&ws), "CUSTOM_STATE\n").unwrap();
    let forced = run(&["init", "--workspace", ws.to_str().unwrap(), "--force", "--json"], &ws);
    assert!(forced.status.success(), "forced init failed: {}", stderr(&forced));
    assert_eq!(std::fs::read_to_string(spec_path(&ws)).unwrap(), GOLDEN_SPEC);
    assert_eq!(std::fs::read_to_string(state_path(&ws)).unwrap(), GOLDEN_STATE);
}
