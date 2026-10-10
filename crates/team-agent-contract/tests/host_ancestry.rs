//! Real OS ancestry, without Kiro, tmux, subscriptions or global environment edits.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use team_agent_contract::host::process::{
    capture_process, fingerprint_file, sample_process, verify_native_ancestry, ImageStamp,
    ProcessStamp, ProcessState,
};

// Both tests capture this executable. Hard-link creation can change macOS's
// reported vnode pathname, so do not race it with the other test's capture.
static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ShellTree(Child);
impl Drop for ShellTree {
    fn drop(&mut self) {
        // Only this test's pipe/tree; the leaf exits and each helper waits/reaps.
        if let Some(mut stdin) = self.0.stdin.take() {
            let _ = stdin.write_all(b"finish\n");
        }
        let _ = self.0.wait();
    }
}

#[test]
fn live_process_hard_link_alias_is_not_replacement_but_changed_identity_is() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    let budget = Duration::from_secs(10);
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let hash = fingerprint_file(&executable, 1024 * 1024 * 1024, budget).unwrap();
    let original = capture_process(std::process::id(), &executable, hash, budget).unwrap();
    // Sibling of the test executable: hard links must be on the same filesystem.
    // Exclusive creation grants cleanup only of this test's directory.
    let root = executable.parent().unwrap().join(format!(
        ".tac-process-alias-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let alias = root.join("captured-alias");
    std::fs::hard_link(&executable, &alias).unwrap();
    let mut aliased = original.clone();
    aliased.identity.executable = alias;
    // The kernel may choose either spelling; both name the same captured inode.
    assert_eq!(sample_process(&original), ProcessState::Alive);
    assert_eq!(sample_process(&aliased), ProcessState::Alive);

    let copy = root.join("same-bytes-different-inode");
    std::fs::copy(&executable, &copy).unwrap();
    assert_eq!(fingerprint_file(&copy, 1024 * 1024 * 1024, budget).unwrap(), hash);
    let metadata = std::fs::metadata(&copy).unwrap();
    let mut copied = original.clone();
    copied.identity.executable = copy;
    copied.image = ImageStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
            + i128::from(metadata.mtime_nsec()),
    };
    assert_ne!(copied.image.inode, original.image.inode);
    assert_eq!(sample_process(&copied), ProcessState::Replaced);

    let changes: [fn(&mut ProcessStamp); 6] = [
        |stamp| stamp.parent ^= 1,
        |stamp| stamp.identity.birth_identity.push_str("-reused"),
        |stamp| stamp.image.device ^= 1,
        |stamp| stamp.image.inode ^= 1,
        |stamp| stamp.image.length ^= 1,
        |stamp| stamp.image.modified_ns += 1,
    ];
    for change in changes {
        let mut changed = aliased.clone();
        change(&mut changed);
        assert_eq!(sample_process(&changed), ProcessState::Replaced);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_direct_and_two_helper_children_pass_but_deeper_or_replaced_subjects_do_not() {
    let _serial = PROCESS_TEST_LOCK.lock().unwrap();
    let budget = Duration::from_secs(10);
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let hash = fingerprint_file(&executable, 1024 * 1024 * 1024, budget).unwrap();
    let native = capture_process(std::process::id(), &executable, hash, budget).unwrap();
    let shell = std::path::Path::new("/bin/sh").canonicalize().unwrap();
    let shell_hash = fingerprint_file(&shell, 1024 * 1024 * 1024, budget).unwrap();
    for helpers in 0..=3 {
        let mut script = "printf '%s\\n' \"$$\"; IFS= read -r finish".to_string();
        for _ in 0..helpers {
            // A following ':' prevents a shell from tail-execing its child.
            script = format!("/bin/sh -c '{}'; :", script.replace('\'', "'\\''"));
        }
        let mut tree = ShellTree(
            Command::new(&shell)
                .args(["-c", &script])
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        BufReader::new(tree.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid = line.trim().parse().unwrap();
        let process = capture_process(pid, &shell, shell_hash, budget).unwrap();
        let result = verify_native_ancestry(&process, &native);
        if helpers <= 2 {
            let chain = result.unwrap();
            assert_eq!(chain.len(), helpers + 1);
            assert_eq!(chain.first(), Some(&process.parent));
            assert_eq!(chain.last(), Some(&std::process::id()));
            let mut replaced = native.clone();
            replaced.identity.birth_identity.push_str("-reused");
            assert!(verify_native_ancestry(&process, &replaced).is_err());
            let mut replaced = process.clone();
            replaced.identity.birth_identity.push_str("-reused");
            assert!(verify_native_ancestry(&replaced, &native).is_err());
        } else {
            assert!(result.is_err());
        }
    }
}
