use super::*;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;

struct TeamSocketGuard {
    ws: std::path::PathBuf,
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn unset(keys: &[&'static str]) -> Self {
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            unsafe {
                std::env::remove_var(key);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

impl Drop for TeamSocketGuard {
    fn drop(&mut self) {
        crate::tmux_backend::TmuxBackend::for_workspace(&self.ws).kill_server();
        if let Some(uid) = current_uid() {
            let socket = crate::tmux_backend::socket_name_for_workspace(&self.ws);
            for root in ["/private/tmp", "/tmp"] {
                let path = std::path::Path::new(root)
                    .join(format!("tmux-{uid}"))
                    .join(&socket);
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn current_uid() -> Option<String> {
    let output = std::process::Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// =========================================================================
// (A) CLI dispatch — run(argv, cwd) parses each subcommand and routes to its (already-tested)
// cmd_*, mapping the exit code. Today run() only handles codex/claude passthrough + help, else
// ExitCode::Error. These prove run DISPATCHES (observable per-command effect + exit). Golden
// cli/parser.py:main. OS-safe handlers only; spawn-bearing routes are #[ignore] real-machine.
// =========================================================================

#[test]
fn run_dispatches_status_to_handler_returns_ok() {
    // run([status --workspace <seeded> --json]) -> cmd_status -> status_port::status (reads state)
    // -> ExitCode::Ok. Today: Error (no dispatch). OS-safe (status only reads runtime state).
    let ws = seed_status_workspace();
    let argv = vec![
        "status".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().to_string(),
        "--json".to_string(),
    ];
    assert_eq!(
        run(&argv, Path::new(".")),
        ExitCode::Ok,
        "run must dispatch `status` to cmd_status and return Ok (not today's hardcoded Error)"
    );
}

#[test]
#[serial_test::serial(env)]
fn run_dispatches_send_to_handler_returns_ok() {
    let _env = hermetic_guard::HermeticTestEnv::enter("send-dispatch");
    // run([send w1 hello --workspace <seeded>]) -> cmd_send -> messaging::send_message
    // -> ok (w1 is an in-team agent) -> ExitCode::Ok. w1 must be seeded: golden refuses non-team targets.
    //
    // OLD seed: flat `{"agents": {"w1": ...}}`.
    // NEW seed (Bug 1/2 — team-in-team state scope, see
    // tests/team_in_team_state_scope_red.rs): send_message projects the raw state
    // through the active team_key, so the in-team agent now lives under
    // `teams[<key>].agents`. Behavior under test (dispatch + ok exit) unchanged.
    let ws = deleg_uniq_dir("run-send");
    // M4: no SQLite message store fixture (MessageStore::open is retired).
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "active_team_key": "current",
            "teams": {"current": {"agents": {"w1": {"provider": "fake"}}}}
        }),
    )
    .unwrap();
    let argv = vec![
        "send".to_string(),
        "w1".to_string(),
        "hello".to_string(),
        "--mailbox".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().to_string(),
    ];
    assert_eq!(
        run(&argv, Path::new(".")),
        ExitCode::Ok,
        "run must dispatch an explicit mailbox send without a coordinator"
    );
    assert_eq!(crate::inbox_store::read(&ws, "w1", 3, None).unwrap()["messages"][0]["content"], "hello");
}

#[test]
fn run_unknown_subcommand_is_error_not_usage() {
    // 0.5.45 naming-addressing (RED-6): unknown subcommand exits 1
    // (Error), aligned with the shared refusal shape used by
    // `send`/`--to-name` typos. Pre-0.5.45 argparse-style Usage (2)
    // was internal drift.
    assert_eq!(
        run(&["totally-not-a-subcommand".to_string()], Path::new(".")),
        ExitCode::Error,
        "unknown subcommand must map to ExitCode::Error (aligned with send/--to-name typo family)"
    );
}

#[test]
#[ignore = "real-machine: `coordinator --once` dispatches to run_daemon which drives a real \
                Coordinator (TmuxBackend); in-process needs a run_daemon_with_coordinator seam"]
#[test]
#[ignore = "real-machine: quick-start --yes spawns a real team (tmux) + coordinator daemon"]
fn run_dispatches_quick_start_compiles_spec() {
    // The full quick-start path spawns workers + the coordinator; on a real machine we assert it
    // dispatched to cmd_quick_start (team.spec.yaml compiled under the team dir). With no positive
    // caller pane, honest-readiness reports leader_receiver_unbound and exits nonzero.
    let _env = EnvGuard::unset(&[
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let dir = std::env::temp_dir().join(format!("ta-cli-qs-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    std::fs::write(
        dir.join("TEAM.md"),
        "---\nname: t\nobjective: o\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(
            dir.join("agents").join("implementer.md"),
            "---\nname: implementer\nrole: Impl\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nImpl.\n",
        )
        .unwrap();
    let argv = vec![
        "quick-start".to_string(),
        dir.to_string_lossy().to_string(),
        "--yes".to_string(),
    ];
    let exit = run(&argv, Path::new("."));
    assert!(
        dir.join("team.spec.yaml").exists(),
        "quick-start must compile the spec under the team dir"
    );
    assert_eq!(
            exit,
            ExitCode::Error,
            "quick-start must still dispatch and compile, but an unbound leader receiver is an honest-readiness failure, not ExitCode::Ok"
        );
}

// =========================================================================
// (DELEGATION) cli *_port handlers must DELEGATE TO THE REAL SUBSYSTEMS (crate::lifecycle /
// crate::coordinator / crate::message_store), not the placeholder stubs in cli/mod.rs (which do
// `let _ = (...)` and fabricate canned Values). Each test asserts the REAL subsystem's effect
// (spec compiled, real error text, real coordinator_health, real DB counts) appears in the
// handler's CmdResult/Value — distinguishable from the placeholder's canned output. OS edge mocked;
// real tmux spawn / real daemon = #[ignore]. Golden: lifecycle/launch.rs+restart.rs, compiler.rs,
// coordinator/health.rs, message_store.
// =========================================================================
#[test]
#[serial_test::serial(env)]
fn cli_quick_start_invokes_real_lifecycle_admission_before_runtime_effects() {
    let env = hermetic_guard::HermeticTestEnv::enter("issue337-cli-admission");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace("ws");
    let team = workspace.join("roles");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), DELEG_TEAM_MD).unwrap();
    std::fs::write(team.join("agents/implementer.md"), DELEG_VALID_ROLE).unwrap();
    let args = QuickStartArgs {
        workspace: crate::model::paths::team_workspace(&team).unwrap(),
        agents_dir: team.clone(),
        name: None,
        team_id: None,
        yes: true,
        backend: None,
        json: true,
        detail: false,
    };
    let result = cmd_quick_start(&args).expect("real lifecycle admission");
    assert_eq!(result.exit, ExitCode::Error);
    let CmdOutput::Json(value) = result.output else { panic!() };
    assert_eq!(value["status"], "preflight_blocked");
    assert!(value["blockers"].to_string().contains("caller_pane_missing"));
    assert_eq!(value["effects"]["worker_panes_started"], json!([]));
    assert_eq!(value["effects"]["coordinator_start"], "not_attempted");
    let runtime_spec = crate::model::paths::runtime_spec_path(&args.workspace, "clidelegteam");
    assert!(!runtime_spec.exists(), "#337 admission precedes runtime spec publication");
    assert!(!crate::state::persist::runtime_state_path(&args.workspace).exists());
    assert!(!args.workspace.join(".team/team.db").exists());
    assert!(
        !team.join("team.spec.yaml").exists(),
        "E5: quick_start must NOT write spec into the user team dir"
    );
}

// 2 [P0] — cmd_quick_start over an INVALID role doc surfaces the REAL compiler error (missing
// `provider`), proving delegation to crate::lifecycle (compile fails before any spawn). The
// placeholder returns a canned {ok:true} with no error -> RED. OS-safe (compile fails early).
#[test]
fn cli_quick_start_invalid_spec_surfaces_real_compile_error() {
    let team = deleg_uniq_dir("qsbad");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), DELEG_TEAM_MD).unwrap();
    std::fs::write(team.join("agents").join("broken.md"), DELEG_INVALID_ROLE).unwrap();
    let args = QuickStartArgs {
        workspace: crate::model::paths::team_workspace(&team).unwrap(),
        agents_dir: team,
        name: None,
        team_id: None,
        yes: false,
        backend: None,
        json: true,
        detail: false,
    };
    let text = outcome_text(cmd_quick_start(&args));
    assert!(
            text.contains("provider"),
            "cmd_quick_start must surface the REAL compiler error for a role doc missing `provider` \
             (proving delegation to crate::lifecycle, not the placeholder's canned ok:true); got {text}"
        );
}

// 3 [P1] — cmd_restart on a no-spec workspace surfaces the REAL crate::lifecycle::restart error
// "missing spec for restart" (restart.rs:147). The placeholder returns {ok:true, allow_fresh, team}
// -> RED. OS-safe (restart errors on the missing spec before any spawn).
#[test]
fn cli_restart_missing_spec_surfaces_real_teamselect() {
    let ws = deleg_uniq_dir("restart"); // no team.spec.yaml
    let args = RestartArgs {
        workspace: ws,
        team: None,
        allow_fresh: false,
        session_converge_deadline_ms: None,
        json: true,
        detail: false,
    };
    let text = outcome_text(cmd_restart(&args));
    // RED-2-STILL: entry gate now resolves via canonical_run_workspace; for a no-spec workspace
    // the "no spec" surfacing may come from either the entry gate ("missing spec for restart")
    // or, when a stray ancestor .team lets it past, the resolve gate ("active team spec not
    // found"). Both are the REAL crate::lifecycle error (not a placeholder ok:true).
    assert!(
        text.contains("missing spec for restart") || text.contains("active team spec not found"),
        "cmd_restart must surface the REAL crate::lifecycle::restart missing-spec error \
             (not the placeholder's canned ok:true); got {text}"
    );
}

// 4 [P1] — cmd_add_agent with a DUPLICATE runtime agent id surfaces the REAL
// crate::lifecycle::add_agent "agent id already exists" error. E25: role docs/spec files are
// inputs, not membership; duplicate membership is runtime-state based.
#[test]
fn cli_add_agent_duplicate_id_surfaces_real_error() {
    let team = deleg_uniq_dir("addagent");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), DELEG_TEAM_MD).unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), DELEG_VALID_ROLE).unwrap(); // 'implementer' already exists
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-deleg-addagent",
            "agents": {
                "implementer": {"status": "running", "provider": "fake", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    let dup_role = team.join("dup-role.md");
    std::fs::write(&dup_role, DELEG_VALID_ROLE).unwrap(); // role file must exist
    let args = AddAgentArgs {
        role_config: Default::default(),
        agent: "implementer".to_string(),
        workspace: team,
        team: None,
        role_file: dup_role.to_string_lossy().to_string(),
        force: false,
        json: true,
    };
    let text = outcome_text(cmd_add_agent(&args));
    assert!(
            text.contains("already exists"),
            "cmd_add_agent must surface the REAL crate::lifecycle::add_agent 'agent id already exists' error \
             (not the placeholder's canned ok:true); got {text}"
        );
}

// Nine base fields remain; #338 adds only observation reason and scoped action
// when uncertain. No coordinator/db/history/payload data enters the node.
#[test]
fn cli_status_emits_brief_with_scoped_unknown_diagnostics() {
    let ws = seed_status_workspace(); // writes .team/runtime/state.json
    // M4: the SQL message/result seeding that proved "no db data enters the node" is gone
    // with the store itself; the brief shape below is still asserted.
    // a HEALTHY coordinator at the status workspace (this process's pid + matching metadata).
    let wp = crate::coordinator::WorkspacePath::new(ws.clone());
    let me = crate::coordinator::Pid::new(std::process::id());
    crate::coordinator::write_coordinator_metadata(
        &wp,
        me,
        crate::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        crate::coordinator::coordinator_pid_path(&wp),
        me.to_string(),
    )
    .unwrap();

    let args = StatusArgs {
        agent: None,
        workspace: ws,
        detail: true,
        summary: false,
        json: true,
        team: None,
    };
    let r = cmd_status(&args).expect("cmd_status");
    let value = match r.output {
        CmdOutput::Json(v) => v,
        other => panic!("status --json must yield a Json CmdOutput; got {other:?}"),
    };
    let nodes = value
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .expect("status brief must expose a nodes array");
    assert_eq!(nodes.len(), 1);
    let fields: std::collections::BTreeSet<&str> = nodes[0]
        .as_object()
        .expect("status brief node must be an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "name",
            "provider",
            "model",
            "effort",
            "runtime_status",
            "activity",
            "health",
            "session_name",
            "tmux_command",
            "observation_reason",
            "next_action",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(nodes[0]["runtime_status"], "unknown");
    assert!(!nodes[0]["observation_reason"].as_str().unwrap().is_empty());
    assert!(nodes[0]["next_action"].as_str().unwrap().contains(&*args.workspace.to_string_lossy()));
}

#[test]
#[serial_test::serial(env)]
fn issue338_doctor_reports_same_scope_unknown_observations_without_state_writes() {
    let env = hermetic_guard::HermeticTestEnv::enter("issue338-doctor");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace("ws");
    crate::state::persist::save_runtime_state(&workspace, &json!({
        "team_key": "current", "session_name": "test-workers",
        "agents": {"worker": {"provider": "pi", "status": "running"}},
    })).unwrap();
    // Complete the existing legacy-to-current state normalization as fixture
    // preparation; this test covers observation of an already-current runtime.
    crate::state::persist::load_runtime_state(&workspace).unwrap();
    let state_path = crate::state::persist::runtime_state_path(&workspace);
    let before = std::fs::read(&state_path).unwrap();
    let result = cmd_doctor(&DoctorArgs {
        spec: None, workspace: workspace.clone(), gate: None, comms: false,
        team: Some("current".into()), fix: false, fix_schema: false,
        cleanup_orphans: false, confirm: false, json: true,
    }).unwrap();
    let CmdOutput::Json(value) = result.output else { panic!() };
    let node = &value["runtime"]["observations"][0];
    assert_eq!(node["name"], "worker");
    assert_eq!(node["observation_reason"], "endpoint_unregistered");
    let action = node["next_action"].as_str().unwrap();
    assert!(action.contains("--team 'current'"));
    assert!(action.contains(&*workspace.to_string_lossy()));
    assert_eq!(std::fs::read(state_path).unwrap(), before);
}
