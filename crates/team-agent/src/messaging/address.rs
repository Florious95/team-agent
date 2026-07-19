//! Pure logical-address grammar shared by public entry-point adapters.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalAddress {
    pub workspace: Option<PathBuf>,
    pub target: LogicalAddressTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalAddressTarget {
    Worker(String),
    TeamEntity { team: String, entity: String },
    SessionWindow { session: String, window: String },
}

pub fn parse_logical_address(raw_name: &str) -> Result<LogicalAddress, &'static str> {
    let raw = raw_name.trim();
    if raw.is_empty() {
        return Err("name is empty");
    }
    let (workspace, name) = if let Some((workspace, rest)) = raw.split_once("::") {
        if workspace.trim().is_empty() || rest.trim().is_empty() {
            return Err("workspace-qualified name must include workspace and target");
        }
        (Some(PathBuf::from(workspace)), rest.trim())
    } else {
        (None, raw)
    };
    if name.contains("//") {
        return Err("name contains an empty path segment");
    }
    let target = if name.contains('/') {
        let parts = name.split('/').collect::<Vec<_>>();
        if parts.len() != 2 || !valid_component(parts[0]) || !valid_component(parts[1]) {
            return Err("expected <team>/<agent> or <team>/leader");
        }
        LogicalAddressTarget::TeamEntity {
            team: parts[0].to_string(),
            entity: parts[1].to_string(),
        }
    } else if name.contains(':') {
        let parts = name.split(':').collect::<Vec<_>>();
        if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
            return Err("expected <session>:<window>");
        }
        LogicalAddressTarget::SessionWindow {
            session: parts[0].to_string(),
            window: parts[1].to_string(),
        }
    } else {
        if !valid_component(name) {
            return Err("expected a non-empty agent id");
        }
        LogicalAddressTarget::Worker(name.to_string())
    };
    Ok(LogicalAddress { workspace, target })
}

fn valid_component(raw: &str) -> bool {
    !raw.trim().is_empty()
        && !raw.contains(char::is_whitespace)
        && !raw.contains('/')
        && !raw.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_send_grammar_without_runtime_state() {
        assert_eq!(
            parse_logical_address("/tmp/ws::team/w1").unwrap(),
            LogicalAddress {
                workspace: Some(PathBuf::from("/tmp/ws")),
                target: LogicalAddressTarget::TeamEntity {
                    team: "team".into(),
                    entity: "w1".into(),
                },
            }
        );
        assert!(parse_logical_address("team//w1").is_err());
    }
}
