#[test]
fn e23_fallback_pane_has_single_shared_noisy_surface() {
    let leader_receiver = include_str!("../leader_receiver.rs");
    assert!(leader_receiver.contains("deliver_to_leader_fallback_pane"));
    assert!(leader_receiver.contains("leader_receiver.fallback_pane_attempt"));
    assert!(leader_receiver.contains("leader_receiver.fallback_pane_submitted"));
    assert!(leader_receiver.contains("leader_receiver.fallback_pane_failed"));
    assert!(leader_receiver.contains("delivered_via=fallback_pane"));
    assert!(
        leader_receiver.contains("if submit_ok {")
            && !leader_receiver.contains("submit_ok && readback_ok"),
        "fallback pane delivery must accept submit_ok alone; stale readback must not veto it"
    );
    assert!(
        leader_receiver.find("if primary_ok").unwrap()
            < leader_receiver
                .find("leader_receiver.fallback_pane_attempt")
                .unwrap(),
        "primary-ok guard must run before any fallback audit/inject attempt"
    );
    let already = leader_receiver
        .split("fn message_already_delivered")
        .nth(1)
        .unwrap_or("");
    assert!(
        already.contains("submitted_pending_acceptance")
            && already.contains("injected_awaiting_receipt"),
        "fallback already-delivered set must include the live post-submit statuses, not only the dead string submitted"
    );
}

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
