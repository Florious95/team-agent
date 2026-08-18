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
//! boundary:
//!   - 不写盘
//!   - 不数 grok 席位个数
//!   - 剥离结果只含键名，不含键值
//! maturity: wired
//! ---

/// Shared identity keys belong on pane env. The only TEAM_AGENT_* key
/// allowed in the directory-scoped grok toml is WORKSPACE (same cwd).
pub(crate) fn is_per_seat_env_key(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("TEAM_AGENT_") && key != "TEAM_AGENT_WORKSPACE"
}

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
}
