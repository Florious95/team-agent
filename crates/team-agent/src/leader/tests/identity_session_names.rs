#[test]
fn unit0_leader_session_prefix_constant_is_stable() {
    // Pinned: the literal leader session prefix that gates "is this a
    // leader session" decisions everywhere in the runtime.
    assert_eq!(
        crate::leader::start::LEADER_SESSION_PREFIX,
        "team-agent-leader-"
    );
    // The layout module re-exports the same value (kept in sync via
    // const re-export).
    assert_eq!(
        crate::layout::sessions::LEADER_SESSION_PREFIX,
        crate::leader::start::LEADER_SESSION_PREFIX
    );
}

#[test]
fn unit0_leader_prefixed_name_must_never_be_taken_as_worker_session() {
    // Pinned invariant fed into unit-1 typed identity: any session
    // name starting with `LEADER_SESSION_PREFIX` is a leader launcher
    // session, never a worker session. unit-1 will wrap this rule in
    // typed constructors (`WorkerSession::new` rejects the prefix,
    // `LeaderLauncherSession::new` requires it).
    let leader = "team-agent-leader-claude-x-aaaaaaaaaaaa";
    let worker = "team-real-worker";
    let prefix = crate::leader::start::LEADER_SESSION_PREFIX;
    assert!(leader.starts_with(prefix));
    assert!(!worker.starts_with(prefix));
    // Symmetry: the runtime today uses this exact check
    // (cli/mod.rs:261 `starts_with(LEADER_SESSION_PREFIX)`) to spare
    // leader sessions during shutdown.
    let leader_name = crate::transport::SessionName::new(leader);
    let worker_name = crate::transport::SessionName::new(worker);
    assert!(
        crate::layout::sessions::is_leader_session(&leader_name),
        "is_leader_session must accept a leader-prefixed session",
    );
    assert!(
        !crate::layout::sessions::is_leader_session(&worker_name),
        "is_leader_session must reject a non-prefixed session",
    );
}
