//! ---
//! purpose: claude 家族的 fork 验证——在预算内轮询「精确快照路径」与「provider 侧 projects 路径」两处，任一处出现可读的新 backing 即出证明
//! contract:
//!   provides:
//!     - name: verify_claude_fork
//!       what: 双路径轮询验证，成功给 ContextForkProof(captured_via=context_fork_verified)
//!   requires:
//!     - name: crate::provider::session_scan::claude::projects_dir_for_cwd
//!       what: 第二条探测路径由 HOME + spawn_cwd 推导出 ~/.claude/projects 下的目录
//!     - name: super::ContextBackingSnapshot
//!       what: 判「文件变过」的基线
//! boundary:
//!   - 只服务 Provider::Claude / ClaudeCode
//!   - 文件名必须精确等于 <expected_session_id>.jsonl，不做同目录最新文件回落
//!   - 不解析会话正文语义，只验「首行可解析 JSON」与「sessionId 记录(若有)一致」
//!   - 未在 deadline 内满足条件即 Timeout，不降级放行
//! maturity: wired
//! ---
use super::*;

/// ---
/// purpose: 轮询等待 claude fork 的新 transcript 落盘并验明身份
/// params:
///   provider: 写进证明的 provider 标记(Claude 或 ClaudeCode)
///   source_session_id: 源会话;新会话等于它即不出证明
///   plan: 必须带 expected_session_id，否则立即拒绝
///   before: spawn 前基线，用于判 stamp 变化
///   expected_backing_path: 必需的精确快照路径;文件名不等于 <expected>.jsonl 即拒绝
///   spawn_cwd: 用于推导第二条 provider 侧探测路径
///   deadline: 轮询预算，每轮间隔 50ms
/// returns: 证明中 backing_path 是命中的那一条路径，attribution_confidence 固定 "high"
/// errors: Rejected(CaptureFailed) 当 plan 缺 expected id / 缺快照路径 / 文件名不匹配;Timeout 当预算内两条路径都没满足
/// contract:
///   provides:
///     - name: verify_claude_fork
///       what: 只读文件元数据与首行/sessionId 记录，不写任何文件
/// boundary:
///   - 两条路径的严格程度不同:快照路径用 is_none_or(无 sessionId 记录时视为匹配)，provider 路径用 is_some_and(必须读到 sessionId)
///   - 不排除 .team/logs/events.jsonl 之外的无关写入方——硬绑定只有「文件名等于 expected uuid」这一条
/// ---
pub(super) fn verify_claude_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    spawn_cwd: &Path,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let expected = plan.expected_session_id.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no expected session id".to_string(),
        )
    })?;
    let path = expected_backing_path.ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no exact snapshot backing".to_string(),
        )
    })?;
    let expected_name = format!("{}.jsonl", expected.as_str());
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(ProviderError::CaptureFailed(format!(
            "context_fork_unverified: Claude snapshot backing does not match expected session {}",
            expected.as_str()
        ))
        .into());
    }
    let started = std::time::Instant::now();
    loop {
        if let Some(stamp) = file_stamp(path) {
            let changed = before.files.get(path).is_none_or(|old| *old != stamp);
            let observed_matches =
                session_id_from_jsonl(path).is_none_or(|observed| observed == expected.as_str());
            if changed && readable_jsonl(path) && observed_matches && expected != source_session_id
            {
                return Ok(ContextForkProof {
                    provider,
                    source_session_id: source_session_id.clone(),
                    new_session_id: expected.clone(),
                    backing_path: path.to_path_buf(),
                    captured_via: "context_fork_verified".to_string(),
                    attribution_confidence: "high".to_string(),
                    managed_backing_root: None,
                });
            }
        }
        if let Some(provider_path) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|home| {
                crate::provider::session_scan::claude::projects_dir_for_cwd(&home, spawn_cwd)
            })
            .map(|root| root.join(&expected_name))
        {
            let observed_matches = session_id_from_jsonl(&provider_path)
                .is_some_and(|observed| observed == expected.as_str());
            let changed = file_stamp(&provider_path)
                .is_some_and(|stamp| before.files.get(&provider_path) != Some(&stamp));
            if changed
                && readable_jsonl(&provider_path)
                && observed_matches
                && expected != source_session_id
            {
                return Ok(ContextForkProof {
                    provider,
                    source_session_id: source_session_id.clone(),
                    new_session_id: expected.clone(),
                    backing_path: provider_path,
                    captured_via: "context_fork_verified".to_string(),
                    attribution_confidence: "high".to_string(),
                    managed_backing_root: None,
                });
            }
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ContextForkTermination::Timeout {
        provider,
        deadline_ms: deadline.as_millis(),
    })
}
