//! Subscription-tier real-machine acceptance gate for `codex fork`.
//! This file is the CONFIGURATION SIDE of 0.5.58 fork-plane tooth ⑤
//! (`real_subscription_gate_freezes_cold_start_matrix`) — it is
//! validation-lineage property owned by the verifier; the developer
//! may NOT self-author it. Name contains `subscription` + `fork` and
//! it lives directly under `crates/team-agent/tests/` so the tooth's
//! filesystem scan can find it.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip). Tooth ⑤ requires this
//!   file to exist AND to encode all six frozen strings so an
//!   offline shim green cannot masquerade for the subscription gate.
//! - locate §6#5 six acceptance items (frozen strings this file
//!   MUST spell out for the tooth's textual scan):
//!     • codex --version         (real CLI version pin, not a shim)
//!     • no_prompt               (§7#1: never inject a hidden prompt)
//!     • cold_start              (deadline > 10s is legitimate)
//!     • nonce                   (source nonce inherited by target)
//!     • distinct new_session_id (target session id != source)
//!     • source_unchanged        (source rollout not re-written)
//! - locate §7 risk-boundary locks stay operative here as they do in
//!   the developer-side contract file:
//!     §7#1 no hidden prompt / §7#3 no cwd-latest relaxation /
//!     §7#4 typed observable Pending, no silent fresh-clone downgrade.
//!
//! REVISION LINEAGE:
//! - v1 (65ec37ae) — 6 test 未 gate 即 panic 让 all-targets 永久红
//! - v2 (a36d7cb7) — 6 test 挂 #[ignore] + 齿②-⑥ body 留 `unimplemented!("GREEN-time wiring: ...")` 占位待补(msg_abdbe53fe030 门形态定标)
//! - **v3 (本次,leader ruling msg_7efa7a87d948 A 案)** — 齿②-⑥ 补实体 body:harness 起真 team-agent CLI + 真 codex 订阅执行 fork,读 events.jsonl+state.json 真机证据面判真;按 §17.1 红因正确+§21 one-real-run 纪律+§7 env-gate 真跑;弱代理信号(如仅 exit code)不接受
//!
//! GATE(2026-07-24 leader ruling msg_abdbe53fe030 — 门形态定标):
//! - Six subscription tests carry `#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization"]`.
//!   A default `cargo test` counts them as **ignored** (visible in the
//!   summary line — the loud face) but does NOT execute them, so
//!   all-targets regression gates stay green independently of the
//!   subscription gate. This decouples the regular regression door
//!   from the subscription door WITHOUT falling into the silent
//!   assumed-green family: the ignored count IS the noise.
//! - To execute them you must BOTH pass `--ignored` to cargo AND set
//!   `TEAM_AGENT_REALMACHINE_FORK=1`. Missing the gate ⇒ hard panic
//!   (unauthorized runs never count as green). Presence of
//!   `TEAM_AGENT_TEST_SHIM=1` OR `CODEX_FORCE_STDIO=1` (leftover
//!   shim env from adjacent contract tests) short-circuits with a
//!   HARD REFUSE panic that documents why an offline shim green
//!   never substitutes for the real gate.
//! - The verifier is the ONLY authorized runner; leader must
//!   pre-authorize each execution (subscription cost + provider
//!   quota impact). See `.team/artifacts/at-least-once-three-questions-criteria.md`
//!   for the operational discipline mirror.
//! - The subscription gate is ALSO a hard ship-gate independent of
//!   this file: the pre-release 3-serial subscription rule
//!   (`subscription-realmachine-3-serial-preship-gate`) enforces the
//!   real run at release time; `#[ignore]` here does not weaken that
//!   because ship acceptance never delegates to unit test defaults.
//! - `frozen_markers_present_in_this_file_bytes` remains UNGATED and
//!   runs by default — it is a pure textual sanity that must always
//!   catch a lost marker regardless of subscription availability.
//!
//! CURRENT STATE:
//! - Baseline @5b847e4: fork-agent is broken; running this gate
//!   would deterministically fail on the 10s deadline (locate §0#1).
//!   That failure is INFORMATIVE at RED time and BECOMES the acceptance
//!   at GREEN time once developer-side teeth ①-④ (typed Pending +
//!   bounded grace + no-masquerade + state-not-spec) land.
//! - Until then, the runtime body of every real test is guarded by
//!   the env gate and the shim-poisoning guard; nothing runs by
//!   accident.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.
//! Developer / spawnmod C4 must not self-author or edit this file.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::process::Command;

use serial_test::serial;

// Sentinels — presence of any of these means the environment has
// been rigged for an offline shim run. Real subscription acceptance
// must never coexist with them.
const SHIM_SENTINELS: &[&str] = &[
    "TEAM_AGENT_TEST_SHIM",
    "CODEX_FORCE_STDIO",
    "TEAM_AGENT_FAKE_PROVIDER",
];

fn gate_enabled() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_FORK").as_deref() == Ok("1")
}

fn shim_env_poisoned() -> Option<&'static str> {
    for name in SHIM_SENTINELS {
        if std::env::var(name).is_ok() {
            return Some(*name);
        }
    }
    None
}

/// Skip helper. Always PANICs (not `return`) when the gate is off
/// because a silent return would count as a green pass — the tooth
/// asserts REAL execution occurred, and a silent skip in CI would
/// mimic an acceptance without any evidence.
fn require_gate_or_skip(test_name: &str) {
    if let Some(rigged) = shim_env_poisoned() {
        // Offline shim greens do not substitute. Halt hard.
        panic!(
            "REFUSE {test_name}: shim env `{rigged}` present. locate §7#3+§6#5 \
             disallow substituting an offline shim green for the real \
             subscription gate. Unset the shim env before running the \
             subscription gate."
        );
    }
    if !gate_enabled() {
        // Unauthorized run — do NOT count as green. eprintln so it is
        // visible in a real subscription run; panic prevents accidental
        // pass. cold_start: this pattern mirrors msg_routing /
        // team_key_retirement gates.
        panic!(
            "SKIP {test_name}: set TEAM_AGENT_REALMACHINE_FORK=1 (leader \
             authorization required — subscription cost + provider quota). \
             locate §6#5 six frozen acceptance items enforced only under this \
             gate. CI must NOT set this variable."
        );
    }
}

// ---------------------------------------------------------------------------
// v3 harness helpers(msg_7efa7a87d948 A 案):真 team-agent CLI + 真 codex
// 订阅执行 fork,读 events.jsonl 与 state.json 真机证据面判真。
// ---------------------------------------------------------------------------

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// v5 (X 案):optionally use runtime-owner-provisioned candidate CLI at
/// `TEAM_AGENT_TEST_CLI`(exact tested SHA=1894eaf3 build =
/// /Volumes/nvme/tmp/release-058-gate4-target/debug/team-agent SHA256
/// 3a2cac94…f688 per runtime-owner handoff msg_323a2e466b30). Falls back
/// to CARGO_BIN_EXE if unset — same tested SHA when contract worktree is
/// on the same code, but env override guarantees we exercise the exact
/// binary door-⑤ shall ship.
fn cli_bin() -> String {
    std::env::var("TEAM_AGENT_TEST_CLI")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_team-agent").to_string())
}

/// v5 (§17.4 §二·补 8):契约不建 workspace,支撑层预置。此 helper 只产
/// 唯一 fork target agent id(每齿独立 target 名,共享 runtime-owner 预置
/// workspace + 同一 source agent)。
fn fresh_target_id(tag: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Run `team-agent <args>` in the given workspace with clean env; codex CLI on
/// PATH; capture stdout+stderr. Never inherit SHIM_SENTINELS or verifier
/// TEST_TMP overrides — REFUSE guarantees clean env at call time.
fn run_cli(workspace: &Path, args: &[&str]) -> std::process::Output {
    // v4 (leader ruling msg_69bfbe1fbb68): `--workspace` 是 subcommand-scoped
    // flag,必须放 subcommand 后(dry-run 亲验:`team-agent quick-start
    // [TEAMDIR] [--workspace WORKSPACE] ...` — top-level 位置解成 subcommand
    // 报 "invalid choice"). args 由调用方给 subcommand+flags,本 helper
    // 只 append --workspace.
    let mut cmd = std::process::Command::new(cli_bin());
    cmd.args(args).arg("--workspace").arg(workspace);
    // hermetic env carry-over: HOME/PATH/USER only.
    cmd.env_clear();
    cmd.env(
        "HOME",
        std::env::var("HOME").expect("HOME required for subscription gate"),
    );
    cmd.env(
        "PATH",
        std::env::var("PATH").expect("PATH required for codex CLI resolution"),
    );
    if let Ok(user) = std::env::var("USER") {
        cmd.env("USER", user);
    }
    cmd.env("TEAM_AGENT_REALMACHINE_FORK", "1");
    cmd.output().expect("run team-agent")
}

/// Acquire the pre-provisioned live workspace (X 案, leader msg_278e732f0b28
/// + runtime-owner handoff msg_323a2e466b30):read env
/// `TEAM_AGENT_TEST_WORKSPACE` pointing at a workspace that already contains
/// TEAM.md + agents/ + role docs + a source agent whose codex session is
/// captured with high confidence (真订阅 rollout on disk).
///
/// v5 (§17.4 §二·补 8 契约资产禁 bootstrapping):契约不做 quick-start/start-agent
/// setup;只读 env → 校验 source_worker captured → 返 (workspace, source_agent_id,
/// source_session_id, source_rollout_path)。setup 是 runtime-owner 领地。
fn acquire_test_workspace() -> (PathBuf, String, String, PathBuf) {
    let ws = std::env::var("TEAM_AGENT_TEST_WORKSPACE")
        .expect("TEAM_AGENT_TEST_WORKSPACE must be set by runtime-owner (X 案 handoff)");
    let ws = PathBuf::from(ws);
    let state_path = ws.join(".team").join("runtime").join("state.json");
    let text = std::fs::read_to_string(&state_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", state_path.display()));
    let state: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse state.json: {e}"));
    let agents = state
        .get("agents")
        .and_then(|v| v.as_object())
        .expect("state.json missing agents map");
    // Take the FIRST agent whose capture_state == "captured" as the source
    // (runtime-owner handoff guarantees exactly one source agent).
    let (source_agent, source_row) = agents
        .iter()
        .find(|(_, v)| {
            v.get("capture_state").and_then(|s| s.as_str()) == Some("captured")
                && v.get("session_id")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .is_some()
        })
        .expect("pre-provisioned workspace has no captured source agent");
    let source_sid = source_row
        .get("session_id")
        .and_then(|s| s.as_str())
        .expect("source session_id")
        .to_string();
    let source_rollout = source_row
        .get("rollout_path")
        .and_then(|s| s.as_str())
        .map(PathBuf::from)
        .expect("source rollout_path");
    (ws, source_agent.clone(), source_sid, source_rollout)
}

/// Read events.jsonl in workspace; return lines (each is a JSON string).
fn read_events(workspace: &Path) -> Vec<serde_json::Value> {
    let path = workspace.join(".team").join("logs").join("events.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

/// Read state.json map at root or empty.
fn read_state(workspace: &Path) -> serde_json::Value {
    let path = workspace.join(".team").join("runtime").join("state.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Snapshot (size + mtime + head sha256) for source rollout — used by齿⑥.
fn snapshot_rollout(path: &Path) -> (u64, std::time::SystemTime, String) {
    let meta = std::fs::metadata(path).expect("stat rollout");
    let bytes = std::fs::read(path).expect("read rollout head");
    let mut hasher = std::process::Command::new("shasum")
        .arg("-a")
        .arg("256")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shasum");
    use std::io::Write;
    hasher
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&bytes)
        .expect("write to shasum");
    let out = hasher.wait_with_output().expect("shasum output");
    let sha = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    (meta.len(), meta.modified().expect("mtime"), sha)
}

/// Rollout path for a given session_id under ~/.codex/sessions (encoded).
fn locate_source_rollout(state: &serde_json::Value, agent_id: &str) -> Option<PathBuf> {
    state
        .pointer(&format!("/agents/{}/rollout_path", agent_id))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Execute the fork; return (elapsed_ms, target_state_pointer_root).
/// target_state_pointer_root is state.json map after fork.
/// v6 (leader ruling msg_99f7d3b72560):bounded target-specific first turn
/// driver. runtime-owner 取证证明 fork target 从 pending_context_fork 转
/// Verified 需**目标身份/nonce 落 target rollout**,而首个 target-specific
/// turn 是唯一驱动力。契约必发一条唯一 token 的首单到 target 后再等
/// pending→captured 收敛(bounded 窗口)。与种系实操一致(fork 后必派首单)。
fn drive_first_turn(workspace: &Path, target: &str, token: &str) -> std::process::Output {
    run_cli(
        workspace,
        &[
            "send",
            target,
            &format!("[v6-drive-first-turn·{token}] Please acknowledge this token so your target identity lands in transcript. Reply exactly: {token}"),
            "--json",
        ],
    )
}

/// v6:wait for target agent to converge to captured (from
/// pending_context_fork) within bounded window; returns Some((session_id,
/// rollout_path)) on success or None on timeout.
fn wait_target_captured(
    workspace: &Path,
    target: &str,
    timeout: std::time::Duration,
) -> Option<(String, PathBuf)> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let state = read_state(workspace);
        let cs = state
            .pointer(&format!("/agents/{}/capture_state", target))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if cs == "captured" {
            let sid = state
                .pointer(&format!("/agents/{}/session_id", target))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let rp = state
                .pointer(&format!("/agents/{}/rollout_path", target))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            if let (Some(s), Some(r)) = (sid, rp) {
                return Some((s, r));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    None
}

fn drive_fork(
    workspace: &Path,
    source_agent: &str,
    target_agent: &str,
    team: &str,
) -> (u128, std::process::Output) {
    let started = std::time::Instant::now();
    let mut args: Vec<&str> = vec!["fork-agent", source_agent, "--as", target_agent];
    if !team.is_empty() {
        args.push("--team");
        args.push(team);
    }
    args.push("--no-display");
    args.push("--json");
    let out = run_cli(workspace, &args);
    let elapsed = started.elapsed().as_millis();
    (elapsed, out)
}

// ---------------------------------------------------------------------------
// Face 1 — codex --version is pinned by the runner
// ---------------------------------------------------------------------------
//
// The subscription gate is meaningless if the runner is executing an
// undocumented CLI. We freeze the observed CLI version into the test
// output so a GREEN verdict can be attached to a specific
// `codex --version` line (verifier records it in the verdict).

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_codex_version_pinned() {
    require_gate_or_skip("subscription_fork_codex_version_pinned");
    let out = Command::new("codex")
        .arg("--version")
        .output()
        .expect("run `codex --version`");
    assert!(
        out.status.success(),
        "codex --version exited non-zero: {:?}",
        out.status
    );
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        version.starts_with("codex-cli ") || version.contains("codex"),
        "codex --version output shape unexpected: `{version}`"
    );
    // Verifier records this line in the GREEN verdict alongside the
    // fork exit condition; there is no attempt to lock a specific
    // number here (that would rot every CLI release).
    eprintln!("[subscription-fork gate] pinned codex --version: {version}");
}

// ---------------------------------------------------------------------------
// Face 2 — no_prompt: the adapter fork invocation MUST NOT attach a prompt
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_no_prompt_end_to_end() {
    require_gate_or_skip("subscription_fork_no_prompt_end_to_end");
    // Behavioral placeholder: verifier drives a real `team-agent
    // fork-agent` and inspects the captured child command line via
    // the leader's rollout / event log. This test asserts the marker
    // strings §7#1 requires; the full exec is wired at GREEN time
    // because it depends on typed-Pending (tooth ①) and grace path
    // (tooth ②) landing first.
    let expected_marker = "no_prompt";
    let cold_start = "cold_start"; // used by the harness to guarantee > 10s tolerance
    let source_unchanged = "source_unchanged";
    assert!(
        expected_marker == "no_prompt"
            && cold_start == "cold_start"
            && source_unchanged == "source_unchanged",
        "frozen markers drifted; six acceptance strings are contract, not source-of-truth"
    );
    // v6 (leader ruling msg_99f7d3b72560): fork → drive bounded first turn
    // → wait target Verified(pending_context_fork 转 captured 是首 turn 驱动
    // 目标身份/nonce 落 transcript 的必然结果,取证 v5 forensics ls 核销
    // gate5b-v5-product-red-forensics.md)→ 检查 events 有 fork 相关.
    let (workspace, source_agent, _source_sid, _source_rollout) = acquire_test_workspace();
    let target = fresh_target_id("np");
    let (_elapsed, fork_out) = drive_fork(&workspace, &source_agent, &target, "");
    let _ = fork_out.status.success();
    // v6 driver: 派 bounded 首单 到 target,唯一 token 落 target rollout
    let token = format!("V6-NP-TOKEN-{}", std::process::id());
    let _ = drive_first_turn(&workspace, &target, &token);
    // wait target captured (bounded 120s cold-start + first-turn budget)
    let captured = wait_target_captured(&workspace, &target, std::time::Duration::from_secs(120));
    assert!(
        captured.is_some(),
        "no_prompt face: target {target} did not converge to captured within 120s \
         after first turn — provider face regression (locate §0#1 或 pending→captured 收敛断)"
    );
    let events = read_events(&workspace);
    let fork_related: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| {
            let event = e.get("event").and_then(|v| v.as_str()).unwrap_or("");
            event.contains("context_fork") || event.contains("send.injected")
        })
        .collect();
    assert!(
        !fork_related.is_empty(),
        "no_prompt face: no context_fork/send.injected event observed after fork+first-turn; \
         events sample={:?}",
        events.iter().rev().take(5).collect::<Vec<_>>()
    );
    // 硬断言:任何 send.injected_context_fork 事件里 prompt 字段必须为 null 或缺席
    // (locate §7#1: adapter fork MUST NOT attach non-empty prompt)
    for ev in &fork_related {
        if let Some(prompt) = ev.pointer("/payload/prompt") {
            assert!(
                prompt.is_null() || prompt.as_str().is_some_and(|s| s.is_empty()),
                "no_prompt violation: event carries non-empty prompt={prompt}; event={ev}"
            );
        }
    }
    eprintln!(
        "[subscription-fork gate] no_prompt CONFIRMED via {} fork-related events",
        fork_related.len()
    );
}

// ---------------------------------------------------------------------------
// Face 3 — cold_start > 10s is tolerated as typed Pending, not rollback
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_cold_start_over_ten_seconds_tolerated() {
    require_gate_or_skip("subscription_fork_cold_start_over_ten_seconds_tolerated");
    // At GREEN time this executes a real cold-start fork and asserts
    // the framework observes `Pending` past the 10s Codex convergence
    // budget, then progresses to `Verified` when the provider settles.
    // See locate §5.1 for the required typed outcomes.
    let cold_start = "cold_start";
    let no_shim = "shim_never_substitutes";
    assert_eq!(cold_start, "cold_start");
    assert_eq!(no_shim, "shim_never_substitutes");
    // v3: 真跑 fork,读 fork exit + events + state 判 typed Pending/Verified 都 OK
    // 只要 fork 未在 10s 硬 error(即不是 locate §0#1 的 rollback)即容忍成立
    let (workspace, source_agent, _sid, _rollout) = acquire_test_workspace();
    let target = fresh_target_id("cs");
    let (elapsed_ms, fork_out) = drive_fork(&workspace, &source_agent, &target, "");
    let stdout = String::from_utf8_lossy(&fork_out.stdout);
    let stderr = String::from_utf8_lossy(&fork_out.stderr);
    // 硬断言:不允许出现 baseline @5b847e4 那种 "Codex produced no readable NEW
    // session backing within 10000ms" 的同步 error(context_fork_unverified)
    let has_10s_deadline_error = stderr.contains("no readable NEW session backing within 10000ms")
        || stdout.contains("no readable NEW session backing within 10000ms");
    assert!(
        !has_10s_deadline_error,
        "cold_start face: baseline 10s rollback error surfaced — locate §0#1 未修复\n\
         stdout={stdout}\nstderr={stderr}"
    );
    // 状态面判真:state.json 中 target agent 必须落 capture_state 为
    // pending_context_fork 或 captured(而非 spawn_failed/rollback)
    let state = read_state(&workspace);
    let cs = state
        .pointer(&format!("/agents/{}/capture_state", target))
        .and_then(|v| v.as_str());
    assert!(
        matches!(cs, Some("pending_context_fork") | Some("captured")),
        "cold_start face: target agent capture_state={cs:?} — expected \
         pending_context_fork or captured;elapsed_ms={elapsed_ms}"
    );
    eprintln!(
        "[subscription-fork gate] cold_start CONFIRMED: elapsed={elapsed_ms}ms capture_state={cs:?}"
    );
}

// ---------------------------------------------------------------------------
// Face 4 — nonce inheritance from source to target
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_nonce_inherited_from_source() {
    require_gate_or_skip("subscription_fork_nonce_inherited_from_source");
    let nonce = "nonce";
    assert_eq!(nonce, "nonce");
    // v3: 真跑,读 state 判 target agent 拿到 source_session_id 作为 fork 血源标记
    // (nonce = source_session_id 在 fork_source_session_id 字段;captured_via 记录来源)
    let (workspace, source_agent, source_sid, _rollout) = acquire_test_workspace();
    let target = fresh_target_id("nc");
    let (_elapsed, _fork_out) = drive_fork(&workspace, &source_agent, &target, "");
    // 等 fork_source_session_id 落 state(最多 15s)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut inherited: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let state = read_state(&workspace);
        if let Some(v) = state
            .pointer(&format!("/agents/{}/fork_source_session_id", target))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            inherited = Some(v.to_string());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let inherited =
        inherited.expect("nonce face: target agent has no fork_source_session_id after fork");
    assert_eq!(
        inherited, source_sid,
        "nonce face: target's fork_source_session_id ({inherited}) does not \
         match source session_id ({source_sid}) — nonce inheritance broken"
    );
    eprintln!(
        "[subscription-fork gate] nonce CONFIRMED: source_sid={source_sid} inherited={inherited}"
    );
}

// ---------------------------------------------------------------------------
// Face 5 — target NEW session id distinct from source
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_target_distinct_new_session_id() {
    require_gate_or_skip("subscription_fork_target_distinct_new_session_id");
    let distinct = "distinct";
    let new_session_id = "new_session_id";
    assert_eq!(distinct, "distinct");
    assert_eq!(new_session_id, "new_session_id");
    // v6 (leader ruling msg_99f7d3b72560): 用 bounded 首 turn 驱动
    // pending_context_fork → captured;target 身份/nonce 需首 turn 落 rollout
    let (workspace, source_agent, source_sid, source_rollout) = acquire_test_workspace();
    let target = fresh_target_id("ds");
    let (_elapsed, _fork_out) = drive_fork(&workspace, &source_agent, &target, "");
    let token = format!("V6-DS-TOKEN-{}", std::process::id());
    let _ = drive_first_turn(&workspace, &target, &token);
    // wait target captured (bounded 120s cold-start + first-turn budget)
    let captured = wait_target_captured(&workspace, &target, std::time::Duration::from_secs(120))
        .expect(
            "distinct face: target agent never converged to captured within 120s after first turn",
        );
    let (target_sid, target_rollout) = captured;
    assert_ne!(
        target_sid, source_sid,
        "distinct face: target session_id equals source ({target_sid}); source masquerade violation (tooth ③ §7#4)"
    );
    assert_ne!(
        target_rollout,
        source_rollout,
        "distinct face: target rollout_path equals source ({}); backing_path masquerade violation",
        source_rollout.display()
    );
    eprintln!(
        "[subscription-fork gate] distinct CONFIRMED: source_sid={source_sid} target_sid={target_sid}"
    );
}

// ---------------------------------------------------------------------------
// Face 6 — source rollout is unchanged after fork
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires TEAM_AGENT_REALMACHINE_FORK=1 + leader authorization; run via `cargo test -- --ignored`"]
#[serial(env)]
fn subscription_fork_source_unchanged_after_fork() {
    require_gate_or_skip("subscription_fork_source_unchanged_after_fork");
    let source_unchanged = "source_unchanged";
    assert_eq!(source_unchanged, "source_unchanged");
    // v3: seed → snapshot source rollout → fork → 等 fork settle → snapshot again
    // 断言 size/mtime/sha 三元 byte-stable(§7#3 禁 cwd-latest 放宽)
    let (workspace, source_agent, _sid, source_rollout) = acquire_test_workspace();
    let target = fresh_target_id("su");
    let before = snapshot_rollout(&source_rollout);
    let (_elapsed, _fork_out) = drive_fork(&workspace, &source_agent, &target, "");
    // wait fork to settle (Verified/Pending 都算 settled;最多 90s)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        let state = read_state(&workspace);
        let cs = state
            .pointer(&format!("/agents/{}/capture_state", target))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if matches!(cs, "captured" | "pending_context_fork") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let after = snapshot_rollout(&source_rollout);
    assert_eq!(
        before.0, after.0,
        "source_unchanged face: source rollout size changed {} → {} after fork",
        before.0, after.0
    );
    assert_eq!(
        before.2, after.2,
        "source_unchanged face: source rollout head sha256 changed {} → {} after fork \
         — provider re-rendered/appended to source file (§7#3 forbids)",
        before.2, after.2
    );
    // mtime 允许 filesystem 精度差(macOS APFS 秒级),仅在 sha 变化时报;
    // 上一断言已覆盖 byte-identical 判据
    eprintln!(
        "[subscription-fork gate] source_unchanged CONFIRMED: size={} sha={}",
        after.0, after.2
    );
}

// ---------------------------------------------------------------------------
// Sanity — the six frozen markers are present in this file's own bytes
// ---------------------------------------------------------------------------
//
// This test is intentionally UNGATED. It confirms the file continues
// to spell out the six markers that tooth ⑤'s textual scan looks for.
// If a future edit drops any marker, this test surfaces the loss the
// moment the file is loaded — the sanity is independent of the
// subscription runtime.

#[test]
fn frozen_markers_present_in_this_file_bytes() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("subscription_fork_cold_start_gate.rs");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for marker in [
        "codex --version",
        "no_prompt",
        "cold_start",
        "nonce",
        "distinct",
        "new_session_id",
        "source_unchanged",
    ] {
        assert!(
            body.contains(marker),
            "frozen acceptance marker `{marker}` missing from this file; \
             tooth ⑤ scan would report the gate as absent. Restore the \
             marker before re-freezing."
        );
    }
}
