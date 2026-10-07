//! Retired commands are unknown input, not hidden compatibility routes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic;

use hermetic::HermeticTestEnv;
use serial_test::serial;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RETIRED: &[&str] = &[
    "e2e", "allow-peer-talk", "results", "validate", "identity", "sessions",
    "watch", "peek", "wait-ready", "preflight",
];

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            snapshot.insert(path.clone(), Vec::new());
            snapshot.extend(files(&path));
        } else {
            snapshot.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    snapshot
}

#[test]
#[serial(env)]
fn retired_commands_refuse_without_touching_workspace() {
    let env = HermeticTestEnv::enter("cli-excision");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace("retired");
    std::fs::write(workspace.join("canary"), "must remain unchanged").unwrap();
    let before = files(&workspace);
    for command in RETIRED {
        for flags in [vec![], vec!["--json"], vec!["--help"], vec!["-h"]] {
            let mut args = vec![*command];
            args.extend(flags);
            args.extend(["--workspace", workspace.to_str().unwrap(), "--team", "alpha"]);
            let output = env.run_cli(&workspace, &args);
            let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.status.code(), Some(1), "{args:?}: {text}");
            assert!(text.contains("team-agent --help"), "{args:?}: {text}");
            assert!(text.contains("没有这个操作"), "{args:?}: {text}");
            assert!(!text.contains(&format!("team-agent {command}")), "retired usage/suggestion leaked: {text}");
            assert_eq!(files(&workspace), before, "{args:?} changed workspace");
        }
    }
}

#[test]
#[serial(env)]
fn retired_commands_are_absent_from_help_and_typo_suggestions() {
    let env = HermeticTestEnv::enter("cli-excision-help");
    let workspace = env.workspace("help");
    let help = env.run_cli(&workspace, &["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    for command in RETIRED {
        assert!(!help.split_whitespace().any(|word| word == *command));
        let typo = format!("{command}x");
        let output = env.run_cli(&workspace, &[&typo]);
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(!text.contains(&format!("team-agent {command}")), "retired suggestion leaked: {text}");
    }
}
