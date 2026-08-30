//! ---
//! purpose: codex rollout 的时间与归属过滤——用「创建时间」而非 mtime 判新旧，用 rollout 头里的 cwd 判是不是本席位的
//! contract:
//!   provides:
//!     - name: parse_spawned_at
//!       what: RFC3339 文本 → SystemTime，解析不了给 None(调用方据此 fail-closed 清空候选)
//!     - name: truncate_to_uuid_precision
//!       what: 把时间截到毫秒，与 uuid-v7 里能表达的精度对齐，避免亚毫秒误差把自己的会话判成"太旧"
//!     - name: rollout_created_at
//!       what: 三级取创建时间:uuid-v7 时间戳 → 头记录 created_at → 文件名里的时间戳
//!     - name: apply_expected_session_filter
//!       what: 有 pending id 时只留 session id 精确相等的候选
//!     - name: retain_spawn_cwd
//!       what: 只留 rollout 头记录里 cwd 与席位 spawn_cwd 等价的候选
//!   requires:
//!     - name: super::common
//!       what: 读头、解析记录、record_cwd、paths_equivalent 均复用 common
//!     - name: chrono
//!       what: RFC3339 解析
//! boundary:
//!   - 只服务 Provider::Codex
//!   - 一律只读:不写盘、不改 plan、不改 state
//!   - 创建时间刻意不取文件 mtime——邻席活动会抬升 mtime，把别人的会话冒充成新的
//!   - 头窗口之外的记录读不到;三级取值全失败时 rollout_created_at 给 None，调用方按"不通过"处理
//! maturity: wired
//! ---
/// ---
/// purpose: 把持久化在 state 上的 spawned_at 文本解析成时间点
/// params:
///   raw: RFC3339 时间串
/// returns: 解析成功给 UTC SystemTime;失败给 None
/// contract:
///   provides:
///     - name: parse_spawned_at
///       what: 纯解析，无 I/O、不读系统时钟
/// boundary:
///   - 只认 RFC3339;不接受 epoch 秒、不做宽松格式回落
///   - 返回 None 的处置由调用方定:common::apply_spawned_at_filter 选择清空候选(fail-closed)
/// ---
pub(super) fn parse_spawned_at(raw: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| std::time::SystemTime::from(dt.with_timezone(&chrono::Utc)))
}

/// ---
/// purpose: 把时间点截到毫秒，与 uuid-v7 能表达的精度对齐
/// params:
///   timestamp: 待截断的时间点
/// returns: 同一毫秒的起点;早于 UNIX 纪元或毫秒数溢出 u64 时 None
/// contract:
///   provides:
///     - name: truncate_to_uuid_precision
///       what: 纯算术，无 I/O
/// boundary:
///   - 只向下截断，绝不向上取整——否则会把自己刚开的会话判成"早于 spawn"
///   - 只在「Codex 且有 expected id」的比较里使用，不是通用时间归一
/// ---
pub(super) fn truncate_to_uuid_precision(
    timestamp: std::time::SystemTime,
) -> Option<std::time::SystemTime> {
    let since_epoch = timestamp
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(
        u64::try_from(since_epoch.as_millis()).ok()?,
    ))
}

/// ---
/// purpose: 取 codex rollout 的创建时间，作为「这条存档是不是本次 spawn 之后产生的」的判据
/// params:
///   path: rollout 文件路径
/// returns: 三级依次尝试:文件名尾部 uuid-v7 的时间戳 → 头记录里的 created_at → 文件名中的时间戳;全失败给 None
/// contract:
///   provides:
///     - name: rollout_created_at
///       what: 只读文件名与头 64KB
/// boundary:
///   - 刻意不取文件 mtime:mtime 会被后续追加与邻席活动抬升，不是创建时间
///   - uuid 分支要求版本位为 '7';非 v7 的 uuid 直接落到下一级
///   - 返回 None 时调用方按"不满足时间窗"处理，不做放行
/// ---
pub(super) fn rollout_created_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
    created_at_from_rollout_uuid(path)
        .or_else(|| created_at_from_rollout_head(path))
        .or_else(|| created_at_from_rollout_filename(path))
}

/// ---
/// purpose: 有 pending id 时把候选收窄到 session id 精确相等的那些
/// params:
///   context: 无 expected_session_id 时原样返回，不做任何过滤
///   out: 候选列表，按值传入并就地 retain
/// returns: 过滤后的候选;无一条命中则空向量
/// contract:
///   provides:
///     - name: apply_expected_session_filter
///       what: 纯内存过滤，无 I/O
/// boundary:
///   - 判据是 session id 全等;session_id 为 None 的候选一律剔除
///   - 与 claude 同名函数语义不同:此处未命中即空，不退到"身份阳性子集"
/// ---
pub(super) fn apply_expected_session_filter(
    context: &super::CaptureSessionContext,
    mut out: Vec<super::CapturedSessionCandidate>,
) -> Vec<super::CapturedSessionCandidate> {
    let Some(expected) = context.expected_session_id.as_ref() else {
        return out;
    };
    out.retain(|candidate| {
        candidate
            .captured
            .session_id
            .as_ref()
            .is_some_and(|session| session.as_str() == expected.as_str())
    });
    out
}

/// ---
/// purpose: 只保留 rollout 头里记着的 cwd 与本席位 spawn_cwd 等价的候选，挡掉其它工作目录的会话
/// params:
///   context: 提供 spawn_cwd 基准
///   out: 就地过滤的候选列表
/// contract:
///   provides:
///     - name: retain_spawn_cwd
///       what: 逐个候选读头 64KB 后比对 cwd
/// boundary:
///   - 无 rollout_path、读不出头、头里一条 cwd 都没有 → 剔除(fail-closed)
///   - 等价判定走 common::paths_equivalent，它把「记录 cwd 的父目录等于 spawn_cwd」也算等价
///   - 只看头窗口内的记录;窗口之外的 cwd 记录看不见
/// ---
pub(super) fn retain_spawn_cwd(
    context: &super::CaptureSessionContext,
    out: &mut Vec<super::CapturedSessionCandidate>,
) {
    out.retain(|candidate| {
        let Some(path) = candidate.captured.rollout_path.as_ref() else {
            return false;
        };
        let Ok(text) =
            super::common::read_head_text(path.as_path(), super::common::CAPTURE_HEAD_BYTES)
        else {
            return false;
        };
        super::common::parse_session_records(&text)
            .iter()
            .filter_map(super::common::record_cwd)
            .any(|cwd| {
                super::common::paths_equivalent(std::path::Path::new(&cwd), &context.spawn_cwd)
            })
    });
}

fn created_at_from_rollout_uuid(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    let uuid = stem.get(stem.len().checked_sub(36)?..)?;
    if uuid.as_bytes().get(14).copied() != Some(b'7') {
        return None;
    }
    let millis =
        u64::from_str_radix(&format!("{}{}", uuid.get(..8)?, uuid.get(9..13)?), 16).ok()?;
    std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(millis))
}

fn created_at_from_rollout_head(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let text = super::common::read_head_text(path, super::common::CAPTURE_HEAD_BYTES).ok()?;
    super::common::parse_session_records(&text)
        .iter()
        .find_map(record_created_at)
        .and_then(|raw| parse_spawned_at(&raw))
}

fn record_created_at(record: &serde_json::Value) -> Option<String> {
    record
        .get("created_at")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            record
                .get("session_meta")
                .and_then(|v| v.get("payload"))
                .or_else(|| record.get("payload"))
                .and_then(|v| v.get("created_at"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

fn created_at_from_rollout_filename(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let name = path.file_name()?.to_str()?;
    let start = name.find("rollout-")? + "rollout-".len();
    let stamp = name.get(start..start + 19)?;
    if stamp.as_bytes().get(10).copied() != Some(b'T') {
        return None;
    }
    let raw = format!(
        "{}T{}:{}:{}+00:00",
        &stamp[0..10],
        &stamp[11..13],
        &stamp[14..16],
        &stamp[17..19]
    );
    parse_spawned_at(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::session_scan::{common, CaptureSessionContext, CapturedSessionCandidate};
    use crate::provider::Provider;
    use crate::provider::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};
    use std::path::PathBuf;

    #[test]
    fn parse_spawned_at_rfc3339_roundtrips_and_rejects_junk() {
        assert!(parse_spawned_at("2026-06-10T21:40:00+00:00").is_some());
        assert!(parse_spawned_at("not-a-date").is_none());
        assert!(parse_spawned_at("").is_none());
    }

    fn candidate(path: PathBuf, session_id: &str) -> CapturedSessionCandidate {
        CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: Some(SessionId::new(session_id)),
                rollout_path: Some(RolloutPath::new(path)),
                captured_via: CaptureVia::FsWatch,
                attribution_confidence: Confidence::High,
                spawn_cwd: PathBuf::from("/tmp"),
            },
            embedded_agent_id: None,
            positive_agent_id_match: false,
            agent_path_match: false,
        }
    }

    #[test]
    fn spawned_at_filter_rejects_prior_round_and_accepts_current_rollout() {
        let dir =
            std::env::temp_dir().join(format!("ta-codex-spawn-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stale =
            dir.join("rollout-2026-07-21T22-45-40-11111111-1111-4111-8111-111111111111.jsonl");
        let fresh =
            dir.join("rollout-2026-07-21T23-20-01-22222222-2222-4222-8222-222222222222.jsonl");
        std::fs::write(&stale, "{}\n").unwrap();
        std::fs::write(&fresh, "{}\n").unwrap();
        let context = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: dir.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2026-07-21T23:20:00+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: None,
        };
        let mut out = vec![
            candidate(stale, "11111111-1111-4111-8111-111111111111"),
            candidate(fresh.clone(), "22222222-2222-4222-8222-222222222222"),
        ];
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.rollout_path.as_ref().unwrap().as_path(),
            fresh
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawned_at_filter_fails_closed_with_an_invalid_boundary() {
        let mut out = vec![candidate(PathBuf::from("/tmp/old.jsonl"), "old")];
        let mut context = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: PathBuf::from("/tmp"),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert_eq!(out.len(), 1, "legacy direct scan has no cohort boundary");

        context.spawned_at = Some("not-a-date".to_string());
        common::apply_spawned_at_filter(Provider::Codex, &context, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn unique_window_uses_created_at_not_active_sibling_mtime() {
        let dir =
            std::env::temp_dir().join(format!("ta-codex-created-window-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stale =
            dir.join("rollout-2026-07-22T03-12-13-11111111-1111-4111-8111-111111111111.jsonl");
        let fresh =
            dir.join("rollout-2026-07-22T03-15-30-22222222-2222-4222-8222-222222222222.jsonl");
        std::fs::write(&stale, "{}\n").unwrap();
        std::fs::write(&fresh, "{}\n").unwrap();
        let context = CaptureSessionContext {
            agent_id: "clone".to_string(),
            spawn_cwd: dir.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2026-07-22T03:15:29+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: None,
        };
        let mut out = vec![
            candidate(stale, "11111111-1111-4111-8111-111111111111"),
            candidate(fresh.clone(), "22222222-2222-4222-8222-222222222222"),
        ];
        common::apply_spawn_time_window_if_unique(Provider::Codex, &context, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.rollout_path.as_ref().unwrap().as_path(),
            fresh
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
