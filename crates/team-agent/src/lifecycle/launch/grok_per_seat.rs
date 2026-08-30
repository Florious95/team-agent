//! ---
//! purpose: 判定 .grok/config.toml env 里哪些键是每席私有
//! contract:
//!   provides:
//!     - name: is_per_seat_env_key
//!       what: TEAM_AGENT_* 除 WORKSPACE 外都是每席键
//!     - name: per_seat_keys_in_toml
//!       what: 扫描 toml env 表里的每席键
//!     - name: strip_per_seat_keys_from_toml
//!       what: 从任意 .env 表删掉每席键，留下非框架键
//!     - name: non_per_seat_env_in_tables
//!       what: 从指定表的 .env 取出非每席键（值原样），供 overlay 保留
//! boundary:
//!   - 不写盘
//!   - 不数 grok 席位个数
//!   - 剥离结果只含键名，不含键值
//! maturity: wired
//! ---

/// ---
/// purpose: 判断某环境键是否属于每席私有
/// returns: 以 TEAM_AGENT_ 起头且不是 TEAM_AGENT_WORKSPACE 时为 true
/// ---
/// Shared identity keys belong on pane env. The only TEAM_AGENT_* key
/// allowed in the directory-scoped grok toml is WORKSPACE (same cwd).
pub(crate) fn is_per_seat_env_key(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("TEAM_AGENT_") && key != "TEAM_AGENT_WORKSPACE"
}

/// ---
/// purpose: 扫出 toml 里所有 env 表中的每席键
/// returns: 首次出现顺序的键值对，值已去掉引号
/// ---
pub(crate) fn per_seat_keys_in_toml(text: &str) -> Vec<(String, String)> {
    let mut in_env_table = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            in_env_table = name.ends_with(".env");
            continue;
        }
        if !in_env_table {
            continue;
        }
        let Some((key, rest)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_per_seat_env_key(key) {
            continue;
        }
        let rest = rest.trim();
        let value = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(rest);
        if out.iter().any(|(existing, _)| existing == key) {
            continue;
        }
        out.push((key.to_string(), value.to_string()));
    }
    out
}

/// ---
/// purpose: 从所有 env 表里删掉每席键
/// returns: 删后的全文，以及被删键名的首次出现顺序列表，不含值
/// ---
/// Drop per-seat `TEAM_AGENT_*` assignments from any `*.env` table.
/// Other keys (`TEAM_AGENT_WORKSPACE`, `GROK_FOLDER_TRUST`, user keys) stay.
/// Returned names are first-seen order, values never included.
pub(crate) fn strip_per_seat_keys_from_toml(text: &str) -> (String, Vec<String>) {
    let mut in_env_table = false;
    let mut out = String::new();
    let mut removed = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            in_env_table = name.ends_with(".env");
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_env_table {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if is_per_seat_env_key(key) {
                    if !removed.iter().any(|existing| existing == key) {
                        removed.push(key.to_string());
                    }
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, removed)
}

fn toml_env_value(rest: &str) -> String {
    let rest = rest.trim();
    rest.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(rest)
        .to_string()
}

/// ---
/// purpose: 取出指定表的 env 段里非每席的键值
/// params:
///   tables: 目标表名前缀，只看这些表下的 env 段
/// returns: 键到值的有序映射，供覆盖写入时保留用户键
/// ---
/// Keys in `tables*.env` that are **not** per-seat. Unknown keys must be
/// kept — detection failure does not authorize deletion.
pub(crate) fn non_per_seat_env_in_tables(
    text: &str,
    tables: &[&str],
) -> std::collections::BTreeMap<String, String> {
    let mut in_target_env = false;
    let mut out = std::collections::BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            in_target_env = name.ends_with(".env")
                && tables
                    .iter()
                    .any(|table| name == *table || name.starts_with(&format!("{table}.")));
            continue;
        }
        if !in_target_env {
            continue;
        }
        let Some((key, rest)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || is_per_seat_env_key(key) {
            continue;
        }
        out.entry(key.to_string())
            .or_insert_with(|| toml_env_value(rest));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_is_shared_other_team_agent_keys_are_per_seat() {
        assert!(!is_per_seat_env_key("TEAM_AGENT_WORKSPACE"));
        assert!(is_per_seat_env_key("TEAM_AGENT_ID"));
        assert!(is_per_seat_env_key("TEAM_AGENT_OWNER_TEAM_ID"));
        assert!(is_per_seat_env_key("TEAM_AGENT_AUTH_MODE"));
        assert!(is_per_seat_env_key("TEAM_AGENT_FUTURE_SEAT_KEY"));
        assert!(!is_per_seat_env_key("GROK_FOLDER_TRUST"));
    }

    #[test]
    fn scan_finds_per_seat_keys_in_env_table_only() {
        let text = r#"
[mcp_servers.other.env]
TEAM_AGENT_ID = "ignore-other-server-still-counts"

[mcp_servers.team_orchestrator]
command = "/bin/x"

[mcp_servers.team_orchestrator.env]
TEAM_AGENT_WORKSPACE = "/ws"
TEAM_AGENT_AUTH_MODE = "impostor"
"#;
        let keys = per_seat_keys_in_toml(text);
        assert!(
            keys.iter()
                .any(|(k, v)| k == "TEAM_AGENT_AUTH_MODE" && v == "impostor"),
            "must see AUTH; keys={keys:?}"
        );
        assert!(
            keys.iter().any(|(k, _)| k == "TEAM_AGENT_ID"),
            "any env table per-seat key counts; keys={keys:?}"
        );
        assert!(
            !keys.iter().any(|(k, _)| k == "TEAM_AGENT_WORKSPACE"),
            "WORKSPACE is shared; keys={keys:?}"
        );
    }

    #[test]
    fn strip_drops_only_per_seat_keys_and_returns_names() {
        let text = r#"
[mcp_servers.keep.env]
GROK_FOLDER_TRUST = "1"
TEAM_AGENT_ID = "impostor"
TEAM_AGENT_WORKSPACE = "/ws"

[mcp_servers.keep]
command = "/bin/keep"

[other]
TEAM_AGENT_AUTH_MODE = "not-in-env-table"
"#;
        let (cleaned, removed) = strip_per_seat_keys_from_toml(text);
        assert_eq!(removed, vec!["TEAM_AGENT_ID".to_string()]);
        assert!(
            !cleaned.contains("TEAM_AGENT_ID"),
            "per-seat key must leave the env table; cleaned={cleaned}"
        );
        assert!(
            cleaned.contains("GROK_FOLDER_TRUST = \"1\"")
                && cleaned.contains("TEAM_AGENT_WORKSPACE = \"/ws\"")
                && cleaned.contains("TEAM_AGENT_AUTH_MODE = \"not-in-env-table\"")
                && cleaned.contains("command = \"/bin/keep\""),
            "non-per-seat keys and non-env tables stay; cleaned={cleaned}"
        );
        assert!(
            !cleaned.contains("impostor"),
            "stripped values must not be rewritten; cleaned={cleaned}"
        );
    }

    #[test]
    fn non_per_seat_scan_keeps_unknown_keys_from_named_tables_only() {
        let text = r#"
[mcp_servers.keep-me.env]
GROK_FOLDER_TRUST = "other-table"

[mcp_servers.team_orchestrator.env]
TEAM_AGENT_ID = "stale"
GROK_FOLDER_TRUST = "1"
TEAM_AGENT_WORKSPACE = "/ws"
USER_EXTRA = "keep"
"#;
        let kept = non_per_seat_env_in_tables(text, &["mcp_servers.team_orchestrator"]);
        assert_eq!(kept.get("GROK_FOLDER_TRUST").map(String::as_str), Some("1"));
        assert_eq!(kept.get("USER_EXTRA").map(String::as_str), Some("keep"));
        assert_eq!(
            kept.get("TEAM_AGENT_WORKSPACE").map(String::as_str),
            Some("/ws")
        );
        assert!(
            !kept.contains_key("TEAM_AGENT_ID"),
            "per-seat keys are not preserved; kept={kept:?}"
        );
        assert_ne!(
            kept.get("GROK_FOLDER_TRUST").map(String::as_str),
            Some("other-table"),
            "only the named table is read; kept={kept:?}"
        );
    }
}
