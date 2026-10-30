//! Developer-owned C5 unit tests. The independently supplied red suite is opaque.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "station-agenda-test-{}",
            seat_store::random_instance().unwrap()
        ));
        seat_store::private_dir(&root).unwrap();
        let state = root.join(".team/runtime/state.json");
        seat_store::private_dir(state.parent().unwrap()).unwrap();
        fs::write(
            state,
            json!({"active_team_key":"alpha","team_key":"alpha","teams":{
                "alpha":{"team_key":"alpha","agents":{}},"beta":{"team_key":"beta","agents":{}}
            }})
            .to_string(),
        )
        .unwrap();
        Self(root)
    }
    fn store(&self, team: &str) -> Store {
        Store::new(&self.0, team).unwrap()
    }
    fn job(&self, id: &str) -> Job {
        let now = Utc::now().to_rfc3339();
        Job {
            v: 1,
            requires: CAPABILITIES.iter().map(|s| (*s).into()).collect(),
            id: id.into(),
            workspace: self
                .0
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            team: "alpha".into(),
            text: "agenda unit reminder".into(),
            trigger: "in 1s".into(),
            created_at: now.clone(),
            due_at: now,
            interval_ms: None,
            occurrence: 0,
            status: "active".into(),
            result: None,
            extra: BTreeMap::new(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn duration_parser_preserves_existing_units_and_bounds() {
    for (value, ms) in [
        ("1ms", 1),
        ("2s", 2000),
        ("3m", 180000),
        ("1h", 3600000),
        ("1d", 86400000),
    ] {
        assert_eq!(parse_duration(value).unwrap().num_milliseconds(), ms);
    }
    for value in [
        "",
        "0s",
        "-1s",
        "1",
        "1.5s",
        "NaNs",
        "1w",
        "1s2m",
        "9223372036854775807d",
    ] {
        assert!(parse_duration(value).is_err(), "{value}");
    }
}
#[test]
fn list_and_missing_cancel_are_read_only() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    assert!(store.list().unwrap().is_empty());
    assert!(!store.cancel("sched_ab").unwrap());
    assert!(!store.root.exists());
    assert!(!fixture.0.join(".team/runtime/team.db").exists());
}
#[test]
fn job_storage_is_team_scoped_and_isolates_corruption() {
    let fixture = Fixture::new();
    let alpha = fixture.store("alpha");
    let beta = fixture.store("beta");
    alpha.write(&fixture.job("sched_a")).unwrap();
    fs::write(alpha.path("sched_b").unwrap(), b"{broken").unwrap();
    let rows = alpha.list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["status"], "active");
    assert_eq!(rows[1]["status"], "unreadable");
    assert!(beta.list().unwrap().is_empty());
    assert!(!beta.cancel("sched_a").unwrap());
    assert!(alpha.read("sched_a").unwrap().is_some());
}
#[test]
fn copied_job_cannot_escape_its_recorded_scope() {
    let fixture = Fixture::new();
    let beta = fixture.store("beta");
    beta.write(&fixture.job("sched_a")).unwrap();
    assert_eq!(beta.read("sched_a").unwrap_err(), "agenda_scope_mismatch");
}
#[test]
fn unknown_required_feature_is_never_executed() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    let mut job = fixture.job("sched_a");
    job.requires.push("future-critical-v2".into());
    store.write(&job).unwrap();
    assert!(store.fire("sched_a").is_err());
    assert!(store.path("sched_a").unwrap().exists());
    assert!(!fixture.0.join(".team/inbox").exists());
}
#[test]
fn cancellation_archives_instead_of_rewriting_history() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    let mut job = fixture.job("sched_a");
    job.extra
        .insert("future_display".into(), json!({"keep":true}));
    store.write(&job).unwrap();
    assert!(store.cancel("sched_a").unwrap());
    assert!(!store.cancel("sched_a").unwrap());
    let archived = store
        .read_path("sched_a", &store.root.join("done/sched_a.json"))
        .unwrap()
        .unwrap();
    assert_eq!(archived.status, "cancelled");
    assert_eq!(archived.extra["future_display"]["keep"], true);
    assert!(store.fire("sched_a").unwrap()["cancelled"] == true);
}
#[test]
fn interrupted_one_shot_is_not_replayed_past_m1_dedupe_window() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    let mut job = fixture.job("sched_a");
    job.status = "running".into();
    store.write(&job).unwrap();
    let recovered = store.recover_interrupted("sched_a").unwrap();
    assert_eq!(recovered.status, "failed");
    assert_eq!(recovered.result.unwrap()["submission"], "unknown");
    assert!(store.list().unwrap().is_empty());
}
#[test]
fn interrupted_recurrence_skips_the_ambiguous_occurrence() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    let mut job = fixture.job("sched_a");
    job.status = "running".into();
    job.interval_ms = Some(60000);
    store.write(&job).unwrap();
    let recovered = store.recover_interrupted("sched_a").unwrap();
    assert_eq!(recovered.status, "active");
    assert_eq!(recovered.occurrence, 1);
    assert!(recovered.due().unwrap() > Utc::now());
    assert_eq!(recovered.result.unwrap()["submission"], "unknown");
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn native_wait_detects_moved_storage_ancestors_without_a_tick() {
    for relative in ["", ".team", ".team/agenda", ".team/agenda/alpha"] {
        let fixture = Fixture::new();
        let store = fixture.store("alpha");
        store.write(&fixture.job("sched_a")).unwrap();
        let socket = std::os::unix::net::UnixDatagram::unbound().unwrap();
        let waiter = wait::Wait::new(&store, &socket).unwrap();
        let path = if relative.is_empty() { fixture.0.clone() } else { fixture.0.join(relative) };
        let moved = path.with_extension("moved");
        fs::rename(&path, &moved).unwrap();
        let ready = waiter.ready(&socket, Some(StdDuration::from_secs(1)));
        fs::rename(&moved, &path).unwrap();
        assert!(!ready.unwrap(), "ancestor {relative:?} disappeared without a wake");
    }
}
#[cfg(unix)]
#[test]
fn activation_lock_survives_workspace_disappearance() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    let _activation = store.lock("activation").unwrap();
    let moved = fixture.0.with_extension("moved");
    fs::rename(&fixture.0, &moved).unwrap();
    let contender = store.try_activation_lock();
    fs::rename(&moved, &fixture.0).unwrap();
    assert!(contender.unwrap().is_none());
}
#[test]
fn one_shot_uses_shared_durable_send_and_never_creates_sqlite() {
    let fixture = Fixture::new();
    let store = fixture.store("alpha");
    store.write(&fixture.job("sched_a")).unwrap();
    let receipt = store.fire("sched_a").unwrap();
    assert_eq!(receipt["status"], "done");
    assert_eq!(receipt["result"]["status"], "stored");
    assert_eq!(receipt["result"]["live"], false);
    assert!(store.fire("sched_a").unwrap()["cancelled"] == true);
    let inbox = crate::inbox_store::read(&fixture.0, "leader", 20, Some("alpha")).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert!(!fixture.0.join(".team/runtime/team.db").exists());
}
