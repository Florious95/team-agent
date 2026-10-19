//! Public lifecycle guidance follows the intuitive surface without changing bridges.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const LEGACY: &[&str] = &[
    "quick-start", "shutdown", "add-agent", "start-agent", "stop-agent", "remove-agent",
    "reset-agent", "clone-agent", "fork-agent",
];
const REFERENCES: &[&str] = &[
    "references/team-agent-operator.md", "references/team-in-team.md",
    "references/recovery-runbook.md", "references/bug-as-artifact-flow.md",
];
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}
fn skill() -> PathBuf { repo().join("skills/team-agent") }
fn read(path: &Path) -> String { fs::read_to_string(path).unwrap() }
struct Case { root: PathBuf }
impl Case {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("ta-phase2-docs-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        let root = path.canonicalize().unwrap();
        fs::create_dir(root.join("home")).unwrap();
        Self { root }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args).current_dir(&self.root).env("HOME", self.root.join("home"))
            .env("TEAM_AGENT_CLI_ARGV_ROUTING", "off").output().unwrap()
    }
}
impl Drop for Case {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}
fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { collect(root, &path, out); }
            else { out.insert(path.strip_prefix(root).unwrap().to_owned(), fs::read(path).unwrap()); }
        }
    }
    let mut out = BTreeMap::new(); collect(root, root, &mut out); out
}

#[test]
fn actual_help_keeps_new_and_legacy_commands_discoverable_without_effects() {
    let case = Case::new(); let before = inventory(&case.root);
    let root = case.run(&["--help"]); assert!(root.status.success());
    let root = String::from_utf8(root.stdout).unwrap();
    for command in ["start", "stop", "restart", "team", "agent"].into_iter().chain(LEGACY.iter().copied()) {
        assert!(root.lines().any(|line| line.split_whitespace().next() == Some(command)), "missing {command}: {root}");
        let output = case.run(&[command, "--help"]);
        assert!(output.status.success(), "{command}: {output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        for section in ["Purpose:", "Usage:", "Options:", "Examples:", "Next Action:"] {
            assert!(help.contains(section), "{command} missing {section}");
        }
    }
    assert_eq!(inventory(&case.root), before);
}

#[test]
fn every_legacy_lifecycle_help_redirects_only_to_current_daily_forms() {
    for (legacy, preferred) in [
        ("quick-start", "team-agent start"), ("shutdown", "team-agent stop"),
        ("add-agent", "team-agent start"), ("start-agent", "team-agent start"),
        ("stop-agent", "team-agent stop"), ("remove-agent", "team-agent agent remove"),
        ("reset-agent", "team-agent agent reset"), ("clone-agent", "team-agent agent clone"),
        ("fork-agent", "team-agent agent fork"),
    ] {
        let help = team_agent::cli::emit::__test_command_help(Some(legacy));
        assert!(help.contains("Compatibility"), "unmarked bridge: {legacy}");
        let next = help.split_once("Next Action:\n").unwrap().1;
        assert!(next.contains(preferred), "{legacy}: {next}");
        for old in LEGACY {
            assert!(!next.contains(&format!("team-agent {old}")), "old next action: {legacy}: {next}");
        }
    }
}

#[test]
fn restart_help_states_the_two_defaults_and_context_loss_boundary() {
    let help = team_agent::cli::emit::__test_command_help(Some("restart"));
    for phrase in ["default to --session auto", "keeps preserve", "--dry-run", "not guaranteed saved memory", "explicitly stopped/paused", "partial/unknown"] {
        assert!(help.contains(phrase), "missing {phrase}: {help}");
    }
}

#[test]
fn current_public_guides_have_no_legacy_shell_recipe() {
    let mut paths = vec![skill().join("SKILL.md"), repo().join("README.md"), repo().join("README.zh.md")];
    paths.extend(REFERENCES.iter().map(|name| skill().join(name)));
    for path in paths {
        let text = read(&path); let mut shell = false;
        for line in text.lines() {
            if let Some(language) = line.trim().strip_prefix("```") {
                shell = matches!(language, "sh" | "bash" | "shell"); continue;
            }
            if shell {
                let mut words = line.split_whitespace();
                if words.next() == Some("team-agent") {
                    let verb = words.next().unwrap_or("");
                    assert!(!LEGACY.contains(&verb), "obsolete recipe in {}: {line}", path.display());
                }
            }
        }
    }
}

#[test]
fn skill_and_references_preserve_choices_scope_and_recovery_guards() {
    let core = read(&skill().join("SKILL.md"));
    for phrase in ["name: team-agent", "team-agent start --from", "team-agent stop", "team-agent team restart", "--session preserve", "provider", "bypass", "in-team short name", "<workspace>::<team>/<agent>", "report_result", "Compatibility bridges", "version string"] {
        assert!(core.contains(phrase), "missing {phrase}");
    }
    let recovery = read(&skill().join("references/recovery-runbook.md"));
    for phrase in ["--session preserve", "payload handoff", "L1", "L2", "L3", "residuals.owned_files", "recovery.context_reset"] {
        assert!(recovery.contains(phrase), "missing {phrase}");
    }
    let nested = read(&skill().join("references/team-in-team.md"));
    for phrase in ["--workspace \"$CHILD_WORKSPACE\"", "--team \"$CHILD_TEAM\"", "parent", "do not spoof", "two-hop"] {
        assert!(nested.contains(phrase), "missing {phrase}");
    }
}

#[test]
fn local_skill_links_resolve_inside_the_packaged_single_source() {
    let mut paths = vec![skill().join("SKILL.md")];
    paths.extend(REFERENCES.iter().map(|name| skill().join(name)));
    for path in paths {
        for segment in read(&path).split("](").skip(1) {
            let link = segment.split(')').next().unwrap().split('#').next().unwrap();
            if link.is_empty() || link.starts_with("https://") { continue; }
            let target = path.parent().unwrap().join(link).canonicalize().unwrap();
            assert!(target.starts_with(skill()) && target.is_file(), "bad link: {link}");
        }
    }
    let package: Value = serde_json::from_str(&read(&repo().join("package.json"))).unwrap();
    assert!(package["files"].as_array().unwrap().iter().any(|value| value == "skills"));
    assert!(read(&repo().join("npm/install.mjs")).contains("[\"install-skill\", \"--target\", \"all\", \"--source\", source, \"--json\"]"));
}

#[test]
fn actual_install_skill_copies_core_and_all_references_to_private_home() {
    let case = Case::new(); let source = skill();
    let result = case.run(&["install-skill", "--target", "all", "--source", source.to_str().unwrap(), "--json"]);
    assert!(result.status.success(), "{result:?}");
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    let installed = value["installed"].as_array().unwrap(); assert_eq!(installed.len(), 3);
    let mut files = vec!["SKILL.md"]; files.extend_from_slice(REFERENCES);
    for target in installed {
        let dest = Path::new(target["dest"].as_str().unwrap());
        assert!(dest.starts_with(case.root.join("home")), "not isolated: {target}");
        assert_eq!(target["dry_run"], false);
        for file in &files { assert_eq!(fs::read(dest.join(file)).unwrap(), fs::read(source.join(file)).unwrap(), "copy mismatch: {file}"); }
    }
}

#[test]
fn documented_creation_forms_are_readonly_plans_not_native_spawns() {
    let case = Case::new(); let source = case.root.join(".team/current");
    fs::create_dir_all(source.join("agents")).unwrap();
    fs::write(source.join("TEAM.md"), "---\nname: project-team\n---\nBounded documentation example.\n").unwrap();
    fs::write(source.join("agents/reviewer.md"), "---\nprovider: pi\ndangerously_skip_permissions: false\n---\nReview the change.\n").unwrap();
    let before = inventory(&case.root);
    for args in [
        vec!["start", "--from", ".team/current", "--workspace", ".", "--team", "project-team", "--dry-run", "--json"],
        vec!["team", "start", ".team/current", "--workspace", ".", "--team", "project-team", "--dry-run", "--json"],
    ] {
        let result = case.run(&args); assert!(result.status.success(), "{result:?}");
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["dry_run"], true); assert_eq!(value["scope"]["team"], "project-team");
        assert_eq!(value["steps"][0]["action"], "quick_start");
        assert_eq!(inventory(&case.root), before);
    }
}
