#![cfg(unix)]
mod support;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::time::Duration;
use support::Sandbox;
use team_agent_contract::host::command::*;
use team_agent_contract::kiro::native::*;
fn executable(path: &Path) {
    std::fs::write(path, b"test executable bytes; never run").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn failed(end: CommandEnd, code: Option<i32>, stderr: &str) -> CommandReceipt {
    CommandReceipt {
        end,
        exit_code: code,
        child_pid: Some(42),
        child_reaped: true,
        stdout: vec![],
        stderr: stderr.as_bytes().to_vec(),
        elapsed: Duration::from_millis(1),
    }
}

struct VersionOnly;
impl CommandRunner for VersionOnly {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        assert_eq!(request.arguments, vec![std::ffi::OsString::from("--version")]);
        assert_eq!(request.executable.file_name().unwrap(), "kiro-cli");
        let mut receipt = failed(CommandEnd::Exited, Some(0), "");
        receipt.stdout = b"kiro-cli 2.28.0\n".to_vec();
        receipt
    }
}
/// The observed macOS layout: a PATH symlink to the bundle's public dispatcher.
fn bundle(sandbox: &Sandbox) -> (std::path::PathBuf, std::path::PathBuf) {
    let bundle = sandbox.parent.join("App Bundle/MacOS");
    std::fs::create_dir_all(&bundle).unwrap();
    executable(&bundle.join("kiro-cli"));
    executable(&bundle.join("kiro-cli-chat"));
    let bin = sandbox.parent.join("bin");
    std::fs::create_dir(&bin).unwrap();
    symlink(bundle.join("kiro-cli"), bin.join("kiro-cli")).unwrap();
    (bin, bundle)
}
#[test]
fn public_kiro_cli_on_path_is_the_entry_and_its_dispatcher_image_is_the_fence() {
    let sandbox = Sandbox::new();
    let (bin, bundle) = bundle(&sandbox);
    let command = PublicCommand::resolve("kiro-cli", bin.as_os_str()).unwrap();
    assert_eq!(command.entry, bin.join("kiro-cli"), "the PATH symlink is kept as found");
    let runtime = probe_runtime(&command, &mut VersionOnly, discovery_bounds()).unwrap();
    assert_eq!(runtime.engine, bundle.join("kiro-cli").canonicalize().unwrap());
    assert_eq!(runtime.native.harness, "v2");
    assert_eq!(runtime.launcher().command, "kiro-cli");
    assert_eq!(runtime.launcher().executable, bin.join("kiro-cli"));
    assert!(std::fs::symlink_metadata(bin.join("kiro-cli")).unwrap().file_type().is_symlink());
}
#[test]
fn user_wrapper_before_kiro_cli_is_kept_and_no_install_directory_is_guessed() {
    let sandbox = Sandbox::new();
    let (bin, bundle) = bundle(&sandbox);
    let wrappers = sandbox.parent.join("wrappers");
    std::fs::create_dir(&wrappers).unwrap();
    std::fs::write(wrappers.join("kiro-cli"), b"#!/bin/sh\nexec kiro-cli \"$@\"\n").unwrap();
    std::fs::set_permissions(wrappers.join("kiro-cli"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let command = PublicCommand::resolve("kiro-cli", &std::env::join_paths([&wrappers, &bin]).unwrap()).unwrap();
    assert_eq!(command.entry, wrappers.join("kiro-cli"));
    let runtime = probe_runtime(&command, &mut VersionOnly, discovery_bounds()).unwrap();
    assert_eq!(runtime.engine, bundle.join("kiro-cli").canonicalize().unwrap());
    assert_eq!(runtime.launcher().executable, wrappers.join("kiro-cli"));
    // Only the user's PATH is searched: a helper in a conventional install
    // directory never substitutes for a missing native `kiro-cli`.
    let local = sandbox.parent.join(".local/bin");
    std::fs::create_dir_all(&local).unwrap();
    executable(&local.join("kiro-cli-chat"));
    let alone = PublicCommand::resolve("kiro-cli", wrappers.as_os_str()).unwrap();
    assert!(probe_runtime(&alone, &mut VersionOnly, discovery_bounds()).is_err());
    assert!(PublicCommand::resolve("kiro-cli", local.as_os_str()).is_err());
}
#[test]
fn nonexecutable_kiro_cli_is_not_claimed_as_available() {
    let sandbox = Sandbox::new();
    let direct = sandbox.parent.join("kiro-cli");
    std::fs::write(&direct, b"not executable").unwrap();
    std::fs::set_permissions(&direct, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(PublicCommand::resolve("kiro-cli", sandbox.parent.as_os_str()).is_err());
}
#[test]
fn launcher_and_engine_banners_parse_release_versions_without_an_update_allowlist() {
    for version in ["0.0.0", "2.28.0", "2.29.0", "2.99.17", "3.0.0"] {
        for prefix in ["kiro-cli", "kiro-cli-chat"] {
            for ending in ["", "\n", "\r\n"] {
                assert_eq!(
                    parse_version(format!("{prefix} {version}{ending}").as_bytes()).unwrap(),
                    version
                );
            }
        }
    }
    for text in [
        b"2.28.0".as_slice(),
        b"other-cli 2.28.0",
        b"kiro-cli-chat-preview 2.28.0",
        b"kiro-cli 2.29",
        b"kiro-cli-chat 2.29.0.1",
        b"kiro-cli-chat 02.29.0",
        b"kiro-cli-chat 2..0",
        b"kiro-cli-chat 2.29.-1",
        b"kiro-cli-chat 2.29.0-beta",
        b"kiro-cli 2.28.0\nextra",
        b"kiro-cli-chat 2.28.0\nextra",
        b"kiro-cli-chat \xff",
    ] {
        assert!(parse_version(text).is_err());
    }
}
#[test]
fn malformed_version_diagnostics_preserve_the_banner_but_escape_and_bound_output() {
    let error = parse_version(b"kiro-cli-chat 2.29.invalid\n\x1b[2J")
        .unwrap_err()
        .to_string();
    assert!(error.contains("kiro-cli-chat 2.29.invalid"));
    assert!(!error.contains('\n'));
    assert!(!error.contains('\x1b'));
    let error = parse_version(&vec![b'x'; 4096]).unwrap_err().to_string();
    assert!(error.contains("truncated to 256 bytes"));
    assert!(error.len() < 512);
}
#[test]
fn launcher_probe_binds_the_observed_release_to_unchanged_entry_bytes() {
    struct VersionRunner {
        stdout: &'static [u8],
        replace: bool,
        calls: usize,
    }
    impl CommandRunner for VersionRunner {
        fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
            self.calls += 1;
            assert_eq!(
                request.arguments,
                vec![std::ffi::OsString::from("--version")]
            );
            assert!(request.stdin.is_none());
            assert_eq!(request.reject_stdout, Some(AUTH_PORTAL_MARKER));
            if self.replace {
                std::fs::write(&request.executable, b"updated while probing").unwrap();
            }
            let mut receipt = failed(CommandEnd::Exited, Some(0), "");
            receipt.stdout = self.stdout.to_vec();
            receipt
        }
    }
    for (stdout, replace) in [
        (b"kiro-cli 2.29.0\n".as_slice(), false),
        (b"kiro-cli 2.29.invalid\n", false),
        (b"kiro-cli 2.29.0\n", true),
    ] {
        let sandbox = Sandbox::new();
        executable(&sandbox.parent.join("kiro-cli"));
        let command = PublicCommand::resolve("kiro-cli", sandbox.parent.as_os_str()).unwrap();
        let mut runner = VersionRunner {
            stdout,
            replace,
            calls: 0,
        };
        let result = probe_launcher(&command, &mut runner, discovery_bounds());
        assert_eq!(runner.calls, 1);
        if replace {
            assert!(matches!(result, Err(EngineProbeError::Host(error))
                if error.kind == team_agent_contract::host::HostErrorKind::Conflict));
        } else if stdout.ends_with(b"invalid\n") {
            assert!(result.unwrap_err().to_string().contains("2.29.invalid"));
        } else {
            let native = result.unwrap();
            assert_eq!(native.version, "2.29.0");
            assert_eq!(
                native.executable_sha256,
                team_agent_contract::host::digest(b"test executable bytes; never run")
            );
        }
    }
}
#[test]
fn dispatch_failure_is_classified_without_auth_or_noeffect_inference() {
    let failure = failed(
        CommandEnd::Exited,
        Some(1),
        "error: failed to launch /Users/fixture/.local/bin/kiro-cli-chat\n",
    );
    assert_eq!(
        command_failure(&failure),
        Some(CommandFailure::HelperLaunchFailed)
    );
    assert!(failure.may_have_executed());
    assert_eq!(
        command_failure(&failed(CommandEnd::TimedOut, Some(124), "")),
        Some(CommandFailure::TimedOut)
    );
    assert_eq!(
        command_failure(&failed(CommandEnd::Exited, Some(124), "")),
        Some(CommandFailure::NativeExit(124))
    );
}
#[test]
fn list_is_not_the_model_catalog_option() {
    assert_eq!(
        CHAT_CATALOG_ARGUMENTS,
        &["chat", "--list-models", "--format", "json"]
    );
    assert!(!CHAT_CATALOG_ARGUMENTS.contains(&"--list"));
}
