//! T05 — the public communication-mode catalog is the sole enumeration source.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::communication_mode::CommunicationMode;
use team_agent::compiler::compile_team;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn compile_fixture(team_value: Option<&str>, role_value: Option<&str>) -> Result<(), String> {
    let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!(
        "communication-mode-catalog-red-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("agents")).expect("create T05 fixture agents directory");
    let field = |value: Option<&str>| {
        value
            .map(|value| format!("communication_mode: {value}\n"))
            .unwrap_or_default()
    };
    fs::write(
        root.join("TEAM.md"),
        format!(
            "---\nname: t05-catalog\nprovider: codex\n{}---\n\nT05 fixture.\n",
            field(team_value)
        ),
    )
    .expect("write T05 TEAM.md");
    fs::write(
        root.join("agents/worker.md"),
        format!(
            "---\nname: worker\nrole: Worker\nprovider: codex\nmodel: gpt-5\ntools:\n  - mcp_team\n{}---\n\nT05 worker.\n",
            field(role_value)
        ),
    )
    .expect("write T05 role doc");
    let result = compile_team(&root)
        .map(|_| ())
        .map_err(|error| error.to_string());
    let _ = fs::remove_dir_all(root);
    result
}

#[test]
fn t05_only_official_catalog_modes_parse_and_a_third_value_is_rejected() {
    let official: Vec<_> = CommunicationMode::ALL
        .iter()
        .copied()
        .map(|mode| (mode, mode.as_str()))
        .collect();

    assert_eq!(
        official.len(),
        2,
        "T05: the official product catalog must contain exactly the two signed communication modes"
    );
    assert_eq!(
        official
            .iter()
            .map(|(_, value)| *value)
            .collect::<BTreeSet<_>>()
            .len(),
        official.len(),
        "T05: every CommunicationMode::ALL entry must have a unique canonical value"
    );
    for (mode, value) in &official {
        assert_eq!(
            CommunicationMode::parse(value),
            Some(*mode),
            "T05: every mode derived from CommunicationMode::ALL must parse back to itself"
        );
    }

    assert!(
        CommunicationMode::parse("leader_centric").is_some(),
        "T05/L positive control: leader_centric must remain an official accepted mode"
    );
    assert!(
        CommunicationMode::parse("orchestrated").is_some(),
        "T05/O positive control: orchestrated must remain an official accepted mode"
    );
    assert!(
        CommunicationMode::parse("synthetic_third_mode").is_none(),
        "T05/negative: a third or custom communication mode must fail closed"
    );
}

#[test]
fn t05_present_non_string_values_fail_closed() {
    for (label, team_value, role_value) in [("team", Some("7"), None), ("role", None, Some("7"))] {
        let error = compile_fixture(team_value, role_value)
            .expect_err("T05/non-string: a present non-string communication_mode must fail closed");
        assert!(
            error.contains("communication_mode"),
            "T05/non-string/{label}: rejection must identify communication_mode; error={error}"
        );
    }
}

#[test]
fn t05_whitespace_wrapped_official_spellings_fail_exact_match() {
    for (label, team_value, role_value) in [
        ("team", Some("\" leader_centric \""), None),
        ("role", None, Some("\" orchestrated \"")),
    ] {
        let error = compile_fixture(team_value, role_value).expect_err(
            "T05/whitespace: whitespace-wrapped official spelling must fail exact match",
        );
        assert!(
            error.contains("communication_mode"),
            "T05/whitespace/{label}: rejection must identify communication_mode; error={error}"
        );
    }
}
