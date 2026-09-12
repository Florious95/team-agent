//! ---
//! purpose: 对 compatible_api 席位做发车前的 profile 连通性冒烟
//! contract:
//!   provides:
//!     - name: profile_smoke_checks_for_agents_with_profile_dir
//!       what: 对一组 agent 逐个冒烟，可指定优先查找的 profile 目录
//!     - name: profile_smoke_checks_for_agents
//!       what: 对一组 agent 逐个冒烟，返回逐个结果
//!     - name: smoke_check_agent_profile
//!       what: 单 agent 冒烟，非 compatible_api 直接判 not_required
//!     - name: format_profile_smoke_failures
//!       what: 把失败结果排成一行可读文本
//!   depends:
//!     - crate::lifecycle::profile_launch
//!     - crate::provider::wire
//! boundary:
//!   - 只读 profile 并发探测请求，不改 profile、不写 runtime state
//!   - 不做重试与退避，超时即判失败
//!   - 结果里不带密钥值
//! maturity: wired
//! ---
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::lifecycle::profile_launch;
use crate::model::enums::{AuthMode, Provider};
use crate::model::yaml::Value as YamlValue;
use crate::provider::wire::provider_wire;

pub(crate) const DEFAULT_PROFILE_SMOKE_TIMEOUT: Duration = Duration::from_secs(8);

/// ---
/// purpose: 用默认 profile 目录对一组 agent 逐个冒烟
/// params:
///   agents: spec 里的 agent 节点列表
///   timeout: 单次探测的超时
/// returns: 与入参同序的逐 agent 结果 JSON
/// ---
pub(crate) fn profile_smoke_checks_for_agents(
    workspace: &Path,
    agents: &[YamlValue],
    timeout: Duration,
) -> Vec<Value> {
    profile_smoke_checks_for_agents_with_profile_dir(workspace, agents, None, timeout)
}

/// ---
/// purpose: 用指定 profile 目录对一组 agent 逐个冒烟
/// params:
///   profile_dir: 优先查找的 profile 目录，None 表示只用默认查找顺序
/// returns: 与入参同序的逐 agent 结果 JSON
/// ---
pub(crate) fn profile_smoke_checks_for_agents_with_profile_dir(
    workspace: &Path,
    agents: &[YamlValue],
    profile_dir: Option<&Path>,
    timeout: Duration,
) -> Vec<Value> {
    agents
        .iter()
        .map(|agent| smoke_check_agent_profile(workspace, agent, profile_dir, timeout))
        .collect()
}

/// ---
/// purpose: 单个 agent 的 profile 冒烟
/// params:
///   agent: 单个 agent 节点，缺 provider 按 codex、缺 auth_mode 按 subscription
///   timeout: 单次 HTTP 探测的超时
/// returns: 结果 JSON。非 compatible_api 记 not_required，profile 里关掉冒烟记 skipped_by_profile，其余走真实请求
/// ---
pub(crate) fn smoke_check_agent_profile(
    workspace: &Path,
    agent: &YamlValue,
    profile_dir: Option<&Path>,
    timeout: Duration,
) -> Value {
    let agent_id = agent
        .get("id")
        .and_then(YamlValue::as_str)
        .unwrap_or("agent")
        .to_string();
    let provider = agent
        .get("provider")
        .and_then(YamlValue::as_str)
        .and_then(profile_launch::parse_provider)
        .unwrap_or(Provider::Codex);
    let auth_mode = agent
        .get("auth_mode")
        .and_then(YamlValue::as_str)
        .and_then(profile_launch::parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let profile = agent
        .get("profile")
        .and_then(YamlValue::as_str)
        .filter(|value| !value.is_empty());
    let model = agent
        .get("model")
        .and_then(YamlValue::as_str)
        .filter(|value| !value.is_empty());

    if auth_mode != AuthMode::CompatibleApi {
        return base_result(
            &agent_id,
            provider,
            profile,
            auth_mode,
            true,
            "not_required",
        );
    }
    let Some(profile_name) = profile else {
        return invalid_profile(&agent_id, provider, profile, auth_mode, "missing_profile");
    };
    let loaded = match profile_launch::load_profile(workspace, profile_name, profile_dir) {
        Ok(loaded) => loaded,
        Err(error) => {
            return invalid_profile(&agent_id, provider, profile, auth_mode, &error.to_string());
        }
    };
    if smoke_disabled(&loaded.values) {
        let mut result = base_result(
            &agent_id,
            provider,
            profile,
            auth_mode,
            true,
            "skipped_by_profile",
        );
        merge_proxy_info(&mut result, &loaded.values);
        return result;
    }

    let target = match SmokeTarget::from_profile(provider, model, &loaded) {
        Ok(target) => target,
        Err(reason) => {
            let mut result = invalid_profile(&agent_id, provider, profile, auth_mode, &reason);
            merge_proxy_info(&mut result, &loaded.values);
            return result;
        }
    };
    let mut result = run_http_smoke(&agent_id, provider, profile, auth_mode, &target, timeout);
    merge_proxy_info(&mut result, &loaded.values);
    result
}

/// ---
/// purpose: 把失败结果拼成一行人类可读的摘要
/// returns: 以分号分隔的 agent 与原因，缺字段时退化成 agent 与 failed
/// ---
pub(crate) fn format_profile_smoke_failures(failures: &[Value]) -> String {
    failures
        .iter()
        .filter_map(|failure| {
            let agent = failure
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            let reason = failure
                .get("reason")
                .or_else(|| failure.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("failed");
            Some(format!("{agent}: {reason}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

struct SmokeTarget {
    endpoint: String,
    token: String,
    model: String,
    kind: SmokeKind,
}

enum SmokeKind {
    Anthropic,
    OpenAi,
}

impl SmokeTarget {
    fn from_profile(
        provider: Provider,
        agent_model: Option<&str>,
        loaded: &profile_launch::ProfileValues,
    ) -> Result<Self, String> {
        let values = &loaded.values;
        let (endpoint, token, kind) = match provider {
            Provider::Claude | Provider::ClaudeCode => {
                let base = value_any(values, &["ANTHROPIC_BASE_URL", "BASE_URL"])
                    .ok_or("missing_base_url")?;
                let token = value_any(
                    values,
                    &["ANTHROPIC_AUTH_TOKEN", "AUTH_TOKEN", "ANTHROPIC_API_KEY", "API_KEY", "BEARER_TOKEN"],
                ).ok_or("missing_api_key")?;
                (anthropic_endpoint(base), token, SmokeKind::Anthropic)
            }
            Provider::Codex => {
                let base = value_any(values, &["OPENAI_BASE_URL", "BASE_URL"])
                    .ok_or("missing_base_url")?;
                let token = value_any(values, &["OPENAI_API_KEY", "API_KEY"])
                    .ok_or("missing_api_key")?;
                (openai_endpoint(base), token, SmokeKind::OpenAi)
            }
            Provider::Copilot | Provider::Grok | Provider::CursorAgent | Provider::Pi
            | Provider::GeminiCli | Provider::Fake => {
                return Err("unsupported_provider_smoke_skipped".to_string());
            }
        };
        let model = profile_launch::resolve_profile_model(agent_model, AuthMode::CompatibleApi, loaded)
            .map_err(|error| error.to_string())?
            .ok_or("missing_model")?;
        Ok(Self { endpoint, token: token.to_string(), model, kind })
    }

    fn body(&self) -> String {
        let body = match self.kind {
            SmokeKind::Anthropic => json!({
                "model": self.model,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            }),
            SmokeKind::OpenAi => json!({
                "model": self.model,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            }),
        };
        body.to_string()
    }
}

fn run_http_smoke(
    agent_id: &str,
    provider: Provider,
    profile: Option<&str>,
    auth_mode: AuthMode,
    target: &SmokeTarget,
    timeout: Duration,
) -> Value {
    match curl_post(target, timeout) {
        Ok(response) if (200..300).contains(&response.http_status) => {
            json!({
                "ok": true,
                "agent_id": agent_id,
                "provider": provider_wire(provider),
                "profile": profile,
                "auth_mode": auth_mode_wire(auth_mode),
                "status": "smoke_passed",
                "http_status": response.http_status,
                "endpoint": redact_endpoint(&target.endpoint),
                "secret_values_printed": false,
            })
        }
        Ok(response) => {
            json!({
                "ok": false,
                "agent_id": agent_id,
                "provider": provider_wire(provider),
                "profile": profile,
                "auth_mode": auth_mode_wire(auth_mode),
                "status": "smoke_failed",
                "reason": "http_error",
                "http_status": response.http_status,
                "endpoint": redact_endpoint(&target.endpoint),
                "error": redact_text(&response.body, &[target.token.as_str()]),
                "secret_values_printed": false,
            })
        }
        Err(message) => {
            json!({
                "ok": false,
                "agent_id": agent_id,
                "provider": provider_wire(provider),
                "profile": profile,
                "auth_mode": auth_mode_wire(auth_mode),
                "status": "smoke_failed",
                "reason": "request_failed",
                "endpoint": redact_endpoint(&target.endpoint),
                "error": redact_text(&message, &[target.token.as_str()]),
                "secret_values_printed": false,
            })
        }
    }
}

struct CurlResponse {
    http_status: u16,
    body: String,
}

fn curl_post(target: &SmokeTarget, timeout: Duration) -> Result<CurlResponse, String> {
    let body_path = temp_body_path();
    std::fs::write(&body_path, target.body()).map_err(|e| e.to_string())?;
    let result = curl_post_with_body_file(target, timeout, &body_path);
    let _ = std::fs::remove_file(&body_path);
    result
}

fn curl_post_with_body_file(
    target: &SmokeTarget,
    timeout: Duration,
    body_path: &Path,
) -> Result<CurlResponse, String> {
    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl_spawn_failed: {e}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err("curl_stdin_unavailable".to_string());
    };
    stdin
        .write_all(curl_config(target, timeout, body_path).as_bytes())
        .map_err(|e| format!("curl_config_write_failed: {e}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|e| format!("curl_wait_failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let message = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!("curl_failed: {}", message.trim()));
    }
    parse_curl_output(&stdout)
}

fn curl_config(target: &SmokeTarget, timeout: Duration, body_path: &Path) -> String {
    let mut lines = vec![
        "silent".to_string(),
        "show-error".to_string(),
        format!("max-time = \"{}\"", timeout.as_secs().max(1)),
        "output = \"-\"".to_string(),
        "write-out = \"\\n%{http_code}\"".to_string(),
        "request = \"POST\"".to_string(),
        format!("url = \"{}\"", curl_quote(&target.endpoint)),
        "header = \"content-type: application/json\"".to_string(),
        format!(
            "header = \"authorization: Bearer {}\"",
            curl_quote(&target.token)
        ),
        format!(
            "data-binary = \"@{}\"",
            curl_quote(&body_path.to_string_lossy())
        ),
    ];
    if matches!(target.kind, SmokeKind::Anthropic) {
        lines.push("header = \"anthropic-version: 2023-06-01\"".to_string());
    }
    format!("{}\n", lines.join("\n"))
}

fn parse_curl_output(stdout: &str) -> Result<CurlResponse, String> {
    let Some((body, status)) = stdout.rsplit_once('\n') else {
        return Err("curl_missing_http_status".to_string());
    };
    let http_status = status
        .trim()
        .parse::<u16>()
        .map_err(|e| format!("curl_bad_http_status: {e}"))?;
    Ok(CurlResponse {
        http_status,
        body: body.chars().take(4096).collect(),
    })
}

fn temp_body_path() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "team-agent-profile-smoke-{}-{nanos}.json",
        std::process::id()
    ))
}

fn curl_quote(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "")
        .replace('\r', "")
}

fn base_result(
    agent_id: &str,
    provider: Provider,
    profile: Option<&str>,
    auth_mode: AuthMode,
    ok: bool,
    status: &str,
) -> Value {
    json!({
        "ok": ok,
        "agent_id": agent_id,
        "provider": provider_wire(provider),
        "profile": profile,
        "auth_mode": auth_mode_wire(auth_mode),
        "status": status,
        "secret_values_printed": false,
    })
}

fn invalid_profile(
    agent_id: &str,
    provider: Provider,
    profile: Option<&str>,
    auth_mode: AuthMode,
    reason: &str,
) -> Value {
    json!({
        "ok": false,
        "agent_id": agent_id,
        "provider": provider_wire(provider),
        "profile": profile,
        "auth_mode": auth_mode_wire(auth_mode),
        "status": "profile_invalid",
        "reason": reason,
        "secret_values_printed": false,
    })
}

fn value_any<'a>(values: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| profile_launch::profile_value_or_alternate(values, key))
        .filter(|value| !value.is_empty())
}

fn smoke_disabled(values: &BTreeMap<String, String>) -> bool {
    values
        .get("PROFILE_SMOKE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            )
        })
        .unwrap_or(false)
}

fn anthropic_endpoint(base: &str) -> String {
    endpoint_with_suffix(base, "messages", "v1/messages")
}

fn openai_endpoint(base: &str) -> String {
    endpoint_with_suffix(base, "chat/completions", "v1/chat/completions")
}

fn endpoint_with_suffix(base: &str, terminal: &str, suffix: &str) -> String {
    let clean = base.trim().trim_end_matches('/');
    if clean.ends_with(terminal) {
        return clean.to_string();
    }
    if clean.ends_with("/v1") {
        return format!("{clean}/{}", terminal.trim_start_matches('/'));
    }
    format!("{clean}/{}", suffix.trim_start_matches('/'))
}

fn merge_proxy_info(result: &mut Value, values: &BTreeMap<String, String>) {
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    let mode = profile_launch::profile_proxy_mode(values);
    obj.insert("proxy_mode".to_string(), json!(mode));
    if let Some((key, url)) = proxy_value(values) {
        obj.insert("proxy_configured".to_string(), json!(true));
        obj.insert("proxy_source".to_string(), json!(key));
        obj.insert("proxy_scheme".to_string(), json!(proxy_scheme(url)));
        obj.insert("proxy_url".to_string(), json!(redact_endpoint(url)));
    } else {
        obj.insert("proxy_configured".to_string(), json!(false));
    }
}

fn proxy_value(values: &BTreeMap<String, String>) -> Option<(&'static str, &str)> {
    [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ]
    .into_iter()
    .find_map(|key| {
        values
            .get(key)
            .filter(|value| !value.is_empty())
            .map(|value| (key, value.as_str()))
    })
}

fn proxy_scheme(url: &str) -> Option<String> {
    url.split_once("://").map(|(scheme, _)| scheme.to_string())
}

fn redact_endpoint(raw: &str) -> String {
    let no_query = raw.split_once('?').map(|(head, _)| head).unwrap_or(raw);
    crate::redaction::redact_external_text(no_query)
}

fn redact_text(raw: &str, secrets: &[&str]) -> String {
    let mut out = raw.chars().take(512).collect::<String>();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    crate::redaction::redact_external_text(&out)
}

fn auth_mode_wire(auth_mode: AuthMode) -> &'static str {
    match auth_mode {
        AuthMode::Subscription => "subscription",
        AuthMode::OfficialApi => "official_api",
        AuthMode::CompatibleApi => "compatible_api",
    }
}

#[cfg(test)]
#[path = "../../tests/support/hermetic.rs"]
mod config_authority_hermetic;

#[cfg(test)]
mod config_authority_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    #[serial_test::serial(env)]
    fn config_authority_t03_request_body_matches_actual_launch_model() {
        let env = config_authority_hermetic::HermeticTestEnv::enter("smoke-model-authority");
        let workspace = env.workspace("ws");
        std::fs::create_dir_all(workspace.join("profiles")).unwrap();
        for provider in ["claude", "claude_code", "codex"] {
            for (model, source, fields, expected) in [
                (Some("A"), "role", "MODEL=A\n", "A"),
                (Some("A"), "team", "MODEL=A\n", "A"),
                (None, "profile", "MODEL=A\nANTHROPIC_MODEL=B\n", "A"),
                (None, "profile", "ANTHROPIC_MODEL=B\n", "B"),
            ] {
                let text = format!("BASE_URL=http://127.0.0.1:9/v1\nAPI_KEY=synthetic-only\n{fields}");
                std::fs::write(workspace.join("profiles/p.env"), text).unwrap();
                let agent = crate::model::yaml::loads(&format!(
                    "id: worker\nprovider: {provider}\nauth_mode: compatible_api\nprofile: p\nmodel: {}\nmodel_source: {source}\n",
                    model.unwrap_or("null")
                )).unwrap();
                let loaded = profile_launch::load_profile(&workspace, "p", None).unwrap();
                let provider = profile_launch::parse_provider(provider).unwrap();
                let target = SmokeTarget::from_profile(provider, model, &loaded).unwrap();
                let request: Value = serde_json::from_str(&target.body()).unwrap();
                assert_eq!(request["model"], expected);
                let launch = profile_launch::prepare_provider_profile_launch(&workspace, "worker", &agent, None).unwrap();
                let plan = crate::provider::get_adapter(provider).build_command_plan(crate::provider::ProviderCommandContext {
                    auth_mode: AuthMode::CompatibleApi, mcp_config: None, system_prompt: None,
                    model: launch.command_overrides.model.as_deref().or(model), tools: &[],
                    profile_launch: Some(&launch), agent_id_hint: Some("worker"), effort: None,
                }).unwrap();
                let actual = plan.argv.windows(2).find(|pair| pair[0] == "--model").unwrap();
                assert_eq!(actual[1], expected);
                assert_eq!(request["model"].as_str(), Some(actual[1].as_str()));
            }
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn config_authority_t03_conflict_and_unsupported_model_alias_fail_without_http() {
        let env = config_authority_hermetic::HermeticTestEnv::enter("smoke-model-invalid");
        let workspace = env.workspace("ws");
        std::fs::create_dir_all(workspace.join("profiles")).unwrap();
        for (model, fields, reason) in [
            ("A", "MODEL=B\n", "role/team model does not match profile MODEL"),
            ("null", "OPENAI_MODEL=B\n", "missing_model"),
        ] {
            std::fs::write(workspace.join("profiles/p.env"), format!("BASE_URL=http://127.0.0.1:9/v1\nAPI_KEY=synthetic-only\n{fields}")).unwrap();
            let agent = crate::model::yaml::loads(&format!("id: w\nprovider: codex\nauth_mode: compatible_api\nprofile: p\nmodel: {model}\n")).unwrap();
            let result = smoke_check_agent_profile(&workspace, &agent, None, Duration::from_millis(10));
            assert_eq!(result["status"], "profile_invalid");
            assert!(result["reason"].as_str().unwrap().contains(reason));
            assert!(!result.to_string().contains("synthetic-only"));
            let error = profile_launch::prepare_provider_profile_launch(&workspace, "w", &agent, None).unwrap_err().to_string();
            assert!(error.contains("MODEL"));
            assert!(!workspace.join(".team/runtime/provider-env/w.env").exists());
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn config_authority_t03_skip_semantics_are_preserved() {
        let env = config_authority_hermetic::HermeticTestEnv::enter("smoke-model-skips");
        let workspace = env.workspace("ws");
        std::fs::create_dir_all(workspace.join("profiles")).unwrap();
        let agent = crate::model::yaml::loads("id: w\nprovider: codex\nauth_mode: subscription\nprofile: absent\n").unwrap();
        assert_eq!(smoke_check_agent_profile(&workspace, &agent, None, Duration::from_millis(10))["status"], "not_required");
        std::fs::write(workspace.join("profiles/p.env"), "PROFILE_SMOKE=false\nMODEL=B\n").unwrap();
        let agent = crate::model::yaml::loads("id: w\nprovider: codex\nauth_mode: compatible_api\nprofile: p\nmodel: A\n").unwrap();
        assert_eq!(smoke_check_agent_profile(&workspace, &agent, None, Duration::from_millis(10))["status"], "skipped_by_profile");
        let loaded = profile_launch::load_profile(&workspace, "p", None).unwrap();
        assert_eq!(SmokeTarget::from_profile(Provider::Fake, Some("A"), &loaded).err().unwrap(), "unsupported_provider_smoke_skipped");
    }
}
