//! ---
//! purpose: claude 家族的会话归属过滤——按 expected id 直达 ~/.claude/projects/<编码 cwd>/<sid>.jsonl 读头验身份，并为通用扫描提供 leader-transcript 与 cwd 字段两道排除判据
//! contract:
//!   provides:
//!     - name: projects_dir_for_cwd
//!       what: 由 HOME + spawn_cwd 推出 claude 的 projects 目录(非字母数字字符一律替成 '-')
//!     - name: encode_projects_dir
//!       what: claude 的目录名编码规则本身
//!     - name: scan_expected_session
//!       what: 有 pending id 时的直达捕获:读头 64KB，验 sessionId 一致 + 有 user/assistant 记录 + 无 leader marker + 有 cwd 字段，四条全过才出候选
//!     - name: rollout_path_has_leader_marker
//!       what: 判某条 transcript 是不是 leader 的(customTitle/agentName == "claude leader")
//!     - name: records_have_leader_marker
//!       what: 上一条的记录级判据
//!     - name: has_cwd_field
//!       what: 记录里有没有 cwd 字段——claude transcript 的最低可信度门槛
//!     - name: apply_expected_session_filter
//!       what: 通用扫描结果的收窄:expected 命中则独取，未命中则只留身份/路径阳性的
//!   requires:
//!     - name: super::common
//!       what: 读头、解析记录、embedded 身份、时间窗过滤都复用 common
//!     - name: crate::provider::helpers::find_session_id
//!       what: 从记录里取 sessionId 的统一入口
//! boundary:
//!   - 只服务 Provider::Claude / ClaudeCode;rollout_path_has_leader_marker 对其它 provider 恒 false
//!   - 不写盘、不改 state、不发事件
//!   - 直达路径不做同 cwd「最新文件」回落——有 pending id 就只认那一个文件名
//!   - leader transcript 一律排除，防止 worker 席位绑上 leader 的会话
//! maturity: wired
//! ---
use std::path::{Path, PathBuf};

use crate::provider::helpers::find_session_id;
use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};
use crate::provider::Provider;

use super::{CaptureSessionContext, CapturedSessionCandidate};

/// ---
/// purpose: 由 HOME 与席位 cwd 推出 claude 存放该工作目录 transcript 的 projects 子目录
/// params:
///   home: HOME 根;调用方决定是真实 HOME 还是隔离根
///   spawn_cwd: 席位工作目录;先 canonicalize，失败则原样使用
/// returns: <home>/.claude/projects/<编码后的 cwd>;编码结果为空串时 None
/// contract:
///   provides:
///     - name: projects_dir_for_cwd
///       what: 只拼路径，不创建目录、不判存在性
/// boundary:
///   - 不枚举目录内容、不读任何文件
///   - canonicalize 失败不报错,退回原路径——编码结果因此可能与 claude 实际用的目录不同
/// ---
pub(crate) fn projects_dir_for_cwd(home: &Path, spawn_cwd: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(spawn_cwd).unwrap_or_else(|_| spawn_cwd.to_path_buf());
    let encoded = encode_projects_dir(&canonical.to_string_lossy());
    if encoded.is_empty() {
        return None;
    }
    Some(home.join(".claude").join("projects").join(encoded))
}

/// ---
/// purpose: 复刻 claude 的 projects 目录名编码:非 ASCII 字母数字的字符一律替成单个 '-'
/// params:
///   path: 待编码的路径文本
/// returns: 等长的编码串;输入为空则空串
/// contract:
///   provides:
///     - name: encode_projects_dir
///       what: 纯字符映射，逐字符一对一，不折叠连续分隔符
/// boundary:
///   - 有损且不可逆:不同路径可以编出同一个目录名
///   - 不做长度截断、不做大小写归一
/// ---
pub(super) fn encode_projects_dir(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    out
}

/// ---
/// purpose: 有 pending id 时直达那一个 transcript 文件，读头验明身份后给出唯一候选
/// params:
///   context: 需要 expected_session_id;projects 根优先取 provider_projects_root，否则退到 HOME/.claude/projects;spawn_cwd 决定编码后的子目录
/// returns: 四道校验全过则一条 FsWatch/High 候选(带 embedded 身份与是否与本席位一致);任一条不过则空向量
/// contract:
///   provides:
///     - name: scan_expected_session
///       what: 只读该文件头 64KB;不遍历目录、不比较 mtime
/// boundary:
///   - 无 expected_session_id / 无法确定 projects 根 / 编码为空 / 文件读不出来 → 空向量
///   - 四道校验:sessionId 与 expected 相等、存在 user 或 assistant 记录、不含 leader marker、至少一条记录有 cwd 字段
///   - embedded 身份与本席位不符时仍返回候选(positive_agent_id_match=false)，是否拒绝交给上游分配器判定
///   - agent_path_match 恒 false:直达路径下文件名就是 uuid，不含席位名
/// ---
pub(super) fn scan_expected_session(
    context: &CaptureSessionContext,
) -> Vec<CapturedSessionCandidate> {
    let Some(expected) = context.expected_session_id.as_ref() else {
        return Vec::new();
    };
    let Some(projects_root) = context.provider_projects_root.clone().or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".claude").join("projects"))
    }) else {
        return Vec::new();
    };
    let canonical =
        std::fs::canonicalize(&context.spawn_cwd).unwrap_or_else(|_| context.spawn_cwd.clone());
    let encoded = encode_projects_dir(&canonical.to_string_lossy());
    if encoded.is_empty() {
        return Vec::new();
    }
    let path = projects_root
        .join(encoded)
        .join(format!("{}.jsonl", expected.as_str()));
    let Ok(text) = super::common::read_head_text(&path, super::common::CAPTURE_HEAD_BYTES) else {
        return Vec::new();
    };
    let records = super::common::parse_session_records(&text);
    let session_matches = records
        .iter()
        .find_map(find_session_id)
        .is_some_and(|session_id| session_id == expected.as_str());
    let has_lifecycle_record = records.iter().any(|record| {
        matches!(
            record.get("type").and_then(serde_json::Value::as_str),
            Some("user" | "assistant")
        )
    });
    if !session_matches
        || !has_lifecycle_record
        || records_have_leader_marker(&records)
        || !records.iter().any(has_cwd_field)
    {
        return Vec::new();
    }
    let embedded_agent_id = super::common::embedded_team_agent_worker_id_from_text(&text);
    let positive_agent_id_match = embedded_agent_id.as_deref() == Some(context.agent_id.as_str());
    vec![CapturedSessionCandidate {
        captured: CapturedSession {
            session_id: Some(expected.clone()),
            rollout_path: Some(RolloutPath::new(path)),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd: context.spawn_cwd.clone(),
        },
        embedded_agent_id,
        positive_agent_id_match,
        agent_path_match: false,
    }]
}

/// ---
/// purpose: 判断一条 transcript 是不是 leader 的会话，供捕获与 event-log 修复两条通道共用排除
/// params:
///   provider: 非 Claude/ClaudeCode 一律直接判否
///   rollout_path: 待判定的 transcript 路径
/// returns: 读得到头且头部记录里出现 leader marker 才为 true
/// contract:
///   provides:
///     - name: rollout_path_has_leader_marker
///       what: 只读头 64KB
/// boundary:
///   - 文件打不开、解析不出记录一律返回 false —— 判据是 fail-open 的:读不到不等于不是 leader
///   - marker 只在头窗口内查;超出 64KB 之后才出现的 marker 看不见
/// ---
pub(crate) fn rollout_path_has_leader_marker(provider: Provider, rollout_path: &Path) -> bool {
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return false;
    }
    let Ok(text) = super::common::read_head_text(rollout_path, super::common::CAPTURE_HEAD_BYTES)
    else {
        return false;
    };
    let records = super::common::parse_session_records(&text);
    records_have_leader_marker(&records)
}

/// ---
/// purpose: 在已解析的记录里找 leader 身份 marker
/// params:
///   records: 已解析的 transcript 记录切片
/// returns: 任一记录的 customTitle 或 agentName 小写后等于 "claude leader" 即 true
/// contract:
///   provides:
///     - name: records_have_leader_marker
///       what: 纯内存判定，无 I/O
/// boundary:
///   - 判据是精确串相等(仅大小写不敏感)，不做包含匹配、不认其它别名
/// ---
pub(super) fn records_have_leader_marker(records: &[serde_json::Value]) -> bool {
    records.iter().any(|record| {
        let custom_title = record
            .get("customTitle")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        let agent_name = record
            .get("agentName")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        matches!(custom_title.as_deref(), Some("claude leader"))
            || matches!(agent_name.as_deref(), Some("claude leader"))
    })
}

/// ---
/// purpose: 判断一条 claude 记录是否带 cwd 字段——用作 transcript 是否够格当候选的最低门槛
/// params:
///   record: 单条已解析记录
/// returns: common::record_cwd 能取到值即 true
/// contract:
///   provides:
///     - name: has_cwd_field
///       what: 只判字段有无，不比较 cwd 是否等于席位 cwd
/// boundary:
///   - 不做路径等价判定;是否同 cwd 由调用方另行判断
/// ---
pub(super) fn has_cwd_field(record: &serde_json::Value) -> bool {
    super::common::record_cwd(record).is_some()
}

/// ---
/// purpose: 用 pending id 收窄通用扫描的结果，把「可能是它」压成「就是它」或「至少身份阳性」
/// params:
///   context: 有 expected_session_id 才做收窄;否则退到时间窗过滤
///   out: 待收窄的候选列表，按值传入
/// returns: expected 命中则只留那一条;未命中则只留 positive_agent_id_match 或 agent_path_match 为真的;无 expected 则原表经唯一时间窗过滤后返回
/// errors: 当前实现不产生 Err;返回 Result 是为与其它 provider 过滤器同形
/// contract:
///   provides:
///     - name: apply_expected_session_filter
///       what: 纯过滤，不读盘(时间窗分支会取候选文件 mtime)
/// boundary:
///   - 未命中 expected 时不返回空而是返回身份阳性子集——弱于「必须命中」，允许分配器再判
///   - 不排序;expected 优先排序由 common::sort_expected_first_if_needed 另做
/// ---
pub(super) fn apply_expected_session_filter(
    context: &CaptureSessionContext,
    mut out: Vec<CapturedSessionCandidate>,
) -> Result<Vec<CapturedSessionCandidate>, crate::provider::types::ProviderError> {
    if let Some(expected) = context.expected_session_id.as_ref() {
        if let Some(hit) = out
            .iter()
            .find(|candidate| session_matches(candidate, expected))
        {
            return Ok(vec![hit.clone()]);
        }
        let positive_only: Vec<CapturedSessionCandidate> = out
            .iter()
            .filter(|candidate| candidate.positive_agent_id_match || candidate.agent_path_match)
            .cloned()
            .collect();
        return Ok(positive_only);
    }
    super::common::apply_spawn_time_window_if_unique(Provider::Claude, context, &mut out);
    Ok(out)
}

fn session_matches(candidate: &CapturedSessionCandidate, expected: &SessionId) -> bool {
    candidate
        .captured
        .session_id
        .as_ref()
        .is_some_and(|session| session.as_str() == expected.as_str())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::provider::types::SessionId;
    use crate::provider::Provider;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_root(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-e6-attr-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_transcript(dir: &Path, uuid: &str, cwd: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{uuid}.jsonl"));
        let line = serde_json::json!({
            "type": "user",
            "sessionId": uuid,
            "cwd": cwd.to_string_lossy(),
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();
        path
    }

    fn expected_transcript_dir(projects_root: &Path, cwd: &Path) -> PathBuf {
        let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        projects_root.join(encode_projects_dir(&canonical.to_string_lossy()))
    }

    #[test]
    fn claude_leader_marker_in_custom_title_is_detected() {
        let records = vec![serde_json::json!({
            "type": "custom-title",
            "customTitle": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_leader_marker_in_agent_name_is_detected() {
        let records = vec![serde_json::json!({
            "type": "agent-name",
            "agentName": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_worker_records_have_no_leader_marker() {
        let records = vec![
            serde_json::json!({
                "type": "custom-title",
                "customTitle": "claude release-engineer",
                "sessionId": "abc12345",
            }),
            serde_json::json!({
                "type": "user",
                "content": "Team Agent message from leader: do X",
                "sessionId": "abc12345",
            }),
        ];
        assert!(!records_have_leader_marker(&records));
    }

    #[test]
    fn claude_leader_marker_is_case_insensitive() {
        let records = vec![serde_json::json!({
            "customTitle": "Claude Leader",
        })];
        assert!(records_have_leader_marker(&records));
    }

    #[test]
    fn claude_projects_dir_for_cwd_encodes_slashes_to_dashes() {
        let home = Path::new("/home/u");
        let cwd = tmp_root("encode");
        let got = projects_dir_for_cwd(home, &cwd).unwrap();
        let canon = std::fs::canonicalize(&cwd).unwrap();
        let expected_leaf = encode_projects_dir(&canon.to_string_lossy());
        assert_eq!(
            got,
            home.join(".claude").join("projects").join(expected_leaf)
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn encode_claude_projects_dir_parity_with_real_claude_naming() {
        assert_eq!(
            encode_projects_dir("/Users/alauda/code"),
            "-Users-alauda-code"
        );
        assert_eq!(
            encode_projects_dir("/Users/alauda/Documents/code/agent前沿探索/多agent协作"),
            "-Users-alauda-Documents-code-agent------agent--"
        );
        assert_eq!(
            encode_projects_dir("/Users/foo bar.baz/v1.2"),
            "-Users-foo-bar-baz-v1-2"
        );
        assert_eq!(
            encode_projects_dir("/proj/.team/runtime"),
            "-proj--team-runtime"
        );
    }

    #[test]
    fn scan_expected_session_id_hit_returns_only_that_candidate() {
        let base = tmp_root("c-hit");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        let expected = write_transcript(
            &expected_transcript_dir(&proj, &cwd),
            "22222222-2222-4222-8222-222222222222",
            &cwd,
        );
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new("22222222-2222-4222-8222-222222222222")),
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "22222222-2222-4222-8222-222222222222"
        );
        assert_eq!(
            out[0].captured.rollout_path.as_ref().unwrap().as_path(),
            expected
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_spawn_time_window_disambiguates_two_siblings() {
        let base = tmp_root("b-window");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        let old = write_transcript(&proj, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &cwd);
        let new = write_transcript(&proj, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &cwd);
        let long_ago =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        filetime_set(&old, long_ago);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        let _ = new;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_no_spawned_at_keeps_both_siblings_ambiguous() {
        let base = tmp_root("b-noamb");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "cccccccc-cccc-4ccc-8ccc-cccccccccccc", &cwd);
        write_transcript(&proj, "dddddddd-dddd-4ddd-8ddd-dddddddddddd", &cwd);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(out.len() >= 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_expected_session_id_miss_refuses_to_pick_leader_sibling() {
        let base = tmp_root("strict-no-leader-fallback");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        let leader = write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        let stale = write_transcript(&proj, "22222222-2222-4222-8222-222222222222", &cwd);
        let addressed =
            expected_transcript_dir(&proj, &cwd).join("99999999-9999-4999-8999-999999999999.jsonl");
        std::fs::create_dir_all(addressed.parent().unwrap()).unwrap();
        std::fs::write(
            &addressed,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "user",
                    "sessionId": "22222222-2222-4222-8222-222222222222",
                    "cwd": cwd.to_string_lossy(),
                })
            ),
        )
        .unwrap();
        let _ = (leader, stale);
        let ctx = CaptureSessionContext {
            agent_id: "claude-worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            expected_session_id: Some(SessionId::new("99999999-9999-4999-8999-999999999999")),
            provider_projects_root: Some(proj.clone()),
        };
        let out = super::super::scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(
            out.is_empty(),
            "expected-id miss must not fall back to latest"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
