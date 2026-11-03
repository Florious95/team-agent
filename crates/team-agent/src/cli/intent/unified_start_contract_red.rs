//! RED — unified start contract, END STATE (planner seam).
//!
//! `team-agent start [AGENT]` is the only seat lifecycle verb. It creates an absent
//! seat, adopts an existing role doc, wakes a stopped seat, migrates a stopped seat
//! to another provider in place (`--session fresh`), is a clean no-op on an identical
//! live seat, and refuses to mutate a live seat with an admitted `team-agent stop`.
//! Every planned seat step reports `op`, the resolved `declaration` and the
//! `defaulted` fields; refusals name admitted canonical commands.
//!
//! Hermetic: fixture state/role docs and fake physical observations; `--dry-run`
//! never executes a lifecycle step and planning must not write.
#![allow(clippy::unwrap_used, clippy::panic)]
use super::super::tests::Sandbox;
use super::*;
use crate::cli::guidance_oracle::{admitted_verbs, assert_admitted};

const TEAM: &str = "---\nname: fixture\n---\nFixture objective\n";
const PI_ROLE: &str = "---\nprovider: pi\nmodel: openai-codex/gpt-6-luna\neffort: high\ndangerously_skip_permissions: true\n---\nReview the change.\n";
const CLAUDE_ROLE: &str =
    "---\nprovider: claude\ndangerously_skip_permissions: false\n---\nAudit the change.\n";

/// Initialized team `fixture`; every row is a Pi seat with the given status.
fn team(rows: &[(&str, &str)]) -> Sandbox {
    let root = Sandbox::new();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(root.path().join("TEAM.md"), TEAM).unwrap();
    for (id, _) in rows {
        std::fs::write(root.path().join(format!("agents/{id}.md")), PI_ROLE).unwrap();
    }
    let agents: serde_json::Map<String, Value> = rows
        .iter()
        .map(|(id, status)| {
            ((*id).into(), json!({"provider":"pi","status":status,"explicit_stop":false,
                "pane_id":format!("%{id}"),"window":id}))
        })
        .collect();
    let entry = json!({"team_key":"fixture","team_dir":root.path(),"status":"alive",
        "session_name":"team-fixture","agents":agents,"tasks":{}});
    let mut raw = entry.clone();
    raw["active_team_key"] = json!("fixture");
    raw["teams"] = json!({"fixture": entry});
    write_state(root.path(), &raw);
    let spec = crate::model::paths::runtime_spec_path(root.path(), "fixture");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    std::fs::write(spec, yaml::dumps(&crate::compiler::compile_team(root.path()).unwrap())).unwrap();
    root
}

fn state_path(root: &Path) -> PathBuf {
    crate::state::persist::runtime_state_path(root)
}
fn write_state(root: &Path, raw: &Value) {
    let path = state_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(raw).unwrap()).unwrap();
}
fn edit_state(root: &Path, edit: impl Fn(&mut Value)) {
    let mut raw: Value = serde_json::from_slice(&std::fs::read(state_path(root)).unwrap()).unwrap();
    edit(&mut raw);
    edit(&mut raw["teams"]["fixture"]);
    write_state(root, &raw);
}

/// Parse exactly like `team-agent start …` (strict schema, then intent grammar).
fn intent(cwd: &Path, argv: &[&str]) -> Result<Intent, CliError> {
    let args: Vec<String> = argv.iter().map(|value| (*value).to_string()).collect();
    let checked = crate::cli::arguments::parse("intent-start", &args)?;
    lifecycle_intent("start", Verb::Start, &checked.tokens, cwd, false, false)
}

/// `start ARGV --workspace ROOT [--team fixture] --dry-run --json` with a fake observer.
fn dry_run(root: &Path, team: bool, argv: &[&str], observed: Observation) -> Result<Value, CliError> {
    let workspace = root.to_string_lossy().into_owned();
    let mut full = argv.to_vec();
    full.extend(["--workspace", workspace.as_str(), "--dry-run", "--json"]);
    if team {
        full.extend(["--team", "fixture"]);
    }
    let plan = plan_with_observer(intent(root, &full)?, move |_, _, _| Ok(observed))?;
    let result = execute_plan(
        plan,
        &mut |_| panic!("dry-run executed a lifecycle step"),
        &mut |_, _, _| Ok(()),
    )?;
    match result.output {
        CmdOutput::Json(value) => Ok(value),
        other => panic!("start --dry-run --json must return JSON: {other:?}"),
    }
}

/// Disk bytes that read-only planning must not change.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = vec![state_path(root)];
    for entry in std::fs::read_dir(root.join("agents")).unwrap() {
        files.push(entry.unwrap().path());
    }
    files.sort();
    files.into_iter().map(|path| {
        let bytes = std::fs::read(&path).unwrap_or_default();
        (path, bytes)
    }).collect()
}

fn seat<'a>(plan: &'a Value, id: &str) -> &'a Value {
    let steps = plan["steps"].as_array().unwrap_or_else(|| panic!("plan has no steps: {plan}"));
    let mine: Vec<_> = steps.iter().filter(|step| step["agent"] == id).collect();
    assert_eq!(mine.len(), 1, "exactly one planned step for {id}: {plan}");
    mine[0]
}
fn op(plan: &Value, id: &str) -> String {
    seat(plan, id)["op"].as_str().unwrap_or_else(|| panic!("step for {id} must report op: {plan}")).to_owned()
}
fn declared(plan: &Value, id: &str, field: &str) -> Value {
    let declaration = &seat(plan, id)["declaration"];
    assert!(declaration.is_object(), "step for {id} must report its resolved declaration: {plan}");
    declaration[field].clone()
}
fn provider_is(value: &Value, expected: &str) -> bool {
    value.as_str().and_then(crate::provider::wire::parse_provider)
        == crate::provider::wire::parse_provider(expected)
}
fn defaulted(plan: &Value, id: &str) -> Vec<String> {
    seat(plan, id)["defaulted"]
        .as_array()
        .unwrap_or_else(|| panic!("step for {id} must report defaulted fields: {plan}"))
        .iter()
        .filter_map(|field| field.as_str().map(str::to_owned))
        .collect()
}
#[track_caller]
fn refusal_names(error: &CliError, verb: &str) {
    let text = error.to_string();
    assert_admitted(&text);
    assert!(
        admitted_verbs(&text).iter().any(|named| named == verb),
        "refusal must name an admitted `team-agent {verb} …`: {text}"
    );
}

struct LeaderProvider(Option<String>);
impl LeaderProvider {
    fn set(value: &str) -> Self {
        let previous = std::env::var("TEAM_AGENT_LEADER_PROVIDER").ok();
        std::env::set_var("TEAM_AGENT_LEADER_PROVIDER", value);
        Self(previous)
    }
}
impl Drop for LeaderProvider {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("TEAM_AGENT_LEADER_PROVIDER", value),
            None => std::env::remove_var("TEAM_AGENT_LEADER_PROVIDER"),
        }
    }
}

// ---- grammar ---------------------------------------------------------------

#[test]
fn session_policy_grammar_is_preserve_auto_fresh() {
    let root = Sandbox::new();
    let workspace = root.path().to_string_lossy().into_owned();
    for (value, expected) in [("preserve", "preserve"), ("auto", "auto"), ("fresh", "fresh")] {
        let parsed = intent(root.path(), &["one", "--session", value, "--workspace", &workspace])
            .unwrap_or_else(|error| panic!("--session {value} must parse: {error}"));
        assert_eq!(json!(parsed.session), json!(expected));
    }
    assert!(intent(root.path(), &["one", "--session", "fresh", "--allow-fresh"]).is_err());
    assert!(intent(root.path(), &["one", "--session", "bogus"]).is_err());
}

// ---- creation, adoption, wake ---------------------------------------------

#[test]
#[serial_test::serial(env)]
fn creates_an_absent_seat_and_reports_every_default() {
    let _leader = LeaderProvider::set("claude");
    let root = team(&[("one", "stopped")]);
    let before = snapshot(root.path());
    let plan = dry_run(root.path(), true, &["newbie"], Observation::StoppedVerified).unwrap();
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    assert_eq!(op(&plan, "newbie"), "created");
    assert!(provider_is(&declared(&plan, "newbie", "provider"), "claude"), "leader provider: {plan}");
    assert_eq!(declared(&plan, "newbie", "bypass"), json!(false));
    assert!(declared(&plan, "newbie", "model").is_null(), "native default model stays unset: {plan}");
    let defaulted = defaulted(&plan, "newbie");
    for field in ["provider", "bypass"] {
        assert!(defaulted.iter().any(|name| name == field), "{field} must be reported: {plan}");
    }
}

#[test]
#[serial_test::serial(env)]
fn explicit_creation_reports_no_defaulted_decisions() {
    let _leader = LeaderProvider::set("pi");
    let root = team(&[("one", "stopped")]);
    let plan = dry_run(
        root.path(), true,
        &["newbie", "--provider", "claude", "--model", "claude-sonnet-x", "--bypass", "true", "--prompt", "Audit"],
        Observation::StoppedVerified,
    ).unwrap();
    assert_eq!(op(&plan, "newbie"), "created");
    assert!(provider_is(&declared(&plan, "newbie", "provider"), "claude"));
    assert_eq!(declared(&plan, "newbie", "model"), json!("claude-sonnet-x"));
    assert_eq!(declared(&plan, "newbie", "bypass"), json!(true));
    let defaulted = defaulted(&plan, "newbie");
    for field in ["provider", "model", "bypass", "prompt"] {
        assert!(!defaulted.iter().any(|name| name == field), "{field} was explicit: {plan}");
    }
}

#[test]
#[serial_test::serial(env)]
fn adopts_an_existing_role_doc_without_collision() {
    let root = team(&[("one", "stopped")]);
    // Written after the runtime spec: a role doc the team has never started.
    std::fs::write(root.path().join("agents/auditor.md"), CLAUDE_ROLE).unwrap();
    let before = snapshot(root.path());
    let plan = dry_run(root.path(), true, &["auditor"], Observation::StoppedVerified)
        .unwrap_or_else(|error| panic!("own role doc is input, not a collision: {error}"));
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    assert_eq!(op(&plan, "auditor"), "adopted");
    assert!(provider_is(&declared(&plan, "auditor", "provider"), "claude"));
    assert_eq!(declared(&plan, "auditor", "bypass"), json!(false));
    let defaulted = defaulted(&plan, "auditor");
    assert!(!defaulted.iter().any(|name| name == "provider" || name == "bypass"), "{plan}");
}

#[test]
#[serial_test::serial(env)]
fn readopts_a_removed_seat_from_its_surviving_role_doc() {
    let root = team(&[("one", "stopped")]);
    std::fs::write(root.path().join("agents/gone.md"), CLAUDE_ROLE).unwrap();
    edit_state(root.path(), |state| {
        state["agent_lifecycle"] = json!({"gone": {"state": "retired", "reason": "remove-agent"}});
    });
    let plan = dry_run(root.path(), true, &["gone"], Observation::StoppedVerified).unwrap();
    assert_eq!(op(&plan, "gone"), "adopted");
}

#[test]
#[serial_test::serial(env)]
fn wakes_a_stopped_seat_and_preserves_its_session_by_default() {
    // A launched, talked-to seat whose Pi session was captured with an exact backing.
    let root = team(&[("one", "stopped")]);
    let session = "pi-session-one";
    let backing = root.path().join("pi-sessions/one.jsonl");
    std::fs::create_dir_all(backing.parent().unwrap()).unwrap();
    let header = json!({"type":"session","version":3,"id":session,"cwd":root.path(),
        "timestamp":"2026-11-03T06:00:01+00:00"});
    std::fs::write(&backing, format!("{header}\n")).unwrap();
    edit_state(root.path(), |state| {
        let row = &mut state["agents"]["one"];
        row["spawned_at"] = json!("2026-11-03T06:00:00+00:00");
        row["spawn_cwd"] = json!(root.path());
        row["first_send_at"] = json!("2026-11-03T06:01:00+00:00");
        row["session_id"] = json!(session);
        row["rollout_path"] = json!(backing);
        row["capture_state"] = json!("captured");
    });
    let plan = dry_run(root.path(), true, &["one"], Observation::StoppedVerified).unwrap();
    assert_eq!(plan["session_policy"], json!("preserve"));
    assert_eq!(op(&plan, "one"), "resumed");
}

#[test]
#[serial_test::serial(env)]
fn preserve_never_replaces_unverified_context_with_a_fresh_session() {
    // Live H11 A6: launched and talked to, but no verified session. The receipt
    // must not promise a resume, and preserve must not silently start fresh.
    let root = team(&[("one", "stopped")]);
    edit_state(root.path(), |state| {
        let row = &mut state["agents"]["one"];
        row["spawned_at"] = json!("2026-11-03T06:00:00+00:00");
        row["first_send_at"] = json!("2026-11-03T06:01:00+00:00");
        row["_pending_session_id"] = json!("pending-one");
    });
    let before = snapshot(root.path());
    let error = dry_run(root.path(), true, &["one"], Observation::StoppedVerified)
        .expect_err("preserve must refuse instead of discarding delivered context");
    refusal_names(&error, "start");
    assert!(error.to_string().contains("--session fresh"), "{error}");
    assert_eq!(snapshot(root.path()), before, "refusal precedes writes");
    for policy in ["auto", "fresh"] {
        let plan = dry_run(root.path(), true, &["one", "--session", policy], Observation::StoppedVerified)
            .unwrap_or_else(|error| panic!("--session {policy} is the explicit way out: {error}"));
        assert_eq!(op(&plan, "one"), "fresh", "--session {policy}: {plan}");
    }
    // A seat that never launched has nothing to preserve; it must not claim a resume.
    let virgin = team(&[("two", "stopped")]);
    let plan = dry_run(virgin.path(), true, &["two"], Observation::StoppedVerified).unwrap();
    assert_eq!(op(&plan, "two"), "fresh", "{plan}");
}

#[test]
#[serial_test::serial(env)]
fn fresh_wake_on_the_same_provider_is_explicit() {
    let root = team(&[("one", "stopped")]);
    let plan = dry_run(root.path(), true, &["one", "--session", "fresh"], Observation::StoppedVerified).unwrap();
    assert_eq!(plan["session_policy"], json!("fresh"));
    assert_eq!(op(&plan, "one"), "fresh");
}

#[test]
#[serial_test::serial(env)]
fn ad_hoc_creation_in_an_empty_workspace_uses_the_same_resolver() {
    let _leader = LeaderProvider::set("pi");
    let empty = Sandbox::new();
    let flags = ["newbie", "--provider", "claude", "--bypass", "false"];
    let ad_hoc = dry_run(empty.path(), false, &flags, Observation::StoppedVerified).unwrap();
    assert!(std::fs::read_dir(empty.path()).unwrap().next().is_none(), "planning must not write");
    let root = team(&[("one", "stopped")]);
    let in_team = dry_run(root.path(), true, &flags, Observation::StoppedVerified).unwrap();
    assert_eq!(op(&ad_hoc, "newbie"), "created");
    assert_eq!(seat(&ad_hoc, "newbie")["declaration"], seat(&in_team, "newbie")["declaration"]);
    assert_eq!(defaulted(&ad_hoc, "newbie"), defaulted(&in_team, "newbie"));
}

// ---- live seats ------------------------------------------------------------

#[test]
#[serial_test::serial(env)]
fn identical_live_seat_is_a_clean_noop() {
    let root = team(&[("one", "running")]);
    let plan = dry_run(root.path(), true, &["one"], Observation::Live).unwrap();
    assert_eq!(plan["ok"], json!(true), "{plan}");
    assert!(plan["steps"].as_array().is_some_and(Vec::is_empty), "{plan}");
    assert!(plan["skipped"].as_array().unwrap().iter()
        .any(|entry| entry["agent"] == "one" && entry["reason"] == "already_running"), "{plan}");
}

#[test]
#[serial_test::serial(env)]
fn live_seat_mutation_refuses_before_writes_and_names_stop() {
    let root = team(&[("one", "running")]);
    let before = snapshot(root.path());
    for argv in [
        vec!["one", "--model", "openai-codex/gpt-6-sol"],
        vec!["one", "--provider", "claude", "--session", "fresh"],
        vec!["one", "--session", "fresh"],
    ] {
        let error = dry_run(root.path(), true, &argv, Observation::Live)
            .expect_err("a live seat must not be mutated in place");
        refusal_names(&error, "stop");
        assert_eq!(snapshot(root.path()), before, "refusal precedes writes");
    }
}

// ---- in-place provider/model migration ---------------------------------------

#[test]
#[serial_test::serial(env)]
fn stopped_seat_migrates_provider_in_place_with_fresh_session() {
    let root = team(&[("one", "stopped"), ("peer", "running")]);
    let before = snapshot(root.path());
    let plan = dry_run(
        root.path(), true,
        &["one", "--provider", "claude", "--model", "claude-sonnet-x", "--session", "fresh"],
        Observation::StoppedVerified,
    ).unwrap_or_else(|error| panic!("stopped seat must migrate in place: {error}"));
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    assert_eq!(plan["session_policy"], json!("fresh"));
    assert_eq!(op(&plan, "one"), "provider_changed");
    assert!(provider_is(&declared(&plan, "one", "provider"), "claude"));
    assert_eq!(declared(&plan, "one", "model"), json!("claude-sonnet-x"));
    let steps = plan["steps"].as_array().unwrap();
    assert!(steps.iter().all(|step| step["agent"] == "one"), "peers untouched: {plan}");
    assert!(
        !steps.iter().any(|step| {
            let action = step["action"].as_str().unwrap_or_default();
            action.contains("remove") || action.contains("add") || action.contains("force")
        }),
        "no remove/add/force dance: {plan}"
    );
}

#[test]
#[serial_test::serial(env)]
fn stopped_seat_migrates_to_a_contract_provider_keeping_its_id() {
    let root = team(&[("one", "stopped")]);
    let before = snapshot(root.path());
    let result = dry_run(
        root.path(), true,
        &["one", "--provider", "cursor", "--model", "grok-4.7", "--session", "fresh"],
        Observation::StoppedVerified,
    );
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    match result {
        Ok(plan) => {
            assert_eq!(op(&plan, "one"), "provider_changed");
            assert!(provider_is(&declared(&plan, "one", "provider"), "cursor") || declared(&plan, "one", "provider") == json!("cursor"));
            assert_eq!(declared(&plan, "one", "model"), json!("grok-4.7"));
        }
        // Native admission (installed CLI, tmux, catalog model) is host-dependent and may
        // refuse at planning; the seat identity is never the reason. H11 A10 proves the
        // positive path with a model id taken from `team-agent models --provider cursor`.
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("native"),
                "legacy→contract migration may only be refused by native admission: {text}"
            );
            for identity in ["new seat identity", "provider cannot change", "provider change is not allowed"] {
                assert!(!text.contains(identity), "migration must keep the seat id: {text}");
            }
        }
    }
}

#[test]
#[serial_test::serial(env)]
fn migration_does_not_inherit_provider_scoped_settings() {
    let root = team(&[("one", "stopped")]);
    let plan = dry_run(
        root.path(), true,
        &["one", "--provider", "claude", "--session", "fresh"],
        Observation::StoppedVerified,
    ).unwrap();
    assert!(declared(&plan, "one", "model").is_null(), "Pi model must not follow to Claude: {plan}");
    assert!(declared(&plan, "one", "effort").is_null(), "Pi effort must not follow: {plan}");
    assert_eq!(declared(&plan, "one", "bypass"), json!(true), "provider-neutral bypass is kept: {plan}");
}

#[test]
#[serial_test::serial(env)]
fn provider_change_without_fresh_refuses_with_fresh_guidance() {
    let root = team(&[("one", "stopped")]);
    let before = snapshot(root.path());
    for argv in [
        vec!["one", "--provider", "claude"],
        vec!["one", "--provider", "claude", "--session", "preserve"],
        vec!["one", "--provider", "claude", "--session", "auto"],
    ] {
        let error = dry_run(root.path(), true, &argv, Observation::StoppedVerified)
            .expect_err("a provider change cannot keep the old native session");
        let text = error.to_string();
        assert!(text.contains("--session fresh"), "must name --session fresh: {text}");
        refusal_names(&error, "start");
        assert_eq!(snapshot(root.path()), before, "refusal precedes writes");
    }
}

// ---- poisoned (half-migrated) seats and rollback ---------------------------

/// The row live H11 A10/B4 left behind: the role doc and row already name the contract
/// provider, but no native instance exists and the seat is explicitly stopped.
fn poisoned_team() -> Sandbox {
    let root = team(&[("one", "stopped")]);
    std::fs::write(
        root.path().join("agents/one.md"),
        "---\nprovider: cursor\nmodel: composer-2.5\ndangerously_skip_permissions: true\n---\nReview the change.\n",
    )
    .unwrap();
    edit_state(root.path(), |state| {
        state["agents"]["one"] = json!({"provider":"cursor","status":"stopped","explicit_stop":true,
            "pane_id":null,"session_id":null,"window":"one"});
    });
    root
}

#[track_caller]
fn never_unknown_provider(result: &Result<Value, CliError>) {
    if let Err(error) = result {
        let text = error.to_string();
        assert!(!text.contains("unknown provider"), "a half-migrated seat must stay operable: {text}");
    }
}

#[test]
#[serial_test::serial(env)]
fn poisoned_seat_rolls_back_to_its_previous_provider_in_place() {
    let root = poisoned_team();
    let before = snapshot(root.path());
    let result = dry_run(
        root.path(), true,
        &["one", "--provider", "pi", "--model", "magpie/codex/gpt-6-luna", "--session", "fresh"],
        Observation::StoppedVerified,
    );
    never_unknown_provider(&result);
    let plan = result.unwrap_or_else(|error| panic!("rollback to the legacy provider must plan: {error}"));
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    assert_eq!(op(&plan, "one"), "provider_changed");
    assert!(provider_is(&declared(&plan, "one", "provider"), "pi"));
    assert_eq!(declared(&plan, "one", "model"), json!("magpie/codex/gpt-6-luna"));
}

#[test]
#[serial_test::serial(env)]
fn poisoned_seat_retry_and_plain_start_are_admitted_or_refused_by_native_admission() {
    let root = poisoned_team();
    for argv in [
        vec!["one", "--provider", "cursor", "--model", "composer-2.5", "--session", "fresh"],
        vec!["one", "--session", "fresh"],
        vec!["one"],
    ] {
        let result = dry_run(root.path(), true, &argv, Observation::StoppedVerified);
        never_unknown_provider(&result);
        match result {
            Ok(plan) => {
                let op = op(&plan, "one");
                assert!(
                    ["created", "adopted", "provider_changed", "fresh"].contains(&op.as_str()),
                    "{argv:?}: an unmaterialized native declaration is admitted, got op {op}: {plan}"
                );
            }
            Err(error) => {
                let text = error.to_string();
                let names_way_out = admitted_verbs(&text).iter().any(|verb| verb == "start");
                assert!(
                    text.contains("native") || names_way_out,
                    "{argv:?}: only native admission may refuse, or the refusal names `team-agent start`: {text}"
                );
                assert_admitted(&text);
            }
        }
    }
}

/// Executes (not dry-run) a legacy→contract migration on a host where the native
/// provider cannot be admitted. A definite pre-commit failure must leave the role doc
/// and the state row exactly as they were, so the seat stays startable on its old provider.
#[test]
#[serial_test::serial(env)]
fn failed_native_admission_leaves_the_declaration_and_row_untouched() {
    struct PathGuard(Option<std::ffi::OsString>);
    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                std::env::set_var("PATH", path);
            }
        }
    }
    let _path = PathGuard(std::env::var_os("PATH"));
    std::env::set_var("PATH", "/usr/bin:/bin"); // no cursor-agent, no tmux, no providers
    let root = team(&[("one", "stopped")]);
    let doc = root.path().join("agents/one.md");
    let role_before = std::fs::read(&doc).unwrap();
    let workspace = root.path().to_string_lossy().into_owned();
    let args: Vec<String> = ["one", "--provider", "cursor", "--model", "composer-2.5", "--session", "fresh",
        "--workspace", workspace.as_str(), "--team", "fixture", "--json"]
        .iter().map(|value| (*value).to_string()).collect();
    let outcome = run("start", &args, root.path()).expect("start is a lifecycle command");
    let failed = match &outcome {
        Ok(result) => match &result.output {
            CmdOutput::Json(value) => value["ok"] != json!(true),
            _ => true,
        },
        Err(_) => true,
    };
    assert!(failed, "the migration cannot succeed without the native provider: {outcome:?}");
    assert_eq!(std::fs::read(&doc).unwrap(), role_before, "role doc restored or never committed");
    let state: Value = serde_json::from_slice(&std::fs::read(state_path(root.path())).unwrap()).unwrap();
    for row in [&state["agents"]["one"], &state["teams"]["fixture"]["agents"]["one"]] {
        assert_eq!(row["provider"], json!("pi"), "the row keeps its previous provider: {row}");
    }
}

// ---- refusals that must point somewhere admitted --------------------------

#[test]
#[serial_test::serial(env)]
fn paused_seat_refusal_names_the_admitted_way_out() {
    let root = team(&[("one", "paused")]);
    // The declaration is the authority: the role doc itself says paused.
    std::fs::write(
        root.path().join("agents/one.md"),
        PI_ROLE.replacen("---\n", "---\npaused: true\n", 1),
    )
    .unwrap();
    let error = dry_run(root.path(), true, &["one"], Observation::StoppedVerified)
        .expect_err("start does not unpause");
    refusal_names(&error, "start");
}

#[test]
#[serial_test::serial(env)]
fn stopped_seat_paused_only_in_its_role_doc_is_not_started() {
    // Live H11 A15: `stop helper`, then the user declares `paused: true`; the row
    // still says stopped. The declaration is the authority, so start must refuse.
    let root = team(&[("one", "stopped")]);
    std::fs::write(
        root.path().join("agents/one.md"),
        PI_ROLE.replacen("---\n", "---\npaused: true\n", 1),
    )
    .unwrap();
    let before = snapshot(root.path());
    let error = dry_run(root.path(), true, &["one"], Observation::StoppedVerified)
        .expect_err("a seat declared paused must not start");
    refusal_names(&error, "start");
    assert_eq!(snapshot(root.path()), before, "refusal precedes writes");
}

#[test]
#[serial_test::serial(env)]
fn unknown_identity_refusal_names_an_admitted_diagnosis() {
    let root = team(&[("one", "running")]);
    let error = dry_run(root.path(), true, &["one"], Observation::Unknown)
        .expect_err("unknown identity performs no action");
    let text = error.to_string();
    assert_admitted(&text);
    assert!(
        admitted_verbs(&text).iter().any(|verb| verb == "status" || verb == "doctor"),
        "must name `team-agent status` or `team-agent doctor`: {text}"
    );
}

// ---- one seat verb ---------------------------------------------------------

#[test]
#[serial_test::serial(env)]
fn agent_add_is_not_a_second_engine() {
    let root = team(&[("one", "stopped")]);
    let workspace = root.path().to_string_lossy().into_owned();
    let args: Vec<String> = ["add", "newbie", "--provider", "claude", "--bypass", "false",
        "--workspace", workspace.as_str(), "--team", "fixture", "--dry-run", "--json"]
        .iter().map(|value| (*value).to_string()).collect();
    match run("agent", &args, root.path()).expect("agent is a lifecycle command") {
        Ok(result) => {
            let CmdOutput::Json(value) = result.output else { panic!("JSON expected") };
            assert_eq!(value["action"], json!("start"), "agent add must be start: {value}");
            assert_eq!(value["dry_run"], json!(true), "{value}");
            assert_eq!(op(&value, "newbie"), "created");
        }
        Err(error) => refusal_names(&error, "start"),
    }
}

#[test]
fn help_advertises_one_seat_verb() {
    let catalog = crate::cli::emit::default_help();
    for retired in ["add-agent", "start-agent", "stop-agent", "quick-start", "shutdown",
        "reset-agent", "remove-agent", "clone-agent", "fork-agent"] {
        assert!(
            !catalog.lines().any(|line| line.trim_start().starts_with(&format!("{retired} "))),
            "default help must not advertise {retired}:\n{catalog}"
        );
        if let Some(spec) = crate::cli::spec::command_spec(retired) {
            assert!(!spec.default_help && !spec.suggestion_index, "{retired} must not be suggested");
        }
    }
    let agent = help("agent");
    assert!(!agent.contains("start|stop|restart|add|"), "agent add is retired:\n{agent}");
    assert!(!agent.contains("complete decisions"), "help must match the defaulting resolver:\n{agent}");
    let start = help("start");
    assert!(start.contains("--session preserve|auto|fresh"), "{start}");
    assert!(start.contains("--provider") && start.contains("--session fresh"), "start help must teach in-place migration:\n{start}");
}

#[test]
#[serial_test::serial(env)]
fn team_restart_skips_a_running_seat_declared_paused_in_its_role_doc() {
    // Live H11 R3 C2-07: the user declares `paused: true` on a running seat; the row
    // still says running. The declaration is the authority: a team restart must skip
    // that seat with reason `paused`, exactly as `start` refuses it.
    let root = team(&[("one", "running"), ("two", "running")]);
    std::fs::write(
        root.path().join("agents/one.md"),
        PI_ROLE.replacen("---\n", "---\npaused: true\n", 1),
    )
    .unwrap();
    let before = snapshot(root.path());
    let mut restart = super::super::tests::intent(Verb::Restart);
    restart.workspace = root.path().to_owned();
    restart.subject = Subject::Team { source: None };
    restart.team = Some("fixture".into());
    restart.session = SessionPolicy::Auto;
    let plan = plan_with_observer(restart, |_, _, _| Ok(Observation::Live))
        .unwrap_or_else(|error| panic!("the unpaused member is still restartable: {error}"));
    assert_eq!(snapshot(root.path()), before, "planning must not write");
    assert!(
        plan.skipped.iter().any(|entry| entry["agent"] == "one" && entry["reason"] == "paused"),
        "a seat declared paused is skipped with reason paused: {:?}",
        plan.skipped
    );
    assert!(
        plan.steps.iter().all(|step| step.agent() != Some("one")),
        "no step may stop or start the paused seat: {:?}",
        plan.steps.iter().map(|step| (step.name(), step.agent())).collect::<Vec<_>>()
    );
}
