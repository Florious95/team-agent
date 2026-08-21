//! ---
//! purpose: 各 provider 共用的通用候选扫描与解析底座——枚举可能的会话文件、读头解析、按进程世代与身份 marker 过滤
//! contract:
//!   provides:
//!     - name: candidate_session_files
//!       what: 按 provider 枚举候选文件(隔离根 + spawn_cwd 递归 + provider HOME 目录)，去重排序并按 mtime 截到 300 条
//!     - name: parse_candidate_files
//!       what: 逐个读头 64KB 解析成 CapturedSessionCandidate，途中套 cwd 匹配、embedded 身份、claude cwd 字段与 leader marker 四道排除
//!     - name: apply_spawned_at_filter
//!       what: 剔除早于席位进程世代边界(spawned_at)的存档;spawned_at 存在但解析不了则清空候选
//!     - name: apply_spawn_time_window_if_unique
//!       what: 只有当"落在 spawn 之后"的候选恰好剩一条时才据此收窄，否则不动
//!     - name: sort_expected_first_if_needed
//!       what: 有 pending id 时把命中的候选排到最前
//!     - name: read_head_text
//!       what: 读文件头若干字节并截到最后一个完整行
//!     - name: parse_session_records
//!       what: 先按整体 JSON 解析(数组/单值)，失败再按 JSONL 逐行解析
//!     - name: embedded_team_agent_worker_id_from_text
//!       what: 从正文里抽出 "You are Team Agent worker `<id>`" 的 id，字符集受限校验
//!     - name: rollout_path_embedded_team_agent_worker_id
//!       what: 上一条的按路径版本(读头后再抽)
//!     - name: record_cwd
//!       what: 从记录里取 cwd(顶层 cwd 或 session_meta.payload.cwd / payload.cwd)
//!     - name: paths_equivalent
//!       what: 路径等价判定:相等、canonicalize 后相等、或左的父目录等于右
//!     - name: CAPTURE_HEAD_BYTES
//!       what: 统一的读头窗口 64KB
//!   requires:
//!     - name: crate::provider::helpers
//!       what: find_session_id 与 parse_jsonl_records
//!     - name: super::claude
//!       what: claude 家族的 projects 目录推导、cwd 字段判据与 leader marker 判据
//!     - name: super::codex
//!       what: spawned_at 解析、uuid 精度截断与 rollout 创建时间
//! boundary:
//!   - 一律只读:不写盘、不改 state、不发事件
//!   - 只产出候选与事实;归属最终由上游分配器裁定，本文件不写会话
//!   - spawn_cwd 递归最深 4 层，且跳过 .team 运行时目录(claude 的隔离根例外)
//!   - 候选上限 300 条，超出按 mtime 由新到旧截断——截掉的部分不留痕迹
//!   - "看起来像会话文件"的判据很宽(.json/.jsonl 后缀，或名字含 session/rollout/席位 id)，收窄靠后续 provider 专属过滤
//! maturity: wired
//! ---
use std::path::{Path, PathBuf};

use crate::provider::helpers::{find_session_id, parse_jsonl_records};
use crate::provider::types::{
    CaptureVia, CapturedSession, Confidence, ProviderError, RolloutPath, SessionId,
};
use crate::provider::Provider;

use super::{CaptureSessionContext, CapturedSessionCandidate};

pub(super) struct SessionCandidate {
    pub(super) path: PathBuf,
    requires_cwd_match: bool,
}

/// P2 (C-P2-2/3) / Python claude.py:300 — candidates are capped to the newest `cap`
/// by mtime (descending priority: old candidates must not crowd out new ones).
const CAPTURE_CANDIDATE_CAP: usize = 300;

/// P2 (C-P2-1): head window >= Python's 200-line read.
pub(super) const CAPTURE_HEAD_BYTES: u64 = 65_536;

/// ---
/// purpose: 枚举本席位可能对应的会话文件，产出待解析的候选路径表
/// params:
///   provider: 决定要不要去 ~/.codex/sessions 或 ~/.claude/projects 找
///   context: provider_projects_root(隔离根)、spawn_cwd(必扫)、agent_id(参与文件名匹配)
/// returns: 去重并排序后的候选;requires_cwd_match=false 的排前面，同组按路径字典序
/// errors: 只有 spawn_cwd 这一层根目录读不开才上抛 Io;provider HOME 侧目录读失败静默跳过
/// contract:
///   provides:
///     - name: candidate_session_files
///       what: 只列路径，不打开文件内容(截断时会取 mtime)
/// boundary:
///   - spawn_cwd 递归最深 4 层;工作区是用户代码仓，任何名含 session 的 json 都会被列进来
///   - .team 运行时目录一律排除，除非它就在 claude 的隔离根之下
///   - 超过 300 条按 mtime 由新到旧截断;被截掉的候选不产生任何信号
///   - Copilot / Grok / CursorAgent / GeminiCli / Fake 不走 HOME 分支——它们各有专属扫描
/// ---
pub(super) fn candidate_session_files(
    provider: Provider,
    context: &CaptureSessionContext,
) -> Result<Vec<SessionCandidate>, ProviderError> {
    let mut out = Vec::new();
    if let Some(root) = context.provider_projects_root.as_ref() {
        let allowed_team_root =
            matches!(provider, Provider::Claude | Provider::ClaudeCode).then_some(root.as_path());
        collect_optional_candidate_files(root, &context.agent_id, allowed_team_root, &mut out)?;
    }
    collect_candidate_files(
        &context.spawn_cwd,
        &context.agent_id,
        0,
        false,
        None,
        &mut out,
    )?;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        match provider {
            Provider::Codex => {
                collect_optional_candidate_files(
                    &home.join(".codex").join("sessions"),
                    &context.agent_id,
                    None,
                    &mut out,
                )?;
            }
            Provider::Claude | Provider::ClaudeCode => {
                if let Some(dir) = super::claude::projects_dir_for_cwd(&home, &context.spawn_cwd) {
                    collect_optional_candidate_files(&dir, &context.agent_id, None, &mut out)?;
                }
                collect_optional_candidate_files(
                    &home.join(".claude").join("projects"),
                    &context.agent_id,
                    None,
                    &mut out,
                )?;
            }
            Provider::Copilot
            | Provider::Grok
            | Provider::CursorAgent
            | Provider::GeminiCli
            | Provider::Fake => {}
        }
    }
    out.sort_by(|a, b| {
        a.requires_cwd_match
            .cmp(&b.requires_cwd_match)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.requires_cwd_match == b.requires_cwd_match);
    cap_candidates_by_mtime(&mut out, CAPTURE_CANDIDATE_CAP);
    Ok(out)
}

/// ---
/// purpose: 把候选路径读头解析成结构化候选，并在解析途中就滤掉明显不属于本席位的
/// params:
///   provider: 决定是否套用 claude 专属的两道排除
///   context: 提供 spawn_cwd 与 agent_id 两个比对基准
///   candidates: candidate_session_files 的产物
/// returns: 通过全部排除的候选;每条带 embedded 身份、positive_agent_id_match 与 agent_path_match
/// contract:
///   provides:
///     - name: parse_candidate_files
///       what: 每个文件只读头 64KB，且只取到最后一个完整行
/// boundary:
///   - 四道排除:读不出头 / 解析不出记录 → 跳过;requires_cwd_match 的候选 cwd 对不上 → 跳过;embedded 身份存在且不等于本席位 → 跳过;claude 家族有 session id 却无 cwd 字段、或含 leader marker → 跳过
///   - 无 session id 的候选不丢弃，降级为 FsMtimeFallback + Confidence::Low
///   - 身份判定用两套文本:正文 text 判 TEAM_AGENT_ID，未截行的原始字节判 embedded worker id
/// ---
pub(super) fn parse_candidate_files(
    provider: Provider,
    context: &CaptureSessionContext,
    candidates: Vec<SessionCandidate>,
) -> Vec<CapturedSessionCandidate> {
    let mut out = Vec::new();
    for candidate in candidates {
        let path = candidate.path;
        let Ok(head_bytes) = read_head_bytes(&path, CAPTURE_HEAD_BYTES) else {
            continue;
        };
        let text = complete_head_text(&head_bytes);
        let identity_text = String::from_utf8_lossy(&head_bytes);
        let records = parse_session_records(&text);
        if records.is_empty() {
            continue;
        }
        if candidate.requires_cwd_match
            && !provider_home_records_match_spawn_cwd(&records, &context.spawn_cwd)
        {
            continue;
        }
        let session_id = records.iter().find_map(find_session_id);
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && session_id.is_some()
            && !records.iter().any(super::claude::has_cwd_field)
        {
            continue;
        }
        let captured_via = if session_id.is_some() {
            CaptureVia::FsWatch
        } else {
            CaptureVia::FsMtimeFallback
        };
        let attribution_confidence = if session_id.is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        let embedded_agent_id = embedded_team_agent_worker_id_from_text(&identity_text);
        if embedded_agent_id
            .as_deref()
            .is_some_and(|id| id != context.agent_id.as_str())
        {
            continue;
        }
        let positive_agent_id_match = candidate_text_has_team_agent_id(&text, context)
            || embedded_agent_id.as_deref() == Some(context.agent_id.as_str());
        let agent_path_match = candidate_path_matches_agent_id(&path, context);
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && super::claude::records_have_leader_marker(&records)
        {
            continue;
        }
        out.push(CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: session_id.map(SessionId::new),
                rollout_path: Some(RolloutPath::new(path)),
                captured_via,
                attribution_confidence,
                spawn_cwd: context.spawn_cwd.clone(),
            },
            embedded_agent_id,
            positive_agent_id_match,
            agent_path_match,
        });
    }
    out
}

/// Reject provider files older than the persisted process-cohort boundary.
/// ---
/// purpose: 用席位的进程世代边界剔除上一代/别人留下的旧存档
/// params:
///   provider: Codex 走 rollout 创建时间，其余走文件 mtime
///   context: spawned_at 缺失则不过滤(legacy 行);expected id 存在且是 Codex 时把边界截到毫秒
///   out: 就地过滤的候选列表
/// contract:
///   provides:
///     - name: apply_spawned_at_filter
///       what: fail-closed —— spawned_at 存在但解析不了(或截断失败)时清空候选，绝不放行
/// boundary:
///   - 无 spawned_at 时整体跳过过滤,把守卫让给分配器的身份与唯一性判据
///   - 无 rollout_path、取不到时间的候选一律剔除
///   - Codex 刻意不用 mtime:追加写与邻席活动都会抬升 mtime
/// ---
pub(super) fn apply_spawned_at_filter(
    provider: Provider,
    context: &CaptureSessionContext,
    out: &mut Vec<CapturedSessionCandidate>,
) {
    let Some(raw_spawned_at) = context.spawned_at.as_deref() else {
        // Legacy direct capture_session_id callers have no cohort boundary;
        // the canonical runtime capture path rejects such rows before scan.
        return;
    };
    let Some(spawned_at) = super::codex::parse_spawned_at(raw_spawned_at) else {
        out.clear();
        return;
    };
    let codex_spawned_at =
        if matches!(provider, Provider::Codex) && context.expected_session_id.is_some() {
            let Some(timestamp) = super::codex::truncate_to_uuid_precision(spawned_at) else {
                out.clear();
                return;
            };
            timestamp
        } else {
            spawned_at
        };
    out.retain(|candidate| {
        let Some(path) = candidate.captured.rollout_path.as_ref() else {
            return false;
        };
        if matches!(provider, Provider::Codex) {
            return super::codex::rollout_created_at(path.as_path())
                .is_some_and(|created_at| created_at >= codex_spawned_at);
        }
        candidate_mtime(path.as_path()).is_some_and(|mtime| mtime >= spawned_at)
    });
}

fn candidate_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// ---
/// purpose: 在时间窗只能唯一确定一条候选时才据此收窄，避免用弱判据在多条候选间乱挑
/// params:
///   provider: Codex 用 rollout 创建时间，其余用 mtime
///   context: 需要 spawned_at;有 expected id 且候选不超过 1 条时直接不动
///   out: 就地可能被替换的候选列表
/// contract:
///   provides:
///     - name: apply_spawn_time_window_if_unique
///       what: 只在窗内恰好剩 1 条时替换整表;0 条或 ≥2 条一律保持原样
/// boundary:
///   - 无 spawned_at 或解析失败则不过滤(与 apply_spawned_at_filter 的 fail-closed 相反,此处是不作为)
///   - 不排序、不去重;只做"唯一即取"的收窄
/// ---
pub(super) fn apply_spawn_time_window_if_unique(
    provider: Provider,
    context: &CaptureSessionContext,
    out: &mut Vec<CapturedSessionCandidate>,
) {
    if context.expected_session_id.is_some() && out.len() <= 1 {
        return;
    }
    let Some(spawned_at) = context
        .spawned_at
        .as_deref()
        .and_then(super::codex::parse_spawned_at)
    else {
        return;
    };
    let within: Vec<CapturedSessionCandidate> = out
        .iter()
        .filter(|candidate| {
            candidate
                .captured
                .rollout_path
                .as_ref()
                .and_then(|p| match provider {
                    Provider::Codex => super::codex::rollout_created_at(p.as_path()),
                    _ => candidate_mtime(p.as_path()),
                })
                .is_some_and(|created_at| created_at >= spawned_at)
        })
        .cloned()
        .collect();
    if within.len() == 1 {
        *out = within;
    }
}

/// ---
/// purpose: 有 pending id 时把命中的候选排到最前，让下游取首条即取到期望的那个
/// params:
///   context: 无 expected_session_id 则不动
///   out: 就地排序的候选切片
/// contract:
///   provides:
///     - name: sort_expected_first_if_needed
///       what: 稳定排序，只按"是否命中 expected"分两档，不改组内相对次序
/// boundary:
///   - 只排序不过滤——未命中的候选仍在表里
/// ---
pub(super) fn sort_expected_first_if_needed(
    context: &CaptureSessionContext,
    out: &mut [CapturedSessionCandidate],
) {
    if let Some(expected) = context.expected_session_id.as_ref() {
        out.sort_by_key(|candidate| {
            candidate
                .captured
                .session_id
                .as_ref()
                .is_none_or(|session| session.as_str() != expected.as_str())
        });
    }
}

fn cap_candidates_by_mtime(out: &mut Vec<SessionCandidate>, cap: usize) {
    if out.len() <= cap {
        return;
    }
    let mut ranked: Vec<(std::time::SystemTime, usize)> = out
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mtime = std::fs::metadata(&candidate.path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, index)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    let keep: std::collections::BTreeSet<usize> = ranked
        .into_iter()
        .take(cap)
        .map(|(_, index)| index)
        .collect();
    let mut index = 0;
    out.retain(|_| {
        let kept = keep.contains(&index);
        index += 1;
        kept
    });
}

/// ---
/// purpose: 读文件头若干字节并截到最后一个完整行，供各 provider 解析记录
/// params:
///   path: 待读文件
///   max_bytes: 读取上限;全模块统一用 CAPTURE_HEAD_BYTES(64KB)
/// returns: 到最后一个换行为止的文本;整段无换行时返回读到的全部
/// errors: 打不开或读失败时上抛 io::Error
/// contract:
///   provides:
///     - name: read_head_text
///       what: 只读、不加锁、不改 mtime
/// boundary:
///   - 刻意丢弃末尾不完整的一行——半条 JSON 会让解析器把整段判废
///   - 非 UTF-8 字节按 lossy 转换，不报错
///   - 只看头部;窗口之外的内容对所有基于本函数的判据都不可见
/// ---
pub(super) fn read_head_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    read_head_bytes(path, max_bytes).map(|bytes| complete_head_text(&bytes))
}

fn read_head_bytes(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn complete_head_text(bytes: &[u8]) -> String {
    let complete = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(last_newline) => &bytes[..=last_newline],
        None => &bytes[..],
    };
    String::from_utf8_lossy(complete).into_owned()
}

fn collect_optional_candidate_files(
    dir: &Path,
    agent_id: &str,
    allowed_team_root: Option<&Path>,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if dir.exists() {
        let _ = collect_candidate_files(dir, agent_id, 0, true, allowed_team_root, out);
    }
    Ok(())
}

fn collect_candidate_files(
    dir: &Path,
    agent_id: &str,
    depth: usize,
    requires_cwd_match: bool,
    allowed_team_root: Option<&Path>,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if depth > 4 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if depth == 0 => return Err(ProviderError::Io(format!("{}: {e}", dir.display()))),
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            collect_candidate_files(
                &path,
                agent_id,
                depth.saturating_add(1),
                requires_cwd_match,
                allowed_team_root,
                out,
            )?;
        } else if looks_like_session_file(&path, agent_id, allowed_team_root) {
            out.push(SessionCandidate {
                path,
                requires_cwd_match,
            });
        }
    }
    Ok(())
}

fn looks_like_session_file(path: &Path, agent_id: &str, allowed_team_root: Option<&Path>) -> bool {
    if path_is_under_team_runtime(path)
        && !allowed_team_root.is_some_and(|root| path.starts_with(root))
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl")
        || name.ends_with(".json")
        || name.contains("session")
        || name.contains("rollout")
        || (!agent_id.is_empty() && name.contains(agent_id))
}

/// ---
/// purpose: 把会话文本解析成记录数组，兼容整体 JSON 与 JSONL 两种存档形态
/// params:
///   text: 已读入的文本(通常是 read_head_text 的产物)
/// returns: JSON 数组则展开;JSON 单值则包成一条;都不是则按 JSONL 逐行解析
/// contract:
///   provides:
///     - name: parse_session_records
///       what: 纯解析，无 I/O
/// boundary:
///   - JSONL 分支跳过解析失败的行,不报错——因此"记录为空"不区分"文件空"与"全是坏行"
/// ---
pub(super) fn parse_session_records(text: &str) -> Vec<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(value) => vec![value],
        Err(_) => parse_jsonl_records(text),
    }
}

fn provider_home_records_match_spawn_cwd(records: &[serde_json::Value], spawn_cwd: &Path) -> bool {
    let cwd_values: Vec<String> = records.iter().filter_map(record_cwd).collect();
    !cwd_values.is_empty()
        && cwd_values
            .iter()
            .any(|cwd| paths_equivalent(Path::new(cwd), spawn_cwd))
}

fn candidate_text_has_team_agent_id(text: &str, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    [
        format!("\"TEAM_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_ID={id}"),
        format!("env.TEAM_AGENT_ID=\"{id}\""),
        format!("env.TEAM_AGENT_ID=\\\"{id}\\\""),
        format!("\"TEAM_AGENT_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_AGENT_ID={id}"),
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// ---
/// purpose: 从会话正文里抽出 team-agent 写进 prompt 的席位身份 marker，作为归属的强证据
/// params:
///   text: 待搜索的文本
/// returns: marker 后反引号之间的 id;未找到、为空或含非法字符时 None
/// contract:
///   provides:
///     - name: embedded_team_agent_worker_id_from_text
///       what: 纯文本查找，无 I/O
/// boundary:
///   - 只取第一处 marker;后续出现的一律忽略
///   - id 字符集限定为 ASCII 字母数字与 _ - .;越界即判无
///   - 找不到只说明"这段文本里没有",不等于该会话不属于任何席位
/// ---
pub(crate) fn embedded_team_agent_worker_id_from_text(text: &str) -> Option<String> {
    const PREFIX: &str = "You are Team Agent worker `";
    let start = text.find(PREFIX)? + PREFIX.len();
    let rest = &text[start..];
    let end = rest.find('`')?;
    let id = &rest[..end];
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return None;
    }
    Some(id.to_string())
}

/// ---
/// purpose: 按路径取该存档的 embedded 席位身份，供捕获链外的调用方直接查归属
/// params:
///   path: 存档文件路径
/// returns: 读头 64KB 后抽到的席位 id;读失败或没抽到给 None
/// contract:
///   provides:
///     - name: rollout_path_embedded_team_agent_worker_id
///       what: read_head_text + embedded_team_agent_worker_id_from_text 的组合，只读
/// boundary:
///   - 读失败与"文件里确实没有 marker"都返回 None，两者不可区分
///   - marker 若出现在 64KB 之后则看不见
/// ---
pub(crate) fn rollout_path_embedded_team_agent_worker_id(path: &Path) -> Option<String> {
    read_head_text(path, CAPTURE_HEAD_BYTES)
        .ok()
        .and_then(|text| embedded_team_agent_worker_id_from_text(&text))
}

fn candidate_path_matches_agent_id(path: &Path, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let dashed = id.replace('_', "-");
    name.contains(id) || name.contains(&dashed)
}

/// ---
/// purpose: 从一条会话记录里取工作目录，兼容 claude 顶层 cwd 与 codex session_meta.payload.cwd 两种形状
/// params:
///   record: 单条已解析记录
/// returns: 顶层 cwd 优先;否则取 session_meta.payload.cwd 或 payload.cwd;都没有则 None
/// contract:
///   provides:
///     - name: record_cwd
///       what: 纯取值，不做路径归一或存在性检查
/// boundary:
///   - 只认这三个位置;其它自定义字段一概不看
/// ---
pub(super) fn record_cwd(record: &serde_json::Value) -> Option<String> {
    record
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            record
                .get("session_meta")
                .and_then(|v| v.get("payload"))
                .or_else(|| record.get("payload"))
                .and_then(|v| v.get("cwd"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

/// ---
/// purpose: 判断记录里的 cwd 与席位 spawn_cwd 是不是同一个工作目录
/// params:
///   left: 记录里的 cwd
///   right: 席位 spawn_cwd
/// returns: 字面相等、canonicalize 后相等、或 left 的父目录等于 right 时为 true
/// contract:
///   provides:
///     - name: paths_equivalent
///       what: 会触发文件系统 canonicalize;失败则退回原路径继续比
/// boundary:
///   - 父目录等价是刻意放宽的一档:记录 cwd 是 spawn_cwd 的直接子目录也算同 cwd，嵌套 workspace 下会把子目录的存档收进来
///   - 不对称:只查 left 的父等于 right，反向不查
///   - 不解析符号链接之外的等价(如大小写不敏感文件系统的同名不同写)
/// ---
pub(super) fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right || left.parent().is_some_and(|parent| parent == right)
}

fn path_is_under_team_runtime(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(".team"))
}

#[cfg(test)]
mod tests;
