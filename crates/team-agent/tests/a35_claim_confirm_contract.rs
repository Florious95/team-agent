//! A-35 RED/green contract for the public claim-leader confirm gate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic;

use hermetic::HermeticTestEnv;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Output;

const OWNER_PANE: &str = "%1";
const CALLER_PANE: &str = "%9";

#[test]
fn live_owner_without_confirm_is_refused() {
    let hermetic = HermeticTestEnv::enter("a35-live-owner");
    let ws = hermetic.workspace("claim");
    seed_state(&ws, OWNER_PANE);
    let fake_tmux = fake_tmux_bin(&ws, &[OWNER_PANE, CALLER_PANE]);
    let path = format!(
        "{}:{}",
        fake_tmux.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = hermetic.run_cli_env(
        &ws,
        &[
            "claim-leader",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--json",
        ],
        &[("PATH", path.as_str()), ("TMUX_PANE", CALLER_PANE)],
    );
    assert_eq!(output.status.code(), Some(1));
    let value = json_output(&output);
    assert_eq!(value["status"], json!("refused"), "value={value}");
    assert_eq!(
        value["reason"],
        json!("force_confirm_required"),
        "value={value}"
    );
}

#[test]
fn dead_owner_without_confirm_is_reclaimed() {
    let hermetic = HermeticTestEnv::enter("a35-dead-owner");
    let ws = hermetic.workspace("claim");
    seed_state(&ws, OWNER_PANE);
    let fake_tmux = fake_tmux_bin(&ws, &[CALLER_PANE]);
    let path = format!(
        "{}:{}",
        fake_tmux.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = hermetic.run_cli_env(
        &ws,
        &[
            "claim-leader",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--json",
        ],
        &[("PATH", path.as_str()), ("TMUX_PANE", CALLER_PANE)],
    );
    assert!(output.status.success());
    let value = json_output(&output);
    assert_eq!(value["ok"], json!(true), "value={value}");
    assert_eq!(value["status"], json!("claimed"), "value={value}");
}

fn seed_state(ws: &Path, owner_pane: &str) {
    team_agent::state::persist::save_runtime_state(
        ws,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "team_dir": ws.to_string_lossy().to_string(),
            "team_owner": {"pane_id": owner_pane, "owner_epoch": 2, "leader_session_uuid": "owner"},
            "leader_receiver": {"pane_id": owner_pane, "owner_epoch": 2, "leader_session_uuid": "owner"},
            "teams": {"current": {
                "status": "alive",
                "session_name": "team-current",
                "team_dir": ws.to_string_lossy().to_string(),
                "team_owner": {"pane_id": owner_pane, "owner_epoch": 2, "leader_session_uuid": "owner"},
                "leader_receiver": {"pane_id": owner_pane, "owner_epoch": 2, "leader_session_uuid": "owner"},
                "agents": {}
            }}
        }),
    )
    .unwrap();
}

fn fake_tmux_bin(ws: &Path, panes: &[&str]) -> std::path::PathBuf {
    let bin_dir = ws.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tmux = bin_dir.join("tmux");
    let lines = panes
        .iter()
        .map(|pane| {
            format!(
                "{pane}\tteam-current\t0\tleader\t0\t/dev/ttys001\tclaude\t1\t{}\t1\t0",
                ws.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in\n  *\" list-panes \"*)\n    printf '%s\\n' '{lines}'\n    exit 0\n    ;;\n  *\" display-message \"*)\n    printf 'claude\\n'\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n"
    );
    std::fs::write(&tmux, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    tmux
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "claim-leader output must be JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}
