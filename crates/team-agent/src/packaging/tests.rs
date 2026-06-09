#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]
use super::*;
use std::path::{Path, PathBuf};

// ───────────────────────────────────────────────────────────────────────
// §19 散字符串 → enum:SkillTarget→Provider 关联 (skeleton:112 codex→Codex,
// claude→ClaudeCode;all→None)。Python _skill_dest_dir 据 target 选 dir。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn skill_target_codex_maps_to_provider_codex() {
    // skeleton line 112: codex→Codex.
    assert_eq!(SkillTarget::Codex.provider(), Some(Provider::Codex));
}

#[test]
fn skill_target_claude_maps_to_provider_claude_code() {
    // skeleton line 112: claude→ClaudeCode (NOT bare Claude — §3 claude vs claude_code 不能漏归一).
    assert_eq!(SkillTarget::Claude.provider(), Some(Provider::ClaudeCode));
}

#[test]
fn skill_target_all_has_no_single_provider() {
    // `All` fan-out 两者 → 无单一 provider.
    assert_eq!(SkillTarget::All.provider(), None);
}

// ───────────────────────────────────────────────────────────────────────
// _skill_dest_dir (commands.py:467-472):claude→~/.claude/skills/team-agent,
// 其余(含 codex)→~/.codex/skills/team-agent;All→None(非单 dir,fan-out)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn dest_dir_codex_resolves_to_dot_codex() {
    // commands.py:471 — codex (else branch) → ~/.codex/skills/team-agent.
    let home = Path::new("/home/testuser");
    let got = SkillTarget::Codex.dest_dir(home);
    assert_eq!(
        got,
        Some(SkillDestDir(PathBuf::from(
            "/home/testuser/.codex/skills/team-agent"
        )))
    );
}

#[test]
fn dest_dir_claude_resolves_to_dot_claude() {
    // commands.py:469 — claude → ~/.claude/skills/team-agent.
    let home = Path::new("/home/testuser");
    let got = SkillTarget::Claude.dest_dir(home);
    assert_eq!(
        got,
        Some(SkillDestDir(PathBuf::from(
            "/home/testuser/.claude/skills/team-agent"
        )))
    );
}

#[test]
fn dest_dir_all_is_none_not_single_dir() {
    // `All` 应 fan-out 到两者 → 非单 dir → None (skeleton:116).
    let home = Path::new("/home/testuser");
    assert_eq!(SkillTarget::All.dest_dir(home), None);
}

// ───────────────────────────────────────────────────────────────────────
// Version 单一真相源:env!("CARGO_PKG_VERSION") — 修双源漂移
// (pyproject 0.1.4 vs package.json 0.2.11)。current() == Cargo.toml 版本,
// 禁手抄第二处。as_str() 即透传。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn version_current_equals_cargo_pkg_version() {
    // 单一真相源 — 禁手抄。current() 必须 == 编译期 CARGO_PKG_VERSION.
    assert_eq!(Version::current().as_str(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn version_current_is_not_a_hand_copied_python_drift_literal() {
    // STRENGTHENED (gate w59ds828k): the old test only asserted !is_empty() && != "dev",
    // which a buggy impl returning a hardcoded "0.2.11" copied from package.json would PASS —
    // exactly the double-source-drift bug the subsystem forbids. Now CONCRETE & falsifiable.
    //
    // CONCRETE golden: workspace Cargo.toml version == "0.0.0" (Phase 0; team-agent-rs
    // Cargo.toml:12). CARGO_PKG_VERSION therefore resolves to "0.0.0" — which differs from
    // BOTH Python drift sources (pyproject.toml 0.1.4 / package.json 0.2.11). So a porter who
    // hand-copies either Python literal instead of using env!("CARGO_PKG_VERSION") FAILS here.
    let v = Version::current();
    assert_eq!(
        v.as_str(),
        env!("CARGO_PKG_VERSION"),
        "single source of truth = CARGO_PKG_VERSION"
    );
    assert_ne!(v.as_str(), "0.1.4", "must not hand-copy pyproject.toml drift source");
    assert_ne!(v.as_str(), "0.2.11", "must not hand-copy package.json drift source");
    assert_ne!(v.as_str(), "dev", "must not be install.mjs:54 'dev' fallback");
}

#[test]
fn no_literal_version_string_hardcoded_in_packaging_code() {
    // STRENGTHENED (gate w59ds828k): grep-assert the production CODE (comments stripped)
    // contains no hand-copied semver literal — only env!("CARGO_PKG_VERSION") may supply the
    // version. Reads the file at test time via CARGO_MANIFEST_DIR. The Python drift literals
    // 0.1.4 / 0.2.11 legitimately appear in doc/line comments documenting the bug, so we strip
    // comment text first and scan only executable code. This is the one place where
    // "double-source-drift forbidden" is statically checked against the source itself.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/packaging/types.rs"))
        .expect("read own source");
    // Production region only (the #[cfg(test)] mod uses these literals as golden anti-examples):
    let prod = match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };
    // Strip everything from the first `//` on each line (covers `//!` doc + `//` line comments).
    let code: String = prod
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("0.1.4"),
        "packaging.rs code must not hand-copy pyproject 0.1.4 (use env!(CARGO_PKG_VERSION))"
    );
    assert!(
        !code.contains("0.2.11"),
        "packaging.rs code must not hand-copy package.json 0.2.11 (use env!(CARGO_PKG_VERSION))"
    );
    // The ONLY version source allowed is env!("CARGO_PKG_VERSION") — assert it is the source.
    assert!(
        code.contains("CARGO_PKG_VERSION"),
        "Version::current() must derive from env!(\"CARGO_PKG_VERSION\")"
    );
}

#[test]
fn version_serde_transparent_roundtrip() {
    // #[serde(transparent)] — 序列化为裸字符串,非 {"0":"..."}.
    let v = Version("1.2.3".to_string());
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, "\"1.2.3\"");
    let back: Version = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

// ───────────────────────────────────────────────────────────────────────
// platform_support (§8 如实声明):macOS/Linux 原生;Windows 原生一等
// (WezTerm/ConPTY,见 transport-backend-design)。不假装兼容。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn macos_aarch64_is_native() {
    assert_eq!(
        platform_support(ReleaseTarget::MacosAarch64),
        PlatformSupport::Native
    );
}

#[test]
fn macos_x8664_is_native() {
    assert_eq!(
        platform_support(ReleaseTarget::MacosX8664),
        PlatformSupport::Native
    );
}

#[test]
fn linux_x8664_is_native() {
    assert_eq!(
        platform_support(ReleaseTarget::LinuxX8664),
        PlatformSupport::Native
    );
}

#[test]
fn linux_aarch64_is_native() {
    assert_eq!(
        platform_support(ReleaseTarget::LinuxAarch64),
        PlatformSupport::Native
    );
}

#[test]
fn windows_x8664_is_native_per_transport_design() {
    // skeleton:203/211 — Windows 原生一等 (WezTerm/ConPTY,非 tmux)。
    // 不是 Unsupported,不是 RequiresWslTmux — 是 Native.
    assert_eq!(
        platform_support(ReleaseTarget::WindowsX8664),
        PlatformSupport::Native
    );
}

// ───────────────────────────────────────────────────────────────────────
// doctor — typed DoctorStatus (commands.py:218-260)。
// error 路径 + 精确消息:
//   - --fix 无 --gate → TeamAgentError("--fix requires --gate")  (commands.py:221)
//   - unknown doctor gate → "unknown doctor gate: <g>"           (commands.py:235)
//   - schema layout drift → HasBlockers{SchemaLayoutDrift}       (commands.py:242-250)
// ───────────────────────────────────────────────────────────────────────

fn doctor_opts(workspace: &Path) -> DoctorOptions {
    DoctorOptions {
        workspace: workspace.to_path_buf(),
        gate: None,
        fix: false,
        cleanup_orphans: false,
        confirm: false,
    }
}

#[test]
fn doctor_fix_without_gate_is_invalid_options() {
    // commands.py:220-221 — `--fix requires --gate`.
    let ws = PathBuf::from("/tmp/ws-doctor-fix");
    let mut opts = doctor_opts(&ws);
    opts.fix = true;
    opts.gate = None;
    let err = doctor(&opts).expect_err("fix without gate must error");
    match err {
        PackagingError::InvalidOptions(msg) => {
            assert!(
                msg.contains("--fix requires --gate"),
                "expected '--fix requires --gate', got: {msg}"
            );
        }
        other => panic!("expected InvalidOptions, got {other:?}"),
    }
}

/// TEST-SUPPORT seed helper (real impl — pure test scaffolding, uses rusqlite directly):
/// build a workspace whose `.team/runtime/team.db` has the LEGACY drifted layout
/// (owner_team_id appended as the last column on the 4 managed tables, user_version=1).
/// This is the exact fixture migration.rs::build_legacy uses; schema_diagnosis on it yields
/// non-empty layout_diffs → doctor() must surface SchemaLayoutDrift. Returns the workspace.
fn seed_workspace_with_drifted_db(tag: &str) -> PathBuf {
    use rusqlite::Connection;
    let ws = std::env::temp_dir().join(format!("ta-doctor-drift-{}-{}", std::process::id(), tag));
    let db = ws.join(".team").join("runtime").join("team.db");
    std::fs::create_dir_all(db.parent().unwrap()).expect("seed runtime dir");
    let conn = Connection::open(&db).expect("open drifted db");
    // Legacy layout: owner_team_id is the LAST column → physical column-order drift vs canonical.
    conn.execute_batch(
        "create table messages (message_id text primary key, task_id text, sender text, recipient text, reply_to text, requires_ack integer, status text, content text, artifact_refs text, created_at text, updated_at text, delivered_at text, acknowledged_at text, error text, delivery_attempts integer not null default 0, owner_team_id text);
         create table results (result_id text primary key, task_id text not null, agent_id text not null, envelope text not null, status text not null, created_at text not null, owner_team_id text);
         create table scheduled_events (id integer primary key, due_at text not null, target text not null, kind text not null, payload_json text not null, status text not null, created_at text not null, fired_at text, result_json text, owner_team_id text);
         create table agent_health (agent_id text not null, status text not null, last_output_at text, context_usage_pct integer, current_task_id text, updated_at text not null, owner_team_id text);
         pragma user_version = 1;",
    )
    .expect("seed legacy schema");
    drop(conn);
    ws
}

#[test]
fn doctor_on_clean_workspace_no_drift_is_ok() {
    // 无 schema layout drift (空/无 db) → DoctorStatus::Ok.
    // schema_diagnosis(missing db) → layout_diffs 空 → 非 HasBlockers(SchemaLayoutDrift).
    // NOTE (gate w59ds828k): clean→Ok cannot distinguish "gates ran & passed" from "gates not
    // wired" because step 11/12 gate entities live elsewhere. The positive drift test below
    // (doctor_drifted_db_emits_schema_layout_drift_blocker) is what proves doctor() actually
    // READS schema_diagnosis and emits the typed SchemaLayoutDrift blocker on real drift.
    let dir = std::env::temp_dir().join(format!("ta-doctor-clean-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let opts = doctor_opts(&dir);
    let status = doctor(&opts).expect("clean workspace doctor should succeed");
    assert_eq!(status, DoctorStatus::Ok);
}

#[test]
fn doctor_comms_gate_failure_maps_to_typed_blocker() {
    let dir = std::env::temp_dir().join(format!("ta-doctor-comms-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut opts = doctor_opts(&dir);
    opts.gate = Some(DoctorGate::Comms);
    let status = doctor(&opts).expect("comms gate should return typed blockers");
    match status {
        DoctorStatus::HasBlockers { blockers } => {
            let blocker = blockers
                .iter()
                .find(|blocker| blocker.source == BlockerSource::CommsGate)
                .expect("must surface CommsGate blocker");
            assert!(
                blocker.detail.contains("receiver_binding"),
                "blocker detail must name failing check: {}",
                blocker.detail
            );
        }
        DoctorStatus::Ok => panic!("missing receiver binding must not report Ok"),
    }
}

#[test]
fn doctor_drifted_db_emits_schema_layout_drift_blocker() {
    // STRENGTHENED (gate w59ds828k): the ONLY drift assertion previously
    // (doctor_status_has_blockers_carries_typed_source) was a pure serde test that hand-built
    // the Blocker — it never proved doctor() EMITS SchemaLayoutDrift from a real drifted db.
    // This drives real doctor() on a SEEDED drifted team.db and pins the CONCRETE golden:
    //   commands.py:242-250 → schema layout drift → coordinator.schema_error ==
    //   "team.db physical layout drift detected"  (EXACT string, commands.py:248).
    let ws = seed_workspace_with_drifted_db("blocker");
    let opts = doctor_opts(&ws);
    let status = doctor(&opts).expect("doctor on drifted workspace should succeed (returns blockers)");
    match status {
        DoctorStatus::HasBlockers { blockers } => {
            let drift = blockers
                .iter()
                .find(|b| b.source == BlockerSource::SchemaLayoutDrift)
                .expect("must surface a SchemaLayoutDrift blocker");
            // EXACT golden string from commands.py:248.
            assert_eq!(
                drift.detail, "team.db physical layout drift detected",
                "schema_error golden must match commands.py:248 verbatim"
            );
        }
        DoctorStatus::Ok => panic!("drifted team.db must NOT report Ok — layout_diffs non-empty"),
    }
}

#[test]
fn doctor_status_ok_serializes_with_status_tag() {
    // #[serde(tag = "status")] — Ok → {"status":"ok"}.
    let json = serde_json::to_string(&DoctorStatus::Ok).unwrap();
    assert_eq!(json, "{\"status\":\"ok\"}");
}

#[test]
fn doctor_status_has_blockers_carries_typed_source() {
    // HasBlockers serde:tag status=has_blockers + blockers[].source snake_case.
    let status = DoctorStatus::HasBlockers {
        blockers: vec![Blocker {
            source: BlockerSource::SchemaLayoutDrift,
            detail: "team.db physical layout drift detected".to_string(),
        }],
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"status\":\"has_blockers\""), "got: {json}");
    assert!(json.contains("\"source\":\"schema_layout_drift\""), "got: {json}");
    // detail 精确 == commands.py:248 schema_error 文本.
    assert!(
        json.contains("team.db physical layout drift detected"),
        "got: {json}"
    );
}

#[test]
fn blocker_source_serde_exact_snake_case_strings() {
    // §19 穷尽 enum 序列化 == Python 散字符串等价 (逐 variant 钉).
    let cases: &[(BlockerSource, &str)] = &[
        (BlockerSource::SchemaLayoutDrift, "\"schema_layout_drift\""),
        (BlockerSource::OrphanCoordinator, "\"orphan_coordinator\""),
        (BlockerSource::CommsGate, "\"comms_gate\""),
        (BlockerSource::PathNotConfigured, "\"path_not_configured\""),
    ];
    for (src, want) in cases {
        assert_eq!(&serde_json::to_string(src).unwrap(), want);
    }
}

// ───────────────────────────────────────────────────────────────────────
// repair_schema — 转调 step 3 fix_schema_layout(workspace, SCHEMA_VERSION=3)。
// (commands.py:239-240 doctor --fix-schema)。
// 决策:
//   - db 不存在 → Missing → 无 drift → UpToDate (转调外壳)。
//   - 撞活跃锁 → Blocked{reason:"active_lock"} 且不写备份。
// 破坏性 rebuild 真路径 #[ignore](需真 db fixture / 真机)。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn repair_schema_missing_db_is_up_to_date() {
    // db 不存在 (.team/runtime/team.db) → fix_schema_layout 返 Missing →
    // packaging 包成 UpToDate (无 drift 需迁移)。
    let dir = std::env::temp_dir().join(format!("ta-repair-missing-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let outcome = repair_schema(&dir).expect("missing db repair should succeed");
    match outcome {
        MigrationOutcome::UpToDate { diagnosis } => {
            // schema_diagnosis(missing) → ok=true, layout_diffs 空.
            assert!(diagnosis.layout_diffs.is_empty(), "missing db has no drift");
        }
        other => panic!("expected UpToDate for missing db, got {other:?}"),
    }
}

#[test]
#[ignore = "REAL-MACHINE-E2E: needs real drifted team.db fixture + destructive rebuild"]
fn repair_schema_drifted_db_migrates() {
    // 真 drift fixture → Migrated{fix: FixResult::Fixed{rebuilds non-empty}}.
    let ws = PathBuf::from("/nonexistent-fixture");
    let outcome = repair_schema(&ws).expect("drift repair");
    assert!(matches!(outcome, MigrationOutcome::Migrated { .. }));
}

#[test]
#[ignore = "REAL-MACHINE-E2E: needs a held active lock on team.db (concurrent BEGIN IMMEDIATE)"]
fn repair_schema_active_lock_is_blocked_no_backup() {
    // 撞活跃锁 → Blocked{reason:"active_lock"};且不写备份 (db/migration.rs:db_lock_status).
    let ws = PathBuf::from("/nonexistent-locked-fixture");
    let outcome = repair_schema(&ws).expect("blocked repair returns Ok wrapper");
    match outcome {
        MigrationOutcome::Blocked { reason } => assert_eq!(reason, "active_lock"),
        other => panic!("expected Blocked, got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────
// diagnose_path — bincheck.mjs printMissingBinDiagnostic 等价 (typed PathDiagnostic)。
// bin 在 PATH → OnPath;不在 → NotOnPath{diagnostic}。Rust 无 npm → npmrc_prefix=None。
// 纯逻辑可单测 (真探 PATH 部分 #[ignore])。
// ───────────────────────────────────────────────────────────────────────

#[test]
fn diagnose_path_when_bin_on_path_reports_on_path() {
    // bin_dir 在当前 PATH → OnPath{bin_dir}。构造:把 bin_dir 临时塞进 PATH。
    // 取 PATH 首个真实条目作为 bin_dir,保证「在 PATH 上」。
    let path_var = std::env::var("PATH").unwrap_or_default();
    let first = path_var
        .split(':')
        .find(|p| !p.is_empty())
        .expect("PATH has at least one entry");
    let bin = BinDir(PathBuf::from(first));
    let hint = diagnose_path(&bin).expect("diagnose on-path bin");
    match hint {
        PathHint::OnPath { bin_dir } => assert_eq!(bin_dir, PathBuf::from(first)),
        PathHint::NotOnPath { .. } => panic!("PATH entry should report OnPath"),
    }
}

#[test]
fn diagnose_path_not_on_path_npmrc_prefix_is_none_no_npm() {
    // Rust 版无 npm 路径 → npmrc_prefix == None (skeleton:257).
    // 用一个绝不在 PATH 的目录。
    let bin = BinDir(PathBuf::from("/zzz-definitely-not-on-path-9f3a"));
    let hint = diagnose_path(&bin).expect("diagnose off-path bin");
    match hint {
        PathHint::NotOnPath { bin_dir, diagnostic } => {
            assert_eq!(bin_dir, PathBuf::from("/zzz-definitely-not-on-path-9f3a"));
            // Rust 无 npm → 绝不重新引入 .npmrc 解析.
            assert_eq!(diagnostic.npmrc_prefix, None);
            // path_entries == 当前 PATH 条目数 (bincheck.mjs:43).
            let want = std::env::var("PATH")
                .unwrap_or_default()
                .split(':')
                .filter(|p| !p.is_empty())
                .count();
            assert_eq!(diagnostic.path_entries, want);
        }
        PathHint::OnPath { .. } => panic!("bogus dir must be NotOnPath"),
    }
}

#[test]
fn path_hint_serde_tag_kind() {
    // #[serde(tag = "kind")] — OnPath → {"kind":"on_path",...}.
    let h = PathHint::OnPath {
        bin_dir: PathBuf::from("/home/u/.local/bin"),
    };
    let json = serde_json::to_string(&h).unwrap();
    assert!(json.contains("\"kind\":\"on_path\""), "got: {json}");
}

// ───────────────────────────────────────────────────────────────────────
// install_skill — cmd_install_skill (commands.py:451-481)。
// error 路径 + 精确消息:
//   - --dest + --target all → InvalidOptions("--dest cannot be combined with --target all")
//                                                                  (commands.py:453-454)
// dry-run 决策 (无副作用,可单测):
//   - dry_run=true → SkillInstallOutcome{dry_run:true, removed_stale:[]} 不落地.
// 真拷 / removed_stale #[ignore] (文件系统副作用)。
// ───────────────────────────────────────────────────────────────────────

fn skill_opts(target: SkillTarget, dest: Option<PathBuf>, dry_run: bool) -> SkillInstallOptions {
    SkillInstallOptions {
        target,
        dest,
        dry_run,
        source: PathBuf::from("/repo/skills/team-agent"),
    }
}

#[test]
fn install_skill_dest_with_target_all_is_invalid() {
    // commands.py:453-454 — `--dest cannot be combined with --target all`.
    let opts = skill_opts(
        SkillTarget::All,
        Some(PathBuf::from("/custom/dest")),
        false,
    );
    let err = install_skill(&opts).expect_err("dest + all must error");
    match err {
        PackagingError::InvalidOptions(msg) => assert!(
            msg.contains("--dest cannot be combined with --target all"),
            "got: {msg}"
        ),
        other => panic!("expected InvalidOptions, got {other:?}"),
    }
}

#[test]
fn install_skill_dry_run_single_target_reports_plan_no_side_effects() {
    // commands.py:477-478 dry_run → {ok, source, dest, dry_run:true} 不落地.
    let opts = skill_opts(SkillTarget::Codex, None, true);
    let outcomes = install_skill(&opts).expect("dry-run install-skill");
    assert_eq!(outcomes.len(), 1, "single target → 1 outcome");
    let o = &outcomes[0];
    assert_eq!(o.target, SkillTarget::Codex);
    assert!(o.dry_run, "dry_run flag preserved");
    // dry-run 绝不清理残留 (无副作用).
    assert!(o.removed_stale.is_empty(), "dry-run touches nothing");
    // dest == codex skill dir (HOME 依赖,用 dirs 解析;只断后缀稳定部分).
    assert!(
        o.dest
            .0
            .ends_with(PathBuf::from(".codex/skills/team-agent")),
        "got: {:?}",
        o.dest
    );
}

#[test]
fn install_skill_dry_run_target_all_fans_out_to_two() {
    // commands.py:458-463 — target all → 两个 outcome (codex + claude),顺序固定.
    let opts = skill_opts(SkillTarget::All, None, true);
    let outcomes = install_skill(&opts).expect("dry-run install-skill all");
    assert_eq!(outcomes.len(), 2, "all → fan-out codex+claude");
    // KEY ORDER:commands.py:460-461 codex first, claude second.
    assert_eq!(outcomes[0].target, SkillTarget::Codex);
    assert_eq!(outcomes[1].target, SkillTarget::Claude);
    assert!(outcomes.iter().all(|o| o.dry_run));
}

#[test]
fn install_skill_dry_run_explicit_dest_single_target() {
    // commands.py:455-457 — --dest 显式目录,覆盖 target 路径解析,单 outcome.
    let dest = PathBuf::from("/custom/skills/team-agent");
    let opts = skill_opts(SkillTarget::Codex, Some(dest.clone()), true);
    let outcomes = install_skill(&opts).expect("dry-run explicit dest");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].dest, SkillDestDir(dest));
    assert!(outcomes[0].dry_run);
}

#[test]
#[ignore = "REAL-MACHINE-E2E: real copytree + stale diff removal (fixes dirs_exist_ok=True residue)"]
fn install_skill_real_copy_removes_stale_files() {
    // 修 commands.py:480 dirs_exist_ok 残留:Rust 拷前清旧 SKILL,记录 removed_stale.
    let opts = skill_opts(SkillTarget::Codex, Some(PathBuf::from("/tmp/ta-skill-real")), false);
    let outcomes = install_skill(&opts).expect("real install-skill");
    assert!(!outcomes[0].dry_run);
    // 真路径下若有旧残留,removed_stale 非空 (具体值依 fixture).
}

// ───────────────────────────────────────────────────────────────────────
// uninstall — install.mjs:109-130。安全护栏:
//   - 默认 purge_runtime=false → purged_runtime=false,不删 workspace/.team.
//   - purge_runtime=true 且检测有 team 在跑 → 拒绝:purge_refused_team_running=true
//     (或返 PurgeRefusedTeamRunning err — 取实现纪律,这里钉「不真删」)。
// 真删 / team-running 判定 #[ignore]。
// ───────────────────────────────────────────────────────────────────────

/// TEST-SUPPORT seed helper: build a workspace whose state.json projects a RUNNING team
/// (status "running" + a live coordinator pid). The team-running guard reads this projection.
/// Real impl (pure scaffolding, not a production fn) so the NEW-CONTRACT guard test has a
/// concrete fixture instead of a bare nonexistent path.
fn seed_workspace_with_running_team(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("ta-ws-running-{}-{}", std::process::id(), tag));
    let team_dir = ws.join(".team");
    std::fs::create_dir_all(&team_dir).expect("seed .team dir");
    // state.json shape mirrors step-5 state projection: a team marked running.
    let state = serde_json::json!({
        "teams": {
            "demo": { "status": "running", "coordinator_pid": std::process::id() }
        }
    });
    std::fs::write(
        team_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .expect("seed state.json");
    ws
}

/// TEST-SUPPORT seed helper: an idle workspace (state.json present, no running team).
fn seed_workspace_idle(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("ta-ws-idle-{}-{}", std::process::id(), tag));
    let team_dir = ws.join(".team");
    std::fs::create_dir_all(&team_dir).expect("seed .team dir");
    std::fs::write(
        team_dir.join("state.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "teams": {} })).unwrap(),
    )
    .expect("seed state.json");
    ws
}

#[test]
#[serial_test::serial(env)]
fn uninstall_default_does_not_purge_runtime() {
    // PORT-GOLDEN (install.mjs:127-129): default (no --purge-runtime) leaves runtime; the
    // "runtime directories are left ... for rollback" branch. Default must NOT purge
    // workspace/.team. With workspace seeded, default must STILL leave it untouched on disk.
    // Isolate HOME: uninstall() now removes ~/.codex|.claude skill dirs (reads HOME). Without an
    // isolated empty HOME this test would (a) delete the real user's skill dir and (b) race
    // p2_uninstall_removes_both_skill_dirs' HOME mutation → remove_dir_all NotFound flake under
    // parallel cargo. Shared ENV_LOCK_PKG serializes the two HOME-touching uninstall tests.
    let _g = ENV_LOCK_PKG.lock().unwrap_or_else(|p| p.into_inner());
    let home = std::env::temp_dir().join(format!("ta-uninst-default-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let _h = HomeGuard::set(&home);
    let ws = seed_workspace_idle("default-nopurge");
    let opts = UninstallOptions {
        prefix: Prefix(std::env::temp_dir().join(format!("ta-uninst-{}", std::process::id()))),
        purge_runtime: false,
        workspace: Some(ws.clone()),
    };
    let outcome = uninstall(&opts).expect("default uninstall");
    assert!(!outcome.purged_runtime, "default must NOT purge runtime");
    assert!(
        !outcome.purge_refused_team_running,
        "no purge requested → no refusal"
    );
    // SAFETY INVARIANT (card §uninstall 绝不默认删 workspace/.team): the seeded workspace
    // .team must still exist after a default uninstall.
    assert!(
        ws.join(".team").join("state.json").exists(),
        "default uninstall must NEVER delete workspace/.team"
    );
}

#[test]
#[ignore = "NEW-CONTRACT (Rust hardening, NOT a Python port-golden): real team-running guard \
            needs live state projection (step 5). Python install.mjs:123-128 has NO such guard \
            — it purges unconditionally on --purge-runtime. Confirmed with gate w59ds828k: this \
            is intentional hardening backed by card §uninstall prose 'pass --purge-runtime only \
            when no teams are running.' Marked NEW-CONTRACT, not PORT."]
fn uninstall_purge_refused_when_team_running_NEW_CONTRACT() {
    // NEW-CONTRACT safety guard: purge_runtime=true but workspace projects a RUNNING team →
    // REFUSE purge (purge_refused_team_running=true OR PurgeRefusedTeamRunning err). The
    // seeded workspace .team MUST survive regardless. This behavior is NOT in install.mjs.
    let ws = seed_workspace_with_running_team("guard");
    let opts = UninstallOptions {
        prefix: Prefix(std::env::temp_dir().join("ta-uninst-guard")),
        purge_runtime: true,
        workspace: Some(ws.clone()),
    };
    match uninstall(&opts) {
        Ok(o) => {
            assert!(!o.purged_runtime, "must not purge while team running");
            assert!(o.purge_refused_team_running, "must set refusal flag");
        }
        Err(PackagingError::PurgeRefusedTeamRunning(refused_ws)) => {
            assert_eq!(refused_ws, ws, "refusal must name the running workspace");
        }
        Err(other) => panic!("expected refusal, got {other:?}"),
    }
    // Hard invariant: refused purge must leave the workspace fully intact.
    assert!(
        ws.join(".team").join("state.json").exists(),
        "refused purge must NEVER delete workspace/.team"
    );
}

#[test]
#[ignore = "REAL-MACHINE-E2E: PORT-GOLDEN — --purge-runtime with NO running team really removes \
            the runtime root (install.mjs:123-126 unconditional rmSync). File-system side effect."]
fn uninstall_purge_runtime_idle_workspace_purges_PORT_GOLDEN() {
    // PORT-GOLDEN (install.mjs:123-126): --purge-runtime DOES purge when no team is running.
    // This is the faithful Python behavior (the guard above is the only Rust addition).
    let ws = seed_workspace_idle("port-purge");
    let opts = UninstallOptions {
        prefix: Prefix(std::env::temp_dir().join("ta-uninst-port-purge")),
        purge_runtime: true,
        workspace: Some(ws.clone()),
    };
    let outcome = uninstall(&opts).expect("purge on idle workspace");
    assert!(outcome.purged_runtime, "idle + --purge-runtime → purged");
    assert!(!outcome.purge_refused_team_running, "no team → no refusal");
}

// ───────────────────────────────────────────────────────────────────────
// install / update — install.mjs:48-95。
//   - install 首装:InstallReport.replace == None (无二进制替换).
//   - update:replace == Some(..) (有原子替换;失败回滚到 .previous,bug-084 同源).
//   - installer 默认 skill_target = All (install.mjs:74 `--target all`).
// 全副作用 #[ignore] (写 bin / 拷 skill / 探 PATH / 跑 doctor) — clean-install E2E.
// ───────────────────────────────────────────────────────────────────────

fn install_opts(skill_target: SkillTarget) -> InstallOptions {
    InstallOptions {
        prefix: Prefix(PathBuf::from("/home/u/.local")),
        self_binary: PathBuf::from("/proc/self/exe"),
        skill_target,
    }
}

#[test]
#[ignore = "REAL-MACHINE-E2E: clean-install writes bin + copies skill + runs doctor + probes PATH"]
fn install_first_time_has_no_binary_replace() {
    // install.mjs:48 install 入口 — 首装无 replace.
    let opts = install_opts(SkillTarget::All);
    let report = install(&opts).expect("clean install");
    assert!(report.replace.is_none(), "first install must NOT replace");
    // installer 默认装两个 skill (--target all).
    assert_eq!(report.skills.len(), 2);
    // 版本 == 单一真相源.
    assert_eq!(report.version, Version::current());
}

#[test]
#[ignore = "REAL-MACHINE-E2E: atomic binary replace + .previous backup + rollback (bug-084 同源)"]
fn update_performs_atomic_binary_replace() {
    // install.mjs:60-66 — update 有 dest→backup + tmp→dest 原子替换.
    let opts = install_opts(SkillTarget::All);
    let report = update(&opts).expect("update");
    match report.replace {
        Some(AtomicReplaceOutcome::Replaced { .. })
        | Some(AtomicReplaceOutcome::ReplacedCrossDevice { .. }) => {}
        other => panic!("update must replace binary, got {other:?}"),
    }
}

#[test]
fn atomic_replace_outcome_serde_tag_outcome() {
    // #[serde(tag = "outcome")] — RolledBack carries restored_from + error.
    let o = AtomicReplaceOutcome::RolledBack {
        restored_from: PathBuf::from("/home/u/.local/bin/.previous"),
        error: "EXDEV".to_string(),
    };
    let json = serde_json::to_string(&o).unwrap();
    assert!(json.contains("\"outcome\":\"rolled_back\""), "got: {json}");
    assert!(json.contains("\"error\":\"EXDEV\""), "got: {json}");
}

// ───────────────────────────────────────────────────────────────────────
// §84 — packaging 绝不触发 provider client / prompt / token.
// install_skill 只拷文件 (provider 调用计数 = 0)。此处以「无 provider 依赖」
// 的结构性断言代替运行时计数:dry-run install-skill 不应需要任何 provider opts。
// (真 mock-provider-call-count==0 断言归集成层,这里钉 dry-run 不读 provider.)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn install_skill_dry_run_is_pure_no_provider_state() {
    // §84:install-skill 只拷文件;dry-run 连文件都不动 → 纯函数式可重复.
    let opts = skill_opts(SkillTarget::Claude, None, true);
    let first = install_skill(&opts).expect("dry-run 1");
    let second = install_skill(&opts).expect("dry-run 2");
    assert_eq!(first, second, "dry-run install-skill must be deterministic & side-effect free");
}

// ───────────────────────────────────────────────────────────────────────
// CRLF / platform — wrapper 内容在 Python 是 sh wrapper (LF)。Rust 单二进制
// 后无 sh wrapper;但 PATH 诊断的 path_entries 分隔在 Windows 用 ';'。
// 此处钉 diagnose_path 在空 PATH 时 path_entries==0 (bincheck.mjs:43 三元)。
// ───────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "REAL-MACHINE-E2E: needs to override process PATH env to empty/Windows-delimited"]
fn diagnose_path_empty_path_has_zero_entries() {
    // bincheck.mjs:43 — searchPath ? split.length : 0;空 PATH → 0 entries.
    // (真改 process env PATH 影响并行测试,故 ignore;实现层应支持注入 PATH.)
    let bin = BinDir(PathBuf::from("/anything"));
    let hint = diagnose_path(&bin).expect("diagnose empty path");
    if let PathHint::NotOnPath { diagnostic, .. } = hint {
        assert_eq!(diagnostic.path_entries, 0);
    } else {
        panic!("empty PATH → NotOnPath");
    }
}

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════

static ENV_LOCK_PKG: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct HomeGuard {
    prev: Option<String>,
}
impl HomeGuard {
    fn set(home: &Path) -> Self {
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", home);
        Self { prev }
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

// P1 — update() must perform a REAL atomic replace (rename dest→.previous), not fabricate
// a Replaced outcome whose backup file never exists (install.mjs:60-66; bug-084).
#[test]
fn p2_update_creates_real_atomic_replace_backup() {
    let base = std::env::temp_dir().join(format!("ta-p2-update-{}", std::process::id()));
    let prefix = base.join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let dest = prefix.join("bin").join("team-agent");
    std::fs::write(&dest, b"OLD BINARY").unwrap(); // pre-existing bin to back up
    let self_bin = base.join("team-agent-new");
    std::fs::write(&self_bin, b"NEW BINARY").unwrap();

    let opts = InstallOptions {
        prefix: Prefix(prefix.clone()),
        self_binary: self_bin,
        skill_target: SkillTarget::All,
    };
    let report = update(&opts).unwrap();
    let backup = match report.replace {
        Some(AtomicReplaceOutcome::Replaced { backup }) => backup,
        other => panic!("update must report a Replaced atomic replace, got {other:?}"),
    };
    assert!(
        backup.exists(),
        "update() must actually rename dest→.previous; the claimed backup file must exist on disk"
    );
}

// P1 — uninstall() must remove BOTH ~/.codex/skills/team-agent and ~/.claude/skills/team-agent
// and record them (install.mjs:115-122). Current returns removed_skill_dirs empty and leaves
// the dirs on disk.
#[test]
#[serial_test::serial(env)]
fn p2_uninstall_removes_both_skill_dirs() {
    let _g = ENV_LOCK_PKG.lock().unwrap_or_else(|p| p.into_inner());
    let base = std::env::temp_dir().join(format!("ta-p2-uninst-{}", std::process::id()));
    let home = base.join("home");
    let codex = home.join(".codex").join("skills").join("team-agent");
    let claude = home.join(".claude").join("skills").join("team-agent");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(codex.join("SKILL.md"), b"x").unwrap();
    std::fs::write(claude.join("SKILL.md"), b"x").unwrap();
    let _h = HomeGuard::set(&home);

    let opts = UninstallOptions {
        prefix: Prefix(base.join("prefix")),
        purge_runtime: false,
        workspace: None,
    };
    let out = uninstall(&opts).unwrap();
    assert_eq!(
        out.removed_skill_dirs.len(),
        2,
        "uninstall must remove BOTH ~/.codex and ~/.claude skill dirs"
    );
    assert!(!codex.exists(), "~/.codex skill dir must be removed");
    assert!(!claude.exists(), "~/.claude skill dir must be removed");
}
