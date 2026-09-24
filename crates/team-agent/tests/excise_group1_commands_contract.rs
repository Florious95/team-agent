//! Process-level contract for the complete removal of the first eight legacy CLI commands.
//! Every invocation must be rejected by the normal unknown-command parser before side effects.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Child;

use hermetic_guard::HermeticTestEnv;
use serial_test::serial;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotMetadata {
    readonly: bool,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    mode: u32,
}

fn snapshot_metadata(metadata: &fs::Metadata) -> SnapshotMetadata {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    SnapshotMetadata {
        readonly: metadata.permissions().readonly(),
        modified: metadata
            .modified()
            .unwrap_or_else(|error| panic!("read modification time: {error}")),
        #[cfg(unix)]
        mode: metadata.permissions().mode(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    Directory(SnapshotMetadata),
    File {
        bytes: Vec<u8>,
        metadata: SnapshotMetadata,
    },
    Symlink {
        target: PathBuf,
        metadata: SnapshotMetadata,
    },
    Other(SnapshotMetadata),
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let metadata = fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()));
        let relative = path
            .strip_prefix(root)
            .unwrap_or_else(|error| panic!("{} outside {}: {error}", path.display(), root.display()))
            .to_path_buf();
        let file_type = metadata.file_type();
        let snapshot_metadata = snapshot_metadata(&metadata);
        let entry = if file_type.is_symlink() {
            SnapshotEntry::Symlink {
                target: fs::read_link(path)
                    .unwrap_or_else(|error| panic!("read symlink {}: {error}", path.display())),
                metadata: snapshot_metadata,
            }
        } else if file_type.is_dir() {
            SnapshotEntry::Directory(snapshot_metadata)
        } else if file_type.is_file() {
            SnapshotEntry::File {
                bytes: fs::read(path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
                metadata: snapshot_metadata,
            }
        } else {
            SnapshotEntry::Other(snapshot_metadata)
        };
        entries.insert(relative, entry);
        if file_type.is_dir() {
            let children = fs::read_dir(path)
                .unwrap_or_else(|error| panic!("read directory {}: {error}", path.display()))
                .map(|entry| entry.expect("read directory entry").path())
                .collect::<Vec<_>>();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn changed_paths(
    before: &BTreeMap<PathBuf, SnapshotEntry>,
    after: &BTreeMap<PathBuf, SnapshotEntry>,
) -> Vec<PathBuf> {
    let paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect()
}

fn invocation_variants(command: &str, workspace: &Path) -> Vec<(String, Vec<String>)> {
    let workspace = workspace.to_string_lossy().into_owned();
    let original_args = match command {
        "diagnose" | "init" | "stop" | "stuck-list" => {
            vec!["--workspace".to_string(), workspace.clone()]
        }
        "start" => Vec::new(),
        "restart-agent" => vec![
            "worker-a".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
        ],
        "stuck-cancel" => vec![
            "worker-a".to_string(),
            "--workspace".to_string(),
            workspace.clone(),
        ],
        "acknowledge-idle" => vec![
            "--workspace".to_string(),
            workspace,
            "--team".to_string(),
            "team-a".to_string(),
        ],
        _ => unreachable!("unlisted command {command}"),
    };

    let mut variants = Vec::new();
    for (label, suffix) in [
        ("bare", None),
        ("json", Some("--json")),
        ("help", Some("--help")),
        ("short-help", Some("-h")),
    ] {
        let mut argv = vec![command.to_string()];
        if let Some(suffix) = suffix {
            argv.push(suffix.to_string());
        }
        variants.push((label.to_string(), argv));
    }
    if !original_args.is_empty() {
        for (label, suffix) in [
            ("original-args", None),
            ("original-args-json", Some("--json")),
            ("original-args-help", Some("--help")),
            ("original-args-short-help", Some("-h")),
        ] {
            let mut argv = vec![command.to_string()];
            argv.extend(original_args.iter().cloned());
            if let Some(suffix) = suffix {
                argv.push(suffix.to_string());
            }
            variants.push((label.to_string(), argv));
        }
    }
    variants
}

#[cfg(unix)]
struct ProcessCanary(Child);

#[cfg(unix)]
impl ProcessCanary {
    fn spawn(workspace: &Path) -> Self {
        Self(hermetic_guard::spawn_owned_coordinator(workspace))
    }

    fn alive(&mut self) -> Result<(), String> {
        match self.0.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!("canary exited unexpectedly: {status}")),
            Err(error) => Err(format!("cannot inspect canary: {error}")),
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessCanary {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn assert_removed_command_contract(command: &str) {
    let env = HermeticTestEnv::enter(&format!("excise-{command}"));
    let temp_root = env.root().to_str().expect("hermetic root is UTF-8");
    let variant_count = invocation_variants(command, env.root()).len();
    let mut failures = Vec::new();

    for index in 0..variant_count {
        let workspace = env.workspace(&format!("{command}-{index}"));
        #[cfg(unix)]
        let mut canary = ProcessCanary::spawn(&workspace);
        fs::write(workspace.join("keep.txt"), b"untouched\n").expect("write sentinel");
        fs::create_dir(workspace.join("nested")).expect("create sentinel directory");
        fs::write(workspace.join("nested/state.json"), b"{\"preserve\":true}\n")
            .expect("write nested sentinel");
        assert!(!workspace.join(".team").exists());

        let variants = invocation_variants(command, &workspace);
        let (label, argv) = &variants[index];
        let argv_refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let before = snapshot(env.root());
        let output = env.run_cli_env(
            &workspace,
            &argv_refs,
            &[("TMPDIR", temp_root), ("TEAM_AGENT_TEST_TMP", temp_root)],
        );
        let after = snapshot(env.root());
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut violations = Vec::new();
        if output.status.code() != Some(1) {
            violations.push(format!("exit={:?}, expected 1", output.status.code()));
        }
        let expected_error = format!("invalid choice: '{command}'");
        if !stderr.contains(&expected_error) {
            violations.push(format!("stderr lacks {expected_error:?}: {}", preview(&output.stderr)));
        }
        if !output.stdout.is_empty() {
            violations.push(format!("stdout must be empty: {}", preview(&output.stdout)));
        }
        if before != after {
            violations.push(format!("workspace/home snapshot changed: {:?}", changed_paths(&before, &after)));
        }
        if workspace.join(".team").exists() {
            violations.push("created .team in a workspace that had none".to_string());
        }
        #[cfg(unix)]
        if let Err(error) = canary.alive() {
            violations.push(error);
        }
        if !violations.is_empty() {
            failures.push(format!("{label} argv={argv:?}: {}", violations.join("; ")));
        }
    }

    assert!(
        failures.is_empty(),
        "removed command {command:?} violated its rejection/zero-side-effect contract:\n{}",
        failures.join("\n")
    );
}

fn preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let preview = text.chars().take(320).collect::<String>();
    if text.chars().count() > 320 {
        format!("{preview}…")
    } else {
        preview
    }
}

macro_rules! removed_command_test {
    ($name:ident, $command:literal) => {
        #[test]
        #[serial(env)]
        fn $name() {
            assert_removed_command_contract($command);
        }
    };
}

removed_command_test!(diagnose_is_rejected_without_side_effects, "diagnose");
removed_command_test!(init_is_rejected_without_side_effects, "init");
removed_command_test!(start_is_rejected_without_side_effects, "start");
removed_command_test!(stop_is_rejected_without_side_effects, "stop");
removed_command_test!(restart_agent_is_rejected_without_side_effects, "restart-agent");
removed_command_test!(stuck_list_is_rejected_without_side_effects, "stuck-list");
removed_command_test!(stuck_cancel_is_rejected_without_side_effects, "stuck-cancel");
removed_command_test!(acknowledge_idle_is_rejected_without_side_effects, "acknowledge-idle");

#[test]
#[serial(env)]
fn canonical_doctor_shutdown_and_reset_agent_remain_registered() {
    let env = HermeticTestEnv::enter("excise-canonical-commands");
    let workspace = env.workspace("canonical");
    for command in ["doctor", "shutdown", "reset-agent"] {
        let output = env.run_cli(&workspace, &[command, "--help"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "canonical command {command} --help must remain available: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(&format!("invalid choice: '{command}'")),
            "canonical command {command} was treated as unknown"
        );
    }
    for args in [
        vec!["doctor", "--workspace", workspace.to_str().unwrap(), "--json"],
        vec!["shutdown", "--workspace", workspace.to_str().unwrap(), "--json"],
        vec![
            "reset-agent",
            "worker-a",
            "--workspace",
            workspace.to_str().unwrap(),
            "--json",
        ],
    ] {
        let command = args[0];
        let output = env.run_cli(&workspace, &args);
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(&format!("invalid choice: '{command}'")),
            "canonical command {command} invocation must reach its handler, not unknown-command rejection: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[serial(env)]
fn canonical_doctor_reads_a_workspace_and_not_an_unknown_command() {
    let env = HermeticTestEnv::enter("excise-doctor-preserved");
    let workspace = env.workspace("doctor");
    let output = env.run_cli(&workspace, &["doctor", "--workspace", workspace.to_str().unwrap(), "--json"]);
    assert!(
        output.status.code().is_some(),
        "doctor must complete with a process exit status"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("invalid choice: 'doctor'"),
        "doctor must remain a recognized command"
    );
}

#[test]
#[serial(env)]
fn profile_init_remains_a_separate_supported_command() {
    let env = HermeticTestEnv::enter("excise-profile-init-preserved");
    let workspace = env.workspace("profile-init");
    let output = env.run_cli(&workspace, &["profile", "init", "--help"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "profile init remains supported: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
