//! ---
//! purpose: codex 的 fork 两件事——把源 rollout 改写成新身份的快照物化出来(materialize)，再验证 CLI 确实认领了它(verify)
//! contract:
//!   provides:
//!     - name: materialize_codex_fork
//!       what: 读源 rollout 完整记录 → 改 session_meta.id 与 worker 身份 marker → no-clobber 硬链接发布 → 读回自证 → 把 plan 从 `codex fork` 改写成 `codex resume <新 id>`
//!     - name: CodexForkMaterialization
//!       what: 物化产物的 RAII 句柄:未 handoff 就 Drop 时删除目标文件
//!     - name: verify_codex_fork
//!       what: 有 expected id 时按精确路径+embedded 身份认领;无 expected id 时退到 legacy「唯一变过的新 rollout」路径
//!   requires:
//!     - name: crate::provider::session_scan
//!       what: 认领新 rollout 靠一次性候选扫描，不自己解析目录
//!     - name: crate::provider::adapter::ForkBackingMaterialization
//!       what: 句柄实现的对外 trait(path/handoff)
//! boundary:
//!   - 改写只动 session_meta.payload.id 与恰好一处 worker 身份 marker;二者数量不是恰好各 1 就整体拒绝，绝不部分改写
//!   - 发布用 create_new 临时文件 + hard_link，绝不覆盖已存在的目标路径
//!   - 只服务 Provider::Codex
//!   - 新 session id 是本地生成的 uuid-v7 形状串，不问 CLI 要
//! maturity: wired
//! ---
use super::*;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

#[derive(Debug)]
pub(crate) struct CodexForkMaterialization {
    path: PathBuf,
    keep: bool,
}

impl CodexForkMaterialization {
    /// ---
    /// purpose: 暴露已物化的目标 rollout 路径，供调用方拼 resume 参数与做 fork 验证
    /// returns: 物化目标文件的绝对路径;句柄还活着时该路径必然存在
    /// contract:
    ///   provides:
    ///     - name: path
    ///       what: 只借出路径，不转移所有权、不影响 Drop 时的删除决定
    /// boundary:
    ///   - 拿到路径不等于拿到保留承诺——不调 handoff 就仍会在 Drop 时被删
    /// ---
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn mark_handoff(&mut self) {
        self.keep = true;
    }
}

impl crate::provider::adapter::ForkBackingMaterialization for CodexForkMaterialization {
    fn path(&self) -> &Path {
        self.path()
    }

    fn handoff(&mut self) {
        self.mark_handoff();
    }
}

impl Drop for CodexForkMaterialization {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// ---
/// purpose: 为 codex fork 物化一份改写了会话身份的新 rollout，并把命令计划改成对该新会话的精确 resume
/// params:
///   source_path: 源 rollout 文件;新文件发布在它的同一父目录
///   source_session_id: 源会话 id;源快照的 session_meta.payload.id 必须与之相等，否则拒绝
///   source_agent_id: 源席位 id，用于定位待替换的 worker 身份 marker
///   target_agent_id: 目标席位 id，替换后的 marker 内容，并参与读回自证
///   plan: 就地改写;要求形如 `codex fork ... <source_session_id>`，会被改成 `codex resume ... <新 id>` 并设上 expected_session_id
/// returns: RAII 句柄——未 handoff 即 Drop 时删除新文件，避免留下半成品存档
/// errors: Io(源无父目录/源无完整 JSONL 记录/源某行非法 JSON/session_meta 缺 payload.id/id 与源不符/session_meta 或 marker 命中数不是恰好各 1/发布失败/读回身份不符);Command(plan 形状不是可转换的 codex fork)
/// contract:
///   provides:
///     - name: materialize_codex_fork
///       what: 写盘发生在此;读回校验失败会先删目标文件再返回错误
/// boundary:
///   - 只截到源文件最后一个换行处，绝不把半条正在写入的记录带进新快照
///   - 不改源文件
///   - 不启动进程、不调 codex CLI——只准备好 backing 与 argv
/// ---
pub(crate) fn materialize_codex_fork(
    source_path: &Path,
    source_session_id: &SessionId,
    source_agent_id: &str,
    target_agent_id: &str,
    plan: &mut CommandPlan,
) -> Result<CodexForkMaterialization, ProviderError> {
    let target_session_id = SessionId::new(codex_session_v7());
    let parent = source_path.parent().ok_or_else(|| {
        ProviderError::Io(format!(
            "codex source backing has no parent: {}",
            source_path.display()
        ))
    })?;
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S");
    let target_path = parent.join(format!(
        "rollout-{timestamp}-{}.jsonl",
        target_session_id.as_str()
    ));
    let source = complete_source_snapshot(source_path)?;
    let rewritten = rewrite_snapshot(
        &source,
        source_session_id,
        &target_session_id,
        source_agent_id,
        target_agent_id,
    )?;
    publish_target_no_clobber(&target_path, rewritten.as_bytes())?;
    if let Err(error) =
        validate_materialized_target(&target_path, &target_session_id, target_agent_id)
    {
        let _ = std::fs::remove_file(&target_path);
        return Err(error);
    }
    apply_resume_target(plan, source_session_id, &target_session_id)?;
    Ok(CodexForkMaterialization {
        path: target_path,
        keep: false,
    })
}

fn complete_source_snapshot(path: &Path) -> Result<String, ProviderError> {
    let file = File::open(path).map_err(|error| ProviderError::Io(error.to_string()))?;
    let visible_len = file
        .metadata()
        .map_err(|error| ProviderError::Io(error.to_string()))?
        .len();
    let mut bytes = Vec::new();
    file.take(visible_len)
        .read_to_end(&mut bytes)
        .map_err(|error| ProviderError::Io(error.to_string()))?;
    let Some(boundary) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Err(ProviderError::Io(
            "codex fork source has no complete JSONL record".to_string(),
        ));
    };
    bytes.truncate(boundary + 1);
    String::from_utf8(bytes).map_err(|error| ProviderError::Io(error.to_string()))
}

fn rewrite_snapshot(
    source: &str,
    source_session_id: &SessionId,
    target_session_id: &SessionId,
    source_agent_id: &str,
    target_agent_id: &str,
) -> Result<String, ProviderError> {
    let source_marker = format!("You are Team Agent worker `{source_agent_id}`");
    let target_marker = format!("You are Team Agent worker `{target_agent_id}`");
    let mut meta_count = 0_usize;
    let mut marker_count = 0_usize;
    let mut output = String::new();
    for (index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut record = serde_json::from_str::<serde_json::Value>(line).map_err(|error| {
            ProviderError::Io(format!(
                "codex fork source has invalid JSONL at line {}: {error}",
                index + 1
            ))
        })?;
        if record.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
            let id = record
                .get_mut("payload")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|payload| payload.get_mut("id"))
                .and_then(|id| id.as_str())
                .ok_or_else(|| {
                    ProviderError::Io(
                        "codex fork session_meta has no string payload.id".to_string(),
                    )
                })?;
            if id != source_session_id.as_str() {
                return Err(ProviderError::Io(
                    "codex fork session_meta id does not match source".to_string(),
                ));
            }
            record["payload"]["id"] =
                serde_json::Value::String(target_session_id.as_str().to_string());
            meta_count += 1;
        }
        marker_count += replace_exact_marker(&mut record, &source_marker, &target_marker);
        output.push_str(
            &serde_json::to_string(&record)
                .map_err(|error| ProviderError::Io(error.to_string()))?,
        );
        output.push('\n');
    }
    if meta_count != 1 || marker_count != 1 {
        return Err(ProviderError::Io(format!(
            "codex fork source identity is ambiguous: session_meta={meta_count}, marker={marker_count}"
        )));
    }
    Ok(output)
}

fn replace_exact_marker(value: &mut serde_json::Value, source: &str, target: &str) -> usize {
    match value {
        serde_json::Value::String(text) => {
            let count = text.matches(source).count();
            if count == 1 {
                *text = text.replacen(source, target, 1);
            }
            count
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|item| replace_exact_marker(item, source, target))
            .sum(),
        serde_json::Value::Object(fields) => fields
            .values_mut()
            .map(|item| replace_exact_marker(item, source, target))
            .sum(),
        _ => 0,
    }
}

fn publish_target_no_clobber(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    let parent = path.parent().ok_or_else(|| {
        ProviderError::Io(format!("codex target has no parent: {}", path.display()))
    })?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("codex-fork"),
        std::process::id()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::hard_link(&temp, path)?;
        File::open(parent)?.sync_all()?;
        std::fs::remove_file(&temp)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(ProviderError::Io(error.to_string()));
    }
    Ok(())
}

fn validate_materialized_target(
    path: &Path,
    session_id: &SessionId,
    target_agent_id: &str,
) -> Result<(), ProviderError> {
    let text =
        std::fs::read_to_string(path).map_err(|error| ProviderError::Io(error.to_string()))?;
    let actual = session_id_from_jsonl(path);
    let marker = format!("You are Team Agent worker `{target_agent_id}`");
    if actual.as_deref() != Some(session_id.as_str())
        || !path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(session_id.as_str()))
        || text.matches(&marker).count() != 1
    {
        return Err(ProviderError::Io(
            "codex fork target read-back identity mismatch".to_string(),
        ));
    }
    Ok(())
}

fn apply_resume_target(
    plan: &mut CommandPlan,
    source_session_id: &SessionId,
    target_session_id: &SessionId,
) -> Result<(), ProviderError> {
    if plan.argv.first().map(String::as_str) != Some("codex")
        || plan.argv.get(1).map(String::as_str) != Some("fork")
        || plan.argv.last().map(String::as_str) != Some(source_session_id.as_str())
    {
        return Err(ProviderError::Command(
            "codex fork command shape cannot be converted to exact resume".to_string(),
        ));
    }
    plan.argv[1] = "resume".to_string();
    *plan.argv.last_mut().expect("validated non-empty argv") =
        target_session_id.as_str().to_string();
    plan.expected_session_id = Some(target_session_id.clone());
    Ok(())
}

fn codex_session_v7() -> String {
    use sha2::Digest;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let mut hasher = sha2::Sha256::new();
    hasher.update(millis.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(
        COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_le_bytes(),
    );
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest[..10]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[4..6].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[6..8].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[8..10].try_into().expect("fixed slice")),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

/// ---
/// purpose: 验证 codex 已经在物化出来的那条 rollout 上开出了新会话，且该会话的 embedded 身份就是目标席位
/// params:
///   source_session_id: 源会话;目标等于它即拒绝
///   plan: 无 expected_session_id 时整体退到 legacy 路径(靠「唯一一条变过的新 rollout」认领)
///   before: spawn 前基线;legacy 路径用它排除源会话所在文件与未变文件
///   expected_backing_path: 有 expected id 时必需——候选路径必须逐字节等于它，不做同目录择新
///   agent_id: 候选的 embedded worker id 必须等于它，且 positive_agent_id_match 必须为真
///   spawn_cwd: 构造扫描上下文
///   spawned_at: 扫描的时间边界
///   deadline: 轮询预算，每轮间隔 50ms
/// returns: ContextForkProof，captured_via="context_fork_verified"、attribution_confidence="high"
/// errors: Rejected(CaptureFailed) 当目标等于源 / 缺物化目标路径;Rejected 亦透传扫描期的 ProviderError;Timeout 当预算内没有满足全部条件的候选
/// contract:
///   provides:
///     - name: verify_codex_fork
///       what: 只读;认领判据是路径精确相等 + session id 等于 expected + embedded 身份等于 agent_id 三条同时成立
/// boundary:
///   - 不写 backing、不改 plan
///   - legacy 路径(无 expected id)不做身份比对，只要求「不是源会话、不是基线里未变的文件、且全场恰好一条」——多于一条即不认领
/// ---
pub(super) fn verify_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let Some(expected) = plan.expected_session_id.as_ref() else {
        return verify_legacy_codex_fork(
            source_session_id,
            plan,
            before,
            agent_id,
            spawn_cwd,
            spawned_at,
            deadline,
        );
    };
    if expected == source_session_id {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: codex target session equals source".to_string(),
        )
        .into());
    }
    let expected_path = expected_backing_path.ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: codex plan has no materialized target backing".to_string(),
        )
    })?;
    let context = crate::provider::session_scan::CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(spawned_at.to_string()),
        expected_session_id: plan.expected_session_id.clone(),
        provider_projects_root: plan.provider_projects_root.clone(),
    };
    let started = std::time::Instant::now();
    loop {
        for candidate in
            crate::provider::session_scan::scan_session_candidates_once(Provider::Codex, &context)?
        {
            let Some(path) = candidate.captured.rollout_path.as_ref() else {
                continue;
            };
            if path.as_path() != expected_path {
                continue;
            }
            let Some(new_session_id) = candidate.captured.session_id else {
                continue;
            };
            if &new_session_id != expected || &new_session_id == source_session_id {
                continue;
            }
            if !candidate.positive_agent_id_match
                || candidate.embedded_agent_id.as_deref() != Some(agent_id)
            {
                continue;
            }
            return Ok(ContextForkProof {
                provider: Provider::Codex,
                source_session_id: source_session_id.clone(),
                new_session_id,
                backing_path: path.as_path().to_path_buf(),
                captured_via: "context_fork_verified".to_string(),
                attribution_confidence: "high".to_string(),
                managed_backing_root: None,
            });
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ContextForkTermination::Timeout {
        provider: Provider::Codex,
        deadline_ms: deadline.as_millis(),
    })
}

fn verify_legacy_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let mut excluded_session_ids = BTreeSet::new();
    excluded_session_ids.insert(source_session_id.as_str().to_string());
    let mut excluded_backing_paths = BTreeSet::new();
    for path in before.files.keys() {
        if session_id_from_jsonl(path).as_deref() == Some(source_session_id.as_str()) {
            excluded_backing_paths.insert(path.clone());
        }
    }
    let context = crate::provider::session_scan::CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(spawned_at.to_string()),
        expected_session_id: None,
        provider_projects_root: plan.provider_projects_root.clone(),
    };
    let started = std::time::Instant::now();
    loop {
        let current = jsonl_files(&provider_backing_root(Provider::Codex, plan));
        let mut matches = Vec::new();
        for candidate in
            crate::provider::session_scan::scan_session_candidates_once(Provider::Codex, &context)?
        {
            let Some(path) = candidate.captured.rollout_path.as_ref() else {
                continue;
            };
            if excluded_backing_paths.contains(path.as_path()) {
                continue;
            }
            let Some(stamp) = current.get(path.as_path()) else {
                continue;
            };
            if before.files.get(path.as_path()) == Some(stamp) {
                continue;
            }
            let Some(session_id) = candidate.captured.session_id else {
                continue;
            };
            if excluded_session_ids.contains(session_id.as_str()) {
                continue;
            }
            matches.push((session_id, path.as_path().to_path_buf()));
        }
        if let [(new_session_id, backing_path)] = matches.as_slice() {
            return Ok(ContextForkProof {
                provider: Provider::Codex,
                source_session_id: source_session_id.clone(),
                new_session_id: new_session_id.clone(),
                backing_path: backing_path.clone(),
                captured_via: "context_fork_verified".to_string(),
                attribution_confidence: "high".to_string(),
                managed_backing_root: None,
            });
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ContextForkTermination::Timeout {
        provider: Provider::Codex,
        deadline_ms: deadline.as_millis(),
    })
}
