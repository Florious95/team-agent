use super::*;

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
fn run_dispatches_send_to_handler_returns_ok() {
    // run([send w1 hello --workspace <seeded>]) -> cmd_send -> messaging::send_message
    // -> ok (w1 is an in-team agent) -> ExitCode::Ok. w1 must be seeded: golden refuses non-team targets.
    //
    // OLD seed: flat `{"agents": {"w1": ...}}`.
    // NEW seed (Bug 1/2 — team-in-team state scope, see
    // tests/team_in_team_state_scope_red.rs): send_message projects the raw state
    // through the active team_key, so the in-team agent now lives under
    // `teams[<key>].agents`. Behavior under test (dispatch + ok exit) unchanged.
    let ws = deleg_uniq_dir("run-send");
    let _ = crate::message_store::MessageStore::open(&ws).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "active_team_key": "current",
            "teams": {"current": {"agents": {"w1": {"provider": "codex"}}}}
        }),
    )
    .unwrap();
    let argv = vec![
        "send".to_string(),
        "w1".to_string(),
        "hello".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().to_string(),
    ];
    assert_eq!(
        run(&argv, Path::new(".")),
        ExitCode::Ok,
        "run must dispatch `send` to cmd_send and derive ExitCode::Ok from the delivery outcome"
    );
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
fn run_dispatches_coordinator_once_writes_meta() {
    // run([coordinator --workspace <ws> --once]) -> run_daemon(DaemonArgs{once:true}) -> writes the
    // coordinator boot metadata + ExitCode::Ok. (real machine — drives a real backend.)
    let ws = seed_status_workspace();
    let argv = vec![
        "coordinator".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().to_string(),
        "--once".to_string(),
    ];
    let exit = run(&argv, Path::new("."));
    let wp = crate::coordinator::WorkspacePath::new(ws);
    assert!(
        crate::coordinator::coordinator_meta_path(&wp).exists(),
        "coordinator --once must dispatch to run_daemon and write the coordinator metadata"
    );
    assert_eq!(exit, ExitCode::Ok);
}

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
            "---\nname: implementer\nrole: Impl\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nImpl.\n",
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
fn cli_quick_start_invokes_real_lifecycle_compiles_spec() {
    let team = deleg_team_dir_with_healthy_coordinator();
    let ws = crate::model::paths::team_workspace(&team).unwrap();
    let _guard = TeamSocketGuard { ws };
    let args = QuickStartArgs {
        workspace: crate::model::paths::team_workspace(&team).unwrap(),
        agents_dir: team.clone(),
        name: None,
        team_id: None,
        yes: true,
        no_display: false,
        backend: None,
        json: true,
    };
    let _ = cmd_quick_start(&args); // real quick_start compiles the spec before any coordinator/launch step
                                    // E5: spec compiled to .team/runtime/<team_key>/, NOT the user team dir.
    let team_key = "clidelegteam";
    let workspace = crate::model::paths::team_workspace(&team).unwrap();
    let runtime_spec = crate::model::paths::runtime_spec_path(&workspace, &team_key);
    assert!(
            runtime_spec.exists(),
            "cmd_quick_start must delegate to crate::lifecycle::quick_start, which compiles team.spec.yaml \
             under .team/runtime/<team_key>/; the placeholder never writes it"
        );
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
        no_display: false,
        backend: None,
        json: true,
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
                "implementer": {"status": "running", "provider": "codex", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    let dup_role = team.join("dup-role.md");
    std::fs::write(&dup_role, DELEG_VALID_ROLE).unwrap(); // role file must exist
    let args = AddAgentArgs {
        agent: "implementer".to_string(),
        workspace: team,
        team: None,
        role_file: dup_role.to_string_lossy().to_string(),
        force: false,
        no_display: false,
        json: true,
    };
    let text = outcome_text(cmd_add_agent(&args));
    assert!(
            text.contains("already exists"),
            "cmd_add_agent must surface the REAL crate::lifecycle::add_agent 'agent id already exists' error \
             (not the placeholder's canned ok:true); got {text}"
        );
}

// 5 [P1] — cmd_status --json reflects the REAL coordinator_health + real DB message/result counts.
// The placeholder status_port::status hardcodes coordinator={status:stopped, schema_ok:false} and
// messages/results={count:0}. Seed: state.json + a real team.db (2 messages + 1 result) + a healthy
// coordinator (this pid + metadata) -> the real path must surface running-coordinator + non-zero
// counts. OS-safe (all reads/seeds, no spawn).
#[test]
fn cli_status_reflects_real_coordinator_health_and_db_counts() {
    let ws = seed_status_workspace(); // writes .team/runtime/state.json
    let store = crate::message_store::MessageStore::open(&ws).unwrap();
    let _ = store
        .create_message(Some("t1"), "leader", "a1", "hi", None, true, None)
        .unwrap();
    let _ = store
        .create_message(Some("t1"), "leader", "a1", "again", None, true, None)
        .unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
            "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at) \
             values ('r1', null, 't1', 'a1', '{}', 'success', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    drop(store);
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
    assert_ne!(
        value["coordinator"],
        json!({"status": "stopped", "schema_ok": false}),
        "status `coordinator` must reflect the REAL coordinator_health (seeded running), not the \
             placeholder's hardcoded {{status:stopped, schema_ok:false}}; got {}",
        value["coordinator"]
    );
    assert_ne!(
            value["messages"],
            json!({"count": 0}),
            "status `messages` must reflect the REAL DB rows (2 seeded), not the placeholder's {{count:0}}; got {}",
            value["messages"]
        );
    assert_ne!(
            value["results"],
            json!({"count": 0}),
            "status `results` must reflect the REAL DB result row, not the placeholder's {{count:0}}; got {}",
            value["results"]
        );
}

// 6 [P1] — cmd_shutdown delegates to the REAL crate::coordinator::stop_coordinator (+ tmux
// kill-session). The placeholder lifecycle_port::shutdown returns only {ok:true, keep_logs, team}.
// #[ignore] real-machine: the real shutdown spawns `tmux kill-session` (and SIGTERMs the coordinator);
// on a no-coordinator workspace the real stop_coordinator outcome (stopped/missing) must surface.
#[test]
#[ignore = "real-machine: cmd_shutdown runs the real tmux kill-session + SIGTERMs the coordinator daemon"]
fn cli_shutdown_invokes_real_stop_coordinator() {
    let ws = seed_status_workspace();
    let args = ShutdownArgs {
        workspace: ws,
        team: None,
        keep_logs: true,
        json: true,
    };
    let text = outcome_text(cmd_shutdown(&args));
    let lower = text.to_lowercase();
    assert!(
            lower.contains("coordinator") || lower.contains("stopped") || lower.contains("missing"),
            "cmd_shutdown must delegate to the real coordinator::stop_coordinator and surface its outcome \
             (no coordinator -> stopped/missing); the placeholder returns only {{ok,keep_logs,team}}; got {text}"
        );
}
