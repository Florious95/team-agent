//! ---
//! purpose: context-fork 的验证总闸——按 provider 分派「新会话 backing 确实生成了」的证明，拿不到证明就超时或拒绝，绝不假绿
//! contract:
//!   provides:
//!     - name: ContextBackingSnapshot
//!       what: spawn 前对 provider backing 根下 .jsonl 的 (len, mtime) 基线快照，用于判「变过」
//!     - name: verify_context_fork
//!       what: 只有 copilot 一条验证路径，成功给 ContextForkProof
//!     - name: context_fork_convergence_deadline
//!       what: 每个 provider 的收敛预算(一律 5s)
//!     - name: ContextForkProof
//!       what: 已验证的 fork 证明:新旧 session id、backing 路径、captured_via、归属置信度
//!     - name: ContextForkTermination
//!       what: 两种失败:Timeout(未在预算内看到新 backing) 与 Rejected(provider 侧错误)
//!   requires:
//!     - name: crate::provider::CommandPlan
//!       what: expected_session_id 与 provider_projects_root 两个关键输入都来自 plan
//! boundary:
//!   - 只回答「fork 有没有产生一个可读的、不等于源会话的新 backing」，不回答会话内容对不对
//!   - 不创建/改写 backing
//!   - 未证明即 Timeout/Rejected，绝不退化成「假定成功」
//!   - gemini / fake 没有可验证 backing，一律直接 Rejected
//!   - Claude Code / Codex / Grok / Pi worker 只走 contract leaf 的原生 new-seat fork；遗留 worker 行一律 Rejected
//! maturity: wired
//! ---
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::enums::Provider;
use crate::provider::{CommandPlan, ProviderError, SessionId};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContextForkTermination {
    #[error(
        "context_fork_unverified: {provider:?} produced no readable NEW session backing within {deadline_ms}ms"
    )]
    Timeout {
        provider: Provider,
        deadline_ms: u128,
    },
    #[error(transparent)]
    Rejected(#[from] ProviderError),
}

mod outcome;
pub(crate) use outcome::{
    observe_context_fork, transition_pending_context_fork, ContextForkOutcome, PendingContextFork,
};

/// ---
/// purpose: 给出该 provider 等待 fork backing 落盘的收敛预算
/// params:
///   provider: 目标 provider;全枚举穷举，无兜底臂
/// returns: 一律 5s(各 provider 仍逐一列出，新增 provider 必须显式决定)
/// contract:
///   provides:
///     - name: context_fork_convergence_deadline
///       what: 纯查表，不读时钟、不读磁盘
/// boundary:
///   - 只给预算数值，不负责在预算内轮询;超时语义由 ContextForkOutcome::Pending 承接
/// ---
pub(crate) fn context_fork_convergence_deadline(provider: Provider) -> Duration {
    // Expiration is consumed by ContextForkOutcome::Pending(PendingContextFork).
    match provider {
        Provider::Claude
        | Provider::ClaudeCode
        | Provider::Codex
        | Provider::Copilot
        | Provider::Grok
        | Provider::Pi
        | Provider::GeminiCli
        | Provider::Fake => Duration::from_secs(5),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextForkProof {
    pub provider: Provider,
    pub source_session_id: SessionId,
    pub new_session_id: SessionId,
    pub backing_path: PathBuf,
    pub captured_via: String,
    pub attribution_confidence: String,
    pub managed_backing_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextBackingSnapshot {
    files: BTreeMap<PathBuf, FileStamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl ContextBackingSnapshot {
    /// ---
    /// purpose: 在 fork spawn 之前对 provider backing 根做一次 .jsonl 基线快照，事后据此判断哪些文件「变过」
    /// params:
    ///   provider: 决定 backing 根的默认位置(plan 未给 provider_projects_root 时按 HOME 推导)
    ///   plan: 优先取 plan.provider_projects_root;隔离根存在时不碰用户全局目录
    /// returns: 根下递归到底的 path → (len, modified) 映射;根不可读时是空映射，不报错
    /// contract:
    ///   provides:
    ///     - name: capture
    ///       what: 只读元数据(len+mtime)，不打开文件正文
    /// boundary:
    ///   - 只收 .jsonl 后缀;copilot 的 session-store.db 不在快照内，其证明走 sqlite 查询另算
    ///   - 目录读失败静默跳过——快照是「变没变」的参照物，不是完整性断言
    /// ---
    pub(crate) fn capture(provider: Provider, plan: &CommandPlan) -> Self {
        let root = provider_backing_root(provider, plan);
        let files = jsonl_files(&root);
        Self { files }
    }
}

/// ---
/// purpose: fork 后按 provider 分派验证，只有拿到「新会话 backing 可读且不等于源会话」的实证才返回证明
/// params:
///   provider: 只有 copilot(sqlite 行存在) 一条路，其余一律拒绝
///   source_session_id: 被 fork 的源会话 id;新会话等于它即判失败
///   plan: 提供 expected_session_id 与隔离 backing 根
///   before: spawn 前的 ContextBackingSnapshot 基线，用于判「文件变过」
///   expected_backing_path: 精确快照路径;copilot 不使用
///   spawn_cwd: 保留调用形状
///   spawned_at: 保留调用形状
///   deadline: 轮询预算，来自 context_fork_convergence_deadline
/// returns: ContextForkProof——新旧 session id、backing 路径、captured_via、attribution_confidence
/// errors: Timeout 表示预算内没看到可验证的新 backing;Rejected 包装 ProviderError(缺 expected id、路径不匹配、无可验证 backing 的 provider)
/// contract:
///   provides:
///     - name: verify_context_fork
///       what: 分派 + 兜底拒绝;本函数自身不轮询，轮询在各 provider 分支内
/// boundary:
///   - 不修改 backing、不写 state、不发事件
///   - _source_agent_id / agent_id 当前不参与任何判定
///   - 非 copilot 的 provider 一律 CaptureFailed，绝不返回「无法验证但放行」
/// ---
pub(crate) fn verify_context_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    _before: &ContextBackingSnapshot,
    _expected_backing_path: Option<&Path>,
    _source_agent_id: &str,
    _agent_id: &str,
    _spawn_cwd: &Path,
    _spawned_at: &str,
    _deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    if provider == Provider::Copilot {
        return verify_copilot_fork(source_session_id, plan).map_err(Into::into);
    }
    Err(ProviderError::CaptureFailed(format!(
        "context_fork_unverified: {provider:?} has no verifiable fork backing"
    ))
    .into())
}

fn verify_copilot_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
) -> Result<ContextForkProof, ProviderError> {
    let expected = plan.expected_session_id.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: copilot plan has no expected session id".to_string(),
        )
    })?;
    if expected == source_session_id {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: copilot NEW session equals source".to_string(),
        ));
    }
    let root = plan.provider_projects_root.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: copilot plan has no isolated backing root".to_string(),
        )
    })?;
    let state_dir = root.join("session-state").join(expected.as_str());
    let db_path = root.join("session-store.db");
    if !state_dir.is_dir() || !db_path.is_file() {
        return Err(ProviderError::CaptureFailed(format!(
            "context_fork_unverified: copilot backing missing under {}",
            root.display()
        )));
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| ProviderError::CaptureFailed(error.to_string()))?;
    let found: i64 = conn
        .query_row(
            "select count(*) from sessions where id = ?1",
            [expected.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| ProviderError::CaptureFailed(error.to_string()))?;
    if found != 1 {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: copilot NEW session is absent from session-store".to_string(),
        ));
    }
    // Copilot's isolated database row is its synchronous fork proof; the
    // pre-spawn materialization baseline is never accepted by transcript scan.
    Ok(ContextForkProof {
        provider: Provider::Copilot,
        source_session_id: source_session_id.clone(),
        new_session_id: expected.clone(),
        backing_path: db_path,
        captured_via: "copilot_store_fork_verified".to_string(),
        attribution_confidence: "high".to_string(),
        managed_backing_root: Some(root.clone()),
    })
}

fn provider_backing_root(provider: Provider, plan: &CommandPlan) -> PathBuf {
    if let Some(root) = plan.provider_projects_root.as_ref() {
        return root.clone();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    match provider {
        Provider::Copilot => home.join(".copilot").join("session-state"),
        Provider::Claude | Provider::ClaudeCode | Provider::Codex | Provider::Grok | Provider::Pi => {
            PathBuf::new()
        }
        Provider::GeminiCli | Provider::Fake => home,
    }
}

fn jsonl_files(root: &Path) -> BTreeMap<PathBuf, FileStamp> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                if let Some(stamp) = file_stamp(&path) {
                    files.insert(path, stamp);
                }
            }
        }
    }
    files
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}
