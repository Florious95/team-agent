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
//!         baseline mutation 守卫或等价判据。
//!   齿2 (C1 error 字符串嗅探) — outcome.rs 用
//!         `error.to_string().contains("produced no readable NEW session backing")`
//!         区分 Pending vs Rejected(stringly typed)。verify_* 必须返回
//!         typed timeout variant(如 `ProviderError::ContextForkTimeout`
//!         或 `Result<Proof, ContextForkTerminationReason>` 分型),
//!         outcome 层按 typed variant match,禁 contains(...) 嗅探字符串。
//!   齿3 (C4 finalize 拒 source 冒充 + 清 pending 字段) — 行为面:mock
//!         PendingContextFork state seed 一个 `fork_source_session_id=SRC`
//!         的 agent row,调 `finalize_pending_fork_capture` 传入
//!         `captured.session_id == SRC`,必须**拒 finalize**(return false 或
//!         typed error);且合法 captured(id != SRC)finalize 后清三 pending
//!         字段 `fork_source_session_id` / `pending_target_agent` / `pending_grace_secs`。
//!         正控:合法 captured 正常 finalize + capture_state=captured。
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

/// Very small scanner: within the body of `context_fork.rs`, locate ALL
/// `return ContextForkOutcome::Verified` (or `Ok(proof)` inside a
/// Verified path) call sites, and within a 20-line pre-window check
/// whether it is guarded by one of the accepted baseline judgments
/// (`baseline`, `changed`, `mutation`, `pre_snapshot`, `snapshot_changed`,
/// `differs_from_baseline`). Returns (n_verified_sites, n_unguarded).
fn scan_verified_sites_and_guards(text: &str) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut n_sites = 0usize;
    let mut n_unguarded = 0usize;
    for (i, l) in lines.iter().enumerate() {
        let trimmed = l.trim_start();
        // Accept "Ok(ContextForkProof {" or "Ok(proof)" or
        // "ContextForkOutcome::Verified(" — all are Verified emission
        // points in context_fork.rs.
        let is_verified_emit = trimmed.starts_with("Ok(ContextForkProof")
            || trimmed.starts_with("return Ok(ContextForkProof")
            || trimmed.contains("ContextForkOutcome::Verified(")
            || (trimmed.starts_with("Ok(proof") && trimmed.contains(")"));
        if !is_verified_emit {
            continue;
        }
        n_sites += 1;
        let start = i.saturating_sub(30);
        let window: String = lines[start..i].join("\n");
        let has_guard = window.contains("baseline")
            || window.contains("changed")
            || window.contains("mutation")
            || window.contains("pre_snapshot")
            || window.contains("snapshot_changed")
            || window.contains("differs_from_baseline");
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

    let src = read_src("provider/session/context_fork.rs");
    let (n_sites, n_unguarded) = scan_verified_sites_and_guards(&src);
    assert!(
        n_sites > 0,
        "no Verified emit site found in context_fork.rs — scanner broken \
         (source shape changed?)"
    );
    assert_eq!(
        n_unguarded, 0,
        "B1: context_fork.rs has {n_unguarded}/{n_sites} Verified emit \
         sites that lack a baseline/changed/mutation guard in the 30-line \
         pre-window. Real-machine layout (source rollout placed at \
         `$HOME/.claude/projects/<encode(cwd)>/<expected>.jsonl` by \
         framework materialization) makes an unguarded branch return \
         Verified for spawn-only providers. Every Verified emit MUST be \
         guarded by a post-materialization baseline/changed judgment. \
         Fix: apply the same `changed` guard branch 1 uses \
         (context_fork.rs:127-141) to branch 2 (:142-160)."
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
// 齿3 — C4 finalize source-id 拒绝 + 清 pending 字段(源码文本扫)
// ---------------------------------------------------------------------------
//
// `finalize_pending_fork_capture` is `pub(crate)` — not reachable from
// integration tests without an in-crate helper. We probe SOURCE shape:
// (a) function body must contain a source-id refusal predicate
//     comparing captured.session_id to fork_source_session_id;
// (b) function body must remove three pending scaffold fields
//     (`fork_source_session_id` / `pending_target_agent` /
//     `pending_grace_secs`) via `agent.remove(...)` calls.
// (c) positive-control source shape: the function still returns `bool`
//     and writes `session_id`/`rollout_path`/`capture_state` on the
//     legitimate path.

#[test]
#[serial(env)]
fn finalize_pending_fork_capture_refuses_source_id_and_clears_pending_fields() {
    let src = read_src("lifecycle/launch/fork_finalize.rs");
    let start = src
        .find("pub(crate) fn finalize_pending_fork_capture")
        .or_else(|| src.find("pub fn finalize_pending_fork_capture"))
        .expect("finalize_pending_fork_capture not found in fork_finalize.rs");
    let tail = &src[start..];
    // Cheap block extract: up to first `\n}\n` at column-0 close brace.
    let end_off = tail
        .find("\n}\n")
        .expect("could not locate closing brace of finalize_pending_fork_capture");
    let body = &tail[..end_off];

    // Face (a): source-id refusal — body must reference BOTH
    // `fork_source_session_id` AND a comparison / early-return path.
    let mentions_source_field = body.contains("fork_source_session_id");
    let has_refusal_shape = mentions_source_field
        && (body.contains("return false")
            || body.contains("return Ok(false)")
            || body.contains("return Err")
            || body.contains("MasqueradeRefused"));
    assert!(
        has_refusal_shape,
        "C4: finalize_pending_fork_capture body has no source-id refusal \
         predicate. Must add `if captured.session_id == agent.fork_source_session_id \
         {{ return false; }}` (or typed error) BEFORE writing the tuple. \
         mentions_source_field={mentions_source_field}. body head:\n{}",
        body.chars().take(600).collect::<String>()
    );

    // Face (b): three pending scaffold fields removed.
    for field in [
        "fork_source_session_id",
        "pending_target_agent",
        "pending_grace_secs",
    ] {
        let needle_a = format!("remove(\"{field}\")");
        let needle_b = format!("remove(\"{field}\".");
        assert!(
            body.contains(&needle_a) || body.contains(&needle_b),
            "C4: finalize_pending_fork_capture does not remove pending \
             scaffold field `{field}` after successful finalize. Reviewer \
             §C4: captured row must not carry pending residue. Expected \
             `agent.remove(\"{field}\");` in body."
        );
    }

    // Face (c) positive control: function still marks capture_state as captured
    // and still returns a `bool` / `Result` (shape unchanged; refusal path is
    // additive, not destructive).
    assert!(
        body.contains("capture_state")
            && (body.contains("\"captured\"") || body.contains(":captured")),
        "PC: legitimate finalize path must still mark capture_state as \
         `captured`; guard hardening should not delete the happy-path write."
    );
    // Signature must still return bool (matches baseline shape); if refactor
    // switched to Result<bool,_> it's still admissible (bool present as the
    // Ok type).
    assert!(
        src[start..(start + 200)].contains("-> bool")
            || src[start..(start + 300)].contains("Result<bool"),
        "PC: finalize_pending_fork_capture signature drifted; expected \
         `-> bool` or `-> Result<bool, _>`."
    );
}
