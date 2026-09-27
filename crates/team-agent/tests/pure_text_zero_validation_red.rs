#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::model::yaml::Value;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct TeamFixture {
    root: PathBuf,
    team_dir: PathBuf,
}

impl TeamFixture {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "team-agent-pure-text-{case}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let team_dir = root.join("team");
        fs::create_dir_all(team_dir.join("agents")).expect("create isolated team fixture");
        Self { root, team_dir }
    }

    fn write_docs(&self, team: &str, role_file: &str, role: &str) {
        fs::write(self.team_dir.join("TEAM.md"), team).expect("write TEAM.md");
        fs::write(self.team_dir.join("agents").join(role_file), role).expect("write role doc");
    }

    fn compile(&self) -> Result<Value, team_agent::model::ModelError> {
        team_agent::compiler::compile_team(&self.team_dir)
    }
}

impl Drop for TeamFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn first_agent(spec: &Value) -> &Value {
    spec.get("agents")
        .and_then(Value::as_list)
        .and_then(|agents| agents.first())
        .expect("compiled spec has one agent")
}

fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("compiled value has string field {key}: {value:?}"))
}

fn prompt(agent: &Value) -> &str {
    let system_prompt = agent.get("system_prompt").expect("agent has system_prompt");
    string_field(system_prompt, "inline")
}

fn bypass(agent: &Value) -> bool {
    match agent.get("dangerously_skip_permissions") {
        Some(Value::Bool(value)) => *value,
        value => panic!("expected boolean dangerously_skip_permissions, got {value:?}"),
    }
}

const VALID_ROLE: &str = "---\nname: worker\nrole: Worker\nprovider: codex\nmodel: custom-model\nauth_mode: subscription\ndangerously_skip_permissions: true\n---\nA valid role prompt.";

#[test]
fn r01_plain_text_team_md_becomes_objective() {
    let fixture = TeamFixture::new("r01");
    let objective = "Coordinate the release using a plain-text team document.";
    fixture.write_docs(objective, "worker.md", VALID_ROLE);

    let spec = fixture
        .compile()
        .expect("TEAM.md without frontmatter must compile");
    let compiled_objective = spec
        .get("team")
        .and_then(|team| team.get("objective"))
        .and_then(Value::as_str)
        .expect("compiled team has an objective");
    assert!(compiled_objective.contains(objective));
}

#[test]
fn r02_plain_text_role_derives_agent_id_from_filename() {
    let fixture = TeamFixture::new("r02");
    fixture.write_docs(
        "Plain-text team objective.",
        "alpha.md",
        "# Alpha\n\n## Responsibilities\n\nHelp coordinate the work.",
    );

    let spec = fixture
        .compile()
        .expect("frontmatter-free role documents must compile");
    assert_eq!(string_field(first_agent(&spec), "id"), "alpha");
}

#[test]
fn r03_undeclared_bypass_defaults_to_true() {
    let fixture = TeamFixture::new("r03");
    fixture.write_docs(
        "Plain-text team objective.",
        "worker.md",
        "---\nname: worker\nrole: Worker\nprovider: codex\nmodel: custom-model\nauth_mode: subscription\n---\nDo the work.",
    );

    let spec = fixture
        .compile()
        .expect("omitting the bypass field must not prevent role compilation");
    assert!(bypass(first_agent(&spec)), "all compiled roles default to Bypass");
}

#[test]
fn r04_undeclared_provider_defaults_to_pi() {
    let fixture = TeamFixture::new("r04");
    fixture.write_docs(
        "Plain-text team objective.",
        "worker.md",
        "---\nname: worker\nrole: Worker\nauth_mode: subscription\ndangerously_skip_permissions: true\n---\nDo the work.",
    );

    let spec = fixture
        .compile()
        .expect("omitting provider must use the documented default");
    assert_eq!(string_field(first_agent(&spec), "provider"), "pi");
}

#[test]
fn r05_undeclared_model_and_effort_use_luna_max_defaults() {
    let fixture = TeamFixture::new("r05");
    fixture.write_docs(
        "Plain-text team objective.",
        "worker.md",
        "---\nname: worker\nrole: Worker\nprovider: pi\nauth_mode: subscription\ndangerously_skip_permissions: true\n---\nDo the work.",
    );

    let spec = fixture
        .compile()
        .expect("omitting model and effort must use the documented defaults");
    let agent = first_agent(&spec);
    assert_eq!(
        string_field(agent, "model"),
        "openai-codex/gpt-6-luna"
    );
    assert_eq!(string_field(agent, "effort"), "max");
}

#[test]
fn r06_markdown_headings_lists_and_prose_are_preserved_as_prompt() {
    let fixture = TeamFixture::new("r06");
    let role = "# Title\n\n## Subtitle\n\n* bullet item\n\nAn ordinary prose paragraph.";
    fixture.write_docs("Plain-text team objective.", "worker.md", role);

    let spec = fixture
        .compile()
        .expect("Markdown structure in a role prompt must not be validated");
    assert_eq!(prompt(first_agent(&spec)), role);
}

#[test]
fn r07_unclosed_pseudo_yaml_falls_back_to_the_entire_plain_text_prompt() {
    let fixture = TeamFixture::new("r07");
    let role = "---\nname: ignored\n  malformed: indentation\n# Prompt after pseudo YAML";
    fixture.write_docs("Plain-text team objective.", "alpha.md", role);

    let spec = fixture
        .compile()
        .expect("unclosed or malformed pseudo-frontmatter must fall back to plain text");
    let agent = first_agent(&spec);
    assert_eq!(string_field(agent, "id"), "alpha");
    assert_eq!(prompt(agent), role);
}

#[test]
fn r08_valid_frontmatter_explicit_values_override_defaults() {
    let fixture = TeamFixture::new("r08");
    let role = "---\nname: explicit-worker\nrole: Explicit Worker\nprovider: codex\nmodel: custom-model\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\nExplicitly configured prompt.";
    fixture.write_docs("Plain-text team objective.", "worker.md", role);

    let spec = fixture
        .compile()
        .expect("valid historical frontmatter remains supported");
    let agent = first_agent(&spec);
    assert_eq!(string_field(agent, "id"), "explicit-worker");
    assert_eq!(string_field(agent, "provider"), "codex");
    assert_eq!(string_field(agent, "model"), "custom-model");
    assert!(!bypass(agent), "an explicit false value overrides the default");
    assert_eq!(prompt(agent), "Explicitly configured prompt.");
}

#[test]
fn r09_plain_text_directory_compiles_to_valid_yaml_spec() {
    let fixture = TeamFixture::new("r09");
    let objective = "Build a team from plain-text documents.";
    fixture.write_docs(objective, "worker.md", "# Worker\n\nImplement the assigned task.");

    let spec = fixture
        .compile()
        .expect("compile_team must accept a plain-text TEAM.md and role document");
    let serialized = team_agent::model::yaml::dumps(&spec);
    let spec_path = fixture.team_dir.join("team.spec.yaml");
    fs::write(&spec_path, &serialized).expect("persist compiled team.spec.yaml");
    let persisted = fs::read_to_string(&spec_path).expect("read persisted team.spec.yaml");
    let loaded = team_agent::model::spec::load_and_validate_spec(&persisted, &fixture.root)
        .expect("persisted spec must remain valid and loadable");

    assert_eq!(loaded, spec, "YAML round-trip preserves the compiled spec");
    assert_eq!(
        string_field(loaded.get("team").expect("spec has team"), "objective"),
        objective
    );
    let agent = first_agent(&loaded);
    assert_eq!(string_field(agent, "id"), "worker");
    assert_eq!(string_field(agent, "provider"), "pi");
    assert_eq!(string_field(agent, "model"), "openai-codex/gpt-6-luna");
    assert_eq!(string_field(agent, "effort"), "max");
    assert!(bypass(agent));
}