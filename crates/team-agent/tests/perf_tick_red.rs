//! PERF batch P1-P5: tick IO/fork/event-budget contracts.
//!
//! Basis docs (sole inputs):
//! - `.team/artifacts/perf-tick-audit.md` (fable-architect, RS canonical repo)
//! - `perf-batch-cr-verdict.md` (constitution-reviewer, workspace artifacts) —
//!   25 constraints + the 10 named reverse cases below ARE this file's test list.
//!
//! P1 transcript whole-file read every tick (tick.rs:498-511): (size, mtime_ns) pair
//!    must skip unchanged files (C-P1-2/3), tail window >= Python `_TAIL_BYTES`
//!    (idle_takeover_wiring.py:100-114) keeps late errors visible (C-P1-1/4),
//!    truncation (size change) must still re-read (C-P1-5).
//! P2 session capture unbounded read (adapter.rs:776-786): Python parity head-bounded
//!    read (claude.py:432, 200 lines) and candidate cap 300 (claude.py:300) — #264
//!    faithful-port family. Bounded read must survive a poisoned (invalid UTF-8) tail
//!    (C-P2-4); newest candidates keep mtime priority (C-P2-3).
//! P3 tick counter forces a state.json rewrite every tick (tick.rs:188, :1095+):
//!    steady-state second tick must be a ZERO state write (C-P3-4); old states carrying
//!    the counter field must load gracefully (C-P3-3).
//! P4 compaction_observed re-emitted unchanged every tick (runtime_detectors.rs:43-66):
//!    same value must not re-emit (C-P4-1/4); changed value must still emit (leader
//!    refinement, anti-over-fix lock).
//! P5 per-tick subprocess amplification (tick.rs:358,370,495): list_targets <= 1 and
//!    list_windows <= 1 per tick (C-P5-1/2/5); TMUX_PANE_FORMAT must carry
//!    `#{pane_pid}` to kill the N+1 display-message fallback (C-P5-3).
//! Umbrella (leader's batch contract): steady-state two ticks — the second tick is
//!    silent (zero state write, zero new events, bounded forks).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use team_agent::coordinator::{
    observe_runtime, Coordinator, ErrorLists, ProviderRegistry, WorkspacePath,
};
use team_agent::model::enums::Provider;
use team_agent::model::ids::AgentId;
use team_agent::provider::{get_adapter, CaptureSessionContext, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, runtime_state_path, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

// ───────────────────────────── P1 · transcript tail IO ─────────────────────────────

/// C-P1-3 (reverse case `p1_transcript_unchanged_metadata_skips_read`): when the
/// rollout file's (size, mtime) did not change, the second tick must not read it at
/// all — proven by making the file unreadable (chmod 000) without touching
/// content/size/mtime: the abnormal payload must stay unchanged and must NOT degrade
/// to "unverifiable" (today's read-fail path, tick.rs:511-520).
#[test]
fn p1_transcript_unchanged_metadata_skips_read() {
    let ws = tmp_ws("p1-skip");
    let rollout = ws.join("rollout-w1.jsonl");
    std::fs::write(&rollout, "{\"method\":\"turn/started\",\"params\":{}}\n").unwrap();
    seed_tick_state(&ws, &[("w1", Some(&rollout))]);
    let transport = CountingTransport::new();
    let coord = coordinator(&ws, transport.clone());

    coord.tick().expect("first tick");
    let first = abnormal_watch(&ws, "w1");
    chmod(&rollout, 0o000);
    coord.tick().expect("second tick");
    chmod(&rollout, 0o644);
    let second = abnormal_watch(&ws, "w1");

    let mut failures = Vec::new();
    if second.to_string().contains("unverifiable") {
        failures.push(format!(
            "C-P1-3: second tick read the unchanged file (and failed on it) — \
(size, mtime) skip must avoid the read entirely; payload={second}"
        ));
    }
    if stable_watch_fields(&first) != stable_watch_fields(&second) {
        failures.push(format!(
            "C-P1-3: abnormal payload must be unchanged when the file is unchanged; \
first={first} second={second}"
        ));
    }
    assert!(
        failures.is_empty(),
        "P1 unchanged-skip contract failed:\n{}",
        failures.join("\n")
    );
}

/// C-P1-5 (reverse case `p1_transcript_truncate_triggers_reread`): a size change with
/// an unchanged mtime must STILL trigger a re-read (truncate / rewrite protection) —
/// the new tail fact must be observed. Green today (always reads); locks the fix.
#[test]
fn p1_transcript_truncate_triggers_reread() {
    let ws = tmp_ws("p1-truncate");
    let rollout = ws.join("rollout-w1.jsonl");
    let long_line = "{\"method\":\"turn/started\",\"params\":{\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxx\"}}\n";
    std::fs::write(&rollout, long_line.repeat(8)).unwrap();
    seed_tick_state(&ws, &[("w1", Some(&rollout))]);
    let coord = coordinator(&ws, CountingTransport::new());
    coord.tick().expect("first tick");

    // Shrink the file (size changes) and pin mtime back to the original value.
    let reference = ws.join("mtime-ref");
    std::process::Command::new("cp")
        .arg("-p")
        .arg(&rollout)
        .arg(&reference)
        .status()
        .unwrap();
    std::fs::write(
        &rollout,
        "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t9\",\"status\":\"failed\"}}}\n",
    )
    .unwrap();
    std::process::Command::new("touch")
        .arg("-r")
        .arg(&reference)
        .arg(&rollout)
        .status()
        .unwrap();
    coord.tick().expect("second tick");

    let watch = abnormal_watch(&ws, "w1");
    assert!(
        watch.to_string().contains("turn_failed"),
        "C-P1-5: size shrink with unchanged mtime must trigger a re-read and surface \
the new tail fact (turn_failed); payload={watch}"
    );
}

/// C-P1-4 (reverse case `p1_transcript_tail_64kb_finds_late_error`): with a >64KB
/// transcript whose LAST record is an explicit error, the fact must still be
/// recognized (tail window >= Python `_TAIL_BYTES` parity). Green today; locks the
/// bounded-tail fix against under-reading.
#[test]
fn p1_transcript_tail_64kb_finds_late_error() {
    let ws = tmp_ws("p1-tail");
    let rollout = ws.join("rollout-w1.jsonl");
    let pad = "{\"method\":\"turn/started\",\"params\":{\"pad\":\"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\"}}\n";
    let mut text = pad.repeat(1200); // ~100KB of benign records
    text.push_str(
        "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t1\",\"status\":\"failed\"}}}\n",
    );
    std::fs::write(&rollout, text).unwrap();
    seed_tick_state(&ws, &[("w1", Some(&rollout))]);
    let coord = coordinator(&ws, CountingTransport::new());
    coord.tick().expect("tick");

    let watch = abnormal_watch(&ws, "w1");
    assert!(
        watch.to_string().contains("turn_failed"),
        "C-P1-4: the explicit error at the END of a >64KB transcript must be \
recognized; payload={watch}"
    );
}

// ──────────────────────────── P2 · session capture bounds ───────────────────────────

/// C-P2-4 (reverse case `p2_session_capture_tail_skips_invalid_utf8_body`): a
/// candidate with a valid session_meta head and invalid UTF-8 bytes beyond 64KB must
/// still be captured. Today `read_to_string` of the WHOLE file fails on the poisoned
/// tail and silently skips the candidate (adapter.rs:783-785) — a silent miss.
#[test]
fn p2_session_capture_tail_skips_invalid_utf8_body() {
    let dir = tmp_ws("p2-utf8");
    let mut bytes = format!(
        "{{\"session_meta\":{{\"payload\":{{\"id\":\"11111111-2222-4333-8444-555555555555\",\"cwd\":\"{}\"}}}}}}\n",
        dir.to_string_lossy()
    )
    .into_bytes();
    let pad = "{\"type\":\"pad\",\"data\":\"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\"}\n".as_bytes();
    while bytes.len() < 80 * 1024 {
        bytes.extend_from_slice(pad);
    }
    bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFF]); // poisoned tail, invalid UTF-8
    std::fs::write(dir.join("rollout-w1.jsonl"), &bytes).unwrap();

    let candidates = get_adapter(Provider::Codex)
        .capture_session_candidates(&capture_context("w1", &dir), 0)
        .expect("scan should succeed");
    assert!(
        candidates.iter().any(|candidate| {
            candidate.captured.session_id.as_ref().map(|id| id.as_str())
                == Some("11111111-2222-4333-8444-555555555555")
        }),
        "C-P2-4: head-bounded read (Python claude.py:432 reads 200 lines; session_meta \
is in the file head) must capture the session despite a poisoned tail; whole-file \
read_to_string silently skips the candidate. candidates={candidates:?}"
    );
}

/// C-P2-3 (reverse case `p2_candidate_cap_300_mtime_priority`): with >300 stale decoy
/// candidates, the NEWEST valid candidate must still be found (mtime-descending
/// priority — old candidates must not crowd out new ones). Green today; locks the
/// cap-300 fix against bad ordering. (The exclusion direction — a candidate older
/// than the newest 300 being dropped — is deliberately NOT asserted: C-P2-2 allows
/// raising the cap above Python's 300.)
#[test]
fn p2_candidate_cap_300_mtime_priority() {
    let dir = tmp_ws("p2-cap");
    for index in 0..320 {
        let decoy = dir.join(format!("rollout-decoy-{index}.jsonl"));
        std::fs::write(&decoy, "not json at all\n").unwrap();
        std::process::Command::new("touch")
            .args(["-t", "202001010000", &decoy.to_string_lossy()])
            .status()
            .unwrap();
    }
    std::fs::write(
        dir.join("rollout-w1.jsonl"),
        format!(
            "{{\"session_meta\":{{\"payload\":{{\"id\":\"22222222-3333-4444-8555-666666666666\",\"cwd\":\"{}\"}}}}}}\n",
            dir.to_string_lossy()
        ),
    )
    .unwrap();

    let candidates = get_adapter(Provider::Codex)
        .capture_session_candidates(&capture_context("w1", &dir), 0)
        .expect("scan should succeed");
    assert!(
        candidates.iter().any(|candidate| {
            candidate.captured.session_id.as_ref().map(|id| id.as_str())
                == Some("22222222-3333-4444-8555-666666666666")
        }),
        "C-P2-3: the newest valid candidate must survive a >300 stale-decoy directory \
(mtime-descending candidate priority); candidates={candidates:?}"
    );
}

// ───────────────────────────── P3 · steady-state zero write ─────────────────────────

/// C-P3-4 (reverse case `p3_steady_tick_no_state_write`): a steady-state tick (no
/// messages, no changes) must not rewrite state.json. Today the tick iteration
/// counter (tick.rs:188) makes every tick dirty, defeating both save short-circuits.
#[test]
fn p3_steady_tick_no_state_write() {
    let ws = tmp_ws("p3-steady");
    seed_tick_state(&ws, &[]);
    let coord = coordinator(&ws, CountingTransport::new());

    coord.tick().expect("first tick");
    let after_first = std::fs::read(runtime_state_path(&ws)).unwrap();
    coord.tick().expect("second tick");
    let after_second = std::fs::read(runtime_state_path(&ws)).unwrap();

    assert_eq!(
        String::from_utf8_lossy(&after_first),
        String::from_utf8_lossy(&after_second),
        "C-P3-4: the second steady-state tick must be a ZERO state write — the tick \
counter (or any per-tick mutation) must not live in persistent state (N1: transient \
metric is not source-of-truth state)"
    );
}

/// C-P3-3 (reverse case `p3_upgrade_compat_old_state_tick_field`): an old state file
/// still carrying coordinator.coordinator_tick_iteration_count must load gracefully
/// and tick without errors (read-compat; new versions just stop writing it).
#[test]
fn p3_upgrade_compat_old_state_tick_field() {
    let ws = tmp_ws("p3-compat");
    save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-perf",
            "agents": {},
            "coordinator": { "coordinator_tick_iteration_count": 7 },
        }),
    )
    .unwrap();
    let loaded = load_runtime_state(&ws).expect("old state with tick counter must load");
    assert!(loaded.get("session_name").is_some());
    coordinator(&ws, CountingTransport::new())
        .tick()
        .expect("tick over an old state with the legacy counter field must succeed");
}

// ───────────────────────────── P4 · compaction event dedup ──────────────────────────

/// C-P4-1/4 (reverse case `p4_compaction_observed_dedup_no_redundant_event`): the same
/// scrollback observed twice must emit exactly ONE coordinator.compaction_observed
/// event (N35 anti-nag: events are change-driven, not per-tick heartbeats). Live
/// sample: 1037 identical events / 19.5 min today.
#[test]
fn p4_compaction_observed_dedup_no_redundant_event() {
    let ws = tmp_ws("p4-dedup");
    let mut state = json!({"session_name": "team-perf", "agents": {"w1": {"provider": "codex"}}});

    observe_runtime(&ws, &mut state, compaction_fact_map("context compacted\n"), None);
    observe_runtime(&ws, &mut state, compaction_fact_map("context compacted\n"), None);

    assert_eq!(
        count_events(&ws, "coordinator.compaction_observed"),
        1,
        "C-P4-4: an unchanged compaction count must not re-emit the event on the \
second observation (events.jsonl: {})",
        events_text(&ws)
    );
}

/// Leader refinement (anti-over-fix lock): when the compaction VALUE changes, the
/// event must still be emitted. Green today; locks the dedup fix.
#[test]
fn p4_compaction_value_change_still_emits() {
    let ws = tmp_ws("p4-change");
    let mut state = json!({"session_name": "team-perf", "agents": {"w1": {"provider": "codex"}}});

    observe_runtime(&ws, &mut state, compaction_fact_map("context compacted\n"), None);
    observe_runtime(
        &ws,
        &mut state,
        compaction_fact_map("context compacted\ncontext compacted\n"),
        None,
    );

    assert_eq!(
        count_events(&ws, "coordinator.compaction_observed"),
        2,
        "P4 reverse lock: a CHANGED compaction count must still emit (dedup must be \
value-keyed, not a blanket suppression); events={}",
        events_text(&ws)
    );
}

// ───────────────────────────── P5 · subprocess budget ───────────────────────────────

/// C-P5-5 (reverse case `p5_single_tick_list_targets_called_once`): one tick must call
/// list_targets at most once (sync_health + abnormal share one snapshot — N3 allows
/// same-tick reuse) and list_windows at most once per session (hoisted out of the
/// agent loop). Today: 2 list_targets + one list_windows per agent.
#[test]
fn p5_single_tick_list_targets_called_once() {
    let ws = tmp_ws("p5-forks");
    let rollout = ws.join("rollout-w1.jsonl");
    std::fs::write(&rollout, "{\"method\":\"turn/started\",\"params\":{}}\n").unwrap();
    seed_tick_state(&ws, &[("w1", Some(&rollout)), ("w2", None)]);
    let transport = CountingTransport::new();
    let counters = transport.counters();
    coordinator(&ws, transport).tick().expect("tick");

    let list_targets = counters.list_targets.load(Ordering::Relaxed);
    let list_windows = counters.list_windows.load(Ordering::Relaxed);
    assert!(
        list_targets <= 1 && list_windows <= 1,
        "C-P5-1/2: one tick must use a single pane snapshot (list_targets <= 1, got \
{list_targets}) and one list_windows per session (got {list_windows})"
    );
}

/// C-P5-3 (reverse case `p5_pane_pid_in_format_string_no_n_plus_one`): the tmux pane
/// list format must carry `#{pane_pid}` so pane pids come from the single list-panes
/// call instead of an N+1 display-message fallback.
#[test]
fn p5_pane_pid_in_format_string_no_n_plus_one() {
    let backend_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tmux_backend.rs"
    ))
    .unwrap();
    let format_line = backend_src
        .lines()
        .find(|line| line.contains("TMUX_PANE_FORMAT: &str"))
        .expect("TMUX_PANE_FORMAT constant must exist");
    assert!(
        format_line.contains("#{pane_pid}"),
        "C-P5-3: TMUX_PANE_FORMAT must include #{{pane_pid}} (kills the N+1 \
display-message fallback); format={format_line}"
    );
}

// ───────────────────────────── umbrella · silent second tick ────────────────────────

/// Leader's batch contract: steady-state two ticks — the second tick performs zero
/// state writes, zero new events, and stays within the single-snapshot fork budget.
/// (coordinator.result_collect is excluded: it is per-tick collection chatter outside
/// the approved P1-P5 scope — see audit P6/P7 adjudications.)
#[test]
fn perf_total_steady_state_second_tick_is_silent() {
    let ws = tmp_ws("p-total");
    let rollout = ws.join("rollout-w1.jsonl");
    std::fs::write(&rollout, "{\"method\":\"turn/started\",\"params\":{}}\n").unwrap();
    seed_tick_state(&ws, &[("w1", Some(&rollout))]);
    let transport = CountingTransport::new();
    let counters = transport.counters();
    let coord = coordinator(&ws, transport);

    coord.tick().expect("first tick");
    let state_after_first = std::fs::read(runtime_state_path(&ws)).unwrap();
    let events_after_first = noisy_event_lines(&ws);
    counters.list_targets.store(0, Ordering::Relaxed);
    counters.list_windows.store(0, Ordering::Relaxed);

    coord.tick().expect("second tick");

    let mut failures = Vec::new();
    if std::fs::read(runtime_state_path(&ws)).unwrap() != state_after_first {
        failures.push("umbrella: second steady tick rewrote state.json".to_string());
    }
    let new_events = noisy_event_lines(&ws).saturating_sub(events_after_first);
    if new_events > 0 {
        failures.push(format!(
            "umbrella: second steady tick emitted {new_events} new event(s); tail={}",
            events_text(&ws).lines().rev().take(3).collect::<Vec<_>>().join(" | ")
        ));
    }
    if counters.list_targets.load(Ordering::Relaxed) > 1 {
        failures.push(format!(
            "umbrella: second tick exceeded the single pane-snapshot budget \
(list_targets={})",
            counters.list_targets.load(Ordering::Relaxed)
        ));
    }
    assert!(
        failures.is_empty(),
        "PERF umbrella steady-state contract failed:\n{}",
        failures.join("\n")
    );
}

// ───────────────────────────── P7 · orphan self-terminate ───────────────────────────

/// PERF-7 main contract (perf7-coordinator-orphan-locate.md §4.1): the orphan
/// self-check predicate exists and is unit-locked (orphan.rs:19-25,
/// coordinator/tests/basics.rs:206-212) but `run_daemon_with_coordinator`
/// (backoff.rs:49-108) NEVER calls it — #264 silent-omission family (parts present,
/// wiring missing). A daemon whose parent died (reparented to pid 1) and whose
/// workspace was deleted must exit and write `coordinator.orphan_self_terminate`
/// (Python __main__.py:44-59). Deterministic double-hop spawn: `sh -c '... &'` exits
/// immediately, so the daemon is reparented to pid 1 by construction, not by race.
/// The state carries no truthy session_name, so the tmux-session-missing stop gate
/// never fires (matching the leaked-daemon real case).
#[test]
fn p7_orphaned_coordinator_self_terminates_after_workspace_delete() {
    // One bounded retry for the µs-scale capture race documented in run_orphan_scenario
    // (events seen in neither the per-cycle capture nor the final read).
    let mut last = run_orphan_scenario("p7-orphan-a");
    if !(last.exited && last.events.contains("coordinator.orphan_self_terminate")) {
        let retry = run_orphan_scenario("p7-orphan-b");
        if retry.exited || !last.exited {
            last = retry;
        }
    }
    let mut failures = Vec::new();
    if !last.exited {
        failures.push(
            "P7: orphaned coordinator (ppid reparented to 1, workspace deleted) must \
self-terminate within the poll window; it is still running (today: predicate never \
wired into the daemon loop)"
                .to_string(),
        );
    }
    if !last.events.contains("coordinator.orphan_self_terminate") {
        failures.push(format!(
            "P7: the exit must be announced via coordinator.orphan_self_terminate \
(Python __main__.py:44-59 literal); captured events={:?}",
            last.events.lines().rev().take(3).collect::<Vec<_>>()
        ));
    }
    assert!(
        failures.is_empty(),
        "PERF-7 orphan wiring contract failed:\n{}",
        failures.join("\n")
    );
}

struct OrphanRun {
    exited: bool,
    events: String,
}

/// Drive one orphan scenario: spawn (Python-shaped chain), wait for the reparent to
/// pid 1, then keep the workspace deleted while polling. Each cycle CAPTURES the
/// events file before deleting (the daemon recreates the dir to write the orphan
/// event right before exiting); once the marker is seen we stop deleting so the file
/// survives the final read. A µs-scale write-between-read-and-delete race remains —
/// the caller retries the whole scenario once.
fn run_orphan_scenario(tag: &str) -> OrphanRun {
    let ws = tmp_ws(tag);
    save_runtime_state(&ws, &json!({"agents": {}})).unwrap();
    let daemon = spawn_detached_daemon(&ws, &["--tick-interval", "0.1"]);
    wait_until(5_000, || ws.join(".team/runtime/coordinator.pid").exists());
    assert!(
        wait_until(5_000, || parent_pid(daemon.pid) == Some(1)),
        "fixture: daemon must reparent to pid 1 once the intermediate sh exits"
    );

    let mut captured = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let exited = loop {
        let snapshot = events_text(&ws);
        if !snapshot.is_empty() {
            captured = snapshot;
        }
        if !pid_alive(daemon.pid) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        if !captured.contains("coordinator.orphan_self_terminate") {
            let _ = std::fs::remove_dir_all(&ws);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let final_read = events_text(&ws);
    if !final_read.is_empty() {
        captured = final_read;
    }
    OrphanRun { exited, events: captured }
}

/// PERF-7 F2 (folded into the P4 dedup family per leader): repeated tick failures
/// with the SAME error signature must not each write a full coordinator.tick_error
/// event (Python __main__.py:66-89 signature dedup + `.suppressed` companions). With
/// tick-interval 0.1 a corrupt state.json produces identical failures every backoff
/// step — today every one writes a full tick_error (event flood).
#[test]
fn p7_f2_identical_tick_errors_are_deduped() {
    let ws = tmp_ws("p7-tickerr");
    let state_path = runtime_state_path(&ws);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, "{ this is not json").unwrap();
    let daemon = spawn_detached_daemon(&ws, &["--tick-interval", "0.1"]);

    // Wait until at least 3 error-cycle events exist (full or suppressed), then stop.
    wait_until(15_000, || {
        events_text(&ws)
            .lines()
            .filter(|line| line.contains("coordinator.tick_error"))
            .count()
            >= 3
    });
    let _ = std::process::Command::new("kill").arg(daemon.pid.to_string()).status();

    let full_errors = events_text(&ws)
        .lines()
        .filter(|line| {
            line.contains("\"event\":\"coordinator.tick_error\"")
                || line.contains("\"event\": \"coordinator.tick_error\"")
        })
        .count();
    assert!(
        full_errors <= 1,
        "P7-F2: identical-signature tick failures must be deduped after the first \
full coordinator.tick_error (suppressed companions allowed); got {full_errors} full \
events. events={}",
        events_text(&ws)
    );
}

struct DaemonHandle {
    pid: u32,
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        // Teardown of the test's OWN daemon (never touches any live coordinator).
        let _ = std::process::Command::new("kill").arg(self.pid.to_string()).status();
    }
}

/// Detached spawn replicating the PYTHON spawn-chain shape (leader adjudication, plan A):
/// the intermediate `sh` must OUTLIVE the daemon's birth so the daemon's initial_ppid is
/// the sh pid (NOT 1); when sh exits ~0.5s later the daemon reparents to pid 1 and the
/// literal three-condition predicate (current != initial && current == 1 && !workspace)
/// becomes satisfiable — an immediate-exit sh would give initial_ppid == 1 and make the
/// Python-literal condition永假 (clashing with the basics.rs:206-212 predicate lock).
fn spawn_detached_daemon(ws: &Path, extra: &[&str]) -> DaemonHandle {
    let bin = env!("CARGO_BIN_EXE_team-agent");
    let line = format!(
        "'{bin}' coordinator --workspace '{}' {} >/dev/null 2>&1 & echo $!; sleep 0.5",
        ws.to_string_lossy(),
        extra.join(" "),
    );
    let out = std::process::Command::new("sh")
        .args(["-c", &line])
        .output()
        .expect("spawn detached daemon");
    let pid = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .expect("daemon pid on stdout");
    DaemonHandle { pid }
}


fn parent_pid(pid: u32) -> Option<u32> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Bounded poll helper (fixture readiness / bounded assertions only).
fn wait_until(timeout_ms: u64, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

fn seed_tick_state(ws: &Path, agents: &[(&str, Option<&PathBuf>)]) {
    let mut agent_map = serde_json::Map::new();
    for (id, rollout) in agents {
        agent_map.insert(
            (*id).to_string(),
            json!({
                "status": "running",
                "provider": "codex",
                "agent_id": id,
                "window": id,
                "pane_id": format!("%9{id}"),
                "session_id": "33333333-4444-4555-8666-777777777777",
                "rollout_path": rollout.map(|p| p.to_string_lossy().to_string()),
                "spawn_cwd": ws.to_string_lossy(),
            }),
        );
    }
    save_runtime_state(
        ws,
        &json!({
            "session_name": "team-perf",
            "active_team_key": "team-perf",
            "agents": agent_map,
        }),
    )
    .unwrap();
}

fn coordinator(ws: &Path, transport: CountingTransport) -> Coordinator {
    Coordinator::new(
        WorkspacePath::new(ws.to_path_buf()),
        Box::new(RealAdapterRegistry),
        Box::new(transport),
    )
}

fn abnormal_watch(ws: &Path, agent: &str) -> Value {
    load_runtime_state(ws)
        .unwrap()
        .pointer(&format!("/coordinator/abnormal_exit_watch/{agent}"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Watch payload minus bookkeeping timestamps/keys (they legitimately move per tick).
fn stable_watch_fields(watch: &Value) -> Value {
    let Some(obj) = watch.as_object() else {
        return watch.clone();
    };
    Value::Object(
        obj.iter()
            .filter(|(key, _)| !key.ends_with("_at") && !key.ends_with("_key"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn chmod(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn capture_context(agent_id: &str, spawn_cwd: &Path) -> CaptureSessionContext {
    CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: None,
        provider_projects_root: None,
    }
}

fn compaction_fact_map(
    scrollback: &str,
) -> BTreeMap<AgentId, team_agent::coordinator::CapturedRuntimeFact> {
    let mut map = BTreeMap::new();
    map.insert(
        AgentId::new("w1"),
        team_agent::coordinator::CapturedRuntimeFact {
            team_key: None,
            agent_id: AgentId::new("w1"),
            provider: Some(Provider::Codex),
            session_name: Some(SessionName::new("team-perf")),
            window: Some(WindowName::new("w1")),
            pane_id: Some(PaneId::new("%91")),
            scrollback_tail: scrollback.to_string(),
            pane_info: None,
            agent_state_snapshot: json!({}),
            stored_session_id: None,
            last_output_at: None,
            rollout_path: None,
            process_liveness: None,
        },
    );
    map
}

fn events_text(ws: &Path) -> String {
    std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn count_events(ws: &Path, name: &str) -> usize {
    events_text(ws)
        .lines()
        .filter(|line| line.contains(&format!("\"event\":\"{name}\"")) || line.contains(&format!("\"event\": \"{name}\"")))
        .count()
}

/// Event lines excluding the per-tick collection chatter outside this batch's scope.
fn noisy_event_lines(ws: &Path) -> usize {
    events_text(ws)
        .lines()
        .filter(|line| !line.contains("coordinator.result_collect"))
        .count()
}

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }
    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists { whitelist: Vec::new(), blacklist: Vec::new() }
    }
}

#[derive(Default)]
struct TransportCounters {
    list_targets: AtomicUsize,
    list_windows: AtomicUsize,
}

#[derive(Clone)]
struct CountingTransport {
    counters: Arc<TransportCounters>,
}

impl CountingTransport {
    fn new() -> Self {
        Self { counters: Arc::new(TransportCounters::default()) }
    }

    fn counters(&self) -> Arc<TransportCounters> {
        Arc::clone(&self.counters)
    }
}

impl Transport for CountingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult {
            pane_id: PaneId::new("%1"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: "OpenAI Codex\ncodex>".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.counters.list_targets.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        self.counters.list_windows.fetch_add(1, Ordering::Relaxed);
        Ok(vec![WindowName::new("w1"), WindowName::new("w2")])
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-perf-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
