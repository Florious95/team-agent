#![allow(clippy::unwrap_used, clippy::panic)]
use super::super::tests::Sandbox;
use super::*;

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
    std::fs::write(state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let plan = plan_with_observer(intent.clone(), |_, id, _| {
        assert_ne!(id, "one", "held seat was probed");
        Ok(Observation::StoppedVerified)
    })
    .unwrap();
    assert_eq!(actions(&plan), [("start_agent", Some("two"))]);
    assert_eq!(plan.skipped[0]["reason"], "explicit_stop");
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
    assert!(plan_with_observer(intent.clone(), |_, _, _| panic!("paused probe")).is_err());
    intent.verb = Verb::Restart;
    intent.subject = Subject::Agent(AgentId::new("one"));
    assert!(plan_with_observer(intent, |_, _, _| panic!("paused probe")).is_err());
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
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::StoppedVerified));
    assert!(plan.is_err());
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
