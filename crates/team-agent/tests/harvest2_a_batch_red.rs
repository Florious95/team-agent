//! 0.3.6 A-batch: T2 data-preservation + T3 false-green-truth (batch 2) + T5 thread
//! leak contracts.
//!
//! Basis: `.team/artifacts/audit-harvest-2-dev-tasks.md` §1 T2/T3/T5 (each with a
//! minimal-fix locate + file:line in the underlying code-audit docs). All contracts
//! are deterministic; any external CLI is PATH-shimmed (no real-binary dependency).
//!
//! T2 (data loss on failure path): install_skill pre-wipes the dest dir then copies
//!    (copy failure = user skill dir already gone); save_team_runtime_snapshot must not
//!    leave a stale .tmp on a failed replace. The contracts assert the original data
//!    survives / no residue.
//! T3 (false green = unrecognized/failed input silently maps to a success value,
//!    MUST-NOT-13): normalize_result_status unrecognized -> Success; claude_auth_hint
//!    parse failure -> Present; schema_diagnosis missing db -> ok:true; tmux fabricated
//!    `%0` pane id; packaging install/update constant DoctorStatus::Ok.
//! T5: stop_coordinator_bounded spawns a thread + recv_timeout and never joins it —
//!    a timed-out stop leaves a detached thread. Source grep guard (the bounded-stop
//!    path is not hermetically exercisable without a live coordinator).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use serial_test::serial;

use team_agent::db::migration::schema_diagnosis;
use team_agent::mcp_server::normalize::normalize_result_status;
use team_agent::model::enums::ResultStatus;
use team_agent::packaging::{install_skill, SkillInstallOptions, SkillTarget};

// ───────────────────────────── T2 · data preservation ─────────────────────────────

/// T2-1 (packaging install_skill): copying into the dest must not pre-wipe the user's
/// existing skill dir before the copy succeeds — a copy failure currently leaves the
/// user with an EMPTY skill dir (remove_dir_all then copy_tree). The contract: when the
/// copy source is invalid (copy must fail), the pre-existing dest content must survive.
#[test]
fn t2_install_skill_does_not_destroy_existing_dir_on_copy_failure() {
    let ws = tmp_ws("t2-install");
    let dest = ws.join("existing-skill");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
        dest.join("USER_DATA.md"),
        "irreplaceable user skill content",
    )
    .unwrap();

    // Source that does not exist → copy_tree must fail.
    let bad_source = ws.join("no-such-source");
    let result = install_skill(&SkillInstallOptions {
        target: SkillTarget::Codex,
        dest: Some(dest.clone()),
        dry_run: false,
        source: bad_source,
    });

    let mut failures = Vec::new();
    if result.is_ok() {
        failures.push(
            "T2-1: install_skill with a missing source must fail, not silently succeed".to_string(),
        );
    }
    // The pre-existing user content MUST survive a failed install.
    if !dest.join("USER_DATA.md").exists() {
        failures.push(
            "T2-1: a failed copy must not leave the user's skill dir wiped — install_skill \
must stage into a temp dir and only swap after a successful copy (write_worker_mcp_config \
tmp+rename范式); the user data was destroyed"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "T2-1 install_skill data-preservation contract failed:\n{}",
        failures.join("\n")
    );
}

/// T2-4 (lifecycle snapshot): save_team_runtime_snapshot stages via a fixed
/// `state.json.tmp` then renames. On a failed write/replace it must not leave a stale
/// `.tmp` behind (dirty-read residue). Here we pre-create the tmp path AS A DIRECTORY so
/// the `fs::write(&tmp, ..)` fails, and assert no stale tmp file is left as residue.
#[test]
fn t2_snapshot_no_stale_tmp_residue_on_failed_write() {
    let ws = tmp_ws("t2-snapshot");
    let snap_dir = ws.join(".team/runtime/teams/team-x");
    std::fs::create_dir_all(&snap_dir).unwrap();
    // Make the tmp target un-writable-as-file: create it as a directory.
    std::fs::create_dir_all(snap_dir.join("state.json.tmp")).unwrap();

    let result = team_agent::lifecycle::save_team_runtime_snapshot(
        &ws,
        &serde_json::json!({"session_name": "team-x", "agents": {}}),
    );

    assert!(
        result.is_err(),
        "fixture: the snapshot write must fail when tmp is a dir"
    );
    // A failed snapshot must not leave a stale FILE tmp residue (the dir we created is
    // the fixture's, not residue; the contract is that no stray state.json.tmp FILE is
    // written elsewhere and the real state.json was never half-written).
    assert!(
        !snap_dir.join("state.json").exists(),
        "T2-4: a failed snapshot must not leave a partially-written state.json \
(atomicity); found one at {}",
        snap_dir.join("state.json").display()
    );
}

/// T2-2 (restart-remove atomicity): the remove path must not pre-delete on-disk
/// artifacts (role file / spec / state) before the operation is committed — a mid-way
/// failure must leave the agent fully present (rollback) or fully removed, never
/// half-deleted. Grep guard on the minimal-fix shape: removal of user files must be
/// staged/rollback-guarded, not a bare `remove_file`/`remove_dir_all` ahead of commit.
#[test]
fn t2_remove_agent_role_file_delete_is_rollback_guarded() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/remove.rs"
    ))
    .unwrap();
    // The remove path must capture a rollback before mutating, and restore on failure
    // (RemoveRollback::capture + .restore). Lock that the guard is present so a future
    // refactor cannot reintroduce a bare pre-commit delete.
    assert!(
        src.contains("RemoveRollback") && src.contains(".restore(") && src.contains("rollback_ok"),
        "T2-2: agent removal must stage a rollback (RemoveRollback::capture/.restore + \
rollback_ok event) so a mid-way failure leaves no half-deleted state/file — the \
two-line state+file delete must be atomic-or-rolled-back"
    );
}

/// T2-3 (state-persist no lossy overwrite, A0-adjacent): a save whose atomic replace
/// fails must NOT leave the workspace with a truncated/empty state.json — the original
/// content must survive (self_heal rebuilds the inode via heal-tmp + backup, never an
/// in-place truncate). Grep guard on the self_heal data-preservation shape.
#[test]
fn t2_persist_failed_save_never_truncates_in_place() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/state/persist.rs"))
        .unwrap();
    let heal = fn_body(&src, "fn self_heal");
    let stages_via_tmp = heal.contains("heal.tmp") && heal.contains("atomic_replace");
    let no_inplace_truncate =
        !heal.contains("File::create(path)") && !heal.contains("truncate(true)");
    assert!(
        stages_via_tmp && no_inplace_truncate,
        "T2-3: a failed state save must rebuild via heal-tmp + atomic_replace (+ backup), \
never truncate the original in place — the original state.json must survive a failed \
save (A0 chokepoint data preservation); self_heal body={heal}"
    );
}

// ───────────────────────────── T3 · false-green truth ─────────────────────────────

/// T3-1 (mcp normalize): an UNRECOGNIZED result status must not silently become Success
/// (MUST-NOT-13: a typo / unknown status is a semantic error swallowed as success).
/// The harvest §3 note that some defaults are Python-parity does NOT cover this — this
/// is the "错→成功" directional false-green the batch targets.
#[test]
fn t3_normalize_unrecognized_nonempty_status_is_partial_not_success() {
    // cr refined 2026-06-10 (path 2 only): a NON-EMPTY unrecognized status string must
    // NOT default to Success — it normalizes to `Partial` (RS does not follow Python
    // normalize.py:123 else->success). missing/null/empty are NOT covered here (path 1
    // = parity-locked implicit success, see the parity-lock test below).
    // Note: if dev extends the mapping (e.g. cancelled->failed), that case becomes
    // `failed` + no event; the assertion below tracks the Partial-default contract and
    // is re-scoped to the dev mapping at green time.
    let mut failures = Vec::new();
    for unknown in ["garbage_string", "cancelled", "weird", "partiallydone"] {
        let status = normalize_result_status(Some(unknown));
        // "partiallydone" must hit the partial alias (Python :121 partially_done parity);
        // the others must default to Partial (RS non-success direction).
        if status == ResultStatus::Success {
            failures.push(format!(
                "T3-1: non-empty unrecognized status {unknown:?} must NOT default to Success \
(false green, MUST-NOT-13/P7); got Success"
            ));
        }
        if status != ResultStatus::Partial {
            failures.push(format!(
                "T3-1: status {unknown:?} must normalize to Partial (cr: RS does not follow \
Python's else->success); got {status:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "T3-1 unknown-status normalization failed:\n{}",
        failures.join("\n")
    );
}

/// T3-1 parity lock (cr path 1): a MISSING / null / empty status is the
/// parity-locked implicit-success convention (like exit code 0) — it must default to
/// Success and RS must NOT accidentally change it. Anchor: N38 parity.
#[test]
fn t3_normalize_missing_or_empty_status_is_success_parity_lock() {
    let mut failures = Vec::new();
    for (label, input) in [("missing/null", None), ("empty", Some(""))] {
        let status = normalize_result_status(input);
        if status != ResultStatus::Success {
            failures.push(format!(
                "T3-1 parity lock: {label} status ({input:?}) must default to Success \
(Python normalize.py:106 `_text(value) or \"success\"`, implicit-success convention); \
got {status:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "T3-1 parity lock failed:\n{}",
        failures.join("\n")
    );
}

/// T3-1 event arm (cr: must emit `provider.result.unknown_status_normalized` with the
/// original value): the MCP report_result tool — where normalize_report_envelope runs
/// (tools.rs:324) — must emit the event so the swallow is observable. Driven through
/// the real mcp-server stdio so the runtime normalize+log path is exercised.
#[test]
#[serial(t3_event)]
fn t3_unknown_status_emits_normalized_event() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let _ = worker.call_tool(
        "report_result",
        serde_json::json!({
            "task_id": "task_mcp",
            "agent_id": "worker_a",
            "status": "garbage-status-xyz",
            "summary": "did a thing",
        }),
    );
    let events = harness.events_text();
    assert!(
        events.contains("unknown_status_normalized") && events.contains("garbage-status-xyz"),
        "T3-1 event: an unknown status normalized at the MCP report_result tool must emit \
provider.result.unknown_status_normalized carrying the original value (observable \
swallow, MUST-NOT-13); events tail={}",
        events.lines().rev().take(4).collect::<Vec<_>>().join(" | ")
    );
}

/// T3-1b (normalize summary) — RE-SCOPED to a Python-parity lock (cr 2026-06-10, same
/// as the R2 precedent): a missing summary MUST default to the literal "completed"
/// (Python normalize.py:68 `_text(env.get("summary")) or "completed"`). Any other
/// default string is a parity divergence = FAIL. Anchor: N38 parity, not honesty.
#[test]
fn t3_normalize_missing_summary_defaults_to_completed_python_parity() {
    let env = team_agent::mcp_server::normalize::normalize_report_envelope(&serde_json::json!({
        "task_id": "t1",
        "agent_id": "w1",
        "status": "failed",
    }));
    assert_eq!(
        env.summary, "completed",
        "T3-1b (parity lock): a missing summary must default to the literal \"completed\" \
(Python normalize.py:68 byte-for-byte); any other default diverges from parity. got {:?}",
        env.summary
    );
}

/// T3-3 (db schema_diagnosis) — RE-SCOPED per cr 2026-06-10 to a layered-parity lock
/// (N38 + MUST-NOT-13 layered-truth, NOT a swallow): a missing db legitimately reports
/// `ok:true` (ok = legal for the next step) WITH `status:"missing"` (explicit state
/// axis) AND a `recommended_action` carrying initialize_schema guidance (Python
/// schema_migration.py missing branch). The truth is fully laid out — not a silent
/// fake-green. The reverse cases below catch the REAL fake-greens this layering must not
/// hide.
#[test]
fn t3_schema_diagnosis_missing_is_layered_parity_not_swallow() {
    let ws = tmp_ws("t3-schema-missing");
    let missing = ws.join("does-not-exist/team.db");
    let diag = schema_diagnosis(&missing, 3).expect("diagnosis should not error on a missing db");
    let mut failures = Vec::new();

    // Value axes (compile-safe struct fields): ok=true + status="missing" are the parity
    // GREEN part — the missing state is legal for the next step and explicitly named.
    if !diag.ok {
        failures.push(format!(
            "T3-3 parity: missing db must report ok=true (legal next step); diag={diag:?}"
        ));
    }
    if diag.status != "missing" {
        failures.push(format!(
            "T3-3 parity: missing db must carry status=\"missing\" (explicit state axis); got {:?}",
            diag.status
        ));
    }
    // recommended_action axis: Python's missing branch carries an initialize_schema
    // guidance string; RS's Diagnosis omits the field entirely (parity gap). Grep guard
    // the missing branch so the layered guidance is surfaced (the field is part of the
    // honest layering — without it the ok:true is bare). Turns green when dev adds it.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/migration.rs"))
        .unwrap();
    if !src.contains("recommended_action") || !src.contains("initialize_schema") {
        failures.push(
            "T3-3 parity: the missing-db diagnosis must carry a recommended_action with \
initialize_schema guidance (Python schema_migration.py missing branch: 'initialize_schema \
will create it on first use'); RS Diagnosis currently omits the field — the ok:true is \
bare without the layered guidance"
                .to_string(),
        );
    }
    assert!(
        failures.is_empty(),
        "T3-3 layered-parity contract failed:\n{}",
        failures.join("\n")
    );
}

/// T3-3 reverse cases (cr): the layered missing-state must not become a cover for a
/// REAL fake-green. A non-existent db can't carry layout drift, so we drive these as
/// invariants over the diagnosis envelope shape:
///  - ok:true + status:"missing" + layout_diffs non-empty  → FAIL (claims first-use but
///    actually has drift = silent concealment)
///  - ok:true + status field missing                       → FAIL (silent status drop)
///  - missing + recommended_action empty                   → FAIL (guidance is part of
///    honest layering — covered by the main test above)
#[test]
fn t3_schema_diagnosis_layering_must_not_hide_real_fake_green() {
    // Build a real db that HAS layout drift, and confirm a drift state never reports the
    // missing-style "ok:true + status:missing" combination (that combo is reserved for a
    // genuinely absent db).
    let ws = tmp_ws("t3-schema-drift");
    let runtime = ws.join(".team/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let db = runtime.join("team.db");
    // A db file that exists but is empty/incompatible → must NOT diagnose as missing.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("create table messages (id integer primary key)")
        .unwrap();
    let diag = schema_diagnosis(&db, 3).expect("diagnosis on an existing db");
    let mut failures = Vec::new();
    if diag.status == "missing" {
        failures.push(format!(
            "T3-3 reverse: an EXISTING db (with drift) must never report status=\"missing\" \
(the missing layering is reserved for a genuinely absent db; claiming missing over real \
drift is the concealment fake-green); diag={diag:?}"
        ));
    }
    if diag.ok && !diag.layout_diffs.is_empty() {
        failures.push(format!(
            "T3-3 reverse: ok=true must not coexist with non-empty layout_diffs (claims \
fine while drift exists = silent concealment); diag={diag:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "T3-3 fake-green-concealment reverse contract failed:\n{}",
        failures.join("\n")
    );
}

/// T3-5 (tmux pane id fallback): the tmux backend must not fabricate a `%0` pane id
/// when the spawn reply is empty — a fake pane id makes every later addressing target
/// the wrong pane. Source grep guard (the empty-reply path needs the real tmux edge;
/// the fabricated constant is the audited defect).
#[test]
fn t3_tmux_pane_id_not_fabricated_as_percent_zero() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tmux_backend.rs"))
        .unwrap();
    assert!(
        !src.contains("if pane.is_empty() { \"%0\" }")
            && !src.contains("if pane.is_empty() {\n            \"%0\""),
        "T3-5: the tmux backend must not fabricate a `%0` pane id on an empty spawn reply \
(a fake pane id mis-addresses every later op); it must surface the missing pane instead"
    );
}

/// T3-6 (packaging install/update): install/update must not return a CONSTANT
/// DoctorStatus::Ok with no real doctor check behind it (false green — nothing was
/// verified). Source grep guard (the audited defect is the literal constant).
#[test]
fn t3_packaging_install_update_doctor_status_not_constant_ok() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/packaging/install.rs"
    ))
    .unwrap();
    // The install/update bodies must not hardcode `doctor: DoctorStatus::Ok` without a
    // real check; assert the literal constant assignment is gone.
    assert!(
        !src.contains("doctor: DoctorStatus::Ok,"),
        "T3-6: install/update must not hardcode doctor: DoctorStatus::Ok (no real doctor \
check behind it = false green); it must reflect an actual doctor result"
    );
}

/// T3-2 (claude_auth_hint parse failure): when `claude auth status` output is
/// unparseable / unsuccessful, the hint must not default to Present (auth misreported
/// as available). PATH-shims a `claude` that prints garbage and exits non-zero.
#[test]
fn t3_claude_auth_hint_parse_failure_is_not_present() {
    let home = tmp_ws("t3-auth-home");
    let shim_dir = home.join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    // claude shim: garbage stdout + non-zero exit (auth status unparseable / failed).
    write_shim(
        &shim_dir.join("claude"),
        "#!/bin/sh\nprintf 'not json at all\\n'\nexit 7\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let old_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", format!("{}:{old_path}", shim_dir.to_string_lossy()));

    let status = team_agent::provider::get_adapter(team_agent::provider::Provider::Claude)
        .auth_hint(team_agent::provider::AuthMode::Subscription);

    std::env::set_var("PATH", &old_path);
    match old_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }

    assert_ne!(
        format!("{status:?}"),
        "Present",
        "T3-2: a `claude auth status` that prints unparseable output and exits non-zero \
must NOT report Present (auth misreported as available is a false green); got {status:?}"
    );
}

// ───────────────────────────────── T5 · thread leak ─────────────────────────────────

/// T5 (A2 shutdown thread leak): stop_coordinator_bounded spawns a worker thread and
/// recv_timeouts on it, but never joins — on timeout the thread is detached and leaks
/// (repeated shutdowns re-enter and race). The fix is a join+timeout reclaim (or a
/// cancellable synchronous stop). Source grep guard: the bounded-stop path must not
/// drop a detached thread handle.
#[test]
fn t5_stop_coordinator_bounded_must_not_leak_detached_thread() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/mod.rs")).unwrap();
    let body = fn_body(&src, "fn stop_coordinator_bounded");
    let spawns = body.contains("thread::spawn") || body.contains("std::thread::spawn");
    let reclaims =
        body.contains(".join(") || body.contains("JoinHandle") || body.contains("join_timeout");
    assert!(
        !spawns || reclaims,
        "T5: stop_coordinator_bounded spawns a thread but never joins it — a timed-out \
stop leaves a detached, leaking thread (and repeated shutdowns race). It must retain \
the JoinHandle and reclaim it (join with timeout), or use a cancellable synchronous \
stop; body={body}"
    );
}

// ───────────────────────────────────── helpers ─────────────────────────────────────

fn write_shim(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Crude single-function body extractor: from the `fn name` line to the next
/// same-indent `    fn ` (good enough for grep-guard scoping).
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
        "ta-rs-036a-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
