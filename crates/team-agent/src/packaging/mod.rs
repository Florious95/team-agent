//! step 15 · packaging — release 产物布局 / 薄 self-install / migration·repair SKELETON (ROUND-0).
//!
//! Card: `docs/phase0/subsystems/15-packaging.md`.
//! Truth source (READ-ONLY snapshot `team-agent-public` @ v0.2.11 / `439bef8`):
//!   - `npm/install.mjs`                       (npx installer 主体:install/update/doctor/uninstall
//!     四子命令;原子升级 `copyTree→tmp` / `dest→.previous` rename / `tmp→dest` rename;写三个 sh
//!     wrapper;跑 `install-skill --target all`;跑 `doctor --json` 自检)
//!   - `npm/bincheck.mjs`                      (npm postinstall 钩子:bin 不在 PATH 时打印 WSL
//!     `/mnt/c` + 项目级 `.npmrc prefix` 诊断 —— Rust 版无 npm,但 PATH-not-found 诊断保留)
//!   - `scripts/install.py`                    (纯 Python 后备 installer:只写三个 sh wrapper,
//!     是 install.mjs 的子集)
//!   - `src/team_agent/rust_core.py`           (**前车之鉴**:engine `rust`/`rust_failed`/
//!     `python_fallback` 三态 + 静默 Python 回退 —— Rust 全量后整座桥删除,无此散字符串态)
//!   - `src/team_agent/cli/commands.py:451`    (`cmd_install_skill`:repo `skills/team-agent/`
//!     copytree 到 `~/.codex|.claude/skills/team-agent/`;`--dry-run`/`--dest`/`--target` 矩阵)
//!   - `src/team_agent/cli/commands.py:218`    (`cmd_doctor`:packaging 的自检入口;实体逻辑在
//!     step 3 schema_migration / step 11 comms selftest / step 12 coordinator·orphan gate —
//!     packaging 只「装完跑一次」并把结论转成 typed DoctorStatus)
//!   - `pyproject.toml` / `package.json`       (**版本号双源漂移** 0.1.4 vs 0.2.11 —— Rust 用
//!     `CARGO_PKG_VERSION` 单一真相源,禁手抄第二处)
//!
//! 职责(card §职责):把 1-14 的产物打包(各平台静态二进制)+ 落地(极薄 self-install:SKILL 拷贝 +
//! PATH 提示 + 跑 doctor)+ self-check + schema/state migration·repair 转调。**产品逻辑全在 step 1-14**
//! (§8 薄 shim 纪律);packaging 是最末步(15),几乎依赖全链但只调它们的「装/检」面。
//!
//! 铁律(card §bug/陷阱):
//!   - **bug-084 同源:rename 在 Windows/跨卷/占用时抛 PermissionError/EXDEV 没人接**。Rust 版
//!     原子替换二进制(`AtomicReplacePlan{dest,tmp,backup}`)必须 `Result` 强制处理 + 跨卷 fallback
//!     (copy+fsync+rename)+ 失败回滚到 `.previous`,**绝不裸 unwrap/rename**(本 module 的原子替换 /
//!     migration·repair 路径建议局部纪律等同 daemon 门,见 lib.rs §10 锁)。
//!   - **版本号双源漂移**:[`Version`] 单一真相源 = `env!("CARGO_PKG_VERSION")`,禁手抄第二处。
//!   - **engine 三态 + python_fallback 是反例**:Rust 全量后无双引擎、无 fallback;此 module **不存在**
//!     `engine`/`fallback`/`fallback_reason` 字段(§19 散字符串态打地鼠 + §10 不假绿要根除的)。
//!   - **`copytree(dirs_exist_ok=True)` 残留**:install-skill 叠拷使旧 SKILL 被删文件残留;Rust 版拷
//!     skill 前先清目标目录(或 diff 删除)—— [`SkillInstallOutcome`] 记录 removed_stale。
//!   - **uninstall 默认保留 runtime/workspace**:有 team 在跑时勿 purge;[`UninstallOutcome`] 默认
//!     `purged_runtime:false`,且**绝不**默认删 workspace/`.team`。
//!   - **§84 provider-client 规则**:packaging **绝不依赖任何 provider client crate**;`install-skill`
//!     只拷文件、`doctor` 只调 step 3/11/12 的 trait 入口(注入 mock 时 provider 调用计数 = 0);
//!     installer 路径**绝不触发任何 prompt/token 消耗**。
//!   - **平台能力如实声明**(§8):Windows 仅 tmux backend 时显式标 WSL+tmux 要求或「不支持原生」,
//!     不假装兼容(能力门归 step 9,packaging 只如实声明 release 矩阵覆盖面)。
//!
//! **real-machine / clean-install E2E 标注**:本 module 是 15 步中最薄、最平台门控的一层。下列入口的
//! 真实落地(文件系统副作用、跨卷 rename、SKILL 真拷、PATH 真探测、二进制原子替换)只能在 §9 验收阶梯的
//! **真机/容器 clean-install E2E**(`fixture_kind=real`,macOS/Linux clean machine、Windows/WSL 能力判定)
//! 中验证;纯逻辑部分(版本解析 / 路径布局 / DoctorStatus 转码 / SkillInstallPlan diff)可单测。带
//! `// REAL-MACHINE-E2E` 标记的字段/分支即「只能真机/容器验」的部分。
//!
//! ROUND-0:仅类型 + struct/enum + fn 签名。所有 body = `unimplemented!("step15 port: <what>")`。
//! fallible 路径返 `Result`/`Option`(§10 实现层禁 unwrap/expect/panic,签名先做成 fallible)。
//! daemon/CLI entry fns 返 rich `Result<Report, PackagingError>`。`#![deny(...)]` 由 leader 在集成时
//! 统一加(原子替换/migration·repair 段建议局部等同 daemon 门),本骨架不加。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/method 暂未被用;P2 porter 落实现时移除。
#![allow(dead_code, unused_imports, unused_variables, clippy::result_large_err, clippy::doc_overindented_list_items, clippy::doc_lazy_continuation, clippy::io_other_error)]
// §10:原子替换二进制 / migration·repair 路径实现层禁 unwrap/expect/panic(unimplemented!() stub 不被拦);
// tests 子模块各自 allow。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod install;
pub mod migrate;
pub mod repair;
pub mod types;

// ── 重新导出 module 根原本可见的全部条目(保持 `crate::packaging::X` 与测试 `super::*` 解析不变)──
pub use install::*;
pub use migrate::*;
pub use repair::*;
pub use types::*;

// ── REUSE — committed model types(测试经 `super::*` 引用 `Provider`;与原单文件根 import 等价)──
use crate::model::enums::Provider;

// ── REUSE — step 4 event_log(install/upgrade/repair 审计事件;原子替换的 rebuild/rollback 标记)──
use crate::event_log::EventLog;

// ── REUSE — step 5 state(uninstall「有 team 在跑勿删」判定经 state 投影;repair-state 转调)──
// `state` 操作 state.json = serde_json::Value;此处仅在 fn 签名内经全限定路径引用,避免未用顶层 import。

#[cfg(test)]
mod tests;
