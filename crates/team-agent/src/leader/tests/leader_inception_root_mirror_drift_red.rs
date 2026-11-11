//! RED: leader inception (`team-agent pi` managed launch) must not be refused by a
//! stale root `agents` mirror that has drifted from canonical `teams[active].agents`.
//!
//! Field evidence (2026-11-02, real workspace, 0.5.129 @ 26d4405e):
//! `state save conflict: agent_id=frontend-dev projection=agents
//! conflicting_fields=pane_id,pane_pid,window,spawned_at,spawn_epoch`, followed by
//! `leader.launcher.rollback reason=binding_persist_failed`. Root `agents` still held a
//! `running` row with full topology (tmux server long dead), while the canonical team
//! roster no longer contained that seat. `incept_launched_leader` projects the canonical
//! roster onto the root and the ordinary merge treats the stale mirror row as an
//! omitted live peer.

use super::*;
use serde_json::json;

const TEAM: &str = "demo-team";

fn running(agent_id: &str, pane: &str, pid: u32, epoch: u64) -> Value {
    json!({
        "agent_id": agent_id, "provider": "pi", "role": agent_id, "status": "running",
        "pane_id": pane, "pane_pid": pid, "window": agent_id,
        "spawned_at": "2026-09-16T03:30:25.555010+00:00", "spawn_epoch": epoch,
        "owner_team_id": TEAM,
    })
}

/// `drift = true` mirrors the forensic shape; `false` is the aligned control.
fn seed(tag: &str, drift: bool) -> PathBuf {
    let ws = p2_temp_ws(tag);
    let _ = std::fs::remove_dir_all(ws.join(".team"));
    let tester = running("tester", "%40", 45212, 6);
    let canonical = json!({
        "tester": tester,
        "new-seat": {"agent_id": "new-seat", "provider": "pi", "role": "new-seat"},
    });
    let root_agents = if drift {
        json!({"tester": tester, "frontend-dev": running("frontend-dev", "%43", 46942, 5)})
    } else {
        canonical.clone()
    };
    let common = json!({
        "workspace": ws, "team_dir": ws.join(".team").join(TEAM), "team_key": TEAM,
        "session_name": format!("team-{TEAM}"), "active_team_key": TEAM,
    });
    let mut team_entry = common.clone();
    team_entry["agents"] = canonical;
    let mut state = common;
    state["agents"] = root_agents;
    state["teams"] = json!({ TEAM: team_entry });
    let path = crate::state::persist::runtime_state_path(&ws);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    ws
}

fn incept(ws: &Path) -> Result<(), LeaderError> {
    let identity = LeaderIdentity {
        leader_session_uuid: uuid("", &ws.to_string_lossy(), "u", TEAM),
        leader_session_uuid_source: LeaderSessionUuidSource::Derived,
        machine_fingerprint: String::new(),
        workspace_abspath: ws.to_path_buf(),
        os_user: "u".into(),
        team_id: TeamKey::new(TEAM),
    };
    let observed = crate::transport::PaneInfo {
        pane_id: PaneId::new("%0"),
        session: SessionName::new("team-agent-leader-pi-repro"),
        window_index: None,
        window_name: Some(WindowName::new("pi")),
        pane_index: None,
        tty: None,
        current_command: None,
        current_path: Some(ws.to_path_buf()),
        active: true,
        pane_pid: Some(4242),
        leader_env: BTreeMap::new(),
    };
    super::super::lease::incept_launched_leader(
        ws,
        &identity,
        Provider::Pi,
        &observed,
        "/private/tmp/repro-state-conflict-unit.sock",
        &json!({}),
        false,
        LeaderStartMode::ManagedTmuxClient,
    ).map(|_| ())
}

fn assert_inception_committed(ws: &Path) {
    let state = crate::state::persist::load_runtime_state_without_migrations(ws).unwrap();
    let team = &state["teams"][TEAM];
    assert_eq!(team["team_owner"]["pane_id"], json!("%0"), "binding must commit: {team:#}");
    // Canonical roster is the truth: inception must not resurrect a seat it dropped,
    // and the root mirror must converge to it rather than keep the stale row.
    let canonical: Vec<_> = team["agents"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(canonical, ["tester", "new-seat"]);
    let root: Vec<_> = state["agents"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(root, canonical, "root agents mirror must match teams[active].agents");
    assert_eq!(team["agents"]["tester"], running("tester", "%40", 45212, 6));
}

#[test]
#[serial_test::serial(env)]
fn control_aligned_root_mirror_incepts_leader() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
    let ws = seed("inception_mirror_aligned", false);
    incept(&ws).expect("aligned control must incept");
    assert_inception_committed(&ws);
}

#[test]
#[serial_test::serial(env)]
fn red_stale_root_mirror_row_does_not_refuse_leader_inception() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _e = EnvGuard::apply(&[("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None)]);
    let ws = seed("inception_mirror_drift", true);
    if let Err(error) = incept(&ws) {
        panic!("leader inception refused by stale root mirror: {error}");
    }
    assert_inception_committed(&ws);
}

/// Guard (must stay green before AND after the fix): the fix must not blanket-disable
/// the omission guard. A seat that is live in the CANONICAL roster and silently omitted
/// by an ordinary writer is still a conflict.
#[test]
#[serial_test::serial(env)]
fn guard_canonical_live_peer_omission_still_conflicts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let ws = seed("inception_mirror_guard", false);
    let path = crate::state::persist::runtime_state_path(&ws);
    let mut latest: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let live = running("frontend-dev", "%43", 46942, 5);
    latest["agents"]["frontend-dev"] = live.clone();
    latest["teams"][TEAM]["agents"]["frontend-dev"] = live;
    std::fs::write(&path, serde_json::to_string_pretty(&latest).unwrap()).unwrap();
    let mut stale = latest.clone();
    stale["agents"].as_object_mut().unwrap().remove("frontend-dev");
    stale["teams"][TEAM]["agents"].as_object_mut().unwrap().remove("frontend-dev");
    stale["note"] = json!("ordinary writer");
    let error = crate::state::persist::save_runtime_state(&ws, &stale)
        .expect_err("omitting a canonical live peer must still conflict");
    assert!(matches!(error, crate::state::StateError::SaveConflict(_)), "{error}");
}
