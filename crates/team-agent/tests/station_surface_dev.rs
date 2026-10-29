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
fn check_guard(mode: &str, expected_pastes: usize) {
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
    if mode == 'named-draft':
        print('── w-claude ──\n❯ HUMAN-DRAFT-A\n' + '─' * 40)
    elif (root / 'blocked').exists():
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    if mode == "named-draft" {
        fs::create_dir_all(fixture.0.join(".team/runtime")).unwrap();
        fs::write(fixture.0.join(".team/runtime/state.json"), serde_json::to_vec(&serde_json::json!({
            "team_key":"alpha", "agents":{"worker":{"provider":"claude","pane_id":"%7","native_endpoint":"synthetic-surface"}}
        })).unwrap()).unwrap();
        command.args(["send", "worker", "hello", "--team", "alpha", "--json"]);
    } else {
        command.args([
            "send",
            "--socket",
            "synthetic-surface",
            "--pane",
            "%7",
            "--dialect",
            "enter",
            "hello",
        ]);
    }
    command
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
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let started = std::time::Instant::now();
    let mut child = command.spawn().unwrap();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(75) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("surface guard exceeded 75s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if mode == "named-draft" {
        assert!(output.status.success(), "{diagnostic}");
        assert!((55.0..75.0).contains(&started.elapsed().as_secs_f64()));
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["status"], "stored");
        assert!(receipt["delivery_error"]
            .as_str()
            .unwrap()
            .contains("composer_contention_timeout"));
        let inbox = team_agent::inbox_store::read(&fixture.0, "worker", 10, Some("alpha")).unwrap();
        assert_eq!(inbox["messages"][0]["content"], "hello");
    } else {
        assert!(!output.status.success());
        assert!(diagnostic.contains("non_composer"), "{diagnostic}");
        if expected_pastes > 0 {
            assert!(diagnostic.contains("uncertain"), "{diagnostic}");
        }
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
    if mode == "named-draft" {
        assert!(!actions.iter().any(|s| s == "load-buffer"), "{actions:?}");
    } else if expected_pastes == 0 {
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
fn named_composer_borders_preserve_native_and_normalized_draft_detection() {
    use team_agent_contract::{
        contract::delivery::InputSurface,
        providers::claude::interaction::{inspect_legacy_composer, surface},
    };
    let bottom = "─".repeat(40);
    for top in [
        "─ w-claude ─",
        "── w-claude ──",
        "─── w-claude ───────────────────────────────────────",
    ] {
        let empty = format!("{top}\n❯\n{bottom}");
        assert_eq!(
            inspect_legacy_composer(&empty),
            Some((InputSurface::ComposerReady, String::new()))
        );
        assert_eq!(surface(&empty), InputSurface::ComposerReady);
        for space in [" ", "\u{00a0}"] {
            let text = format!("{top}\n❯{space}HUMAN-DRAFT-A\n{bottom}");
            assert_eq!(
                inspect_legacy_composer(&text),
                Some((InputSurface::ComposerContainsPaste, "HUMAN-DRAFT-A".into())),
                "{text}"
            );
            if space == "\u{00a0}" {
                assert_eq!(surface(&text), InputSurface::ComposerContainsPaste);
            }
        }
    }
    for top in ["  ── w-claude ──", "──w-claude──", "── w-claude", "── ──"]
    {
        assert!(
            inspect_legacy_composer(&format!("{top}\n❯ HUMAN-DRAFT-A\n{bottom}")).is_none(),
            "{top}"
        );
    }
}

#[test]
fn modal_appearing_after_staging_gets_no_paste_or_enter_even_with_raw_dialect_override() {
    check_guard("before-paste", 0);
}
#[test]
fn modal_appearing_after_paste_withholds_enter_and_reports_uncertainty() {
    check_guard("before-enter", 1);
}

#[test]
fn named_draft_is_held_for_60_seconds_without_input_and_message_stays_durable() {
    check_guard("named-draft", 0);
}
