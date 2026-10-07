//! Issue #286 independent Human catalog / hidden discovery / actual help properties.
//! Original frozen product: 768c9352; #289 additions are implementation-owned regression properties.
//! Equivalent real-machine isolation: HermeticTestEnv owns HOME/cwd and scrubs
//! all caller identities; native canaries belong only to each fixture.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[path = "../src/cli/spec.rs"]
mod catalog;
#[path = "support/hermetic.rs"]
mod hermetic;
#[path = "support/human_catalog.rs"]
mod human_catalog;

use hermetic::HermeticTestEnv;
use human_catalog::RETIRED_COMMANDS as RETIRED;
use regex::Regex;
use serial_test::serial;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

const HUMAN: &[&str] = &[
    "quick-start",
    "send",
    "status",
    "models",
    "restart",
    "shutdown",
    "add-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "claim-leader",
    "takeover",
    "attach-leader",
    "codex",
    "claude",
    "copilot",
    "grok",
    "cursor",
    "pi",
    "leaders",
    "doctor",
    "remove-agent",
    "fork-agent",
    "clone-agent",
    "approvals",
    "route",
    "profile",
    "install-skill",
    "inbox",
];
const MACHINE: &[&str] = &[
    "wait",
    "attach-app-server-leader",
    "coordinator",
];
const JARGON: &[&str] = &[
    "fully-qualified",
    "fully qualified",
    "logical recipient",
    "logical to",
    "logical target",
    "persist message",
    "persisted message",
    "harness",
    "stable-qualified",
    "qualified name",
    "case_id",
    "result_received",
    "provider.worker.spawn_argv",
];

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, at: &Path, map: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                map.insert(key, None);
                visit(root, &path, map);
            } else {
                map.insert(key, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut map = BTreeMap::new();
    visit(root, root, &mut map);
    map
}
fn canary_path(env: &HermeticTestEnv) -> String {
    let bin = env.root().join("native-canaries");
    fs::create_dir(&bin).unwrap();
    let log = env.root().join("native-called");
    for tool in [
        "pi", "codex", "claude", "copilot", "grok", "agent", "cursor", "gh", "tmux",
    ] {
        let path = bin.join(tool);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' native >> '{}'\nexit 77\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}
fn words(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter(|s| !s.is_empty())
        .collect()
}
fn no_jargon(text: &str) {
    let lower = text.to_lowercase();
    let found = JARGON
        .iter()
        .filter(|word| lower.contains(**word))
        .collect::<Vec<_>>();
    assert!(
        found.is_empty(),
        "H3 author-generated Human jargon leaked {found:?}:\n{text}"
    );
}
fn examples(help: &str, command: &str) -> Vec<Vec<String>> {
    let mut in_examples = false;
    let mut found = Vec::new();
    for line in help.lines() {
        let lower = line.to_lowercase();
        if lower.contains("下一步") || lower.contains("next action") {
            break;
        }
        if lower.contains("示例")
            || lower.contains("例子")
            || lower.contains("examples")
            || lower.contains("怎么用")
        {
            in_examples = true;
        }
        if !in_examples {
            continue;
        }
        let line = line.trim().trim_start_matches("$ ").trim_matches('`');
        if let Some(rest) = line.strip_prefix("team-agent ") {
            let argv = shell_words(rest);
            if argv.first().map(String::as_str) == Some(command) {
                found.push(argv);
            }
        }
    }
    found
}
// Only quote/escape splitting: never execute a shell, expand credentials, or run substitutions.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut active = false;
    for c in line.chars() {
        if escape {
            token.push(c);
            escape = false;
            active = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            active = true;
        } else if c == '#' {
            break;
        } else if c.is_whitespace() {
            if active {
                words.push(std::mem::take(&mut token));
                active = false;
            }
        } else {
            token.push(c);
            active = true;
        }
    }
    assert!(
        quote.is_none() && !escape,
        "invalid quoting in actual help example: {line}"
    );
    if active {
        words.push(token);
    }
    words
}
fn help_property(command: &str) {
    let env = HermeticTestEnv::enter(command);
    let cwd = env.workspace("help");
    let path = canary_path(&env);
    fs::write(
        env.home().join(".team-agent/argv-routing.json"),
        "{invalid route config",
    )
    .unwrap();
    let before = snapshot(env.root());
    for flag in ["-h", "--help"] {
        let out = env.run_cli_env(
            &cwd,
            &[command, flag],
            &[("PATH", &path), ("TEAM_AGENT_CLI_ARGV_ROUTING", "on")],
        );
        let help = text(&out);
        assert_eq!(
            snapshot(env.root()),
            before,
            "H5 help wrote files or invoked native: {command} {flag}: {help}"
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "H4 help exit: {command}: {help}"
        );
        no_jargon(&help);
        assert!(
            !help.trim().is_empty(),
            "H4/H5 empty launcher help: {command} {flag}"
        );
        let sections =
            Regex::new(r"(?m)^\s*(?:怎么用|关键参数|参数|选项|示例|Examples|下一步)[：:]").unwrap();
        let description = sections
            .find(&help)
            .map(|m| &help[..m.start()])
            .unwrap_or(&help);
        assert!(
            description
                .chars()
                .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
                .count()
                >= 4,
            "H4 plain Chinese description missing before Args/Examples: {help}"
        );
        assert!(
            help.contains("怎么用")
                || help.contains("关键参数")
                || help.contains("参数：")
                || help.contains("参数:")
                || help.contains("选项"),
            "H4 key Args/prerequisites explanation missing: {help}"
        );
        assert!(
            help.contains(command) && (help.contains("用法") || help.contains("usage:")),
            "H4 actual usage missing: {help}"
        );
        assert!(
            help.contains("下一步") || help.to_lowercase().contains("next action"),
            "H4 Next Action missing: {help}"
        );
        let examples = examples(&help, command);
        assert!(
            (2..=3).contains(&examples.len()),
            "H4 needs 2–3 concrete examples for {command}, got {}:\n{help}",
            examples.len()
        );
        assert!(
            help.lines().count() <= if command == "quick-start" { 80 } else { 30 },
            "H4 help budget: {help}"
        );
        let references = Regex::new(r"team-agent\s+([a-z][a-z-]*)").unwrap();
        for reference in references.captures_iter(&help) {
            assert!(
                !MACHINE.contains(&&reference[1]) && !RETIRED.contains(&&reference[1]),
                "H1 private or retired command tutorial leaked: {help}"
            );
        }
    }
}
fn examples_property(command: &str) {
    let env = HermeticTestEnv::enter(command);
    let cwd = env.workspace("examples");
    let path = canary_path(&env);
    fs::create_dir(cwd.join("agents")).unwrap();
    fs::write(cwd.join("agents/reviewer.md"), "---\nname: reviewer\nrole: assistant\nprovider: pi\nmodel: openai-codex/gpt-6-luna\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\nReview changes.\n").unwrap();
    let help = text(&env.run_cli(&cwd, &[command, "--help"]));
    let examples = examples(&help, command);
    assert!(
        (2..=3).contains(&examples.len()),
        "H4 examples missing for {command}:\n{help}"
    );
    let functional = examples
        .iter()
        .filter(|argv| {
            !argv
                .iter()
                .skip(1)
                .take_while(|arg| arg.as_str() != "--")
                .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
        })
        .count();
    let launcher = matches!(
        command,
        "pi" | "codex" | "claude" | "copilot" | "grok" | "cursor"
    );
    let minimum = if launcher && command != "pi" { 1 } else { 2 };
    assert!(
        functional >= minimum,
        "H4 too few real non-help examples for {command}: {help}"
    );
    if launcher {
        assert!(
            examples
                .iter()
                .any(|argv| argv.len() == 1 && argv[0] == command),
            "H4 launcher must include its actual bare startup example: {help}"
        );
    }
    for mut argv in examples {
        for word in &mut argv {
            *word = word
                .replace("$SKILL_DIR", cwd.to_str().unwrap())
                .replace("${SKILL_DIR}", cwd.to_str().unwrap())
                .replace("$PANE", "%999999")
                .replace("${PANE}", "%999999");
        }
        // Execute the printed argv WITHOUT adding --help; missing fixture resources may
        // safely refuse with exit1, but a parser/required-argument refusal (2) is not an example.
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let out = env.run_cli_env(
            &cwd,
            &refs,
            &[("PATH", &path), ("TEAM_AGENT_CLI_ARGV_ROUTING", "off")],
        );
        assert_ne!(
            out.status.code(),
            Some(2),
            "H4 advertised example rejected by actual parser: {argv:?}: {}",
            text(&out)
        );
        assert!(
            !text(&out).contains("invalid choice") && !text(&out).contains("unknown option"),
            "H4 invalid example: {argv:?}: {}",
            text(&out)
        );
    }
}

#[test]
fn h1_public_catalog_is_exactly_twenty_nine_human_records_and_no_machine_record() {
    let actual = catalog::COMMAND_SPECS
        .iter()
        .map(|s| s.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        catalog::COMMAND_SPECS.len(),
        29,
        "H1 physically remove Machine records, not only change visibility/tier"
    );
    assert_eq!(actual, HUMAN.iter().copied().collect());
}
#[test]
#[serial(env)]
fn h1_root_discovers_all_twenty_nine_once_in_catalog_and_zero_private_names() {
    let env = HermeticTestEnv::enter("root-human");
    let cwd = env.workspace("empty");
    let out = env.run_cli(&cwd, &["--help"]);
    let help = text(&out);
    assert_eq!(out.status.code(), Some(0));
    let visible = help
        .lines()
        .filter_map(|line| {
            let name = line.trim().split_whitespace().next()?;
            (HUMAN.contains(&name) || MACHINE.contains(&name) || RETIRED.contains(&name))
                .then_some(name)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        visible.len(),
        29,
        "H1 root command list: {visible:?}\n{help}"
    );
    assert_eq!(
        visible.into_iter().collect::<BTreeSet<_>>(),
        HUMAN.iter().copied().collect()
    );
    let tokens = words(&help);
    let leaks = MACHINE
        .iter()
        .chain(RETIRED)
        .filter(|name| tokens.contains(**name))
        .collect::<Vec<_>>();
    assert!(
        leaks.is_empty(),
        "H1 root private command leaks: {leaks:?}\n{help}"
    );
    for step in [
        "quick-start",
        "send worker",
        "inbox leader",
        "doctor",
        "shutdown",
        "--workspace",
        "--team",
    ] {
        assert!(
            help.contains(step),
            "H1 four-step root journey missing {step}: {help}"
        );
    }
    no_jargon(&help);
}
#[test]
#[serial(env)]
fn h1_unknown_suggestions_and_full_user_index_never_offer_machine_commands() {
    let env = HermeticTestEnv::enter("suggestions");
    let cwd = env.workspace("empty");
    for typo in [
        "resultz",
        "wair",
        "identit",
        "watc",
        "sessionz",
        "validat",
        "prefligh",
        "wait-readz",
        "e2x",
        "pee",
        "coordinato",
        "attach-app-server-leade",
    ] {
        let out = env.run_cli(&cwd, &[typo]);
        let help = text(&out);
        let tokens = words(&help);
        assert_eq!(
            out.status.code(),
            Some(1),
            "unknown command legacy exit preserved: {help}"
        );
        let leaks = MACHINE
            .iter()
            .chain(RETIRED)
            .filter(|name| tokens.contains(**name))
            .collect::<Vec<_>>();
        assert!(
            leaks.is_empty(),
            "H1 private suggestion/index leaks for {typo}: {leaks:?}\n{help}"
        );
    }
}
#[test]
#[serial(env)]
fn h3_all_actual_help_outputs_are_free_of_author_jargon() {
    let env = HermeticTestEnv::enter("jargon-all");
    let cwd = env.workspace("empty");
    let mut all = text(&env.run_cli(&cwd, &["--help"]));
    for command in HUMAN {
        all.push_str(&text(&env.run_cli(&cwd, &[command, "--help"])));
    }
    no_jargon(&all);
}
fn hidden_help(command: &str) {
    let env = HermeticTestEnv::enter(command);
    let cwd = env.workspace("hidden");
    let path = canary_path(&env);
    let before = snapshot(env.root());
    for flag in ["-h", "--help"] {
        let out = env.run_cli_env(&cwd, &[command, flag], &[("PATH", &path)]);
        let help = text(&out);
        assert_eq!(
            snapshot(env.root()),
            before,
            "H2 hidden help executed business/wrote files: {help}"
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "H2 hidden help must be generic Usage exit2: {command} {flag}: {help}"
        );
        assert!(
            help.contains("team-agent --help"),
            "H2 no generic Human navigation: {help}"
        );
        assert!(
            !words(&help).contains(command),
            "H2 private name leaked: {help}"
        );
        for flag in [
            "--task",
            "--case",
            "--socket",
            "--thread-id",
            "--providers",
            "--allow-raw-screen",
            "--once",
            "--workspace",
            "--team",
            "--json",
        ] {
            assert!(
                !help.contains(flag),
                "H2 private usage/flags leaked {flag}: {help}"
            );
        }
    }
}
macro_rules! human_properties {
    ($($id:ident => $command:literal),+ $(,)?) => {$ (
        mod $id {
            use super::*;
            #[test] #[serial(env)] fn help_structure_and_pure_on_bad_route() { help_property($command); }
            #[test] #[serial(env)] fn printed_examples_reach_actual_parser_without_help_shortcut() { examples_property($command); }
        }
    )+};
}
human_properties! {
    quick_start => "quick-start", send => "send", status => "status", models => "models",
    restart => "restart", shutdown => "shutdown", add_agent => "add-agent", start_agent => "start-agent",
    stop_agent => "stop-agent", reset_agent => "reset-agent", clone_agent => "clone-agent", fork_agent => "fork-agent",
    remove_agent => "remove-agent", doctor => "doctor", approvals => "approvals", leaders => "leaders",
    route => "route", profile => "profile", install_skill => "install-skill",
    claim_leader => "claim-leader", takeover => "takeover", attach_leader => "attach-leader",
    pi => "pi", codex => "codex", claude => "claude", copilot => "copilot", grok => "grok", cursor => "cursor", inbox => "inbox",
}
macro_rules! hidden_properties {
    ($($id:ident => $command:literal),+ $(,)?) => {$ (
        #[test] #[serial(env)] fn $id() { hidden_help($command); }
    )+};
}
hidden_properties! {
    h2_wait => "wait", h2_attach_app_server_leader => "attach-app-server-leader", h2_coordinator => "coordinator",
}

// Retired commands reject all argv forms even with a valid legacy spec and
// unreadable runtime state, without workspace/native-tool side effects.
fn retired_unknown(command: &str) {
    let env = HermeticTestEnv::enter(command);
    let cwd = env.workspace("retired");
    let path = canary_path(&env);
    fs::write(
        cwd.join("TEAM.md"),
        "---\nname: retired-demo\nobjective: Retired command fixture.\nprovider: pi\nmodel: openai-codex/gpt-6-luna\n---\nFixture team.\n",
    )
    .unwrap();
    fs::create_dir(cwd.join("agents")).unwrap();
    fs::write(cwd.join("agents/worker.md"), "---\nname: worker\nrole: assistant\nprovider: pi\nmodel: openai-codex/gpt-6-luna\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\nReply to leader.\n").unwrap();
    let compiled = team_agent::compiler::compile_team(&cwd).expect("valid retired-command fixture");
    team_agent::model::spec::validate_spec(&compiled, &cwd).expect("valid legacy spec");
    fs::write(
        cwd.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&compiled),
    )
    .unwrap();
    fs::create_dir_all(cwd.join(".team/runtime")).unwrap();
    fs::write(cwd.join(".team/runtime/state.json"), "{unreadable state").unwrap();
    let ws = cwd.to_str().unwrap().to_string();
    let spec = cwd.join("team.spec.yaml").to_str().unwrap().to_string();
    let mut forms: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--json"],
        vec!["--help"],
        vec!["-h"],
        vec!["--workspace", ws.as_str(), "--team", "t", "--json"],
    ];
    forms.push(match command {
        "e2e" => vec!["--providers", "fake", "--real"],
        "allow-peer-talk" => vec!["alpha", "bravo", "--team", "t"],
        "results" => vec!["--case", "c", "--team", "t"],
        "validate" | "preflight" => vec![ws.as_str()],
        "peek" => vec![
            "worker", "--allow-raw-screen", "--head", "2", "--tail", "5", "--search", "needle",
        ],
        "wait-ready" => vec!["--timeout", "0"],
        _ => vec!["--team", "t"],
    });
    if command == "validate" {
        forms.push(vec![spec.as_str()]);
    }
    let before = snapshot(env.root());
    for form in forms {
        let mut argv = vec![command];
        argv.extend(form);
        let out = env.run_cli_env(&cwd, &argv, &[("PATH", &path)]);
        let output = text(&out);
        assert_eq!(out.status.code(), Some(1), "retired {argv:?}: {output}");
        assert!(
            out.stdout.is_empty(),
            "retired {argv:?} printed output: {output}"
        );
        assert!(
            output.contains(&format!("没有这个操作：'{command}'")),
            "retired {argv:?} must use the generic unknown-command refusal: {output}"
        );
        assert!(output.contains("team-agent --help"), "{output}");
        assert!(
            !output.contains(&format!("team-agent {command}")),
            "retired usage leaked: {output}"
        );
        let tokens = words(&output);
        let leaks = MACHINE
            .iter()
            .chain(RETIRED.iter().filter(|name| **name != command))
            .filter(|name| tokens.contains(**name))
            .collect::<Vec<_>>();
        assert!(
            leaks.is_empty(),
            "retired {argv:?} suggested {leaks:?}: {output}"
        );
        assert_eq!(
            snapshot(env.root()),
            before,
            "retired {argv:?} changed files or invoked a native tool: {output}"
        );
    }
    let typo = format!("{command}x");
    let out = env.run_cli_env(&cwd, &[&typo], &[("PATH", &path)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!text(&out).contains(&format!("team-agent {command}")));
    assert_eq!(snapshot(env.root()), before, "retired typo changed fixture");
}

macro_rules! retired_properties {
    ($($id:ident => $command:literal),+ $(,)?) => {$ (
        #[test] #[serial(env)] fn $id() { retired_unknown($command); }
    )+};
}
retired_properties! {
    r289_e2e => "e2e", r289_allow_peer_talk => "allow-peer-talk", r289_results => "results",
    r289_validate => "validate", r289_identity => "identity", r289_sessions => "sessions",
    r289_watch => "watch", r289_peek => "peek", r289_wait_ready => "wait-ready", r289_preflight => "preflight",
}

#[test]
fn r289_retired_names_have_no_catalog_record() {
    for command in RETIRED {
        assert!(catalog::COMMAND_SPECS.iter().all(|spec| spec.name != *command));
    }
}
