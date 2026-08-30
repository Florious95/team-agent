//! purpose: 钉死每席生成 env 的 ID/OWNER/AUTH 非空，空串不得静默变成 unknown
//! contract:
//!   provides:
//!     - name: J6-owner-auth-not-empty
//!       what: worker_spawn_env + grok overlay 合并后三键非空；空串不进 toml
//! boundary:
//!   - 不改 generic non_empty_string 的其它调用方
//!   - 不写本仓 .grok
//! maturity: wired

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::layout::worker_env::worker_spawn_env;
use team_agent::lifecycle::apply_grok_mcp_overlay;
use team_agent::provider::McpConfig;

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-identity-env-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn sample_mcp(agent_id: &str, workspace: &str, owner: &str, auth: &str) -> McpConfig {
    McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": "/bin/team-agent-test",
                "args": ["mcp-server", "--workspace", workspace],
                "env": {
                    "TEAM_AGENT_ID": agent_id,
                    "TEAM_AGENT_WORKSPACE": workspace,
                    "TEAM_AGENT_OWNER_TEAM_ID": owner,
                    "TEAM_AGENT_AUTH_MODE": auth,
                }
            }
        }),
    }
}

fn child_env(pane: &BTreeMap<String, String>, toml: &str) -> BTreeMap<String, String> {
    let mut out = pane.clone();
    for line in toml.lines() {
        let trimmed = line.trim();
        let Some((key, rest)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("TEAM_AGENT_") && key != "AUTH_MODE" {
            continue;
        }
        let rest = rest.trim();
        let value = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(rest)
            .to_string();
        out.insert(key.to_string(), value);
    }
    out
}

#[test]
fn pane_env_carries_id_owner_and_auth() {
    let pane = worker_spawn_env(
        Vec::<(String, String)>::new(),
        Path::new("/ws"),
        "alpha",
        Some("team-alpha"),
        Some("subscription"),
    );
    for key in [
        "TEAM_AGENT_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_AUTH_MODE",
    ] {
        let value = pane.get(key).map(String::as_str).unwrap_or("");
        assert!(
            !value.trim().is_empty(),
            "{key} must be non-empty on the generated pane env (not via grok toml); pane={pane:?}"
        );
    }
}

#[test]
fn generated_per_seat_env_id_owner_auth_are_non_empty() {
    let ws = tmp_dir("generated");
    let workspace = ws.to_string_lossy().into_owned();
    let pane = worker_spawn_env(
        Vec::<(String, String)>::new(),
        &ws,
        "alpha",
        Some("team-alpha"),
        Some("subscription"),
    );
    apply_grok_mcp_overlay(
        &ws,
        &sample_mcp("alpha", &workspace, "team-alpha", "subscription"),
    )
    .expect("overlay write");
    let toml = std::fs::read_to_string(ws.join(".grok/config.toml")).expect("toml");
    let child = child_env(&pane, &toml);

    for key in [
        "TEAM_AGENT_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_AUTH_MODE",
    ] {
        let value = child.get(key).map(String::as_str).unwrap_or("");
        assert!(
            !value.trim().is_empty(),
            "{key} must be non-empty in the generated per-seat env; toml={toml} pane={pane:?}"
        );
        assert!(
            !toml.contains(key),
            "shared grok toml must not carry per-seat {key}; toml={toml}"
        );
    }
    assert_eq!(
        child.get("TEAM_AGENT_ID").map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        child.get("TEAM_AGENT_OWNER_TEAM_ID").map(String::as_str),
        Some("team-alpha")
    );
    assert_eq!(
        child.get("TEAM_AGENT_AUTH_MODE").map(String::as_str),
        Some("subscription")
    );
}

#[test]
fn empty_overlay_owner_is_omitted_not_written() {
    let ws = tmp_dir("empty-owner");
    let workspace = ws.to_string_lossy().into_owned();
    apply_grok_mcp_overlay(&ws, &sample_mcp("alpha", &workspace, "", "subscription"))
        .expect("overlay write");
    let toml = std::fs::read_to_string(ws.join(".grok/config.toml")).expect("toml");
    assert!(
        !toml.contains("TEAM_AGENT_OWNER_TEAM_ID"),
        "empty OWNER must not land in toml (toml wins and non_empty_string turns \"\" into None); toml={toml}"
    );
    assert!(
        !toml.contains("TEAM_AGENT_AUTH_MODE"),
        "AUTH must not land in toml once pane env carries it; toml={toml}"
    );
}

#[test]
fn empty_overlay_owner_must_not_clobber_pane_or_become_unknown() {
    // mcp/*.json 实测 OWNER 是空串。toml 同名键赢：若把空串写进共享槽，
    // 子进程拿到 "" → non_empty_string → None → sender/owner 掉成 unknown。
    // 空串必须被省略，让 pane 里已经证过的 OWNER 继承下去。
    let ws = tmp_dir("empty-not-unknown");
    let workspace = ws.to_string_lossy().into_owned();
    let pane = worker_spawn_env(
        Vec::<(String, String)>::new(),
        &ws,
        "alpha",
        Some("team-alpha"),
        Some("subscription"),
    );
    apply_grok_mcp_overlay(&ws, &sample_mcp("alpha", &workspace, "", "subscription"))
        .expect("overlay write");
    let toml = std::fs::read_to_string(ws.join(".grok/config.toml")).expect("toml");
    let child = child_env(&pane, &toml);
    let owner = child.get("TEAM_AGENT_OWNER_TEAM_ID").map(String::as_str);
    assert_eq!(
        owner,
        Some("team-alpha"),
        "empty toml OWNER must not win over pane; got {owner:?} toml={toml}"
    );
    assert_ne!(
        owner,
        Some("unknown"),
        "OWNER must not degrade to unknown; toml={toml}"
    );
    assert!(
        !owner.unwrap_or("").trim().is_empty(),
        "merged OWNER must stay non-empty (empty toml must not win); toml={toml}"
    );
}

fn _keep_path(_: &Path) {}
