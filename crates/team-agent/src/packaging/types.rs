//! packaging 共享类型 —— §19 散字符串 → enum / §3 路径·版本 newtype / typed Outcome·Report /
//! entry fn 入参 / PackagingError / 平台能力声明(纯函数 `platform_support`)。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── REUSE — committed model types(不重定义)──────────────────────────────────
// `Provider` 给 SkillTarget→provider 关联用(SkillTarget 是 installer 域的 codex/claude/all,
// 与 ProviderKind 对齐,见 card §拥有的类型「与 step 8 ProviderKind 对齐」)。
use crate::model::enums::Provider;
use crate::model::errors::ModelError;

// ── REUSE — step 3 db schema/migration(doctor --fix-schema / migration·repair 实体在此)──
// packaging 只转调:doctor 把 schema_diagnosis 结论转成 typed DoctorStatus;repair 走 fix_schema_layout。
use crate::db::migration::{Diagnosis, FixResult};
use crate::db::DbError;

// ===========================================================================
// §19 散字符串态 → 穷尽 enum
// ===========================================================================

/// installer 子命令(`install.mjs:20-30` argv 散字符串 `install/update/doctor/uninstall/help`)。
/// `unknown command` 走 exit 2 —— Rust 侧由 clap(step 14)在解析层挡掉,此 enum 是穷尽 match 无 fallthrough。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerCommand {
    Install,
    Update,
    Doctor,
    Uninstall,
    Help,
}

/// skill 安装目标(`cli/parser.py:443` choices `codex/claude/all`)。`_skill_dest_dir` 据此选
/// `~/.codex` vs `~/.claude`(`commands.py:467-472`)。**散字符串必须 enum**,与 step 8 Provider 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTarget {
    Codex,
    Claude,
    /// GitHub Copilot CLI(0.3.x 新增 provider)。skill 落 `~/.copilot/skills/team-agent`
    /// —— 实证:copilot bundle skill 源枚举含 `personal-copilot (~/.copilot/skills)`,
    /// config dir = `~/.copilot`(可被 `$COPILOT_HOME` 覆盖),manifest 约定 `**/SKILL.md`
    /// 同 claude/codex(见 `.team/artifacts/copilot-probe/`)。
    Copilot,
    All,
}

impl SkillTarget {
    /// `All` fan-out 的单目标全集(表驱动唯一真相源:新增 provider 只改这里,
    /// install/uninstall/install-skill 三处共用,杜绝漏装/漏卸)。
    pub const SINGLE_TARGETS: [SkillTarget; 3] =
        [SkillTarget::Codex, SkillTarget::Claude, SkillTarget::Copilot];

    /// 单目标 → 对应 provider(`All` 无单一 provider → `None`)。与 [`Provider`] 对齐,防散字符串再生。
    pub fn provider(self) -> Option<Provider> {
        match self {
            Self::Codex => Some(Provider::Codex),
            Self::Claude => Some(Provider::ClaudeCode),
            Self::Copilot => Some(Provider::Copilot),
            Self::All => None,
        }
    }

    /// `_skill_dest_dir`:`~/.codex|.claude|.copilot/skills/team-agent`(`All` fan-out 全集,非单 dir → None)。
    pub fn dest_dir(self, home: &Path) -> Option<SkillDestDir> {
        match self {
            Self::Codex => Some(SkillDestDir(home.join(".codex").join("skills").join("team-agent"))),
            Self::Claude => Some(SkillDestDir(home.join(".claude").join("skills").join("team-agent"))),
            Self::Copilot => Some(SkillDestDir(home.join(".copilot").join("skills").join("team-agent"))),
            Self::All => None,
        }
    }
}

/// `doctor` 的 typed 结论(取代 `install.mjs:89-94` 的 exit-code + stdout `doctor: ok`/`has blockers`
/// 猜测)。`team-agent doctor --json`(`commands.py:218-260`)结构化为此,**typed,非 exit-code 猜**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum DoctorStatus {
    /// 自检全过(schema 无 layout drift + 各 gate 通过)。
    Ok,
    /// 有阻断项 —— 携带逐条 blocker(schema drift / orphan / comms gate 失败等)。
    HasBlockers { blockers: Vec<Blocker> },
}

/// doctor 单条阻断项。来源分类:packaging 不拥有实体逻辑,只把 step 3/11/12 的结论归一为此 typed 项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub source: BlockerSource,
    /// 人读说明(对应 Python 各 gate 的 reason/error 字段)。
    pub detail: String,
}

/// blocker 来源(穷尽 —— 标明 doctor 实体在哪一 step,packaging 仅转调)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerSource {
    /// step 3:`team.db` 物理 layout drift(`schema_diagnosis().layout_diffs` 非空)。
    SchemaLayoutDrift,
    /// step 12:孤儿 coordinator(`orphan_gate`)。// 实体在 step 12,placeholder 转调。
    OrphanCoordinator,
    /// step 11:comms selftest 未过(`run_comms_selftest`)。// 实体在 step 11,placeholder 转调。
    CommsGate,
    /// installer 自身:bin 不在 PATH / 不可执行(`bincheck.mjs` PATH-not-found 诊断的等价)。
    PathNotConfigured,
}

// ===========================================================================
// §3 路径/版本 newtype(语义不同,禁混传)
// ===========================================================================

/// **单一真相源**版本(`CARGO_PKG_VERSION`)。修掉 `pyproject.toml(0.1.4)` vs `package.json(0.2.11)`
/// 双源漂移 —— 禁手抄第二处。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(pub String);

impl Version {
    /// 编译期注入的唯一版本(`env!("CARGO_PKG_VERSION")`)。
    pub fn current() -> Self {
        Self(env!("CARGO_PKG_VERSION").to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// wrapper 安装前缀(`install.mjs:51` 默认 `~/.local`;`<prefix>/bin` 落 bin)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix(pub PathBuf);

/// `<prefix>/bin`(`install.mjs:52`)。PATH 诊断针对此目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinDir(pub PathBuf);

/// skill 落地目录(`~/.codex|.claude/skills/team-agent`,`commands.py:469-471`)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillDestDir(pub PathBuf);

// 注:`RuntimeRoot` / `VersionedRuntimePath`(`runtimeRoot/<version>` 三态目录)在 Python 单二进制后
// **大概率消失**(无 Python 源树要复制);Rust 版「原子替换 + rollback」作用于**二进制本身**,故由
// [`AtomicReplacePlan`] 承载语义,不保留 RuntimeRoot newtype(card §拥有的类型)。

// ===========================================================================
// release 产物矩阵 / 平台能力如实声明(§8;能力门归 step 9,packaging 只声明覆盖面)
// ===========================================================================

/// release 目标平台(各平台静态二进制)。`Backend::Tmux`/`Pty`(step 9)能力门在 step 9,
/// 此 enum 只用于「release 矩阵覆盖面」声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTarget {
    MacosAarch64,
    MacosX8664,
    LinuxX8664,
    LinuxAarch64,
    /// Windows 原生(WezTerm/ConPTY 后端,非 tmux);见下 [`PlatformSupport`]。
    WindowsX8664,
}

/// 平台支持等级(§8:如实声明,不假装兼容)。
///
/// 0.5.x Windows portability CR C-1 (P0) 引入 `PreviewCompileOnly`:
/// Windows 目前 `cargo check --target x86_64-pc-windows-msvc` **仍红**
/// (三处 non-Unix fallback + platform 层未接入 + Batch 1-4 未完成);
/// 保持 `Native` 会构成 MUST-NOT-13 假绿承诺 → 用户装 Windows 二进制
/// 会真报错。降级为 `PreviewCompileOnly` 明示"当前 Windows 尚未通过
/// 编译门,Batch 6/7 真机订阅测通过前不承诺 Native"。
/// Truth source: `.team/artifacts/0.5.x-windows-portability-cr-verdict.md` §C-1。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "level")]
pub enum PlatformSupport {
    /// 原生一等(macOS/Linux tmux)。
    Native,
    /// 需 WSL + tmux backend(显式标要求,不假装原生)。
    RequiresWslTmux { note: String },
    /// Windows 移植进行中,`cargo check` 仍红或 Batch 6/7 真机订阅测未过。
    /// 用户不应期待可运行的 `.exe`;`preview_gate` 字段说明当前挡位
    /// (`compile_gate` = Batch 0-4 编译门,`fake_provider_smoke` = Batch 6,
    /// `subscription_realmachine` = Batch 7)。
    /// 该档位不承诺可用,仅承诺"正在移植 + 每 Batch 有 CI 编译门追踪"。
    PreviewCompileOnly { preview_gate: String, note: String },
    /// 当前 release 矩阵不覆盖。
    Unsupported { reason: String },
}

/// release 矩阵中某目标平台的支持等级(§8 如实声明;能力门归 step 9)。纯函数。
pub fn platform_support(target: ReleaseTarget) -> PlatformSupport {
    match target {
        ReleaseTarget::MacosAarch64
        | ReleaseTarget::MacosX8664
        | ReleaseTarget::LinuxX8664
        | ReleaseTarget::LinuxAarch64 => PlatformSupport::Native,
        // 0.5.x Windows portability CR C-1 (P0):
        // `cargo check --target x86_64-pc-windows-msvc` 是当前挡位;
        // Batch 6 fake-provider smoke + Batch 7 真机订阅测通过后
        // 才升回 `Native`。历史 tag 期间 Windows 曾声明为 Native 但
        // 从未通过编译门,构成 MUST-NOT-13 假绿承诺 → 本 batch 降级。
        ReleaseTarget::WindowsX8664 => PlatformSupport::PreviewCompileOnly {
            preview_gate: "compile_gate".to_string(),
            note: "Windows native port in progress; `cargo check --target x86_64-pc-windows-msvc` \
                   is the current gate. See \
                   `.team/artifacts/0.5.x-windows-portability-survey-design.md` §Batch 0-6 \
                   and CR verdict §C-1 for burn-down status. Promote back to `Native` only \
                   after Batch 6 (fake-provider smoke) + Batch 7 (subscription real-machine) \
                   pass on the SSH host."
                .to_string(),
        },
    }
}

// ===========================================================================
// typed Outcome / Report(取代 install.mjs console.log 自由文本 + engine 三态)
// ===========================================================================

/// `install`/`update` 的结构化结果(取代 `install.mjs:83-94` 自由文本行;`--json` mode serde)。
/// **无 engine/fallback/fallback_reason 字段**(card §陷阱:Rust 全量后无双引擎)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    /// 落地的 bin 路径(`<binDir>/team-agent`)。// REAL-MACHINE-E2E: 真写需文件系统。
    pub installed_bin: PathBuf,
    pub version: Version,
    /// 二进制原子替换的结果(install=首装无 replace;update=有 replace + 可能 rollback)。
    pub replace: Option<AtomicReplaceOutcome>,
    /// SKILL 安装结果(per-target)。
    pub skills: Vec<SkillInstallOutcome>,
    /// 装完跑一次 doctor 的结论(`install.mjs:89` `doctor --json`)。
    pub doctor: DoctorStatus,
    /// PATH 提示/诊断(bin 是否在 PATH;不在则带 [`PathDiagnostic`])。
    pub path_hint: PathHint,
}

/// PATH 提示(`install.mjs:87` `ensure <binDir> is on PATH` + `bincheck.mjs` not-found 诊断)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PathHint {
    /// bin 已在 PATH。
    OnPath { bin_dir: PathBuf },
    /// bin 不在 PATH —— 携带诊断(保留 WSL/`.npmrc` 等价提示)。// REAL-MACHINE-E2E: 真探 PATH。
    NotOnPath { bin_dir: PathBuf, diagnostic: PathDiagnostic },
}

/// 「bin 不在 PATH」诊断(`bincheck.mjs:39-65` 自由 console.error → typed struct)。
/// Rust 版无 npm,但 PATH-not-found 提示要保留(下载即跑也要提示 PATH/可执行位)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDiagnostic {
    pub init_cwd: PathBuf,
    /// cwd 在 WSL `/mnt/c/` 下(`bincheck.mjs:67` isWslMntC)。
    pub wsl_mnt_c: bool,
    /// 项目级 `.npmrc` 是否设了 `prefix`(`bincheck.mjs:56` summarizeNpmrc);Rust 无 npm 路径 → `None`。
    pub npmrc_prefix: Option<bool>,
    /// PATH 条目数(`bincheck.mjs:43`)。
    pub path_entries: usize,
    /// bin 文件是否有可执行位(下载即跑场景的等价检查)。
    pub executable_bit_set: bool,
}

/// 单个 SKILL 安装结果(`commands.py:_install_skill_to` 返回 dict → typed)。
/// **修 `dirs_exist_ok=True` 残留**:`removed_stale` 记录拷前清掉的陈旧文件(Python 不清 → 残留)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInstallOutcome {
    pub target: SkillTarget,
    pub source: PathBuf,
    pub dest: SkillDestDir,
    /// `--dry-run`(`commands.py:477`)→ 不落地,仅报告将拷的源/目标。
    pub dry_run: bool,
    /// 拷前清理掉的旧 SKILL 残留文件(diff 删除;Python 不清这一类)。// REAL-MACHINE-E2E: 真写。
    pub removed_stale: Vec<PathBuf>,
}

/// 二进制原子替换结果(bug-084 同源:rename 必须 Result + 跨卷 fallback + 回滚)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum AtomicReplaceOutcome {
    /// `dest→backup` rename + `tmp→dest` rename 成功(同卷快路径)。// REAL-MACHINE-E2E.
    Replaced { backup: PathBuf },
    /// 跨卷 `EXDEV` → copy+fsync+rename fallback 成功。// REAL-MACHINE-E2E: 需跨卷真机。
    ReplacedCrossDevice { backup: PathBuf },
    /// 替换失败 → 已回滚到 `.previous`(原 dest 仍可用)。// REAL-MACHINE-E2E.
    RolledBack { restored_from: PathBuf, error: String },
}

/// `uninstall` 结果(`install.mjs:109-130`)。默认保留 runtime/workspace(有 team 在跑勿 purge)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallOutcome {
    /// 移除的 wrapper/bin。// REAL-MACHINE-E2E: 真删需文件系统。
    pub removed_bins: Vec<PathBuf>,
    /// 移除的 skill 目录(`~/.codex|.claude/skills/team-agent`)。
    pub removed_skill_dirs: Vec<SkillDestDir>,
    /// 默认 false;仅 `--purge-runtime` 且确认无 team 在跑才 true(`install.mjs:128`)。
    pub purged_runtime: bool,
    /// 因检测到 team 在跑而拒绝 purge(安全护栏:绝不默认删 workspace/.team)。
    pub purge_refused_team_running: bool,
}

/// schema migration / repair 转调结果(`doctor --fix-schema` → step 3 `fix_schema_layout`)。
/// 直接转 step 3 的 [`FixResult`](不重新散字符串化);此 enum 是 packaging 侧的「转调外壳」。
#[derive(Debug, Clone)]
pub enum MigrationOutcome {
    /// 已是最新 / 无 drift —— 无需迁移。
    UpToDate { diagnosis: Diagnosis },
    /// 执行了 schema layout 重建(转 step 3 FixResult::Fixed)。
    Migrated { fix: FixResult },
    /// 撞活跃锁,拒绝且不写备份(转 step 3 FixResult::Blocked)。
    Blocked { reason: String },
}

// ===========================================================================
// entry fn 入参(installer 四子命令的 typed 选项;取代 install.mjs parseOptions 散字符串)
// ===========================================================================

/// `install`/`update` 选项(`install.mjs:132` parseOptions)。**无 `--python`**(单二进制不再 resolve
/// python —— card §PythonResolution「整体删除」;此类「找不到/找错 python」故障消失是 §1 重写核心收益)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOptions {
    /// `--prefix`(默认 `~/.local`)。
    pub prefix: Prefix,
    /// 当前进程二进制源(用于原子替换 update 的 `tmp`);install 首装时是「自拷贝」源。
    pub self_binary: PathBuf,
    /// install-skill 的目标(installer 默认 `All`,`install.mjs:74`)。
    pub skill_target: SkillTarget,
}

/// `uninstall` 选项(`install.mjs:109`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOptions {
    pub prefix: Prefix,
    /// `--purge-runtime`(`install.mjs:148`)。默认 false。
    pub purge_runtime: bool,
    /// purge 前据此 workspace 判定有无 team 在跑(经 state 投影;`None` = 不检查,纯 bin/skill 卸载)。
    pub workspace: Option<PathBuf>,
}

/// `install-skill` 选项(`commands.py:451` + `parser.py:442`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstallOptions {
    pub target: SkillTarget,
    /// `--dest`(显式目标目录;不能与 `--target all` 组合)。
    pub dest: Option<PathBuf>,
    /// `--dry-run`。
    pub dry_run: bool,
    /// repo `skills/team-agent/` 源(`commands.py:452` `repo_root()/skills/team-agent`)。
    pub source: PathBuf,
}

/// `doctor` 选项(`commands.py:218` + `parser.py:340-351`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorOptions {
    pub workspace: PathBuf,
    /// `--gate orphans|comms`(`None` = schema + 默认自检)。
    pub gate: Option<DoctorGate>,
    /// `--fix`(需配 `--gate`;`commands.py:220` `--fix requires --gate`)。
    pub fix: bool,
    /// `--cleanup-orphans` / `--confirm`(转 step 12)。
    pub cleanup_orphans: bool,
    pub confirm: bool,
}

/// doctor gate(`parser.py` choices + `commands.py:222/234`)。穷尽,无 fallthrough(`unknown doctor gate` → Err)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorGate {
    Orphans,
    Comms,
}

// ===========================================================================
// 错误(daemon/CLI entry 返 rich Result<Report, PackagingError>;§10)
// ===========================================================================

#[derive(Debug, Error)]
pub enum PackagingError {
    /// 文件系统副作用失败(写 bin / 拷 skill / 删除)。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// 二进制原子替换失败且回滚也失败(bug-084 同源最坏情形;携带最终错误)。
    #[error("atomic binary replace failed and rollback failed: {0}")]
    ReplaceFailed(String),
    /// 跨卷 rename(`EXDEV`)且 copy fallback 也失败。
    #[error("cross-device binary replace fallback failed: {0}")]
    CrossDeviceReplaceFailed(String),
    /// 非法选项组合(`--dest` + `--target all`;`--fix` 无 `--gate`)。对应 Python `TeamAgentError`。
    #[error("invalid options: {0}")]
    InvalidOptions(String),
    /// 未知 doctor gate(`commands.py:235` `unknown doctor gate`)。
    #[error("unknown doctor gate: {0}")]
    UnknownGate(String),
    /// PATH/可执行位诊断本身失败(无法读 PATH 等)。
    #[error("path diagnostic failed: {0}")]
    PathDiagnostic(String),
    /// uninstall 拒绝 purge(检测到 team 在跑;非真错误而是安全护栏,但走 Err 强制调用方处理)。
    #[error("refused to purge runtime: a team appears to be running under {0}")]
    PurgeRefusedTeamRunning(PathBuf),
    /// 平台不支持当前操作(§8 如实声明;不假装兼容)。
    #[error("unsupported platform for operation: {0}")]
    UnsupportedPlatform(String),
    /// step 3 schema/migration 转调错误。
    #[error("schema: {0}")]
    Db(#[from] DbError),
    /// step 5 state(repair-state / team-running 判定)转调错误。
    #[error("state: {0}")]
    State(String),
    /// model 校验/解析转调(版本解析等)。
    #[error(transparent)]
    Model(#[from] ModelError),
}
