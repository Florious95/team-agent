//! RED contract — 0.5.58 H-batch mini(异元审查裁定 msg_1d447689df48)
//!
//! Motivation: reviewer-c3 verdict `.team/artifacts/0.5.58-fork-reviewer-c3-verdict.md`
//! BLOCKING B1 + CONCERN C1 + CONCERN C4 三条,leader 裁定补冻:0.5.58
//! Verified/Pending 分型面存在真机布局假 Verified、error 字符串嗅探、
//! finalize 无 source-id 拒绝+pending 残渣四类洞。
//!
//!   齿1 (B1 真机布局假 Verified) — canonical Claude 路径分支 2(context_fork.rs
//!         :142-160) 只测 readable/id-match/id!=source,无 baseline mutation
//!         判据;真机布局下 framework 已把 source 复制到 canonical HOME 目录
//!         `$HOME/.claude/projects/<encode(cwd)>/<expected>.jsonl`,spawn-only
//!         shim 未写字节即返 Verified。红齿探源码:context_fork.rs 分支 2
//!         若持有 "readable && id_match && id_ne_source" 三条件放行 且无
//!         baseline/changed/mutation 判据 → 红。GREEN 必须让分支 2 也复用
//!         baseline mutation 守卫或等价判据。预生成且只读交接的 backing
//!         若同时锁定 exact path、expected session id 与 agent identity，
//!         亦是合法的等价判据。
//!   齿2 (C1 error 字符串嗅探) — outcome.rs 用
//!         `error.to_string().contains("produced no readable NEW session backing")`
//!         区分 Pending vs Rejected(stringly typed)。verify_* 必须返回
//!         typed timeout variant(如 `ProviderError::ContextForkTimeout`
//!         或 `Result<Proof, ContextForkTerminationReason>` 分型),
//!         outcome 层按 typed variant match,禁 contains(...) 嗅探字符串。
//!   齿3 (C4 空壳 finalize 已拔) — 0.5.67 就地 /fork 后 `fork_finalize.rs`
//!         与 `finalize_pending_fork_capture` 不再存在。旧测试读已删文件
//!         会 os error 2。新齿钉：文件已删、空壳函数/调用方已删、
//!         apply_captured_session 不再被 pending_context_fork 拐进恒 None。
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip;fork-agent 破,0.5.58 加 typed
//!   Pending 未闭合三口子)。
//! - Sources for the three CONCERN anchors:
//!     .team/artifacts/0.5.58-fork-reviewer-c3-verdict.md B1 / C1 / C4.
//! - Retains 判据台账 三防线(测试消费产品源 + 独立 canary + canary 分词
//!   粒度锁)。
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.
//!
//! REVISION LINEAGE:
//! - c34258e7 (V1) — tooth 1 硬读单文件 context_fork.rs.
//! - THIS revision (V2, A 案 msg_c59c74fafd10) — tooth 1 改用
//!   read_context_fork_module_text() 聚合 context_fork.rs + context_fork/
//!   子模块树(outcome.rs / claude.rs / codex.rs);d8 拆分 abb0ccf 后
//!   Verified 发射点已分散到 claude.rs/codex.rs,单文件锚扫不到=假绿.
//!   语义断言/canary/tooth 2 & 3 均不动. 台账 §二·补4 落实.
//!   B 案(留死字面注释喂扫描)驳回=注释欺骗扫描器.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

// R6 密闭边界:齿3 直调 lifecycle finalize + serde_json,不动 MessageStore
// 但源码字面量 CARGO_BIN_EXE / delivery / claim-leader 未出现;仍加 hermetic
// 触点满足 R6 静态守卫(与 msg_624b6075c1c8 一致);实跑齿会 enter().
#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::PathBuf;

use serial_test::serial;

fn crate_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    let path = crate_src().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Scan every native context-fork producer in the live restart module tree.
/// File splits must not evade the all-Verified-sites proof check.
fn read_context_fork_module_text() -> String {
    let mut aggregate = String::new();
    let main_path = crate_src().join("lifecycle/restart.rs");
    if let Ok(t) = fs::read_to_string(&main_path) {
        aggregate.push_str(&t);
        aggregate.push('\n');
    }
    let sub_root = crate_src().join("lifecycle/restart");
    fn walk(dir: &std::path::Path, out: &mut String) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = fs::read_to_string(&p) {
                    out.push_str(&t);
                    out.push('\n');
                }
            }
        }
    }
    walk(&sub_root, &mut aggregate);
    aggregate
}

// ---------------------------------------------------------------------------
// 齿1 — B1 真机布局假 Verified 防线
// ---------------------------------------------------------------------------

/// Independent native-proof scanner canaries.
fn synthetic_branch_with_guard() -> &'static str {
    "fn fork_child() {\n\
     child.filter(|child| *child != parent.as_str());\n\
     resume_backing_exists_for_agent();\n\
     backing_state: if child.is_some() {\n\
     ForkBackingState::Verified\n\
     }\n\
     }"
}
fn synthetic_branch_without_guard() -> &'static str {
    "fn fork_child() {\n\
     ForkBackingState::Verified\n\
     }"
}

/// Check all native Verified emits in the restart module tree. Each needs
/// child/backing proof or staged Pi snapshot proof in its own function's
/// bounded 500-line pre-emit window. Returns (n_verified_sites, n_unguarded).
fn scan_verified_sites_and_guards(src: &str) -> (usize, usize) {
    let lines: Vec<&str> = src.lines().collect();
    let mut total_verified = 0usize;
    let mut unguarded = 0usize;
    let mut last_fn_start = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub(crate) fn ")
        {
            last_fn_start = i;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.contains("ForkBackingState::Verified") {
            total_verified += 1;
            let start = last_fn_start.max(i.saturating_sub(500));
            let window = lines[start..=i].join("\n");
            let native_child = window.contains("*child != parent.as_str()")
                && window.contains("resume_backing_exists_for_agent(")
                && window.contains("backing_state: if child.is_some()");
            let native_snapshot = window.contains("target_session_id == binding.session_id")
                && window.contains("session_id_in_use(&full_state, &target_session_id)")
                && window.contains("read_stable_source_snapshot(")
                && window.contains("stage_pi_session(")
                && window.contains("revalidate_source_binding(")
                && window.contains("commit_fork_agent(")
                && window.contains("captured_via: CaptureVia::ForkSnapshot");
            if !(native_child || native_snapshot) {
                unguarded += 1;
            }
        }
    }
    (total_verified, unguarded)
}

#[test]
#[serial(env)]
fn verified_emit_sites_all_guarded_by_baseline_mutation() {
    // The retired manual-copy verifier is not a native fork proof owner.
    // Scan the whole live restart tree, retaining the nonempty/all-site canary.
    let src = read_context_fork_module_text();
    let (verified_sites, unguarded) = scan_verified_sites_and_guards(&src);
    assert!(
        verified_sites > 0,
        "native fork tree has zero ForkBackingState::Verified emit sites; refusing vacuous green"
    );
    assert_eq!(
        unguarded, 0,
        "every native Verified emit must have child/backing or staged-snapshot proof in its function window"
    );
    assert_eq!(
        scan_verified_sites_and_guards(synthetic_branch_with_guard()),
        (1, 0),
        "scanner must accept a guarded native child"
    );
    assert_eq!(
        scan_verified_sites_and_guards(synthetic_branch_without_guard()),
        (1, 1),
        "scanner must reject a bare native Verified emit"
    );
    // Both actual producers must be covered; removing any proof fence must fail.
    let mut covered_sites = 0;
    for (file, guards) in [
        (
            "lifecycle/restart/claude_fork.rs",
            &[
                "*child != parent.as_str()",
                "resume_backing_exists_for_agent(",
                "backing_state: if child.is_some()",
            ][..],
        ),
        (
            "lifecycle/restart/avatar.rs",
            &[
                "target_session_id == binding.session_id",
                "session_id_in_use(&full_state, &target_session_id)",
                "read_stable_source_snapshot(",
                "stage_pi_session(",
                "revalidate_source_binding(",
                "commit_fork_agent(",
                "captured_via: CaptureVia::ForkSnapshot",
            ][..],
        ),
    ] {
        let producer = read_src(file);
        assert_eq!(scan_verified_sites_and_guards(&producer), (1, 0), "{file}");
        covered_sites += 1;
        for guard in guards {
            let mutant = producer.replace(guard, "removed_native_proof_guard");
            assert_eq!(
                scan_verified_sites_and_guards(&mutant),
                (1, 1),
                "{file}: removing {guard} must invalidate native Verified proof"
            );
        }
    }
    assert_eq!(verified_sites, covered_sites, "unreviewed native Verified producer");
}

// ---------------------------------------------------------------------------
// 齿2 — C1 error 字符串嗅探禁令
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn outcome_module_forbids_error_string_sniffing() {
    // Canary sanity — multi-line synthetic proves scanner catches the
    // exact contains(...) sniff pattern independent of product state.
    let sniff = "\
        if error.to_string().contains(\"produced no readable NEW session backing\") {\n\
        \x20   Pending\n\
        }";
    let typed = "\
        match verify_result {\n\
        \x20   Err(ContextForkTerminationReason::Timeout) => Pending,\n\
        \x20   Err(_) => Rejected,\n\
        }";
    let scan = |s: &str| {
        s.contains("error.to_string()") && s.contains(".contains(") && s.contains("Pending")
    };
    assert!(
        scan(sniff) && !scan(typed),
        "canary broken: sniff={} typed={}",
        scan(sniff),
        scan(typed)
    );

    let outcome = read_src("provider/session/context_fork/outcome.rs");
    // Forbid the sniff pattern; require typed variant match.
    let has_sniff = outcome.contains(".to_string()")
        && outcome.contains(".contains(")
        && outcome.contains("Pending");
    assert!(
        !has_sniff,
        "C1: outcome.rs uses `error.to_string().contains(...)` to decide \
         Pending vs Rejected — stringly-typed contract. locate §5.1.2 + \
         reviewer-c3 verdict §C1: verify_* must return a typed timeout \
         variant (e.g. `ContextForkTerminationReason::Timeout` or \
         `ProviderError::ContextForkTimeout`); outcome.rs must match on \
         the typed variant, not sniff error text. Baseline: two producer \
         call sites in context_fork.rs:169,232 both format the same \
         string; renaming the string silently downgrades Pending to \
         Rejected."
    );
}

// ---------------------------------------------------------------------------
// 齿3 — 空壳 finalize 已连调用方一起拔掉（不再读 fork_finalize.rs）
// ---------------------------------------------------------------------------
//
// 就地 /fork 之后没有 pending-capture 需要 finalize。旧齿读
// lifecycle/launch/fork_finalize.rs，拔根后 os error 2。本齿改钉：
// (a) fork_finalize.rs 不存在（阳性对照：同目录 fork_agent.rs 可读）；
// (b) fork_agent.rs 不再定义 finalize_pending_fork_capture / write_audit；
// (c) capture.rs 的 apply_captured_session 不再拐进恒 None 的空壳，
//     有 session_id+rollout_path 时仍写出 captured。

#[test]
#[serial(env)]
fn pending_fork_finalize_shell_is_gone_and_capture_falls_through() {
    let launch_dir = crate_src().join("lifecycle").join("launch");
    let fork_agent_path = launch_dir.join("fork_agent.rs");
    let fork_agent = fs::read_to_string(&fork_agent_path).unwrap_or_else(|e| {
        panic!(
            "positive control: must read {}: {e}",
            fork_agent_path.display()
        )
    });
    assert!(
        fork_agent.contains("fn fork_agent_with_transport"),
        "positive control: fork_agent.rs must still export fork_agent_with_transport"
    );

    let finalize_path = launch_dir.join("fork_finalize.rs");
    assert!(
        !finalize_path.exists(),
        "fork_finalize.rs must stay deleted (was the os-error-2 target)"
    );

    assert!(
        !fork_agent.contains("fn finalize_pending_fork_capture"),
        "silent-None finalize_pending_fork_capture must be deleted from fork_agent.rs"
    );
    assert!(
        !fork_agent.contains("fn write_audit"),
        "write_audit empty Ok shell must be deleted from fork_agent.rs"
    );

    let capture = read_src("provider/session/capture.rs");
    assert!(
        capture.contains("fn apply_captured_session"),
        "positive control: apply_captured_session must still exist"
    );
    assert!(
        !capture.contains("finalize_pending_fork_capture"),
        "capture.rs must not call the deleted finalize_pending_fork_capture"
    );
    assert!(
        !capture.contains("ContextForkFinalized"),
        "capture.rs must not keep ContextForkFinalized after the shell was removed"
    );

    let apply_start = capture
        .find("fn apply_captured_session")
        .expect("apply_captured_session not found after positive control");
    let apply_tail = &capture[apply_start..];
    let apply_end = apply_tail
        .find("\nfn ")
        .unwrap_or(apply_tail.len());
    let apply_body = &apply_tail[..apply_end];
    assert!(
        !apply_body.contains("pending_context_fork"),
        "apply_captured_session must not special-case pending_context_fork into a silent None"
    );
    assert!(
        apply_body.contains("session_id")
            && apply_body.contains("rollout_path")
            && apply_body.contains("\"captured\""),
        "apply_captured_session must still write the captured tuple on the happy path"
    );

    // R-M3: `coordinator/tick.rs` (and its capture_missing_sessions step) was retired with the
    // coordinator; the shell must not reappear anywhere in the runtime sources instead.
    fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs")
                && fs::read_to_string(&path).is_ok_and(|text| text.contains("ContextForkFinalized"))
            {
                hits.push(path.display().to_string());
            }
        }
    }
    let mut hits = Vec::new();
    walk(&crate_src(), &mut hits);
    assert!(hits.is_empty(), "ContextForkFinalized must not come back after the shell was removed: {hits:?}");
}
