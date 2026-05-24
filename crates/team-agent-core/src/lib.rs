use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub pane_id: String,
    pub session_name: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_index: String,
    pub pane_tty: String,
    pub pane_current_command: String,
    pub pane_active: bool,
}

impl Target {
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.session_name, self.window_index, self.pane_index, self.pane_tty
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetResolution {
    Resolved(Target),
    Ambiguous(Vec<Target>),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Accepted,
    TargetResolved,
    Injected,
    Visible,
    Failed,
    Ambiguous,
}

impl DeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryStatus::Accepted => "accepted",
            DeliveryStatus::TargetResolved => "target_resolved",
            DeliveryStatus::Injected => "injected",
            DeliveryStatus::Visible => "visible",
            DeliveryStatus::Failed => "failed",
            DeliveryStatus::Ambiguous => "ambiguous",
        }
    }

    pub fn can_transition_to(&self, next: &DeliveryStatus) -> bool {
        matches!(
            (self, next),
            (DeliveryStatus::Accepted, DeliveryStatus::TargetResolved)
                | (DeliveryStatus::Accepted, DeliveryStatus::Failed)
                | (DeliveryStatus::Accepted, DeliveryStatus::Ambiguous)
                | (DeliveryStatus::TargetResolved, DeliveryStatus::Injected)
                | (DeliveryStatus::TargetResolved, DeliveryStatus::Failed)
                | (DeliveryStatus::TargetResolved, DeliveryStatus::Ambiguous)
                | (DeliveryStatus::Injected, DeliveryStatus::Visible)
                | (DeliveryStatus::Injected, DeliveryStatus::Failed)
                | (DeliveryStatus::Visible, DeliveryStatus::Failed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryError {
    pub code: String,
    pub message: String,
}

pub fn list_tmux_targets() -> Result<Vec<Target>, String> {
    let format = "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}";
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", format])
        .output()
        .map_err(|err| format!("tmux list-panes failed: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_target_line)
        .collect())
}

pub fn parse_target_line(line: &str) -> Option<Target> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 8 {
        return None;
    }
    Some(Target {
        pane_id: parts[0].to_string(),
        session_name: parts[1].to_string(),
        window_index: parts[2].to_string(),
        window_name: parts[3].to_string(),
        pane_index: parts[4].to_string(),
        pane_tty: parts[5].to_string(),
        pane_current_command: parts[6].to_string(),
        pane_active: parts[7] == "1",
    })
}

pub fn resolve_target(candidates: Vec<Target>, expected_fingerprint: Option<&str>) -> TargetResolution {
    if let Some(fingerprint) = expected_fingerprint {
        let matched: Vec<Target> = candidates
            .iter()
            .cloned()
            .filter(|target| target.fingerprint() == fingerprint)
            .collect();
        if matched.len() == 1 {
            return TargetResolution::Resolved(matched[0].clone());
        }
        if matched.len() > 1 {
            return TargetResolution::Ambiguous(matched);
        }
    }
    let usable: Vec<Target> = candidates
        .into_iter()
        .filter(|target| {
            let cmd = target
                .pane_current_command
                .rsplit('/')
                .next()
                .unwrap_or(&target.pane_current_command);
            matches!(cmd, "codex" | "node" | "nodejs")
        })
        .collect();
    match usable.len() {
        0 => TargetResolution::Missing,
        1 => TargetResolution::Resolved(usable[0].clone()),
        _ => TargetResolution::Ambiguous(usable),
    }
}

pub fn render_message(sender: &str, task_id: Option<&str>, content: &str, token: &str) -> String {
    let mut header = format!("Team Agent message from {}", sender);
    if let Some(task) = task_id {
        if !task.is_empty() {
            header.push_str(" for ");
            header.push_str(task);
        }
    }
    format!("{header}:\n\n{content}\n\n[team-agent-token:{token}]")
}

pub fn redact_secrets(input: &str) -> String {
    let mut out = Vec::new();
    for raw in input.split_whitespace() {
        let lower = raw.to_ascii_lowercase();
        let redacted = lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("token=")
            || lower.contains("secret")
            || lower == "bearer"
            || raw.starts_with("sk-")
            || looks_base64_secret(raw);
        out.push(if redacted { "[REDACTED]" } else { raw });
    }
    out.join(" ")
}

pub fn looks_base64_secret(value: &str) -> bool {
    value.len() >= 32
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub provider: String,
    pub model: String,
    pub auth_mode: String,
    pub profile: String,
    pub credential_ref: Option<String>,
}

pub fn validate_profile(profile: &Profile) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if !matches!(
        profile.auth_mode.as_str(),
        "subscription" | "official_api" | "compatible_api"
    ) {
        errors.push("auth_mode must be subscription, official_api, or compatible_api".to_string());
    }
    if profile.provider.is_empty() {
        errors.push("provider must not be empty".to_string());
    }
    if profile.model.is_empty() {
        errors.push("model must not be empty".to_string());
    }
    if profile.profile.is_empty() {
        errors.push("profile must not be empty".to_string());
    }
    for value in [
        &profile.provider,
        &profile.model,
        &profile.auth_mode,
        &profile.profile,
    ] {
        if contains_inline_secret(value) {
            errors.push("profile metadata contains a probable inline secret".to_string());
            break;
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn contains_inline_secret(value: &str) -> bool {
    contains_secret_assignment(value)
        || contains_bearer_secret(value)
        || value
            .split_whitespace()
            .any(|chunk| chunk.starts_with("sk-") || looks_base64_secret(chunk))
        || value.starts_with("sk-")
        || looks_base64_secret(value)
}

fn contains_secret_assignment(value: &str) -> bool {
    for line in value.lines() {
        for separator in ['=', ':'] {
            let Some((key, raw)) = line.split_once(separator) else {
                continue;
            };
            let normalized: String = key
                .to_ascii_lowercase()
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect();
            if !matches!(
                normalized.as_str(),
                "apikey" | "token" | "secret" | "password" | "credential"
            ) {
                continue;
            }
            let candidate = raw.trim().trim_matches(|ch| ch == '"' || ch == '\'');
            if candidate.starts_with("sk-") || candidate.len() >= 8 || looks_base64_secret(candidate) {
                return true;
            }
        }
    }
    false
}

fn contains_bearer_secret(value: &str) -> bool {
    let mut previous_was_bearer = false;
    for chunk in value.split_whitespace() {
        if previous_was_bearer && chunk.len() >= 16 && chunk.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '~' | '+' | '/' | '=' | '-')
        }) {
            return true;
        }
        previous_was_bearer = chunk.eq_ignore_ascii_case("bearer");
    }
    false
}

pub fn json_escape(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_and_fingerprint() {
        let target = parse_target_line("%1\ts\t2\tw\t0\t/dev/ttys001\tnode\t1").unwrap();
        assert_eq!(target.pane_id, "%1");
        assert_eq!(target.fingerprint(), "s|2|0|/dev/ttys001");
    }

    #[test]
    fn delivery_state_transitions_are_explicit() {
        assert!(DeliveryStatus::Accepted.can_transition_to(&DeliveryStatus::TargetResolved));
        assert!(DeliveryStatus::TargetResolved.can_transition_to(&DeliveryStatus::Injected));
        assert!(DeliveryStatus::Injected.can_transition_to(&DeliveryStatus::Visible));
        assert!(!DeliveryStatus::Accepted.can_transition_to(&DeliveryStatus::Visible));
    }

    #[test]
    fn renders_human_readable_message() {
        let rendered = render_message("worker", Some("task_a"), "done", "msg_1");
        assert!(rendered.contains("Team Agent message from worker for task_a:"));
        assert!(rendered.contains("[team-agent-token:msg_1]"));
    }

    #[test]
    fn redacts_common_secret_shapes() {
        let redacted = redact_secrets("Authorization Bearer sk-test abcdefghijklmnopqrstuvwxyz123456");
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("sk-test"));
    }

    #[test]
    fn inline_secret_scan_allows_ordinary_token_prose() {
        assert!(!contains_inline_secret("Use the visible token in logs for delivery evidence."));
        assert!(contains_inline_secret("API_KEY=sk-inline-secret"));
    }

    #[test]
    fn profile_rejects_inline_secret() {
        let profile = Profile {
            provider: "codex".to_string(),
            model: "gpt-5.5".to_string(),
            auth_mode: "subscription".to_string(),
            profile: "sk-inline-secret".to_string(),
            credential_ref: None,
        };
        assert!(validate_profile(&profile).is_err());
    }
}
