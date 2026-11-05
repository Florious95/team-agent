#![allow(clippy::unwrap_used, clippy::panic)]
use super::super::tests::Sandbox;
use super::*;

#[test]
fn start_help_and_source_errors_include_usable_optional_templates() {
    let root = Sandbox::new();
    let help = super::super::help("start");
    let usage = help.split("Usage:").nth(1).unwrap().split("Options:").next().unwrap();
    for flag in ["--team NAME", "--provider PROVIDER", "--model MODEL", "--effort EFFORT", "--prompt TEXT"] {
        assert!(usage.contains(flag), "missing {flag}: {usage}");
    }
    assert!(usage.contains("creates the team if it does not exist"));
    assert!(help.contains("--team NAME selects a team and creates it if it does not exist"));
    assert!(help.contains("team-agent start reviewer --team release-review --provider pi"));
    for explicit in [None, Some(root.path())] {
        let error = discover(root.path(), explicit, None).unwrap_err().to_string();
        assert!(error.contains("No files are required for direct command-line start"));
        assert!(error.contains("TEAM.md:\n---\nname: release-review\n---"));
        assert!(error.contains("agents/reviewer.md:\n---\nprovider: pi\n---"));
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for file in ["TEAM.md", "agents/reviewer.md"] {
        let marker = format!("{file}:\n");
        let template = help.split_once(&marker).unwrap().1.lines().take(3).collect::<Vec<_>>().join("\n");
        std::fs::write(root.path().join(file), format!("{template}\n")).unwrap();
    }
    let spec = crate::compiler::compile_team(root.path()).expect("help templates must compile");
    assert_eq!(spec.get("agents").unwrap().as_list().unwrap()[0].get("id").unwrap().as_str(), Some("reviewer"));
}

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
#[test]
#[serial_test::serial(env)]
fn batch_start_does_not_adopt_an_unrelated_owner_change_as_recovery() {
    let (root, intent) = fixture(&["one", "two"]);
    install(root.path(), &[("one", "stopped", false), ("two", "stopped", false)]);
    let path = crate::state::persist::runtime_state_path(root.path());
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let stopped_pid = child.id();
    assert!(child.wait().unwrap().success());
    for id in ["one", "two"] {
        state["agents"][id]["pane_pid"] = json!(stopped_pid);
        state["teams"]["fixture"]["agents"][id]["pane_pid"] = json!(stopped_pid);
    }
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let mut planned = plan(intent).unwrap();
    let step = planned.steps.iter().find(|step| matches!(step, Step::StartAgent(_))).unwrap().clone();
    state["teams"]["fixture"]["team_owner"] = json!({"pane_id":"%foreign", "owner_epoch":2});
    std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
    let baseline = planned.topology.clone();
    for receipt in [json!({"runtime":"contract", "ok":true}), json!({"ok":true, "status":"running"})] {
        let error = advance_topology(&mut planned, &step, &receipt).unwrap_err();
        assert!(error.to_string().contains("owner changed during batch startup"));
        assert_eq!(planned.topology, baseline);
    }
}

#[test]
fn batch_start_accepts_only_its_exact_scoped_leader_claim() {
    let scope = ResolvedScope { workspace: PathBuf::from("/project"), team: Some("fixture".into()) };
    let owner = json!({
        "pane_id":"%2", "provider":"pi", "owner_epoch":2,
        "machine_fingerprint":"host", "leader_session_uuid":"uuid",
    });
    let next = topology(&json!({"team_owner":owner, "owner_epoch":2}));
    let receipt = json!({"leader_claim_receipt":{
        "workspace":"/project", "team":"fixture", "owner":owner, "owner_epoch":2,
        "receiver":{"pane_id":"%2", "tmux_socket":"/owned/socket", "session_name":"leader"},
    }});
    assert!(claim_matches_topology(&scope, &next, &receipt));
    assert!(!claim_matches_topology(&scope, &next, &json!({"leader_bind_ok":true})));
    for pointer in ["/owner/pane_id", "/owner/provider", "/owner/leader_session_uuid", "/owner/tmux_socket", "/owner/session_name", "/owner_epoch"] {
        let mut changed = next.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("foreign");
        assert!(!claim_matches_topology(&scope, &changed, &receipt), "{pointer}");
    }
    for key in ["workspace", "team", "owner_epoch"] {
        let mut changed = receipt.clone();
        changed["leader_claim_receipt"][key] = json!("foreign");
        assert!(!claim_matches_topology(&scope, &next, &changed), "{key}");
    }
}

#[test]
fn legacy_claim_allows_only_receiver_proven_absent_metadata_backfill() {
    let scope = ResolvedScope { workspace: PathBuf::from("/project"), team: Some("fixture".into()) };
    let previous = topology(&json!({"owner_epoch":1, "team_owner":{
        "pane_id":"%0", "provider":"claude_code", "owner_epoch":1, "leader_session_uuid":"uuid",
    }}));
    let mut next = previous.clone();
    next["owner"]["tmux_socket"] = json!("/owned/socket");
    let receipt = json!({"leader_claim_receipt":{
        "workspace":"/project", "team":"fixture", "owner":null, "owner_epoch":1,
        "receiver":{"pane_id":"%0", "provider":"claude_code", "owner_epoch":1,
            "leader_session_uuid":"uuid", "tmux_socket":"/owned/socket", "session_name":"leader"},
    }});
    assert!(!claim_matches_topology(&scope, &next, &receipt));
    assert!(legacy_claim_backfills_topology(&scope, &previous, &next, &receipt));
    for key in ["pane_id", "provider", "owner_epoch", "leader_session_uuid", "machine_fingerprint", "tmux_socket"] {
        let mut changed = next.clone();
        changed["owner"][key] = json!("foreign");
        assert!(!legacy_claim_backfills_topology(&scope, &previous, &changed, &receipt), "{key}");
    }
    for pointer in ["/leader_claim_receipt/workspace", "/leader_claim_receipt/team",
        "/leader_claim_receipt/receiver/pane_id", "/leader_claim_receipt/receiver/provider",
        "/leader_claim_receipt/receiver/owner_epoch", "/leader_claim_receipt/receiver/leader_session_uuid",
        "/leader_claim_receipt/receiver/tmux_socket"] {
        let mut changed = receipt.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("foreign");
        assert!(!legacy_claim_backfills_topology(&scope, &previous, &next, &changed), "{pointer}");
    }
    let mut occupied = previous.clone();
    occupied["owner"]["tmux_socket"] = json!("/previous/socket");
    assert!(!legacy_claim_backfills_topology(&scope, &occupied, &next, &receipt));
    let mut missing_projection_epoch = previous.clone();
    missing_projection_epoch["owner_epoch"] = Value::Null;
    assert!(legacy_claim_backfills_topology(&scope, &missing_projection_epoch, &next, &receipt));
}

#[test]
fn legacy_claim_normalizes_claude_aliases_but_not_foreign_providers() {
    let scope = ResolvedScope { workspace: PathBuf::from("/project"), team: Some("fixture".into()) };
    for old in ["claude", "claude_code", "claude-code"] {
        let previous = topology(&json!({"owner_epoch":1, "team_owner":{
            "pane_id":"%0", "provider":old, "owner_epoch":1, "leader_session_uuid":"uuid",
        }}));
        for current in ["claude", "claude_code", "claude-code"] {
            let mut next = previous.clone();
            next["owner"]["provider"] = json!(current);
            next["owner"]["tmux_socket"] = json!("/owned/socket");
            let receipt = json!({"leader_claim_receipt":{
                "workspace":"/project", "team":"fixture", "owner":null, "owner_epoch":1,
                "receiver":{"pane_id":"%0", "provider":"claude_code", "owner_epoch":1,
                    "leader_session_uuid":"uuid", "tmux_socket":"/owned/socket"},
            }});
            assert!(legacy_claim_backfills_topology(&scope, &previous, &next, &receipt), "{old} -> {current}");
            for foreign in ["codex", "pi", "unknown-provider", ""] {
                let mut changed = next.clone();
                changed["owner"]["provider"] = json!(foreign);
                assert!(!legacy_claim_backfills_topology(&scope, &previous, &changed, &receipt));
                let mut changed = receipt.clone();
                changed["leader_claim_receipt"]["receiver"]["provider"] = json!(foreign);
                assert!(!legacy_claim_backfills_topology(&scope, &previous, &next, &changed));
            }
        }
    }
}

#[test]
fn legacy_start_receipt_preserves_the_committed_carrier_across_own_claim() {
    let mut state = json!({
        "session_name":"owned", "generation":"session-1", "owner_epoch":1,
        "agents":{"one":{"provider":"fake", "pane_id":"%2", "pane_pid":42, "spawned_at":"birth-1"}},
    });
    let committed = crate::lifecycle::legacy_start_identity(&state, "one");
    state["owner_epoch"] = json!(2);
    assert_eq!(committed, crate::lifecycle::legacy_start_identity(&state, "one"));
    for pointer in ["/session_name", "/generation", "/agents/one/pane_id", "/agents/one/pane_pid", "/agents/one/spawned_at"] {
        let mut changed = state.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("replacement");
        assert_ne!(committed, crate::lifecycle::legacy_start_identity(&changed, "one"), "{pointer}");
    }
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
fn unverified_configuration_change_advises_scoped_stop_not_status_loop() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    intent.subject = Subject::Agent(AgentId::new("one"));
    intent.session = SessionPolicy::Fresh;
    let state = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state).unwrap();
    for observed in [Observation::Unknown, Observation::NativeReconcile] {
        let error = plan_with_observer(intent.clone(), |_, _, _| Ok(observed)).unwrap_err().to_string();
        assert!(error.contains("team-agent stop one --workspace"), "{error}");
        assert!(error.contains("--team fixture"), "{error}");
        assert!(error.contains("before applying changes or starting fresh"), "{error}");
        assert!(!error.contains("team-agent status"), "{error}");
        assert_eq!(std::fs::read(&state).unwrap(), before);
    }
}

#[test]
fn bare_and_agent_start_share_the_scoped_stop_advice_for_unverified_seats() {
    let (root, _) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    let path = crate::state::persist::runtime_state_path(root.path());
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["agents"]["one"]["pane_pid"] = Value::Null;
    state["teams"]["fixture"]["agents"]["one"]["pane_pid"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    for (command, prefix) in [("start", vec!["one"]), ("agent", vec!["start", "one"])] {
        let mut tokens = prefix.into_iter().map(str::to_string).collect::<Vec<_>>();
        tokens.extend(["--workspace".into(), root.path().to_string_lossy().into_owned(), "--team".into(), "fixture".into(), "--dry-run".into()]);
        let result = super::super::run(command, &tokens, root.path()).unwrap().unwrap();
        let text = crate::cli::emit::emit(&result.output, result.as_json).unwrap();
        assert!(text.starts_with("error: "), "{text}");
        assert!(text.contains("team-agent stop one --workspace"), "{text}");
        assert!(!text.contains("team-agent status one"), "{text}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn owned_native_reconciliation_still_allows_the_original_fenced_stop() {
    assert_eq!(seat_actions(Verb::Stop, Observation::NativeReconcile, false, false, false, false).unwrap(), ["stop"]);
    assert!(seat_actions(Verb::Stop, Observation::Unknown, false, false, false, false).is_err());
}

#[test]
#[serial_test::serial(env)]
fn team_start_skips_bad_native_preflight_but_explicit_target_still_refuses() {
    let (root, intent) = fixture(&["healthy", "invalid"]);
    std::fs::write(root.path().join("agents/invalid.md"),
        "---\nprovider: codex\nmodel: native-model\nprofile: codex-default\n---\nInvalid native profile\n").unwrap();
    install(root.path(), &[("healthy", "stopped", false), ("invalid", "stopped", false)]);
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    raw["teams"]["fixture"]["agents"]["invalid"]["provider"] = json!("codex_tui");
    std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let before = std::fs::read(&state).unwrap();
    let plan = plan_with_observer(intent.clone(), |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    assert_eq!(actions(&plan), [("start_agent", Some("healthy"))]);
    assert!(plan.skipped.iter().any(|entry| entry["agent"] == "invalid" && entry["reason"] == "preflight_blocked"));
    let mut single = intent;
    single.subject = Subject::Agent("invalid".into());
    assert!(plan_with_observer(single, |_, _, _| Ok(Observation::StoppedVerified)).is_err());
    assert_eq!(std::fs::read(&state).unwrap(), before);
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
        &[("one", "stopped", false), ("two", "stopped", false)],
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
    let (root, mut intent) = fixture(&["one"]);
    intent.subject = Subject::Agent("one".into());
    // The launched seat has no verified session; preserve would refuse before any repair.
    intent.session = SessionPolicy::Auto;
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
    install(root.path(), &[("one", "stopped", false)]);
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
        &[("one", "stopped", false), ("two", "stopped", false)],
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
fn mixed_restart_fences_the_paused_declaration_without_probing_its_live_carrier() {
    let (root, mut intent) = fixture(&["one", "two"]);
    install(root.path(), &[("one", "running", false), ("two", "running", false)]);
    let source = root.path().join("agents/one.md");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, original.replacen("---\n", "---\npaused: true\n", 1)).unwrap();
    intent.verb = Verb::Restart;
    intent.session = SessionPolicy::Auto;
    let state_path = crate::state::persist::runtime_state_path(root.path());
    let before = std::fs::read(&state_path).unwrap();
    let plan = plan_with_observer(intent, |_, id, _| {
        assert_eq!(id, "two", "declaration-paused carrier must not be probed");
        Ok(Observation::Live)
    }).unwrap();
    assert_eq!(actions(&plan), [("restart_team", None)]);
    assert_eq!(plan.skipped[0]["agent"], "one");
    assert_eq!(plan.skipped[0]["reason"], "paused");
    assert!(plan.skipped[0].get("observation").is_some());
    assert_eq!(std::fs::read(state_path).unwrap(), before);
    std::fs::write(source, original).unwrap();
    let error = recheck(&plan, &plan.steps[0], false).unwrap_err();
    assert!(error.to_string().contains("role/spec changed since planning"));
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
fn role_only_add_preserves_source_and_absent_creation_uses_defaults() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", false)]);
    write_role(root.path(), "two");
    intent.subject = Subject::Agent(AgentId::new("two"));
    let plan = plan_with_observer(intent.clone(), |_, _, _| panic!("unregistered probe")).unwrap();
    assert_eq!(actions(&plan), [("start", Some("two"))]);
    let Step::UnifiedStart { description, .. } = &plan.steps[0] else { panic!() };
    assert_eq!(description.op, "adopted");
    assert!(plan.files.iter().any(|(path, _)| path == &root.path().join("agents/two.md")));
    intent.subject = Subject::Agent(AgentId::new("new-worker"));
    let defaults = plan_with_observer(intent.clone(), |_, _, _| panic!()).unwrap();
    let Step::UnifiedStart { request, description } = &defaults.steps[0] else { panic!() };
    assert!(request.role_file.is_none());
    assert_eq!(description.op, "created");
    assert_eq!(description.declaration["provider"], "pi");
    assert_eq!(description.declaration["bypass"], false);
    assert!(description.declaration["model"].is_null());
    assert!(!root.path().join("agents/new-worker.md").exists());
    intent.patch.provider = Some("pi".into());
    intent.patch.bypass = Some(true);
    assert!(matches!(
        plan_with_observer(intent, |_, _, _| panic!())
            .unwrap()
            .steps[0],
        Step::UnifiedStart { .. }
    ));
}
#[test]
#[serial_test::serial(env)]
fn issue391_start_readmits_a_removed_seat_from_its_left_behind_role_file() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "stopped", false)]);
    // remove-agent keeps the user's role file and leaves a retired tombstone.
    write_role(root.path(), "two");
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    let tombstone = json!({"two": {"state": "retired", "changed_at": "2026-10-27T15:25:48Z", "reason": "remove-agent"}});
    raw["agent_lifecycle"] = tombstone.clone();
    raw["teams"]["fixture"]["agent_lifecycle"] = tombstone;
    std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    intent.subject = Subject::Agent(AgentId::new("two"));
    let plan = plan_with_observer(intent.clone(), |_, _, _| panic!("unregistered probe")).unwrap();
    assert_eq!(actions(&plan), [("start", Some("two"))]);
    let Step::UnifiedStart { description, .. } = &plan.steps[0] else { panic!() };
    assert_eq!(description.op, "adopted");
    assert!(plan.files.iter().any(|(path, _)| path == &root.path().join("agents/two.md")), "the left-behind role remains the seat's definition");
    // Team start still leaves a removed seat retired.
    let mut team = intent.clone();
    team.subject = Subject::Team { source: None };
    let plan = plan_with_observer(team, |_, _, _| Ok(Observation::StoppedVerified)).unwrap();
    assert_eq!(actions(&plan), [("start_agent", Some("one"))]);
    assert!(plan
        .skipped
        .iter()
        .any(|entry| entry["agent"] == "two" && entry["reason"] == "retired"));
    // History the planner cannot classify is refused without naming a phantom command.
    raw["agent_lifecycle"]["two"]["state"] = json!("quarantined");
    raw["teams"]["fixture"]["agent_lifecycle"]["two"]["state"] = json!("quarantined");
    std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let error = plan_with_observer(intent, |_, _, _| panic!())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unrecognized lifecycle history") && !error.contains("recovery"),
        "{error}"
    );
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
    install(root.path(), &[("one", "stopped", false)]);
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
fn native_stop_recheck_accepts_dead_carrier_observations_but_fences_identity_and_intent() {
    let (root, mut intent) = fixture(&["one"]);
    install(root.path(), &[("one", "running", false)]);
    intent.verb = Verb::Stop;
    intent.subject = Subject::Agent("one".into());
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    let original = json!({"provider":"claude_tui", "contract_instance":"instance-original",
        "generation":1, "owner_team_id":"fixture", "spawn_epoch":1,
        "status":"running", "pane_pid":std::process::id(), "pane_id":"%0", "window":"worker",
        "session_id":"observed-session", "spawned_at":"original-observation", "explicit_stop":false});
    let mut publish = |row: &Value| {
        raw["agents"]["one"] = row.clone();
        raw["teams"]["fixture"]["agents"]["one"] = row.clone();
        std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    };
    publish(&original);
    let plan = plan_with_observer(intent, |_, _, _| Ok(Observation::Live)).unwrap();
    let step = &plan.steps[0];
    assert!(matches!(step, Step::StopAgent(_)));
    let mut observed = original.clone();
    observed["status"] = json!("unknown");
    for key in ["pane_pid", "pane_id", "window", "session_id", "spawned_at"] { observed[key] = Value::Null; }
    publish(&observed);
    recheck(&plan, step, false).unwrap();
    for (key, value) in [
        ("contract_instance", json!("instance-replacement")), ("generation", json!(2)),
        ("provider", json!("pi_tui")), ("owner_team_id", json!("other-team")),
        ("spawn_epoch", json!(2)), ("explicit_stop", json!(true)),
        ("dynamic_role_file", json!("replacement-role.md")),
    ] {
        let mut changed = observed.clone(); changed[key] = value;
        publish(&changed);
        assert!(recheck(&plan, step, false).is_err(), "{key} must remain fenced");
    }
    let mut unresolved = original.clone(); unresolved.as_object_mut().unwrap().remove("contract_instance");
    let mut dead = unresolved.clone(); dead["pane_pid"] = Value::Null;
    assert_ne!(relevant(&unresolved), relevant(&dead), "unresolved native identity remains strict");
    unresolved["provider"] = json!("pi"); dead["provider"] = json!("pi");
    assert_ne!(relevant(&unresolved), relevant(&dead), "legacy carrier fencing remains strict");
}

#[test]
#[serial_test::serial(env)]
#[cfg(unix)]
fn live_and_reconciling_restart_rechecks_accept_their_verified_stop() {
    for observation in [Observation::Live, Observation::NativeReconcile] {
        let (root, mut intent) = fixture(&["one"]);
        intent.verb = Verb::Restart;
        intent.subject = Subject::Agent("one".into());
        install(root.path(), &[("one", "running", false)]);
        let plan = plan_with_observer(intent, |_, _, _| Ok(observation)).unwrap();
        let step = plan.steps.iter().find(|step| matches!(step, Step::StartAgent(_))).unwrap();
        let path = crate::state::persist::runtime_state_path(root.path());
        let mut raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let stopped_pid = child.id();
        assert!(child.wait().unwrap().success());
        raw["teams"]["fixture"]["agents"]["one"]["status"] = json!("stopped");
        raw["teams"]["fixture"]["agents"]["one"]["pane_pid"] = json!(stopped_pid);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(recheck(&plan, step, false).is_err(), "unplanned state changes remain fenced");
        recheck(&plan, step, true).unwrap();
        raw["teams"]["fixture"]["agents"]["one"]["generation"] = json!(99);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(recheck(&plan, step, true).is_err(), "a different generation is not our stopped seat");
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
#[cfg(unix)]
#[serial_test::serial(env)]
fn shell_supervised_claude_is_live_for_both_team_start_spellings() {
    use std::process::Command;
    use std::time::{Duration, Instant};
    let (root, _) = fixture(&["one"]);
    std::fs::write(root.path().join("agents/one.md"),
        "---\nprovider: claude\nmodel: claude-opus-4.6\n---\nFixture\n").unwrap();
    install(root.path(), &[("one", "running", false)]);
    let sleep = Command::new("which").arg("sleep").output().unwrap();
    assert!(sleep.status.success());
    std::os::unix::fs::symlink(String::from_utf8(sleep.stdout).unwrap().trim(), root.path().join("claude")).unwrap();
    let socket = root.path().join("t.sock");
    struct Server(std::path::PathBuf);
    impl Drop for Server {
        fn drop(&mut self) { let _ = Command::new("tmux").arg("-S").arg(&self.0).arg("kill-server").output(); }
    }
    let _server = Server(socket.clone());
    let output = Command::new("tmux").arg("-S").arg(&socket)
        .args(["new-session", "-d", "-P", "-F", "#{pane_id} #{pane_pid}", "-s", "team-fixture", "-n", "one"])
        .arg(format!("sh -c '{} 60 & wait'", root.path().join("claude").display()))
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let pane = String::from_utf8(output.stdout).unwrap();
    let fields: Vec<_> = pane.split_whitespace().collect();
    let pid: u32 = fields[1].parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !launched_as_provider(pid, crate::provider::Provider::Claude) {
        assert!(Instant::now() < deadline, "controlled Claude child did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let state = crate::state::persist::runtime_state_path(root.path());
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    raw["tmux_socket"] = json!(socket);
    raw["teams"]["fixture"]["tmux_socket"] = json!(socket);
    raw["teams"]["fixture"]["generation"] = json!("fixture-generation");
    raw["teams"]["fixture"]["agents"]["one"] = json!({
        "provider":"claude", "status":"running", "pane_id":fields[0], "pane_pid":pid, "window":"one"
    });
    std::fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let before = std::fs::read(&state).unwrap();
    let selected = resolve_active_team_readonly(root.path(), Some("fixture"), SelectorMode::RuntimeOnly).unwrap();
    let backend = crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
        &selected.run_workspace, &selected.state).unwrap().backend;
    backend.set_session_owner_with_generation(&crate::transport::SessionName::new("team-fixture"),
        &selected.run_workspace, "fixture", "fixture-generation").unwrap();
    let row = &selected.state["agents"]["one"];
    assert_eq!(observe(&selected, "one", row).unwrap(), Observation::Live);
    for (command, values) in [
        ("team", vec!["start".into(), root.path().to_string_lossy().into_owned(), "--dry-run".into(), "--json".into()]),
        ("start", vec!["--team".into(), "fixture".into(), "--dry-run".into(), "--json".into()]),
    ] {
        let output = super::super::run(command, &values, root.path()).unwrap().unwrap();
        let CmdOutput::Json(value) = output.output else { panic!() };
        assert_eq!(value["ok"], true, "{value}");
        assert!(value["skipped"].as_array().unwrap().iter().any(|row| row["reason"] == "already_running"), "{value}");
    }
    let mut wrong = row.clone();
    wrong["provider"] = json!("copilot");
    assert_eq!(observe(&selected, "one", &wrong).unwrap(), Observation::Unknown);
    assert_eq!(std::fs::read(state).unwrap(), before);
}

#[test]
fn declared_provider_aliases_do_not_create_false_drift_or_absorb_native_routes() {
    assert!(same_provider("claude-code", "claude_code"));
    assert!(!same_provider("cursor", "cursor_agent"));
    assert!(same_provider("grok_build", "grok"));
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
fn provider_argv0_is_exact_basename_not_substring() {
    use crate::model::enums::Provider;
    assert!(argv0_names("claude", Provider::Claude));
    assert!(argv0_names("/Users/u/.local/bin/claude", Provider::Claude));
    assert!(argv0_names("claude", Provider::ClaudeCode));
    for argv0 in ["2.1.309", "claude-wrapper", "team-agent", "node", ""] {
        assert!(!argv0_names(argv0, Provider::Claude), "{argv0}");
    }
}

#[test]
#[cfg(unix)]
fn version_named_native_install_is_identified_by_launch_argv0() {
    // Claude Code's native installer execs `versions/<semver>`; tmux then
    // reports e.g. `2.1.309` as the pane command while argv[0] stays `claude`.
    use crate::model::enums::Provider;
    use std::os::unix::process::CommandExt;
    let mut claude = std::process::Command::new("sleep")
        .arg0("claude")
        .arg("30")
        .spawn()
        .unwrap();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let launched = launched_as_provider(claude.id(), Provider::Claude);
    let _ = claude.kill();
    let _ = claude.wait();
    assert!(launched);
    let launched = launched_as_provider(other.id(), Provider::Claude);
    let _ = other.kill();
    let _ = other.wait();
    assert!(!launched);
}

#[test]
#[cfg(unix)]
fn supervised_carrier_child_launched_as_provider_is_identified() {
    // Original restart supervises the provider as a child of a shell carrier.
    use crate::model::enums::Provider;
    use std::os::unix::process::CommandExt;
    let mut carrier = std::process::Command::new("bash")
        .args(["-c", "exec -a claude sleep 30 & wait"])
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut launched = false;
    while !launched && std::time::Instant::now() < deadline {
        launched = launched_as_provider(carrier.id(), Provider::Claude);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    unsafe { libc::kill(-(carrier.id() as i32), libc::SIGKILL) };
    let _ = carrier.wait();
    assert!(launched);
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
