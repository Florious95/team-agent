//! Developer-owned tests of the C4 state machine; no provider or tmux dependency.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use team_agent::station::lifecycle::{execute, Action, NativeSeat, Observation, Request};
use team_agent::station::seat_store::{SeatManifest, SeatScope, SeatStore};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "station-lifecycle-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Native {
    root: PathBuf,
    live: BTreeMap<String, Value>,
    starts: usize,
    stop_calls: usize,
    stop_effects: usize,
    fail_after_start: bool,
    fail_before_stop: bool,
    require_stop_intent: bool,
    discarded: bool,
}
impl NativeSeat for Native {
    fn validate(&mut self, launch: &Value, _: Action) -> Result<(), String> {
        if launch.get("broken") == Some(&json!(true)) {
            Err("broken launch".into())
        } else {
            Ok(())
        }
    }
    fn observe(&mut self, seat: &SeatManifest) -> Result<Observation, String> {
        Ok(self
            .live
            .get(&seat.instance_id)
            .cloned()
            .map(Observation::Present)
            .unwrap_or(Observation::Absent))
    }
    fn start(
        &mut self,
        seat: &SeatManifest,
        request: &Request,
        _: Option<&SeatManifest>,
    ) -> Result<Value, String> {
        self.starts += 1;
        self.discarded = request.discard_session;
        let endpoint = json!({"binding":seat.instance_id,"scope":seat.scope});
        self.live.insert(seat.instance_id.clone(), endpoint.clone());
        if std::mem::take(&mut self.fail_after_start) {
            return Err("injected error after spawn".into());
        }
        Ok(endpoint)
    }
    fn stop(&mut self, seat: &SeatManifest) -> Result<(), String> {
        self.stop_calls += 1;
        if self.require_stop_intent {
            let current = store(&self.root, "a")
                .read()?
                .ok_or("missing durable intent")?;
            if !matches!(current.desired.as_str(), "stopped" | "removed") {
                return Err("stop effect preceded durable stop intent".into());
            }
        }
        if std::mem::take(&mut self.fail_before_stop) {
            return Err("injected error before stop".into());
        }
        if self.live.remove(&seat.instance_id).is_some() {
            self.stop_effects += 1;
        }
        Ok(())
    }
}
fn store(root: &Path, seat: &str) -> SeatStore {
    SeatStore::new(
        root,
        SeatScope {
            workspace: "workspace".into(),
            team: "t".into(),
            seat: seat.into(),
        },
    )
    .unwrap()
}
fn request(id: &str, action: Action) -> Request {
    Request {
        operation_id: id.into(),
        action,
        expected_instance: None,
        launch: None,
        discard_session: action == Action::Reset,
    }
}

#[test]
fn all_six_actions_converge_and_replay_without_repeating_effects() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    let add = request("add", Action::AddAgent);
    let first = execute(&store, add.clone(), &mut native).unwrap();
    let replay = execute(&store, add, &mut native).unwrap();
    assert_eq!(first.manifest.instance_id, replay.manifest.instance_id);
    assert_eq!(native.starts, 1);
    let noop = execute(&store, request("start", Action::Start), &mut native).unwrap();
    assert!(!noop.changed);
    assert_eq!(noop.manifest.instance_id, first.manifest.instance_id);
    let restarted = execute(&store, request("restart", Action::Restart), &mut native).unwrap();
    assert_ne!(restarted.manifest.instance_id, first.manifest.instance_id);
    execute(&store, request("reset", Action::Reset), &mut native).unwrap();
    assert!(native.discarded);
    execute(&store, request("stop", Action::Stop), &mut native).unwrap();
    assert!(
        !execute(&store, request("stop-again", Action::Stop), &mut native)
            .unwrap()
            .changed
    );
    execute(&store, request("remove", Action::RemoveAgent), &mut native).unwrap();
    assert!(
        !execute(
            &store,
            request("remove-again", Action::RemoveAgent),
            &mut native
        )
        .unwrap()
        .changed
    );
    assert!(native.live.is_empty());
    assert_eq!((native.starts, native.stop_effects), (3, 3));
    assert_eq!(store.read().unwrap().unwrap().desired, "removed");
}

#[test]
fn an_uncertain_spawn_is_observed_on_retry_instead_of_replayed() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native {
        fail_after_start: true,
        ..Native::default()
    };
    let start = request("start", Action::Start);
    assert!(execute(&store, start.clone(), &mut native).is_err());
    assert_eq!(native.starts, 1);
    assert_eq!(
        execute(&store, start, &mut native).unwrap().manifest.phase,
        "ready"
    );
    assert_eq!(native.starts, 1);
}

#[test]
fn cancelling_an_interrupted_spawn_publishes_stop_intent_before_cleanup() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native {
        root: root.path().into(),
        fail_after_start: true,
        ..Native::default()
    };
    let start = request("start", Action::Start);
    assert!(execute(&store, start.clone(), &mut native).is_err());
    native.require_stop_intent = true;
    native.fail_before_stop = true;
    let stop = request("stop", Action::Stop);
    assert!(execute(&store, stop.clone(), &mut native).is_err());
    assert_eq!(store.read().unwrap().unwrap().desired, "stopped");
    assert!(execute(&store, start, &mut native)
        .unwrap_err()
        .contains("operation_cancelled"));
    execute(&store, stop, &mut native).unwrap();
    assert!(native.live.is_empty());
    assert_eq!(native.starts, 1);
}

#[test]
fn stale_instances_are_rejected_but_display_generation_is_not_an_admission_gate() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    let first = execute(&store, request("start", Action::Start), &mut native).unwrap();
    {
        let lock = store.lock().unwrap();
        let mut manifest = lock.read().unwrap().unwrap();
        manifest.generation += 100;
        manifest
            .extra
            .insert("future_optional".into(), json!({"preserve":[1,2,3]}));
        lock.publish(&manifest).unwrap();
    }
    let mut reset = request("reset", Action::Reset);
    reset.expected_instance = Some(first.manifest.instance_id.clone());
    let current = execute(&store, reset, &mut native).unwrap();
    assert_eq!(
        current.manifest.extra["future_optional"],
        json!({"preserve":[1,2,3]})
    );
    let mut stale = request("stale-stop", Action::Stop);
    stale.expected_instance = Some(first.manifest.instance_id);
    let calls = native.stop_calls;
    assert!(execute(&store, stale, &mut native)
        .unwrap_err()
        .contains("stale_instance"));
    assert_eq!(native.stop_calls, calls);
    assert!(native.live.contains_key(&current.manifest.instance_id));
}

#[test]
fn forged_recovery_metadata_cannot_stop_a_sibling_seat() {
    let root = Workspace::new();
    let a = store(root.path(), "a");
    let peer = store(root.path(), "peer");
    let mut native = Native::default();
    execute(&a, request("start", Action::Start), &mut native).unwrap();
    let peer_receipt = execute(&peer, request("start", Action::Start), &mut native).unwrap();
    native.fail_before_stop = true;
    let restart = request("restart", Action::Restart);
    assert!(execute(&a, restart.clone(), &mut native).is_err());
    let path = root
        .path()
        .join(".team/station-v1/t/operations/a/restart.json");
    let mut operation: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    operation["before"] = serde_json::to_value(&peer_receipt.manifest).unwrap();
    std::fs::write(&path, serde_json::to_vec(&operation).unwrap()).unwrap();
    let calls = native.stop_calls;
    assert!(execute(&a, restart, &mut native)
        .unwrap_err()
        .contains("operation_scope_mismatch"));
    assert_eq!(native.stop_calls, calls);
    assert!(native.live.contains_key(&peer_receipt.manifest.instance_id));
}

#[test]
fn operation_retention_is_bounded_and_keeps_the_latest_receipt() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    for index in 0..24 {
        execute(
            &store,
            request(&format!("op-{index:02}"), Action::Start),
            &mut native,
        )
        .unwrap();
    }
    let directory = root.path().join(".team/station-v1/t/operations/a");
    let count = std::fs::read_dir(&directory).unwrap().count();
    assert_eq!(count, 16);
    assert!(directory.join("op-23.json").exists());
    assert!(!directory.join("op-00.json").exists());
    let replay = execute(&store, request("op-23", Action::Start), &mut native).unwrap();
    assert!(!replay.changed);
    assert_eq!(native.starts, 1);
}

#[test]
fn unknown_required_features_fail_before_native_effects() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    execute(&store, request("start", Action::Start), &mut native).unwrap();
    let path = store.path();
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["requires"] = json!(["future-critical-capability"]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let calls = native.stop_calls;
    assert!(execute(&store, request("stop", Action::Stop), &mut native).is_err());
    assert_eq!(native.stop_calls, calls);
    assert_eq!(native.live.len(), 1);
}

#[test]
fn reading_an_uninitialized_seat_does_not_create_runtime_files() {
    let root = Workspace::new();
    assert!(store(root.path(), "a").read().unwrap().is_none());
    assert!(!root.path().join(".team").exists());
}

#[test]
fn receipt_recovery_does_not_roll_back_a_newer_same_instance_publication() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    let start = request("start", Action::Start);
    execute(&store, start.clone(), &mut native).unwrap();
    let path = root
        .path()
        .join(".team/station-v1/t/operations/a/start.json");
    let mut operation: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Crash boundary: the manifest was published, but the final receipt was not.
    operation["phase"] = json!("native_done");
    std::fs::write(&path, serde_json::to_vec(&operation).unwrap()).unwrap();
    let latest = execute(&store, request("noop", Action::Start), &mut native).unwrap();
    {
        let lock = store.lock().unwrap();
        let mut manifest = lock.read().unwrap().unwrap();
        manifest
            .extra
            .insert("later_optional_metadata".into(), json!(42));
        lock.publish(&manifest).unwrap();
    }
    let recovered = execute(&store, start, &mut native).unwrap();
    let current = store.read().unwrap().unwrap();
    assert_eq!(current.last_operation_id, "noop");
    assert_eq!(current.generation, latest.manifest.generation);
    assert_eq!(
        recovered.manifest.extra["later_optional_metadata"],
        json!(42)
    );
    assert_eq!(native.starts, 1);
}

#[test]
fn orphan_reservations_do_not_make_the_operation_window_unbounded() {
    let root = Workspace::new();
    let store = store(root.path(), "a");
    let mut native = Native::default();
    let first = execute(&store, request("start", Action::Start), &mut native).unwrap();
    let directory = root.path().join(".team/station-v1/t/operations/a");
    let base: Value =
        serde_json::from_slice(&std::fs::read(directory.join("start.json")).unwrap()).unwrap();
    let lock = store.lock().unwrap();
    for index in 0..24 {
        // Crash before publishing the reserved manifest: no native effect was possible.
        let id = format!("orphan-{index:02}");
        let mut operation = base.clone();
        operation["request"]["operation_id"] = json!(id);
        operation["request"]["action"] = json!("restart");
        operation["request"]["expected_instance"] = json!(first.manifest.instance_id);
        operation["before"] = serde_json::to_value(&first.manifest).unwrap();
        operation["after"]["instance_id"] = json!(format!("{index:032x}"));
        operation["after"]["last_operation_id"] = json!(id);
        operation["after"]["generation"] = json!(first.manifest.generation + 1);
        operation["after"]["phase"] = json!("starting");
        operation["after"]["endpoint"] = Value::Null;
        operation["phase"] = json!("reserved");
        lock.publish_operation(&id, &operation).unwrap();
    }
    // Also exercises reentrancy while the same thread owns a live seat guard.
    execute(&store, request("latest", Action::Start), &mut native).unwrap();
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 16);
    assert!(directory.join("latest.json").exists());
    assert_eq!(native.starts, 1);
}
