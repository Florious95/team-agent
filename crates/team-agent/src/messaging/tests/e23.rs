#[test]

#[test]
fn e23_no_parallel_direct_inject_or_fake_worker_mirror() {
    let results = include_str!("../results.rs");
    assert!(
        !results.contains("inject_leader_notification_direct"),
        "report_result must use the shared fallback pane primitive, not a private direct-inject path"
    );

    let fake_worker = include_str!("../../fake_worker.rs");
    assert!(
        !fake_worker.contains("mirror_fake_result_to_leader"),
        "fake_worker must not keep an independent leader pane mirror"
    );
    assert!(
        !fake_worker.contains("Transport::inject"),
        "fake_worker must report through report_result_for_owner_team, not inject leader panes directly"
    );
}
