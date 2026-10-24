#![allow(clippy::unwrap_used, clippy::panic)]
use super::super::tests::Sandbox;
use super::*;

#[cfg(unix)]
#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;

fn fixture(ids: &[&str]) -> (Sandbox, Intent) {
    let root = Sandbox::new();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("TEAM.md"),
        "---\nname: fixture\nprovider: pi\n---\nFixture\n",
    )
    .unwrap();
    for id in ids {
        write_role(root.path(), id);
    }
    let mut intent = super::super::tests::intent(Verb::Start);
    intent.workspace = root.path().to_owned();
    intent.subject = Subject::Team { source: None };
    intent.team = Some("fixture".into());
    (root, intent)
}
fn write_role(root: &Path, id: &str) {
    std::fs::write(root.join("agents").join(format!("{id}.md")),format!("---\nname: {id}\nprovider: pi\nmodel: openai-codex/gpt-6-luna\neffort: max\ndangerously_skip_permissions: true\n---\nFixture\n")).unwrap();
}
fn install(root: &Path, ids: &[(&str, &str, bool)]) {
    let agents:serde_json::Map<String,Value>=ids.iter().map(|(id,status,explicit)|((*id).into(),json!({"provider":"pi","status":status,"explicit_stop":explicit,"pane_pid":std::process::id(),"pane_id":format!("%{id}"),"window":id}))).collect();
    let entry = json!({"team_key":"fixture","team_dir":root,"status":"alive","session_name":"team-fixture","agents":agents,"tasks":{}});
    let mut raw = entry.clone();
    raw["active_team_key"] = json!("fixture");
    raw["teams"] = json!({"fixture":entry});
    let state = crate::state::persist::runtime_state_path(root);
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let spec = crate::model::paths::runtime_spec_path(root, "fixture");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    std::fs::write(
        spec,
        yaml::dumps(&crate::compiler::compile_team(root).unwrap()),
    )
    .unwrap();
}
fn actions(plan: &Plan) -> Vec<(&str, Option<&str>)> {
    plan.steps.iter().map(|s| (s.name(), s.agent())).collect()
}
#[test]
#[serial_test::serial(env)]
fn absent_source_is_one_original_create_not_restart_or_probe() {
    let (root, intent) = fixture(&["one", "two"]);
    let plan = plan_with_observer(intent, |_, _, _| panic!("fresh team probed")).unwrap();
    assert_eq!(actions(&plan), [("quick_start", None)]);
    assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
}
#[test]
#[serial_test::serial(env)]
fn startup_spec_red_stopped_team_uses_start_without_restart_or_planning_writes() {
    let (root, intent) = fixture(&["one", "two"]);
    install(
        root.path(),
        &[("one", "stopped", true), ("two", "stopped", false)],
    );
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    let state = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state).unwrap();
    let result = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified));
    assert_eq!(std::fs::read(&state).unwrap(), before);
    assert!(
        !spec.exists(),
        "readonly planning must not rebuild the spec"
    );
    let plan = result.expect("complete roles must provide a noncircular start plan when only the derived spec is missing");
    assert_eq!(
        plan.steps
            .iter()
            .filter_map(|step| match step {
                Step::StartAgent(args) => Some(args.agent.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert!(!plan
        .steps
        .iter()
        .any(|step| matches!(step, Step::RestartTeam(_) | Step::StopAgent(_))));
}

#[test]
#[serial_test::serial(env)]
fn startup_spec_red_live_team_recovers_metadata_without_process_restart() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    let state = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state).unwrap();
    let result = plan_with_observer(intent, |_, _, _| Ok(Observation::Live));
    assert_eq!(std::fs::read(&state).unwrap(), before);
    assert!(!spec.exists());
    let plan =
        result.expect("missing derived metadata must not require restarting a verified live seat");
    assert!(!plan.steps.iter().any(|step| matches!(
        step,
        Step::StartAgent(_) | Step::StopAgent(_) | Step::RestartTeam(_) | Step::CreateTeam(_)
    )));
    assert!(plan
        .skipped
        .iter()
        .any(|entry| entry["reason"] == "already_running"));
}

#[test]
#[serial_test::serial(env)]
fn startup_spec_repair_executes_only_metadata_and_preserves_seat_identity() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", true)]);
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    let state = crate::state::persist::runtime_state_path(root.path());
    let before: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    let repair = plan
        .steps
        .iter()
        .find(|step| matches!(step, Step::RestoreSpec { .. }))
        .unwrap();
    for _ in 0..2 {
        recheck(&plan, repair, false).unwrap();
        assert_eq!(repair.execute().unwrap().exit, ExitCode::Ok);
        assert!(spec.is_file());
        let after: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        assert_eq!(after["teams"]["fixture"]["spec_path"], json!(spec));
        assert_eq!(
            relevant(&after["teams"]["fixture"]["agents"]["one"]),
            relevant(&before["teams"]["fixture"]["agents"]["one"])
        );
        assert_eq!(
            after["teams"]["fixture"]["agents"]["one"]["explicit_stop"],
            true
        );
    }
}

#[test]
#[serial_test::serial(env)]
fn startup_spec_repair_rechecks_source_under_lock_and_unknown_stays_blocked() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", true)]);
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    assert!(plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::Unknown)).is_err());
    assert!(!spec.exists());
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    let repair = plan
        .steps
        .iter()
        .find(|step| matches!(step, Step::RestoreSpec { .. }))
        .unwrap();
    let state = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state).unwrap();
    let role = root.path().join("agents/one.md");
    let changed = std::fs::read_to_string(&role)
        .unwrap()
        .replace("Fixture", "Changed role body");
    std::fs::write(role, changed).unwrap();
    assert!(
        repair.execute().is_err(),
        "the executor must not rely only on the earlier planner recheck"
    );
    assert_eq!(std::fs::read(state).unwrap(), before);
    assert!(!spec.exists());
}

#[test]
#[serial_test::serial(env)]
fn startup_spec_missing_restart_still_preserves_explicit_stop() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", true)]);
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    intent.verb = Verb::Restart;
    let plan =
        plan_with_observer(intent, |_, _, _| panic!("held seat must not be probed")).unwrap();
    assert!(plan.steps.is_empty());
    assert!(plan.no_eligible.is_some());
    assert_eq!(plan.skipped[0]["reason"], "explicit_stop");
    assert!(!spec.exists());
}

#[test]
#[serial_test::serial(env)]
fn stopped_team_starts_both_and_mixed_start_leaves_live_peer_untouched() {
    let (root, intent) = fixture(&["one", "two"]);
    install(
        root.path(),
        &[("one", "stopped", true), ("two", "stopped", false)],
    );
    let plan =
        plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    assert_eq!(
        actions(&plan),
        [("start_agent", Some("one")), ("start_agent", Some("two"))]
    );
    assert_eq!(plan.scope.team.as_deref(), Some("fixture"));
    install(
        root.path(),
        &[("one", "running", false), ("two", "stopped", false)],
    );
    let plan = plan_with_observer(intent, |_, id, _| {
        Ok(if id == "one" {
            Observation::Live
        } else {
            Observation::StoppedVerified
        })
    })
    .unwrap();
    assert_eq!(actions(&plan), [("start_agent", Some("two"))]);
    assert_eq!(plan.skipped[0]["reason"], "already_running");
}
#[test]
#[serial_test::serial(env)]
fn verified_all_live_is_noop_but_known_drift_and_unknown_refuse() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    assert!(
        plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::Live))
            .unwrap()
            .steps
            .is_empty()
    );
    assert!(plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::Unknown)).is_err());
    let mut patched = intent.clone();
    patched.subject = Subject::Agent(AgentId::new("one"));
    patched.patch.model = Some("new-model".into());
    assert!(plan_with_observer(patched, |_, _, _| Ok(Observation::Live)).is_err());
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    raw["teams"]["fixture"]["agents"]["one"]["as_launched_dangerously_skip_permissions"] =
        json!(false);
    std::fs::write(state, serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(plan_with_observer(intent, |_, _, _| Ok(Observation::Live)).is_err());
}
#[test]
#[serial_test::serial(env)]
fn new_restart_preserves_held_seats_and_shutdown_branch_starts_only_eligible() {
    let (root, mut intent) = fixture(&["one", "two"]);
    install(
        root.path(),
        &[("one", "stopped", true), ("two", "stopped", false)],
    );
    intent.verb = Verb::Restart;
    intent.session = SessionPolicy::Auto;
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    raw["teams"]["fixture"]["status"] = json!("shutdown");
    std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let plan = plan_with_observer(intent.clone(), |_, id, _| {
        assert_ne!(id, "one", "held seat was probed");
        Ok(Observation::StoppedVerified)
    })
    .unwrap();
    assert_eq!(actions(&plan), [("start_agent", Some("two"))]);
    assert_eq!(plan.skipped[0]["reason"], "explicit_stop");
    raw["teams"]["fixture"]["agents"]["two"]["status"] = json!("running");
    std::fs::write(state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::Live)).unwrap();
    assert_eq!(actions(&plan), [("restart_team", None)]);
    let Step::RestartTeam(args) = &plan.steps[0] else {
        panic!()
    };
    assert!(args.allow_fresh);
}
#[test]
#[serial_test::serial(env)]
fn all_paused_no_eligible_and_single_restart_never_unpauses() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "paused", false)]);
    let blocked = plan_with_observer(intent.clone(), |_, _, _| panic!("paused probe")).unwrap();
    assert!(blocked.no_eligible.is_some());
    assert!(blocked.steps.is_empty());
    assert_eq!(blocked.skipped[0]["reason"], "paused");
    intent.verb = Verb::Restart;
    intent.subject = Subject::Agent(AgentId::new("one"));
    assert!(plan_with_observer(intent, |_, _, _| panic!("paused probe")).is_err());
}
#[test]
#[serial_test::serial(env)]
fn no_eligible_restart_preserves_scoped_held_and_stopped_skip_reasons() {
    for explicit in [false, true] {
        let (root, mut intent) = fixture(&["one"]);
        install(root.path(), &[("one", "stopped", explicit)]);
        intent.verb = Verb::Restart;
        let blocked =
            plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
        assert!(blocked.no_eligible.is_some());
        assert!(blocked.steps.is_empty());
        assert_eq!(blocked.scope.team.as_deref(), Some("fixture"));
        assert_eq!(blocked.skipped[0]["agent"], "one");
        assert_eq!(
            blocked.skipped[0]["reason"],
            if explicit {
                "explicit_stop"
            } else {
                "stopped_preserved_by_original_restart"
            }
        );
    }
}

#[test]
#[serial_test::serial(env)]
fn role_only_add_and_absent_creation_need_explicit_decisions() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", false)]);
    write_role(root.path(), "two");
    intent.subject = Subject::Agent(AgentId::new("two"));
    let plan = plan_with_observer(intent.clone(), |_, _, _| panic!("unregistered probe")).unwrap();
    assert_eq!(actions(&plan), [("add_agent", Some("two"))]);
    let Step::AddAgent(args) = &plan.steps[0] else {
        panic!()
    };
    assert_eq!(
        Path::new(&args.role_file),
        root.path().join("agents/two.md")
    );
    intent.subject = Subject::Agent(AgentId::new("typo"));
    assert!(plan_with_observer(intent.clone(), |_, _, _| panic!()).is_err());
    intent.patch.provider = Some("pi".into());
    intent.patch.bypass = Some(true);
    assert!(matches!(
        plan_with_observer(intent, |_, _, _| panic!())
            .unwrap()
            .steps[0],
        Step::AddAgent(_)
    ));
}
#[test]
#[serial_test::serial(env)]
fn spec_only_state_only_and_missing_spec_do_not_fallback_to_creation() {
    let (root, mut intent) = fixture(&["one", "two"]);
    install(root.path(), &[("one", "stopped", false)]);
    assert!(
        plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::StoppedVerified)).is_err()
    );
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::remove_file(&spec).unwrap();
    assert!(
        plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::StoppedVerified)).is_err()
    );
    intent.verb = Verb::Restart;
    let blocked = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    assert!(blocked.no_eligible.is_some());
    assert!(blocked.steps.is_empty());
    assert_eq!(blocked.skipped[0]["agent"], "one");
}
#[test]
#[serial_test::serial(env)]
fn runtime_files_and_source_are_unchanged_by_dry_planning() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", true)]);
    let state = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state).unwrap();
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    assert!(!plan.steps.is_empty());
    assert_eq!(std::fs::read(state).unwrap(), before);
}
#[test]
fn discovery_never_prefers_current_over_two_distinct_sources() {
    let root = Sandbox::new();
    for name in ["one", "current"] {
        let path = root.path().join(".team").join(name);
        std::fs::create_dir_all(path.join("agents")).unwrap();
        std::fs::write(path.join("TEAM.md"), "fixture").unwrap();
    }
    assert!(discover(root.path(), None, None).is_err());
}
#[test]
#[serial_test::serial(env)]
fn explicit_source_and_relative_role_file_are_resolved_from_caller_not_workspace() {
    let (root, _) = fixture(&["one"]);
    let workspace = root.path().join("other");
    std::fs::create_dir(&workspace).unwrap();
    let args = vec![
        "--from".into(),
        ".".into(),
        "--workspace".into(),
        workspace.to_string_lossy().into_owned(),
        "--dry-run".into(),
        "--json".into(),
    ];
    let result = super::super::run("start", &args, root.path())
        .unwrap()
        .unwrap();
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(
        value["scope"]["workspace"],
        workspace.to_string_lossy().as_ref()
    );
    assert!(!workspace.join(".team").exists());
}

#[test]
#[serial_test::serial(env)]
fn genuine_bootstrap_uses_original_predicate_but_history_is_not_fresh() {
    let (root, intent) = fixture(&["one"]);
    let state = crate::state::persist::runtime_state_path(root.path());
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    let bootstrap = json!({"team_key":"fixture","agents":{},"tasks":[],"session_name":null,"team_owner":{"pane_id":"%0"},"leader_receiver":{"pane_id":"%0","status":"attached"}});
    std::fs::write(&state, serde_json::to_vec(&bootstrap).unwrap()).unwrap();
    assert_eq!(
        actions(&plan_with_observer(intent.clone(), |_, _, _| panic!()).unwrap()),
        [("quick_start", None)]
    );
    let mut history = bootstrap;
    history["tasks"] = json!({"old":{}});
    std::fs::write(&state, serde_json::to_vec(&history).unwrap()).unwrap();
    assert!(plan_with_observer(intent, |_, _, _| panic!()).is_err());
}

#[test]
#[serial_test::serial(env)]
fn preserve_rejects_known_missing_backing_before_stop_auto_uses_existing_decision() {
    let (root, _) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    let selected =
        resolve_active_team_readonly(root.path(), Some("fixture"), SelectorMode::RuntimeOnly)
            .unwrap();
    let row = json!({"provider":"pi","session_id":"missing-session","rollout_path":root.path().join("missing.jsonl"),"spawn_cwd":root.path()});
    assert!(preflight_start(&selected, "one", &row, SessionPolicy::Preserve).is_err());
    assert!(preflight_start(&selected, "one", &row, SessionPolicy::Auto).is_ok());
    assert!(
        preflight_start(
            &selected,
            "one",
            &json!({"provider":"pi"}),
            SessionPolicy::Preserve
        )
        .is_ok(),
        "first fresh has no captured context to discard"
    );
    for policy in [SessionPolicy::Auto, SessionPolicy::Preserve] {
        assert!(
            preflight_start(&selected, "one", &json!({"provider":"cursor"}), policy).is_err(),
            "unadmitted native resume cannot fall back to legacy/fresh"
        );
    }
}

#[cfg(unix)]
fn cursor_scope_fixture() -> (
    hermetic_guard::HermeticTestEnv,
    Sandbox,
    SelectedTeam,
    PathBuf,
    Value,
) {
    let env = hermetic_guard::HermeticTestEnv::enter("cursor-intent-scope");
    let (root, _) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", true)]);
    let selected =
        resolve_active_team_readonly(root.path(), Some("fixture"), SelectorMode::RuntimeOnly)
            .unwrap();
    let config = crate::provider::McpConfig {
        raw: json!({"team_orchestrator": {
            "command": std::env::current_exe().unwrap(),
            "args": ["mcp-server", "--workspace", root.path()],
            "env": {"TEAM_AGENT_WORKSPACE": root.path(), "TEAM_AGENT_ID": "one", "TEAM_AGENT_OWNER_TEAM_ID": "fixture", "TEAM_AGENT_AUTH_MODE": "subscription"}
        }}),
    };
    let primary = crate::lifecycle::launch::prepare_cursor_seat_mcp(root.path(), "one", &config)
        .unwrap()
        .unwrap();
    let sid = "af3ce55e-2ee9-4bc5-8dae-b628e7447286";
    let archive = env
        .home()
        .join(".cursor/chats")
        .join(format!(
            "{:x}",
            md5::compute(primary.to_string_lossy().as_bytes())
        ))
        .join(sid);
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join("meta.json"), b"{}").unwrap();
    let row = json!({"provider":"cursor_agent","status":"stopped","explicit_stop":true,"session_id":sid,"rollout_path":archive,"spawn_cwd":primary});
    (env, root, selected, primary, row)
}

#[test]
#[cfg(unix)]
#[serial_test::serial(env)]
fn issue342_cursor_owned_primary_scope_is_readonly_and_exact() {
    let (_env, root, selected, primary, row) = cursor_scope_fixture();
    let state = crate::state::persist::runtime_state_path(root.path());
    let record = root
        .path()
        .join(".team/runtime/provider-config/one/cursor-project.json");
    let paths = [state, record, primary.join(".cursor/mcp.json")];
    let before: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert!(!primary.starts_with(root.path()));
    for policy in [SessionPolicy::Preserve, SessionPolicy::Auto] {
        assert!(preflight_start(&selected, "one", &row, policy).is_ok());
    }
    let after: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert_eq!(
        before, after,
        "scope preflight must not write state, record or MCP config"
    );

    let mut foreign_provider = row.clone();
    foreign_provider["provider"] = json!("pi");
    assert!(preflight_start(&selected, "one", &foreign_provider, SessionPolicy::Auto).is_err());
    let mut other_team = selected.clone();
    other_team.team_key = "foreign-team".into();
    assert!(preflight_start(&other_team, "one", &row, SessionPolicy::Preserve).is_err());
    assert!(preflight_start(&selected, "two", &row, SessionPolicy::Preserve).is_err());
    let outside = Sandbox::new();
    let mut other_workspace = selected.clone();
    other_workspace.run_workspace = outside.path().to_owned();
    assert!(preflight_start(&other_workspace, "one", &row, SessionPolicy::Preserve).is_err());
    assert!(!outside.path().join(".team").exists());
}

#[test]
#[cfg(unix)]
#[serial_test::serial(env)]
fn issue342_cursor_scope_refuses_unrecorded_alias_and_replaced_incarnation() {
    use std::os::unix::fs::{symlink, DirBuilderExt};
    let (_env, root, selected, primary, row) = cursor_scope_fixture();
    let record = root
        .path()
        .join(".team/runtime/provider-config/one/cursor-project.json");
    let bytes = std::fs::read(&record).unwrap();
    let retained = record.with_extension("retained");
    std::fs::rename(&record, &retained).unwrap();
    assert!(preflight_start(&selected, "one", &row, SessionPolicy::Preserve).is_err());
    assert!(
        !record.exists(),
        "a same-named external directory is not authority to create a record"
    );
    std::fs::rename(retained, &record).unwrap();
    let mut forged = row.clone();
    let arbitrary = Sandbox::new();
    forged["spawn_cwd"] = json!(arbitrary.path());
    assert!(preflight_start(&selected, "one", &forged, SessionPolicy::Auto).is_err());
    let alias = root.path().join("primary-alias");
    symlink(&primary, &alias).unwrap();
    forged["spawn_cwd"] = json!(alias);
    assert!(preflight_start(&selected, "one", &forged, SessionPolicy::Preserve).is_err());
    let old = primary.with_extension("retained");
    std::fs::rename(&primary, old).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&primary)
        .unwrap();
    assert!(preflight_start(&selected, "one", &row, SessionPolicy::Auto).is_err());
    assert_eq!(std::fs::read(record).unwrap(), bytes);
}

#[test]
#[serial_test::serial(env)]
fn changed_role_and_socket_refuse_in_the_real_readonly_recheck() {
    let (root, intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", false)]);
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    let path = root.path().join("agents/one.md");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"changed role").unwrap();
    assert!(recheck(&plan, &plan.steps[0], false).is_err());
    std::fs::write(path, original).unwrap();
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    raw["teams"]["fixture"]["tmux_socket"] = json!("/different/endpoint");
    std::fs::write(state, serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(recheck(&plan, &plan.steps[0], false).is_err());
}

#[test]
#[serial_test::serial(env)]
fn creation_rejects_an_added_role_before_the_original_create_handler() {
    let (root, intent) = fixture(&["one"]);
    let plan = plan_with_observer(intent, |_, _, _| panic!()).unwrap();
    write_role(root.path(), "two");
    assert!(recheck(&plan, &plan.steps[0], false).is_err());
    assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
}

#[test]
#[serial_test::serial(env)]
fn stopped_text_without_physical_identity_is_unknown_and_standalone_source_keeps_its_workspace() {
    let (root, _) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", false)]);
    let selected =
        resolve_active_team_readonly(root.path(), Some("fixture"), SelectorMode::RuntimeOnly)
            .unwrap();
    assert_eq!(
        observe(
            &selected,
            "one",
            &json!({"status":"stopped","provider":"pi"})
        )
        .unwrap(),
        Observation::Unknown
    );
    assert_eq!(
        observe(
            &selected,
            "one",
            &json!({"status":"stopped","provider":"pi","pane_pid":std::process::id()})
        )
        .unwrap(),
        Observation::Unknown
    );
    let (root, _) = fixture(&["one"]);
    let values = vec![
        "--from".into(),
        root.path().to_string_lossy().into_owned(),
        "--dry-run".into(),
        "--json".into(),
    ];
    let result = super::super::run("start", &values, root.path())
        .unwrap()
        .unwrap();
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(
        value["scope"]["workspace"],
        root.path().to_string_lossy().as_ref()
    );
}

#[test]
fn declared_provider_aliases_do_not_create_false_drift_or_absorb_native_routes() {
    assert!(!drift(
        &json!({"provider":"agent"}),
        &json!({"provider":"cursor_agent"})
    ));
    assert!(same_provider("claude-code", "claude_code"));
    assert!(!same_provider("cursor", "cursor_agent"));
    assert!(!same_provider("grok_build", "grok"));
}

#[test]
#[cfg(unix)]
fn native_noop_compares_all_five_authoritative_launch_settings() {
    use crate::contract_runtime::{config::RoleConfig, prompt::Prompt};
    use team_agent_contract::contract::types::{Digest, ProviderId, SeatId};
    let old = RoleConfig {
        id: SeatId::new("one").unwrap(),
        provider: ProviderId::new("cursor").unwrap(),
        source: PathBuf::from("/fixture/one.md"),
        role: "one".into(),
        model: "original".into(),
        effort: None,
        bypass: true,
        paused: false,
        body: "body".into(),
        prompt: Prompt {
            text: String::new(),
            sha256: Digest([0; 32]),
            sources: vec![],
        },
    };
    assert!(!native_settings_changed(&old, &old.clone()));
    for field in ["provider", "model", "effort", "bypass", "paused"] {
        let mut current = old.clone();
        match field {
            "provider" => current.provider = ProviderId::new("kiro").unwrap(),
            "model" => current.model = "changed".into(),
            "effort" => current.effort = Some("high".into()),
            "bypass" => current.bypass = false,
            _ => current.paused = true,
        }
        assert!(
            native_settings_changed(&old, &current),
            "unexamined launch drift: {field}"
        );
    }
}

#[test]
fn original_supervised_shell_needs_both_current_native_frame_and_live_child() {
    for shell in ["sh", "bash", "zsh", "fish"] {
        assert!(surface_liveness(shell, true, true));
        assert!(
            !surface_liveness(shell, true, false),
            "stale native screen without child"
        );
        assert!(
            !surface_liveness(shell, false, true),
            "inert tail sleep is not a provider"
        );
        assert!(!surface_liveness(shell, false, false));
    }
    assert!(!surface_liveness("", true, true));
    assert!(surface_liveness("node", true, false));
    assert!(!surface_liveness("node", false, true));
}

#[test]
fn unchanged_observation_port_rejects_native_screen_followed_by_exit_tail() {
    use crate::model::enums::Provider;
    use crate::transport::native_input::inspect_input;
    let native = "────────────────────────\n\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n";
    assert!(inspect_input(Provider::Pi, native, 24).is_some());
    for tail in ["[team-agent worker] pi exited with 1\n", "[team-agent] Provider exited; this pane no longer accepts input. Restart from another pane with the appropriate team-agent start command.\n", "$ "] {
        assert!(inspect_input(Provider::Pi, &format!("{native}{tail}"), 24).is_none());
    }
}

#[test]
#[cfg(unix)]
fn native_kernel_exit_can_recover_ready_but_mismatched_liveness_never_noops() {
    use team_agent_contract::{
        host::process::ProcessState as P, orchestration::store::SeatStatus as S,
    };
    for (status, process, root, expected) in [
        (S::Ready, P::Alive, "running", Observation::Live),
        (S::Ready, P::Exited, "running", Observation::StoppedVerified),
        (
            S::Stopped,
            P::Exited,
            "stopped",
            Observation::StoppedVerified,
        ),
        (S::Ready, P::Alive, "stopped", Observation::Unknown),
        (S::Stopped, P::Alive, "running", Observation::Unknown),
        (S::Stopped, P::Exited, "running", Observation::Unknown),
        (S::Ready, P::Replaced, "running", Observation::Unknown),
        (
            S::Ready,
            P::Unknown(team_agent_contract::host::HostError::new(
                "fixture",
                team_agent_contract::host::HostErrorKind::Unknown,
            )),
            "running",
            Observation::Unknown,
        ),
    ] {
        assert_eq!(native_observation(status, process, Some(root)), expected);
    }
}

#[test]
#[serial_test::serial(env)]
fn preserve_context_preflight_and_corrupt_metadata_refuse_before_stop() {
    let (root, _) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    let selected =
        resolve_active_team_readonly(root.path(), Some("fixture"), SelectorMode::RuntimeOnly)
            .unwrap();
    for signal in [
        json!({"first_send_at":"2026-10-19T00:00:00Z"}),
        json!({"task_prompt_delivered":true}),
        json!({"last_result_at":"2026-10-19T00:00:00Z"}),
    ] {
        let mut row = json!({"provider":"pi"});
        row.as_object_mut()
            .unwrap()
            .extend(signal.as_object().unwrap().clone());
        assert!(preflight_start(&selected, "one", &row, SessionPolicy::Preserve).is_err());
        assert!(preflight_start(&selected, "one", &row, SessionPolicy::Auto).is_ok());
    }
    for field in [
        "session_id",
        "rollout_path",
        "last_result_at",
        "spawn_cwd",
        "first_send_at",
        "task_prompt_delivered",
    ] {
        let mut row = json!({"provider":"pi"});
        row[field] = json!([]);
        for policy in [SessionPolicy::Auto, SessionPolicy::Preserve] {
            assert!(
                preflight_start(&selected, "one", &row, policy).is_err(),
                "corrupt {field}"
            );
        }
    }
    let outside = Sandbox::new();
    assert!(preflight_start(
        &selected,
        "one",
        &json!({"provider":"pi", "spawn_cwd":outside.path()}),
        SessionPolicy::Auto
    )
    .is_err());
    assert!(preflight_start(
        &selected,
        "one",
        &json!({"provider":"pi", "session_id":"captured"}),
        SessionPolicy::Auto
    )
    .is_err());
}

#[test]
#[serial_test::serial(env)]
fn pinned_child_scope_never_inherits_parent_runtime_and_spec_only_never_creates() {
    let (parent, _) = fixture(&["one"]);
    install(parent.path(), &[("one", "running", false)]);
    let child = parent.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::create_dir(child.join("agents")).unwrap();
    std::fs::write(
        child.join("TEAM.md"),
        "---\nname: child\nprovider: pi\n---\n",
    )
    .unwrap();
    write_role(&child, "two");
    let mut intent = super::super::tests::intent(Verb::Start);
    intent.workspace = child.clone();
    intent.team = None;
    intent.subject = Subject::Team {
        source: Some(child.clone()),
    };
    let plan = plan_with_observer(intent.clone(), |_, _, _| {
        panic!("parent must not be selected")
    })
    .unwrap();
    assert_eq!(plan.scope.workspace, child);
    assert_eq!(actions(&plan), [("quick_start", None)]);
    let path = crate::model::paths::runtime_spec_path(&child, "orphan");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "agents: []\n").unwrap();
    assert!(plan_with_observer(intent, |_, _, _| panic!()).is_err());
    assert!(!crate::state::persist::runtime_state_path(&child).exists());
}
