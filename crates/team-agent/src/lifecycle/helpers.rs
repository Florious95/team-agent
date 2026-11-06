//! ---
//! purpose: runtime 快照的诊断性落盘，以及 plan 状态文件的路径与读写
//! contract:
//!   provides:
//!     - name: save_team_runtime_snapshot
//!       what: 把 runtime state 以带 not_authoritative 标记的诊断副本写到 legacy 每 session 路径
//!     - name: team_snapshot_path
//!       what: 给出某 session 的诊断快照路径
//!     - name: plan_state_path
//!       what: 给出某 plan 的状态文件路径
//!     - name: plan_lock_path
//!       what: 给出某 plan 的锁文件路径
//!     - name: read_plan_state
//!       what: 读取并反序列化 plan 状态
//!     - name: save_plan_state
//!       what: 序列化 plan 状态并经临时文件 rename 落盘
//!   depends:
//!     - crate::state::persist
//!     - crate::model::paths
//!     - std::fs
//! boundary:
//!   - 快照文件不是权威 runtime state，产品写路径不得调用它
//!   - plan 锁文件只被创建，本模块不对它加锁，也不提供并发互斥
//!   - 不解释 plan 语义，只做路径、序列化与落盘
//! maturity: wired
//! ---
//!
//! lifecycle::helpers —— runtime snapshot 原子写(bug-084)+ plan-state 路径/读取。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::*;


/// ---
/// purpose: 给出某 session 的诊断快照文件路径
/// params:
///   session_name: 原始 session 名，非字母数字与下划线点横线的字符会被替换
/// returns: runtime 目录下 teams 子目录中的 state.json 路径
/// ---
pub fn team_snapshot_path(workspace: &Path, session_name: &str) -> PathBuf {
    crate::model::paths::runtime_dir(workspace)
        .join("teams")
        .join(safe_snapshot_name(session_name))
        .join("state.json")
}


fn safe_snapshot_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = replaced.trim_matches(|c| matches!(c, '.' | '_' | '-'));
    if trimmed.is_empty() {
        "team".to_string()
    } else {
        trimmed.to_string()
    }
}
