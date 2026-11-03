//! RED — unified seat lifecycle end state, clean guidance: every recovery command the
//! framework emits is admitted by the current grammar on the canonical surface.
//! Compatibility verbs, the retired `agent add`, and `--force` re-adoption are
//! never recommended. The oracle self-tests pass on the base and prove that the
//! canonical replacements are already admissible on the base grammar.
use crate::cli::guidance_oracle::{
    admitted_verbs, assert_admitted, production_guidance_violations, violations,
};

fn argv(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

#[test]
fn oracle_admits_the_canonical_replacements_on_the_current_grammar() {
    for command in [
        "team-agent start reviewer",
        "team-agent start reviewer --role-file /abs/reviewer.md",
        "team-agent start reviewer --session auto",
        "team-agent start --from ./roles --workspace /tmp/ws --team research",
        "team-agent stop reviewer --team research",
        "team-agent agent restart w1 --workspace '/tmp/ws' --team research --json",
        "team-agent restart --allow-fresh",
        "team-agent agent reset w1 --discard-session",
        "team-agent agent remove w1 --confirm --force",
        "run `team-agent status` to list agent ids",
    ] {
        assert_admitted(command);
    }
}

#[test]
fn oracle_rejects_compatibility_retired_and_inadmissible_guidance() {
    for text in [
        "To recover manually, run: team-agent start-agent w1 --workspace '/tmp/ws' --team research --force --json",
        "To add a NEW role at runtime use: team-agent add-agent <id> --role-file <path>",
        "team-agent agent add reviewer --provider pi",
        "Run `team-agent quick-start <workspace>` for first launch",
        "agent w1 is already running; use stop-agent first",
        "stop the agent, then rerun reset-agent",
        "role file already exists: /x/agents/w1.md; reuse it with --role-file or re-adopt it with --force",
        "team-agent agent reset w1",
        "team-agent agent remove w1",
        "team-agent start w1 --frobnicate",
    ] {
        assert!(!violations(text).is_empty(), "oracle must reject: {text}");
    }
}

#[test]
fn start_agent_force_refusal_names_admitted_canonical_commands() {
    let error = crate::cli::arguments::parse("start-agent", &argv("w1 --force"))
        .err()
        .expect("start-agent --force stays refused")
        .to_string();
    assert_admitted(&error);
    let verbs = admitted_verbs(&error);
    assert!(
        verbs.iter().any(|verb| verb == "stop") && verbs.iter().any(|verb| verb == "start"),
        "refusal must name `team-agent stop AGENT` then `team-agent start AGENT`: {error}"
    );
}

#[test]
fn no_production_recovery_guidance_recommends_a_compatibility_verb() {
    let found = production_guidance_violations();
    assert!(
        found.is_empty(),
        "{} recovery hints still recommend compatibility verbs or --force re-adoption:\n{}",
        found.len(),
        found.join("\n")
    );
}

fn skill_documents() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/team-agent");
    let mut files = vec![root.join("SKILL.md")];
    for directory in [root.join("references"), root.join("docs/reference")] {
        if let Ok(entries) = std::fs::read_dir(directory) {
            files.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    files
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("read skill document");
            (path.display().to_string(), text)
        })
        .collect()
}

#[test]
fn skill_teaches_only_admitted_canonical_commands() {
    let mut found = Vec::new();
    for (path, text) in skill_documents() {
        let mut fenced = false;
        for (index, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            let violations = if fenced && line.trim_start().starts_with("team-agent ") {
                violations(line)
            } else {
                crate::cli::guidance_oracle::compatibility_cues(line)
            };
            found.extend(violations.into_iter().map(|v| format!("{path}:{}: {v}", index + 1)));
        }
    }
    assert!(found.is_empty(), "skill teaches retired or inadmissible commands:\n{}", found.join("\n"));
}

#[test]
fn skill_teaches_in_place_migration_and_visible_defaults() {
    let skill = &skill_documents()[0].1;
    for needle in ["--session fresh", "--provider", "defaulted", "team-agent stop"] {
        assert!(skill.contains(needle), "SKILL.md must teach `{needle}` for the unified start contract");
    }
    for retired in ["add-agent", "start-agent", "agent add", "quick-start"] {
        assert!(!skill.contains(retired), "SKILL.md must not mention retired `{retired}`");
    }
}
