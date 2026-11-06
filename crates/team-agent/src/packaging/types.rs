//!
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
    pub const SINGLE_TARGETS: [SkillTarget; 3] = [
        SkillTarget::Codex,
        SkillTarget::Claude,
        SkillTarget::Copilot,
    ];


    ///
    /// `_skill_dest_dir`:`~/.codex|.claude|.copilot/skills/team-agent`(`All` fan-out 全集,非单 dir → None)。
    pub fn dest_dir(self, home: &Path) -> Option<SkillDestDir> {
        match self {
            Self::Codex => Some(SkillDestDir(
                home.join(".codex").join("skills").join("team-agent"),
            )),
            Self::Claude => Some(SkillDestDir(
                home.join(".claude").join("skills").join("team-agent"),
            )),
            Self::Copilot => Some(SkillDestDir(
                home.join(".copilot").join("skills").join("team-agent"),
            )),
            Self::All => None,
        }
    }
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
    ///
    /// 编译期注入的唯一版本(`env!("CARGO_PKG_VERSION")`)。
    pub fn current() -> Self {
        Self(env!("CARGO_PKG_VERSION").to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}



/// skill 落地目录(`~/.codex|.claude/skills/team-agent`,`commands.py:469-471`)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillDestDir(pub PathBuf);

// 注:`RuntimeRoot` / `VersionedRuntimePath`(`runtimeRoot/<version>` 三态目录)在 Python 单二进制后
// **大概率消失**(无 Python 源树要复制);Rust 版「原子替换 + rollback」作用于**二进制本身**,故由
// [`AtomicReplacePlan`] 承载语义,不保留 RuntimeRoot newtype(card §拥有的类型)。

// ===========================================================================
// release 产物矩阵 / 平台能力如实声明(§8;能力门归 step 9,packaging 只声明覆盖面)
// ===========================================================================




// ===========================================================================
// typed Outcome / Report(取代 install.mjs console.log 自由文本 + engine 三态)
// ===========================================================================




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




// ===========================================================================
// entry fn 入参(installer 四子命令的 typed 选项;取代 install.mjs parseOptions 散字符串)
// ===========================================================================



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



// ===========================================================================
// 错误(daemon/CLI entry 返 rich Result<Report, PackagingError>;§10)
// ===========================================================================

#[derive(Debug, Error)]
pub enum PackagingError {
    /// 文件系统副作用失败(写 bin / 拷 skill / 删除)。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// 非法选项组合(`--dest` + `--target all`;`--fix` 无 `--gate`)。对应 Python `TeamAgentError`。
    #[error("invalid options: {0}")]
    InvalidOptions(String),
    /// step 3 schema/migration 转调错误。
    #[error("schema: {0}")]
    Db(#[from] DbError),
    /// model 校验/解析转调(版本解析等)。
    #[error(transparent)]
    Model(#[from] ModelError),
}
