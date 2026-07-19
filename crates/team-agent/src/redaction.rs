use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;

const REDACTED: &str = "[REDACTED]";
const SENSITIVE_FAMILIES: [&str; 7] = [
    "PROXY",
    "TOKEN",
    "KEY",
    "PASSWORD",
    "AUTH",
    "SECRET",
    "CREDENTIAL",
];
const SENSITIVE_KEYS: [&str; 2] = ["authorization_header", "credential_blob"];
const STRUCTURAL_KEYS: [&str; 13] = [
    "active_team_key",
    "cohort_key",
    "dedupe_key",
    "error_key",
    "error_observation_key",
    "last_check_key",
    "last_error_observation_key",
    "last_notified_key",
    "last_suppressed_key",
    "parent_team_key",
    "runtime_team_key",
    "team_key",
    "team_state_key",
];

static SHELL_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?P<prefix>^|[\s\[,;('"])(?P<key>[a-z_][a-z0-9_.]*)=(?P<value>'[^']*'|"[^"]*"|[^\s,\]\[};)]+)"#,
    )
    .expect("shell assignment redaction regex")
});

static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)[^\s/?#]+@")
        .expect("URL userinfo redaction regex")
});

pub(crate) fn redact_external_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_env_key(key) {
                        Value::String(REDACTED.to_string())
                    } else if is_argument_array_key(key) {
                        redact_argument_array(value)
                    } else {
                        redact_external_value(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_external_value).collect()),
        Value::String(text) => Value::String(redact_external_text(text)),
        other => other.clone(),
    }
}

fn is_argument_array_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("argv") || key.eq_ignore_ascii_case("command")
}

fn redact_argument_array(value: &Value) -> Value {
    let Value::Array(values) = value else {
        return redact_external_value(value);
    };
    let mut redact_next = false;
    Value::Array(
        values
            .iter()
            .map(|value| {
                if redact_next {
                    redact_next = false;
                    return Value::String(REDACTED.to_string());
                }
                if let Some(flag) = value.as_str().filter(|flag| flag.starts_with('-')) {
                    let normalized = flag.trim_start_matches('-').replace('-', "_");
                    redact_next = is_sensitive_env_key(&normalized);
                }
                redact_external_value(value)
            })
            .collect(),
    )
}

pub(crate) fn redact_external_text(text: &str) -> String {
    let assignments = SHELL_ASSIGNMENT.replace_all(text, |captures: &Captures<'_>| {
        let key = captures.name("key").map_or("", |value| value.as_str());
        if !is_sensitive_env_key(key) {
            return captures[0].to_string();
        }
        let prefix = captures.name("prefix").map_or("", |value| value.as_str());
        let value = captures.name("value").map_or("", |value| value.as_str());
        let quote = match (value.as_bytes().first(), value.as_bytes().last()) {
            (Some(b'\''), Some(b'\'')) => "'",
            (Some(b'"'), Some(b'"')) => "\"",
            _ => "",
        };
        let unquoted = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
            .unwrap_or(value);
        let has_url_userinfo = URL_USERINFO.is_match(unquoted);
        let redacted_url = URL_USERINFO
            .replace_all(unquoted, "${scheme}[REDACTED]@")
            .into_owned();
        let safe_value = if has_url_userinfo {
            &redacted_url
        } else {
            REDACTED
        };
        format!("{prefix}{key}={quote}{safe_value}{quote}")
    });
    URL_USERINFO
        .replace_all(&assignments, "${scheme}[REDACTED]@")
        .into_owned()
}

fn is_sensitive_env_key(key: &str) -> bool {
    let key = key.rsplit('.').next().unwrap_or(key);
    if STRUCTURAL_KEYS
        .iter()
        .any(|structural| key.eq_ignore_ascii_case(structural))
    {
        return false;
    }
    if SENSITIVE_KEYS
        .iter()
        .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
    {
        return true;
    }
    let key = key.to_ascii_uppercase();
    SENSITIVE_FAMILIES
        .iter()
        .any(|family| key == *family || key.ends_with(&format!("_{family}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursive_values_mask_env_families_without_erasing_wire_truth() {
        let input = json!({
            "HTTPS_PROXY": "proxy-secret",
            "Copilot_Github_Token": "token-secret",
            "nested": [{"db_password": "password-secret"}],
            "team_key": "current",
            "active_team_key": "current",
            "dedupe_key": "restart:worker",
            "auth_mode": "subscription",
            "model_source": "role",
            "model_stale": true,
        });

        let redacted = redact_external_value(&input);

        assert_eq!(redacted["HTTPS_PROXY"], REDACTED);
        assert_eq!(redacted["Copilot_Github_Token"], REDACTED);
        assert_eq!(redacted["nested"][0]["db_password"], REDACTED);
        assert_eq!(redacted["team_key"], "current");
        assert_eq!(redacted["active_team_key"], "current");
        assert_eq!(redacted["dedupe_key"], "restart:worker");
        assert_eq!(redacted["auth_mode"], "subscription");
        assert_eq!(redacted["model_source"], "role");
        assert_eq!(redacted["model_stale"], true);
        assert_eq!(redact_external_value(&redacted), redacted);
    }

    #[test]
    fn text_masks_quoted_assignments_and_url_userinfo_idempotently() {
        let input = "HTTPS_PROXY='https://user:pass@proxy.invalid:8443/path' http_proxy=other endpoint=https://user:pass@proxy.invalid:8443/path";
        let expected = "HTTPS_PROXY='https://[REDACTED]@proxy.invalid:8443/path' http_proxy=[REDACTED] endpoint=https://[REDACTED]@proxy.invalid:8443/path";

        let redacted = redact_external_text(input);

        assert_eq!(redacted, expected);
        assert_eq!(redact_external_text(&redacted), redacted);
    }

    #[test]
    fn text_leaves_non_env_assignments_and_plain_urls_unchanged() {
        let input = "team_key=current author=operator endpoint=https://proxy.invalid/health";
        assert_eq!(redact_external_text(input), input);
    }

    #[test]
    fn mixed_external_value_is_idempotent() {
        let marker = "synthetic-redaction-unit-marker";
        let credential_url = format!("https://demo-user:{marker}@proxy.invalid:8443/path");
        let diagnostic = format!(
            "subprocess exited 37: argv=[tmux, HTTPS_PROXY='{credential_url}']; endpoint={credential_url}"
        );
        let input = json!({
            "HTTPS_PROXY": marker,
            "ordinary_diagnostic": format!(
                "HTTPS_PROXY='{credential_url}' http_proxy=\"{credential_url}\" OPENAI_API_KEY={marker} endpoint={credential_url}"
            ),
            "nested": [[diagnostic, credential_url]],
            "team_key": "current",
        });

        let once = redact_external_value(&input);
        let twice = redact_external_value(&once);
        let text = once.to_string();

        assert!(!text.contains(marker));
        assert!(text.contains("https://[REDACTED]@proxy.invalid:8443/path"));
        assert_eq!(twice, once);
    }

    #[test]
    fn event_log_shapes_are_masked_without_hiding_structural_arguments() {
        let marker = "synthetic-event-log-marker";
        let input = json!({
            "authorization_header": format!("Bearer {marker}"),
            "credential_blob": marker,
            "config": format!("mcp_servers.demo.env.OPENAI_API_KEY=\"{marker}\""),
            "argv": ["provider", "--api-key", marker, "--team-key", "current"],
            "command": ["provider", "--authorization-header", marker, "--auth-mode", "subscription"],
        });

        let redacted = redact_external_value(&input);

        assert!(!redacted.to_string().contains(marker));
        assert_eq!(redacted["authorization_header"], REDACTED);
        assert_eq!(redacted["credential_blob"], REDACTED);
        assert_eq!(redacted["argv"][2], REDACTED);
        assert_eq!(redacted["argv"][4], "current");
        assert_eq!(redacted["command"][2], REDACTED);
        assert_eq!(redacted["command"][4], "subscription");
        assert_eq!(redact_external_value(&redacted), redacted);
    }
}
