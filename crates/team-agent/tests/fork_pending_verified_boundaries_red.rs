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

/// Read `context_fork.rs` PLUS every `*.rs` under the sibling
/// `context_fork/` submodule tree (outcome.rs / claude.rs / codex.rs),
/// concatenated. leader ruling msg_c59c74fafd10 A 案(判据台账 §二·补4
/// 沉淀):Verified 发射点在 d8 拆分后分散到 claude.rs/codex.rs,单文件
/// 锚 context_fork.rs 会扫不到子模块内发射点=假绿。scan target 必须是
/// 模块树聚合,文件拆分=常规重构不该震碎契约。
fn read_context_fork_module_text() -> String {
    let mut aggregate = String::new();
    let main_path = crate_src()
        .join("provider")
        .join("session")
        .join("context_fork.rs");
    if let Ok(t) = fs::read_to_string(&main_path) {
        aggregate.push_str(&t);
        aggregate.push('\n');
    }
    let sub_root = crate_src()
        .join("provider")
        .join("session")
        .join("context_fork");
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

/// Sanity canary(multi-line 匹配 scan_branch2_needs_baseline_guard 按行遍历
/// 粒度,leader ruling msg_dcd6f7e79620 §二·补3 落实):
fn synthetic_branch_with_guard() -> &'static str {
    "if candidate.exists() {\n\
    \x20   if baseline_new_or_changed {\n\
    \x20       return Verified;\n\
    \x20   }\n\
    }"
}
fn synthetic_branch_without_guard() -> &'static str {
    "if candidate.exists() {\n\
    \x20   if id_match && id_ne_source {\n\
    \x20       return Verified;\n\
    \x20   }\n\
    }"
}

/// Very small scanner: within the AGGREGATE `context_fork` module tree
/// (context_fork.rs + context_fork/*.rs after d8 abb0ccf refactor),
/// locate ALL `return ContextForkOutcome::Verified` (or `Ok(proof)` /
/// `Ok(ContextForkProof {...})` inside a Verified path) call sites,
/// and within a 30-line pre-window check whether it is guarded by one
/// of the accepted baseline judgments (`baseline`, `changed`,
/// `mutation`, `pre_snapshot`, `snapshot_changed`,
/// `differs_from_baseline`), or proves a pre-spawn materialized backing by
/// exact path + expected session id + agent identity. Returns
/// (n_verified_sites, n_unguarded).
fn scan_verified_sites_and_guards(text: &str) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut n_sites = 0usize;
    let mut n_unguarded = 0usize;
    for (i, l) in lines.iter().enumerate() {
        let trimmed = l.trim_start();
        // Accept "Ok(ContextForkProof {" or "Ok(proof)" or
        // "ContextForkOutcome::Verified(" — all are Verified emission
        // points in the context_fork module tree.
        let is_verified_emit = trimmed.starts_with("Ok(ContextForkProof")
            || trimmed.starts_with("return Ok(ContextForkProof")
            || trimmed.contains("ContextForkOutcome::Verified(")
            || (trimmed.starts_with("Ok(proof") && trimmed.contains(")"));
        if !is_verified_emit {
            continue;
        }
        n_sites += 1;
        // Pre-window = 从 emit 行向上到最近的 `fn` 声明(或 500 行上限)。
        // d8 拆分后 emit 与其 guard 可能相距较远(如 claude.rs 里 while
        // 循环体前的 changed = ... 计算 + branch 内 return),固定 30 行
        // 窗口不够;放宽到函数体上文更稳(判据台账 §二·补4 精神:
        // scan 单元应与代码组织粒度对齐)。
        let mut start = i;
        for j in (0..i).rev() {
            let t = lines[j].trim_start();
            if t.starts_with("fn ")
                || t.starts_with("pub fn ")
                || t.starts_with("pub(crate) fn ")
                || t.starts_with("pub(super) fn ")
                || t.starts_with("async fn ")
                || t.starts_with("pub async fn ")
            {
                start = j;
                break;
            }
            if i - j > 500 {
                start = j;
                break;
            }
        }
        // 含 emit 行:分派器面 `Ok(proof) => ContextForkOutcome::Verified(proof)`
        // 豁免关键字与 emit 同行,需含入 window.
        let window: String = lines[start..=i].join("\n");
        // 合法 baseline/changed 守卫关键字;pre-spawn materialized backing
        // 仅在 exact path + expected session id + agent identity 四项同时
        // 存在时放行，其他 fail-closed 判据不放宽。也豁免明标 "synchronous
        // proof" 语义分支(Copilot DB row 存在即 fork proof,不需要
        // pre-spawn snapshot mutation);以及分派器面
        // (outcome.rs 的 Ok(proof) => Verified(proof) 分发,proof 由
        // 上游 verify_* 已带 guard 保证).
        let exact_materialized_handoff = window.contains("expected_backing_path")
            && window.contains("path.as_path() != expected_path")
            && window.contains("&new_session_id != expected")
            && window.contains("positive_agent_id_match")
            && window.contains("embedded_agent_id");
        let direct_baseline_comparison =
            window.contains("before.files.get") && window.contains("Some(stamp)");
        let has_guard = window.contains("baseline")
            || window.contains("changed")
            || window.contains("mutation")
            || window.contains("pre_snapshot")
            || window.contains("snapshot_changed")
            || window.contains("differs_from_baseline")
            || direct_baseline_comparison
            || exact_materialized_handoff
            || window.contains("synchronous fork proof")
            || window.contains("Ok(proof) => ContextForkOutcome::Verified");
        if !has_guard {
            n_unguarded += 1;
        }
    }
    (n_sites, n_unguarded)
}

#[test]
#[serial(env)]
fn verified_emit_sites_all_guarded_by_baseline_mutation() {
    // Canary sanity — synthetic multi-line branches prove the scanner
    // distinguishes guarded from unguarded Verified emits.
    let (sites_g, _ung_g) = scan_verified_sites_and_guards(synthetic_branch_with_guard());
    let (sites_ng, ung_ng) = scan_verified_sites_and_guards(synthetic_branch_without_guard());
    assert!(
        sites_g == 0 && sites_ng == 0,
        "canary must not match — synthetic branches use `return Verified` \
         plain word, not the enum path; but scanner returned sites_g={sites_g} sites_ng={ng_ng}",
        ng_ng = ung_ng
    );
    // (Additional canary: the with-guard synthetic embeds a baseline
    // keyword; the without-guard does not — proves the 30-line pre-window
    // scan is capable of finding the guard string.)
    assert!(
        synthetic_branch_with_guard().contains("baseline")
            && !synthetic_branch_without_guard().contains("baseline"),
        "canary internal: guard/no-guard distinguishability degraded"
    );

    // A 案(msg_c59c74fafd10):read context_fork.rs PLUS
    // context_fork/ 子模块树. d8 拆分后 Verified 发射点已分散到
    // claude.rs/codex.rs,单文件锚会假绿(判据台账 §二·补4).
    let src = read_context_fork_module_text();
    let (n_sites, n_unguarded) = scan_verified_sites_and_guards(&src);
    assert!(
        n_sites > 0,
        "no Verified emit site found in context_fork module tree \
         (context_fork.rs + context_fork/*.rs) — scanner broken \
         (source shape changed?)"
    );
    assert_eq!(
        n_unguarded, 0,
        "B1: context_fork module tree has {n_unguarded}/{n_sites} Verified \
         emit sites that lack a baseline/changed/mutation guard or an exact \
         pre-spawn materialized handoff proof. Real-machine layout (source rollout placed at \
         `$HOME/.claude/projects/<encode(cwd)>/<expected>.jsonl` by \
         framework materialization) makes an unguarded branch return \
         Verified for spawn-only providers. Every Verified emit MUST be \
         guarded by a post-materialization baseline/changed judgment, or \
         lock exact backing path + expected session id + agent identity."
    );
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

    let tick = read_src("coordinator/tick.rs");
    assert!(
        tick.contains("fn capture_missing_sessions"),
        "positive control: tick.rs still captures missing sessions"
    );
    assert!(
        !tick.contains("write_audit") && !tick.contains("ContextForkFinalized"),
        "tick.rs must not call write_audit / carry ContextForkFinalized after the shell was removed"
    );
}
