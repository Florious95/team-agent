//! Synthetic backend checks for surface changes between preflight, paste and Enter.
//! Real pane survival and inbox persistence are independently covered by R17.
#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

const TRUST: &str = "Accessing workspace:\nQuick safety check: Is this a project you created or one you trust?\nYes, I trust this folder\n❯ No, exit\nEnter to confirm · Esc to cancel";

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn check_late_modal(mode: &str, expected_pastes: usize) {
    let root = std::env::temp_dir().join(format!(
        "station-surface-{}-{}-{}",
        std::process::id(),
        mode,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("bin")).unwrap();
    let fixture = Fixture(root);
    let wrapper = fixture.0.join("bin/tmux");
    fs::write(
        &wrapper,
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['STATION_SURFACE_ROOT'])
mode = os.environ['STATION_SURFACE_MODE']
args = sys.argv[1:]
action = args[2] if args[0] in ('-S', '-L') else args[0]
with (root / 'trace.jsonl').open('a') as log:
    log.write(json.dumps(action) + '\n')
if action == 'display-message':
    # This is a synthetic transport identity, not native-process evidence.
    print('0|1|2|' + str(root / 'tmux.sock'))
elif action == 'capture-pane':
    if (root / 'blocked').exists():
        print(os.environ['STATION_SURFACE_MODAL'])
    else:
        print('─' * 40 + '\n❯\u00a0\n' + '─' * 40)
elif action == 'load-buffer':
    sys.stdin.buffer.read()
    if mode == 'before-paste': (root / 'blocked').write_text('blocked')
elif action == 'paste-buffer':
    if mode == 'before-enter': (root / 'blocked').write_text('blocked')
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(fixture.0.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "send",
            "--socket",
            "synthetic-surface",
            "--pane",
            "%7",
            "--dialect",
            "enter",
            "hello",
        ])
        .current_dir(&fixture.0)
        .env("PATH", path)
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_AGENT_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TEAM_AGENT_WORKSPACE")
        .env("STATION_SURFACE_ROOT", &fixture.0)
        .env("STATION_SURFACE_MODE", mode)
        .env("STATION_SURFACE_MODAL", TRUST)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains("non_composer"), "{diagnostic}");
    if expected_pastes > 0 {
        assert!(diagnostic.contains("uncertain"), "{diagnostic}");
    }
    let actions: Vec<String> = fs::read_to_string(fixture.0.join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        actions
            .iter()
            .filter(|s| s.as_str() == "paste-buffer")
            .count(),
        expected_pastes,
        "{actions:?}"
    );
    assert!(!actions.iter().any(|s| s == "send-keys"), "{actions:?}");
    if expected_pastes == 0 {
        assert!(actions.iter().any(|s| s == "delete-buffer"), "{actions:?}");
    }
}

#[test]
fn trust_dialog_is_not_confused_with_quoted_text_or_a_stale_composer() {
    use team_agent_contract::{
        contract::delivery::InputSurface, providers::claude::interaction::surface,
    };
    let rule = "─".repeat(40);
    let ready = format!("{rule}\n❯\u{00a0}\n{rule}");
    assert_eq!(surface(TRUST), InputSurface::StartupRiskWarning);
    assert_eq!(
        surface(&format!("{ready}\n{TRUST}")),
        InputSurface::StartupRiskWarning
    );
    assert_eq!(
        surface(&format!("{TRUST}\n{ready}")),
        InputSurface::ComposerReady
    );
    let quoted = TRUST
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        surface(&format!(
            "{rule}\n❯\u{00a0}Quoted dialog:\n{quoted}\n{rule}"
        )),
        InputSurface::ComposerContainsPaste
    );
}

#[test]
fn modal_appearing_after_staging_gets_no_paste_or_enter_even_with_raw_dialect_override() {
    check_late_modal("before-paste", 0);
}
#[test]
fn modal_appearing_after_paste_withholds_enter_and_reports_uncertainty() {
    check_late_modal("before-enter", 1);
}
