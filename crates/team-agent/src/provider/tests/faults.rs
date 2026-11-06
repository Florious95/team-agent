// P0 — interrupt marker must match EXACTLY (provider_state/claude.py:75 `== _INTERRUPT_TEXT`),
// not `.contains()`. A transcript that merely QUOTES the marker must stay Unknown
// (ping-blocked); only the exact text is idle-eligible IdleInterrupted. §11 wrong-direction.

// P1 — claude api_error fault requires level=="error" (claude.py:54). Missing/other
// level → NO fault (Python count 0).

// P1 — claude api_error turn_id fallback chain = sessionId -> parentUuid -> uuid
// (claude.py:58), NOT sessionId -> requestId. Collapsing to None breaks C8 dedup.



// P1 — codex requestApproval turn_id = params.turnId OR params.turn_id (codex.py:79).

// P1 — codex app-server turn/completed status completed/interrupted/inProgress map to
// idle/idle_interrupted/working (codex.py:69-74), not Unknown.

// P1 — codex event_msg task_started → open turn → working(alive) (codex.py:30-31).

// SPAWN+FAKE-WORKER RED — Provider::Fake::build_command must invoke the fake-worker backing program
// (the single-binary `fake-worker` subcommand), NOT the bare placeholder vec!["fake"] (no backing
// binary). This is what makes launch(dry_run=false)'s spawn path exercisable with NO subscription
// provider. Golden intent: fake_worker.py + provider_cli/fake.py — a subscription-free backing worker.
#[test]
fn fake_build_command_invokes_fake_worker_not_bare_fake() {
    let adapter = get_adapter(Provider::Fake);
    let argv = adapter
        .build_command_with_permissions(AuthMode::Subscription, None, None, None, false)
        .expect("fake build_command");
    assert_ne!(
        argv,
        vec!["fake".to_string()],
        "Provider::Fake::build_command must not be the bare placeholder vec![\"fake\"] (no backing binary)"
    );
    assert!(
        argv.iter().any(|a| a == "fake-worker"),
        "Provider::Fake::build_command must invoke the `fake-worker` subcommand so the spawn path runs the fake backing worker; got {argv:?}"
    );
}
