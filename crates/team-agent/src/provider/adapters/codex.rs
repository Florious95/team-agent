//!
//! Codex provider-local command builder.
//!
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2). Pure
//! extraction — byte-identical to the original inline forms. Shared helper
//! `json_inline` stays in `adapter.rs` because it is also used by other
//! sites; this file reaches it via `super::*`.

use crate::model::enums::AuthMode;
use crate::provider::adapter::json_inline;
use crate::provider::command_helpers::{append_opt_pair, append_pair};
use crate::provider::{McpConfig, ProviderCommandOverrides};

pub(crate) fn codex_base_command(
    subcommand: Option<&str>,
    _auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    dangerously_skip_permissions: bool,
    overrides: Option<&ProviderCommandOverrides>,
    // 0.4.x provider effort MVP step 6: when Some, inject
    // `-c model_reasoning_effort=<level>` AFTER existing profile
    // codex_config overrides — explicit launch effort wins over profile.
    // Preserve max/ultra verbatim: ultra enables Codex client delegation
    // on top of max reasoning, rather than a separate model API effort.
    effort: Option<crate::model::enums::ProviderEffort>,
) -> Vec<String> {
    let mut argv = vec!["codex".to_string()];
    if let Some(subcommand) = subcommand {
        argv.push(subcommand.to_string());
    }
    argv.extend([
        "--no-alt-screen".to_string(),
        "--disable".to_string(),
        "shell_snapshot".to_string(),
        "--disable".to_string(),
        "apps".to_string(),
    ]);
    append_opt_pair(&mut argv, "--profile", overrides.and_then(|o| o.codex_profile.as_deref()));
    if dangerously_skip_permissions {
        // 0.5.66 bypass 单源:flag 由 provider_bypass_flag 表供给。
        let flag = crate::provider::bypass_flags::provider_bypass_flag(crate::model::enums::Provider::Codex)
            .expect("codex provider must define a bypass flag");
        argv.push(flag.to_string());
    }
    append_opt_pair(&mut argv, "--model", model);
    if let Some(overrides) = overrides {
        for config in &overrides.codex_config {
            append_pair(&mut argv, "-c", config.clone());
        }
    }
    if let Some(effort) = effort {
        argv.push("-c".to_string());
        argv.push(format!("model_reasoning_effort={}", effort.as_str()));
    }
    if let Some(prompt) = system_prompt {
        // codex.py:120 — escape order matters: backslash first, then quote, then newline.
        let escaped = prompt
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        append_pair(&mut argv, "-c", format!("developer_instructions=\"{escaped}\""));
    }
    if let Some(config) = mcp_config {
        append_codex_mcp_overrides(&mut argv, &config.raw);
    }
    // Update check only. Directory trust is a config file entry, never `-c projects.*`.
    append_pair(&mut argv, "-c", "check_for_update_on_startup=false".to_string());
    argv
}

/// Git root of `cwd`. Codex records directory trust on that absolute path.
pub(crate) fn git_work_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = std::path::PathBuf::from(text.trim());
    root.is_absolute().then_some(root)
}

/// Trust this cwd's git root inside the worker Codex home. Does not send keys.
pub(crate) fn ensure_worker_directory_trust(
    codex_home: &std::path::Path,
    git_root: &std::path::Path,
) -> std::io::Result<()> {
    if !git_root.is_absolute() {
        return Ok(());
    }
    let root = git_root.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "git root is not utf-8")
    })?;
    if root.contains('"') || root.contains('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "git root cannot be quoted into the trust table",
        ));
    }
    std::fs::create_dir_all(codex_home)?;
    let path = codex_home.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let header = format!("[projects.\"{root}\"]");
    if existing.contains(&header) {
        return Ok(());
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.contains("check_for_update_on_startup") {
        next.push_str("check_for_update_on_startup = false\n");
    }
    next.push('\n');
    next.push_str(&header);
    next.push_str("\ntrust_level = \"trusted\"\n");
    let tmp = codex_home.join("config.toml.tmp");
    std::fs::write(&tmp, next)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub(crate) fn codex_home_from_env(env: &std::collections::BTreeMap<String, String>) -> Option<std::path::PathBuf> {
    if let Some(home) = env.get("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Some(std::path::PathBuf::from(home));
    }
    env.get("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| std::path::PathBuf::from(home).join(".codex"))
}

#[cfg(test)]
mod trust_file_tests {
    use super::ensure_worker_directory_trust;

    #[test]
    fn trust_file_records_git_root_once() {
        let dir = std::env::temp_dir().join(format!("codex-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = std::path::Path::new("/Volumes/nvme/tmp/example-root");
        ensure_worker_directory_trust(&dir, root).unwrap();
        ensure_worker_directory_trust(&dir, root).unwrap();
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert_eq!(
            text,
            "check_for_update_on_startup = false\n\n[projects.\"/Volumes/nvme/tmp/example-root\"]\ntrust_level = \"trusted\"\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub(crate) fn codex_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| std::path::PathBuf::from(home).join(".codex"))
        })
}

/// Render an `McpConfig::raw` ({ name: { type, command, args, env: {...} } }) into Codex
/// `-c mcp_servers.<name>.<field>=...` overrides. JSON values are stringified with serde
/// so arrays/objects survive (Codex parses the right-hand side as JSON; this is what the
/// Python golden + the live attached Codex panes do).
pub(crate) fn append_codex_mcp_overrides(argv: &mut Vec<String>, raw: &serde_json::Value) {
    let Some(servers) = raw.as_object() else {
        return;
    };
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            continue;
        };
        for (key, value) in obj {
            if key == "env" {
                if let Some(env) = value.as_object() {
                    for (env_key, env_value) in env {
                        append_pair(argv, "-c", format!(
                            "mcp_servers.{name}.env.{env_key}={}",
                            json_inline(env_value)
                        ));
                    }
                }
                continue;
            }
            append_pair(argv, "-c", format!("mcp_servers.{name}.{key}={}", json_inline(value)));
        }
        // Every MCP server gets a 600s tool timeout so long-running
        // team_orchestrator calls (report_result etc.) survive the codex default.
        append_pair(argv, "-c", format!("mcp_servers.{name}.tool_timeout_sec=600.0"));
    }
}
