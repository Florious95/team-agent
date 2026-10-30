use super::*;

// M3 excision: GROUP I/J (tick orchestration and coordinator health/start/stop outcomes) is
// retired with the resident coordinator. Only the retained pid probe (GROUP K) stays here.

// ═════════════════════════════════════════════════════════════════════════
// GROUP K — pid_is_running (metadata.py:16) — zombie detection — RED
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn pid_is_running_false_for_impossible_pid() {
    // metadata.py:19-21 — os.kill(pid, 0) raises → False. pid 0/巨大 pid 不存在。
    let alive = pid_is_running(Pid(2_000_000_000)).expect("probe returns Result");
    assert!(!alive, "non-existent pid → not running");
}

#[test]
fn pid_is_running_true_for_self() {
    // current process is alive & not zombie → true.
    let me = std::process::id();
    let alive = pid_is_running(Pid(me)).expect("probe self");
    assert!(alive, "self pid is running");
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP L — resolve_tick_interval fallback (__main__.py:104) — RED
// ═════════════════════════════════════════════════════════════════════════

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
// Golden re-probed vs team-agent-public @ 439bef8 (lifecycle/metadata/watch/abnormal_track).

// P1 — pid_is_running must use os.kill(pid,0) first: a pid owned by another user (pid 1 /
// launchd, root) is EPERM → not signalable → False. Current only `ps -p` (rc=0) → True.
//
// 0.5.x Windows portability Batch 5: this test's PREMISE is Unix
// specific (pid 1 = init/launchd, EPERM semantics). On Windows,
// pid_is_running routes through `platform::process::pid_liveness` which
// has different accessible-pid semantics. Unix-only test.
#[cfg(unix)]
#[test]
fn p2_pid_is_running_false_for_cross_user_pid() {
    // The cross-user semantics only hold for a non-root caller (root CAN signal pid 1).
    if unsafe { libc::geteuid() } != 0 {
        assert!(
            !pid_is_running(Pid(1)).expect("probe pid 1"),
            "pid 1 (root-owned) must read as not-running for a non-root caller (kill EPERM)"
        );
    }
}
