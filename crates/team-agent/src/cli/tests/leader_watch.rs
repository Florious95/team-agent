use super::*;

// =========================================================================
// (DELEGATION sweep, rt-host-a loop #2) — cmd_send persistence + shutdown teardown.
// =========================================================================

// Public mailbox delivery is durable in the inbound log, never the coordinator queue.

// 5 [P1, CONFIRMED PARTIAL] — shutdown stops the coordinator but NEVER kills the team tmux session
// (-> orphan worker panes). #[ignore] real-machine: the wired shutdown kills the real tmux session.
// SEAM NEEDED (note to porter): add shutdown_with_transport(workspace, keep_logs, team, &dyn Transport)
// (mirror restart_with_transport) so this can assert IN-PROCESS that transport.kill_session(team
// session) was called via a RecordingTransport — the clean "team session killed / workers reaped"
// observable. Until that seam lands, this asserts the real teardown surfaces (session-kill / stop).
#[test]
#[ignore = "real-machine: shutdown kills the team tmux session. PORTER SEAM: add \
                shutdown_with_transport(workspace, keep_logs, team, &dyn Transport) so kill_session is \
                assertable in-process via a RecordingTransport (workers reaped, no orphan panes)."]
fn cli_shutdown_kills_team_session_real_teardown() {
    let ws = seed_status_workspace(); // state.json with a running agent + session_name
    let args = ShutdownArgs {
        workspace: ws,
        team: None,
        keep_logs: true,
        json: true,
    };
    let text = format!("{:?}", cmd_shutdown(&args)).to_lowercase();
    assert!(
            text.contains("session") || text.contains("killed") || text.contains("kill_session"),
            "shutdown must KILL the team tmux session + reap workers (real teardown); the rt-host-a partial \
             bug stops the coordinator but leaves the session + workers running (orphans); got {text}"
        );
}

// =========================================================================
// WAVE-2 Lane B — CLI leader handler delegation byte-parity (leader_port::*).
//   The retained CLI verbs are thin pass-throughs (cli/commands.py:152-157):
//     cmd_takeover     -> runtime.takeover(ws, team, confirm)
//     cmd_claim_leader -> runtime.claim_leader(ws, team, confirm)  (Family A)
//   leader_port::{takeover,claim_leader} are STUBS returning
//   the WRONG shape today -> these LOCK the golden dict so the porter wires
//   them into leader::* / runtime.* and matches byte-for-byte.
//   Golden re-probed @ team-agent-public (probe_claim.py / probe_rtclaim.py /
//   probe_lid.py). Label: RED = stub returns wrong shape today.
// =========================================================================

fn leader_port_ws(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ta-cli-leaderport-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// #235 / I-RN-3 — explicit takeover is unconditional once the caller has a live pane:
// it is not a permission gate and must replace a live owner, advancing owner_epoch.
// `--confirm` may remain a UX affordance, but it is not the authority check.
#[test]
fn leader_port_takeover_refuses_without_confirm_byte_parity_obsolete_now_unconditional() {
    let cli = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/mod.rs"),
    )
    .unwrap();
    assert_eq!(
            cli.matches("claim_leader(workspace, team, true)").count(),
            1,
            "takeover must call the lease path as an explicit takeover, not pass the CLI confirm flag as a permission gate"
        );

    let ws = leader_port_ws("tk_unconditional_live");
    let team_id = crate::model::ids::TeamKey::new("current");
    let caller = crate::transport::PaneId::new("%5");
    let mut state = json!({
        "session_name": "team-agent-x",
        "team_owner": {
            "pane_id": "%1",
            "provider": "codex",
            "machine_fingerprint": "fp",
            "leader_session_uuid": "OWNERUUID",
            "owner_epoch": 2,
            "claimed_at": "t",
            "claimed_via": "claim-leader"
        },
        "leader_receiver": {
            "pane_id": "%1",
            "provider": "codex",
            "owner_epoch": 2,
            "leader_session_uuid": "OWNERUUID"
        }
    });
    let event_log = crate::event_log::EventLog::new(&ws);
    let live = LeaderPortSeededLiveness::new(&["%1", "%5"]);
    let r = crate::leader::claim_lease_no_incident(
        &ws, &mut state, None, &team_id, &caller, true, &event_log, &live,
    )
    .unwrap();

    assert!(
        r.ok,
        "explicit takeover must succeed even when old owner pane is live: {r:?}"
    );
    assert_eq!(r.status, crate::leader::LeaseStatus::Claimed);
    assert_eq!(r.owner_epoch, Some(crate::model::ids::OwnerEpoch(3)));
    assert_eq!(r.bound_pane_id, Some(caller.clone()));
    // Stage 3d: canonical owner/receiver at teams.<team_key>.
    let team_key = crate::state::projection::team_state_key(&state);
    assert_eq!(
        state["teams"][&team_key]["leader_receiver"]["pane_id"],
        json!("%5")
    );
    assert_eq!(
        state["teams"][&team_key]["team_owner"]["pane_id"],
        json!("%5")
    );
    assert_eq!(
        state["teams"][&team_key]["team_owner"]["owner_epoch"],
        json!(3)
    );
}

struct LeaderPortSeededLiveness {
    live_panes: std::collections::BTreeSet<String>,
}

impl LeaderPortSeededLiveness {
    fn new(panes: &[&str]) -> Self {
        Self {
            live_panes: panes.iter().map(|pane| (*pane).to_string()).collect(),
        }
    }
}

impl crate::state::owner_gate::PaneLivenessProbe for LeaderPortSeededLiveness {
    fn liveness(&self, pane_id: &str) -> crate::model::enums::PaneLiveness {
        if self.live_panes.contains(pane_id) {
            crate::model::enums::PaneLiveness::Live
        } else {
            crate::model::enums::PaneLiveness::Dead
        }
    }
}

// RED — takeover(confirm=true) with no $TMUX_PANE: the Family A positive-source
// bind gate fires -> refused caller_pane_missing with the bind diagnostic dict.
// golden probe_claim.py takeover(confirm=True): {ok:false, status:"refused",
// reason:"caller_pane_missing", caller_pane_id:"", caller_current_command:"",
// hint:"run team-agent from inside your leader pane (the tmux pane you want to
// own this team)."}. Current stub returns {ok:true,...} (wrong) -> RED.
#[test]
#[ignore = "RED needs $TMUX_PANE ABSENT (Family A bind gate); run `--ignored` in a non-tmux shell. \
                Inside tmux the live-pane resolver would engage. Seam: porter wires leader_port::takeover \
                -> runtime.takeover whose Family A bind refuses caller_pane_missing when $TMUX_PANE missing."]
fn leader_port_takeover_confirm_without_pane_refuses_caller_pane_missing() {
    if std::env::var_os("TMUX_PANE").is_some() {
        return; // inside tmux the bind gate would pass; this case verifies the missing arm.
    }
    let ws = leader_port_ws("tk_confirm_nopane");
    // seed a resolvable team so the gate reaches the bind step (not team_target_unresolved).
    let st = json!({"session_name": "team-agent-x", "teams": {"team-agent-x": {}}});
    let path = crate::state::persist::runtime_state_path(&ws);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&st).unwrap()).unwrap();
    let v = super::leader_port::takeover(&ws, Some("team-agent-x"), true).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["status"], json!("refused"));
    assert_eq!(v["reason"], json!("caller_pane_missing"));
    assert_eq!(v["caller_pane_id"], json!(""));
    assert_eq!(v["caller_current_command"], json!(""));
    assert_eq!(
            v["hint"],
            json!("run team-agent from inside your leader pane (the tmux pane you want to own this team).")
        );
}

#[test]
fn claim_leader_default_response_hides_internal_binding_details() {
    let mut value = serde_json::json!({
        "ok": true,
        "status": "already_bound",
        "owner_epoch": 3,
        "bound_pane_id": "%77",
        "leader_receiver": {"pane_id": "%77"},
        "team_owner": {"pane_id": "%77"},
        "topology_convergence": {"status": "not_converged_old_endpoint_live"},
        "leader_registry": {"status": "registered"}
    });
    super::leader_port::compact_lease_value(&mut value);
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["status"], json!("already_bound"));
    assert_eq!(value["bound_pane_id"], json!("%77"));
    assert_eq!(value["reason"], json!("not_converged_old_endpoint_live"));
    for field in [
        "owner_epoch",
        "leader_receiver",
        "team_owner",
        "topology_convergence",
        "leader_registry",
    ] {
        assert!(value.get(field).is_none(), "default claim response leaked {field}");
    }
}

// RED — claim_leader(confirm=false) with no $TMUX_PANE: runtime.claim_leader is
// ALSO Family A (runtime.py:791) -> the bind gate fires FIRST, so the no-pane
// refusal is caller_pane_missing (NOT the leader-lane "not_in_tmux_pane").
// golden probe_rtclaim.py. This pins the runtime-vs-leader distinction:
// the CLI projection must reflect runtime.claim_leader's Family A bind gate.
// Current stub returns {ok:true, inbox_hint:...} (wrong) -> RED.
#[test]
#[ignore = "RED needs $TMUX_PANE ABSENT (Family A bind gate fires first); run `--ignored` in a \
                non-tmux shell. Inside tmux the resolver would engage. Seam: porter wires \
                leader_port::claim_leader -> runtime.claim_leader (Family A) whose bind refuses \
                caller_pane_missing (NOT leader-lane not_in_tmux_pane) when $TMUX_PANE missing."]
fn leader_port_claim_leader_no_pane_refuses_caller_pane_missing_family_a() {
    if std::env::var_os("TMUX_PANE").is_some() {
        return;
    }
    let ws = leader_port_ws("claim_nopane");
    let v = super::leader_port::claim_leader(&ws, None, false).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["status"], json!("refused"));
    assert_eq!(
        v["reason"],
        json!("caller_pane_missing"),
        "runtime.claim_leader (Family A) bind gate -> caller_pane_missing, NOT not_in_tmux_pane"
    );
    assert_eq!(v["caller_pane_id"], json!(""));
    assert_eq!(
            v["hint"],
            json!("run team-agent from inside your leader pane (the tmux pane you want to own this team).")
        );
}

// D7 [WARN] — WAVE-2 Lane B: empty caller binds refuse as caller_pane_missing.
#[test]
#[serial_test::serial(env)]
fn d7_lease_refusal_dict_is_golden_minimal_four_keys() {
    use std::sync::Mutex;
    static D7_ENV: Mutex<()> = Mutex::new(());
    let _g = D7_ENV.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvGuard::set(&[
        ("TMUX_PANE", Some("")), // empty caller -> caller_pane_missing via the lease path
        ("TEAM_AGENT_LEADER_PANE_ID", None),
    ]);
    let ws = tmp_workspace();
    crate::state::persist::save_runtime_state(&ws, &json!({})).unwrap(); // claim_leader loads state
    let result = super::leader_port::claim_leader(&ws, None, false);
    let v = result.expect("claim_leader projection");
    let obj = v.as_object().expect("lease dict");
    assert_eq!(
        obj.get("reason").and_then(|r| r.as_str()),
        Some("caller_pane_missing"),
        "precondition: empty caller -> caller_pane_missing; got {v:?}"
    );
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "ok",
            "status",
            "reason",
            "caller_pane_id",
            "caller_current_command",
            "hint"
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        "golden caller_pane_missing refusal key set"
    );
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(values: &[(&'static str, Option<&'static str>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}
