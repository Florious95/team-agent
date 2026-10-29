#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use std::cell::RefCell;

pub(super) struct Sandbox(pub PathBuf);
impl Sandbox {
    pub(super) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ta-cli-phase1-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn native_receipt_reconciliation_does_not_turn_start_into_stop_or_allow_restart() {
    assert_eq!(
        seat_actions(
            Verb::Start,
            Observation::NativeReconcile,
            false,
            false,
            false,
            true
        )
        .unwrap(),
        vec!["start"]
    );
    assert!(seat_actions(
        Verb::Restart,
        Observation::NativeReconcile,
        false,
        false,
        false,
        false
    )
    .is_err());
    assert!(seat_actions(
        Verb::Restart,
        Observation::NativeReconcile,
        false,
        true,
        false,
        true
    )
    .unwrap()
    .is_empty());
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}
pub(super) fn intent(verb: Verb) -> Intent {
    Intent {
        surface: Surface::Canonical,
        verb,
        subject: Subject::Agent(AgentId::new("one")),
        workspace: PathBuf::from("/fixture"),
        pinned_workspace: true,
        team: Some("pinned".into()),
        session: SessionPolicy::Preserve,
        session_converge_deadline_ms: None,
        dry_run: false,
        json: true,
        role_file: None,
        patch: RoleConfigPatch::default(),
    }
}
fn plan(verb: Verb, actions: &[&str]) -> Plan {
    let intent = intent(verb);
    let scope = ResolvedScope {
        workspace: intent.workspace.clone(),
        team: intent.team.clone(),
    };
    let steps = actions
        .iter()
        .map(|action| match *action {
            "stop" => Step::StopAgent(StopAgentArgs {
                agent: "one".into(),
                workspace: scope.workspace.clone(),
                team: scope.team.clone(),
                json: true,
            }),
            _ => Step::StartAgent(StartAgentArgs {
                agent: "one".into(),
                workspace: scope.workspace.clone(),
                team: scope.team.clone(),
                force: false,
                allow_fresh: false,
                json: true,
                role_config: RoleConfigPatch::default(),
            }),
        })
        .collect();
    Plan {
        intent,
        scope,
        steps,
        skipped: vec![],
        no_eligible: None,
        files: vec![],
        expected: BTreeMap::new(),
        topology: None,
    }
}
#[test]
fn registered_truth_table_keeps_physics_configuration_and_intent_separate() {
    use Observation::*;
    use Verb::*;
    for (verb, obs, expected) in [
        (Start, Live, vec![]),
        (Start, StoppedVerified, vec!["start"]),
        (Start, Unstarted, vec!["start"]),
        (Stop, Live, vec!["stop"]),
        (Stop, StoppedVerified, vec![]),
        (Restart, Live, vec!["stop", "start"]),
        (Restart, StoppedVerified, vec!["start"]),
    ] {
        assert_eq!(
            seat_actions(verb, obs, false, false, false, false).unwrap(),
            expected
        );
    }
    for verb in [Start, Stop, Restart] {
        assert!(seat_actions(verb, Unknown, false, false, false, false).is_err());
        assert!(seat_actions(verb, Live, true, false, false, false).is_err());
    }
    assert!(seat_actions(Start, Live, false, false, true, false).is_err());
    assert!(seat_actions(Start, Live, false, true, false, false).is_err());
    assert!(seat_actions(Restart, Unstarted, false, false, false, false).is_err());
    assert!(seat_actions(Start, Live, true, false, false, true)
        .unwrap()
        .is_empty());
    assert!(seat_actions(Restart, Live, false, true, false, true)
        .unwrap()
        .is_empty());
    assert_eq!(
        seat_actions(Start, StoppedVerified, false, true, false, true).unwrap(),
        ["start"]
    );
}
#[test]
fn explicit_stop_uses_existing_conservative_interpretation() {
    for value in [json!(true), Value::Null, json!("false"), json!(0)] {
        let row = json!({"explicit_stop":value});
        assert!(crate::state::projection::agent_explicit_stop(&row));
    }
    for row in [json!({}), json!({"explicit_stop":false})] {
        assert!(!crate::state::projection::agent_explicit_stop(&row));
    }
}
#[test]
fn restart_executes_exact_pinned_stop_start_and_preserves_original_receipts() {
    let calls = RefCell::new(Vec::new());
    let result = execute_plan(
        plan(Verb::Restart, &["stop", "start"]),
        &mut |step| {
            calls.borrow_mut().push(step.name());
            Ok(CmdResult::from_json(
                json!({"ok":true,"stopped":true,"start_mode":"resumed","original":17}),
                true,
            ))
        },
        &mut |p, s, after_stop| {
            assert_eq!(p.scope.team.as_deref(), Some("pinned"));
            assert_eq!(s.agent(), Some("one"));
            assert_eq!(after_stop, matches!(s, Step::StartAgent(_)));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(*calls.borrow(), ["stop_agent", "start_agent"]);
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(value["steps"][1]["receipt"]["original"], 17);
    assert_eq!(value["steps"][1]["mode"], "resumed");
}
#[test]
fn stop_failure_never_starts_and_spawn_failure_preserves_partial_uncertainty() {
    for fail_at in [0, 1] {
        let mut calls = Vec::new();
        let result = execute_plan(
            plan(Verb::Restart, &["stop", "start"]),
            &mut |step| {
                let n = calls.len();
                calls.push(step.name());
                Ok(CmdResult::from_json(
                    json!({"ok":n!=fail_at,"stopped":true,"error":"original refusal"}),
                    true,
                ))
            },
            &mut |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        assert_eq!(calls.len(), fail_at + 1);
        let CmdOutput::Json(value) = result.output else {
            panic!()
        };
        assert_eq!(
            value["status"],
            if fail_at == 0 { "failed" } else { "partial" }
        );
        assert_eq!(
            value["steps"][fail_at]["receipt"]["error"],
            "original refusal"
        );
    }
}
#[test]
fn owner_or_incarnation_change_interrupts_even_best_effort_start() {
    let mut calls = 0;
    let mut checks = 0;
    let result = execute_plan(
        plan(Verb::Start, &["start", "start"]),
        &mut |_| {
            calls += 1;
            Ok(CmdResult::from_json(json!({"ok":true}), true))
        },
        &mut |_, _, _| {
            checks += 1;
            if checks == 2 {
                Err(refusal("owner changed"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result.exit, ExitCode::Error);
}
#[test]
fn role_change_between_stop_and_start_refuses_without_rollback_claim() {
    let mut calls = 0;
    let result = execute_plan(
        plan(Verb::Restart, &["stop", "start"]),
        &mut |_| {
            calls += 1;
            Ok(CmdResult::from_json(
                json!({"ok":true,"stopped":true}),
                true,
            ))
        },
        &mut |_, _, after_stop| {
            if after_stop {
                Err(refusal("role changed"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(value["status"], "partial");
    assert_eq!(value["last_confirmed_effect"], "original_instance_stopped");
    assert_eq!(value["current_state"], "unknown");
}
#[test]
fn issue337_failed_handler_preserves_confirmed_and_uncertain_effects() {
    for (coordinator, workers, confirmed) in [
        ("not_attempted", json!(["worker"]), true),
        ("attempted_unknown", json!([]), false),
    ] {
        let result = execute_plan(
            plan(Verb::Start, &["start"]),
            &mut |_| Ok(CmdResult::from_json(json!({
                "ok": false, "status": "partial", "error": "original failure",
                "effects": {"worker_panes_started": workers, "coordinator_start": coordinator},
            }), true)),
            &mut |_, _, _| Ok(()),
        ).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Json(value) = result.output else {
            panic!()
        };
        assert_eq!(value["status"], "partial");
        assert_eq!(value["steps"][0]["receipt"]["error"], "original failure");
        assert_eq!(
            value["effects"][0]["effects"]["coordinator_start"],
            coordinator
        );
        assert_eq!(!value["last_confirmed_effect"].is_null(), confirmed);
        let recovery = value["recovery"].as_str().unwrap();
        assert!(recovery.contains("--workspace"));
        assert!(recovery.contains("--team"));
        assert!(recovery.contains("doctor"));
        assert!(!recovery.contains("claim-leader"));
    }
}

#[test]
fn issue337_zero_effect_refusal_is_explicit_at_top_without_hiding_prior_uncertainty() {
    for prior_unknown in [false, true] {
        let mut call = 0;
        let actions = if prior_unknown {
            vec!["start", "start"]
        } else {
            vec!["start"]
        };
        let result = execute_plan(
            plan(Verb::Start, &actions),
            &mut |_| {
                call += 1;
                let receipt = if prior_unknown && call == 1 {
                    json!({"ok":false,"error":"unclassified handler failure"})
                } else {
                    json!({"ok":false,"status":"preflight_blocked",
                        "effects":{"worker_panes_started":[],"coordinator_start":"not_attempted"},
                        "next_actions":["run from the intended live Leader pane; no process was started"]})
                };
                Ok(CmdResult::from_json(receipt, true))
            },
            &mut |_, _, _| Ok(()),
        ).unwrap();
        let CmdOutput::Json(value) = result.output else {
            panic!()
        };
        assert_eq!(value["status"], "failed");
        assert_eq!(
            value["current_state"],
            if prior_unknown {
                "unknown"
            } else {
                "unchanged"
            }
        );
        assert!(value["last_confirmed_effect"].is_null());
        assert_eq!(
            value["effects"][0]["effects"]["coordinator_start"],
            "not_attempted"
        );
        let recovery = value["recovery"].as_str().unwrap();
        if prior_unknown {
            assert!(recovery.contains("doctor"));
            assert!(!recovery.contains("no process was started"));
        } else {
            assert!(recovery.contains("live Leader pane"));
        }
    }
}

#[test]
fn dry_run_never_calls_handler_or_execution_admission() {
    let mut plan = plan(Verb::Restart, &["stop", "start"]);
    plan.intent.dry_run = true;
    let result = execute_plan(plan, &mut |_| panic!("dry-run executed"), &mut |_, _, _| {
        panic!("dry-run acquired execution admission")
    })
    .unwrap();
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["steps"].as_array().unwrap().len(), 2);
    assert!(!value["deferred"].as_array().unwrap().is_empty());
}
#[test]
fn lexical_paths_never_use_dot_or_directory_existence_as_agent_inference() {
    for id in ["reviewer", "reviewer.v2", "worker-1"] {
        assert!(!path_syntax(id));
    }
    for path in [
        ".",
        "..",
        "./worker",
        "/tmp/worker",
        "a/b",
        "a\\b",
        "C:\\team",
    ] {
        assert!(path_syntax(path));
    }
}
#[test]
fn grammar_is_strict_and_payload_values_are_not_controls() {
    for (schema, values) in [
        ("intent-start", vec!["one", "--force"]),
        ("intent-start", vec!["--from"]),
        ("intent-stop", vec!["one", "two"]),
        ("intent-stop", vec!["--session", "auto"]),
        ("intent-restart", vec!["one", "--model", "wrong"]),
        ("intent-restart", vec!["--session", "fresh"]),
        (
            "intent-start",
            vec!["--workspace=missing", "--workspace=other"],
        ),
    ] {
        assert!(super::super::arguments::parse(schema, &args(&values)).is_err());
    }
    let checked =
        super::super::arguments::parse("intent-start", &args(&["one", "--prompt", "--help"]))
            .unwrap();
    assert!(!checked.help);
    assert!(checked.tokens.contains(&"--prompt=--help".into()));
}
#[test]
fn frozen_legacy_restart_is_not_rerouted() {
    let cwd = std::env::temp_dir();
    for values in [
        vec![],
        vec!["--workspace", "/unread-by-bridge"],
        vec!["./old-path"],
        vec!["--allow-fresh"],
        vec!["--help"],
    ] {
        assert!(run("restart", &args(&values), &cwd).is_none());
    }
    for values in [vec!["--session", "auto", "--help"], vec!["one", "--help"]] {
        assert!(run("restart", &args(&values), &cwd).unwrap().is_ok());
    }
}
#[test]
fn canonical_help_and_invalid_flags_do_not_create_runtime() {
    let root = Sandbox::new();
    for command in ["start", "stop", "team", "agent"] {
        assert_eq!(
            run(command, &args(&["--help"]), root.path())
                .unwrap()
                .unwrap()
                .exit,
            ExitCode::Ok
        );
    }
    assert!(run("start", &args(&["--workspaec", "x"]), root.path())
        .unwrap()
        .is_err());
    assert!(run("stop", &args(&["./one"]), root.path())
        .unwrap()
        .is_err());
    assert!(!root.path().join(".team").exists());
}

#[test]
fn delimiter_and_consumed_prompt_are_never_rendering_or_scope_controls() {
    let checked =
        super::super::arguments::parse("intent-start", &args(&["--", "--workspace=foreign"]))
            .unwrap();
    let (pos, flags) = options(&checked.tokens);
    assert_eq!(pos, ["--workspace=foreign"]);
    assert!(flags.is_empty());
    assert!(!super::super::arguments::has_flag(
        "start",
        &args(&["one", "--prompt", "--json"]),
        "--json"
    ));
    assert!(!super::super::arguments::has_flag(
        "agent",
        &args(&["start", "one", "--prompt", "--json"]),
        "--json"
    ));
}

#[test]
fn unconfirmed_stop_never_authorizes_a_restart_start_step() {
    for receipt in [json!({"ok":true,"stopped":false}), json!({"ok":true})] {
        let mut calls = 0;
        let result = execute_plan(
            plan(Verb::Restart, &["stop", "start"]),
            &mut |_| {
                calls += 1;
                Ok(CmdResult::from_json(receipt.clone(), true))
            },
            &mut |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Json(value) = result.output else {
            panic!()
        };
        assert_eq!(value["steps"][0]["phase"], "stop_not_confirmed");
        assert_eq!(value["status"], "failed");
    }
}

#[test]
fn syntax_session_matrix_and_directory_ambiguity_are_deterministic() {
    let root = Sandbox::new();
    std::fs::create_dir(root.path().join("worker")).unwrap();
    for (command, verb, input, agent, team, expected) in [
        (
            "start",
            Verb::Start,
            vec!["worker"],
            false,
            false,
            SessionPolicy::Preserve,
        ),
        (
            "agent",
            Verb::Restart,
            vec!["worker"],
            true,
            false,
            SessionPolicy::Auto,
        ),
        (
            "team",
            Verb::Restart,
            vec![],
            false,
            true,
            SessionPolicy::Auto,
        ),
        (
            "restart",
            Verb::Restart,
            vec![],
            false,
            false,
            SessionPolicy::Preserve,
        ),
        (
            "restart",
            Verb::Restart,
            vec!["--session=auto"],
            false,
            false,
            SessionPolicy::Auto,
        ),
        (
            "agent",
            Verb::Restart,
            vec!["worker", "--session=preserve"],
            true,
            false,
            SessionPolicy::Preserve,
        ),
        (
            "start",
            Verb::Start,
            vec!["worker", "--allow-fresh"],
            false,
            false,
            SessionPolicy::Auto,
        ),
    ] {
        let intent =
            lifecycle_intent(command, verb, &args(&input), root.path(), agent, team).unwrap();
        assert_eq!(intent.session, expected);
        if input.first() == Some(&"worker") {
            assert!(matches!(intent.subject, Subject::Agent(_)));
        }
    }
    assert!(lifecycle_intent(
        "restart",
        Verb::Restart,
        &args(&["worker"]),
        root.path(),
        false,
        false
    )
    .is_err());
    assert!(lifecycle_intent(
        "restart",
        Verb::Restart,
        &args(&["--session=preserve", "--allow-fresh"]),
        root.path(),
        false,
        false
    )
    .is_err());
    assert!(lifecycle_intent(
        "start",
        Verb::Start,
        &args(&["worker", "--from=worker"]),
        root.path(),
        false,
        false
    )
    .is_err());
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(root.path().join("TEAM.md"), "---\nname: caller\n---\n").unwrap();
    let path_intent = lifecycle_intent(
        "restart",
        Verb::Restart,
        &args(&["./worker", "--dry-run"]),
        root.path(),
        false,
        false,
    )
    .unwrap();
    assert_eq!(path_intent.workspace, root.path().join("worker"));
    assert!(path_intent.pinned_workspace);
    let role = root.path().join("role.md");
    std::fs::write(&role, "fixture").unwrap();
    let tokens = vec![
        "one".into(),
        format!("--workspace={}", root.path().join("worker").display()),
        "--role-file=role.md".into(),
    ];
    let intent =
        lifecycle_intent("start", Verb::Start, &tokens, root.path(), false, false).unwrap();
    assert_eq!(intent.role_file, Some(role));
    assert!(lifecycle_intent(
        "start",
        Verb::Start,
        &args(&["--from=role.md"]),
        root.path(),
        false,
        false
    )
    .is_err());
    assert!(lifecycle_intent(
        "start",
        Verb::Start,
        &args(&["one", "--role-file=worker"]),
        root.path(),
        false,
        false
    )
    .is_err());
}

#[test]
fn no_eligible_is_scoped_nonzero_with_skipped_and_zero_handlers_even_in_dry_run() {
    for dry_run in [false, true] {
        let mut p = plan(Verb::Start, &["start"]);
        p.intent.dry_run = dry_run;
        p.no_eligible = Some("all selected seats are paused");
        p.skipped.push(json!({"agent":"one","reason":"paused"}));
        let result = execute_plan(p, &mut |_| panic!("blocked handler"), &mut |_, _, _| {
            panic!("blocked recheck")
        })
        .unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Json(value) = result.output else {
            panic!()
        };
        assert_eq!(value["scope"]["team"], "pinned");
        assert_eq!(value["status"], "no_eligible_agents");
        assert_eq!(value["phase"], "planning");
        assert_eq!(value["skipped"][0]["agent"], "one");
        assert_eq!(value["skipped"][0]["reason"], "paused");
        assert_eq!(value["steps"], json!([]));
        assert_eq!(value["dry_run"], dry_run);
    }
}

#[test]
fn accepted_pending_creation_keeps_original_receipt_and_actual_canonical_team() {
    let mut p = plan(Verb::Start, &[]);
    p.scope.team = None;
    p.steps.push(Step::CreateTeam(QuickStartArgs {
        workspace: PathBuf::from("/fixture"),
        agents_dir: PathBuf::from("/fixture"),
        name: None,
        team_id: None,
        yes: false,
        json: true,
        detail: true,
        backend: None,
    }));
    let result = execute_plan(
        p,
        &mut |_| {
            Ok(CmdResult::from_json(
                json!({"ok":true,"status":"pending_tool_load","ready":false,"team":"actual-key"}),
                true,
            ))
        },
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    let CmdOutput::Json(value) = result.output else {
        panic!()
    };
    assert_eq!(value["status"], "pending");
    assert_eq!(value["scope"]["team"], "actual-key");
    assert_eq!(value["steps"][0]["receipt"]["ready"], false);
}
