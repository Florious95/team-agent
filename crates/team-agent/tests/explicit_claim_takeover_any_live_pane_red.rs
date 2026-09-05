//! Bug 3 RED: explicit claim-leader/takeover are operator recovery commands.
//!
//! A live caller pane is enough. The pane must not be rejected because tmux reports
//! `pane_current_command=node` for an npm-launched Codex process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Output;
#[cfg(unix)]
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(unix)]
use std::sync::mpsc::{self, Receiver};
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

use serde_json::{json, Value};
#[cfg(unix)]
use sha2::{Digest, Sha256};
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

#[test]
#[serial_test::serial(env)]
fn concurrent_first_claims_persist_shared_conditional_write_winner() {
    let real_registry = HermeticTestEnv::real_home_registry_snapshot();
    let hermetic = HermeticTestEnv::enter("shared-pane-nonce-concurrent");
    let parent = hermetic.workspace("parent");
    let workspace_a = hermetic.workspace("external-a");
    let workspace_b = hermetic.workspace("external-b");
    for workspace in [&workspace_a, &workspace_b] {
        seed_runtime_state(workspace);
    }

    let nonce_file = hermetic.root().join("live-pane-nonce");
    let observers = hermetic.root().join("empty-observers");
    let arrivals = hermetic.root().join("set-option-arrivals");
    let set_log = hermetic.root().join("set-option.log");
    let fake_tmux = shared_nonce_fake_tmux_bin(hermetic.root());
    let path = format!(
        "{}:{}",
        fake_tmux.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let endpoint = hermetic.root().join("tmux-shared.sock");
    let tmux = format!("{},123,0", endpoint.display());
    let parent_path = parent.to_string_lossy().to_string();
    let nonce_path = nonce_file.to_str().unwrap();
    let workspace_a_str = workspace_a.to_str().unwrap();
    let workspace_b_str = workspace_b.to_str().unwrap();

    let (claim_a, claim_b) = thread::scope(|scope| {
        let handle_a = scope.spawn(|| {
            hermetic.run_cli_env(
                &workspace_a,
                &[
                    "claim-leader",
                    "--workspace",
                    workspace_a_str,
                    "--team",
                    "current",
                    "--confirm",
                    "--json",
                ],
                &[
                    ("PATH", path.as_str()),
                    ("TMUX", tmux.as_str()),
                    ("TMUX_PANE", CALLER_PANE),
                    ("FAKE_NONCE_FILE", nonce_path),
                    ("FAKE_EMPTY_OBSERVER_ID", "a"),
                    ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
                    ("TEAM_AGENT_MACHINE_FINGERPRINT", "shared-pane-concurrent"),
                    ("FAKE_PANE_CWD", parent_path.as_str()),
                ],
            )
        });
        let handle_b = scope.spawn(|| {
            hermetic.run_cli_env(
                &workspace_b,
                &[
                    "claim-leader",
                    "--workspace",
                    workspace_b_str,
                    "--team",
                    "current",
                    "--confirm",
                    "--json",
                ],
                &[
                    ("PATH", path.as_str()),
                    ("TMUX", tmux.as_str()),
                    ("TMUX_PANE", CALLER_PANE),
                    ("FAKE_NONCE_FILE", nonce_path),
                    ("FAKE_EMPTY_OBSERVER_ID", "b"),
                    ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
                    ("TEAM_AGENT_MACHINE_FINGERPRINT", "shared-pane-concurrent"),
                    ("FAKE_PANE_CWD", parent_path.as_str()),
                ],
            )
        });
        (
            handle_a.join().expect("claim A thread"),
            handle_b.join().expect("claim B thread"),
        )
    });

    assert!(
        cli_success_failure(&claim_a, "concurrent workspace A claim").is_none(),
        "concurrent workspace A claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_a.stdout),
        String::from_utf8_lossy(&claim_a.stderr)
    );
    assert!(
        cli_success_failure(&claim_b, "concurrent workspace B claim").is_none(),
        "concurrent workspace B claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_b.stdout),
        String::from_utf8_lossy(&claim_b.stderr)
    );

    assert!(
        observers.join("a").is_file() && observers.join("b").is_file(),
        "both claims must lock an empty first inventory before either write; observers={:?}",
        std::fs::read_dir(&observers)
            .map(|entries| entries
                .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    assert!(
        arrivals.join("a").is_file() && arrivals.join("b").is_file(),
        "both claims must arrive at set-option before the nonce is published; arrivals={:?}",
        std::fs::read_dir(&arrivals)
            .map(|entries| entries
                .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );

    let winner = std::fs::read_to_string(&nonce_file).unwrap();
    assert!(
        !winner.trim().is_empty(),
        "concurrent first claims must persist one live pane nonce"
    );

    let set_log_text = std::fs::read_to_string(&set_log).unwrap_or_default();
    let option_attempts = set_log_text
        .lines()
        .filter(|line| line.contains(" o=1 "))
        .count();
    let writes = set_log_text
        .lines()
        .filter(|line| line.starts_with("wrote "))
        .count();
    let already_set = set_log_text
        .lines()
        .filter(|line| line.starts_with("already_set "))
        .count();
    assert_eq!(
        option_attempts, 2,
        "both first claims must hit set-option -o after empty observation; log={set_log_text}"
    );
    assert_eq!(
        writes, 1,
        "only-if-unset must admit exactly one writer; log={set_log_text}"
    );
    assert_eq!(
        already_set, 1,
        "the losing first claim must take the already-set readback path; log={set_log_text}"
    );

    let receiver_a = current_receiver(&workspace_a);
    let receiver_b = current_receiver(&workspace_b);
    assert_eq!(receiver_a["binding_nonce"], json!(winner.trim()));
    assert_eq!(receiver_b["binding_nonce"], json!(winner.trim()));

    let _path = hermetic.with_env("PATH", &path);
    let _tmux = hermetic.with_env("TMUX", &tmux);
    let _pane = hermetic.with_env("TMUX_PANE", CALLER_PANE);
    let _cwd = hermetic.with_env("FAKE_PANE_CWD", parent_path.as_str());
    let _nonce = hermetic.with_env("FAKE_NONCE_FILE", nonce_path);
    let transport = team_agent::transport_factory::tmux_endpoint_transport(endpoint.to_str().unwrap());
    for (workspace, receiver) in [(&workspace_a, &receiver_a), (&workspace_b, &receiver_b)] {
        assert!(
            matches!(
                resolve_live_leader_channel(workspace, receiver, &transport),
                LeaderChannelResolution::Live(_)
            ),
            "concurrent first-claim receiver must resolve Live: workspace={} receiver={receiver}",
            workspace.display()
        );
    }
    hermetic.assert_real_registry_unchanged(real_registry);
}

#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn shared_pane_after_external_and_internal_claims_repeats_mcp_send_and_report_result() {
    let real_registry = HermeticTestEnv::real_home_registry_snapshot();
    let hermetic = HermeticTestEnv::enter("claim-comms");
    let parent = hermetic.workspace("parent");
    let workspace_a = hermetic.workspace("external-a");
    let workspace_b = hermetic.workspace("external-b");
    for workspace in [&parent, &workspace_a, &workspace_b] {
        seed_comms_runtime_state(workspace);
    }

    let candidate = PathBuf::from(env!("CARGO_BIN_EXE_team-agent"))
        .canonicalize()
        .expect("canonicalize candidate team-agent");
    let candidate_sha = sha256_file(&candidate);
    let socket = hermetic::short_tmux_socket("claim-comms");
    let pane = start_shared_leader_pane(&hermetic, &socket, &parent);
    let tmux = format!("{},12345,0", socket.display());
    let claim_env = [
        ("TMUX", tmux.as_str()),
        ("TMUX_PANE", pane.as_str()),
        ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
        ("TEAM_AGENT_MACHINE_FINGERPRINT", "claim-comms"),
    ];

    let claim_a = claim_current(&hermetic, &workspace_a, &claim_env);
    assert!(
        cli_success_failure(&claim_a, "external workspace A claim").is_none(),
        "external workspace A claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_a.stdout),
        String::from_utf8_lossy(&claim_a.stderr)
    );
    let first_nonce = receiver_nonce(&workspace_a);
    assert!(!first_nonce.is_empty(), "first claim must persist a binding nonce");

    let claim_b = claim_current(&hermetic, &workspace_b, &claim_env);
    assert!(
        cli_success_failure(&claim_b, "external workspace B claim").is_none(),
        "external workspace B claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_b.stdout),
        String::from_utf8_lossy(&claim_b.stderr)
    );
    assert_eq!(
        receiver_nonce(&workspace_b),
        first_nonce,
        "second external claim must keep the shared live pane nonce"
    );

    let mut coord_a = spawn_candidate_coordinator(&hermetic, &candidate, &workspace_a);
    let mut coord_b = spawn_candidate_coordinator(&hermetic, &candidate, &workspace_b);
    let receipt = coordinator_identity_receipt(
        &candidate,
        &candidate_sha,
        &workspace_a,
        coord_a.pid(),
        &workspace_b,
        coord_b.pid(),
    );
    eprintln!("{receipt}");
    coord_a.assert_running("workspace A");
    coord_b.assert_running("workspace B");

    let mut mcp_a = spawn_worker_mcp(&hermetic, &candidate, &workspace_a);
    let mut mcp_b = spawn_worker_mcp(&hermetic, &candidate, &workspace_b);
    let pid = std::process::id();
    let round1 = [
        format!("CANARY_A1S_{pid}"),
        format!("CANARY_A1R_{pid}"),
        format!("CANARY_B1S_{pid}"),
        format!("CANARY_B1R_{pid}"),
    ];
    let bodies_r1 = mcp_round(&mut mcp_a, &mut mcp_b, &round1);
    wait_for_pane_tokens(&socket, &pane, &round1);

    let claim_internal = claim_current(&hermetic, &parent, &claim_env);
    assert!(
        cli_success_failure(&claim_internal, "internal parent claim").is_none(),
        "internal parent claim failed: stdout={} stderr={}",
        String::from_utf8_lossy(&claim_internal.stdout),
        String::from_utf8_lossy(&claim_internal.stderr)
    );
    assert_eq!(
        receiver_nonce(&workspace_a),
        first_nonce,
        "internal claim must not rotate workspace A nonce"
    );
    assert_eq!(
        receiver_nonce(&workspace_b),
        first_nonce,
        "internal claim must not rotate workspace B nonce"
    );

    let round2 = [
        format!("CANARY_A2S_{pid}"),
        format!("CANARY_A2R_{pid}"),
        format!("CANARY_B2S_{pid}"),
        format!("CANARY_B2R_{pid}"),
    ];
    let bodies_r2 = mcp_round(&mut mcp_a, &mut mcp_b, &round2);
    wait_for_pane_tokens(&socket, &pane, &round2);

    for token in round1.iter().chain(round2.iter()) {
        let in_a = db_contains_token(&workspace_a, token);
        let in_b = db_contains_token(&workspace_b, token);
        if token.contains("CANARY_A") {
            assert!(in_a, "workspace A db missing own token {token}");
            assert!(!in_b, "workspace B db leaked workspace A token {token}");
        } else {
            assert!(in_b, "workspace B db missing own token {token}");
            assert!(!in_a, "workspace A db leaked workspace B token {token}");
        }
    }
    for body in bodies_r1.iter().chain(bodies_r2.iter()) {
        assert_ne!(
            body.get("notification_status"),
            Some(&json!("queued")),
            "MCP notification_status must not be queued; body={body}"
        );
        assert_ne!(
            body.get("notification_status"),
            Some(&json!("queued_only")),
            "MCP notification_status must not be queued_only; body={body}"
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
    // Concurrent first claims: lock the first inventory as empty, wait until
    // both set-option -o clients arrive before publishing, then mkdir-lock and
    // rename a complete nonce into place. Waits are file rendezvous with a
    // wall-clock deadline, not sleep-based races.
    let script = r##"#!/bin/sh
root=$(dirname "$FAKE_NONCE_FILE")
# This pane exists only on the caller's absolute socket, not every discovery candidate.
if [ "$1" != "-S" ] || [ "$2" != "$root/tmux-shared.sock" ]; then
  exit 1
fi
observers="$root/empty-observers"
arrivals="$root/set-option-arrivals"
setlog="$root/set-option.log"
lockdir="$root/nonce.lockdir"
needed=${FAKE_EMPTY_NEEDED:-2}
deadline=${FAKE_WAIT_DEADLINE_SECS:-3}

file_count() {
  ls -1 "$1" 2>/dev/null | wc -l | tr -d ' '
}

wait_for_files() {
  dir=$1
  need=$2
  what=$3
  mkdir -p "$dir"
  start=$(date +%s)
  while :; do
    have=$(file_count "$dir")
    [ -n "$have" ] || have=0
    if [ "$have" -ge "$need" ]; then
      return 0
    fi
    now=$(date +%s)
    if [ $((now - start)) -ge "$deadline" ]; then
      printf 'fake-tmux fixture timeout: %s have=%s need=%s\n' "$what" "$have" "$need" >&2
      exit 42
    fi
  done
}

acquire_lock() {
  start=$(date +%s)
  while ! mkdir "$lockdir" 2>/dev/null; do
    now=$(date +%s)
    if [ $((now - start)) -ge "$deadline" ]; then
      printf 'fake-tmux fixture timeout: nonce lock\n' >&2
      exit 42
    fi
  done
}

read_nonce() {
  if [ -f "$FAKE_NONCE_FILE" ]; then
    cat "$FAKE_NONCE_FILE"
  fi
}

publish_nonce() {
  staging="$root/nonce.staging.$$"
  if ! printf '%s' "$1" > "$staging"; then
    rm -f "$staging"
    rmdir "$lockdir" 2>/dev/null
    printf 'fake-tmux fixture error: staging nonce write failed\n' >&2
    exit 42
  fi
  if ! mv "$staging" "$FAKE_NONCE_FILE"; then
    rm -f "$staging"
    rmdir "$lockdir" 2>/dev/null
    printf 'fake-tmux fixture error: nonce publish failed\n' >&2
    exit 42
  fi
}

case " $* " in
  *" list-panes "*)
    nonce=""
    if [ -n "$FAKE_EMPTY_OBSERVER_ID" ]; then
      if [ ! -f "$observers/$FAKE_EMPTY_OBSERVER_ID" ]; then
        mkdir -p "$observers"
        printf 'empty\n' > "$observers/$FAKE_EMPTY_OBSERVER_ID"
        wait_for_files "$observers" "$needed" "empty list-panes observers"
        nonce=""
      else
        nonce=$(read_nonce)
      fi
    else
      nonce=$(read_nonce)
    fi
    cwd="${FAKE_PANE_CWD:-/tmp}"
    printf '%%9\tteam-current\t0\tleader\t0\t/dev/ttys001\tcodex\t1\t%s\t1\t0\t4242\t%s\n' "$cwd" "$nonce"
    exit 0
    ;;
  *" set-option "*)
    last=""
    has_o=0
    for arg do
      [ "$arg" = "-o" ] && has_o=1
      last=$arg
    done
    printf 'id=%s o=%s nonce=%s\n' "${FAKE_EMPTY_OBSERVER_ID:-none}" "$has_o" "$last" >> "$setlog"
    if [ -n "$FAKE_EMPTY_OBSERVER_ID" ]; then
      mkdir -p "$arrivals"
      printf 'here\n' > "$arrivals/$FAKE_EMPTY_OBSERVER_ID"
      wait_for_files "$arrivals" "$needed" "set-option arrivals"
    fi
    acquire_lock
    if [ "$has_o" = 1 ] && [ -f "$FAKE_NONCE_FILE" ]; then
      rmdir "$lockdir"
      printf 'already_set id=%s\n' "${FAKE_EMPTY_OBSERVER_ID:-none}" >> "$setlog"
      echo "already set: @team_agent_pane_binding_nonce" >&2
      exit 1
    fi
    publish_nonce "$last"
    printf 'wrote id=%s nonce=%s\n' "${FAKE_EMPTY_OBSERVER_ID:-none}" "$last" >> "$setlog"
    rmdir "$lockdir"
    exit 0
    ;;
  *" show-options "*)
    nonce=$(read_nonce)
    if [ -n "$nonce" ]; then
      printf '%s\n' "$nonce"
      exit 0
    fi
    exit 1
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

#[cfg(unix)]
fn seed_comms_runtime_state(ws: &Path) {
    team_agent::state::persist::save_runtime_state(
        ws,
        &json!({
            "active_team_key": "current",
            "session_name": "current",
            "team_dir": ws.to_string_lossy(),
            "agents": {
                "worker_a": {
                    "provider": "fake",
                    "status": "running"
                }
            },
            "teams": {
                "current": {
                    "session_name": "current",
                    "team_dir": ws.to_string_lossy(),
                    "agents": {
                        "worker_a": { "status": "running" }
                    },
                    "tasks": [
                        { "id": "task_comms", "assignee": "worker_a", "status": "pending" }
                    ]
                }
            },
            "tasks": [
                { "id": "task_comms", "assignee": "worker_a", "status": "pending" }
            ]
        }),
    )
    .unwrap();
}

#[cfg(unix)]
fn claim_current(hermetic: &HermeticTestEnv, workspace: &Path, env: &[(&str, &str)]) -> Output {
    hermetic.run_cli_env(
        workspace,
        &[
            "claim-leader",
            "--workspace",
            workspace.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        env,
    )
}

#[cfg(unix)]
fn receiver_nonce(workspace: &Path) -> String {
    current_receiver(workspace)["binding_nonce"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[cfg(unix)]
fn sha256_file(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path).unwrap_or_else(|error| {
        panic!("read {} for sha256: {error}", path.display())
    }));
    format!("{:x}", hasher.finalize())
}

#[cfg(unix)]
fn start_shared_leader_pane(hermetic: &HermeticTestEnv, socket: &Path, cwd: &Path) -> String {
    let bin_dir = hermetic.root().join("provider-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let codex = bin_dir.join("codex");
    if !codex.exists() {
        std::os::unix::fs::symlink("/bin/cat", &codex).unwrap();
    }
    let socket_str = socket.to_str().expect("tmux socket utf8");
    let session = format!("ta-cc-{}", std::process::id());
    let _ = Command::new("tmux")
        .args(["-S", socket_str, "kill-server"])
        .output();
    let created = Command::new("tmux")
        .args([
            "-S",
            socket_str,
            "new-session",
            "-d",
            "-s",
            &session,
            "-n",
            "leader",
            "-c",
            cwd.to_str().unwrap(),
            codex.to_str().unwrap(),
        ])
        .output()
        .expect("tmux new-session");
    assert!(
        created.status.success(),
        "tmux new-session failed: stderr={}",
        String::from_utf8_lossy(&created.stderr)
    );
    let _ = Command::new("tmux")
        .args([
            "-S",
            socket_str,
            "set-option",
            "-t",
            &session,
            "history-limit",
            "5000",
        ])
        .output();
    let pane_out = Command::new("tmux")
        .args([
            "-S",
            socket_str,
            "display-message",
            "-p",
            "-t",
            &format!("{session}:leader"),
            "#{pane_id}",
        ])
        .output()
        .expect("tmux pane id");
    assert!(
        pane_out.status.success(),
        "tmux pane id failed: stderr={}",
        String::from_utf8_lossy(&pane_out.stderr)
    );
    let pane = String::from_utf8_lossy(&pane_out.stdout).trim().to_string();
    assert!(
        pane.starts_with('%'),
        "expected fixture pane id, got {pane:?}"
    );
    hermetic.register_owned_tmux_socket(socket);
    let pid_out = Command::new("tmux")
        .args([
            "-S",
            socket_str,
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{pane_pid}",
        ])
        .output()
        .expect("tmux pane pid");
    if let Ok(pid) = String::from_utf8_lossy(&pid_out.stdout).trim().parse() {
        hermetic.register_owned_pid(pid);
    }
    pane
}

#[cfg(unix)]
struct OwnedChild {
    child: Child,
}

#[cfg(unix)]
impl OwnedChild {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn assert_running(&mut self, label: &str) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "{label} candidate coordinator exited before identity receipt"
        );
    }
}

#[cfg(unix)]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn spawn_candidate_coordinator(
    hermetic: &HermeticTestEnv,
    candidate: &Path,
    workspace: &Path,
) -> OwnedChild {
    let runtime = workspace.join(".team/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let log = std::fs::File::create(runtime.join("coordinator-stdio.log")).unwrap();
    let mut command = Command::new(candidate);
    command
        .args([
            "coordinator",
            "--workspace",
            workspace.to_str().unwrap(),
            "--tick-interval",
            "0.25",
        ])
        .current_dir(workspace)
        .env("HOME", hermetic.home())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    for key in hermetic::CALLER_IDENTITY_ENVS {
        command.env_remove(key);
    }
    let child = command.spawn().expect("spawn candidate coordinator");
    hermetic.register_owned_pid(child.id());
    OwnedChild { child }
}

#[cfg(unix)]
fn coordinator_identity_receipt(
    candidate: &Path,
    candidate_sha: &str,
    workspace_a: &Path,
    pid_a: u32,
    workspace_b: &Path,
    pid_b: u32,
) -> Value {
    let meta_a = wait_coordinator_metadata(workspace_a);
    let meta_b = wait_coordinator_metadata(workspace_b);
    assert_eq!(
        meta_a.get("pid").and_then(Value::as_u64),
        Some(u64::from(pid_a)),
        "workspace A coordinator.json pid must be the spawned candidate process; metadata={meta_a}"
    );
    assert_eq!(
        meta_b.get("pid").and_then(Value::as_u64),
        Some(u64::from(pid_b)),
        "workspace B coordinator.json pid must be the spawned candidate process; metadata={meta_b}"
    );
    let path_a = coordinator_binary_path(&meta_a, workspace_a);
    let path_b = coordinator_binary_path(&meta_b, workspace_b);
    let sha_a = sha256_file(&path_a);
    let sha_b = sha256_file(&path_b);
    assert_eq!(
        sha_a, candidate_sha,
        "workspace A coordinator binary_path bytes must match candidate; candidate={} meta={}",
        candidate.display(),
        path_a.display()
    );
    assert_eq!(
        sha_b, candidate_sha,
        "workspace B coordinator binary_path bytes must match candidate; candidate={} meta={}",
        candidate.display(),
        path_b.display()
    );
    json!({
        "candidate_binary": {
            "path": candidate.to_string_lossy(),
            "sha256": candidate_sha,
        },
        "coordinators": [
            {
                "workspace": workspace_a.to_string_lossy(),
                "pid": meta_a.get("pid"),
                "binary_path": path_a.to_string_lossy(),
                "sha256": sha_a,
                "metadata_path": workspace_a.join(".team/runtime/coordinator.json").to_string_lossy(),
            },
            {
                "workspace": workspace_b.to_string_lossy(),
                "pid": meta_b.get("pid"),
                "binary_path": path_b.to_string_lossy(),
                "sha256": sha_b,
                "metadata_path": workspace_b.join(".team/runtime/coordinator.json").to_string_lossy(),
            }
        ]
    })
}

#[cfg(unix)]
fn wait_coordinator_metadata(workspace: &Path) -> Value {
    let path = workspace.join(".team/runtime/coordinator.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&path) {
            last = text;
            if let Ok(value) = serde_json::from_str::<Value>(&last) {
                if value
                    .get("binary_path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| !path.is_empty())
                {
                    return value;
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "coordinator.json missing usable binary_path for {}; last={last:?}",
        workspace.display()
    );
}

#[cfg(unix)]
fn coordinator_binary_path(metadata: &Value, workspace: &Path) -> PathBuf {
    let raw = metadata
        .get("binary_path")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "coordinator.json binary_path field missing for {}; metadata={metadata}",
                workspace.display()
            )
        });
    PathBuf::from(raw)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(raw))
}

#[cfg(unix)]
struct WorkerMcp {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<String>,
    next_id: i64,
}

#[cfg(unix)]
impl WorkerMcp {
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let raw = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        let result = raw
            .get("result")
            .unwrap_or_else(|| panic!("tools/call {name} missing result: {raw}"));
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|item| item.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let body = serde_json::from_str(text).unwrap_or_else(|_| json!({"raw_text": text}));
        assert!(
            !is_error,
            "MCP tools/call {name} returned isError; body={body} raw={raw}"
        );
        assert_ne!(
            body.get("ok").and_then(Value::as_bool),
            Some(false),
            "MCP tools/call {name} body ok=false; body={body} raw={raw}"
        );
        body
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        writeln!(self.stdin, "{request}").expect("write json-rpc request");
        self.stdin.flush().expect("flush json-rpc request");
        let line = self
            .stdout_rx
            .recv_timeout(Duration::from_secs(75))
            .unwrap_or_else(|_| panic!("timed out waiting for MCP {method}"));
        let value: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("invalid JSON-RPC for {method}: {error}; line={line}")
        });
        assert!(
            value.get("error").is_none(),
            "JSON-RPC {method} protocol error: {value}"
        );
        value
    }
}

#[cfg(unix)]
impl Drop for WorkerMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn spawn_worker_mcp(hermetic: &HermeticTestEnv, candidate: &Path, workspace: &Path) -> WorkerMcp {
    let mut command = Command::new(candidate);
    command
        .args(["mcp-server", "--workspace", workspace.to_str().unwrap()])
        .current_dir(workspace)
        .env("HOME", hermetic.home())
        .env("TEAM_AGENT_WORKSPACE", workspace)
        .env("TEAM_AGENT_ID", "worker_a")
        .env("TEAM_AGENT_OWNER_TEAM_ID", "current")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in ["TMUX", "TMUX_PANE"] {
        command.env_remove(key);
    }
    let mut child = command.spawn().expect("spawn candidate mcp-server");
    hermetic.register_owned_pid(child.id());
    let stdin = child.stdin.take().expect("mcp stdin");
    let stdout = child.stdout.take().expect("mcp stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut client = WorkerMcp {
        child,
        stdin,
        stdout_rx: rx,
        next_id: 1,
    };
    let init = client.rpc("initialize", json!({"protocolVersion": "2024-11-05"}));
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("team_orchestrator"),
        "mcp-server initialize identity; init={init}"
    );
    client
}

#[cfg(unix)]
fn mcp_round(mcp_a: &mut WorkerMcp, mcp_b: &mut WorkerMcp, tokens: &[String; 4]) -> Vec<Value> {
    vec![
        mcp_a.call_tool(
            "send_message",
            json!({"to": "leader", "content": tokens[0]}),
        ),
        mcp_a.call_tool(
            "report_result",
            json!({
                "task_id": "task_comms",
                "agent_id": "worker_a",
                "status": "success",
                "summary": tokens[1]
            }),
        ),
        mcp_b.call_tool(
            "send_message",
            json!({"to": "leader", "content": tokens[2]}),
        ),
        mcp_b.call_tool(
            "report_result",
            json!({
                "task_id": "task_comms",
                "agent_id": "worker_a",
                "status": "success",
                "summary": tokens[3]
            }),
        ),
    ]
}

#[cfg(unix)]
fn wait_for_pane_tokens(socket: &Path, pane: &str, tokens: &[String]) {
    let socket_str = socket.to_str().expect("tmux socket utf8");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = capture_fixture_pane(socket_str, pane);
        if tokens.iter().all(|token| last.contains(token)) {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
    let missing = tokens
        .iter()
        .filter(|token| !last.contains(token.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    panic!("fixture pane missing canaries {missing:?}; capture={last:?}");
}

#[cfg(unix)]
fn capture_fixture_pane(socket: &str, pane: &str) -> String {
    let output = Command::new("tmux")
        .args([
            "-S",
            socket,
            "capture-pane",
            "-p",
            "-S",
            "-2000",
            "-t",
            pane,
        ])
        .output()
        .expect("tmux capture-pane");
    assert!(
        output.status.success(),
        "tmux capture-pane failed: socket={socket} pane={pane} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
fn db_contains_token(workspace: &Path, token: &str) -> bool {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return false;
    }
    let conn = rusqlite::Connection::open(&db).unwrap();
    let like = format!("%{token}%");
    let messages = conn
        .query_row(
            "select count(*) from messages where content like ?1",
            [&like],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let results = conn
        .query_row(
            "select count(*) from results where envelope like ?1",
            [&like],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    messages + results > 0
}
