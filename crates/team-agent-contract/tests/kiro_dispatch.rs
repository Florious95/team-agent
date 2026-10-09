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

#[test]
fn bundle_dispatcher_symlink_resolves_its_actual_chat_helper_without_path_mutation() {
    let sandbox = Sandbox::new();
    let bundle = sandbox.parent.join("App Bundle/MacOS");
    std::fs::create_dir_all(&bundle).unwrap();
    executable(&bundle.join("kiro-cli"));
    executable(&bundle.join("kiro-cli-chat"));
    let wrapper = sandbox.parent.join("kiro-cli");
    symlink(bundle.join("kiro-cli"), &wrapper).unwrap();
    assert_eq!(
        resolve_helper(&wrapper, &sandbox.parent).unwrap(),
        bundle.join("kiro-cli-chat")
    );
    assert!(std::fs::symlink_metadata(wrapper)
        .unwrap()
        .file_type()
        .is_symlink());
}
#[test]
fn local_install_helper_is_a_declared_fallback_not_a_home_scan() {
    let sandbox = Sandbox::new();
    let wrapper = sandbox.parent.join("kiro-cli");
    executable(&wrapper);
    let local = sandbox.parent.join(".local/bin");
    std::fs::create_dir_all(&local).unwrap();
    executable(&local.join("kiro-cli-chat"));
    assert_eq!(
        resolve_helper(&wrapper, &sandbox.parent).unwrap(),
        local.join("kiro-cli-chat")
    );
    assert!(!sandbox.parent.join(".kiro").exists());
}
#[test]
fn explicit_helper_does_not_silently_switch_installations() {
    let sandbox = Sandbox::new();
    let direct = sandbox.parent.join("explicit/kiro-cli-chat");
    std::fs::create_dir(direct.parent().unwrap()).unwrap();
    let local = sandbox.parent.join(".local/bin");
    std::fs::create_dir_all(&local).unwrap();
    executable(&local.join("kiro-cli-chat"));
    assert!(resolve_helper(&direct, &sandbox.parent).is_err());
    executable(&direct);
    assert_eq!(resolve_helper(&direct, &sandbox.parent).unwrap(), direct);
}
#[test]
fn nonexecutable_helper_is_not_claimed_as_available() {
    let sandbox = Sandbox::new();
    let direct = sandbox.parent.join("kiro-cli-chat");
    std::fs::write(&direct, b"not executable").unwrap();
    std::fs::set_permissions(&direct, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(resolve_helper(&direct, &sandbox.parent).is_err());
}
#[test]
fn observed_version_and_dispatch_failure_are_classified_without_auth_or_noeffect_inference() {
    assert_eq!(parse_version(b"kiro-cli 2.28.0\n").unwrap(), "2.28.0");
    for text in [
        b"2.28.0".as_slice(),
        b"kiro-cli 2.29.0",
        b"kiro-cli 2.28.0\nextra",
    ] {
        assert!(parse_version(text).is_err());
    }
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
