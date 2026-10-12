//! Real OS ancestry, without Kiro, tmux, subscriptions or global environment edits.
#![cfg(any(target_os = "linux", target_os = "macos"))]
mod support;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use team_agent_contract::host::process::{
    capture_process, fingerprint_file, sample_process, verify_native_ancestry, ImageStamp,
    ProcessStamp, ProcessState,
};

// Both tests capture this executable. Hard-link creation can change macOS's
// reported vnode pathname, so do not race it with the other test's capture.
static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ProcessTree(Child);
impl ProcessTree {
    fn finish(&mut self) -> std::io::Result<ExitStatus> {
        // Only this test's pipe/tree; the leaf exits and each helper waits/reaps.
        if let Some(mut stdin) = self.0.stdin.take() {
            let _ = stdin.write_all(b"finish\n");
        }
        self.0.wait()
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
fn spawn_fixture(executable: &Path, helpers: usize, stdout: Stdio) -> ProcessTree {
    let mut tree = ProcessTree(
        Command::new(executable)
            .args([
                "--exact",
                "ancestry_fixture_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(stdout)
            .spawn()
            .unwrap(),
    );
    writeln!(tree.0.stdin.as_mut().unwrap(), "{helpers}").unwrap();
    tree
}

// Explicit native helper entry, not another functional test or a shell script.
// Depth and shutdown travel over owned stdin; no process argv/env are inspected.
#[test]
#[ignore = "internal owned-process fixture; invoked only by the ancestry test"]
fn ancestry_fixture_child() {
    let mut input = std::io::stdin().lock();
    let mut depth = String::new();
    if input.read_line(&mut depth).unwrap() == 0 {
        return;
    }
    let helpers: usize = depth.trim().parse().unwrap();
    assert!(helpers <= 3);
    if helpers == 0 {
        // Start a separate line even if libtest printed a status prefix.
        println!("\nTAC_ANCESTRY_LEAF={}", std::process::id());
        std::io::stdout().flush().unwrap();
        let mut finish = String::new();
        input.read_line(&mut finish).unwrap();
        assert_eq!(finish, "finish\n");
    } else {
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut child = spawn_fixture(&executable, helpers - 1, Stdio::inherit());
        std::io::copy(&mut input, child.0.stdin.as_mut().unwrap()).unwrap();
        child.0.stdin.take();
        assert!(child.0.wait().unwrap().success());
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let sandbox = support::Sandbox::new();
    let fixture = sandbox.parent.join("ancestry-fixture");
    // A script or /bin/sh launcher need not be its final kernel image. Use a
    // user-owned native program with fixed bytes instead of adopting a sampled
    // image or weakening the production process fence.
    std::fs::copy(&executable, &fixture).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        std::fs::metadata(&fixture).unwrap().uid(),
        std::fs::metadata(&sandbox.parent).unwrap().uid()
    );
    let fixture_hash = fingerprint_file(&fixture, 1024 * 1024 * 1024, budget).unwrap();
    assert_eq!(fixture_hash, hash);
    for helpers in 0..=3 {
        let mut tree = spawn_fixture(&fixture, helpers, Stdio::piped());
        let pid = {
            // Borrow rather than take stdout: keep it open during finish/reap,
            // so helper libtest trailers cannot encounter a closed pipe.
            let mut output = BufReader::new(tree.0.stdout.as_mut().unwrap());
            let mut leaf = None;
            for _ in 0..32 {
                let mut line = String::new();
                assert!(
                    output.read_line(&mut line).unwrap() > 0,
                    "owned helper exited before PID receipt"
                );
                if let Some(pid) = line.trim().strip_prefix("TAC_ANCESTRY_LEAF=") {
                    leaf = Some(pid.parse::<u32>().unwrap());
                    break;
                }
            }
            leaf.expect("owned helper PID receipt missing")
        };
        let process = capture_process(pid, &fixture, fixture_hash, budget).unwrap_or_else(|error| {
            // Diagnostics only, restricted to this owned child. Never use the
            // observed path/hash as replacement authorization.
            #[cfg(target_os = "macos")]
            let observed = i32::try_from(pid)
                .ok()
                .and_then(|pid| libproc::libproc::proc_pid::pidpath(pid).ok());
            #[cfg(target_os = "linux")]
            let observed = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            panic!("owned process capture failed: helpers={helpers}; pid={pid}; expected={fixture:?}; observed={observed:?}; error={error}");
        });
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
        assert!(
            tree.finish().unwrap().success(),
            "owned helper tree must exit and reap cleanly"
        );
    }
}
