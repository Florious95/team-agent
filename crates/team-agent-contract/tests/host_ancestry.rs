//! Real OS ancestry, without Kiro, tmux, subscriptions or global environment edits.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use team_agent_contract::host::process::{capture_process, fingerprint_file, verify_native_ancestry};

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
fn live_direct_and_two_helper_children_pass_but_deeper_or_replaced_subjects_do_not() {
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
