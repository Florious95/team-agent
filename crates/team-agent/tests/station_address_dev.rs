//! Release-base address compatibility through the shared durable-send engine.
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use team_agent::{
    messaging::presentation::PresentationRequest,
    station::send::{execute, Authority, Request},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // macOS clocks tick in microseconds; the counter keeps parallel roots unique.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "station-address-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn workspace(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        fs::create_dir_all(root.join(".team/runtime")).unwrap();
        fs::write(
            root.join(".team/runtime/state.json"),
            serde_json::to_vec(&json!({
                "active_team_key": "alpha",
                "teams": {
                    "alpha": {"team_id": "team-alpha", "agents": {"w1": {}, "w2": {}}},
                    "beta": {"team_id": "team-beta", "agents": {"w1": {}}}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        root
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(
    workspace: &Path,
    to: &str,
    team: Option<&str>,
    authority: Authority<'_>,
) -> Result<Value, String> {
    execute(
        Request {
            workspace,
            team,
            to,
            content: "address compatibility",
            task_id: None,
            message_id: None,
            presentation: &PresentationRequest::default(),
            mailbox: true,
            envelope: None,
        },
        authority,
    )
}
const OPERATOR: Authority<'static> = Authority::Operator { sender: "leader" };

#[test]
fn qualified_addresses_use_their_own_scope_without_a_caller_snapshot() {
    let fixture = Fixture::new();
    let target = fixture.workspace("target");
    // The caller directory deliberately has no runtime snapshot.
    let result = send(
        &fixture.0,
        &format!("{}::team-beta/w1", target.display()),
        Some("not-a-team"),
        OPERATOR,
    )
    .unwrap();
    assert_eq!(result["status"], "stored");
    let inbox = team_agent::inbox_store::read(&target, "w1", 10, Some("beta")).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    // A workspace-qualified bare name selects that workspace's active team.
    assert_eq!(
        send(
            &fixture.0,
            &format!("{}::w1", target.display()),
            Some("beta"),
            OPERATOR
        )
        .unwrap()["status"],
        "stored"
    );
    assert_eq!(
        team_agent::inbox_store::read(&target, "w1", 10, Some("alpha")).unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn seats_cannot_use_qualified_addresses_to_escape_workspace_or_team_scope() {
    let fixture = Fixture::new();
    let here = fixture.workspace("here");
    let elsewhere = fixture.workspace("elsewhere");
    let seat = Authority::Seat {
        sender: "leader",
        team: "alpha",
    };
    assert!(send(
        &here,
        &format!("{}::alpha/w1", elsewhere.display()),
        None,
        seat
    )
    .is_err());
    assert!(send(&here, "beta/w1", None, seat).is_err());
    assert!(!elsewhere.join(".team/inbox").exists());
    assert!(!here.join(".team/inbox").exists());
    assert_eq!(
        send(&here, "team-alpha/w1", None, seat).unwrap()["status"],
        "stored"
    );
}

#[test]
fn mixed_broadcast_and_aliases_deduplicate_canonical_destinations() {
    let fixture = Fixture::new();
    let root = fixture.workspace("here");
    let result = send(&root, "w1,team-alpha/w1,*", Some("alpha"), OPERATOR).unwrap();
    assert_eq!(result["stored"], 2);
    assert_eq!(result["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn bad_batch_addresses_do_not_abort_good_recipients() {
    let fixture = Fixture::new();
    let root = fixture.workspace("here");
    let result = send(&root, "beta//w1,alpha/w1", None, OPERATOR).unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["stored"], 1);
    assert_eq!(result["messages"][0]["ok"], false);
    assert!(send(&root, "session:window", None, OPERATOR).is_err());
}
