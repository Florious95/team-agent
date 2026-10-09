//! Independent red/green contract for removing the public `compile` CLI adapter.
//!
//! The R cases must fail on v0.5.97 (where `compile` is still registered). The
//! P cases exercise compiler and lifecycle consumers that must remain intact.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use hermetic_guard::HermeticTestEnv;
use serde_json::Value;
use serial_test::serial;
use team_agent::cli::{
    cmd_add_agent, cmd_quick_start, cmd_restart, AddAgentArgs, QuickStartArgs, RestartArgs,
};

const TEAM_MD: &str = "---\nname: contract-team\nobjective: Contract fixture.\nprovider: codex\nmodel: gpt-5.5\n---\n\nContract fixture.\n";
const ROLE_MD: &str = "---\nname: worker\nrole: Contract Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nDo contract work.\n";
const INVALID_ROLE_MD: &str = "---\nname: broken\nrole: Broken Worker\nmodel: gpt-5.5\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\n\nMissing provider.\n";

fn fixture(env: &HermeticTestEnv, tag: &str) -> PathBuf {
    let team = env.workspace(tag);
    fs::create_dir_all(team.join("agents")).expect("create agents fixture");
    fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    fs::write(team.join("agents/worker.md"), ROLE_MD).expect("write role fixture");
    team
}

fn invalid_fixture(env: &HermeticTestEnv, tag: &str) -> PathBuf {
    let team = fixture(env, tag);
    fs::write(team.join("agents/broken.md"), INVALID_ROLE_MD).expect("write invalid role");
    team
}

fn run(env: &HermeticTestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.run_cli(cwd, args)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON stdout, rc={:?}, stdout={:?}, stderr={:?}: {error}",
            output.status.code(),
            stdout(output),
            stderr(output)
        )
    })
}

fn assert_unknown_compile(output: &Output) {
    let err = stderr(output);
    assert_eq!(output.status.code(), Some(1), "stderr={err:?}");
    assert!(
        output.stdout.is_empty(),
        "unknown command must not write stdout"
    );
    assert!(
        err.contains("Unknown command: 'compile'"),
        "not generic unknown: {err:?}"
    );
    assert!(
        !err.contains("missing --team"),
        "reached compile argument parsing: {err:?}"
    );
    assert!(
        !err.contains("team.spec.yaml"),
        "reached compiler/runtime handling: {err:?}"
    );
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = fs::read_dir(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
            .map(|entry| entry.expect("fixture entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            let key = entry
                .strip_prefix(root)
                .expect("snapshot path under root")
                .to_string_lossy()
                .into_owned();
            if entry.is_dir() {
                visit(root, &entry, files);
            } else {
                files.insert(key, fs::read(entry).expect("read fixture file"));
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

// ───────────────────────────────────────────── R: removed-command contract ──

#[test]
#[serial(env)]
fn r1_compile_and_compile_json_are_generic_unknown_commands() {
    let env = HermeticTestEnv::enter("compile-r1");
    let cwd = env.workspace("cwd");
    for args in [["compile"].as_slice(), ["compile", "--json"].as_slice()] {
        assert_unknown_compile(&run(&env, &cwd, args));
    }
}

#[test]
#[serial(env)]
fn r2_compile_help_forms_never_show_private_help() {
    let env = HermeticTestEnv::enter("compile-r2");
    let cwd = env.workspace("cwd");
    for args in [
        ["compile", "--help"].as_slice(),
        ["compile", "-h"].as_slice(),
        ["compile", "--help", "--json"].as_slice(),
    ] {
        let output = run(&env, &cwd, args);
        assert_unknown_compile(&output);
        assert!(!stderr(&output).contains("team-agent compile"));
    }
}

#[test]
#[serial(env)]
fn r3_legacy_arguments_cannot_write_out_or_default_spec() {
    let env = HermeticTestEnv::enter("compile-r3");
    let team = fixture(&env, "team");
    let out = team.join("exported.yaml");
    fs::write(&out, b"sentinel bytes\n").expect("write sentinel");
    let before = fs::read(&out).expect("read sentinel");

    let output = run(
        &env,
        &team,
        &[
            "compile",
            "--team",
            team.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ],
    );
    assert_unknown_compile(&output);
    assert_eq!(fs::read(&out).expect("read sentinel"), before);

    let default_spec = team.join("team.spec.yaml");
    let _ = fs::remove_file(&default_spec);
    let output = run(
        &env,
        &team,
        &["compile", "--team", team.to_str().unwrap(), "--json"],
    );
    assert_unknown_compile(&output);
    assert!(!default_spec.exists(), "default team.spec.yaml was created");
}

#[test]
#[serial(env)]
fn r4_bad_team_role_and_path_inputs_are_not_read() {
    let env = HermeticTestEnv::enter("compile-r4");
    let invalid_role = invalid_fixture(&env, "invalid role");
    let no_team_md = env.workspace("no TEAM.md");
    fs::create_dir_all(no_team_md.join("agents")).expect("create incomplete fixture");
    fs::write(no_team_md.join("agents/worker.md"), ROLE_MD).expect("write role");
    let missing = env.root().join("does-not-exist");
    let valid = fixture(&env, "valid team with spaces");
    let cases = [
        vec!["compile", "--team", missing.to_str().unwrap()],
        vec!["compile", "--team", no_team_md.to_str().unwrap()],
        vec!["compile", "--team", invalid_role.to_str().unwrap()],
        vec!["compile", "--team", valid.to_str().unwrap(), "--json"],
        vec![
            "compile",
            "--out",
            "old.yaml",
            "--team",
            valid.to_str().unwrap(),
        ],
    ];
    for args in cases {
        assert_unknown_compile(&run(&env, env.root(), &args));
    }
}

#[test]
#[serial(env)]
fn r5_compile_has_no_fixture_or_adjacent_team_side_effects() {
    let env = HermeticTestEnv::enter("compile-r5");
    let team = fixture(&env, "team");
    let adjacent = fixture(&env, "adjacent-team");
    fs::write(adjacent.join("sentinel.log"), b"unchanged\n").expect("write sentinel");
    let before = snapshot(env.root());
    let output = run(
        &env,
        env.root(),
        &["compile", "--team", team.to_str().unwrap(), "--json"],
    );
    assert_unknown_compile(&output);
    assert_eq!(
        snapshot(env.root()),
        before,
        "compile changed fixture state"
    );
}

#[test]
#[serial(env)]
fn r6_help_and_registry_do_not_expose_compile() {
    let env = HermeticTestEnv::enter("compile-r6");
    let cwd = env.workspace("cwd");
    let help = run(&env, &cwd, &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(!stdout(&help).contains("compile"));

    let typo = run(&env, &cwd, &["compil"]);
    assert_eq!(typo.status.code(), Some(1));
    assert!(!stderr(&typo).contains("did you mean `compile`"));
    assert_unknown_compile(&run(&env, &cwd, &["compile", "--help"]));
}

// ───────────────────────────────────────────── P: retained compiler/lifecycle consumers ──

#[test]
#[serial(env)]
fn p1_compiler_and_spec_validation_remain_available() {
    let env = HermeticTestEnv::enter("compile-p1");
    let team = fixture(&env, "team");
    let spec = team_agent::compiler::compile_team(&team).expect("valid fixture compiles");
    let workspace = spec
        .get("team")
        .and_then(|team| team.get("workspace"))
        .and_then(team_agent::model::yaml::Value::as_str)
        .expect("compiled workspace");
    team_agent::model::spec::validate_spec(&spec, Path::new(workspace))
        .expect("compiled fixture passes validation");
    let agents = spec
        .get("agents")
        .and_then(team_agent::model::yaml::Value::as_list)
        .expect("compiled agents");
    assert_eq!(
        agents[0]
            .get("id")
            .and_then(team_agent::model::yaml::Value::as_str),
        Some("worker")
    );
}

#[test]
#[serial(env)]
fn p2_doctor_still_executes_compile_checks() {
    let env = HermeticTestEnv::enter("compile-p2");
    let team = fixture(&env, "team");

    let doctor = run(
        &env,
        env.root(),
        &["doctor", "--workspace", team.to_str().unwrap(), "--json"],
    );
    assert_eq!(doctor.status.code(), Some(0), "doctor: {}", stderr(&doctor));
    let body = json_stdout(&doctor);
    assert_eq!(body["ok"], true);
    assert_eq!(body["profile_smoke"]["ok"], true);
}

#[test]
#[serial(env)]
fn p3_invalid_role_remains_a_real_compile_diagnostic() {
    let env = HermeticTestEnv::enter("compile-p3");
    let team = invalid_fixture(&env, "invalid role");
    let error = team_agent::compiler::compile_team(&team).expect_err("invalid role must fail");
    assert!(error.to_string().contains("provider"));
    let doctor = run(
        &env,
        env.root(),
        &["doctor", "--workspace", team.to_str().unwrap(), "--json"],
    );
    assert_eq!(doctor.status.code(), Some(1), "doctor: {}", stderr(&doctor));
    let body = json_stdout(&doctor);
    assert_eq!(body["ok"], false);
    assert_eq!(body["profile_smoke"]["ok"], false, "{body}");
    assert!(body["profile_smoke"].to_string().contains("provider"), "{body}");
}

#[test]
#[serial(env)]
fn p4_quick_start_compiles_valid_team_and_rejects_invalid_role() {
    let env = HermeticTestEnv::enter("compile-p4");
    let valid = fixture(&env, "valid");
    let workspace = env.workspace("runtime");
    let _result = cmd_quick_start(&QuickStartArgs {
        workspace: workspace.clone(),
        agents_dir: valid,
        name: None,
        team_id: Some("contract-team".into()),
        yes: false,
        backend: None,
        json: true,
        detail: true,
    })
    .expect("quick-start returns a typed result");
    assert!(workspace
        .join(".team/runtime/contract-team/team.spec.yaml")
        .exists());

    let invalid = invalid_fixture(&env, "invalid");
    let result = cmd_quick_start(&QuickStartArgs {
        workspace: env.workspace("invalid-runtime"),
        agents_dir: invalid,
        name: None,
        team_id: None,
        yes: false,
        backend: None,
        json: true,
        detail: true,
    })
    .expect("invalid quick-start returns a typed result");
    assert!(format!("{result:?}").contains("provider"));
}

#[test]
#[serial(env)]
fn p5_restart_and_add_agent_keep_real_lifecycle_paths() {
    let env = HermeticTestEnv::enter("compile-p5");
    let workspace = env.workspace("runtime");
    let restart = cmd_restart(&RestartArgs {
        workspace: workspace.clone(),
        team: None,
        allow_fresh: false,
        session_converge_deadline_ms: None,
        json: true,
        detail: false,
    });
    assert!(format!("{restart:?}").contains("spec") || format!("{restart:?}").contains("Err"));

    let add = cmd_add_agent(&AddAgentArgs {
        role_config: Default::default(),
        agent: "new-worker".into(),
        workspace,
        team: None,
        role_file: env.root().join("missing-role.md").display().to_string(),
        force: false,
        json: true,
    });
    assert!(format!("{add:?}").contains("missing") || format!("{add:?}").contains("role"));
}

#[test]
#[serial(env)]
fn p1_compiler_retains_core_shape_and_is_read_only() {
    let env = HermeticTestEnv::enter("compile-p1-shape");
    let team = fixture(&env, "team");
    let before = snapshot(env.root());
    let spec = team_agent::compiler::compile_team(&team).expect("fixture compiles");
    assert_eq!(
        spec.get("version")
            .and_then(team_agent::model::yaml::Value::as_i64),
        Some(1)
    );
    assert_eq!(
        spec.get("team")
            .and_then(|team| team.get("name"))
            .and_then(team_agent::model::yaml::Value::as_str),
        Some("contract-team")
    );
    let agents = spec
        .get("agents")
        .and_then(team_agent::model::yaml::Value::as_list)
        .expect("compiled agents");
    assert_eq!(
        agents[0]
            .get("provider")
            .and_then(team_agent::model::yaml::Value::as_str),
        Some("codex")
    );
    assert_eq!(snapshot(env.root()), before);
}

#[test]
#[serial(env)]
fn p3_missing_team_md_is_reported_by_compiler_path() {
    let env = HermeticTestEnv::enter("compile-p3-missing-team");
    let team = env.workspace("missing TEAM.md");
    fs::create_dir_all(team.join("agents")).expect("create agents");
    fs::write(team.join("agents/worker.md"), ROLE_MD).expect("write role");
    let error = team_agent::compiler::compile_team(&team).expect_err("TEAM.md is required");
    assert!(error.to_string().contains("TEAM.md"));
}

#[test]
#[serial(env)]
fn p4_quick_start_keeps_canonical_runtime_spec_location() {
    let env = HermeticTestEnv::enter("compile-p4-location");
    let team = fixture(&env, "team");
    let workspace = env.workspace("runtime");
    let _ = cmd_quick_start(&QuickStartArgs {
        workspace: workspace.clone(),
        agents_dir: team.clone(),
        name: None,
        team_id: Some("location-team".into()),
        yes: false,
        backend: None,
        json: true,
        detail: true,
    });
    assert!(workspace
        .join(".team/runtime/location-team/team.spec.yaml")
        .is_file());
    assert!(!team.join("team.spec.yaml").exists());
}

#[test]
#[serial(env)]
fn p5_lifecycle_commands_remain_registered() {
    let env = HermeticTestEnv::enter("compile-p5-registry");
    for (command, exit) in [
        ("restart", 0),
        ("add-agent", 0),
        ("doctor", 0),
        ("quick-start", 0),
    ] {
        let output = run(&env, env.root(), &[command, "--help"]);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{command}: {}",
            stderr(&output)
        );
    }
}
