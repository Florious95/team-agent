//! PERF-6 (shutdown process-table budget) + swallow-family batch 1 (shutdown probe
//! observability) contracts.
//!
//! Basis docs (sole inputs):
//! - `.team/artifacts/perf6-shutdown-deadline-locate.md` (architect; budget table:
//!   today a happy-path shutdown forks `ps` 10-15x and sleeps R×(0.15..1.15)s serially)
//! - `.team/artifacts/perf6-cr-verdict.md` (constitution-reviewer; C-①-1..5 single
//!   snapshot, C-②-1..8 batched signals; N39 same-snapshot derivation)
//! - `.team/artifacts/swallow-family-slices.md` batch 1 (shutdown/process probes must
//!   not swallow failures: probe_failed events + degraded markers, CLAUDE.md §5)
//!
//! Real-machine grounding: the 20s ShutdownDeadline false-timeout was measured live on
//! this machine (lane-a0, load 5-7: every phase hits {"status":"timeout"}); Python
//! 0.2.11 shutdown performs ZERO ps scans (runtime.py:485-560) — the scan machinery is
//! an RS addition whose cost must be bounded by itself.
//!
//! Shape: PATH-shim `ps` counting (forks recorded to a file, then exec /bin/ps) — the
//! contract counts forks, never wall-time under load. Reaped processes are test-owned
//! `sleep`/`sh` children. No live coordinator, no real workspace is touched.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use serial_test::serial;
use team_agent::cli::lifecycle_port;
use team_agent::state::persist::save_runtime_state;

/// C-①-5 (reverse case `shutdown_full_run_ps_fork_count_le_4`): one bare shutdown must
/// fork `ps` at most K=4 times (entry snapshot 1 + residual verification 1 +
/// coordinator discover 1 + 1 headroom). Today the same table is re-fetched by
/// protection / pgids / per-root tree walks / residual rounds: 10-15 forks.
#[test]
#[serial(perf6)]
fn perf6_shutdown_ps_fork_budget_le_4() {
    let ws = tmp_ws("ps-budget");
    let _children = seed_reapable_state(&ws);
    let count_file = ws.join("ps-count.log");
    let _path_guard = install_counting_ps_shim(&ws, &count_file);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    let forks = std::fs::read_to_string(&count_file)
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        forks <= 4,
        "C-①-5: a full shutdown must fork ps <= 4 times (single entry snapshot + one \
residual-verification re-fetch + coordinator discover + headroom); got {forks} forks. \
Python 0.2.11 shutdown forks ZERO ps; the RS scan machinery must bound its own cost."
    );
}

/// Leader's loose regression bound (verdict `wallclock_low_load_under_5s`): on an idle
/// machine the whole shutdown must finish in <5s — this guards the serial per-root
/// sleep chain (R×(0.15..1.15)s lower bound), not load timing.
#[test]
#[serial(perf6)]
fn perf6_shutdown_wallclock_low_load_under_5s() {
    let ws = tmp_ws("wallclock");
    let _children = seed_reapable_state(&ws);

    let start = std::time::Instant::now();
    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "wallclock regression bound (idle machine): shutdown took {elapsed:?}; the \
batched TERM->grace->KILL rework must remove the serial per-root sleep chain \
(C-②: union TERM -> shared >=150ms grace -> union KILL -> single wait)"
    );
}

/// C-①-3/4 + `single_snapshot_consumers_dont_call_ps_independently` (static): the
/// shutdown table consumers must take the snapshot as a parameter instead of
/// re-fetching `ps` themselves. Today `reap_process_tree` walks via
/// `process_parent_pairs()` (a second full-table ps per root) and `process_pgids`
/// re-fetches `process_table()`.
#[test]
fn perf6_grep_snapshot_consumers_take_table_param() {
    let src = cli_mod_source();
    let mut failures = Vec::new();
    let pgids = fn_body(&src, "fn process_pgids");
    if pgids.contains("process_table()") {
        failures.push("C-①-4: process_pgids re-fetches process_table() instead of taking the entry snapshot".to_string());
    }
    let tree = fn_body(&src, "fn process_tree_pids");
    if tree.contains("process_parent_pairs()") {
        failures.push(
            "C-①-3: process_tree_pids re-fetches process_parent_pairs() per root — the parent \
pairs must derive from the single entry snapshot (the function is a subset of process_table)"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "PERF-6 single-snapshot grep contract failed:\n{}",
        failures.join("\n")
    );
}

/// Verdict `term_kill_sequence_preserved` (GREEN lock): the reap path must keep the
/// TERM -> grace -> KILL escalation order (Gap 37); KILL must never become first-line.
#[test]
fn perf6_grep_term_before_kill_sequence() {
    let src = cli_mod_source();
    let reap = fn_body(&src, "fn reap_process_tree");
    let term_pos = reap.find("SIGTERM");
    let kill_pos = reap.find("SIGKILL");
    assert!(
        matches!((term_pos, kill_pos), (Some(t), Some(k)) if t < k),
        "Gap 37 lock: reap_process_tree must send SIGTERM before SIGKILL (escalation, \
not first-line kill); term={term_pos:?} kill={kill_pos:?}"
    );
}

/// C-②-6 (GREEN lock `slow_cleanup_process_survives_grace_window`): a process that
/// exits gracefully ~100ms after SIGTERM must finish inside the >=150ms grace window
/// and never be SIGKILLed. Observable: the TERM handler writes a marker file before
/// exiting — a SIGKILL inside the grace window would preempt the write. (The process
/// is double-hop detached: in-process shutdown waitpid()s its victims, so a direct
/// child's exit status is consumed before the test could read it.)
#[test]
#[serial(perf6)]
fn perf6_grace_window_slow_cleanup_not_killed() {
    let ws = tmp_ws("grace");
    let marker = ws.join("graceful-exit.marker");
    let pid = spawn_detached_trap_loop(&ws, &format!(
        "trap 'sleep 0.1; echo graceful > {}; exit 0' TERM; while :; do sleep 0.05; done",
        marker.to_string_lossy()
    ));
    seed_state_with_pids(&ws, &[pid]);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    assert!(
        wait_until(3_000, || marker.exists()),
        "C-②-6: the TERM handler (100ms graceful exit) must complete inside the \
>=150ms grace window without being preempted by SIGKILL; marker file missing"
    );
}

/// C-②-8 (GREEN lock `mixed_fast_slow_pid_get_correct_kill`): a fast-exiting pid and
/// a TERM-ignoring pid in the same shutdown — the stubborn one must be escalated to
/// SIGKILL and be gone; the fast one exits on TERM (marker written).
#[test]
#[serial(perf6)]
fn perf6_mixed_fast_slow_kill_semantics() {
    let ws = tmp_ws("mixed");
    let fast_marker = ws.join("fast-exit.marker");
    let fast = spawn_detached_trap_loop(&ws, &format!(
        "trap 'echo fast > {}; exit 0' TERM; while :; do sleep 0.02; done",
        fast_marker.to_string_lossy()
    ));
    let stubborn = spawn_detached_trap_loop(&ws, "trap '' TERM; while :; do sleep 0.05; done");
    seed_state_with_pids(&ws, &[fast, stubborn]);

    lifecycle_port::shutdown(&ws, false, None).expect("bare shutdown should succeed");

    let mut failures = Vec::new();
    if !wait_until(3_000, || fast_marker.exists()) {
        failures.push("C-②-8: the fast pid must exit via its TERM handler (marker missing)".to_string());
    }
    if !wait_until(3_000, || !pid_alive(stubborn)) {
        failures.push(format!(
            "C-②-8: the TERM-ignoring pid {stubborn} must be escalated to SIGKILL and be gone"
        ));
        let _ = std::process::Command::new("kill").args(["-9", &stubborn.to_string()]).status();
    }
    assert!(
        failures.is_empty(),
        "C-②-8 mixed kill semantics failed:\n{}",
        failures.join("\n")
    );
}

/// Swallow batch 1 family contract: when the `ps` probe itself FAILS, shutdown must
/// not silently pretend "no processes" — the failure must be observable (a
/// `*probe_failed*` event with a non-null error) and the result must carry a degraded
/// marker instead of a clean fake-green (CLAUDE.md §5: any "looks executed but had no
/// effect" needs a log that explains why).
#[test]
#[serial(perf6)]
fn swallow1_broken_ps_probe_is_observable_not_silent() {
    let ws = tmp_ws("broken-ps");
    let _children = seed_reapable_state(&ws);
    let _path_guard = install_failing_ps_shim(&ws);

    let result = lifecycle_port::shutdown(&ws, false, None)
        .expect("shutdown should still complete in degraded mode");

    let events = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap_or_default();
    let mut failures = Vec::new();
    let probe_event = events
        .lines()
        .find(|line| line.contains("probe_failed"));
    match probe_event {
        None => failures.push(
            "batch1: ps failure produced NO probe_failed event — the empty process table \
silently masquerades as 'no processes' (cli/mod.rs:738-740 `_ => return Vec::new()`)"
                .to_string(),
        ),
        Some(line) => {
            if line.contains("\"error\":null") || line.contains("\"error\": null") {
                failures.push(format!(
                    "batch1: probe_failed event must carry a non-null error field; line={line}"
                ));
            }
        }
    }
    let result_text = result.to_string();
    if !result_text.contains("degraded") {
        failures.push(format!(
            "batch1: the shutdown result must carry a probe_degraded marker instead of a \
clean fake-green; result={result_text}"
        ));
    }
    assert!(
        failures.is_empty(),
        "swallow batch1 probe-observability contract failed:\n{}",
        failures.join("\n")
    );
}

// ───────────────────────────────────── fixtures ─────────────────────────────────────

struct ChildGuard(Vec<std::process::Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// State with two reapable test-owned `sleep` children registered as pane pids
/// (the real-machine agent row shape: status/provider/window/pane_pid/spawn_cwd).
fn seed_reapable_state(ws: &Path) -> ChildGuard {
    let a = std::process::Command::new("sleep").arg("300").spawn().unwrap();
    let b = std::process::Command::new("sleep").arg("300").spawn().unwrap();
    seed_state_with_pids(ws, &[a.id(), b.id()]);
    ChildGuard(vec![a, b])
}

fn seed_state_with_pids(ws: &Path, pids: &[u32]) {
    let mut agents = serde_json::Map::new();
    for (index, pid) in pids.iter().enumerate() {
        let id = format!("w{index}");
        let rollout = ws.join(format!("rollout-{id}.jsonl"));
        std::fs::write(&rollout, "{\"method\":\"turn/started\",\"params\":{}}\n").unwrap();
        agents.insert(
            id.clone(),
            json!({
                "status": "running",
                "provider": "codex",
                "agent_id": id,
                "window": id,
                "pane_pid": pid,
                "spawn_cwd": ws.to_string_lossy(),
                // A captured session keeps shutdown's refresh_provider_sessions from
                // scanning the provider home (the unbounded P2 read would eat the
                // whole 20s deadline before the phases under test even start).
                "session_id": format!("44444444-5555-4666-8777-88888888888{index}"),
                "rollout_path": rollout.to_string_lossy(),
                "captured_via": "fs_watch",
                "attribution_confidence": "high",
            }),
        );
    }
    save_runtime_state(
        ws,
        &json!({
            "session_name": "team-perf6",
            "active_team_key": "team-perf6",
            "agents": agents,
        }),
    )
    .unwrap();
}

struct PathGuard {
    old_path: String,
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.old_path);
        std::env::remove_var("PS_COUNT_FILE");
    }
}

fn install_shim(ws: &Path, script: &str) -> PathGuard {
    let shim_dir = ws.join("shim-bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(shim_dir.join("ps"), script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(shim_dir.join("ps"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", shim_dir.to_string_lossy(), old_path));
    PathGuard { old_path }
}

/// `ps` shim that records each fork and then behaves like the real ps.
fn install_counting_ps_shim(ws: &Path, count_file: &Path) -> PathGuard {
    std::env::set_var("PS_COUNT_FILE", count_file);
    install_shim(
        ws,
        "#!/bin/sh\necho fork >> \"$PS_COUNT_FILE\"\nexec /bin/ps \"$@\"\n",
    )
}

/// `ps` shim that always fails (probe outage injection).
fn install_failing_ps_shim(ws: &Path) -> PathGuard {
    install_shim(ws, "#!/bin/sh\necho 'ps: injected probe failure' >&2\nexit 1\n")
}



/// Double-hop detach (`sh -c '... & echo $!'`): the loop process is NOT a child of the
/// test process, so the in-process shutdown's waitpid cannot consume its status and
/// `kill -0` observation stays valid.
fn spawn_detached_trap_loop(ws: &Path, body: &str) -> u32 {
    let line = format!("sh -c '{body}' >/dev/null 2>&1 & echo $!");
    let out = std::process::Command::new("sh")
        .args(["-c", &line])
        .current_dir(ws)
        .output()
        .expect("spawn detached loop");
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().expect("pid")
}

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn wait_until(timeout_ms: u64, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn cli_mod_source() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/mod.rs")).unwrap()
}

/// Crude single-function extractor: from the `fn name` line to the next top-level
/// `    fn ` at the same indentation (good enough for grep-guard scoping).
fn fn_body(src: &str, marker: &str) -> String {
    let Some(start) = src.find(marker) else {
        return String::new();
    };
    let rest = &src[start..];
    let end = rest[marker.len()..]
        .find("\n    fn ")
        .map(|offset| offset + marker.len())
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-perf6-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
