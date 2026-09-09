//! ---
//! purpose: 在记录的 Pi 席位 session root 内按真实 header 捕获 exact JSONL backing
//! contract:
//!   provides:
//!     - name: scan_session_store
//!       what: 只读每个 JSONL 的首个完整 header，并按 id/cwd/spawned_at 返回候选
//!     - name: validate_exact_backing
//!       what: restart 前重验 exact path 的 id/cwd header
//! boundary:
//!   - 只递归 recorded provider_projects_root，不扫描 HOME 或消息正文
//!   - 不以文件名、mtime 或唯一文件替代 header 匹配
//! maturity: wired
//! ---

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};
use crate::provider::ProviderError;

use super::{CaptureSessionContext, CapturedSessionCandidate};

#[derive(serde::Deserialize)]
struct PiSessionHeader {
    #[serde(rename = "type")]
    record_type: String,
    version: u64,
    id: String,
    cwd: PathBuf,
    timestamp: String,
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn read_header(path: &Path) -> Option<PiSessionHeader> {
    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    let bytes = std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    if bytes == 0 || !first_line.ends_with('\n') {
        return None;
    }
    let header: PiSessionHeader = serde_json::from_str(first_line.trim_end()).ok()?;
    (header.record_type == "session" && header.version == 3).then_some(header)
}

fn header_matches(
    header: &PiSessionHeader,
    expected_session_id: &SessionId,
    spawn_cwd: &Path,
    spawned_at: Option<&str>,
) -> bool {
    if header.id != expected_session_id.as_str() || !paths_equal(&header.cwd, spawn_cwd) {
        return false;
    }
    let Ok(header_timestamp) = chrono::DateTime::parse_from_rfc3339(&header.timestamp) else {
        return false;
    };
    let Some(spawned_at) = spawned_at else {
        return true;
    };
    let Ok(spawned_at) = chrono::DateTime::parse_from_rfc3339(spawned_at) else {
        return false;
    };
    header_timestamp >= spawned_at
}

fn collect_jsonl(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), ProviderError> {
    let entries = std::fs::read_dir(root)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", root.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| ProviderError::Io(format!("{}: {error}", root.display())))?;
        let file_type = entry
            .file_type()
            .map_err(|error| ProviderError::Io(format!("{}: {error}", entry.path().display())))?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl(&path, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

/// ---
/// purpose: restart 前重验调用方给出的 exact Pi JSONL backing
/// returns: header type/version/id/cwd 全匹配时成功
/// errors: 文件缺失、header 不完整或 identity 不匹配时返回 ResumeUnavailable
/// ---
pub(crate) fn validate_exact_backing(
    path: &Path,
    expected_session_id: &SessionId,
    spawn_cwd: &Path,
) -> Result<(), ProviderError> {
    if !path.is_file() {
        return Err(ProviderError::ResumeUnavailable(format!(
            "Pi exact session backing is missing: {}",
            path.display()
        )));
    }
    let header = read_header(path).ok_or_else(|| {
        ProviderError::ResumeUnavailable(format!(
            "Pi exact session backing has no complete session header: {}",
            path.display()
        ))
    })?;
    if !header_matches(&header, expected_session_id, spawn_cwd, None) {
        return Err(ProviderError::ResumeUnavailable(format!(
            "Pi exact session backing header does not match id/cwd: {}",
            path.display()
        )));
    }
    Ok(())
}

/// ---
/// purpose: 仅在 recorded seat root 内递归捕获 Pi session header 候选
/// returns: 每个 id/cwd/timestamp 精确匹配的 JSONL 各返回一个候选
/// errors: root 内目录枚举失败时返回 ProviderError
/// ---
pub(super) fn scan_session_store(
    context: &CaptureSessionContext,
) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
    let (Some(root), Some(expected_session_id), Some(spawned_at)) = (
        context.provider_projects_root.as_deref(),
        context.expected_session_id.as_ref(),
        context.spawned_at.as_deref(),
    ) else {
        return Ok(Vec::new());
    };
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    collect_jsonl(root, &mut paths)?;
    paths.sort();
    Ok(paths
        .into_iter()
        .filter_map(|path| {
            let header = read_header(&path)?;
            header_matches(
                &header,
                expected_session_id,
                &context.spawn_cwd,
                Some(spawned_at),
            )
            .then(|| CapturedSessionCandidate {
                captured: CapturedSession {
                    session_id: Some(expected_session_id.clone()),
                    rollout_path: Some(RolloutPath::new(path)),
                    captured_via: CaptureVia::FsWatch,
                    attribution_confidence: Confidence::High,
                    spawn_cwd: context.spawn_cwd.clone(),
                },
                embedded_agent_id: None,
                positive_agent_id_match: false,
                agent_path_match: false,
            })
        })
        .collect())
}
