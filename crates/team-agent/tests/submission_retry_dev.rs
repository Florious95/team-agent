//! Developer regression tests for explicit retry, not automatic mailbox replay.
#[path = "support/direct_fixture.rs"]
mod direct_fixture;
use direct_fixture::DirectFixture;
use serde_json::{json, Value};
use team_agent::messaging::presentation::PresentationRequest;
use team_agent::station::send::{self, Authority, Request};

fn state(f: &DirectFixture, pane: &str, status: &str) {
    f.set_state(&json!({"active_team_key":"current", "teams":{"current":{
        "team_id":"current", "tmux_socket":f.socket,
        "agents":{"producer":{"status":"running"},
            "worker-1":{"provider":"retired-provider","status":status,"pane_id":pane}}
    }}}));
}
fn send(root: &std::path::Path, mailbox: bool) -> Value {
    send::execute(Request {
        workspace:root, team:Some("current"), to:"worker-1", content:"retry-nonce",
        task_id:None, message_id:Some("retry-event-1"),
        presentation:&PresentationRequest::default(), mailbox, envelope:None,
    }, Authority::Seat {sender:"producer",team:"current"}).unwrap()
}

#[test]
fn stopped_seat_mailbox_is_durable_and_same_id_can_explicitly_submit_once() {
    let f = DirectFixture::new();
    state(&f, &f.pane, "stopped");
    let first = send(&f.root, true);
    assert_eq!(first["stored"], true, "{first}");
    assert_eq!(first["live"], false);
    state(&f, &f.pane, "running");
    let retry = send(&f.root, false);
    assert_eq!(retry["duplicate"], true, "{retry}");
    assert_eq!(retry["live"], true, "{retry}");
    assert_eq!(retry["record_ref"], first["record_ref"]);
    f.received(&send::render("producer", "retry-nonce", retry["message_id"].as_str().unwrap(), false));
    let bytes = std::fs::read(f.root.join("received")).unwrap();
    assert_eq!(send(&f.root, false)["live"], false);
    assert_eq!(std::fs::read(f.root.join("received")).unwrap(), bytes);
    assert_eq!(f.inbox("worker-1").len(), 1);
}

#[test]
fn physical_preflight_failure_can_retry_after_endpoint_recovery() {
    let f = DirectFixture::new();
    state(&f, "%999999", "dead");
    let first = send(&f.root, false);
    assert_eq!(first["stored"], true, "{first}");
    assert_eq!(first["submission"], "not_attempted", "{first}");
    state(&f, &f.pane, "running");
    let retry = send(&f.root, false);
    assert_eq!(retry["live"], true, "{retry}");
    assert_eq!(retry["record_ref"], first["record_ref"]);
    assert_eq!(f.inbox("worker-1").len(), 1);
}

#[test]
fn concurrent_same_record_retries_never_duplicate_physical_submission() {
    let f = DirectFixture::new();
    state(&f, &f.pane, "running");
    let workers: Vec<_> = (0..6).map(|_| {
        let root = f.root.clone();
        std::thread::spawn(move || send(&root, false))
    }).collect();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result["live"] == true).count(), 1, "{results:?}");
    assert!(results.iter().all(|result| result["stored"] == true));
    assert_eq!(f.inbox("worker-1").len(), 1);
}
