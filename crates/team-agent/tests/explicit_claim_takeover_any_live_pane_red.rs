//! Bug 3 RED: explicit claim-leader/takeover are operator recovery commands.
//!
//! A live caller pane is enough. The pane must not be rejected because tmux reports
//! `pane_current_command=node` for an npm-launched Codex process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};
use team_agent::messaging::leader_channel::{
    resolve_live_leader_channel, LeaderChannelResolution,
};

#[path = "support/hermetic.rs"]
mod hermetic;
use hermetic::HermeticTestEnv;

const CALLER_PANE: &str = "%9";

#[test]
#[serial_test::serial(env)]
fn claim_leader_and_takeover_accept_live_node_caller_pane() {
    let real_registry = HermeticTestEnv::real_home_registry_snapshot();
    let hermetic = HermeticTestEnv::enter("explicit-claim-takeover");
    let ws = hermetic.workspace("any-live-pane");
    seed_runtime_state(&ws);
    let fake_tmux = fake_tmux_bin(&ws);
    let path = format!(
        "{}:{}",
        fake_tmux.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let caller_env = [
        ("PATH", path.as_str()),
        ("TMUX_PANE", CALLER_PANE),
        ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
        ("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a"),
    ];

    let claim = hermetic.run_cli_env(
        &ws,
        &[
            "claim-leader",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &caller_env,
    );
    let mut failures = Vec::new();
    if let Some(failure) = cli_success_failure(
        &claim,
        "claim-leader must claim from any live caller pane, including pane_current_command=node",
    ) {
        failures.push(failure);
    }
    assert_single_registry_source(&hermetic, "claim-leader");

    let takeover = hermetic.run_cli_env(
        &ws,
        &[
            "takeover",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &caller_env,
    );
    if let Some(failure) = cli_success_failure(
        &takeover,
        "takeover must claim from any live caller pane, including pane_current_command=node",
    ) {
        failures.push(failure);
    }
    assert_single_registry_source(&hermetic, "takeover");
    hermetic.assert_real_registry_unchanged(real_registry);

    assert!(
        failures.is_empty(),
        "explicit claim/takeover live-pane contract failed:\n{}",
        failures.join("\n\n")
    );
}

#[test]
#[serial_test::serial(env)]
fn cross_workspace_claims_reuse_live_nonce_and_internal_claim_preserves_grants() {
    let real_registry = HermeticTestEnv::real_home_registry_snapshot();
    let hermetic = HermeticTestEnv::enter("shared-pane-nonce");
    let parent = hermetic.workspace("parent");
    let workspace_a = hermetic.workspace("external-a");
    let workspace_b = hermetic.workspace("external-b");
    let workspace_internal = hermetic.workspace("internal");
    for workspace in [&workspace_a, &workspace_b, &workspace_internal] {
        seed_runtime_state(workspace);
    }

    let nonce_file = hermetic.root().join("live-pane-nonce");
    let fake_tmux = shared_nonce_fake_tmux_bin(hermetic.root());
    let path = format!(
        "{}:{}",
        fake_tmux.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let endpoint = hermetic.root().join("tmux-shared.sock");
    let tmux = format!("{},123,0", endpoint.display());
    let parent_path = parent.to_string_lossy().to_string();
    let common = [
        ("PATH", path.as_str()),
        ("TMUX", tmux.as_str()),
        ("TMUX_PANE", CALLER_PANE),
        ("FAKE_NONCE_FILE", nonce_file.to_str().unwrap()),
        ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
        ("TEAM_AGENT_MACHINE_FINGERPRINT", "shared-pane-test"),
    ];

    let claim_a = hermetic.run_cli_env(
        &workspace_a,
        &[
            "claim-leader",
            "--workspace",
            workspace_a.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &[
            common[0],
            common[1],
            common[2],
            common[3],
            common[4],
            common[5],
            ("FAKE_PANE_CWD", parent_path.as_str()),
        ],
    );
    assert!(
        cli_success_failure(&claim_a, "external workspace A claim").is_none(),
        "external workspace A claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_a.stdout),
        String::from_utf8_lossy(&claim_a.stderr)
    );
    let first_nonce = std::fs::read_to_string(&nonce_file).unwrap();
    assert!(!first_nonce.trim().is_empty(), "first claim must write a nonce");

    let claim_b = hermetic.run_cli_env(
        &workspace_b,
        &[
            "claim-leader",
            "--workspace",
            workspace_b.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &[
            common[0],
            common[1],
            common[2],
            common[3],
            common[4],
            common[5],
            ("FAKE_PANE_CWD", parent_path.as_str()),
        ],
    );
    assert!(
        cli_success_failure(&claim_b, "external workspace B claim").is_none(),
        "external workspace B claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_b.stdout),
        String::from_utf8_lossy(&claim_b.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&nonce_file).unwrap(),
        first_nonce,
        "second external claim must not rotate the shared live pane nonce"
    );

    let claim_internal = hermetic.run_cli_env(
        &workspace_internal,
        &[
            "claim-leader",
            "--workspace",
            workspace_internal.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &[
            common[0],
            common[1],
            common[2],
            common[3],
            common[4],
            common[5],
            ("FAKE_PANE_CWD", workspace_internal.to_str().unwrap()),
        ],
    );
    assert!(
        cli_success_failure(&claim_internal, "internal workspace claim").is_none(),
        "internal workspace claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_internal.stdout),
        String::from_utf8_lossy(&claim_internal.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&nonce_file).unwrap(),
        first_nonce,
        "an internal claim must not rotate an external grant's live nonce"
    );

    let receiver_a = current_receiver(&workspace_a);
    let receiver_b = current_receiver(&workspace_b);
    assert_eq!(receiver_a["binding_nonce"], json!(first_nonce.trim()));
    assert_eq!(receiver_b["binding_nonce"], json!(first_nonce.trim()));

    // Resolve both persisted external receivers after the later internal claim;
    // the fake inventory now reports the internal workspace cwd, so this proves
    // the explicit grant + shared live nonce path rather than an in-workspace pass.
    let _path = hermetic.with_env("PATH", &path);
    let _tmux = hermetic.with_env("TMUX", &tmux);
    let _pane = hermetic.with_env("TMUX_PANE", CALLER_PANE);
    let _cwd = hermetic.with_env("FAKE_PANE_CWD", workspace_internal.to_str().unwrap());
    let _nonce = hermetic.with_env("FAKE_NONCE_FILE", nonce_file.to_str().unwrap());
    let transport = team_agent::transport_factory::tmux_endpoint_transport(endpoint.to_str().unwrap());
    for (workspace, receiver) in [(&workspace_a, &receiver_a), (&workspace_b, &receiver_b)] {
        assert!(
            matches!(
                resolve_live_leader_channel(workspace, receiver, &transport),
                LeaderChannelResolution::Live(_)
            ),
            "external receiver must still resolve after internal claim: workspace={} receiver={receiver}",
            workspace.display()
        );
    }
    hermetic.assert_real_registry_unchanged(real_registry);
}

fn current_receiver(workspace: &Path) -> Value {
    let state = team_agent::state::persist::load_runtime_state(workspace).unwrap();
    state["teams"]["current"]["leader_receiver"].clone()
}

fn shared_nonce_fake_tmux_bin(root: &Path) -> PathBuf {
    let bin_dir = root.join("shared-nonce-fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tmux = bin_dir.join("tmux");
    let script = r##"#!/bin/sh
case " $* " in
  *" list-panes "*)
    nonce=""
    if [ -f "$FAKE_NONCE_FILE" ]; then nonce=$(cat "$FAKE_NONCE_FILE"); fi
    cwd="${FAKE_PANE_CWD:-/tmp}"
    printf '%%9\tteam-current\t0\tleader\t0\t/dev/ttys001\tcodex\t1\t%s\t1\t0\t4242\t%s\n' "$cwd" "$nonce"
    exit 0
    ;;
  *" set-option "*)
    last=""
    for arg do last=$arg; done
    printf '%s' "$last" > "$FAKE_NONCE_FILE"
    exit 0
    ;;
  *" display-message "*)
    printf 'codex\n'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"##;
    std::fs::write(&tmux, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin_dir
}

fn assert_single_registry_source(hermetic: &HermeticTestEnv, source: &str) {
    let entries = hermetic.registry_entries();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one registry entry under hermetic HOME; entries={entries:?}"
    );
    assert_eq!(
        entries[0].1.get("source").and_then(Value::as_str),
        Some(source),
        "expected hermetic registry entry source={source}; entry={}",
        entries[0].1
    );
}

fn cli_success_failure(output: &Output, label: &str) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value = match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return Some(format!(
                "{label}: stdout must be JSON; code={:?} stdout={stdout:?} stderr={stderr:?}",
                output.status.code()
            ));
        }
    };
    if output.status.code() != Some(0) {
        return Some(format!(
            "{label}: expected exit 0; stdout={stdout:?} stderr={stderr:?}"
        ));
    }
    if value["ok"] != json!(true) {
        return Some(format!("{label}: expected ok=true, got {value}"));
    }
    if !matches!(
        value["status"].as_str(),
        Some("claimed") | Some("already_bound")
    ) {
        return Some(format!(
            "{label}: status must be claimed/already_bound, got {value}"
        ));
    }
    if value["reason"] == json!("caller_not_leader_shaped") {
        return Some(format!(
            "{label}: explicit recovery must not inspect leader-shaped command metadata; got {value}"
        ));
    }
    None
}

fn seed_runtime_state(ws: &Path) {
    team_agent::state::persist::save_runtime_state(
        ws,
        &json!({
            "active_team_key": "current",
            "session_name": "current",
            "team_dir": ws.to_string_lossy().to_string(),
            "agents": {},
            "teams": {
                "current": {
                    "session_name": "current",
                    "team_dir": ws.to_string_lossy().to_string(),
                    "agents": {}
                }
            }
        }),
    )
    .unwrap();
}

fn fake_tmux_bin(ws: &Path) -> PathBuf {
    let bin_dir = ws.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tmux = bin_dir.join("tmux");
    let line = format!(
        "{pane}\tteam-current\t0\tleader\t0\t/dev/ttys001\tnode\t1\t{cwd}\t1\t0\n",
        pane = CALLER_PANE,
        cwd = ws.display(),
    );
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *" list-panes "*)
    printf '%s' '{line}'
    exit 0
    ;;
  *" display-message "*)
    printf 'node\n'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#
    );
    std::fs::write(&tmux, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin_dir
}
