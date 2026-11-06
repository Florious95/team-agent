//!
//! cli · leader — `codex`/`claude`/`copilot`/`grok` passthrough(`cmd_leader_passthrough` + `_provider_args` /
//! `_leader_launcher_args`)+ leader fallback inbox 摘要(`consume_leader_inbox_summary` 及
//! `_leader_inbox_entries` / `_leader_inbox_summary` / `_leader_inbox_entry_title`)。

use super::*;

// =============================================================================
// `codex`/`claude` passthrough 解析(helpers.py `_provider_args`/`_leader_launcher_args`)
// =============================================================================


///
/// `_leader_launcher_args`(`helpers.py:196-226`):解析 attach 旗标;`--attach-session` 缺值 → Err。
pub(super) fn leader_launcher_args(values: &[String]) -> Result<LeaderLauncherArgs, CliError> {
    let mut out = LeaderLauncherArgs::default();
    let mut idx = 0;
    while idx < values.len() {
        let token = &values[idx];
        if token == "--" {
            for value in values.iter().skip(idx + 1) {
                if is_leader_launcher_flag(value) {
                    return Err(CliError::Runtime(format!(
                        "Team Agent launcher flag {value} must appear before --"
                    )));
                }
            }
            out.provider_args
                .extend(values.iter().skip(idx + 1).cloned());
            break;
        } else if token == "--attach" || token == "--attach-existing" {
            out.attach_existing = true;
        } else if token == "--confirm" {
            out.confirm_attach = true;
        } else if token == "--external-leader" {
            out.external_leader = true;
        } else if token == "--allow-nested-attach" {
            out.allow_nested_attach = true;
        } else if token == "--attach-session" {
            let Some(value) = values.get(idx + 1) else {
                return Err(CliError::Runtime(
                    "--attach-session requires a tmux session name".to_string(),
                ));
            };
            out.attach_session = Some(value.clone());
            idx += 1;
        } else if let Some(value) = token.strip_prefix("--attach-session=") {
            out.attach_session = Some(value.to_string());
        } else {
            out.provider_args.push(token.clone());
        }
        idx += 1;
    }
    Ok(out)
}

fn is_leader_launcher_flag(value: &str) -> bool {
    matches!(
        value,
        "--external-leader"
            | "--allow-nested-attach"
            | "--attach"
            | "--attach-existing"
            | "--confirm"
            | "--attach-session"
    ) || value.starts_with("--attach-session=")
}

fn leader_launcher_json(values: &[String]) -> bool {
    values
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--json")
}

fn without_leader_json(values: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(values.len());
    let mut provider_remainder = false;
    for value in values {
        if provider_remainder || value != "--json" {
            out.push(value.clone());
        }
        if value == "--" {
            provider_remainder = true;
        }
    }
    out
}

///
/// `codex`/`claude`/`copilot`/`grok` passthrough(`parser.py:86`/`_run_leader_passthrough`):leader 早返回,
/// **不**进 subparser。`-h`/`--help` 打 usage 直接返回 [`CmdResult::none`]。否则解析 attach
/// 旗标 + `lifecycle_port::start_leader`。`command` 必须是闭集动词,禁止 command-text 回落到 ClaudeCode。
pub fn cmd_leader_passthrough(
    command: &str,
    provider_args: &[String],
    cwd: &Path,
) -> Result<CmdResult, CliError> {
    if provider_args == ["-h"] || provider_args == ["--help"] {
        return Ok(CmdResult::human(&super::emit::command_help(Some(command))));
    }
    let as_json = leader_launcher_json(provider_args);
    let launcher_args = without_leader_json(provider_args);
    let attach = leader_launcher_args(&launcher_args)?;
    let provider = leader_passthrough_provider(command)
        .ok_or_else(|| CliError::Usage(format!("unknown leader passthrough command: {command}")))?;
    if attach.attach_existing {
        if !attach.provider_args.is_empty() || attach.attach_session.is_some() {
            return Err(CliError::Usage("--attach-existing is an alias for claim-leader; provider/session arguments are not accepted".to_string()));
        }
        return super::cmd_claim_leader(&super::ClaimLeaderArgs {
            workspace: cwd.to_path_buf(), team: None, confirm: attach.confirm_attach,
            json: as_json, detail: false,
        });
    }
    let value = lifecycle_port::start_leader(provider, &attach.provider_args, cwd, &attach)?;
    Ok(CmdResult::from_json(value, as_json))
}

/// Public capability gate, not a legacy passthrough or a native launch attempt.
pub(crate) fn cmd_kiro_leader(args: &[String]) -> CmdResult {
    CmdResult::from_json(
        json!({
            "schema_version": "leader_launch.v1",
            "ok": false,
            "provider": "kiro",
            "capability": "leader_launch",
            "reason": "kiro_leader_not_admitted",
            "error": "Kiro leader launching is not admitted by this candidate; no native process was started.",
            "action": "Use provider: kiro in a worker role, then run `team-agent start --from .`. Worker support does not grant leader capability.",
        }),
        leader_launcher_json(args),
    )
}

/// Closed-set verb → Provider. `None` = not a passthrough verb (must not become ClaudeCode).
pub(crate) fn leader_passthrough_provider(command: &str) -> Option<crate::model::enums::Provider> {
    use crate::model::enums::Provider;
    match command {
        "codex" => Some(Provider::Codex),
        "claude" => Some(Provider::ClaudeCode),
        "copilot" => Some(Provider::Copilot),
        "grok" => Some(Provider::Grok),
        "pi" => Some(Provider::Pi),
        _ => None,
    }
}

// =============================================================================
// leader fallback inbox 摘要(helpers.py `consume_leader_inbox_summary`)
// =============================================================================
