//! ---
//! purpose: context-fork 的验证总闸——按 provider 分派「新会话 backing 确实生成了」的证明，拿不到证明就超时或拒绝，绝不假绿
//! contract:
//!   provides:
//!     - name: ContextBackingSnapshot
//!       what: spawn 前对 provider backing 根下 .jsonl 的 (len, mtime) 基线快照，用于判「变过」
//!     - name: verify_context_fork
//!       what: 按 provider 路由到 copilot/claude 两条验证路径，成功给 ContextForkProof
//!     - name: context_fork_convergence_deadline
//!       what: 每个 provider 各自的收敛预算(claude 45s / 其余 5s)
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
//!   - 已退役的 legacy codex 与 grok / gemini / fake 没有可验证 backing，一律直接 Rejected
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

mod claude;
mod outcome;
pub(crate) use outcome::{
    observe_context_fork, transition_pending_context_fork, ContextForkOutcome, PendingContextFork,
};

/// ---
/// purpose: 给出该 provider 等待 fork backing 落盘的收敛预算
/// params:
///   provider: 目标 provider;全枚举穷举，无兜底臂
/// returns: Claude/ClaudeCode 45s、其余(Codex/Copilot/Grok/GeminiCli/Fake) 5s
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
        Provider::Claude | Provider::ClaudeCode => Duration::from_secs(45),
        Provider::Codex | Provider::Copilot | Provider::Grok | Provider::Pi
        | Provider::GeminiCli | Provider::Fake => Duration::from_secs(5),
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
///   provider: 决定走 copilot(sqlite 行存在) / claude(轮询双路径) 两条路之一
///   source_session_id: 被 fork 的源会话 id;新会话等于它即判失败
///   plan: 提供 expected_session_id 与隔离 backing 根
///   before: spawn 前的 ContextBackingSnapshot 基线，用于判「文件变过」
///   expected_backing_path: 精确快照路径。claude 必需，缺失即拒绝;不做同目录猜测
///   spawn_cwd: claude 分支据此推导 provider 侧 projects 目录
///   _spawned_at: 保留签名，当前不参与判定
///   deadline: 轮询预算，来自 context_fork_convergence_deadline
/// returns: ContextForkProof——新旧 session id、backing 路径、captured_via、attribution_confidence
/// errors: Timeout 表示预算内没看到可验证的新 backing;Rejected 包装 ProviderError(缺 expected id、路径不匹配、无可验证 backing 的 provider)
/// contract:
///   provides:
///     - name: verify_context_fork
///       what: 分派 + 兜底拒绝;本函数自身不轮询，轮询在各 provider 分支内
/// boundary:
///   - 不修改 backing、不写 state、不发事件
///   - _source_agent_id / _agent_id 当前不参与任何判定
///   - 未列入两条路径的 provider 一律 CaptureFailed，绝不返回「无法验证但放行」
/// ---
pub(crate) fn verify_context_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    _source_agent_id: &str,
    _agent_id: &str,
    spawn_cwd: &Path,
    _spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    if provider == Provider::Copilot {
        return verify_copilot_fork(source_session_id, plan).map_err(Into::into);
    }
    if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return claude::verify_claude_fork(
            provider,
            source_session_id,
            plan,
            before,
            expected_backing_path,
            spawn_cwd,
            deadline,
        );
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
        Provider::Claude | Provider::ClaudeCode => home.join(".claude").join("projects"),
        Provider::Copilot => home.join(".copilot").join("session-state"),
        Provider::Codex | Provider::Pi => PathBuf::new(),
        Provider::Grok | Provider::GeminiCli | Provider::Fake => home,
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

fn readable_jsonl(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_string)
        })
        .is_some_and(|line| serde_json::from_str::<serde_json::Value>(&line).is_ok())
}

fn session_id_from_jsonl(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|record| {
            record
                .get("sessionId")
                .or_else(|| record.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    record
                        .get("session_meta")
                        .and_then(|value| value.get("payload"))
                        .or_else(|| record.get("payload"))
                        .and_then(|value| value.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_fork_proof_is_partitioned_by_exact_snapshot_path() {
        let root = std::env::temp_dir().join(format!(
            "ta-claude-fork-proof-partition-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".team/logs")).unwrap();
        let expected = SessionId::new("22222222-2222-4222-8222-222222222222");
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: Some(expected.clone()),
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Claude, &plan);
        std::fs::write(
            root.join(".team/logs/events.jsonl"),
            "{\"event\":\"noise\"}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("11111111-1111-4111-8111-111111111111.jsonl"),
            "{\"type\":\"sibling-snapshot\"}\n",
        )
        .unwrap();
        let expected_path = root.join(format!("{}.jsonl", expected.as_str()));
        std::fs::write(&expected_path, "{\"type\":\"own-snapshot\"}\n").unwrap();
        let proof = verify_context_fork(
            Provider::Claude,
            &SessionId::new("source-session"),
            &plan,
            &before,
            Some(&expected_path),
            "source",
            "fork",
            &root,
            "2026-07-22T00:00:00Z",
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(proof.new_session_id, expected);
        assert_eq!(proof.backing_path, expected_path);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn claude_fork_proof_never_uses_events_jsonl_as_backing() {
        let root = std::env::temp_dir().join(format!(
            "ta-claude-fork-proof-no-events-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".team/logs")).unwrap();
        let expected = SessionId::new("33333333-3333-4333-8333-333333333333");
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: Some(expected.clone()),
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Claude, &plan);
        std::fs::write(
            root.join(".team/logs/events.jsonl"),
            "{\"event\":\"noise\"}\n",
        )
        .unwrap();
        let missing = root.join(format!("{}.jsonl", expected.as_str()));
        let error = verify_context_fork(
            Provider::Claude,
            &SessionId::new("source-session"),
            &plan,
            &before,
            Some(&missing),
            "source",
            "fork",
            &root,
            "2026-07-22T00:00:00Z",
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.to_string().contains("context_fork_unverified"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
