//! 0.5.61 communication_mode deterministic configuration RED contracts.
//!
//! These tests use the public document compiler and inspect its canonical output.
//! They deliberately do not call a future API: baseline failures must say that
//! the compiled product projection lacks/rejects `communication_mode`, not fail
//! to compile in the harness.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::compiler::compile_team;
use team_agent::model::yaml::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(team_mode: Option<&str>, roles: &[(&str, Option<&str>)]) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!(
            "communication-mode-red-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("agents")).expect("create fixture agents directory");
        Self::write_team(&root, team_mode);
        for (name, mode) in roles {
            Self::write_role(&root, name, *mode);
        }
        Self { root }
    }

    fn rewrite_team(&self, mode: Option<&str>) {
        Self::write_team(&self.root, mode);
    }

    fn write_team(root: &Path, mode: Option<&str>) {
        let mode = mode
            .map(|value| format!("communication_mode: {value}\n"))
            .unwrap_or_default();
        fs::write(
            root.join("TEAM.md"),
            format!(
                "---\nname: communication-mode-red\nprovider: codex\n{mode}---\n\nContract fixture.\n"
            ),
        )
        .expect("write TEAM.md");
    }

    fn write_role(root: &Path, name: &str, mode: Option<&str>) {
        let mode = mode
            .map(|value| format!("communication_mode: {value}\n"))
            .unwrap_or_default();
        fs::write(
            root.join("agents").join(format!("{name}.md")),
            format!(
                "---\nname: {name}\nrole: Verification Worker\nprovider: codex\nmodel: gpt-5\ntools:\n  - fs_read\n  - mcp_team\n{mode}---\n\nStable persona body for {name}.\n"
            ),
        )
        .expect("write role doc");
    }

    fn compile(&self) -> Result<Value, String> {
        compile_team(&self.root).map_err(|error| error.to_string())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn agent<'a>(spec: &'a Value, id: &str) -> &'a Value {
    spec.get("agents")
        .and_then(Value::as_list)
        .expect("compiled agents array")
        .iter()
        .find(|agent| agent.get("id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("compiled agent {id} exists"))
}

fn effective_mode(agent: &Value, tooth: &str) -> String {
    agent
        .get("communication_mode")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "{tooth}: compiled agent contract lacks effective communication_mode; expected the public projection to contain an official mode"
            )
        })
        .to_string()
}

/// T01 — F2 default/override 1; matrix configuration gate 1.
#[test]
fn t01_omitted_team_and_role_project_leader_centric() {
    let fixture = Fixture::new(None, &[("worker", None)]);
    let spec = fixture.compile().expect("omitted mode remains valid input");
    assert_eq!(
        effective_mode(agent(&spec, "worker"), "T01"),
        "leader_centric",
        "T01: omitted/omitted must project leader_centric"
    );
}

/// T02 — F2 default/override 2-3; matrix configuration gates 2-4.
#[test]
fn t02_team_value_is_inherited_and_role_override_is_local() {
    let fixture = Fixture::new(
        Some("orchestrated"),
        &[("inherited", None), ("overridden", Some("leader_centric"))],
    );
    let spec = fixture.compile().expect("official team/role modes compile");
    assert_eq!(
        effective_mode(agent(&spec, "inherited"), "T02/inherit"),
        "orchestrated"
    );
    assert_eq!(
        effective_mode(agent(&spec, "overridden"), "T02/override"),
        "leader_centric"
    );
}

/// T02 — reverse team/role combination is an independent positive control.
#[test]
fn t02_reverse_override_changes_only_the_overridden_role() {
    let fixture = Fixture::new(
        Some("leader_centric"),
        &[("inherited", None), ("overridden", Some("orchestrated"))],
    );
    let spec = fixture.compile().expect("reverse official modes compile");
    assert_eq!(
        effective_mode(agent(&spec, "inherited"), "T02/reverse-inherit"),
        "leader_centric"
    );
    assert_eq!(
        effective_mode(agent(&spec, "overridden"), "T02/reverse-override"),
        "orchestrated"
    );
}

/// T03 — F2 default/override 4; matrix configuration gate 5.
#[test]
fn t03_unknown_team_mode_fails_before_any_member_projection() {
    let fixture = Fixture::new(Some("synthetic_unknown_mode"), &[("worker", None)]);
    let error = fixture.compile().expect_err(
        "T03/team: unknown team communication_mode must fail closed before members are projected",
    );
    assert!(
        error.contains("communication_mode") && error.contains("synthetic_unknown_mode"),
        "T03/team: rejection must identify communication_mode and the rejected value; error={error}"
    );
}

/// T03 — role-level invalid input has the same pre-projection atomic boundary.
#[test]
fn t03_unknown_role_mode_fails_before_any_member_projection() {
    let fixture = Fixture::new(None, &[("worker", Some("synthetic_unknown_mode"))]);
    let error = fixture.compile().expect_err(
        "T03/role: unknown role communication_mode must fail closed before members are projected",
    );
    assert!(
        error.contains("communication_mode") && error.contains("synthetic_unknown_mode"),
        "T03/role: rejection must identify communication_mode and the rejected value; error={error}"
    );
}

/// T04 — F2 promises 4/6 and matrix common criteria 1/3.
#[test]
fn t04_mode_change_preserves_non_mode_role_projection() {
    let fixture = Fixture::new(Some("leader_centric"), &[("worker", None)]);
    let left = fixture.compile().expect("leader_centric fixture compiles");
    fixture.rewrite_team(Some("orchestrated"));
    let right = fixture.compile().expect("orchestrated fixture compiles");

    let (left_agent, left_mode) = without_mode(agent(&left, "worker"), "leader_centric");
    let (right_agent, right_mode) = without_mode(agent(&right, "worker"), "orchestrated");

    assert_ne!(left_mode, right_mode, "T04: the selected modes must differ");
    assert_eq!(
        left_agent, right_agent,
        "T04: mode selection must not alter persona, provider, model, identity shape, tools, or permission projection"
    );
}

fn without_mode(agent: &Value, label: &str) -> (Value, Value) {
    let Value::Map(items) = agent else {
        panic!("T04: {label} agent projection must be a map")
    };
    let mode = items
        .iter()
        .find(|(key, _)| key == "communication_mode")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("T04: {label} projection lacks communication_mode"));
    let rest = Value::Map(
        items
            .iter()
            .filter(|(key, _)| key != "communication_mode")
            .cloned()
            .collect(),
    );
    (rest, mode)
}
